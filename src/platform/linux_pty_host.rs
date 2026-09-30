use std::io;

use crate::platform::unix_pty::{
    MasterReadError, ProcessIdentity, ProcessObservation, ProcessStart, UnixPtyHost,
};

/// Linux process facts through procfs. Linux reports `EIO` from a PTY master read once every
/// slave descriptor has closed, which is the end of terminal output.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LinuxPtyHost;

impl UnixPtyHost for LinuxPtyHost {
    fn observe_process(&self, process: i32) -> Option<ProcessObservation> {
        if process < 1 {
            return None;
        }
        let stat = std::fs::read(format!("/proc/{process}/stat")).ok()?;
        parse_process_stat(process, &stat)
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

    fn master_read_error(&self, error: &io::Error) -> MasterReadError {
        if error.raw_os_error() == Some(libc::EIO) {
            MasterReadError::Hangup
        } else {
            MasterReadError::Failure
        }
    }
}

/// Parse `/proc/<pid>/stat`. The command name may contain spaces and parentheses, so fields are
/// read after its final closing parenthesis. One read is atomic for the process record.
fn parse_process_stat(process: i32, stat: &[u8]) -> Option<ProcessObservation> {
    let name_end = stat.iter().rposition(|byte| *byte == b')')?;
    let fields = std::str::from_utf8(&stat[name_end + 1..]).ok()?;
    let fields = fields.split_ascii_whitespace().collect::<Vec<_>>();
    // Fields after the command name start at `state` (field 3 in proc_pid_stat(5)).
    let state = *fields.first()?;
    if matches!(state, "Z" | "X" | "x") {
        return None;
    }
    let process_group = fields.get(2)?.parse::<i32>().ok()?;
    let session = fields.get(3)?.parse::<i32>().ok()?;
    let started_ticks = fields.get(19)?.parse::<u64>().ok()?;
    if session < 1 || process_group < 1 {
        return None;
    }
    Some(ProcessObservation {
        identity: ProcessIdentity {
            process,
            start: ProcessStart {
                coarse: started_ticks,
                fine: 0,
            },
        },
        process_group,
        session,
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
    fn linux_stat_parser_reads_group_session_and_start_after_the_command_name() {
        let observation =
            parse_process_stat(4242, &stat("sh) (x y", "S", 4242, 4240, 991_234)).unwrap();

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
    fn linux_stat_parser_rejects_exited_and_sessionless_processes() {
        assert_eq!(parse_process_stat(1, &stat("zsh", "Z", 1, 1, 1)), None);
        assert_eq!(parse_process_stat(1, &stat("zsh", "X", 1, 1, 1)), None);
        assert_eq!(parse_process_stat(1, &stat("kthreadd", "S", 0, 0, 1)), None);
        assert_eq!(parse_process_stat(1, b"1 (truncated"), None);
        assert_eq!(parse_process_stat(1, b"1 (short) S 1 2"), None);
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
