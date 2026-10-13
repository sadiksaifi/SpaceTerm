//! Native child creation shared by the Server and desktop adapters.

use std::io;
use std::process::{Child, Command};

use thiserror::Error;

/// Content-free native spawn failure classifications.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SpawnError {
    /// The selected program or its starting directory is absent.
    #[error("native program is unavailable")]
    NotFound,
    /// The host refused execution of the selected program.
    #[error("native program execution is denied")]
    PermissionDenied,
    /// Native child creation could not complete.
    #[error("native process creation is unavailable")]
    Unavailable,
}

/// Creates a child from an already prepared native command.
///
/// On macOS, native adapters must use this operation rather than `Command::spawn`.
/// It excludes non-atomic CLOEXEC setup and listener retirement until the child has
/// exec'd or failed. CLOEXEC alone does not exclude a live duplicate before exec.
/// Child callbacks must not acquire this module's descriptor guard.
pub fn spawn(command: &mut Command) -> Result<Child, SpawnError> {
    descriptor_guard()
        .and_then(|_guard| command.spawn())
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => SpawnError::NotFound,
            io::ErrorKind::PermissionDenied => SpawnError::PermissionDenied,
            _ => SpawnError::Unavailable,
        })
}

#[cfg(target_os = "macos")]
static DESCRIPTORS: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) struct DescriptorGuard {
    #[cfg(target_os = "macos")]
    _lock: std::sync::MutexGuard<'static, ()>,
}

pub(crate) fn descriptor_guard() -> io::Result<DescriptorGuard> {
    Ok(DescriptorGuard {
        #[cfg(target_os = "macos")]
        _lock: DESCRIPTORS.lock().map_err(|_| io::ErrorKind::Other)?,
    })
}

// Tokio creates and accepts sockets while polling. Protect each native attempt,
// not the async wait: holding the guard across Pending would block process creation.
pub(crate) async fn descriptor_io<T>(
    operation: impl std::future::Future<Output = io::Result<T>>,
) -> io::Result<T> {
    let mut operation = std::pin::pin!(operation);
    std::future::poll_fn(|context| {
        let _guard = match descriptor_guard() {
            Ok(guard) => guard,
            Err(error) => return std::task::Poll::Ready(Err(error)),
        };
        operation.as_mut().poll(context)
    })
    .await
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn spawn_excludes_non_atomic_close_on_exec_setup() {
        const FIXTURE: &str = "SPACETERM_SPAWN_FD_FIXTURE";
        if let Ok(value) = std::env::var(FIXTURE) {
            if value != "create" {
                let facts = value
                    .split(',')
                    .map(|value| value.parse::<u64>().unwrap())
                    .collect::<Vec<_>>();
                let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
                // SAFETY: status is writable and fstat does not take ownership of the numeric fd.
                if unsafe { libc::fstat(facts[0] as i32, status.as_mut_ptr()) } == 0 {
                    // A child may reuse the descriptor number, but never the parent's socket identity.
                    let status = unsafe { status.assume_init() };
                    assert_ne!((status.st_dev as u64, status.st_ino), (facts[1], facts[2]));
                }
                return;
            }
        } else {
            // Isolate the deliberate setup pause from other native tests' deadlines.
            let status = spawn(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "process::tests::spawn_excludes_non_atomic_close_on_exec_setup",
                        "--nocapture",
                    ])
                    .env(FIXTURE, "create"),
            )
            .unwrap()
            .wait()
            .unwrap();
            assert!(status.success());
            return;
        }

        let guard = descriptor_guard().unwrap();
        // SAFETY: socket returns one new owned descriptor, deliberately not CLOEXEC yet.
        let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(raw >= 0);
        let socket = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        assert_eq!(unsafe { libc::fstat(raw, status.as_mut_ptr()) }, 0);
        let status = unsafe { status.assume_init() };
        let fixture = format!("{raw},{},{}", status.st_dev as u64, status.st_ino);
        let (started, starting) = mpsc::channel();
        let (spawned, finished) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "process::tests::spawn_excludes_non_atomic_close_on_exec_setup",
                    "--nocapture",
                ])
                .env(FIXTURE, fixture)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            started.send(()).unwrap();
            spawned.send(spawn(&mut command)).unwrap();
        });
        starting.recv_timeout(Duration::from_secs(2)).unwrap();
        let early = finished.recv_timeout(Duration::from_millis(100));
        let waited = matches!(&early, Err(mpsc::RecvTimeoutError::Timeout));
        assert_eq!(
            unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
        drop(guard);
        let mut child = match early {
            Ok(child) => child.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => finished
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            Err(error) => panic!("spawn worker disconnected: {error}"),
        };
        worker.join().unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "the child inherited the socket");
        assert!(waited, "spawn must wait for CLOEXEC setup");
    }
}
