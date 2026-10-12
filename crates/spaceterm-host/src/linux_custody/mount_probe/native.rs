use super::*;
use std::{
    io,
    mem::{MaybeUninit, size_of},
    os::fd::AsRawFd,
    sync::atomic::{AtomicU32, Ordering},
};

const CLEANUP_ATTEMPTS: u32 = 32;
const CLEANUP_TIME: Duration = Duration::from_millis(100);
const IDMAP: u64 = 0x0010_0000;
const OPEN_TREE_CLONE: u32 = 1;

pub(in crate::linux_custody) struct KernelProbe {
    deadline: Instant,
}

impl KernelProbe {
    pub(super) fn new(deadline: Instant) -> Self {
        Self { deadline }
    }
}

impl ProbePeer for KernelProbe {
    fn identity(&self, original: &File) -> Result<OriginalIdentity, NativeFailure> {
        if Instant::now() >= self.deadline {
            return Err(NativeFailure::TimedOut);
        }
        let identity = descriptor_identity(original.as_raw_fd())?;
        if Instant::now() >= self.deadline {
            return Err(NativeFailure::TimedOut);
        }
        Ok(identity)
    }

    fn apply_to_private_old_clone(&self, original: &File) -> Result<(), NativeFailure> {
        // fork's child performs only raw syscalls and stack-only Rust operations.
        // Refuse callers with other threads, rather than using Rust after a fork
        // from a multithreaded manager. No credential is present in this operation.
        if unsafe { libc::geteuid() } != 0 {
            return Err(NativeFailure::LinuxMountWitnessUnavailable);
        }
        let signals = fork_scope()?;
        let shared = SharedWork::new(self.deadline)?;
        let work = shared.work();
        let mut pipe = [-1; 2];
        work.call(
            || unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } as i64,
        )?;
        let reader = Descriptor(pipe[0], &work);
        let writer = Descriptor(pipe[1], &work);
        let parent = unsafe { libc::getpid() };
        let pid = work.call(|| unsafe { libc::fork() } as i64)? as libc::pid_t;
        if pid == 0 {
            reader.close_in_child();
            let result = probe_child(original.as_raw_fd(), parent, &work);
            let code = match result {
                Ok(()) => 0,
                Err(error) => error as u32,
            };
            let bytes = code.to_ne_bytes();
            let written = work.cleanup(|| unsafe {
                libc::write(writer.0, bytes.as_ptr().cast(), bytes.len()) as i64
            });
            writer.close_in_child();
            // No parent-owned Rust destructors or buffered output in either child.
            unsafe { libc::_exit(if written == Ok(4) { 0 } else { 1 }) }
        }
        let writer_close = writer.close();
        let pidfd = work.call(|| unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i64);
        let outcome = match pidfd {
            Ok(fd) => {
                let pidfd = Descriptor(fd as i32, &work);
                let outcome = writer_close.and_then(|()| read_result(reader.0, &work));
                let cleanup = retire(pid, Some(pidfd.0), &work);
                let close = pidfd.close();
                cleanup.and(close).and(outcome)
            }
            // Without a completed worker report, nested namespace-child
            // retirement is unproven even if the direct child is exactly reaped.
            Err(_) => retire(pid, None, &work).and(Err(NativeFailure::CleanupUnresolved)),
        };
        let descriptors = reader.close();
        let signals = signals.restore();
        descriptors.and(signals).and(outcome)
    }
}

fn fork_scope() -> Result<BlockedSignals, NativeFailure> {
    let signals = BlockedSignals::new()?;
    require_single_thread()?;
    // No other thread or unblocked handler can change this policy before either
    // fork. The nested worker inherits it and never installs another disposition.
    require_waitable_children()?;
    Ok(signals)
}

fn require_waitable_children() -> Result<(), NativeFailure> {
    let mut action = MaybeUninit::<libc::sigaction>::uninit();
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    let action = unsafe { action.assume_init() };
    if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    Ok(())
}

