use super::{ServiceFailure, ServiceProcessIdentity};
use std::{io::Read as _, path::PathBuf};

const LIMIT: usize = 16 * 1024;
fn classify(error: std::io::Error) -> ServiceFailure {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::ESRCH) => ServiceFailure::Missing,
        _ => ServiceFailure::CleanupUnresolved,
    }
}
fn read(path: &std::path::Path) -> Result<Vec<u8>, ServiceFailure> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(classify)?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(classify)?;
    if bytes.len() > LIMIT {
        return Err(ServiceFailure::LimitExceeded);
    }
    Ok(bytes)
}
pub(super) fn pids(_account: u32) -> Result<Vec<u32>, ServiceFailure> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(classify)? {
        let entry = entry.map_err(classify)?;
        if let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            && pid != 0
        {
            pids.push(pid);
        }
        if pids.len() > 65536 {
            return Err(ServiceFailure::LimitExceeded);
        }
    }
    Ok(pids)
}
fn start(bytes: &[u8], pid: u32) -> Result<u64, ServiceFailure> {
    let close = bytes
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or(ServiceFailure::CleanupUnresolved)?;
    let space = bytes
        .iter()
        .position(|byte| *byte == b' ')
        .ok_or(ServiceFailure::CleanupUnresolved)?;
    let before =
        std::str::from_utf8(&bytes[..space]).map_err(|_| ServiceFailure::CleanupUnresolved)?;
    if before.parse::<u32>().ok() != Some(pid) {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    // stat field 22, after pid and opaque comm. comm may contain non-UTF-8 bytes.
    std::str::from_utf8(&bytes[close + 1..])
        .map_err(|_| ServiceFailure::CleanupUnresolved)?
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0)
        .ok_or(ServiceFailure::CleanupUnresolved)
}
fn uids(bytes: &[u8]) -> Result<[u32; 4], ServiceFailure> {
    let line = bytes
        .split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(b"Uid:"))
        .ok_or(ServiceFailure::CleanupUnresolved)?;
    let text = std::str::from_utf8(line).map_err(|_| ServiceFailure::CleanupUnresolved)?;
    let ids = text
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ServiceFailure::CleanupUnresolved)?;
    ids.try_into()
        .map_err(|_| ServiceFailure::CleanupUnresolved)
}
pub(super) fn observe(
    account: u32,
    pid: u32,
) -> Result<Option<ServiceProcessIdentity>, ServiceFailure> {
    let result = (|| {
        let root = PathBuf::from(format!("/proc/{pid}"));
        // /proc directory ownership changes when a same-account process disables
        // dumpability. The native UID fields, not that ownership, select scope.
        let ids = uids(&read(&root.join("status"))?)?;
        if ids[1] != account {
            return Ok(None);
        }
        let before = start(&read(&root.join("stat"))?, pid)?;
        let executable = std::fs::read_link(root.join("exe")).map_err(classify)?;
        if !executable.is_absolute() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        let after = start(&read(&root.join("stat"))?, pid)?;
        if before != after
            || uids(&read(&root.join("status"))?)? != ids
            || std::fs::read_link(root.join("exe")).map_err(classify)? != executable
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        Ok(Some(ServiceProcessIdentity {
            pid,
            uid: account,
            fully_account_owned: ids == [account; 4],
            start: before,
            executable,
        }))
    })();
    match result {
        Err(ServiceFailure::Missing) => Ok(None),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_start_parser_handles_parentheses_inside_comm() {
        let mut stat = b"41 (a name) with ) parentheses) S".to_vec();
        for field in 4..=21 {
            stat.extend_from_slice(format!(" {field}").as_bytes());
        }
        stat.extend_from_slice(b" 98765 23 24\n");
        assert_eq!(start(&stat, 41), Ok(98765));
        assert_eq!(start(&stat, 42), Err(ServiceFailure::CleanupUnresolved));
    }
}
