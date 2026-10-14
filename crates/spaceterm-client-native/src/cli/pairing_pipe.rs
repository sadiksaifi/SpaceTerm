//! Mechanical bounds for the explicit claim/authorization pipe mode. No new
//! framing or encoding is added to the existing JSON claim or binary delivery.

use std::io::{Read, Write};

use zeroize::Zeroizing;

use super::CliFailure;

/// Ceiling for the existing public claim's JSON serialization, including any
/// trailing whitespace. The authorization producer validates the claim itself.
pub const MAX_PUBLIC_CLAIM_BYTES: usize = 8192;
pub const MAX_AUTHORIZATION_DELIVERY_BYTES: usize =
    spaceterm_core::pairing::MAX_PAIRING_MANAGEMENT_BYTES;

/// Reads one EOF-terminated public claim document. The caller must verify pipe
/// authority before invoking this bounded mechanical reader.
pub fn read_public_claim(reader: &mut impl Read) -> Result<Zeroizing<Vec<u8>>, CliFailure> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_PUBLIC_CLAIM_BYTES + 1));
    reader
        .take((MAX_PUBLIC_CLAIM_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CliFailure::IoUnavailable)?;
    if bytes.len() > MAX_PUBLIC_CLAIM_BYTES {
        return Err(CliFailure::LimitExceeded);
    }
    if bytes.is_empty() {
        return Err(CliFailure::Usage);
    }
    Ok(bytes)
}

/// Writes the existing opaque frame exactly once, without a newline, JSON
/// envelope or diagnostic copy. The caller verifies private output authority.
pub fn write_authorization_delivery(
    writer: &mut impl Write,
    bytes: &[u8],
) -> Result<(), CliFailure> {
    if bytes.len() > MAX_AUTHORIZATION_DELIVERY_BYTES {
        return Err(CliFailure::LimitExceeded);
    }
    if bytes.is_empty() {
        return Err(CliFailure::UnsafeNativeState);
    }
    writer
        .write_all(bytes)
        .and_then(|()| writer.flush())
        .map_err(|_| CliFailure::IoUnavailable)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipeDirection {
    Read,
    Write,
}

/// Accepts only an anonymous pipe endpoint owned by the invoking account in
/// the requested direction. "Not a terminal" also admits files, named FIFOs and
/// sockets, so it cannot protect a transient enrollment bearer.
#[cfg(unix)]
pub fn verify_private_pipe(
    fd: std::os::fd::BorrowedFd<'_>,
    direction: PipeDirection,
) -> Result<(), CliFailure> {
    use std::os::fd::AsRawFd;

    let raw = fd.as_raw_fd();
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: BorrowedFd remains live and fstat initializes stat on success.
    if unsafe { libc::fstat(raw, stat.as_mut_ptr()) } != 0 {
        return Err(CliFailure::IoUnavailable);
    }
    // SAFETY: fstat returned success.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: geteuid takes no arguments and has no memory preconditions.
    if stat.st_mode & libc::S_IFMT != libc::S_IFIFO || stat.st_uid != unsafe { libc::geteuid() } {
        return Err(CliFailure::AuthorizationDenied);
    }
    // SAFETY: F_GETFL only inspects the borrowed live descriptor.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(CliFailure::IoUnavailable);
    }
    let expected = match direction {
        PipeDirection::Read => libc::O_RDONLY,
        PipeDirection::Write => libc::O_WRONLY,
    };
    if flags & libc::O_ACCMODE != expected {
        return Err(CliFailure::AuthorizationDenied);
    }
    verify_anonymous(raw, &stat)
}

/// Duplicate before checking so this operation owns the exact verified endpoint.
/// File's unbuffered I/O avoids retaining bearer copies in stdio line buffers.
#[cfg(unix)]
pub(crate) fn duplicate_private_pipe(
    fd: std::os::fd::BorrowedFd<'_>,
    direction: PipeDirection,
) -> Result<std::fs::File, CliFailure> {
    use std::os::fd::AsFd;
    let file = duplicate_descriptor(fd)?;
    verify_private_pipe(file.as_fd(), direction)?;
    Ok(file)
}