struct BlockedSignals(Option<libc::sigset_t>);
impl BlockedSignals {
    fn new() -> Result<Self, NativeFailure> {
        let mut all = MaybeUninit::<libc::sigset_t>::uninit();
        let mut previous = MaybeUninit::<libc::sigset_t>::uninit();
        // Only this calling thread's mask changes. SIGKILL remains effective in
        // both fork levels. No user signal handler may create a thread mid-probe.
        if unsafe { libc::sigfillset(all.as_mut_ptr()) } != 0
            || unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, all.as_ptr(), previous.as_mut_ptr())
            } != 0
        {
            return Err(NativeFailure::LinuxMountWitnessUnavailable);
        }
        Ok(Self(Some(unsafe { previous.assume_init() })))
    }
    fn restore(mut self) -> Result<(), NativeFailure> {
        if let Some(previous) = self.0.take()
            && unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) }
                != 0
        {
            return Err(NativeFailure::CleanupUnresolved);
        }
        Ok(())
    }
}
impl Drop for BlockedSignals {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
            }
        }
    }
}

fn require_single_thread() -> Result<(), NativeFailure> {
    // Two fixed reads establish both the complete listing and EOF. This happens
    // with signals blocked, before fork and before the shared budget exists.
    let fd = unsafe {
        libc::open(
            c"/proc/self/task".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    let result = (|| {
        let mut bytes = [0u8; 1024];
        let length =
            unsafe { libc::syscall(libc::SYS_getdents64, fd, bytes.as_mut_ptr(), bytes.len()) };
        if !(1..1024).contains(&length) {
            return Err(NativeFailure::LinuxMountWitnessUnavailable);
        }
        let mut offset = 0;
        let mut tasks = 0;
        while offset < length as usize {
            if offset + 20 > length as usize {
                return Err(NativeFailure::LinuxMountWitnessUnavailable);
            }
            let size = u16::from_ne_bytes([bytes[offset + 16], bytes[offset + 17]]) as usize;
            if size < 20 || offset + size > length as usize {
                return Err(NativeFailure::LinuxMountWitnessUnavailable);
            }
            let name = &bytes[offset + 19..offset + size];
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(NativeFailure::LinuxMountWitnessUnavailable)?;
            if name[..end].iter().all(u8::is_ascii_digit) && end != 0 {
                tasks += 1;
            } else if &name[..end] != b"." && &name[..end] != b".." {
                return Err(NativeFailure::LinuxMountWitnessUnavailable);
            }
            offset += size;
        }
        if tasks != 1
            || unsafe { libc::syscall(libc::SYS_getdents64, fd, bytes.as_mut_ptr(), bytes.len()) }
                != 0
        {
            return Err(NativeFailure::LinuxMountWitnessUnavailable);
        }
        Ok(())
    })();
    let closed = unsafe { libc::close(fd) };
    if closed != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
        return Err(NativeFailure::CleanupUnresolved);
    }
    result
}

/// Retains the original f_path. No mountinfo pathname matching or clone identity.
pub(in crate::linux_custody) fn descriptor_identity(
    fd: i32,
) -> Result<OriginalIdentity, NativeFailure> {
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    // This producer supports ext4 only. In particular, a successful query of
    // tmpfs, an overlay, or another IDMAP-capable filesystem is not this profile.
    if unsafe { libc::fstatfs(fd, filesystem.as_mut_ptr()) } != 0
        || unsafe { filesystem.assume_init() }.f_type as u64 != 0xef53
    {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    let mut status = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: AT_EMPTY_PATH queries the retained descriptor; storage is statx's ABI.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
            libc::STATX_INO | libc::STATX_MNT_ID,
            status.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    let status = unsafe { status.assume_init() };
    if status.stx_mask & (libc::STATX_INO | libc::STATX_MNT_ID)
        != libc::STATX_INO | libc::STATX_MNT_ID
        || status.stx_mnt_id == 0
        || status.stx_mnt_id > u64::from(u32::MAX)
        || status.stx_ino == 0
    {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    Ok(OriginalIdentity {
        device: libc::makedev(status.stx_dev_major, status.stx_dev_minor),
        inode: status.stx_ino,
        mount: status.stx_mnt_id,
    })
}

#[repr(C)]
struct Counters {
    calls: AtomicU32,
    cleanup: AtomicU32,
    interrupts: AtomicU32,
}

/// Shared between the two fork levels. A fork cannot reset any native budget.
struct SharedWork {
    counters: *mut Counters,
    deadline: Instant,
}
impl SharedWork {
    fn new(deadline: Instant) -> Result<Self, NativeFailure> {
        if Instant::now() >= deadline - CLEANUP_TIME {
            return Err(NativeFailure::TimedOut);
        }
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size_of::<Counters>(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(NativeFailure::LinuxMountWitnessUnavailable);
        }
        let counters = address.cast::<Counters>();
        unsafe {
            counters.write(Counters {
                // Conservatively prepay the fixed preflight, original samples,
                // getpid/getppid, both _exit calls, and signal-mask restoration.
                // Descriptor closes, signals and reaps have a separate reserve.
                calls: AtomicU32::new(25),
                cleanup: AtomicU32::new(0),
                interrupts: AtomicU32::new(0),
            });
        }
        Ok(Self { counters, deadline })
    }
    fn work(&self) -> Work<'_> {
        Work {
            counters: unsafe { &*self.counters },
            deadline: self.deadline,
        }
    }
}
impl Drop for SharedWork {
    fn drop(&mut self) {
        // The fixed mapping close is prepaid. An unresolved child retains its
        // own mapping; releasing the parent's mapping cannot mint a witness.
        unsafe {
            libc::munmap(self.counters.cast(), size_of::<Counters>());
        }
    }
}

struct Work<'a> {
    counters: &'a Counters,
    deadline: Instant,
}
impl Work<'_> {
    fn call(&self, operation: impl FnMut() -> i64) -> Result<i64, NativeFailure> {
        self.invoke(false, operation)
    }
    fn cleanup(&self, operation: impl FnMut() -> i64) -> Result<i64, NativeFailure> {
        self.invoke(true, operation)
    }
    fn invoke(
        &self,
        cleanup: bool,
        mut operation: impl FnMut() -> i64,
    ) -> Result<i64, NativeFailure> {
        loop {
            let (counter, limit) = if cleanup {
                (&self.counters.cleanup, CLEANUP_ATTEMPTS - 1) // munmap prepaid
            } else {
                (&self.counters.calls, MAX_ATTEMPTS - CLEANUP_ATTEMPTS)
            };
            if counter
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    (count < limit).then_some(count + 1)
                })
                .is_err()
            {
                return Err(NativeFailure::LimitExceeded);
            }
            if !cleanup && Instant::now() >= self.deadline - CLEANUP_TIME {
                return Err(NativeFailure::TimedOut);
            }
            let result = operation();
            if result >= 0 {
                return Ok(result);
            }
            if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return Err(if cleanup {
                    NativeFailure::CleanupUnresolved
                } else {
                    NativeFailure::LinuxMountWitnessUnavailable
                });
            }
            if self
                .counters
                .interrupts
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    (count < MAX_INTERRUPTS).then_some(count + 1)
                })
                .is_err()
            {
                return Err(if cleanup {
                    NativeFailure::CleanupUnresolved
                } else {
                    NativeFailure::LimitExceeded
                });
            }
        }
    }
    fn poll(&self, fd: i32, cleanup: bool) -> Result<(), NativeFailure> {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let limit = if cleanup {
            self.deadline
        } else {
            self.deadline - CLEANUP_TIME
        };
        let mut operation = || {
            let remaining = limit.saturating_duration_since(Instant::now()).as_millis();
            unsafe { libc::poll(&mut poll, 1, remaining.min(2000) as i32) as i64 }
        };
        let result = if cleanup {
            self.cleanup(&mut operation)
        } else {
            self.call(&mut operation)
        }?;
        if result == 0 {
            return Err(if cleanup {
                NativeFailure::CleanupUnresolved
            } else {
                NativeFailure::TimedOut
            });
        }
        if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(NativeFailure::LinuxMountWitnessUnavailable);
        }
        Ok(())
    }
}

