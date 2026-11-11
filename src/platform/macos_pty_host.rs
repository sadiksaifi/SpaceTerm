use std::io;

use crate::platform::unix_pty::{
    MasterReadError, ProcessIdentity, ProcessObservation, ProcessStart, UnixPtyHost,
};

/// macOS process facts through libproc. A PTY master read after hangup reports end of file.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MacosPtyHost;

impl UnixPtyHost for MacosPtyHost {
    fn observe_process(&self, process: i32) -> Option<ProcessObservation> {
        observe_process(process)
    }

    fn process_ids(&self) -> io::Result<Vec<i32>> {
        all_process_ids()
    }

    fn observe_exactly(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
        observe_exactly(process)
    }

    /// A session member can drop its controlling terminal without leaving the session.
    /// Query every listed PID's session instead of selecting by terminal device. Any
    /// unreadable PID leaves membership uncertain and therefore counts as work.
    fn session_candidates(&self, leader: i32) -> io::Result<Vec<i32>> {
        let mut members = Vec::new();
        for process in all_process_ids()? {
            if process < 1 {
                continue;
            }
            // SAFETY: getsid performs a read-only process identity query.
            let session = unsafe { libc::getsid(process) };
            if session < 1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                return Err(error);
            }
            if session == leader {
                members.push(process);
            }
        }
        Ok(members)
    }

    fn master_read_error(&self, _error: &io::Error) -> MasterReadError {
        MasterReadError::Failure
    }
}

fn observe_process(process: i32) -> Option<ProcessObservation> {
    let before = process_bsd_info(process)?;
    // SAFETY: getsid performs a read-only process identity query.
    let session = unsafe { libc::getsid(process) };
    if session < 1 {
        return None;
    }
    let after = process_bsd_info(process)?;
    let before_identity = process_identity(&before)?;
    let after_identity = process_identity(&after)?;
    if before_identity != after_identity
        || before.pbi_pgid != after.pbi_pgid
        || before.e_tdev != after.e_tdev
    {
        return None;
    }
    Some(ProcessObservation {
        identity: after_identity,
        process_group: i32::try_from(after.pbi_pgid).ok()?,
        session,
    })
}

/// As [`observe_process`], telling a process proven gone (`ESRCH`) from one that could not be
/// read or changed while it was read.
fn observe_exactly(process: i32) -> io::Result<Option<ProcessObservation>> {
    let unreadable = || io::Error::other("the process could not be read");
    let Some(before) = exact_bsd_info(process)? else {
        return Ok(None);
    };
    // SAFETY: getsid performs a read-only process identity query.
    let session = unsafe { libc::getsid(process) };
    if session < 1 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let Some(after) = exact_bsd_info(process)? else {
        return Ok(None);
    };
    let before_identity = process_identity(&before).ok_or_else(unreadable)?;
    let after_identity = process_identity(&after).ok_or_else(unreadable)?;
    if before_identity != after_identity
        || before.pbi_pgid != after.pbi_pgid
        || before.e_tdev != after.e_tdev
    {
        return Err(io::Error::other("the process changed while it was read"));
    }
    Ok(Some(ProcessObservation {
        identity: after_identity,
        process_group: i32::try_from(after.pbi_pgid).map_err(|_| unreadable())?,
        session,
    }))
}

/// The process's BSD info; None only when it is proven gone.
fn exact_bsd_info(process: i32) -> io::Result<Option<libc::proc_bsdinfo>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| io::Error::other("proc_bsdinfo is too large"))?;
    // SAFETY: info points to writable storage of `expected` bytes.
    let read = unsafe {
        libc::proc_pidinfo(
            process,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<libc::c_void>(),
            expected,
        )
    };
    if read == expected {
        // SAFETY: proc_pidinfo initialized the complete proc_bsdinfo structure.
        return Ok(Some(unsafe { info.assume_init() }));
    }
    let error = io::Error::last_os_error();
    if read <= 0 && error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(io::Error::other("the process could not be read"))
    }
}

fn process_bsd_info(process: i32) -> Option<libc::proc_bsdinfo> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
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
        return None;
    }
    // SAFETY: proc_pidinfo initialized the complete proc_bsdinfo structure.
    Some(unsafe { info.assume_init() })
}

fn process_identity(info: &libc::proc_bsdinfo) -> Option<ProcessIdentity> {
    Some(ProcessIdentity {
        process: i32::try_from(info.pbi_pid).ok()?,
        start: ProcessStart {
            coarse: info.pbi_start_tvsec,
            fine: info.pbi_start_tvusec,
        },
    })
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
    fn an_exact_read_sees_this_process_and_proves_a_missing_one_gone() {
        let current = i32::try_from(std::process::id()).unwrap();
        let observation = observe_exactly(current).unwrap().unwrap();
        // SAFETY: getsid and getpgrp perform read-only identity queries for this process.
        let (session, group) = unsafe { (libc::getsid(0), libc::getpgrp()) };
        assert_eq!(
            (
                observation.identity.process,
                observation.session,
                observation.process_group
            ),
            (current, session, group)
        );
        assert_eq!(observe_exactly(i32::MAX).unwrap(), None);
    }
}
