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

pub(super) fn unit_scope(main: u32, cgroup: &str) -> Result<Vec<u32>, ServiceFailure> {
    let root = std::path::Path::new("/sys/fs/cgroup");
    // A missing hierarchy is not evidence that the manager's cgroup is empty.
    // Supported hosts use the unified hierarchy selected by their user manager.
    if !cgroup.is_empty() && !root.join("cgroup.controllers").is_file() {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    cgroup_scope(root, main, cgroup)
}
fn cgroup_scope(
    root: &std::path::Path,
    main: u32,
    cgroup: &str,
) -> Result<Vec<u32>, ServiceFailure> {
    use std::path::Component;
    let mut pids = std::collections::BTreeSet::new();
    if main > i32::MAX as u32 {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    if main != 0 {
        pids.insert(main);
    }
    if cgroup.is_empty() {
        return Ok(pids.into_iter().collect());
    }
    if cgroup.len() > 4096
        || !cgroup.starts_with('/')
        || cgroup == "/"
        || cgroup.as_bytes().contains(&0)
        || cgroup
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || part == "." || part == "..")
        || std::path::Path::new(&cgroup[1..])
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    let mut directories = vec![root.join(&cgroup[1..])];
    let mut visited = 0usize;
    while let Some(directory) = directories.pop() {
        visited += 1;
        if visited > 1024 {
            return Err(ServiceFailure::LimitExceeded);
        }
        let bytes = match read(&directory.join("cgroup.procs")) {
            Ok(bytes) => bytes,
            // A group removed during shutdown owns no processes. The caller
            // rechecks the manager scope before accepting absence.
            Err(ServiceFailure::Missing) => continue,
            Err(error) => return Err(error),
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| ServiceFailure::CleanupUnresolved)?;
        for line in text.lines() {
            let pid = line
                .parse::<u32>()
                .ok()
                .filter(|pid| *pid != 0 && *pid <= i32::MAX as u32)
                .ok_or(ServiceFailure::CleanupUnresolved)?;
            pids.insert(pid);
            if pids.len() > 65536 {
                return Err(ServiceFailure::LimitExceeded);
            }
        }
        // Delegated child cgroups remain in the unit's custody too.
        for entry in std::fs::read_dir(&directory).map_err(classify)? {
            let entry = entry.map_err(classify)?;
            let kind = entry.file_type().map_err(classify)?;
            if kind.is_symlink() {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            if kind.is_dir() {
                directories.push(entry.path());
            }
            if directories.len() + visited > 1024 {
                return Err(ServiceFailure::LimitExceeded);
            }
        }
    }
    Ok(pids.into_iter().collect())
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
    #[test]
    fn unit_scope_includes_delegated_children_but_not_sibling_cgroups() {
        let root = tempfile::tempdir().unwrap();
        for (group, pids) in [
            ("unit", "41\n"),
            ("unit/delegated", "42\n41\n"),
            ("unrelated", "99\n"),
        ] {
            let directory = root.path().join(group);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("cgroup.procs"), pids).unwrap();
        }
        assert_eq!(cgroup_scope(root.path(), 43, "/unit"), Ok(vec![41, 42, 43]));
        std::fs::write(
            root.path().join("unit/delegated/cgroup.procs"),
            "unobservable",
        )
        .unwrap();
        assert_eq!(
            cgroup_scope(root.path(), 43, "/unit"),
            Err(ServiceFailure::CleanupUnresolved)
        );
    }
}

pub(super) fn instance_birth(pid: u32) -> Result<Option<u64>, ServiceFailure> {
    match read(&PathBuf::from(format!("/proc/{pid}/stat"))) {
        Ok(bytes) => start(&bytes, pid).map(Some),
        Err(ServiceFailure::Missing) => Ok(None),
        Err(error) => Err(error),
    }
}
