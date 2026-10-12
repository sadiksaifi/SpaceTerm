//! Owned anonymous pipes, post-exec receiver setup and exact child reaping.
//!
//! The receiver result proves wire completion and cleanup only. Native startup
//! authority must be revalidated before authenticated readiness. This module does
//! not retrieve credentials, mint release permits, select executables or signal
//! a root process from an account process.

use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;
use std::time::Duration;

use zeroize::Zeroize;

use super::{
    ExchangeDeadline, FRAME_BYTES, FrameKind, NativeFailure, ProcessIdentity, StartupTranscript,
};

/// An anonymous pipe endpoint owned exclusively by an exchange phase.
pub struct PipeEndpoint(Option<OwnedFd>);
impl fmt::Debug for PipeEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PipeEndpoint([redacted])")
    }
}
impl PipeEndpoint {
    /// Takes an owned anonymous pipe, verifies direction and sets CLOEXEC first.
    pub fn take(fd: OwnedFd, writing: bool) -> Result<Self, NativeFailure> {
        let raw = fd.as_raw_fd();
        // SAFETY: stat and fcntl operate on a live exclusively owned descriptor.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(raw, &mut stat) } != 0
            || stat.st_mode & libc::S_IFMT != libc::S_IFIFO
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        let expected = if writing {
            libc::O_WRONLY
        } else {
            libc::O_RDONLY
        };
        if flags < 0 || flags & libc::O_ACCMODE != expected {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let descriptor_flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        if descriptor_flags < 0
            || unsafe { libc::fcntl(raw, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) } != 0
            || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0
        {
            return Err(NativeFailure::IoUnavailable);
        }
        Ok(Self(Some(fd)))
    }

    /// Reads one bounded frame followed by EOF. An extra byte always rejects.
    /// Partial output and EINTR share the original absolute deadline.
    pub fn read_frame<const N: usize>(
        &mut self,
        destination: &mut LockedMemory<N>,
        expected: usize,
        deadline: ExchangeDeadline,
    ) -> Result<(), NativeFailure> {
        let result = (|| {
            if expected >= N {
                return Err(NativeFailure::LimitExceeded);
            }
            let raw = self.raw()?;
            let mut used = 0;
            loop {
                deadline.remaining()?;
                // SAFETY: the slice has room for exactly the frame plus one probe.
                let count = unsafe {
                    libc::read(
                        raw,
                        destination.bytes_mut()[used..expected + 1]
                            .as_mut_ptr()
                            .cast(),
                        expected + 1 - used,
                    )
                };
                if count > 0 {
                    used += count as usize;
                    if used > expected {
                        return Err(NativeFailure::InvalidRecord);
                    }
                } else if count == 0 {
                    deadline.remaining()?;
                    return if used == expected {
                        Ok(())
                    } else {
                        Err(NativeFailure::CredentialUnavailable)
                    };
                } else {
                    match io::Error::last_os_error().raw_os_error() {
                        Some(libc::EINTR) => continue,
                        Some(libc::EAGAIN) => wait(raw, libc::POLLIN, deadline)?,
                        _ => return Err(NativeFailure::IoUnavailable),
                    }
                }
            }
        })();
        self.0.take();
        if result.is_err() {
            destination.wipe();
        }
        result
    }

    /// Writes the frame and closes its unique writer, so the peer must see EOF.
    pub fn write_frame(
        &mut self,
        bytes: &[u8],
        deadline: ExchangeDeadline,
    ) -> Result<(), NativeFailure> {
        let raw = self.raw()?;
        let _signal = BlockedSigpipe::enter()?;
        let mut used = 0;
        let result = (|| {
            while used < bytes.len() {
                deadline.remaining()?;
                // SAFETY: the remaining slice is readable for its full length.
                let count =
                    unsafe { libc::write(raw, bytes[used..].as_ptr().cast(), bytes.len() - used) };
                if count > 0 {
                    used += count as usize;
                } else if count < 0 {
                    match io::Error::last_os_error().raw_os_error() {
                        Some(libc::EINTR) => continue,
                        Some(libc::EAGAIN) => wait(raw, libc::POLLOUT, deadline)?,
                        Some(libc::EPIPE) => return Err(NativeFailure::CredentialUnavailable),
                        _ => return Err(NativeFailure::IoUnavailable),
                    }
                } else {
                    return Err(NativeFailure::IoUnavailable);
                }
            }
            Ok(())
        })();
        self.0.take();
        result
    }

    fn raw(&self) -> Result<RawFd, NativeFailure> {
        self.0
            .as_ref()
            .map(AsRawFd::as_raw_fd)
            .ok_or(NativeFailure::UnsafeNativeState)
    }
}

/// Creates a private anonymous pipe with CLOEXEC on both ends.
pub fn anonymous_pipe() -> Result<(OwnedFd, OwnedFd), NativeFailure> {
    let mut descriptors = [-1; 2];
    // SAFETY: pipe initializes both entries on success. The launcher is single-threaded.
    #[cfg(target_os = "linux")]
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::pipe(descriptors.as_mut_ptr()) };
    if result != 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    // SAFETY: successful pipe returned two fresh owned descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    for fd in [&read, &write] {
        let raw = fd.as_raw_fd();
        if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
    }
    Ok((read, write))
}

