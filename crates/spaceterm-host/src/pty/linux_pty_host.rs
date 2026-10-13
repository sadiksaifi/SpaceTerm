//! Linux process observations and PTY hangup classification through procfs.

use std::io;

use crate::pty::unix_pty::{
    MasterReadError, ProcessCreationEpoch, ProcessIdentity, ProcessObservation, ProcessStart,
    UnixPtyHost,
};

/// Linux process facts through procfs. Linux reports `EIO` from a PTY master read once every
/// slave descriptor has closed, which is the end of terminal output.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxPtyHost;

impl UnixPtyHost for LinuxPtyHost {
    fn observe_process(&self, process: i32) -> Option<ProcessObservation> {
        let stat = read_process_stat(process)?;
        (!stat.exited).then_some(stat.observation)
    }

    fn inspect_process(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
        inspect_process_stat(process).map(|stat| stat.map(|stat| stat.observation))
    }

    fn observe_spawned_child(&self, process: i32) -> Option<ProcessObservation> {
        read_process_stat(process).map(|stat| stat.observation)
    }

    fn process_ids(&self) -> io::Result<Vec<i32>> {
        let mut processes = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let name = entry?.file_name();
            if let Some(process) = name.to_str().and_then(|name| name.parse::<i32>().ok()) {
                processes.push(process);
            }
        }
        Ok(processes)
    }

    fn settlement_process_ids(&self) -> io::Result<Vec<i32>> {
        let mounts =
            std::fs::read_to_string("/proc/self/mountinfo").map_err(|error| error.kind())?;
        if !procfs_census_is_unfiltered(&mounts) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        self.process_ids()
    }

    fn process_creation_epoch(&self) -> io::Result<ProcessCreationEpoch> {
        // Linux increments total_forks under tasklist_lock after publishing the
        // process, before it can run. /proc/stat exposes that monotonic counter.
        let stat = std::fs::read_to_string("/proc/stat").map_err(|error| error.kind())?;
        let count = stat
            .lines()
            .find_map(|line| line.strip_prefix("processes "))
            .and_then(|count| count.trim().parse().ok())
            .ok_or(io::ErrorKind::InvalidData)?;
        Ok(ProcessCreationEpoch::ForkCount(count))
    }

    fn master_read_error(&self, error: &io::Error) -> MasterReadError {
        if error.raw_os_error() == Some(libc::EIO) {
            MasterReadError::Hangup
        } else {
            MasterReadError::Failure
        }
    }
}