#[cfg(unix)]
fn duplicate_descriptor(fd: std::os::fd::BorrowedFd<'_>) -> Result<std::fs::File, CliFailure> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: fcntl duplicates the borrowed live descriptor with CLOEXEC.
    let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if raw < 0 {
        return Err(CliFailure::IoUnavailable);
    }
    // SAFETY: successful fcntl returned a new, exclusively owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}

/// Status flags belong to open-file descriptions, which stdout and stderr can
/// share. Restore them after output stops, before abrupt exit or context Drop.
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct InvocationPipeFlags {
    state: std::sync::Mutex<PipeFlagState>,
}
#[cfg(unix)]
#[derive(Default)]
struct PipeFlagState {
    leases: Vec<NonblockingFile>,
    restored: bool,
}
#[cfg(unix)]
impl PipeFlagState {
    fn restore(&mut self) {
        self.restored = true;
        // Later aliases observed O_NONBLOCK. Restore those first, then restore
        // the first descriptor's original flags, after scoped writes stop.
        while let Some(lease) = self.leases.pop() {
            drop(lease);
        }
    }
}
#[cfg(unix)]
impl InvocationPipeFlags {
    pub(crate) fn keep(&self, fd: std::os::fd::BorrowedFd<'_>) -> Result<(), CliFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CliFailure::UnsafeNativeState)?;
        if state.restored {
            return Err(CliFailure::OutcomePending);
        }
        if state.leases.len() >= 8 {
            return Err(CliFailure::LimitExceeded);
        }
        // Serialize snapshots so reverse restoration also handles aliases.
        state
            .leases
            .push(NonblockingFile::new(duplicate_descriptor(fd)?)?);
        Ok(())
    }

    pub(crate) fn restore(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore();
    }
}
#[cfg(unix)]
impl Drop for InvocationPipeFlags {
    fn drop(&mut self) {
        self.state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore();
    }
}

/// Raw output has a total deadline even when its reader stays open and idle.
pub const AUTHORIZATION_WRITE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
/// Interruption diagnostics must not turn stderr backpressure into an exit hang.
pub const AUTHORIZATION_DIAGNOSTIC_DEADLINE: std::time::Duration =
    std::time::Duration::from_millis(250);
const MAX_AUTHORIZATION_DIAGNOSTIC_BYTES: usize = 1024;

/// Bounded public-metadata output for sinks where O_NONBLOCK cannot bound a
/// syscall, including regular files. Never pass a claim or delivery here.
/// Timeout ends the caller's wait, not a kernel syscall already in progress.
/// The one-shot executable exits without joining a stalled metadata worker.
pub fn write_public_diagnostic_bounded(
    mut writer: impl Write + Send + 'static,
    bytes: &[u8],
) -> Result<(), CliFailure> {
    if bytes.len() > MAX_AUTHORIZATION_DIAGNOSTIC_BYTES {
        return Err(CliFailure::LimitExceeded);
    }
    // The worker owns only public metadata and its CLOEXEC output duplicate.
    // A stalled regular-file write must not block the signal or dispatch thread.
    let deadline = std::time::Instant::now() + AUTHORIZATION_DIAGNOSTIC_DEADLINE;
    let bytes = bytes.to_vec();
    let (completed, result) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("cli-public-diagnostic".into())
        .spawn(move || {
            let outcome = writer
                .write_all(&bytes)
                .map_err(|_| CliFailure::IoUnavailable);
            let _ = completed.send(outcome);
        })
        .map_err(|_| CliFailure::IoUnavailable)?;
    result
        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        .map_err(|_| CliFailure::IoUnavailable)?
}

#[cfg(unix)]
struct NonblockingFile {
    file: std::fs::File,
    original_flags: libc::c_int,
}
#[cfg(unix)]
impl NonblockingFile {
    fn new(file: std::fs::File) -> Result<Self, CliFailure> {
        use std::os::fd::AsRawFd;
        // SAFETY: fcntl inspects and updates a live, exclusively owned duplicate.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(CliFailure::IoUnavailable);
        }
        Ok(Self {
            file,
            original_flags: flags,
        })
    }
}
#[cfg(unix)]
impl std::os::fd::AsRawFd for NonblockingFile {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.file)
    }
}
#[cfg(unix)]
impl Drop for NonblockingFile {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // Duplicates share status flags with inherited stdio. Restore them before
        // releasing our duplicate, including cancellation and timeout paths.
        // SAFETY: self.file still owns this live descriptor during Drop.
        let _ = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_SETFL, self.original_flags) };
    }
}

/// Verify and own the exact anonymous output endpoint, then transfer the existing
/// opaque bytes with Tokio readiness. No lock spans a readiness wait or flush.
#[cfg(unix)]
pub async fn write_private_authorization_delivery(
    fd: std::os::fd::BorrowedFd<'_>,
    bytes: &[u8],
    signal: &super::runner::CliSignalContext,
) -> Result<(), CliFailure> {
    if bytes.len() > MAX_AUTHORIZATION_DELIVERY_BYTES {
        return Err(CliFailure::LimitExceeded);
    }
    if bytes.is_empty() {
        return Err(CliFailure::UnsafeNativeState);
    }
    use std::os::fd::AsFd;
    let file = duplicate_private_pipe(fd, PipeDirection::Write)?;
    signal.keep_nonblocking(file.as_fd())?;
    let output = tokio::io::unix::AsyncFd::new(file).map_err(|_| CliFailure::IoUnavailable)?;
    let writing = async {
        let mut offset = 0;
        while offset < bytes.len() {
            let mut ready = tokio::select! {
                biased;
                () = signal.interrupted() => return Err(CliFailure::OutcomePending),
                ready = output.writable() => ready.map_err(|_| CliFailure::IoUnavailable)?,
            };
            // This gate covers just one O_NONBLOCK write. AsyncFd clears stale
            // readiness on WouldBlock; neither poll nor write_all runs here.
            match signal.try_delivery_write(bytes.len() - offset, || {
                match ready.try_io(|inner| {
                    let mut file = inner.get_ref();
                    file.write(&bytes[offset..])
                }) {
                    Ok(result) => result,
                    Err(_) => Err(std::io::ErrorKind::WouldBlock.into()),
                }
            })? {
                Ok(count) => offset += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(CliFailure::IoUnavailable),
            }
        }
        Ok(())
    };
    tokio::time::timeout(AUTHORIZATION_WRITE_DEADLINE, writing)
        .await
        .map_err(|_| CliFailure::IoUnavailable)?
}