/// Locked, anonymous, noncloneable bytes. No debug output exposes contents.
/// Allocation locks the complete mapping before any secret enters it.
pub struct LockedMemory<const N: usize> {
    pointer: NonNull<u8>,
    mapping_length: usize,
}
impl<const N: usize> fmt::Debug for LockedMemory<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LockedMemory([redacted])")
    }
}
impl<const N: usize> LockedMemory<N> {
    /// Allocates, excludes from core dumps where available and locks one bounded mapping.
    pub fn allocate() -> Result<Self, NativeFailure> {
        if N == 0 || N > 131_072 {
            return Err(NativeFailure::LimitExceeded);
        }
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return Err(NativeFailure::NativeUnavailable);
        }
        let length = (N.div_ceil(page_size as usize)) * page_size as usize;
        // SAFETY: anonymous private mmap needs no descriptor and returns fresh memory.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(NativeFailure::IoUnavailable);
        }
        #[cfg(target_os = "linux")]
        if unsafe { libc::madvise(raw, length, libc::MADV_DONTDUMP) } != 0 {
            unsafe { libc::munmap(raw, length) };
            return Err(NativeFailure::NativeUnavailable);
        }
        if unsafe { libc::mlock(raw, length) } != 0 {
            unsafe { libc::munmap(raw, length) };
            return Err(NativeFailure::NativeUnavailable);
        }
        let Some(pointer) = NonNull::new(raw.cast()) else {
            unsafe {
                libc::munlock(raw, length);
                libc::munmap(raw, length);
            }
            return Err(NativeFailure::IoUnavailable);
        };
        Ok(Self {
            pointer,
            mapping_length: length,
        })
    }

    /// Borrows locked bytes. Callers must not copy credentials into unlocked storage.
    pub fn bytes(&self) -> &[u8; N] {
        // SAFETY: the owned live mapping contains at least N initialized bytes.
        unsafe { &*self.pointer.as_ptr().cast::<[u8; N]>() }
    }
    /// Borrows locked bytes for bounded native I/O.
    pub fn bytes_mut(&mut self) -> &mut [u8; N] {
        // SAFETY: exclusive ownership prevents aliases while mutably borrowed.
        unsafe { &mut *self.pointer.as_ptr().cast::<[u8; N]>() }
    }
    /// Wipes the complete mapping, including unused frame space, while still locked.
    pub fn wipe(&mut self) {
        // SAFETY: the entire mapping is exclusively owned and writable.
        unsafe { std::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.mapping_length) }
            .zeroize();
    }
}
impl<const N: usize> Drop for LockedMemory<N> {
    fn drop(&mut self) {
        self.wipe();
        // SAFETY: wiping precedes unlocking and unmapping the exact owned mapping.
        unsafe {
            libc::munlock(self.pointer.as_ptr().cast(), self.mapping_length);
            libc::munmap(self.pointer.as_ptr().cast(), self.mapping_length);
        }
    }
}