struct Descriptor<'a>(i32, &'a Work<'a>);
impl Descriptor<'_> {
    fn close(mut self) -> Result<(), NativeFailure> {
        let fd = std::mem::replace(&mut self.0, -1);
        self.close_descriptor(fd)
    }
    fn close_descriptor(&self, fd: i32) -> Result<(), NativeFailure> {
        self.1
            .cleanup(|| {
                let result = unsafe { libc::close(fd) };
                // Linux releases the descriptor even when close reports EINTR. Never
                // retry close against a potentially recycled descriptor.
                if result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    0
                } else {
                    -1
                }
            })
            .map(|_| ())
    }
    fn close_in_child(&self) {
        let _ = self.close_descriptor(self.0);
    }
}
impl Drop for Descriptor<'_> {
    fn drop(&mut self) {
        if self.0 >= 0 {
            self.close_in_child();
        }
    }
}

fn read_result(fd: i32, work: &Work<'_>) -> Result<(), NativeFailure> {
    work.poll(fd, false)
        .map_err(|_| NativeFailure::CleanupUnresolved)?;
    let mut bytes = [0u8; 5];
    let count = work
        .call(|| unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) as i64 })
        .map_err(|_| NativeFailure::CleanupUnresolved)?;
    decode_result(&bytes, count)
}

