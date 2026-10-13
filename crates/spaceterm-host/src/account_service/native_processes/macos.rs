use super::{ServiceFailure, ServiceProcessIdentity};
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

fn bsd(pid: u32) -> Result<Option<libc::proc_bsdinfo>, ServiceFailure> {
    let mut value = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: fixed flavor initializes exactly a proc_bsdinfo in the supplied buffer.
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            value.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if count as usize != size {
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(ServiceFailure::CleanupUnresolved);
    }
    // SAFETY: successful proc_pidinfo initialized the complete struct.
    Ok(Some(unsafe { value.assume_init() }))
}
fn birth(value: &libc::proc_bsdinfo) -> Result<u64, ServiceFailure> {
    value
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(value.pbi_start_tvusec))
        .filter(|start| *start != 0)
        .ok_or(ServiceFailure::CleanupUnresolved)
}
pub(super) fn observe(
    account: u32,
    pid: u32,
) -> Result<Option<ServiceProcessIdentity>, ServiceFailure> {
    let Some(before) = bsd(pid)? else {
        return Ok(None);
    };
    if before.pbi_uid != account {
        return Ok(None);
    }
    if before.pbi_pid != pid {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    let start = birth(&before)?;
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: libproc bounds path writes by the supplied buffer size.
    let count =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    if count <= 0 {
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(ServiceFailure::CleanupUnresolved);
    }
    let length = path
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(ServiceFailure::CleanupUnresolved)?;
    path.truncate(length);
    let executable = PathBuf::from(OsString::from_vec(path));
    if !executable.is_absolute() {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    let Some(after) = bsd(pid)? else {
        return Ok(None);
    };
    if after.pbi_pid != pid
        || after.pbi_uid != account
        || after.pbi_ruid != before.pbi_ruid
        || after.pbi_svuid != before.pbi_svuid
        || birth(&after)? != start
    {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    Ok(Some(ServiceProcessIdentity {
        pid,
        uid: account,
        fully_account_owned: before.pbi_ruid == account && before.pbi_svuid == account,
        start,
        executable,
    }))
}
