use std::env;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Error as AnyError;
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use thiserror::Error;

const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

struct ChildProcessState {
    child: Option<Box<dyn Child + Send + Sync>>,
    termination_sent: bool,
}

struct SharedChildProcess {
    state: Mutex<ChildProcessState>,
}

impl SharedChildProcess {
    fn new(child: Box<dyn Child + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(ChildProcessState {
                child: Some(child),
                termination_sent: false,
            }),
        }
    }

    fn terminate(&self) -> io::Result<()> {
        let mut state = self.lock_state();
        if state.termination_sent {
            return Ok(());
        }

        let Some(child) = state.child.as_mut() else {
            return Ok(());
        };
        child.kill()?;
        state.termination_sent = true;
        Ok(())
    }

    fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        let mut state = self.lock_state();
        let Some(child) = state.child.as_mut() else {
            return Err(child_already_reaped_error());
        };
        let status = child.try_wait()?;
        if status.is_some() {
            state.child.take();
        }
        Ok(status)
    }

    fn cleanup(&self) {
        let mut state = self.lock_state();
        let Some(mut child) = state.child.take() else {
            return;
        };

        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => eprintln!("failed to inspect shell process during cleanup: {error}"),
        }

        if !state.termination_sent {
            match child.kill() {
                Ok(()) => state.termination_sent = true,
                Err(error) => {
                    eprintln!("failed to terminate shell process during cleanup: {error}");
                    child_exited_after_failed_termination(child.as_mut());
                    return;
                }
            }
        }

        if let Err(error) = child.wait() {
            eprintln!("failed to reap shell process during cleanup: {error}");
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ChildProcessState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                eprintln!("terminal child-process coordination lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }
}

fn child_already_reaped_error() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "shell process was already reaped")
}

pub(crate) struct PtyTerminator {
    child: Arc<SharedChildProcess>,
}

impl PtyTerminator {
    fn new(child: Arc<SharedChildProcess>) -> Self {
        Self { child }
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        self.child.terminate()
    }

    #[cfg(test)]
    pub(crate) fn for_test(child: Box<dyn Child + Send + Sync>) -> Self {
        Self::new(Arc::new(SharedChildProcess::new(child)))
    }
}

pub(crate) struct SpawnedPty {
    pub(crate) master: Box<dyn MasterPty + Send>,
    pub(crate) reader: Option<Box<dyn Read + Send>>,
    pub(crate) writer: Box<dyn Write + Send>,
    child: Arc<SharedChildProcess>,
}

impl SpawnedPty {
    pub(crate) fn wait_for_child(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
    }
}

impl Drop for SpawnedPty {
    fn drop(&mut self) {
        self.child.cleanup();
    }
}

fn child_exited_after_failed_termination(child: &mut dyn Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            eprintln!(
                "shell process is still running after termination failed; it could not be reaped"
            );
        }
        Err(error) => {
            eprintln!("failed to recheck shell process after termination failed: {error}");
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum PtyError {
    #[error("failed to open the macOS pseudo-terminal: {0}")]
    Open(#[source] AnyError),
    #[error("failed to start shell {shell}: {source}")]
    SpawnShell {
        shell: String,
        #[source]
        source: AnyError,
    },
    #[error("failed to clone the pseudo-terminal reader: {0}")]
    CloneReader(#[source] AnyError),
    #[error("failed to acquire the pseudo-terminal writer: {0}")]
    TakeWriter(#[source] AnyError),
}

pub(crate) fn spawn_user_shell(
    size: PtySize,
    working_directory: &Path,
) -> Result<(SpawnedPty, PtyTerminator), PtyError> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size).map_err(PtyError::Open)?;

    let shell = user_shell();

    let mut command = CommandBuilder::new(&shell);
    command.arg("-l");
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "SpaceTerm");
    command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

    command.cwd(working_directory);

    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|source| PtyError::SpawnShell {
            shell: shell.clone(),
            source,
        })?;

    // The application never uses the slave side after spawning the shell.
    drop(pair.slave);

    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(source) => {
            terminate_after_startup_failure(child.as_mut());
            return Err(PtyError::CloneReader(source));
        }
    };
    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(source) => {
            terminate_after_startup_failure(child.as_mut());
            return Err(PtyError::TakeWriter(source));
        }
    };

    let child = Arc::new(SharedChildProcess::new(child));
    let terminator = PtyTerminator::new(Arc::clone(&child));

    Ok((
        SpawnedPty {
            master: pair.master,
            reader: Some(reader),
            writer,
            child,
        },
        terminator,
    ))
}