fn decode_result(bytes: &[u8; 5], count: i64) -> Result<(), NativeFailure> {
    // Only a complete report proves that the worker's namespace child retired.
    if count != 4 {
        return Err(NativeFailure::CleanupUnresolved);
    }
    let code = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    match code {
        0 => Ok(()),
        code if [
            NativeFailure::LinuxMountWitnessUnavailable as u32,
            NativeFailure::TimedOut as u32,
            NativeFailure::LimitExceeded as u32,
            NativeFailure::CleanupUnresolved as u32,
        ]
        .contains(&code) =>
        {
            Err(NativeFailure::try_from(code).map_err(|_| NativeFailure::CleanupUnresolved)?)
        }
        _ => Err(NativeFailure::CleanupUnresolved),
    }
}

fn retire(pid: libc::pid_t, pidfd: Option<i32>, work: &Work<'_>) -> Result<(), NativeFailure> {
    // The direct child is not reaped until after signalling. Even when pidfd_open
    // fails, wait ownership prevents PID reuse. No process-group signalling.
    let kill = work.cleanup(|| {
        let result = match pidfd {
            Some(fd) => unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd,
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                ) as i64
            },
            None => unsafe { libc::kill(pid, libc::SIGKILL) as i64 },
        };
        if result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            0
        } else {
            result
        }
    });
    if let Some(fd) = pidfd {
        work.poll(fd, true)?;
    }
    let mut status = 0;
    let reaped =
        work.cleanup(|| unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) as i64 })?;
    if reaped != i64::from(pid) {
        return Err(NativeFailure::CleanupUnresolved);
    }
    kill.map(|_| ())
}

fn probe_child(original: i32, parent: libc::pid_t, work: &Work<'_>) -> Result<(), NativeFailure> {
    work.call(|| unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) as i64 })?;
    if unsafe { libc::getppid() } != parent {
        return Err(NativeFailure::CleanupUnresolved);
    }
    work.call(|| unsafe { libc::unshare(libc::CLONE_NEWNS) as i64 })?;
    work.call(|| unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        ) as i64
    })?;
    let namespace = user_namespace(work)?;
    let clone = work.call(|| unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            original,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH as u32 | OPEN_TREE_CLONE | libc::O_CLOEXEC as u32,
        ) as i64
    })?;
    let clone = Descriptor(clone as i32, work);
    #[repr(C)]
    struct MountAttr {
        set: u64,
        clear: u64,
        propagation: u64,
        userns: u64,
    }
    let attr = MountAttr {
        set: IDMAP,
        clear: 0,
        propagation: 0,
        userns: namespace.0 as u64,
    };
    let application = work.call(|| unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            clone.0,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            &attr as *const MountAttr,
            size_of::<MountAttr>(),
        ) as i64
    });
    // IDMAP succeeds only on a detached, previously non-idmapped mount. This
    // clone is never attached, exported, or sent to an account process.
    clone.close()?;
    namespace.close()?;
    application.map(|_| ())
}

