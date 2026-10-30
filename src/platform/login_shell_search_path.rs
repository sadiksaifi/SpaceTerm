//! Learns the PATH a new terminal shell builds by asking the account's login shell for it.
//!
//! The desktop's own PATH is not that PATH: an app opened from the Dock or a desktop launcher
//! inherits a minimal environment, and profiles change PATH only in shells. The shell starts as
//! an interactive login shell in the home directory, as a terminal starts it, with no terminal
//! and no input. It runs in its own process group, which is killed at the deadline.
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::command_line_tool::ShellSearchPath;

/// How long the shell's startup files may take.
const DEADLINE: Duration = Duration::from_secs(5);
/// Startup files may print; only this much output is kept.
const MAXIMUM_OUTPUT: usize = 64 * 1024;
/// The PATH bounds match the tool discovery in `linux_repository_tools.rs`.
const MAXIMUM_PATH_BYTES: usize = 16 * 1024;
const MAXIMUM_PATH_ENTRIES: usize = 128;
/// An external command, so every shell's own syntax runs it the same way.
const PROBE: &str = "printenv PATH";

pub(crate) struct LoginShellSearchPath {
    shell: PathBuf,
    home: PathBuf,
}

impl LoginShellSearchPath {
    pub(crate) fn new(shell: PathBuf, home: PathBuf) -> Self {
        Self { shell, home }
    }
}

impl ShellSearchPath for LoginShellSearchPath {
    fn directories(&self) -> Option<Vec<PathBuf>> {
        let output = run(
            Command::new(&self.shell)
                .args(["-l", "-i", "-c", PROBE])
                .current_dir(&self.home),
            DEADLINE,
        )?;
        let directories = parse(&output)?;
        Some(
            directories
                .into_iter()
                .map(|directory| std::fs::canonicalize(&directory).unwrap_or(directory))
                .collect(),
        )
    }
}

/// The output of a command that exited successfully before `deadline`.
fn run(command: &mut Command, deadline: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + deadline;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let group = child.id() as libc::pid_t;
    let mut stdout = child.stdout.take()?;
    let mut output = Vec::new();
    let mut ended = false;
    // A program the startup files leave running may hold the pipe open, so reading ends when the
    // shell exits rather than at end of file.
    let status = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || output.len() > MAXIMUM_OUTPUT {
            return abandon(group, &mut child);
        }
        let Ok(exited) = child.try_wait() else {
            return abandon(group, &mut child);
        };
        if let Some(status) = exited {
            // Collect what the shell wrote before it exited.
            while !ended {
                match read_ready(&mut stdout, Duration::ZERO, &mut output) {
                    Some(Ready::Read) => {}
                    Some(Ready::Idle | Ready::Ended) => ended = true,
                    None => return None,
                }
            }
            break status;
        }
        if ended {
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
            continue;
        }
        match read_ready(
            &mut stdout,
            remaining.min(Duration::from_millis(50)),
            &mut output,
        ) {
            Some(ready) => ended = ready == Ready::Ended,
            None => return abandon(group, &mut child),
        }
    };
    status.success().then_some(output)
}

/// Ends the probe's process group, so nothing it started outlives the deadline.
fn abandon(group: libc::pid_t, child: &mut std::process::Child) -> Option<Vec<u8>> {
    // SAFETY: kill signals only the probe's own process group, whose leader is not yet reaped.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ready {
    Read,
    Idle,
    /// Every writer closed the pipe.
    Ended,
}

/// Appends what `pipe` has within `wait`. `None` on a read failure.
fn read_ready(
    pipe: &mut std::process::ChildStdout,
    wait: Duration,
    output: &mut Vec<u8>,
) -> Option<Ready> {
    let mut poll = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll reads and writes only the one pollfd it is given.
    if unsafe { libc::poll(&mut poll, 1, wait.as_millis() as libc::c_int) } <= 0 {
        return Some(Ready::Idle);
    }
    let mut buffer = [0_u8; 4096];
    match pipe.read(&mut buffer) {
        Ok(0) => Some(Ready::Ended),
        Ok(read) => {
            output.extend_from_slice(&buffer[..read]);
            Some(Ready::Read)
        }
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Some(Ready::Read),
        Err(_) => None,
    }
}

/// The absolute entries of the PATH the probe printed last. Startup files may print before it.
fn parse(output: &[u8]) -> Option<Vec<PathBuf>> {
    use std::os::unix::ffi::OsStrExt;
    let line = output
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .rfind(|line| !line.is_empty())?;
    if line.len() > MAXIMUM_PATH_BYTES || line.iter().any(u8::is_ascii_control) {
        return None;
    }
    let directories: Vec<PathBuf> = std::env::split_paths(std::ffi::OsStr::from_bytes(line))
        .take(MAXIMUM_PATH_ENTRIES)
        .filter(|directory| directory.is_absolute())
        .collect();
    (!directories.is_empty()).then_some(directories)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_line_is_the_path_and_earlier_output_is_ignored() {
        assert_eq!(
            parse(b"Welcome back!\n/usr/bin:relative:/home/someone/.local/bin\n"),
            Some(vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/home/someone/.local/bin"),
            ])
        );
        assert_eq!(
            parse(b"/usr/bin\r\n\n"),
            Some(vec![PathBuf::from("/usr/bin")])
        );
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"$PATH\n"), None);
        assert_eq!(parse(b"/usr/bin\x1b[0m\n"), None);
    }

    #[test]
    fn the_probe_answers_only_when_the_shell_exits_successfully_in_time() {
        let shell =
            |script: &str, deadline| run(Command::new("/bin/sh").args(["-c", script]), deadline);
        assert_eq!(
            shell("echo noise; echo /usr/bin:/bin", DEADLINE),
            Some(b"noise\n/usr/bin:/bin\n".to_vec())
        );
        assert_eq!(shell("echo /usr/bin; exit 3", DEADLINE), None);
        let started = Instant::now();
        assert_eq!(shell("sleep 30", Duration::from_millis(200)), None);
        // A program left running with the pipe open doesn't hold the answer back.
        assert_eq!(
            shell("sleep 3 & echo /usr/bin", DEADLINE),
            Some(b"/usr/bin\n".to_vec())
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