/// Compatibility hook for immediate callers. Nonblocking File writes can fail
/// on backpressure, but cannot hang. Dispatch uses the async adapter above.
#[cfg(unix)]
pub(crate) fn write_private_authorization_immediate(
    fd: std::os::fd::BorrowedFd<'_>,
    bytes: &[u8],
) -> Result<(), CliFailure> {
    let mut output = NonblockingFile::new(duplicate_private_pipe(fd, PipeDirection::Write)?)?;
    write_authorization_delivery(&mut output.file, bytes)
}

/// A conventional stderr envelope, bounded independently of stdout. No bearer,
/// credential reference, path or raw I/O error reaches this channel.
#[cfg(unix)]
pub(crate) fn write_authorization_diagnostic(
    fd: std::os::fd::BorrowedFd<'_>,
    bytes: &[u8],
) -> Result<(), CliFailure> {
    let output = duplicate_descriptor(fd)?;
    if diagnostic_supports_nonblocking(&output)? {
        let output = NonblockingFile::new(output)?;
        write_diagnostic_file(&output.file, bytes)
    } else {
        write_public_diagnostic_bounded(output, bytes)
    }
}

#[cfg(unix)]
pub(crate) fn write_authorization_diagnostic_scoped(
    fd: std::os::fd::BorrowedFd<'_>,
    bytes: &[u8],
    signal: &super::runner::CliSignalContext,
) -> Result<(), CliFailure> {
    use std::os::fd::AsFd;
    let output = duplicate_descriptor(fd)?;
    if diagnostic_supports_nonblocking(&output)? {
        signal.keep_nonblocking(output.as_fd())?;
        write_diagnostic_file(&output, bytes)
    } else {
        write_public_diagnostic_bounded(output, bytes)
    }
}

#[cfg(unix)]
fn diagnostic_supports_nonblocking(file: &std::fs::File) -> Result<bool, CliFailure> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat receives a live descriptor and correctly sized writable stat.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(CliFailure::IoUnavailable);
    }
    // SAFETY: fstat returned success.
    let mode = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
    Ok(mode == libc::S_IFIFO || mode == libc::S_IFSOCK)
}

#[cfg(unix)]
fn write_diagnostic_file(output: &std::fs::File, bytes: &[u8]) -> Result<(), CliFailure> {
    use std::os::fd::AsRawFd;
    if bytes.len() > MAX_AUTHORIZATION_DIAGNOSTIC_BYTES {
        return Err(CliFailure::LimitExceeded);
    }
    let deadline = std::time::Instant::now() + AUTHORIZATION_DIAGNOSTIC_DEADLINE;
    let mut offset = 0;
    while offset < bytes.len() {
        if std::time::Instant::now() >= deadline {
            return Err(CliFailure::IoUnavailable);
        }
        let mut file = output;
        match file.write(&bytes[offset..]) {
            Ok(0) => return Err(CliFailure::IoUnavailable),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let timeout = remaining.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
                if timeout == 0 {
                    return Err(CliFailure::IoUnavailable);
                }
                let mut poll = libc::pollfd {
                    fd: output.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: poll receives one initialized entry for a live fd.
                let result = unsafe { libc::poll(&mut poll, 1, timeout) };
                if result < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return Err(CliFailure::IoUnavailable);
                }
            }
            Err(_) => return Err(CliFailure::IoUnavailable),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_anonymous(raw: libc::c_int, stat: &libc::stat) -> Result<(), CliFailure> {
    // Darwin anonymous pipes have no filesystem device or links. An unlinked
    // named FIFO still has a filesystem vnode and supports fstatfs.
    if stat.st_nlink != 0 || stat.st_dev != 0 {
        return Err(CliFailure::AuthorizationDenied);
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs receives a correctly sized writable buffer and a live fd.
    if unsafe { libc::fstatfs(raw, filesystem.as_mut_ptr()) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
    {
        return Err(CliFailure::AuthorizationDenied);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_anonymous(raw: libc::c_int, _stat: &libc::stat) -> Result<(), CliFailure> {
    // A filesystem FIFO and an anonymous pipe both have S_IFIFO and can have one
    // link on Linux. The kernel's descriptor link distinguishes them.
    let target = std::fs::read_link(format!("/proc/self/fd/{raw}"))
        .map_err(|_| CliFailure::AuthorizationUnavailable)?;
    use std::os::unix::ffi::OsStrExt;
    let target = target.as_os_str().as_bytes();
    if !target.starts_with(b"pipe:[") || !target.ends_with(b"]") {
        return Err(CliFailure::AuthorizationDenied);
    }
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn verify_anonymous(_raw: libc::c_int, _stat: &libc::stat) -> Result<(), CliFailure> {
    Err(CliFailure::UnsupportedHost)
}