pub(crate) fn user_shell() -> String {
    env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

fn terminate_after_startup_failure(child: &mut dyn Child) {
    if let Err(error) = child.kill() {
        eprintln!("failed to terminate shell after PTY setup failed: {error}");
        return;
    }
    if let Err(error) = child.wait() {
        eprintln!("failed to reap shell after PTY setup failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use portable_pty::{ChildKiller, ExitStatus};

    use super::*;

    #[derive(Default)]
    struct TestMasterPty;

    impl MasterPty for TestMasterPty {
        fn resize(&self, _size: PtySize) -> Result<(), AnyError> {
            Ok(())
        }

        fn get_size(&self) -> Result<PtySize, AnyError> {
            Ok(PtySize::default())
        }

        fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, AnyError> {
            Ok(Box::new(io::empty()))
        }

        fn take_writer(&self) -> Result<Box<dyn Write + Send>, AnyError> {
            Ok(Box::new(io::sink()))
        }

        fn process_group_leader(&self) -> Option<i32> {
            None
        }

        fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
            None
        }

        fn tty_name(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    #[derive(Clone, Default)]
    struct CleanupCounts {
        try_wait: Arc<AtomicUsize>,
        kill: Arc<AtomicUsize>,
        wait: Arc<AtomicUsize>,
    }

    struct TestChild {
        cleanup: CleanupCounts,
    }

    struct RacedExitChild {
        cleanup: CleanupCounts,
    }

    struct ExitedChild {
        cleanup: CleanupCounts,
    }

    impl fmt::Debug for TestChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("TestChild").finish_non_exhaustive()
        }
    }

    impl ChildKiller for TestChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self {
                cleanup: self.cleanup.clone(),
            })
        }
    }

    impl Child for TestChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for RacedExitChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("RacedExitChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for RacedExitChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.kill.fetch_add(1, Ordering::Relaxed);
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test termination failure",
            ))
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self {
                cleanup: self.cleanup.clone(),
            })
        }
    }

    impl Child for RacedExitChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            let attempt = self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok((attempt > 0).then(|| ExitStatus::with_exit_code(0)))
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for ExitedChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("ExitedChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for ExitedChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self {
                cleanup: self.cleanup.clone(),
            })
        }
    }

    impl Child for ExitedChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(Some(ExitStatus::with_exit_code(0)))
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    #[test]
    fn dropping_a_spawned_pty_kills_and_reaps_the_active_process_once() {
        let cleanup = CleanupCounts::default();
        let pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child: Arc::new(SharedChildProcess::new(Box::new(TestChild {
                cleanup: cleanup.clone(),
            }))),
        };

        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.kill.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn waiting_for_a_child_marks_termination_complete() {
        let cleanup = CleanupCounts::default();
        let child = Arc::new(SharedChildProcess::new(Box::new(ExitedChild {
            cleanup: cleanup.clone(),
        })));
        let terminator = PtyTerminator::new(Arc::clone(&child));
        let mut pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child,
        };

        pty.wait_for_child().unwrap();
        drop(pty);
        terminator.terminate().unwrap();

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.kill.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 0)
        );
    }

    #[test]
    fn observing_an_exited_child_marks_termination_complete() {
        let cleanup = CleanupCounts::default();
        let child = Arc::new(SharedChildProcess::new(Box::new(ExitedChild {
            cleanup: cleanup.clone(),
        })));
        let terminator = PtyTerminator::new(Arc::clone(&child));
        let pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child,
        };

        drop(pty);
        terminator.terminate().unwrap();

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.kill.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 0)
        );
    }

    #[test]
    fn terminator_uses_owner_kill_and_drop_reaps_once() {
        let cleanup = CleanupCounts::default();
        let child = Arc::new(SharedChildProcess::new(Box::new(TestChild {
            cleanup: cleanup.clone(),
        })));
        let terminator = PtyTerminator::new(Arc::clone(&child));
        let pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child,
        };

        terminator.terminate().unwrap();
        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.kill.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn drop_reaps_a_child_that_exits_while_kill_fails() {
        let cleanup = CleanupCounts::default();
        let pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child: Arc::new(SharedChildProcess::new(Box::new(RacedExitChild {
                cleanup: cleanup.clone(),
            }))),
        };

        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.kill.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (2, 1, 0)
        );
    }
}