fn procfs_census_is_unfiltered(mountinfo: &str) -> bool {
    let mut found = false;
    for line in mountinfo.lines() {
        let Some((mount, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        let fields = mount.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.get(4) != Some(&"/proc") {
            continue;
        }
        found = true;
        let filesystem = filesystem.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() < 6
            || filesystem.len() < 3
            || fields.get(3) != Some(&"/")
            || filesystem.first() != Some(&"proc")
        {
            return false;
        }
        // hidepid can omit privileged descendants from a PID walk altogether.
        // A filtered or subtree-mounted procfs cannot certify session absence.
        if fields
            .get(5)
            .into_iter()
            .chain(filesystem.get(2))
            .any(|options| {
                options.split(',').any(|option| {
                    option
                        .strip_prefix("hidepid=")
                        .is_some_and(|value| !matches!(value, "0" | "off"))
                })
            })
        {
            return false;
        }
    }
    found
}

struct ProcessStat {
    observation: ProcessObservation,
    exited: bool,
}

fn read_process_stat(process: i32) -> Option<ProcessStat> {
    inspect_process_stat(process).ok().flatten()
}

fn inspect_process_stat(process: i32) -> io::Result<Option<ProcessStat>> {
    if process < 1 {
        return Ok(None);
    }
    let stat = match std::fs::read(format!("/proc/{process}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.kind().into()),
    };
    if let Some(parsed) = parse_process_stat(process, &stat) {
        return Ok(Some(parsed));
    }
    let fields = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .and_then(|end| std::str::from_utf8(&stat[end + 1..]).ok())
        .map(|fields| fields.split_ascii_whitespace().collect::<Vec<_>>());
    if fields.as_ref().is_some_and(|fields| {
        matches!(fields.first(), Some(&"X" | &"x"))
            || (fields.get(2) == Some(&"0") && fields.get(3) == Some(&"0"))
    }) {
        return Ok(None);
    }
    Err(io::ErrorKind::InvalidData.into())
}

/// Parse `/proc/<pid>/stat`. The command name may contain spaces and parentheses, so fields are
/// read after its final closing parenthesis. One read is atomic for the process record.
fn parse_process_stat(process: i32, stat: &[u8]) -> Option<ProcessStat> {
    let name_end = stat.iter().rposition(|byte| *byte == b')')?;
    let fields = std::str::from_utf8(&stat[name_end + 1..]).ok()?;
    let fields = fields.split_ascii_whitespace().collect::<Vec<_>>();
    // Fields after the command name start at `state` (field 3 in proc_pid_stat(5)).
    let state = *fields.first()?;
    if matches!(state, "X" | "x") {
        return None;
    }
    let process_group = fields.get(2)?.parse::<i32>().ok()?;
    let session = fields.get(3)?.parse::<i32>().ok()?;
    let started_ticks = fields.get(19)?.parse::<u64>().ok()?;
    if session < 1 || process_group < 1 {
        return None;
    }
    Some(ProcessStat {
        observation: ProcessObservation {
            identity: ProcessIdentity {
                process,
                start: ProcessStart {
                    coarse: started_ticks,
                    fine: 0,
                },
            },
            process_group,
            session,
        },
        exited: state == "Z",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(name: &str, state: &str, group: i32, session: i32, start: u64) -> Vec<u8> {
        format!(
            "4242 ({name}) {state} 1 {group} {session} 34816 4242 4194560 1 0 0 0 0 0 0 0 20 0 1 0 {start} 1 2 3\n"
        )
        .into_bytes()
    }

    #[test]
    fn settlement_census_requires_a_complete_unfiltered_procfs_mount() {
        for options in ["rw", "rw,hidepid=0", "rw,hidepid=off"] {
            assert!(procfs_census_is_unfiltered(&format!(
                "31 22 0:5 / /proc rw,nosuid,nodev,noexec - proc proc {options}\n"
            )));
        }
        for mount in [
            "31 22 0:5 / /proc rw - proc proc rw,hidepid=2\n",
            "31 22 0:5 / /proc rw - proc proc rw,hidepid=ptraceable\n",
            "31 22 0:5 / /proc rw,hidepid=1 - proc proc rw\n",
            "31 22 0:5 /subtree /proc rw - proc proc rw\n",
            "31 22 0:5 / /proc rw - tmpfs tmpfs rw\n",
            "31 22 0:5 / /other rw - proc proc rw\n",
            "invalid",
        ] {
            assert!(!procfs_census_is_unfiltered(mount));
        }
    }

    #[test]
    fn linux_stat_parser_reads_group_session_and_start_after_the_command_name() {
        let parsed = parse_process_stat(4242, &stat("sh) (x y", "S", 4242, 4240, 991_234)).unwrap();
        assert!(!parsed.exited);
        let observation = parsed.observation;

        assert_eq!(observation.process_group, 4242);
        assert_eq!(observation.session, 4240);
        assert_eq!(
            observation.identity,
            ProcessIdentity {
                process: 4242,
                start: ProcessStart {
                    coarse: 991_234,
                    fine: 0,
                },
            }
        );
    }

    #[test]
    fn linux_stat_parser_retains_unreaped_child_identity_and_rejects_dead_records() {
        let exited = parse_process_stat(1, &stat("zsh", "Z", 1, 1, 7)).unwrap();
        assert!(exited.exited);
        assert_eq!(exited.observation.identity.start.coarse, 7);
        assert_eq!(exited.observation.session, 1);
        assert!(parse_process_stat(1, &stat("zsh", "X", 1, 1, 1)).is_none());
        assert!(parse_process_stat(1, &stat("kthreadd", "S", 0, 0, 1)).is_none());
        assert!(parse_process_stat(1, b"1 (truncated").is_none());
        assert!(parse_process_stat(1, b"1 (short) S 1 2").is_none());
    }

    #[cfg(feature = "native-tests")]
    #[test]
    fn linux_host_retains_an_exited_owned_child_without_reporting_it_alive() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let process = i32::try_from(child.id()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let exited = loop {
            if read_process_stat(process).is_some_and(|stat| stat.exited) {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let host = LinuxPtyHost;
        let identity = host.observe_spawned_child(process);
        let live = host.observe_process(process);
        child.wait().unwrap();

        assert!(
            exited,
            "the child must exit before the parent captures its identity"
        );
        assert_eq!(identity.unwrap().identity.process, process);
        assert_eq!(live, None);
        assert_eq!(host.observe_spawned_child(process), None);
    }

    #[test]
    fn linux_host_reads_the_current_process_and_classifies_pty_hangup() {
        let host = LinuxPtyHost;
        let current = i32::try_from(std::process::id()).unwrap();
        let observation = host.observe_process(current).unwrap();
        // SAFETY: getsid and getpgrp perform read-only identity queries for this process.
        let (session, group) = unsafe { (libc::getsid(0), libc::getpgrp()) };

        assert_eq!(observation.identity.process, current);
        assert_eq!(observation.session, session);
        assert_eq!(observation.process_group, group);
        assert!(host.process_ids().unwrap().contains(&current));
        assert_eq!(
            host.master_read_error(&io::Error::from_raw_os_error(libc::EIO)),
            MasterReadError::Hangup
        );
        assert_eq!(
            host.master_read_error(&io::Error::from_raw_os_error(libc::EBADF)),
            MasterReadError::Failure
        );
    }
}