/// Four server-side endpoints, all CLOEXEC before any exchange or child operation.
pub struct ReceiverPipes {
    ready: PipeEndpoint,
    delivery: PipeEndpoint,
    confirmation: PipeEndpoint,
    acknowledgement: PipeEndpoint,
}
impl ReceiverPipes {
    /// Takes only the four intended inherited endpoints and validates their directions.
    pub fn take(
        ready: OwnedFd,
        delivery: OwnedFd,
        confirmation: OwnedFd,
        acknowledgement: OwnedFd,
    ) -> Result<Self, NativeFailure> {
        Ok(Self {
            ready: PipeEndpoint::take(ready, true)?,
            delivery: PipeEndpoint::take(delivery, false)?,
            confirmation: PipeEndpoint::take(confirmation, true)?,
            acknowledgement: PipeEndpoint::take(acknowledgement, false)?,
        })
    }
}

/// Receiver completion facts and a still-locked secret. Not authenticated readiness.
pub struct ReceiverReceipt {
    secret: LockedMemory<32>,
}
impl ReceiverReceipt {
    /// Borrows the received secret without creating a copy in unlocked memory.
    pub fn secret(&self) -> &[u8; 32] {
        self.secret.bytes()
    }
}

/// Completes the receiver protocol and reaps the exact retriever.
///
/// This must be called as the earliest post-exec operation, before application
/// initialization, listeners or Terminal Sessions. Native composition must still
/// revalidate startup authority after return. Failures close pipes and wipe all
/// mappings before bounded cleanup; unresolved cleanup overrides any other result.
/// Successful retirement and receipt issuance share the original exchange deadline.
/// A separate 250 ms failure-cleanup allowance cannot produce a receipt.
pub fn receive(
    transcript: &StartupTranscript,
    pipes: ReceiverPipes,
    child: ExactChild,
    deadline: ExchangeDeadline,
) -> Result<ReceiverReceipt, NativeFailure> {
    receive_using(
        transcript,
        pipes,
        child,
        deadline,
        StartupTranscript::confirmation_macs,
    )
}

