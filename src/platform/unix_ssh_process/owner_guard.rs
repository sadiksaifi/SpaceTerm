use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;

/// A direct child reserves the private group before the command exists. Its only descriptor
/// observes the owner's lifetime; EOF kills its own group, including itself, without a PID lookup.
pub(super) struct OwnerGuard {
    process: libc::pid_t,
    lease: UnixStream,
}

impl OwnerGuard {
    pub(super) fn spawn() -> io::Result<Self> {
        let (lease, watch) = UnixStream::pair()?;
        // Command remaps standard IO before pre_exec. Keep the lifetime writer outside that
        // range even when the application was launched with a closed standard descriptor.
        let lease = if lease.as_raw_fd() < 3 {
            // SAFETY: fcntl duplicates the live socket into a descriptor owned by this call.
            let descriptor = unsafe { libc::fcntl(lease.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
            if descriptor == -1 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the successful fcntl returned a new, exclusively owned socket descriptor.
            let duplicate = unsafe { UnixStream::from_raw_fd(descriptor) };
            drop(lease);
            duplicate
        } else {
            lease
        };
        let ceiling = descriptor_ceiling()?;
        let mut blocked = MaybeUninit::<libc::sigset_t>::uninit();
        let mut previous = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: both sets are valid output buffers. Blocking before fork prevents inherited
        // application handlers and TERM from running in the guard, including during setup.
        unsafe {
            libc::sigfillset(blocked.as_mut_ptr());
        }
        let result = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, blocked.as_ptr(), previous.as_mut_ptr())
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        // SAFETY: the child takes only the syscall-only, diverging watch_owner path. No Rust
        // allocation, destructors, shared state, locks, or application code runs after fork.
        let process = unsafe { libc::fork() };
        if process == 0 {
            unsafe { watch_owner(watch.as_raw_fd(), ceiling) }
        }
        let fork_error = io::Error::last_os_error();
        // SAFETY: the successful pthread_sigmask above initialized previous for this thread.
        let restored = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, previous.as_ptr(), std::ptr::null_mut())
        };
        drop(watch);
        if process == -1 {
            return Err(fork_error);
        }
        let mut guard = Self { process, lease };
        if restored != 0 {
            return Err(io::Error::from_raw_os_error(restored));
        }
        let mut ready = [0];
        guard.lease.read_exact(&mut ready)?;
        if ready != [1] {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(guard)
    }

    pub(super) fn group(&self) -> libc::pid_t {
        self.process
    }

    pub(super) fn lease_fd(&self) -> RawFd {
        self.lease.as_raw_fd()
    }
}

fn descriptor_ceiling() -> io::Result<libc::c_int> {
    // RLIMIT_NOFILE can be unlimited, or lower than an already-open descriptor. Use the
    // kernel's allocation ceiling and include existing higher descriptors if it was lowered.
    // Compute this before fork: the guard must neither allocate nor traverse directories.
    #[cfg(target_os = "macos")]
    let (directory, maximum) = {
        let mut key = [libc::CTL_KERN, libc::KERN_MAXFILESPERPROC];
        let mut maximum = 0 as libc::c_int;
        let mut size = std::mem::size_of_val(&maximum);
        // SAFETY: the key and output buffer have the lengths supplied to sysctl.
        if unsafe {
            libc::sysctl(
                key.as_mut_ptr(),
                2,
                (&mut maximum as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == -1
            || maximum <= 0
        {
            return Err(io::Error::last_os_error());
        }
        ("/dev/fd", maximum)
    };
    #[cfg(target_os = "linux")]
    let (directory, maximum) = {
        // close_range normally makes this bound unnecessary. Retain a complete close-loop
        // fallback for older kernels without requiring a finite hard resource limit.
        let maximum = std::fs::read_to_string("/proc/sys/fs/nr_open")
            .ok()
            .and_then(|text| text.trim().parse::<libc::c_int>().ok())
            .filter(|maximum| *maximum > 0)
            .unwrap_or(libc::c_int::MAX);
        ("/proc/self/fd", maximum)
    };
    let highest = std::fs::read_dir(directory)?.try_fold(maximum, |maximum, entry| {
        let entry = entry?;
        let descriptor = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::c_int>().ok());
        Ok::<_, io::Error>(maximum.max(descriptor.and_then(|fd| fd.checked_add(1)).unwrap_or(0)))
    })?;
    Ok(highest)
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        // Shutdown also revokes writer copies inherited by a concurrent fork. SIGKILL covers
        // ordinary cleanup even if the guard has not reached read yet. Keep its direct-child
        // identity reserved until every signal has been sent, then reap it exactly once.
        let _ = self.lease.shutdown(Shutdown::Write);
        unsafe {
            libc::kill(-self.process, libc::SIGKILL);
        }
        loop {
            let result = unsafe { libc::waitpid(self.process, std::ptr::null_mut(), 0) };
            if result != -1 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

/// Runs only in the forked guard with every maskable signal blocked.
unsafe fn watch_owner(watch: RawFd, ceiling: libc::c_int) -> ! {
    // SAFETY: these syscalls affect only this fork's descriptor table and process group.
    // Moving the sole lease to fd 0 lets every inherited writer, capture pipe and runtime
    // descriptor be closed, with no directory traversal or allocation after fork.
    unsafe {
        if libc::setpgid(0, 0) == -1 || (watch != 0 && libc::dup2(watch, 0) == -1) {
            libc::_exit(1);
        }
        close_inherited_descriptors(ceiling);
        let ready = [1_u8];
        if libc::write(0, ready.as_ptr().cast(), 1) != 1 {
            libc::kill(0, libc::SIGKILL);
            libc::_exit(1);
        }
        let mut byte = 0_u8;
        // No data is sent after readiness. A read error is also a lost lease, so fail closed.
        libc::read(0, (&mut byte as *mut u8).cast(), 1);
        // Membership itself reserves this group. Never signal a stored or probed PID here.
        libc::kill(0, libc::SIGKILL);
        libc::_exit(1);
    }
}

unsafe fn close_inherited_descriptors(ceiling: libc::c_int) {
    #[cfg(target_os = "linux")]
    // SAFETY: close_range operates only on this fork's private descriptor table. Use the raw
    // syscall so older glibc versions remain supported; older kernels take the close loop.
    if unsafe { libc::syscall(libc::SYS_close_range, 1_u32, u32::MAX, 0_u32) } == 0 {
        return;
    }
    for descriptor in 1..ceiling {
        // SAFETY: close is async-signal-safe, including for unused descriptors.
        unsafe {
            libc::close(descriptor);
        }
    }
}
