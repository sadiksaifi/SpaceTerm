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
