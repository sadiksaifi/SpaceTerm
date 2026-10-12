use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};

use super::{
    MAX_PROCESSES, MAX_STATUS_BYTES, MAX_TASKS, NamespaceIdentity, NativeFailure, ProcessPeer,
    TaskFacts, TaskPeer, decode_task,
};

const PROCFS: u32 = 0x9fa0;
const NSFS: u32 = 0x6e736673;
const NS_GET_NSTYPE: libc::c_ulong = 0xb703;
const DIRECTORY_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

pub(super) struct Procfs {
    root: File,
    user_namespace: File,
}

impl Procfs {
    pub(super) fn open() -> Result<Self, NativeFailure> {
        // This checks only permission to obtain a complete inventory. It does
        // not authenticate the observer's namespace as the host initial one.
        // SAFETY: getuid/geteuid have no pointer or ownership preconditions.
        if unsafe { libc::getuid() != 0 || libc::geteuid() != 0 } {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
        Self::open_procfs()
    }

    fn open_procfs() -> Result<Self, NativeFailure> {
        // SAFETY: the fixed name is terminated. No creation flags are used.
        let root = file_from_result(retry(|| unsafe {
            libc::open(c"/proc".as_ptr(), DIRECTORY_FLAGS)
        })?)?;
        require_filesystem(&root, PROCFS)?;
        // thread-self is a kernel link in the authenticated procfs descriptor.
        let thread_self = open_child(&root, c"thread-self", DIRECTORY_FLAGS & !libc::O_NOFOLLOW)?;
        require_filesystem(&thread_self, PROCFS)?;
        let user_namespace = open_user_namespace(&thread_self)?;
        Ok(Self {
            root,
            user_namespace,
        })
    }
}

impl ProcessPeer for Procfs {
    type Task = ProcTask;

    fn retain_context(&self) -> Result<(File, File), NativeFailure> {
        Ok((
            self.root
                .try_clone()
                .map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)?,
            self.user_namespace
                .try_clone()
                .map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)?,
        ))
    }

    fn namespace(&self) -> Result<NamespaceIdentity, NativeFailure> {
        let pinned = namespace_identity(&self.user_namespace)?;
        let thread_self = open_child(
            &self.root,
            c"thread-self",
            DIRECTORY_FLAGS & !libc::O_NOFOLLOW,
        )?;
        let current = open_user_namespace(&thread_self)?;
        if namespace_identity(&current)? != pinned {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
        Ok(pinned)
    }

    fn processes(&self) -> Result<Vec<u32>, NativeFailure> {
        numeric_children(&self.root, MAX_PROCESSES)
    }

    fn tasks(&self, pid: u32) -> Result<Vec<ProcTask>, NativeFailure> {
        let name = numeric_name(pid)?;
        let process = open_child(&self.root, &name, DIRECTORY_FLAGS)?;
        require_filesystem(&process, PROCFS)?;
        let task_root = open_child(&process, c"task", DIRECTORY_FLAGS)?;
        let ids = numeric_children(&task_root, MAX_TASKS)?;
        ids.into_iter()
            .map(|tid| {
                let name = numeric_name(tid)?;
                let directory = open_child(&task_root, &name, DIRECTORY_FLAGS)?;
                require_filesystem(&directory, PROCFS)?;
                Ok(ProcTask {
                    directory,
                    pid,
                    tid,
                })
            })
            .collect()
    }
}

pub(super) struct ProcTask {
    directory: File,
    pid: u32,
    tid: u32,
}

impl TaskPeer for ProcTask {
    fn id(&self) -> u32 {
        self.tid
    }
    fn sample(&self) -> Result<TaskFacts, NativeFailure> {
        // Bracket status and namespace reads with the same retained task's stat
        // record. A vanished/reused task or any partial read is unavailable.
        let before = read_child(&self.directory, c"stat")?;
        let namespace = open_user_namespace(&self.directory)?;
        let status = read_child(&self.directory, c"status")?;
        let after = read_child(&self.directory, c"stat")?;
        let current_namespace = open_user_namespace(&self.directory)?;
        let identity = namespace_identity(&namespace)?;
        if namespace_identity(&current_namespace)? != identity {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
        let first = decode_task(&status, &before, identity)?;
        let second = decode_task(&status, &after, identity)?;
        if first != second || first.pid != self.tid || first.tgid != self.pid {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
        Ok(first)
    }
}

fn read_child(parent: &File, name: &CStr) -> Result<Vec<u8>, NativeFailure> {
    let file = open_child(
        parent,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )?;
    require_filesystem(&file, PROCFS)?;
    let mut bytes = Vec::with_capacity(4096);
    let mut buffer = [0u8; 4096];
    loop {
        // One extra byte distinguishes a full record from an oversized one.
        let available = buffer.len().min(MAX_STATUS_BYTES + 1 - bytes.len());
        // SAFETY: the borrowed descriptor is live, buffer is writable, and the
        // bounded offset cannot exceed 16384 bytes.
        let count = retry(|| unsafe {
            libc::pread(
                file.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                available,
                bytes.len() as libc::off_t,
            )
        })? as usize;
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() > MAX_STATUS_BYTES {
            return Err(NativeFailure::LimitExceeded);
        }
    }
}

fn numeric_children(parent: &File, limit: usize) -> Result<Vec<u32>, NativeFailure> {
    // Reopen '.' through the held procfs directory to obtain an independent
    // directory cursor. Duplicating the descriptor would share its offset.
    let directory = open_child(parent, c".", DIRECTORY_FLAGS)?;
    require_filesystem(&directory, PROCFS)?;
    let mut buffer = [0u8; 8192];
    let mut children = Vec::new();
    let mut records = 0usize;
    loop {
        // SAFETY: buffer supplies writable storage and directory is a retained
        // kernel procfs descriptor. getdents64 is available on Linux 5.15.
        let count = retry(|| unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                directory.as_raw_fd(),
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        })? as usize;
        if count == 0 {
            break;
        }
        let mut offset = 0;
        while offset < count {
            let record = &buffer[offset..count];
            if record.len() < 20 {
                return Err(NativeFailure::LinuxPolicyCheckUnavailable);
            }
            let length = usize::from(u16::from_ne_bytes([record[16], record[17]]));
            if length < 20 || length > record.len() {
                return Err(NativeFailure::LinuxPolicyCheckUnavailable);
            }
            records += 1;
            if records > limit + 512 {
                return Err(NativeFailure::LimitExceeded);
            }
            let tail = &record[19..length];
            let end = tail
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(NativeFailure::LinuxPolicyCheckUnavailable)?;
            let name = &tail[..end];
            if !name.is_empty() && name.iter().all(u8::is_ascii_digit) {
                if children.len() >= limit {
                    return Err(NativeFailure::LimitExceeded);
                }
                let value = std::str::from_utf8(name)
                    .map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)?;
                children.push(super::decimal(value)?);
            }
            offset += length;
        }
    }
    children.sort_unstable();
    Ok(children)
}