fn receive_using(
    transcript: &StartupTranscript,
    mut pipes: ReceiverPipes,
    mut child: ExactChild,
    deadline: ExchangeDeadline,
    calculate: impl FnOnce(
        &StartupTranscript,
        &[u8; 32],
        &[u8; 32],
    ) -> Result<super::ConfirmationMacs, NativeFailure>,
) -> Result<ReceiverReceipt, NativeFailure> {
    let result = (|| {
        if child.identity != transcript.retriever_identity() {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        disable_core_dumps()?;
        let mut frame = LockedMemory::<417>::allocate()?;
        let mut secret = LockedMemory::<32>::allocate()?;
        pipes
            .ready
            .write_frame(&transcript.prefix(FrameKind::Ready), deadline)?;
        pipes
            .delivery
            .read_frame(&mut frame, FRAME_BYTES, deadline)?;
        transcript.recognize_prefix(FrameKind::Delivery, &frame.bytes()[..352])?;
        secret.bytes_mut().copy_from_slice(&frame.bytes()[384..416]);
        let mut challenge = LockedMemory::<32>::allocate()?;
        challenge
            .bytes_mut()
            .copy_from_slice(&frame.bytes()[352..384]);
        let macs = calculate(transcript, secret.bytes(), challenge.bytes())?;
        frame.wipe();
        frame.bytes_mut()[..352].copy_from_slice(&transcript.prefix(FrameKind::Confirmation));
        frame.bytes_mut()[352..384].copy_from_slice(challenge.bytes());
        frame.bytes_mut()[384..416].copy_from_slice(macs.confirmation());
        pipes
            .confirmation
            .write_frame(&frame.bytes()[..416], deadline)?;
        frame.wipe();
        pipes
            .acknowledgement
            .read_frame(&mut frame, FRAME_BYTES, deadline)?;
        transcript.recognize_prefix(FrameKind::Acknowledgement, &frame.bytes()[..352])?;
        if frame.bytes()[352..384] != challenge.bytes()[..] {
            return Err(NativeFailure::CredentialUnavailable);
        }
        macs.verify_acknowledgement(&frame.bytes()[384..416])?;
        deadline.remaining()?;
        Ok(ReceiverReceipt { secret })
    })();
    drop(pipes);
    // Success requires retirement within the original exchange deadline.
    // Failed exchanges have already wiped their mappings before cleanup.
    let cleanup_deadline = if result.is_ok() {
        deadline
    } else {
        ExchangeDeadline::start(Duration::from_millis(250))?
    };
    match child.reap(cleanup_deadline) {
        Err(_) if result.is_ok() && child.child.is_some() => {
            // Only timeout retains exact wait ownership. Wipe the receipt before
            // a separate failure-cleanup allowance, which can never admit startup.
            drop(result);
            match child.reap(ExchangeDeadline::start(Duration::from_millis(250))?) {
                Ok(_) => Err(NativeFailure::TimedOut),
                Err(_) => Err(NativeFailure::CleanupUnresolved),
            }
        }
        Err(_) => Err(NativeFailure::CleanupUnresolved),
        Ok(_) if matches!(result, Err(NativeFailure::CleanupUnresolved)) => result,
        Ok(false) => Err(NativeFailure::CredentialUnavailable),
        Ok(true) => {
            if result.is_ok() {
                deadline.remaining()?;
            }
            result
        }
    }
}

/// Exclusive wait ownership of a directly owned child, not copyable PID facts.
/// The service composition must not install an autoreaping SIGCHLD policy or
/// call another waiter after transferring this ownership.
pub struct ExactChild {
    identity: ProcessIdentity,
    child: Option<std::process::Child>,
}
impl ExactChild {
    /// Transfers exclusive ownership on success. Failures leave the caller's
    /// child intact for cleanup; the same child cannot produce a second token.
    pub fn take(
        owned: &mut Option<std::process::Child>,
        identity: ProcessIdentity,
    ) -> Result<Self, NativeFailure> {
        let child = owned.as_mut().ok_or(NativeFailure::UnsafeNativeState)?;
        match child.try_wait() {
            Ok(None) => {}
            _ => return Err(NativeFailure::CleanupUnresolved),
        }
        if child.id() != identity.pid {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        let (actual, parent) = observe_process(identity.pid)?;
        if actual != identity || parent != unsafe { libc::getpid() } as u32 {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0
            || action.sa_sigaction != libc::SIG_DFL
            || action.sa_flags & libc::SA_NOCLDWAIT != 0
        {
            return Err(NativeFailure::CleanupUnresolved);
        }
        Ok(Self {
            identity,
            child: owned.take(),
        })
    }
    /// Waits without signaling a privileged child. A completed wait or lost
    /// reservation permanently invalidates ownership. ECHILD is unresolved,
    /// never retirement. The native retriever must own a lifetime guard.
    pub fn reap(&mut self, deadline: ExchangeDeadline) -> Result<bool, NativeFailure> {
        self.reap_with(deadline, std::process::Child::try_wait)
    }

    fn reap_with(
        &mut self,
        deadline: ExchangeDeadline,
        mut wait: impl FnMut(&mut std::process::Child) -> io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<bool, NativeFailure> {
        loop {
            let child = self
                .child
                .as_mut()
                .ok_or(NativeFailure::UnsafeNativeState)?;
            match wait(child) {
                Ok(Some(status)) => {
                    self.child.take();
                    return Ok(status.success());
                }
                Err(error) if error.raw_os_error() == Some(libc::EINTR) => {}
                Err(_) => {
                    // The original PID reservation may now be gone. Never wait
                    // on it again, even if another direct child reuses that PID.
                    self.child.take();
                    return Err(NativeFailure::CleanupUnresolved);
                }
                Ok(None) => {}
            }
            let remaining = deadline
                .remaining()
                .map_err(|_| NativeFailure::CleanupUnresolved)?;
            std::thread::sleep(remaining.min(Duration::from_millis(2)));
        }
    }
}

/// Observes process start identity through the native kernel interface.
/// No process name, pathname or raw native message escapes this operation.
pub fn process_identity(pid: u32) -> Result<ProcessIdentity, NativeFailure> {
    observe_process(pid).map(|value| value.0)
}

#[cfg(target_os = "macos")]
fn observe_process(pid: u32) -> Result<(ProcessIdentity, u32), NativeFailure> {
    #[repr(C)]
    struct BsdInfo {
        flags: u32,
        status: u32,
        xstatus: u32,
        pid: u32,
        ppid: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        reserved: u32,
        comm: [u8; 16],
        name: [u8; 32],
        nfiles: u32,
        pgid: u32,
        pjobc: u32,
        device: u32,
        tpgid: u32,
        nice: i32,
        seconds: u64,
        microseconds: u64,
    }
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: i32,
        ) -> i32;
    }
    if !(1..=i32::MAX as u32).contains(&pid) {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    let mut info: BsdInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<BsdInfo>();
    // SAFETY: this mirrors the public proc_bsdinfo ABI and provides its exact buffer size.
    let result = unsafe {
        proc_pidinfo(
            pid as i32,
            3,
            0,
            (&mut info as *mut BsdInfo).cast(),
            size as i32,
        )
    };
    if result as usize != size || info.pid != pid || info.microseconds >= 1_000_000 {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    Ok((
        ProcessIdentity {
            pid,
            start_seconds: info.seconds,
            start_microseconds: info.microseconds as u32,
        },
        info.ppid,
    ))
}

#[cfg(target_os = "linux")]
fn observe_process(pid: u32) -> Result<(ProcessIdentity, u32), NativeFailure> {
    use std::io::Read;
    if !(1..=i32::MAX as u32).contains(&pid) {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    let file = std::fs::File::open(format!("/proc/{pid}/stat"))
        .map_err(|_| NativeFailure::ProcessIdentityMismatch)?;
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| NativeFailure::ProcessIdentityMismatch)?;
    if bytes.len() > 4096 {
        return Err(NativeFailure::LimitExceeded);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| NativeFailure::ProcessIdentityMismatch)?;
    let end = text
        .rfind(')')
        .ok_or(NativeFailure::ProcessIdentityMismatch)?;
    let fields: Vec<_> = text[end + 1..].split_ascii_whitespace().collect();
    let parent = fields
        .get(1)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or(NativeFailure::ProcessIdentityMismatch)?;
    let ticks = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(NativeFailure::ProcessIdentityMismatch)?;
    let rate = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if rate <= 0 {
        return Err(NativeFailure::NativeUnavailable);
    }
    let rate = rate as u64;
    Ok((
        ProcessIdentity {
            pid,
            start_seconds: ticks / rate,
            start_microseconds: ((ticks % rate) * 1_000_000 / rate) as u32,
        },
        parent,
    ))
}

fn disable_core_dumps() -> Result<(), NativeFailure> {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
        return Err(NativeFailure::NativeUnavailable);
    }
    #[cfg(target_os = "linux")]
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(NativeFailure::NativeUnavailable);
    }
    Ok(())
}
fn wait(fd: RawFd, events: i16, deadline: ExchangeDeadline) -> Result<(), NativeFailure> {
    loop {
        let remaining = deadline.remaining()?;
        let milliseconds = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, milliseconds) };
        if result > 0 {
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(NativeFailure::IoUnavailable);
            }
            deadline.remaining()?;
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Err(NativeFailure::IoUnavailable);
        }
    }
}

