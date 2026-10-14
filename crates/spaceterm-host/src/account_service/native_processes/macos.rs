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

/// Uses the same immutable XNU birth used by PTY retirement, not a coarse timestamp.
pub(super) fn instance_birth(pid: u32) -> Result<Option<u64>, ServiceFailure> {
    use crate::pty::{macos_pty_host::MacosPtyHost, unix_pty::UnixPtyHost as _};
    MacosPtyHost
        .process_birth(pid as i32)
        .map_err(|_| ServiceFailure::CleanupUnresolved)
}

/// Proves descendant absence using immutable original-parent births. A missing
/// intermediate parent newer than the old instance is unresolved, not clean.
/// The fence excludes fork races and unpublished children; no sampled PID is killed.
pub(super) fn previous_instance_absent(root_birth: u64) -> Result<(), ServiceFailure> {
    use crate::pty::{macos_pty_host::MacosPtyHost, unix_pty::UnixPtyHost as _};
    let host = MacosPtyHost;
    for _ in 0..64 {
        let result = (|| {
            let before = host
                .process_creation_epoch()
                .map_err(|_| ServiceFailure::CleanupUnresolved)?;
            let pids = host
                .settlement_process_ids()
                .map_err(|_| ServiceFailure::CleanupUnresolved)?;
            if pids.len() > 65536 {
                return Err(ServiceFailure::LimitExceeded);
            }
            let mut lineage = std::collections::BTreeMap::new();
            for pid in pids.into_iter().filter(|pid| *pid > 0) {
                if let Some((birth, parent)) = host
                    .process_lineage(pid)
                    .map_err(|_| ServiceFailure::CleanupUnresolved)?
                    && lineage.insert(birth, parent).is_some()
                {
                    return Err(ServiceFailure::CleanupUnresolved);
                }
            }
            let after = host
                .process_creation_epoch()
                .map_err(|_| ServiceFailure::CleanupUnresolved)?;
            if !before.no_intervening_fork(after) {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            lineage_absent(root_birth, &lineage)
        })();
        if result.is_ok() || matches!(result, Err(ServiceFailure::LimitExceeded)) {
            return result;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Err(ServiceFailure::CleanupUnresolved)
}

fn lineage_absent(
    root: u64,
    lineage: &std::collections::BTreeMap<u64, u64>,
) -> Result<(), ServiceFailure> {
    if lineage.contains_key(&root) {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    let mut unrelated = std::collections::BTreeSet::new();
    // Births are ordered, so every retained parent has already been classified.
    // Missing parents newer than the root remain ambiguous. Never guess their
    // ancestry from a reusable PID or a current launchd parent.
    for (&birth, &parent) in lineage.range(root..) {
        if parent >= birth || parent == root || (parent > root && !unrelated.contains(&parent)) {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        unrelated.insert(birth);
    }
    Ok(())
}

#[cfg(test)]
mod lineage_tests {
    use super::*;
    #[test]
    fn dead_root_does_not_discharge_live_or_ambiguous_descendants() {
        assert_eq!(
            lineage_absent(100, &[(101, 100)].into()),
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert_eq!(
            lineage_absent(100, &[(103, 102)].into()),
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert_eq!(lineage_absent(100, &[(103, 102), (102, 99)].into()), Ok(()));
        assert_eq!(
            lineage_absent(100, &[(103, 102), (102, 100)].into()),
            Err(ServiceFailure::CleanupUnresolved)
        );
    }
}
