use std::io;
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use super::{UnixSshProcess, UnixSshProcessAdapter};
use crate::ssh::process::{
    SshProcessAdapter, SshProcessExitInterrupt, SshProcessExitObservation, SshProcessExitWait,
    SshProcessExitWake, SshProcessMechanismError,
};

pub(super) fn observe(
    adapter: &UnixSshProcessAdapter,
    process: &mut UnixSshProcess,
) -> Option<SshProcessExitObservation> {
    if process.collected_exit.is_some() {
        return Some(immediate_exit());
    }
    let observation = native_observation(process.child.id()).ok();
    // A kqueue NOTE_EXIT registration only observes later edges. Checking status after registration
    // also covers a child that exited before attachment, including an ESRCH registration failure.
    // The exclusive process borrow retains its identity until this check completes.
    match adapter.try_status(process) {
        Ok(Some(_)) => Some(immediate_exit()),
        Ok(None) => observation,
        Err(_) => None,
    }
}

fn immediate_exit() -> SshProcessExitObservation {
    SshProcessExitObservation::new(ImmediateExit, Arc::new(ImmediateExit))
}

struct ImmediateExit;

impl SshProcessExitWait for ImmediateExit {
    fn wait(&mut self) -> Result<SshProcessExitWake, SshProcessMechanismError> {
        Ok(SshProcessExitWake::Exit)
    }
}

impl SshProcessExitInterrupt for ImmediateExit {
    fn interrupt(&self) {}
}

/// Waits for one child exit or a private cancellation, through the host kernel process event
/// facility: a kqueue `EVFILT_PROC` registration on macOS and a pidfd on Linux.
struct NativeExitWait {
    events: OwnedFd,
    cancellation: UnixStream,
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    process: usize,
    interrupt: Arc<NativeExitInterrupt>,
}

struct NativeExitInterrupt(Mutex<Option<UnixStream>>);

impl SshProcessExitInterrupt for NativeExitInterrupt {
    fn interrupt(&self) {
        let stream = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(stream) = stream else { return };
        // Shutdown wakes the peer even if a concurrent fork inherited a duplicate endpoint.
        // This socket is privately owned and never connected or reconfigured after creation.
        loop {
            match stream.shutdown(Shutdown::Write) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                _ => return,
            }
        }
    }
}

impl Drop for NativeExitWait {
    fn drop(&mut self) {
        // The portable stop handle can outlive this waiter after fallback or exit. Release its
        // native endpoint now rather than retaining a descriptor until the Workspace closes.
        self.interrupt.interrupt();
    }
}

fn native_observation(process: u32) -> Result<SshProcessExitObservation, SshProcessMechanismError> {
    let (cancellation, interrupt) =
        UnixStream::pair().map_err(|_| SshProcessMechanismError::StatusFailed)?;
    let events = register_exit_events(process, &cancellation)?;
    let interrupt = Arc::new(NativeExitInterrupt(Mutex::new(Some(interrupt))));
    Ok(SshProcessExitObservation::new(
        NativeExitWait {
            events,
            cancellation,
            process: process as usize,
            interrupt: Arc::clone(&interrupt),
        },
        interrupt,
    ))
}

#[cfg(target_os = "macos")]
fn register_exit_events(
    process: u32,
    cancellation: &UnixStream,
) -> Result<OwnedFd, SshProcessMechanismError> {
    // SAFETY: kqueue has no input pointers and returns a new owned descriptor on success.
    let descriptor = unsafe { libc::kqueue() };
    if descriptor < 0 {
        return Err(SshProcessMechanismError::StatusFailed);
    }
    // SAFETY: this successful kqueue descriptor has exactly one owner.
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    // SAFETY: queue remains live; FD_CLOEXEC affects only this privately owned descriptor.
    if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(SshProcessMechanismError::StatusFailed);
    }
    let changes = [
        event(process as usize, libc::EVFILT_PROC, libc::NOTE_EXIT),
        event(cancellation.as_raw_fd() as usize, libc::EVFILT_READ, 0),
    ];
    loop {
        // SAFETY: both initialized changes and the live queue are valid for the duration of this
        // call. No output events are requested, so registration cannot wait for process exit.
        let result = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                changes.as_ptr(),
                2,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result >= 0 {
            return Ok(queue);
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(SshProcessMechanismError::StatusFailed);
        }
    }
}

