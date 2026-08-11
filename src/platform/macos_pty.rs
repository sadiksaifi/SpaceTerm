use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Error as AnyError;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use thiserror::Error;

use crate::platform::shell_integration::{
    ShellEnvironment, configured_mode, plan_shell_integration, resource_root,
};
use crate::terminal::identity;

const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const FORCED_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const FOREIGN_RUNTIME_ENVIRONMENT: &[&str] = &[
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ITERM_SESSION_ID",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_WINDOW_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "STY",
    "TERM_SESSION_ID",
    "TERMINAL_EMULATOR",
    "TMUX",
    "TMUX_PANE",
    "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID",
    "WEZTERM_CONFIG_FILE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_EXECUTABLE_DIR",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WT_PROFILE_ID",
    "WT_SESSION",
    "ZELLIJ",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_SESSION_NAME",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownDisposition {
    NotRequested,
    Graceful,
    Forced,
}

#[derive(Debug)]
pub(crate) struct ShellExit {
    pub(crate) status: ExitStatus,
    pub(crate) shutdown: ShutdownDisposition,
}

struct TerminationTarget {
    process_group: Option<i32>,
    fallback: Box<dyn ChildKiller + Send + Sync>,
}

impl TerminationTarget {
    fn owns_process_group(&self) -> bool {
        self.process_group.is_some()
    }

    fn hang_up(&mut self) -> io::Result<()> {
        match self.process_group {
            Some(process_group) => signal_process_group(process_group, libc::SIGHUP),
            None => self.fallback.kill(),
        }
    }

    fn force(&mut self) -> io::Result<()> {
        match self.process_group {
            Some(process_group) => signal_process_group(process_group, libc::SIGKILL),
            None => self.fallback.kill(),
        }
    }

    fn is_alive(&self) -> io::Result<bool> {
        let Some(process_group) = self.process_group else {
            return Ok(true);
        };
        process_group_is_alive(process_group)
    }
}

struct ChildTermination {
    target: Mutex<Option<TerminationTarget>>,
    requested: AtomicBool,
    forced: AtomicBool,
}

impl ChildTermination {
    fn new(process_group: Option<i32>, fallback: Box<dyn ChildKiller + Send + Sync>) -> Self {
        Self {
            target: Mutex::new(Some(TerminationTarget {
                process_group,
                fallback,
            })),
            requested: AtomicBool::new(false),
            forced: AtomicBool::new(false),
        }
    }

    fn signal(&self) -> io::Result<()> {
        if self.requested.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut target = self.lock_target();
        match target.as_mut() {
            Some(target) => target.hang_up(),
            None => Ok(()),
        }
    }