fn user_namespace<'a>(work: &'a Work<'a>) -> Result<Descriptor<'a>, NativeFailure> {
    let mut ready = [-1; 2];
    let mut finish = [-1; 2];
    work.call(|| unsafe {
        libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) as i64
    })?;
    let ready_read = Descriptor(ready[0], work);
    let ready_write = Descriptor(ready[1], work);
    work.call(|| unsafe {
        libc::pipe2(finish.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) as i64
    })?;
    let finish_read = Descriptor(finish[0], work);
    let finish_write = Descriptor(finish[1], work);
    let parent = unsafe { libc::getpid() };
    let pid = work.call(|| unsafe { libc::fork() as i64 })? as libc::pid_t;
    if pid == 0 {
        ready_read.close_in_child();
        finish_write.close_in_child();
        let setup = work
            .call(|| unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) as i64 })
            .and_then(|_| {
                if unsafe { libc::getppid() } != parent {
                    return Err(NativeFailure::CleanupUnresolved);
                }
                work.call(|| unsafe { libc::unshare(libc::CLONE_NEWUSER) as i64 })?;
                // Reinstall the guard after the credential transition, then
                // close the parent-death race before announcing the namespace.
                work.call(|| unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) as i64 })?;
                if unsafe { libc::getppid() } != parent {
                    return Err(NativeFailure::CleanupUnresolved);
                }
                Ok(0)
            });
        let byte = [u8::from(setup.is_ok())];
        let _ = work.call(|| unsafe { libc::write(ready_write.0, byte.as_ptr().cast(), 1) as i64 });
        if setup.is_ok() {
            let _ = work.poll(finish_read.0, false);
        }
        unsafe { libc::_exit(if setup.is_ok() { 0 } else { 1 }) }
    }
    let pipe_closes = ready_write.close().and(finish_read.close());
    let pidfd = work.call(|| unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i64 });
    let result = (|| {
        pipe_closes?;
        work.poll(ready_read.0, false)?;
        let mut byte = [0];
        if work.call(|| unsafe { libc::read(ready_read.0, byte.as_mut_ptr().cast(), 1) as i64 })?
            != 1
            || byte != [1]
        {
            return Err(NativeFailure::LinuxMountWitnessUnavailable);
        }
        // Namespace UID/GID zero maps to host nobody, never host root. The
        // mapping is nontrivial; the clone stays private even for a nobody account.
        write_proc_map(pid, b"setgroups", b"deny\n", work)?;
        write_proc_map(pid, b"uid_map", b"0 65534 1\n", work)?;
        write_proc_map(pid, b"gid_map", b"0 65534 1\n", work)?;
        let path = proc_path(pid, b"ns/user")?;
        let fd = work.call(|| unsafe {
            libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) as i64
        })?;
        Ok(Descriptor(fd as i32, work))
    })();
    let pipe_closes = finish_write.close().and(ready_read.close());
    let cleanup = match pidfd {
        Ok(fd) => {
            let fd = Descriptor(fd as i32, work);
            let cleanup = retire(pid, Some(fd.0), work);
            cleanup.and(fd.close())
        }
        Err(_) => retire(pid, None, work),
    };
    match (cleanup.and(pipe_closes), result) {
        (Ok(()), result) => result,
        (Err(_), _) => Err(NativeFailure::CleanupUnresolved),
    }
}

