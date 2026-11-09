//! Turns a termination signal into a quit request, so `brew uninstall` and the system end
//! SpaceTerm through its ordinary quit, which saves Settings and the client views, instead of
//! killing the process.
#![cfg(unix)]

use std::io;
use std::sync::atomic::{AtomicI32, Ordering};

/// The pipe each watched signal wakes, by signal number. -1 while it is not watched.
static WAKERS: [AtomicI32; 65] = [const { AtomicI32::new(-1) }; 65];

/// Watches `signal` for the rest of the process. Each delivery sends one message on the
/// returned channel. A signal that arrives again before the last message is read may send one.
pub(crate) fn watch(signal: libc::c_int) -> io::Result<async_channel::Receiver<()>> {
    let slot = usize::try_from(signal)
        .ok()
        .and_then(|index| WAKERS.get(index))
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut fds = [0; 2];
    // SAFETY: fds holds two descriptors for pipe to fill.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let [reader, writer] = fds;
    for (fd, flags) in [(reader, 0), (writer, libc::O_NONBLOCK)] {
        // SAFETY: fd is an open descriptor this function owns.
        let configured = unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == 0
                && libc::fcntl(fd, libc::F_SETFL, flags) == 0
        };
        if !configured {
            let error = io::Error::last_os_error();
            // SAFETY: both descriptors are open and owned here.
            unsafe {
                libc::close(reader);
                libc::close(writer);
            }
            return Err(error);
        }
    }
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("spaceterm-quit-signal".to_owned())
        .spawn(move || {
            let mut byte = 0u8;
            loop {
                // SAFETY: reader stays open for this thread's life and byte is one writable byte.
                let read = unsafe { libc::read(reader, (&raw mut byte).cast(), 1) };
                if read == 1 {
                    if sender.send_blocking(()).is_err() {
                        return;
                    }
                } else if read == 0
                    || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                {
                    return;
                }
            }
        })?;
    slot.store(writer, Ordering::SeqCst);
    // SAFETY: an all-zero sigaction is a valid action with an empty mask, completed below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = wake as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: action names a handler that only calls async-signal-safe functions.
    if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
        slot.store(-1, Ordering::SeqCst);
        return Err(io::Error::last_os_error());
    }
    Ok(receiver)
}

/// Writes one byte to the signal's pipe. It calls only async-signal-safe functions and keeps
/// errno for the code it interrupted.
extern "C" fn wake(signal: libc::c_int) {
    let Some(slot) = usize::try_from(signal)
        .ok()
        .and_then(|index| WAKERS.get(index))
    else {
        return;
    };
    let writer = slot.load(Ordering::SeqCst);
    if writer < 0 {
        return;
    }
    // SAFETY: errno is thread-local and readable from a signal handler.
    let saved = unsafe { *errno() };
    let byte = 1u8;
    // SAFETY: write is async-signal-safe; writer is a nonblocking pipe this module keeps open.
    let _ = unsafe { libc::write(writer, (&raw const byte).cast(), 1) };
    // SAFETY: as above.
    unsafe { *errno() = saved };
}

#[cfg(target_os = "macos")]
fn errno() -> *mut libc::c_int {
    // SAFETY: __error returns this thread's errno location.
    unsafe { libc::__error() }
}

#[cfg(not(target_os = "macos"))]
fn errno() -> *mut libc::c_int {
    // SAFETY: __errno_location returns this thread's errno location.
    unsafe { libc::__errno_location() }
}

#[cfg(test)]
mod tests {
    /// A watched signal reaches the channel instead of the default action. The test watches
    /// SIGUSR2, which nothing else in the test process uses, rather than the SIGTERM the app
    /// watches, so the test runner keeps its own termination.
    #[test]
    fn a_watched_signal_arrives_as_one_message() {
        let signals = super::watch(libc::SIGUSR2).expect("the signal is watched");
        // SAFETY: raise delivers SIGUSR2 to this process, whose handler is installed above.
        assert_eq!(unsafe { libc::raise(libc::SIGUSR2) }, 0);
        assert_eq!(signals.recv_blocking(), Ok(()));
        assert!(signals.try_recv().is_err());
    }
}
