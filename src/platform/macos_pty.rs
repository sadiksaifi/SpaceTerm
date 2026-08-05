use std::env;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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
        let (child, termination_sent) = {
            let mut state = self.lock_state();
            (state.child.take(), state.termination_sent)
        };
        let Some(mut child) = child else {
            return;
        };

        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => eprintln!("failed to inspect shell process during cleanup: {error}"),
        }

        if !termination_sent && let Err(error) = child.kill() {
            eprintln!("failed to terminate shell process during cleanup: {error}");
            child_exited_after_failed_termination(child.as_mut());
            return;
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
}

pub(crate) struct SpawnedPty {
    master: Box<dyn MasterPty + Send>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    child: Arc<SharedChildProcess>,
}

impl SpawnedPty {
    pub(crate) fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
        self.reader
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PTY reader was already taken"))
    }

    pub(crate) fn resize(&self, size: PtySize) -> Result<(), AnyError> {
        self.master.resize(size)
    }

    pub(crate) fn wait_for_child(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out after {} ms waiting for the shell process to exit",
                        timeout.as_millis()
                    ),
                ));
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
        }
    }
}

impl Write for SpawnedPty {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.writer.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Instant;

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

    #[derive(Clone, Default)]
    struct BlockingWaitControl {
        state: Arc<(Mutex<BlockingWaitState>, Condvar)>,
    }

    #[derive(Default)]
    struct BlockingWaitState {
        entered: bool,
        released: bool,
    }

    impl BlockingWaitControl {
        fn block(&self) {
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap();
            state.entered = true;
            changed.notify_all();
            while !state.released {
                state = changed.wait(state).unwrap();
            }
        }

        fn wait_until_entered(&self) {
            let (state, changed) = &*self.state;
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut state = state.lock().unwrap();
            while !state.entered {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out waiting for child.wait()");
                let (next_state, timeout) = changed.wait_timeout(state, remaining).unwrap();
                state = next_state;
                assert!(
                    !timeout.timed_out() || state.entered,
                    "timed out waiting for child.wait()"
                );
            }
        }

        fn release(&self) {
            let (state, changed) = &*self.state;
            state.lock().unwrap().released = true;
            changed.notify_all();
        }
    }

    struct BlockingWaitChild {
        cleanup: CleanupCounts,
        wait_control: BlockingWaitControl,
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

    impl fmt::Debug for BlockingWaitChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BlockingWaitChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for BlockingWaitChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self {
                cleanup: self.cleanup.clone(),
                wait_control: self.wait_control.clone(),
            })
        }
    }

    impl Child for BlockingWaitChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            self.wait_control.block();
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    #[test]
    fn spawned_pty_should_expose_single_owner_io_operations() {
        let cleanup = CleanupCounts::default();
        let mut pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child: Arc::new(SharedChildProcess::new(Box::new(ExitedChild { cleanup }))),
        };

        pty.resize(PtySize::default()).unwrap();
        pty.write_all(b"input").unwrap();
        pty.flush().unwrap();
        drop(pty.take_reader().unwrap());
        let error = pty.take_reader().err().unwrap();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
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
    fn terminator_should_not_wait_for_cleanup_that_owns_the_child() {
        let cleanup = CleanupCounts::default();
        let wait_control = BlockingWaitControl::default();
        let child = Arc::new(SharedChildProcess::new(Box::new(BlockingWaitChild {
            cleanup: cleanup.clone(),
            wait_control: wait_control.clone(),
        })));
        let terminator = PtyTerminator::new(Arc::clone(&child));
        let pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child,
        };
        let (cleanup_finished, cleanup_completion) = mpsc::sync_channel(1);

        let cleanup_thread = thread::spawn(move || {
            drop(pty);
            cleanup_finished.send(()).unwrap();
        });
        wait_control.wait_until_entered();

        let (termination_finished, termination_completion) = mpsc::sync_channel(1);
        let termination_thread = thread::spawn(move || {
            termination_finished.send(terminator.terminate()).unwrap();
        });
        let termination = match termination_completion.recv_timeout(Duration::from_millis(250)) {
            Ok(termination) => termination,
            Err(error) => {
                wait_control.release();
                cleanup_thread.join().unwrap();
                termination_thread.join().unwrap();
                panic!("termination waited for child cleanup: {error}");
            }
        };

        termination.unwrap();
        assert!(matches!(
            cleanup_completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        termination_thread.join().unwrap();

        wait_control.release();
        cleanup_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        cleanup_thread.join().unwrap();
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

        pty.wait_for_child(Duration::from_secs(1)).unwrap();
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
    fn waiting_for_a_live_child_should_honor_the_deadline() {
        let cleanup = CleanupCounts::default();
        let mut pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: Some(Box::new(io::empty())),
            writer: Box::new(io::sink()),
            child: Arc::new(SharedChildProcess::new(Box::new(TestChild {
                cleanup: cleanup.clone(),
            }))),
        };

        let error = pty.wait_for_child(Duration::ZERO).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "timed out after 0 ms waiting for the shell process to exit"
        );
        assert_eq!(cleanup.try_wait.load(Ordering::Relaxed), 1);
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