fn proc_path(pid: libc::pid_t, suffix: &[u8]) -> Result<[u8; 64], NativeFailure> {
    if pid <= 0 || suffix.len() > 32 {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    let mut path = [0; 64];
    path[..6].copy_from_slice(b"/proc/");
    let mut digits = [0; 10];
    let mut count = 0;
    let mut number = pid as u32;
    while number > 0 {
        digits[count] = b'0' + (number % 10) as u8;
        number /= 10;
        count += 1;
    }
    for index in 0..count {
        path[6 + index] = digits[count - index - 1];
    }
    path[6 + count] = b'/';
    path[7 + count..7 + count + suffix.len()].copy_from_slice(suffix);
    Ok(path)
}

fn write_proc_map(
    pid: libc::pid_t,
    suffix: &[u8],
    bytes: &[u8],
    work: &Work<'_>,
) -> Result<(), NativeFailure> {
    let path = proc_path(pid, suffix)?;
    let fd = work.call(|| unsafe {
        libc::open(
            path.as_ptr().cast(),
            libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        ) as i64
    })?;
    let fd = Descriptor(fd as i32, work);
    let result =
        work.call(|| unsafe { libc::write(fd.0, bytes.as_ptr().cast(), bytes.len()) as i64 });
    fd.close()?;
    if result? == bytes.len() as i64 {
        Ok(())
    } else {
        Err(NativeFailure::LinuxMountWitnessUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn counters() -> Counters {
        Counters {
            calls: AtomicU32::new(0),
            cleanup: AtomicU32::new(0),
            interrupts: AtomicU32::new(0),
        }
    }
    #[test]
    fn probe_rejects_auto_reaping_before_creating_a_child() {
        if let Ok(policy) = std::env::var("SPACETERM_TEST_REAPING_POLICY") {
            exercise_reaping_policy(&policy);
            return;
        }
        for policy in ["ignored", "no-cld-wait", "waitable"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "linux_custody::mount_probe::native::tests::probe_rejects_auto_reaping_before_creating_a_child", "--nocapture"])
                .env("SPACETERM_TEST_REAPING_POLICY", policy)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn exercise_reaping_policy(policy: &str) {
        assert!(["ignored", "no-cld-wait", "waitable"].contains(&policy));
        // This process owns the changed disposition. No other test shares it.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = if policy == "ignored" {
            libc::SIG_IGN
        } else {
            libc::SIG_DFL
        };
        if policy == "no-cld-wait" {
            action.sa_flags = libc::SA_NOCLDWAIT;
        }
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) },
            0
        );
        let result = BlockedSignals::new().and_then(|signals| {
            require_waitable_children()?;
            if policy == "waitable" {
                // Force exit before pidfd acquisition while preserving wait
                // ownership. The exact zombie cannot be recycled at either fork
                // level, including the numeric-PID cleanup fallback.
                for with_pidfd in [true, false] {
                    let pid = unsafe { libc::fork() };
                    assert!(pid >= 0);
                    if pid == 0 {
                        unsafe { libc::_exit(0) }
                    }
                    let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
                    assert_eq!(
                        unsafe {
                            libc::waitid(
                                libc::P_PID,
                                pid as u32,
                                info.as_mut_ptr(),
                                libc::WEXITED | libc::WNOWAIT,
                            )
                        },
                        0
                    );
                    let counters = counters();
                    let work = Work {
                        counters: &counters,
                        deadline: Instant::now() + DEADLINE,
                    };
                    if with_pidfd {
                        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
                        assert!(fd >= 0);
                        let fd = Descriptor(fd, &work);
                        assert_eq!(retire(pid, Some(fd.0), &work), Ok(()));
                        assert_eq!(fd.close(), Ok(()));
                    } else {
                        assert_eq!(retire(pid, None, &work), Ok(()));
                    }
                    assert_eq!(
                        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
                        -1
                    );
                    assert_eq!(
                        io::Error::last_os_error().raw_os_error(),
                        Some(libc::ECHILD)
                    );
                }
            }
            signals.restore()
        });
        assert_eq!(
            result,
            if policy == "waitable" {
                Ok(())
            } else {
                Err(NativeFailure::LinuxMountWitnessUnavailable)
            }
        );
    }

    #[test]
    fn acquisition_and_reserved_cleanup_cannot_exceed_128_attempts() {
        let counters = counters();
        let work = Work {
            counters: &counters,
            deadline: Instant::now() + DEADLINE,
        };
        let calls = Cell::new(0);
        for _ in 0..96 {
            assert!(
                work.call(|| {
                    calls.set(calls.get() + 1);
                    0
                })
                .is_ok()
            );
        }
        assert_eq!(
            work.call(|| {
                calls.set(calls.get() + 1);
                0
            }),
            Err(NativeFailure::LimitExceeded)
        );
        for _ in 0..31 {
            assert!(
                work.cleanup(|| {
                    calls.set(calls.get() + 1);
                    0
                })
                .is_ok()
            );
        }
        assert_eq!(
            work.cleanup(|| {
                calls.set(calls.get() + 1);
                0
            }),
            Err(NativeFailure::LimitExceeded)
        );
        assert_eq!(calls.get() + 1, 128); // single prepaid munmap
    }
    #[test]
    fn acquisition_shares_four_interruption_retries_across_operations() {
        let counters = counters();
        let work = Work {
            counters: &counters,
            deadline: Instant::now() + DEADLINE,
        };
        let calls = Cell::new(0);
        let operation = || {
            calls.set(calls.get() + 1);
            unsafe {
                *libc::__errno_location() = libc::EINTR;
            }
            -1
        };
        assert_eq!(work.call(operation), Err(NativeFailure::LimitExceeded));
        assert_eq!(calls.get(), 5);
    }
    #[test]
    fn expired_acquisition_never_calls_the_peer_but_preserves_cleanup() {
        let counters = counters();
        let work = Work {
            counters: &counters,
            deadline: Instant::now(),
        };
        let called = Cell::new(false);
        assert_eq!(
            work.call(|| {
                called.set(true);
                0
            }),
            Err(NativeFailure::TimedOut)
        );
        assert!(!called.get());
        assert_eq!(work.cleanup(|| 0), Ok(0));
    }
    #[test]
    fn incomplete_or_foreign_worker_reports_leave_cleanup_unresolved() {
        for count in [0, 1, 3, 5] {
            assert_eq!(
                decode_result(&[0; 5], count),
                Err(NativeFailure::CleanupUnresolved)
            );
        }
        for code in [1u32, 47, u32::MAX] {
            let mut bytes = [0; 5];
            bytes[..4].copy_from_slice(&code.to_ne_bytes());
            assert_eq!(
                decode_result(&bytes, 4),
                Err(NativeFailure::CleanupUnresolved)
            );
        }
        assert_eq!(decode_result(&[0; 5], 4), Ok(()));
    }

    #[test]
    fn native_original_samples_share_the_acquisition_deadline() {
        let original = File::open("/").unwrap();
        assert_eq!(
            KernelProbe::new(Instant::now()).identity(&original),
            Err(NativeFailure::TimedOut)
        );
    }

    #[test]
    fn native_unprivileged_probe_refuses_without_mount_or_namespace_changes() {
        assert_ne!(unsafe { libc::geteuid() }, 0);
        use std::os::unix::fs::MetadataExt;
        let namespace = File::open("/proc/thread-self/ns/mnt").unwrap();
        let before = namespace.metadata().unwrap().ino();
        let original = File::open("/").unwrap();
        assert_eq!(
            KernelProbe::new(Instant::now() + DEADLINE).apply_to_private_old_clone(&original),
            Err(NativeFailure::LinuxMountWitnessUnavailable)
        );
        assert!(matches!(
            super::acquire_native(original),
            Err(NativeFailure::LinuxMountWitnessUnavailable)
        ));
        assert_eq!(namespace.metadata().unwrap().ino(), before);
    }
}
