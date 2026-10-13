//! macOS process observations through libproc.

use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::pty::unix_pty::{
    MasterReadError, ProcessCreationEpoch, ProcessIdentity, ProcessObservation, ProcessStart,
    UnixPtyHost,
};

// XNU sys/proc_info.h and sys/proc_info_private.h.
const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;

// XNU marks this 56-byte layout as API; the SDK and libc omit its declaration.
#[repr(C)]
struct ProcessUniqueInfo {
    _uuid: [u8; 16],
    unique_id: u64,
    _parent_unique_id: u64,
    _id_version: i32,
    _original_parent_version: i32,
    _reserved: [u64; 2],
}

/// macOS process facts through libproc. A PTY master read after hangup reports end of file.
#[derive(Clone, Copy, Debug, Default)]
pub struct MacosPtyHost;

impl UnixPtyHost for MacosPtyHost {
    fn observe_process(&self, process: i32) -> Option<ProcessObservation> {
        observe_process(process)
    }

    fn inspect_process(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
        inspect_process(process)
    }

    fn process_birth(&self, process: i32) -> io::Result<Option<u64>> {
        process_birth(process)
    }

    fn inspect_session_member(
        &self,
        process: i32,
        leader: ProcessIdentity,
        leader_birth: Option<u64>,
    ) -> io::Result<Option<ProcessObservation>> {
        let Some(birth) = process_birth(process)? else {
            return Ok(None);
        };
        let Some(session) = process_session(process)? else {
            return Ok(None);
        };
        if process_birth(process)? != Some(birth) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        // Both queries are permission-free across accounts. Full BSD inspection is
        // restricted to the same UID, so first exclude unrelated sessions and births.
        if session != leader.process || birth < leader_birth.ok_or(io::ErrorKind::Unsupported)? {
            return Ok(None);
        }
        let observation = inspect_process(process)?;
        if process_birth(process)?.is_some_and(|after| after != birth) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(observation.filter(|observation| observation.session == leader.process))
    }

    fn process_ids(&self) -> io::Result<Vec<i32>> {
        all_process_ids()
    }

    fn process_creation_epoch(&self) -> io::Result<ProcessCreationEpoch> {
        fork_fence().map(ProcessCreationEpoch::FenceBirth)
    }

    fn settlement_process_ids(&self) -> io::Result<Vec<i32>> {
        // XNU counts allocated processes before they join allproc. Its sizing
        // query reports nprocs + 20; the list also includes uncounted kernel PID 0.
        // Between fork fences, equality proves that no unpublished child is omitted.
        const SIZING_SLOP: i32 = 20;
        // SAFETY: a null buffer requests only the kernel's allocated process count.
        let allocated = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let expected = allocated
            .checked_sub(SIZING_SLOP)
            .ok_or(io::ErrorKind::InvalidData)?;
        let processes = all_process_ids()?;
        let listed = processes.iter().filter(|pid| **pid > 0).count();
        if usize::try_from(expected).ok() != Some(listed) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(processes)
    }

    fn master_read_error(&self, _error: &io::Error) -> MasterReadError {
        MasterReadError::Failure
    }
}

fn fork_fence() -> io::Result<u64> {
    // XNU assigns unique_id under proc_list_lock at every fork/spawn; exec retains
    // it. Two consecutive ordinals fence a census without a privileged counter.
    let mut child = Command::new("/usr/bin/true")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.kind())?;
    let process = i32::try_from(child.id()).map_err(|_| io::ErrorKind::InvalidData);
    // The fence child stays unreaped until try_wait below, so its PID cannot be reused and the
    // zombie-inclusive query still identifies it if it has already exited.
    let birth = process.and_then(|process| {
        process_birth_including_zombies(process, true).map_err(|error| error.kind())
    });
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        let failure = match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return birth?.ok_or_else(|| io::ErrorKind::NotFound.into());
            }
            Ok(Some(_)) => return Err(io::ErrorKind::Other.into()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Ok(None) => io::ErrorKind::TimedOut,
            Err(error) => error.kind(),
        };
        // The owned child handle, not a reusable PID, is the only cleanup authority.
        let _ = child.kill();
        let _ = child.wait();
        return Err(failure.into());
    }
}

fn observe_process(process: i32) -> Option<ProcessObservation> {
    inspect_process(process).ok().flatten()
}

fn inspect_process(process: i32) -> io::Result<Option<ProcessObservation>> {
    for _ in 0..3 {
        let Some(before_start) = process_birth(process)? else {
            return Ok(None);
        };
        let Some(before) = process_bsd_info(process)? else {
            return Ok(None);
        };
        let Some(session) = process_session(process)? else {
            return Ok(None);
        };
        let Some(after) = process_bsd_info(process)? else {
            return Ok(None);
        };
        let Some(after_start) = process_birth(process)? else {
            return Ok(None);
        };
        if before_start != after_start
            || before.pbi_pgid != after.pbi_pgid
            || before.e_tdev != after.e_tdev
        {
            continue;
        }
        return Ok(Some(ProcessObservation {
            identity: ProcessIdentity {
                process,
                start: ProcessStart {
                    coarse: after.pbi_start_tvsec,
                    fine: after.pbi_start_tvusec,
                },
            },
            process_group: i32::try_from(after.pbi_pgid).map_err(|_| io::ErrorKind::InvalidData)?,
            session,
        }));
    }
    Err(io::ErrorKind::WouldBlock.into())
}

fn process_birth(process: i32) -> io::Result<Option<u64>> {
    process_birth_including_zombies(process, false)
}