#[cfg(target_os = "macos")]
fn event(ident: usize, filter: i16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags: libc::EV_ADD | libc::EV_ENABLE,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

#[cfg(target_os = "macos")]
impl SshProcessExitWait for NativeExitWait {
    fn wait(&mut self) -> Result<SshProcessExitWake, SshProcessMechanismError> {
        loop {
            let mut events = [event(0, 0, 0), event(0, 0, 0)];
            // SAFETY: queue is owned and open, and events holds space for both requested records.
            // A null timeout blocks until process exit or cancellation; no polling timer exists.
            let count = unsafe {
                libc::kevent(
                    self.events.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    2,
                    std::ptr::null(),
                )
            };
            if count < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(SshProcessMechanismError::StatusFailed);
            }
            let events = &events[..count as usize];
            if events.iter().any(|event| {
                event.filter == libc::EVFILT_READ
                    && event.ident == self.cancellation.as_raw_fd() as usize
            }) {
                return Ok(SshProcessExitWake::Interrupted);
            }
            for event in events {
                if event.flags & libc::EV_ERROR != 0 {
                    return Err(SshProcessMechanismError::StatusFailed);
                }
                if event.filter == libc::EVFILT_PROC
                    && event.ident == self.process
                    && event.fflags & libc::NOTE_EXIT != 0
                {
                    return Ok(SshProcessExitWake::Exit);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn register_exit_events(
    process: u32,
    _cancellation: &UnixStream,
) -> Result<OwnedFd, SshProcessMechanismError> {
    let process = libc::pid_t::try_from(process).map_err(|_| SshProcessMechanismError::StatusFailed)?;
    // SAFETY: pidfd_open takes a process identifier and flags and returns a new close-on-exec
    // descriptor on success. The exclusive process borrow keeps the unreaped child identity.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, process, 0) };
    if descriptor < 0 {
        // Kernels before 5.3 lack pidfd; the caller falls back to status polling.
        return Err(SshProcessMechanismError::StatusFailed);
    }
    let descriptor =
        i32::try_from(descriptor).map_err(|_| SshProcessMechanismError::StatusFailed)?;
    // SAFETY: this successful pidfd_open descriptor has exactly one owner.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

#[cfg(target_os = "linux")]
impl SshProcessExitWait for NativeExitWait {
    fn wait(&mut self) -> Result<SshProcessExitWake, SshProcessMechanismError> {
        loop {
            let mut events = [
                libc::pollfd {
                    fd: self.events.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.cancellation.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: both pollfd records are initialized and their descriptors remain open.
            // A negative timeout blocks until process exit or cancellation.
            let count = unsafe { libc::poll(events.as_mut_ptr(), 2, -1) };
            if count < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(SshProcessMechanismError::StatusFailed);
            }
            if events[1].revents != 0 {
                return Ok(SshProcessExitWake::Interrupted);
            }
            if events[0].revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(SshProcessMechanismError::StatusFailed);
            }
            if events[0].revents & libc::POLLIN != 0 {
                return Ok(SshProcessExitWake::Exit);
            }
        }
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::ssh::process::{
        ProcessSignal, SpawnedSshProcess, SshProcessSpawnRequest, SshProcessStdio,
    };

    fn child(script: &str) -> SpawnedSshProcess<UnixSshProcess> {
        UnixSshProcessAdapter
            .spawn(SshProcessSpawnRequest::new(
                PathBuf::from("/bin/sh"),
                vec![OsString::from("-c"), OsString::from(script)],
                PathBuf::from("/tmp"),
                Vec::new(),
                SshProcessStdio::Piped,
                SshProcessStdio::Null,
                SshProcessStdio::Null,
            ))
            .unwrap()
    }

    #[test]
    fn exit_observation_can_interrupt_a_live_child() {
        for stop_before_wait in [true, false] {
            let adapter = UnixSshProcessAdapter;
            let mut child = child("read value");
            let mut observation = adapter.observe_exit(child.process_mut()).unwrap();
            let interrupt = observation.interrupt_handle();
            if stop_before_wait {
                interrupt.interrupt();
            }
            let (entered_tx, entered_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let waiter = std::thread::spawn(move || {
                entered_tx.send(()).unwrap();
                result_tx.send(observation.wait()).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            if !stop_before_wait {
                interrupt.interrupt();
                interrupt.interrupt();
            }
            let result = result_rx.recv_timeout(Duration::from_secs(2));
            let still_running = adapter.try_status(child.process_mut()).unwrap().is_none();
            adapter
                .signal(child.process_mut(), ProcessSignal::Kill)
                .unwrap();
            adapter.reap(child.into_process()).unwrap();
            waiter.join().unwrap();
            assert!(
                still_running,
                "interrupting observation must not terminate its child"
            );
            assert!(
                matches!(result, Ok(Ok(SshProcessExitWake::Interrupted))),
                "native exit wait must respond to interruption"
            );
        }
    }

    #[test]
    fn exit_observation_does_not_consume_owned_exit_status() {
        let adapter = UnixSshProcessAdapter;
        let mut child = child("read value");
        let mut observation = adapter.observe_exit(child.process_mut()).unwrap();
        let interrupt = observation.interrupt_handle();
        let (result_tx, result_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || result_tx.send(observation.wait()).unwrap());
        adapter
            .signal(child.process_mut(), ProcessSignal::Kill)
            .unwrap();
        let result = result_rx.recv_timeout(Duration::from_secs(2));
        interrupt.interrupt();
        waiter.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            let status = adapter.try_status(child.process_mut());
            if !matches!(status, Ok(None)) || Instant::now() >= deadline {
                break status;
            }
            // Exit notification can precede collectible status. Collection remains with the owner.
            std::thread::yield_now();
        };
        adapter.reap(child.into_process()).unwrap();
        assert!(matches!(result, Ok(Ok(SshProcessExitWake::Exit))));
        assert!(
            matches!(status, Ok(Some(_))),
            "the process owner must retain exit status collection"
        );
    }

    #[test]
    fn exit_during_observer_registration_is_not_lost() {
        let adapter = UnixSshProcessAdapter;
        for _ in 0..32 {
            let mut child = child("exit 7");
            let mut observation = adapter.observe_exit(child.process_mut()).unwrap();
            let interrupt = observation.interrupt_handle();
            let (result_tx, result_rx) = mpsc::channel();
            let waiter = std::thread::spawn(move || result_tx.send(observation.wait()).unwrap());
            let result = result_rx.recv_timeout(Duration::from_secs(2));
            interrupt.interrupt();
            waiter.join().unwrap();
            adapter.reap(child.into_process()).unwrap();
            assert!(
                matches!(result, Ok(Ok(SshProcessExitWake::Exit))),
                "an early child exit must remain observable"
            );
        }
    }
}
