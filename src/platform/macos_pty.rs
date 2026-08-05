use std::env;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Error as AnyError;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use thiserror::Error;

const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

struct ChildTermination {
    signaller: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
}

impl ChildTermination {
    fn new(signaller: Box<dyn ChildKiller + Send + Sync>) -> Self {
        Self {
            signaller: Mutex::new(Some(signaller)),
        }
    }

    fn signal(&self) -> io::Result<()> {
        let mut gate = self.lock_signaller();
        let Some(mut signaller) = gate.take() else {
            return Ok(());
        };
        // Keep the liveness gate locked through signal delivery so the worker cannot reap the
        // process and allow its PID to be reused before this one-shot capability is consumed.
        let result = signaller.kill();
        drop(gate);
        result
    }

    fn lock_signaller(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<Box<dyn ChildKiller + Send + Sync>>> {
        match self.signaller.lock() {
            Ok(signaller) => signaller,
            Err(poisoned) => {
                eprintln!("terminal child-termination coordination lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }
}

fn child_already_reaped_error() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "shell process was already reaped")
}

pub(crate) struct PtyTerminator {
    termination: Arc<ChildTermination>,
}

impl PtyTerminator {
    fn new(termination: Arc<ChildTermination>) -> Self {
        Self { termination }
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        self.termination.signal()
    }
}

pub(crate) struct SpawnedPty {
    master: Box<dyn MasterPty + Send>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    termination: Arc<ChildTermination>,
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
            if let Some(status) = self.try_wait_for_child()? {
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

    fn try_wait_for_child(&mut self) -> io::Result<Option<ExitStatus>> {
        let mut signaller = self.termination.lock_signaller();
        let Some(child) = self.child.as_mut() else {
            return Err(child_already_reaped_error());
        };
        let status = child.try_wait()?;
        if status.is_some() {
            signaller.take();
            self.child.take();
        }
        Ok(status)
    }

    fn cleanup_child(&mut self) {
        let child = {
            let mut signaller = self.termination.lock_signaller();
            signaller.take();
            self.child.take()
        };
        let Some(mut child) = child else {
            return;
        };

        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => eprintln!("failed to inspect shell process during cleanup: {error}"),
        }

        if let Err(error) = child.kill() {
            eprintln!("failed to terminate shell process during cleanup: {error}");
            Self::report_child_after_failed_termination(child.as_mut());
            return;
        }

        if let Err(error) = child.wait() {
            eprintln!("failed to reap shell process during cleanup: {error}");
        }
    }

    fn report_child_after_failed_termination(child: &mut dyn Child) {
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
        self.cleanup_child();
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

    let termination = Arc::new(ChildTermination::new(child.clone_killer()));
    let terminator = PtyTerminator::new(Arc::clone(&termination));

    Ok((
        SpawnedPty {
            master: pair.master,
            reader: Some(reader),
            writer,
            child: Some(child),
            termination,
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
        owner_kill: Arc<AtomicUsize>,
        signal: Arc<AtomicUsize>,
        wait: Arc<AtomicUsize>,
    }

    #[derive(Clone, Debug)]
    struct TestSignaller {
        signals: Arc<AtomicUsize>,
        fail: bool,
    }

    impl TestSignaller {
        fn new(cleanup: &CleanupCounts) -> Self {
            Self {
                signals: Arc::clone(&cleanup.signal),
                fail: false,
            }
        }

        fn failing(signals: Arc<AtomicUsize>) -> Self {
            Self {
                signals,
                fail: true,
            }
        }
    }

    impl ChildKiller for TestSignaller {
        fn kill(&mut self) -> io::Result<()> {
            self.signals.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "test signal failure",
                ))
            } else {
                Ok(())
            }
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    struct TestChild {
        cleanup: CleanupCounts,
    }

    struct FailingSignalChild {
        cleanup: CleanupCounts,
    }

    struct RacedExitChild {
        cleanup: CleanupCounts,
    }

    struct ExitedChild {
        cleanup: CleanupCounts,
    }

    #[derive(Clone, Default)]
    struct BlockingCallControl {
        state: Arc<(Mutex<BlockingCallState>, Condvar)>,
    }

    #[derive(Default)]
    struct BlockingCallState {
        entered: bool,
        released: bool,
    }

    impl BlockingCallControl {
        fn block(&self) {
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap();
            state.entered = true;
            changed.notify_all();
            while !state.released {
                state = changed.wait(state).unwrap();
            }
        }

        fn wait_until_entered(&self, operation: &str) {
            let (state, changed) = &*self.state;
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut state = state.lock().unwrap();
            while !state.entered {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out waiting for {operation}");
                let (next_state, timeout) = changed.wait_timeout(state, remaining).unwrap();
                state = next_state;
                assert!(
                    !timeout.timed_out() || state.entered,
                    "timed out waiting for {operation}"
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
        wait_control: BlockingCallControl,
    }

    struct BlockingOwnerKillChild {
        cleanup: CleanupCounts,
        kill_control: BlockingCallControl,
    }

    struct BlockingTryWaitChild {
        cleanup: CleanupCounts,
        try_wait_control: BlockingCallControl,
    }

    impl fmt::Debug for TestChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("TestChild").finish_non_exhaustive()
        }
    }

    impl ChildKiller for TestChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
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

    impl fmt::Debug for FailingSignalChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("FailingSignalChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for FailingSignalChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::failing(Arc::clone(&self.cleanup.signal)))
        }
    }

    impl Child for FailingSignalChild {
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
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test termination failure",
            ))
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
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
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
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
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
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

    impl fmt::Debug for BlockingOwnerKillChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BlockingOwnerKillChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for BlockingOwnerKillChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            self.kill_control.block();
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for BlockingOwnerKillChild {
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

    impl fmt::Debug for BlockingTryWaitChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BlockingTryWaitChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for BlockingTryWaitChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for BlockingTryWaitChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            self.try_wait_control.block();
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

    fn spawned_pty(child: Box<dyn Child + Send + Sync>) -> (SpawnedPty, PtyTerminator) {
        let termination = Arc::new(ChildTermination::new(child.clone_killer()));
        let terminator = PtyTerminator::new(Arc::clone(&termination));
        (
            SpawnedPty {
                master: Box::new(TestMasterPty),
                reader: Some(Box::new(io::empty())),
                writer: Box::new(io::sink()),
                child: Some(child),
                termination,
            },
            terminator,
        )
    }

    #[test]
    fn spawned_pty_should_expose_single_owner_io_operations() {
        let cleanup = CleanupCounts::default();
        let (mut pty, _terminator) = spawned_pty(Box::new(ExitedChild { cleanup }));

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
        let (pty, _terminator) = spawned_pty(Box::new(TestChild {
            cleanup: cleanup.clone(),
        }));

        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn terminator_should_signal_without_invoking_owner_kill() {
        let cleanup = CleanupCounts::default();
        let kill_control = BlockingCallControl::default();
        let (pty, terminator) = spawned_pty(Box::new(BlockingOwnerKillChild {
            cleanup: cleanup.clone(),
            kill_control: kill_control.clone(),
        }));
        let (termination_finished, termination_completion) = mpsc::sync_channel(1);

        let termination_thread = thread::spawn(move || {
            termination_finished.send(terminator.terminate()).unwrap();
        });
        let termination = match termination_completion.recv_timeout(Duration::from_millis(250)) {
            Ok(termination) => termination,
            Err(error) => {
                kill_control.release();
                termination_thread.join().unwrap();
                panic!("termination invoked or waited for owner kill: {error}");
            }
        };

        termination.unwrap();
        termination_thread.join().unwrap();
        assert_eq!(
            (
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
            ),
            (1, 0)
        );

        kill_control.release();
        drop(pty);
        kill_control.wait_until_entered("owner child kill");
        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn terminator_should_not_wait_for_cleanup_blocked_in_owner_kill() {
        let cleanup = CleanupCounts::default();
        let kill_control = BlockingCallControl::default();
        let (pty, terminator) = spawned_pty(Box::new(BlockingOwnerKillChild {
            cleanup: cleanup.clone(),
            kill_control: kill_control.clone(),
        }));
        let (cleanup_finished, cleanup_completion) = mpsc::sync_channel(1);
        let cleanup_thread = thread::spawn(move || {
            drop(pty);
            cleanup_finished.send(()).unwrap();
        });
        kill_control.wait_until_entered("owner child kill");

        let (termination_finished, termination_completion) = mpsc::sync_channel(1);
        let termination_thread = thread::spawn(move || {
            termination_finished.send(terminator.terminate()).unwrap();
        });
        let termination = match termination_completion.recv_timeout(Duration::from_millis(250)) {
            Ok(termination) => termination,
            Err(error) => {
                kill_control.release();
                cleanup_thread.join().unwrap();
                termination_thread.join().unwrap();
                panic!("termination waited for owner kill: {error}");
            }
        };

        termination.unwrap();
        assert!(matches!(
            cleanup_completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            (
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
            ),
            (1, 0)
        );
        termination_thread.join().unwrap();

        kill_control.release();
        cleanup_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        cleanup_thread.join().unwrap();
        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn terminator_should_not_wait_for_cleanup_that_owns_the_child() {
        let cleanup = CleanupCounts::default();
        let wait_control = BlockingCallControl::default();
        let (pty, terminator) = spawned_pty(Box::new(BlockingWaitChild {
            cleanup: cleanup.clone(),
            wait_control: wait_control.clone(),
        }));
        let (cleanup_finished, cleanup_completion) = mpsc::sync_channel(1);

        let cleanup_thread = thread::spawn(move || {
            drop(pty);
            cleanup_finished.send(()).unwrap();
        });
        wait_control.wait_until_entered("owner child wait");

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
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn waiting_for_a_child_marks_termination_complete() {
        let cleanup = CleanupCounts::default();
        let (mut pty, terminator) = spawned_pty(Box::new(ExitedChild {
            cleanup: cleanup.clone(),
        }));

        pty.wait_for_child(Duration::from_secs(1)).unwrap();
        drop(pty);
        terminator.terminate().unwrap();

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 0, 0)
        );
    }

    #[test]
    fn waiting_for_a_live_child_should_honor_the_deadline() {
        let cleanup = CleanupCounts::default();
        let (mut pty, _terminator) = spawned_pty(Box::new(TestChild {
            cleanup: cleanup.clone(),
        }));

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
        let (pty, terminator) = spawned_pty(Box::new(ExitedChild {
            cleanup: cleanup.clone(),
        }));

        drop(pty);
        terminator.terminate().unwrap();

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 0, 0)
        );
    }

    #[test]
    fn repeated_termination_signals_once_and_worker_cleanup_still_kills_and_reaps() {
        let cleanup = CleanupCounts::default();
        let (pty, terminator) = spawned_pty(Box::new(TestChild {
            cleanup: cleanup.clone(),
        }));

        terminator.terminate().unwrap();
        terminator.terminate().unwrap();
        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn failed_termination_signal_should_be_consumed_before_worker_cleanup_recovers() {
        let cleanup = CleanupCounts::default();
        let (pty, terminator) = spawned_pty(Box::new(FailingSignalChild {
            cleanup: cleanup.clone(),
        }));

        let first_error = terminator.terminate().unwrap_err();
        terminator.terminate().unwrap();
        drop(pty);

        assert_eq!(first_error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn child_reap_should_exclude_and_revoke_a_concurrent_signal() {
        let cleanup = CleanupCounts::default();
        let try_wait_control = BlockingCallControl::default();
        let (mut pty, terminator) = spawned_pty(Box::new(BlockingTryWaitChild {
            cleanup: cleanup.clone(),
            try_wait_control: try_wait_control.clone(),
        }));
        let (wait_finished, wait_completion) = mpsc::sync_channel(1);
        let wait_thread = thread::spawn(move || {
            let result = pty.wait_for_child(Duration::from_secs(1));
            wait_finished.send((result, pty)).unwrap();
        });
        try_wait_control.wait_until_entered("child try_wait");

        let (termination_started, termination_start) = mpsc::sync_channel(1);
        let (termination_finished, termination_completion) = mpsc::sync_channel(1);
        let termination_thread = thread::spawn(move || {
            termination_started.send(()).unwrap();
            termination_finished.send(terminator.terminate()).unwrap();
        });
        termination_start.recv().unwrap();
        assert!(matches!(
            termination_completion.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(cleanup.signal.load(Ordering::Relaxed), 0);

        try_wait_control.release();
        let (wait_result, pty) = wait_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        wait_result.unwrap();
        wait_thread.join().unwrap();
        termination_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        termination_thread.join().unwrap();
        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 0, 0)
        );
    }

    #[test]
    fn drop_reaps_a_child_that_exits_while_kill_fails() {
        let cleanup = CleanupCounts::default();
        let (pty, _terminator) = spawned_pty(Box::new(RacedExitChild {
            cleanup: cleanup.clone(),
        }));

        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (2, 1, 0, 0)
        );
    }
}