/// XNU searches exited, unreaped processes for this flavor only when the argument is nonzero.
fn process_birth_including_zombies(process: i32, zombies: bool) -> io::Result<Option<u64>> {
    let mut info = std::mem::MaybeUninit::<ProcessUniqueInfo>::zeroed();
    let expected = i32::try_from(std::mem::size_of::<ProcessUniqueInfo>())
        .map_err(|_| io::ErrorKind::InvalidData)?;
    // SAFETY: info points to the exact writable layout required by this libproc flavor.
    let read = unsafe {
        libc::proc_pidinfo(
            process,
            PROC_PIDUNIQIDENTIFIERINFO,
            u64::from(zombies),
            info.as_mut_ptr().cast(),
            expected,
        )
    };
    if read != expected {
        if read > 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error.kind().into())
        };
    }
    // SAFETY: proc_pidinfo initialized the complete process identity structure.
    let info = unsafe { info.assume_init() };
    if info.unique_id == 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(info.unique_id))
}

fn process_session(process: i32) -> io::Result<Option<i32>> {
    // SAFETY: getsid performs a read-only process identity query without a UID restriction.
    let session = unsafe { libc::getsid(process) };
    if session > 0 {
        return Ok(Some(session));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(error.kind().into())
    }
}

#[cfg(test)]
fn process_bsd_short_info(process: i32) -> io::Result<Option<libc::proc_bsdshortinfo>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::zeroed();
    let expected = i32::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>())
        .map_err(|_| io::ErrorKind::InvalidData)?;
    // SAFETY: info points to writable storage of the declared size.
    let read = unsafe {
        libc::proc_pidinfo(
            process,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            expected,
        )
    };
    if read != expected {
        if read > 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error.kind().into())
        };
    }
    // SAFETY: proc_pidinfo initialized the complete short process record.
    Ok(Some(unsafe { info.assume_init() }))
}

fn process_bsd_info(process: i32) -> io::Result<Option<libc::proc_bsdinfo>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| io::ErrorKind::InvalidData)?;
    // SAFETY: info points to writable storage with the declared size.
    let read = unsafe {
        libc::proc_pidinfo(
            process,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            expected,
        )
    };
    if read != expected {
        if read > 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error.kind().into())
        };
    }
    // SAFETY: proc_pidinfo initialized the complete proc_bsdinfo structure.
    Ok(Some(unsafe { info.assume_init() }))
}

fn all_process_ids() -> io::Result<Vec<i32>> {
    let process_count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if process_count < 1 {
        return Err(io::Error::other("failed to size the macOS process list"));
    }
    let mut capacity = process_count as usize + 64;
    loop {
        let mut process_ids = vec![0_i32; capacity];
        let buffer_size = capacity
            .checked_mul(std::mem::size_of::<i32>())
            .and_then(|size| i32::try_from(size).ok())
            .ok_or_else(|| io::Error::other("process-list buffer size overflowed"))?;
        let listed = unsafe {
            libc::proc_listallpids(process_ids.as_mut_ptr().cast::<libc::c_void>(), buffer_size)
        };
        if listed < 0 {
            return Err(io::Error::last_os_error());
        }
        if listed as usize >= capacity {
            capacity = capacity
                .checked_mul(2)
                .ok_or_else(|| io::Error::other("process-list capacity overflowed"))?;
            continue;
        }
        process_ids.truncate(listed as usize);
        return Ok(process_ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_uid_process_newer_than_session_fence_needs_no_full_bsd_inspection() {
        let host = MacosPtyHost;
        let current = i32::try_from(std::process::id()).unwrap();
        let leader = host.inspect_process(current).unwrap().unwrap().identity;
        // Reuse a live foreign process without creating an account or taking privileges.
        // The supplied birth fence models a PTY leader born before that process.
        let (foreign, birth) = host
            .process_ids()
            .unwrap()
            .into_iter()
            .filter(|process| *process > 0)
            .find_map(|process| {
                let short = process_bsd_short_info(process).ok()??;
                // SAFETY: getuid reads this process's account identity.
                if short.pbsi_uid == unsafe { libc::getuid() } {
                    return None;
                }
                let birth = process_birth(process).ok()??;
                process_bsd_info(process)
                    .is_err()
                    .then_some((process, birth))
            })
            .expect("macOS must expose a foreign process whose full BSD record is denied");
        assert_eq!(
            host.inspect_session_member(foreign, leader, Some(birth - 1))
                .unwrap(),
            None
        );
    }

    #[test]
    fn permanent_birth_keeps_the_existing_native_start_identity() {
        let process = i32::try_from(std::process::id()).unwrap();
        let host = MacosPtyHost;
        let native = process_bsd_info(process).unwrap().unwrap();
        assert_eq!(
            host.inspect_process(process)
                .unwrap()
                .unwrap()
                .identity
                .start,
            ProcessStart {
                coarse: native.pbi_start_tvsec,
                fine: native.pbi_start_tvusec
            }
        );
        assert_eq!(
            host.process_birth(process).unwrap(),
            process_birth(process).unwrap()
        );
        assert!(host.process_birth(process).unwrap().unwrap() > 0);
        assert!(host.inspect_process(i32::MAX).unwrap().is_none());
        assert!(process_birth(i32::MAX).unwrap().is_none());
    }

    #[test]
    fn fork_fence_birth_is_observable_after_its_child_exits_unreaped() {
        let mut child = std::process::Command::new("/usr/bin/true")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let process = i32::try_from(child.id()).unwrap();
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: WNOWAIT observes the exit without reaping, so the zombie and its PID remain.
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        assert_eq!(waited, 0);
        assert!(process_birth(process).unwrap().is_none());
        assert!(
            process_birth_including_zombies(process, true)
                .unwrap()
                .is_some_and(|birth| birth > 0)
        );
        assert!(child.wait().unwrap().success());
    }
}