fn open_user_namespace(parent: &File) -> Result<File, NativeFailure> {
    let directory = open_child(parent, c"ns", DIRECTORY_FLAGS)?;
    // Only this kernel namespace link is followed. No caller pathname is used.
    let namespace = open_child(&directory, c"user", libc::O_RDONLY | libc::O_CLOEXEC)?;
    require_filesystem(&namespace, NSFS)?;
    // SAFETY: NS_GET_NSTYPE has no pointer argument and owns no resources.
    let kind = retry(|| unsafe { libc::ioctl(namespace.as_raw_fd(), NS_GET_NSTYPE) })?;
    if kind != libc::CLONE_NEWUSER as i64 {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    Ok(namespace)
}

fn namespace_identity(namespace: &File) -> Result<NamespaceIdentity, NativeFailure> {
    let metadata = namespace
        .metadata()
        .map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)?;
    use std::os::unix::fs::MetadataExt;
    Ok(NamespaceIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_filesystem(file: &File, expected: u32) -> Result<(), NativeFailure> {
    let mut status = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is borrowed and status is writable native storage.
    retry(|| unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) })?;
    // SAFETY: fstatfs initialized the output on success.
    if unsafe { status.assume_init() }.f_type as u32 != expected {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    Ok(())
}

fn open_child(parent: &File, name: &CStr, flags: libc::c_int) -> Result<File, NativeFailure> {
    // SAFETY: parent is held, name is terminated, and no creation flags are used.
    file_from_result(retry(|| unsafe {
        libc::openat(parent.as_raw_fd(), name.as_ptr(), flags)
    })?)
}

fn file_from_result(result: i64) -> Result<File, NativeFailure> {
    let descriptor =
        libc::c_int::try_from(result).map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)?;
    // SAFETY: callers supply a fresh successful open/openat descriptor exactly once.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn numeric_name(id: u32) -> Result<CString, NativeFailure> {
    CString::new(id.to_string()).map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)
}

fn retry<T: TryInto<i64>>(mut operation: impl FnMut() -> T) -> Result<i64, NativeFailure> {
    for _ in 0..=4 {
        let result = operation()
            .try_into()
            .map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)?;
        if result >= 0 {
            return Ok(result);
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
    }
    Err(NativeFailure::LinuxPolicyCheckUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_task_status_is_read_through_retained_procfs_descriptors() {
        let peer = Procfs::open_procfs().unwrap();
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        // Procfs task enumeration is not an atomic snapshot of the concurrently
        // changing test harness. Inspect our stable single-threaded child instead.
        let child = OwnedChild(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        let tid = pid;
        let process = open_child(&peer.root, &numeric_name(pid).unwrap(), DIRECTORY_FLAGS).unwrap();
        let tasks = open_child(&process, c"task", DIRECTORY_FLAGS).unwrap();
        assert!(numeric_children(&tasks, MAX_TASKS).unwrap().contains(&tid));
        // Retain the exact live child task through status and namespace inspection.
        let own = ProcTask {
            directory: open_child(&tasks, &numeric_name(tid).unwrap(), DIRECTORY_FLAGS).unwrap(),
            pid,
            tid,
        };
        let observed = own.sample().unwrap();
        assert_eq!(observed.pid, tid);
        assert_eq!(observed.tgid, pid);
        assert!(observed.user_namespace == peer.namespace().unwrap());
        assert_eq!(observed.uids[1], unsafe { libc::geteuid() });
    }
    #[test]
    fn procfs_adapter_refuses_ordinary_files_as_kernel_evidence() {
        let ordinary = tempfile::tempfile().unwrap();
        assert_eq!(
            require_filesystem(&ordinary, PROCFS),
            Err(NativeFailure::LinuxPolicyCheckUnavailable)
        );
    }
    #[test]
    fn unprivileged_full_inventory_is_unavailable() {
        // SAFETY: geteuid has no pointer or ownership preconditions.
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "run this gate without privilege"
        );
        assert!(matches!(
            super::super::ProcessPolicy::observe(1000),
            Err(NativeFailure::LinuxPolicyCheckUnavailable)
        ));
    }
}