struct BlockedSigpipe {
    old: libc::sigset_t,
    was_pending: bool,
}
impl BlockedSigpipe {
    fn enter() -> Result<Self, NativeFailure> {
        let mut set = unsafe { std::mem::zeroed() };
        let mut old = unsafe { std::mem::zeroed() };
        let mut pending = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGPIPE);
        }
        if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old) } != 0 {
            return Err(NativeFailure::NativeUnavailable);
        }
        if unsafe { libc::sigpending(&mut pending) } != 0 {
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            }
            return Err(NativeFailure::NativeUnavailable);
        }
        Ok(Self {
            old,
            was_pending: unsafe { libc::sigismember(&pending, libc::SIGPIPE) } == 1,
        })
    }
}
impl Drop for BlockedSigpipe {
    fn drop(&mut self) {
        let mut pending = unsafe { std::mem::zeroed() };
        if !self.was_pending
            && unsafe { libc::sigpending(&mut pending) } == 0
            && unsafe { libc::sigismember(&pending, libc::SIGPIPE) } == 1
        {
            let mut set = unsafe { std::mem::zeroed() };
            unsafe {
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGPIPE);
            }
            let mut signal = 0;
            unsafe {
                libc::sigwait(&set, &mut signal);
            }
        }
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.old, std::ptr::null_mut());
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn native_memory_is_locked(bytes: &[u8]) -> bool {
    unsafe extern "C" {
        fn mach_vm_region(
            task: u32,
            address: *mut u64,
            size: *mut u64,
            flavor: i32,
            info: *mut i32,
            count: *mut u32,
            object: *mut u32,
        ) -> i32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
    }
    #[allow(deprecated)]
    let task = unsafe { libc::mach_task_self() };
    let requested = bytes.as_ptr() as u64;
    let mut address = requested;
    let mut size = 0;
    let mut info = [0i32; 9];
    let mut count = 9;
    let mut object = 0;
    let result = unsafe {
        mach_vm_region(
            task,
            &mut address,
            &mut size,
            9,
            info.as_mut_ptr(),
            &mut count,
            &mut object,
        )
    };
    if object != 0 {
        unsafe {
            mach_port_deallocate(task, object);
        }
    }
    result == 0
        && count == 9
        && address <= requested
        && requested + bytes.len() as u64 <= address + size
        && info[8] & 0xffff != 0
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn native_memory_is_locked(bytes: &[u8]) -> bool {
    let address = bytes.as_ptr() as usize;
    let mut selected = false;
    for line in std::fs::read_to_string("/proc/self/smaps").unwrap().lines() {
        if let Some(range) = line
            .split_ascii_whitespace()
            .next()
            .filter(|word| word.contains('-'))
        {
            let (start, end) = range.split_once('-').unwrap();
            selected = usize::from_str_radix(start, 16).unwrap() <= address
                && address + bytes.len() <= usize::from_str_radix(end, 16).unwrap();
        } else if selected && line.starts_with("VmFlags:") {
            return line.split_ascii_whitespace().any(|flag| flag == "lo");
        }
    }
    false
}

#[cfg(test)]
mod child_tests {
    use super::*;

    #[test]
    fn cryptographic_join_failure_survives_unsuccessful_exact_retriever_exit() {
        use super::super::{FrameKind, PREFIX_BYTES};
        use std::io::Write;
        use std::process::Stdio;
        let mut owned = Some(
            std::process::Command::new("/bin/sh")
                .args(["-c", "read line; exit 7"])
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut exit = owned.as_mut().unwrap().stdin.take().unwrap();
        let retriever = process_identity(owned.as_ref().unwrap().id()).unwrap();
        let exact = ExactChild::take(&mut owned, retriever).unwrap();
        let original = process_identity(std::process::id()).unwrap();
        let mut ready = [0; PREFIX_BYTES];
        ready[..4].copy_from_slice(b"STN2");
        ready[4] = 2;
        ready[5] = 1;
        ready[8..12].copy_from_slice(&352u32.to_be_bytes());
        for (offset, value) in [(16, 501u32), (20, 20), (24, 3), (28, 63)] {
            ready[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        for offset in [32, 40, 48, 56] {
            ready[offset..offset + 8].copy_from_slice(&1u64.to_be_bytes());
        }
        ready[64..272].fill(1);
        for (offset, process) in [(272, original), (296, retriever)] {
            ready[offset..offset + 8].copy_from_slice(&u64::from(process.pid).to_be_bytes());
            ready[offset + 8..offset + 16].copy_from_slice(&process.start_seconds.to_be_bytes());
            ready[offset + 16..offset + 20]
                .copy_from_slice(&process.start_microseconds.to_be_bytes());
        }
        let transcript = StartupTranscript::recognize_ready(&ready).unwrap();
        let (rr, rw) = anonymous_pipe().unwrap();
        let (dr, dw) = anonymous_pipe().unwrap();
        let (cr, cw) = anonymous_pipe().unwrap();
        let (ar, aw) = anonymous_pipe().unwrap();
        let deadline = ExchangeDeadline::start(Duration::from_millis(500)).unwrap();
        let peer = std::thread::spawn(move || {
            let expected = StartupTranscript::recognize_ready(&ready).unwrap();
            let mut input = PipeEndpoint::take(rr, false).unwrap();
            let mut output = PipeEndpoint::take(dw, true).unwrap();
            let mut confirmation = PipeEndpoint::take(cr, false).unwrap();
            let mut frame = LockedMemory::<417>::allocate().unwrap();
            input.read_frame(&mut frame, 352, deadline).unwrap();
            frame.wipe();
            frame.bytes_mut()[..352].copy_from_slice(&expected.prefix(FrameKind::Delivery));
            getrandom::fill(&mut frame.bytes_mut()[352..416]).unwrap();
            output.write_frame(&frame.bytes()[..416], deadline).unwrap();
            assert_eq!(
                confirmation.read_frame(&mut frame, 416, deadline),
                Err(NativeFailure::CredentialUnavailable)
            );
            // Nonzero retirement follows the failed calculation's pipe closure,
            // not a scheduler-dependent delay before exact-child ownership.
            exit.write_all(b"exit\n").unwrap();
            drop(aw);
        });
        let outcome = receive_using(
            &transcript,
            ReceiverPipes::take(rw, dr, cw, ar).unwrap(),
            exact,
            deadline,
            |transcript, secret, challenge| {
                super::super::crypto::failed_join(
                    &transcript.prefix(FrameKind::Ready),
                    secret,
                    challenge,
                )
            },
        );
        peer.join().unwrap();
        assert!(matches!(outcome, Err(NativeFailure::CleanupUnresolved)));
    }

    #[test]
    fn external_wait_consumption_cannot_reap_a_reused_direct_child_pid() {
        let mut owned = Some(
            std::process::Command::new("/bin/sleep")
                .arg("0.1")
                .spawn()
                .unwrap(),
        );
        let identity = process_identity(owned.as_ref().unwrap().id()).unwrap();
        let mut exact = ExactChild::take(&mut owned, identity).unwrap();
        let mut calls = 0;
        let deadline = ExchangeDeadline::start(Duration::from_millis(100)).unwrap();
        let mut wait = |child: &mut std::process::Child| {
            calls += 1;
            // The native boundary reports the original reservation was consumed.
            // A second call would model reuse by another direct child.
            if calls == 1 {
                child.wait().unwrap();
                Err(io::Error::from_raw_os_error(libc::ECHILD))
            } else {
                Ok(Some(std::os::unix::process::ExitStatusExt::from_raw(0)))
            }
        };
        assert_eq!(
            exact.reap_with(deadline, &mut wait),
            Err(NativeFailure::CleanupUnresolved)
        );
        assert_eq!(
            exact.reap_with(deadline, &mut wait),
            Err(NativeFailure::UnsafeNativeState)
        );
        assert_eq!(calls, 1);
    }
}
