//! Spawn with only the PTY standard streams, without depending on /dev/fd.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

pub(super) fn spawn(command: &mut Command, slave_path: &Path) -> io::Result<Child> {
    if !command.get_current_dir().is_some_and(Path::is_dir) {
        // Preserve portable-pty's spawn-time HOME fallback. Server admission separately owns
        // restored-directory validation; this native extraction does not change launch policy.
        let home = command
            .get_envs()
            .find_map(|(name, value)| {
                (name == "HOME")
                    .then_some(value)
                    .flatten()
                    .and_then(std::ffi::OsStr::to_str)
            })
            .map(std::path::PathBuf::from)
            .map_or_else(account_home_directory, Ok)?;
        command.current_dir(home);
    }
    // portable-pty owns PTY creation and master I/O, but its spawn operation silently ignores
    // descriptor cleanup errors. Reopen its slave without acquiring the parent's controlling TTY.
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(slave_path)?;
    command
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave);
    let mut descriptors = InheritedDescriptors::prepare()?;
    // SAFETY: the child callback uses only native operations and preallocated storage. It neither
    // allocates nor acquires application locks after fork. Every failed operation aborts exec.
    unsafe {
        command.pre_exec(move || {
            for signal in [
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
            ] {
                if libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(io::Error::last_os_error());
                }
            }
            let mut empty_set = std::mem::zeroed();
            if libc::sigemptyset(&mut empty_set) == -1
                || libc::sigprocmask(libc::SIG_SETMASK, &empty_set, std::ptr::null_mut()) == -1
                || libc::setsid() == -1
                || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1
            {
                return Err(io::Error::last_os_error());
            }
            // Mark rather than close, retaining std::process::Command's exec-error pipe until
            // exec. Only the child's private descriptor table changes, never the parent's flags.
            descriptors.mark_close_on_exec()
        });
    }
    command.spawn()
}

fn account_home_directory() -> io::Result<std::path::PathBuf> {
    let mut buffer = vec![0_u8; 1_024];
    loop {
        let mut account = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: getpwuid_r writes the account and strings into these live caller-owned buffers.
        // This lookup runs before fork and cannot race other account lookups' static storage.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                account.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE {
            let size = buffer
                .len()
                .checked_mul(2)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
            buffer
                .try_reserve_exact(size - buffer.len())
                .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
            buffer.resize(size, 0);
            continue;
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        if result.is_null() {
            return Ok("/".into());
        }
        // SAFETY: successful getpwuid_r initialized the account and its NUL-terminated pw_dir
        // points into buffer, which remains live until the path is copied.
        let home = unsafe { std::ffi::CStr::from_ptr(account.assume_init().pw_dir) };
        return home
            .to_str()
            .map(std::path::PathBuf::from)
            .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ));
    }
}

#[cfg(target_os = "linux")]
struct InheritedDescriptors;

#[cfg(target_os = "linux")]
impl InheritedDescriptors {
    fn prepare() -> io::Result<Self> {
        Ok(Self)
    }

    fn mark_close_on_exec(&mut self) -> io::Result<()> {
        // Linux 5.11+ applies CLOEXEC to the complete descriptor table. No procfs, resource-limit
        // bound, or racy parent-side enumeration is needed. Older or restricted kernels reject
        // launch rather than falling back to best-effort descriptor cleanup.
        // SAFETY: the arguments match Linux close_range's native syscall ABI. Calling the
        // kernel directly avoids imposing a newer glibc symbol requirement on the host crate.
        let result = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
struct InheritedDescriptors {
    entries: Vec<libc::proc_fdinfo>,
    buffer_size: i32,
}

#[cfg(target_os = "macos")]
impl InheritedDescriptors {
    fn prepare() -> io::Result<Self> {
        // SAFETY: a null buffer asks libproc for the storage required for this process's fd list.
        let size = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                std::ptr::null_mut(),
                0,
            )
        };
        if size <= 0 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        let entry_size = std::mem::size_of::<libc::proc_fdinfo>();
        // Reserve headroom for Command's stdio and exec-error descriptors, and concurrent parent
        // activity before fork. The child rejects a full buffer, so growth cannot truncate cleanup.
        let capacity = size as usize / entry_size + 128;
        let buffer_size = capacity
            .checked_mul(entry_size)
            .and_then(|size| i32::try_from(size).ok())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        entries.resize_with(capacity, || libc::proc_fdinfo {
            proc_fd: -1,
            proc_fdtype: 0,
        });
        Ok(Self {
            entries,
            buffer_size,
        })
    }

    fn mark_close_on_exec(&mut self) -> io::Result<()> {
        // SAFETY: the preallocated buffer is writable, correctly aligned, and exactly buffer_size
        // bytes. The forked child has no other thread that can mutate its descriptor table.
        let read = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                self.entries.as_mut_ptr().cast(),
                self.buffer_size,
            )
        };
        if read <= 0 || read >= self.buffer_size || read % libc::PROC_PIDLISTFD_SIZE != 0 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        for entry in &self.entries[..read as usize / std::mem::size_of::<libc::proc_fdinfo>()] {
            if entry.proc_fd < 3 {
                continue;
            }
            // SAFETY: the kernel just reported this live child descriptor. Even an inspection or
            // flag-setting failure rejects exec; missing entries are never silently skipped.
            let flags = unsafe { libc::fcntl(entry.proc_fd, libc::F_GETFD) };
            if flags == -1
                || unsafe { libc::fcntl(entry.proc_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) }
                    == -1
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