    fn lock_target(&self) -> std::sync::MutexGuard<'_, Option<TerminationTarget>> {
        match self.target.lock() {
            Ok(target) => target,
            Err(poisoned) => {
                eprintln!("terminal child-termination coordination lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }

    fn revoke(&self) {
        self.lock_target().take();
    }

    fn complete_process_group(&self, graceful_deadline: Instant) -> ShutdownDisposition {
        if !self.requested.load(Ordering::Acquire) {
            self.revoke();
            return ShutdownDisposition::NotRequested;
        }

        let mut target_slot = self.lock_target();
        let Some(target) = target_slot.as_mut() else {
            return if self.forced.load(Ordering::Acquire) {
                ShutdownDisposition::Forced
            } else {
                ShutdownDisposition::Graceful
            };
        };
        if !target.owns_process_group() {
            target_slot.take();
            return ShutdownDisposition::Graceful;
        }

        while Instant::now() < graceful_deadline {
            match target.is_alive() {
                Ok(false) => {
                    target_slot.take();
                    return ShutdownDisposition::Graceful;
                }
                Ok(true) => thread::sleep(CHILD_EXIT_POLL_INTERVAL),
                Err(error) => {
                    eprintln!("failed to inspect shell process group during shutdown: {error}");
                    break;
                }
            }
        }

        if let Err(error) = target.force() {
            eprintln!("failed to force shell process group shutdown: {error}");
        }
        self.forced.store(true, Ordering::Release);
        target_slot.take();
        ShutdownDisposition::Forced
    }

    fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

fn signal_process_group(process_group: i32, signal: i32) -> io::Result<()> {
    // SAFETY: a negative PID addresses the process group created for this PTY.
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn process_group_is_alive(process_group: i32) -> io::Result<bool> {
    // SAFETY: signal zero performs only an existence/permission check.
    if unsafe { libc::kill(-process_group, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
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

    pub(crate) fn hidden_input(&self) -> io::Result<bool> {
        let descriptor = self
            .master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("PTY master descriptor is unavailable"))?;
        read_termios(descriptor).map(|termios| termios_hidden_input(&termios))
    }

    pub(crate) fn wait_for_child(&mut self, timeout: Duration) -> io::Result<ShellExit> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait_for_child()? {
                let shutdown = self
                    .termination
                    .complete_process_group(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT);
                return Ok(ShellExit { status, shutdown });
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
        let Some(child) = self.child.as_mut() else {
            return Err(child_already_reaped_error());
        };
        let status = child.try_wait()?;
        if status.is_some() {
            self.child.take();
        }
        Ok(status)
    }

    fn cleanup_child(&mut self) {
        let child = self.child.take();
        let Some(mut child) = child else {
            return;
        };

        match child.try_wait() {
            Ok(Some(_)) => {
                self.termination
                    .complete_process_group(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT);
                return;
            }
            Ok(None) => {}
            Err(error) => eprintln!("failed to inspect shell process during cleanup: {error}"),
        }

        let owns_process_group = self
            .termination
            .lock_target()
            .as_ref()
            .is_some_and(TerminationTarget::owns_process_group);
        if owns_process_group {
            self.cleanup_process_group(child.as_mut());
            return;
        }

        self.termination.revoke();

        if let Err(error) = child.kill() {
            eprintln!("failed to terminate shell process during cleanup: {error}");
            Self::report_child_after_failed_termination(child.as_mut());
            return;
        }

        if let Err(error) = child.wait() {
            eprintln!("failed to reap shell process during cleanup: {error}");
        }
    }

    fn cleanup_process_group(&self, child: &mut dyn Child) {
        if !self.termination.requested()
            && let Err(error) = self.termination.signal()
        {
            eprintln!("failed to gracefully terminate shell process group: {error}");
        }

        let graceful_deadline = Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT;
        if Self::poll_child_until(child, graceful_deadline) {
            self.termination.complete_process_group(graceful_deadline);
            return;
        }

        self.termination.complete_process_group(graceful_deadline);
        let forced_deadline = Instant::now() + FORCED_SHUTDOWN_TIMEOUT;
        if !Self::poll_child_until(child, forced_deadline) {
            eprintln!("shell process did not become reapable after forced process-group shutdown");
        }
    }

    fn poll_child_until(child: &mut dyn Child, deadline: Instant) -> bool {
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => {}
                Err(error) => {
                    eprintln!("failed to inspect shell process during shutdown: {error}");
                    return false;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
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
    #[error("Workspace working directory {} is unavailable: {source}", path.display())]
    WorkingDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to open the macOS pseudo-terminal: {0}")]
    Open(#[source] AnyError),
    #[error("the macOS pseudo-terminal did not expose its native descriptor")]
    MissingDescriptor,
    #[error("failed to read the macOS pseudo-terminal attributes: {0}")]
    ReadTermios(#[source] io::Error),
    #[error("failed to enable UTF-8 input on the macOS pseudo-terminal: {0}")]
    ConfigureTermios(#[source] io::Error),
    #[error("failed to apply the initial macOS pseudo-terminal size: {0}")]
    InitialResize(#[source] AnyError),
    #[error("failed to start shell {shell}: {source}")]
    SpawnShell {
        shell: String,
        #[source]
        source: AnyError,
    },
    #[error("the shell process did not expose its process-group identifier")]
    MissingProcessGroup,
    #[error("failed to clone the pseudo-terminal reader: {0}")]
    CloneReader(#[source] AnyError),
    #[error("failed to acquire the pseudo-terminal writer: {0}")]
    TakeWriter(#[source] AnyError),
}

pub(crate) fn spawn_user_shell(
    size: PtySize,
    working_directory: &Path,
) -> Result<(SpawnedPty, PtyTerminator), PtyError> {
    validate_working_directory(working_directory)?;
    let shell = user_shell();
    let command = build_shell_command(&shell, working_directory);
    spawn_command_in_pty(size, command, &shell)
}

fn spawn_command_in_pty(
    size: PtySize,
    command: CommandBuilder,
    description: &str,
) -> Result<(SpawnedPty, PtyTerminator), PtyError> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size).map_err(PtyError::Open)?;
    initialize_pty(pair.master.as_ref(), size)?;

    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|source| PtyError::SpawnShell {
            shell: description.to_owned(),
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

    let process_group = child
        .process_id()
        .and_then(|process_id| i32::try_from(process_id).ok());
    let Some(process_group) = process_group else {
        terminate_after_startup_failure(child.as_mut());
        return Err(PtyError::MissingProcessGroup);
    };
    let termination = Arc::new(ChildTermination::new(
        Some(process_group),
        child.clone_killer(),
    ));
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

fn initialize_pty(master: &dyn MasterPty, size: PtySize) -> Result<(), PtyError> {
    let descriptor = master.as_raw_fd().ok_or(PtyError::MissingDescriptor)?;
    let mut termios = read_termios(descriptor).map_err(PtyError::ReadTermios)?;
    termios.c_iflag |= libc::IUTF8;
    // SAFETY: descriptor remains live and termios contains attributes read from this PTY.
    if unsafe { libc::tcsetattr(descriptor, libc::TCSANOW, &termios) } == -1 {
        return Err(PtyError::ConfigureTermios(io::Error::last_os_error()));
    }
    master.resize(size).map_err(PtyError::InitialResize)
}

fn read_termios(descriptor: std::os::fd::RawFd) -> io::Result<libc::termios> {
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: descriptor is a live PTY master and termios points to writable storage.
    if unsafe { libc::tcgetattr(descriptor, termios.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: tcgetattr initialized termios after succeeding above.
    Ok(unsafe { termios.assume_init() })
}

const fn termios_hidden_input(termios: &libc::termios) -> bool {
    termios.c_lflag & libc::ICANON != 0 && termios.c_lflag & libc::ECHO == 0
}

fn validate_working_directory(working_directory: &Path) -> Result<(), PtyError> {
    let metadata = working_directory
        .metadata()
        .map_err(|source| PtyError::WorkingDirectory {
            path: working_directory.to_owned(),
            source,
        })?;
    if metadata.is_dir() {
        Ok(())
    } else {
        Err(PtyError::WorkingDirectory {
            path: working_directory.to_owned(),
            source: io::Error::new(io::ErrorKind::NotADirectory, "path is not a directory"),
        })
    }
}

fn build_shell_command(shell: &str, working_directory: &Path) -> CommandBuilder {
    build_shell_command_with_resources(shell, working_directory, &resource_root())
}

fn build_shell_command_with_resources(
    shell: &str,
    working_directory: &Path,
    resources: &Path,
) -> CommandBuilder {
    let mut command = CommandBuilder::new(shell);
    let integration = plan_shell_integration(
        Path::new(shell),
        resources,
        configured_mode(),
        &ShellEnvironment::capture(),
    );
    integration.apply(&mut command);
    command.arg("-l");
    apply_terminal_identity(&mut command, resources);
    command.cwd(working_directory);
    command
}

fn apply_terminal_identity(command: &mut CommandBuilder, resources: &Path) {
    for name in FOREIGN_RUNTIME_ENVIRONMENT {
        command.env_remove(name);
    }
    command.env_remove("TERMINFO");

    let terminal_identity = identity::launch_identity(resources);
    command.env("TERM", terminal_identity.term);
    if let Some(terminfo) = terminal_identity.terminfo {
        command.env("TERMINFO", terminfo);
    }
    command.env("COLORTERM", identity::COLORTERM);
    command.env("TERM_PROGRAM", identity::COMPATIBILITY_PROGRAM_NAME);
    command.env("TERM_PROGRAM_VERSION", identity::PROGRAM_VERSION);
    command.env("SPACETERM", "1");
}

pub(crate) fn user_shell() -> String {
    env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

#[cfg(test)]
pub(crate) fn conformance_initialization_observation() -> String {
    let command = build_shell_command_with_resources(
        "/bin/zsh",
        Path::new("/tmp"),
        Path::new("/spaceterm-conformance-missing-resources"),
    );
    format!(
        "argv={:?} cwd={} term={} colorterm={} program={} version={} spaceterm={} controlling-tty={}",
        command.get_argv(),
        command
            .get_cwd()
            .and_then(|path| path.to_str())
            .unwrap_or("missing"),
        command
            .get_env("TERM")
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("missing"),
        command
            .get_env("COLORTERM")
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("missing"),
        command
            .get_env("TERM_PROGRAM")
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("missing"),
        command
            .get_env("TERM_PROGRAM_VERSION")
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("missing"),
        command
            .get_env("SPACETERM")
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("missing"),
        command.get_controlling_tty(),
    )
}

#[cfg(test)]
pub(crate) fn conformance_shutdown_observation() -> String {
    use std::sync::atomic::AtomicUsize;

    #[derive(Debug)]
    struct CountingKiller(Arc<AtomicUsize>);

    impl ChildKiller for CountingKiller {
        fn kill(&mut self) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self(Arc::clone(&self.0)))
        }
    }

    let kills = Arc::new(AtomicUsize::new(0));
    let termination = ChildTermination::new(None, Box::new(CountingKiller(Arc::clone(&kills))));
    let first = termination.signal().is_ok();
    let second = termination.signal().is_ok();
    let disposition = termination.complete_process_group(Instant::now());
    format!(
        "first={first} duplicate={second} signals={} disposition={disposition:?} revoked={}",
        kills.load(Ordering::Relaxed),
        termination.lock_target().is_none(),
    )
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
    use std::collections::HashMap;
    use std::fmt;
    use std::io::{self, BufRead, BufReader};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Instant;

    use portable_pty::{ChildKiller, ExitStatus};

    use super::*;

    #[test]
    fn hidden_input_requires_canonical_mode_without_echo() {
        // SAFETY: termios is a plain C data structure whose zero value is valid for flag tests.
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        termios.c_lflag = libc::ICANON | libc::ECHO;
        assert!(!termios_hidden_input(&termios));

        termios.c_lflag = libc::ICANON;
        assert!(termios_hidden_input(&termios));

        termios.c_lflag = 0;
        assert!(!termios_hidden_input(&termios));
    }

    const CONTROLLED_CHILD_ENV: &str = "SPACETERM_CONTROLLED_PTY_CHILD";

    #[test]
    #[ignore = "runs only as a child of the controlled PTY integration tests"]
    fn controlled_pty_child_reports_runtime_configuration() {
        if env::var_os(CONTROLLED_CHILD_ENV).is_none() {
            return;
        }

        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        let mut window_size = std::mem::MaybeUninit::<libc::winsize>::uninit();
        // SAFETY: stdin is the PTY slave installed by portable-pty. Both calls initialize their
        // output structs on success, which is asserted before assume_init.
        let termios_result = unsafe { libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) };
        // SAFETY: TIOCGWINSZ writes a winsize to the valid pointer supplied here.
        let window_result = unsafe {
            libc::ioctl(
                libc::STDIN_FILENO,
                libc::TIOCGWINSZ,
                window_size.as_mut_ptr(),
            )
        };
        assert_eq!(termios_result, 0);
        assert_eq!(window_result, 0);
        // SAFETY: the successful system calls above initialized both values.
        let termios = unsafe { termios.assume_init() };
        // SAFETY: the successful ioctl above initialized the window size.
        let window_size = unsafe { window_size.assume_init() };
        let cwd = env::current_dir().unwrap();

        // SAFETY: these process and terminal identity queries have no pointer preconditions.
        let (pid, group, session, foreground, is_tty) = unsafe {
            (
                libc::getpid(),
                libc::getpgrp(),
                libc::getsid(0),
                libc::tcgetpgrp(libc::STDIN_FILENO),
                libc::isatty(libc::STDIN_FILENO),
            )
        };
        println!(
            "SPACETERM_PTY_REPORT pid={pid} group={group} session={session} foreground={foreground} tty={is_tty} utf8={} rows={} cols={} pixel_width={} pixel_height={} cwd={} marker={}",
            usize::from(termios.c_iflag & libc::IUTF8 != 0),
            window_size.ws_row,
            window_size.ws_col,
            window_size.ws_xpixel,
            window_size.ws_ypixel,
            cwd.display(),
            env::var("SPACETERM_PTY_MARKER").unwrap(),
        );
        thread::sleep(Duration::from_millis(100));
    }

    fn controlled_child_command(working_directory: &Path, marker: &str) -> CommandBuilder {
        let mut command = CommandBuilder::new(env::current_exe().unwrap());
        command.args([
            "--ignored",
            "--exact",
            "platform::macos_pty::tests::controlled_pty_child_reports_runtime_configuration",
            "--nocapture",
        ]);
        command.env(CONTROLLED_CHILD_ENV, "1");
        command.env("SPACETERM_PTY_MARKER", marker);
        command.cwd(working_directory);
        command
    }

    fn read_controlled_report(pty: &mut SpawnedPty) -> HashMap<String, String> {
        let mut output = String::new();
        pty.take_reader()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        let status = pty.wait_for_child(Duration::from_secs(2)).unwrap();
        assert!(
            status.status.success(),
            "controlled PTY child failed: {output}"
        );
        let report = output
            .lines()
            .find(|line| line.starts_with("SPACETERM_PTY_REPORT "))
            .unwrap_or_else(|| panic!("controlled PTY report was missing from: {output}"));

        report
            .split_whitespace()
            .skip(1)
            .map(|field| {
                let (key, value) = field.split_once('=').unwrap();
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }

    #[test]
    fn controlled_child_should_observe_initialized_terminal_state_before_interaction() {
        let working_directory = env::current_dir().unwrap();
        let size = PtySize {
            rows: 31,
            cols: 97,
            pixel_width: 1_164,
            pixel_height: 682,
        };
        let command = controlled_child_command(&working_directory, "initialized");

        let (mut pty, _terminator) =
            spawn_command_in_pty(size, command, "controlled child").unwrap();
        let report = read_controlled_report(&mut pty);

        assert_eq!(
            (
                &report["pid"],
                &report["group"],
                &report["session"],
                &report["foreground"],
                report["tty"].as_str(),
                report["utf8"].as_str(),
                report["rows"].as_str(),
                report["cols"].as_str(),
                report["pixel_width"].as_str(),
                report["pixel_height"].as_str(),
                report["cwd"].as_str(),
                report["marker"].as_str(),
            ),
            (
                &report["pid"],
                &report["pid"],
                &report["pid"],
                &report["pid"],
                "1",
                "1",
                "31",
                "97",
                "1164",
                "682",
                working_directory.to_str().unwrap(),
                "initialized",
            )
        );
    }

    #[test]
    fn concurrent_terminal_sessions_should_own_distinct_process_groups() {
        let working_directory = env::current_dir().unwrap();
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 640,
            pixel_height: 480,
        };
        let (mut first, _first_terminator) = spawn_command_in_pty(
            size,
            controlled_child_command(&working_directory, "first"),
            "first controlled child",
        )
        .unwrap();
        let (mut second, _second_terminator) = spawn_command_in_pty(
            size,
            controlled_child_command(&working_directory, "second"),
            "second controlled child",
        )
        .unwrap();

        let first_report = read_controlled_report(&mut first);
        let second_report = read_controlled_report(&mut second);

        assert_eq!(
            (
                first_report["group"] != second_report["group"],
                first_report["session"] != second_report["session"],
                first_report["marker"].as_str(),
                second_report["marker"].as_str(),
            ),
            (true, true, "first", "second")
        );
    }

    #[test]
    fn stubborn_process_group_should_receive_bounded_forced_shutdown() {
        let working_directory = env::current_dir().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP; (trap '' HUP; exec </dev/null >/dev/null 2>&1; while :; do sleep 1; done) & child=$!; echo SPACETERM_SHUTDOWN_REPORT leader=$$ child=$child; exec </dev/null >/dev/null 2>&1; while :; do sleep 1; done",
        ]);
        command.cwd(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            "stubborn process group",
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let mut report = String::new();
        reader.read_line(&mut report).unwrap();
        let fields: HashMap<_, _> = report
            .split_whitespace()
            .skip_while(|field| *field != "SPACETERM_SHUTDOWN_REPORT")
            .skip(1)
            .map(|field| field.split_once('=').unwrap())
            .collect();
        let leader = fields["leader"].parse::<i32>().unwrap();
        let child = fields["child"].parse::<i32>().unwrap();
        // SAFETY: getpgid performs a read-only process identity query.
        assert_eq!(unsafe { libc::getpgid(child) }, leader);

        let started = Instant::now();
        terminator.terminate().unwrap();
        pty.cleanup_child();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= GRACEFUL_SHUTDOWN_TIMEOUT,
            "forced shutdown skipped the grace window: {elapsed:?}"
        );
        assert!(
            elapsed <= GRACEFUL_SHUTDOWN_TIMEOUT + FORCED_SHUTDOWN_TIMEOUT + Duration::from_secs(1),
            "forced shutdown exceeded its bound: {elapsed:?}"
        );
        assert!(pty.termination.forced.load(Ordering::Acquire));
        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    #[test]
    fn responsive_process_group_should_finish_during_the_grace_window() {
        let working_directory = env::current_dir().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "sleep 30 & child=$!; echo SPACETERM_SHUTDOWN_REPORT leader=$$ child=$child; wait",
        ]);
        command.cwd(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            "responsive process group",
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let mut report = String::new();
        reader.read_line(&mut report).unwrap();
        let fields: HashMap<_, _> = report
            .split_whitespace()
            .skip_while(|field| *field != "SPACETERM_SHUTDOWN_REPORT")
            .skip(1)
            .map(|field| field.split_once('=').unwrap())
            .collect();
        let leader = fields["leader"].parse::<i32>().unwrap();
        let child = fields["child"].parse::<i32>().unwrap();

        let started = Instant::now();
        terminator.terminate().unwrap();
        pty.cleanup_child();

        assert!(started.elapsed() < GRACEFUL_SHUTDOWN_TIMEOUT);
        assert!(!pty.termination.forced.load(Ordering::Acquire));
        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    fn assert_process_disappears(process: i32) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            // SAFETY: signal zero performs only an existence/permission check.
            if unsafe { libc::kill(process, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "process {process} remained after process-group shutdown"
            );
            thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
    }

    #[test]
    fn shell_command_should_apply_compatibility_identity_and_working_directory() {
        let working_directory = Path::new("/private/tmp/workspace root");

        let command = build_shell_command("/bin/zsh", working_directory);

        assert_eq!(
            command.get_argv(),
            &vec![
                std::ffi::OsString::from("/bin/zsh"),
                std::ffi::OsString::from("-l"),
            ]
        );
        assert_eq!(
            command.get_cwd(),
            Some(&working_directory.as_os_str().to_owned())
        );
        assert_eq!(
            command.get_env("TERM"),
            Some(std::ffi::OsStr::new("xterm-256color"))
        );
        assert_eq!(
            command.get_env("COLORTERM"),
            Some(std::ffi::OsStr::new("truecolor"))
        );
        assert_eq!(
            command.get_env("TERM_PROGRAM"),
            Some(std::ffi::OsStr::new("ghostty"))
        );
        assert_eq!(
            command.get_env("SPACETERM"),
            Some(std::ffi::OsStr::new("1"))
        );
        assert_eq!(
            command.get_env("TERM_PROGRAM_VERSION"),
            Some(std::ffi::OsStr::new(env!("CARGO_PKG_VERSION")))
        );
        assert!(command.get_controlling_tty());
    }

    #[test]
    fn shell_command_uses_packaged_terminfo_only_when_the_entry_is_discoverable() {
        let resources = std::env::temp_dir().join(format!(
            "spaceterm-command-terminfo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let terminfo = resources.join("terminfo/78/xterm-spaceterm");
        std::fs::create_dir_all(terminfo.parent().unwrap()).unwrap();
        std::fs::write(&terminfo, b"compiled").unwrap();

        let command = build_shell_command_with_resources("/bin/zsh", Path::new("/tmp"), &resources);

        assert_eq!(
            command.get_env("TERM"),
            Some(std::ffi::OsStr::new(identity::TERM_NAME))
        );
        assert_eq!(
            command.get_env("TERMINFO"),
            Some(resources.join("terminfo").as_os_str())
        );
        assert_eq!(
            command.get_env("TERM_PROGRAM"),
            Some(std::ffi::OsStr::new("ghostty"))
        );
        assert_eq!(
            command.get_env("TERM_PROGRAM_VERSION"),
            Some(std::ffi::OsStr::new(identity::PROGRAM_VERSION))
        );
        std::fs::remove_dir_all(resources).unwrap();
    }

    #[test]
    fn terminal_identity_should_remove_inherited_foreign_runtime_markers() {
        let mut command = CommandBuilder::new("/bin/zsh");
        for name in FOREIGN_RUNTIME_ENVIRONMENT {
            command.env(name, "inherited");
        }
        command.env("TERMINFO", "/inherited/terminfo");

        apply_terminal_identity(&mut command, Path::new("/spaceterm-test-missing-resources"));

        assert!(
            FOREIGN_RUNTIME_ENVIRONMENT
                .iter()
                .all(|name| command.get_env(name).is_none())
        );
        assert_eq!(command.get_env("TERMINFO"), None);
    }

    #[test]
    fn shell_spawn_should_reject_a_missing_working_directory() {
        let result = spawn_user_shell(
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            Path::new("/private/tmp/spaceterm-missing-working-directory"),
        );

        let error = match result {
            Ok(_) => panic!("a missing Workspace root must not silently fall back to HOME"),
            Err(error) => error,
        };
        assert!(matches!(error, PtyError::WorkingDirectory { .. }));
    }

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
        let termination = Arc::new(ChildTermination::new(None, child.clone_killer()));
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
    fn termination_should_not_block_behind_a_concurrent_child_reap() {
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
        termination_completion
            .recv_timeout(Duration::from_millis(50))
            .unwrap()
            .unwrap();
        assert_eq!(cleanup.signal.load(Ordering::Relaxed), 1);

        try_wait_control.release();
        let (wait_result, pty) = wait_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        wait_result.unwrap();
        wait_thread.join().unwrap();
        termination_thread.join().unwrap();
        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 1, 0)
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
