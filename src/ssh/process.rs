use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use gpui::BackgroundExecutor;
use thiserror::Error;

use super::command::SshCommandSpec;
use crate::platform::macos_askpass_transport::AskPassEnvironment;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessExit {
    success: bool,
    code: Option<i32>,
}

impl ProcessExit {
    pub(crate) const fn successful() -> Self {
        Self {
            success: true,
            code: Some(0),
        }
    }

    pub(crate) const fn unsuccessful(code: Option<i32>) -> Self {
        Self {
            success: false,
            code,
        }
    }

    pub(crate) const fn is_success(self) -> bool {
        self.success
    }

    pub(crate) const fn code(self) -> Option<i32> {
        self.code
    }
}

impl From<ExitStatus> for ProcessExit {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
        }
    }
}

pub(crate) trait SshProcessBackend: Send + Sync + 'static {
    type Child: Send + 'static;

    fn spawn(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<Self::Child>> + Send;

    fn run(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<ProcessExit>> + Send;

    fn try_wait(&self, child: &mut Self::Child) -> io::Result<Option<ProcessExit>>;

    fn terminate_and_reap(&self, child: &mut Self::Child) -> io::Result<()>;

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

pub(crate) trait SshAuthenticationOverlay: Send + Sync + 'static {
    fn entries(&self) -> Vec<(&'static str, &OsStr)>;
}

impl SshAuthenticationOverlay for AskPassEnvironment {
    fn entries(&self) -> Vec<(&'static str, &OsStr)> {
        AskPassEnvironment::entries(self).collect()
    }
}

#[derive(Debug, Error)]
pub(crate) enum SshProcessEnvironmentError {
    #[error("the captured local HOME must be an absolute control-free path")]
    UnsafeHome,
    #[error("the captured SSH agent socket must be an absolute control-free path")]
    UnsafeAgentSocket,
}

#[derive(Clone)]
pub(crate) struct SshProcessEnvironment {
    home: PathBuf,
    authentication: Arc<dyn SshAuthenticationOverlay>,
    agent_socket: Option<OsString>,
}

impl SshProcessEnvironment {
    pub(crate) fn new(
        home: PathBuf,
        authentication: Arc<dyn SshAuthenticationOverlay>,
        agent_socket: Option<OsString>,
    ) -> Result<Self, SshProcessEnvironmentError> {
        if !safe_absolute_path(&home) {
            return Err(SshProcessEnvironmentError::UnsafeHome);
        }
        if agent_socket.as_ref().is_some_and(|socket| {
            socket.as_bytes().is_empty() || !safe_absolute_path(Path::new(socket))
        }) {
            return Err(SshProcessEnvironmentError::UnsafeAgentSocket);
        }
        Ok(Self {
            home,
            authentication,
            agent_socket,
        })
    }

    fn apply(&self, command: &mut Command) {
        command
            .env_clear()
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin");
        if let Some(agent_socket) = &self.agent_socket {
            command.env("SSH_AUTH_SOCK", agent_socket);
        }
        for (name, value) in self.authentication.entries() {
            command.env(name, value);
        }
    }
}

#[derive(Clone)]
pub(crate) struct NativeSshProcessBackend {
    executor: BackgroundExecutor,
    environment: SshProcessEnvironment,
}

impl NativeSshProcessBackend {
    pub(crate) const fn new(
        executor: BackgroundExecutor,
        environment: SshProcessEnvironment,
    ) -> Self {
        Self {
            executor,
            environment,
        }
    }
}

pub(crate) struct NativeSshChild {
    child: Option<Child>,
    process_group: libc::pid_t,
}

impl SshProcessBackend for NativeSshProcessBackend {
    type Child = NativeSshChild;

    fn spawn(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<Self::Child>> + Send {
        let command = command(spec, &self.environment);
        async move { spawn_owned_command_off_thread(command).await }
    }

    fn run(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<ProcessExit>> + Send {
        let backend = self.clone();
        async move {
            let mut child = backend.spawn(spec).await?;
            loop {
                if let Some(exit) = backend.try_wait(&mut child)? {
                    return Ok(exit);
                }
                backend.delay(Duration::from_millis(10)).await;
            }
        }
    }

    fn try_wait(&self, child: &mut Self::Child) -> io::Result<Option<ProcessExit>> {
        let Some(process) = child.child.as_mut() else {
            return Ok(Some(ProcessExit::unsuccessful(None)));
        };
        process
            .try_wait()
            .map(|status| status.map(ProcessExit::from))
    }

    fn terminate_and_reap(&self, child: &mut Self::Child) -> io::Result<()> {
        let Some(process) = child.child.as_mut() else {
            return Ok(());
        };
        if process.try_wait()?.is_some() {
            child.child.take();
            return Ok(());
        }
        signal_process_group(child.process_group, libc::SIGKILL)?;
        process.wait()?;
        child.child.take();
        Ok(())
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send {
        let executor = self.executor.clone();
        async move { executor.timer(duration).await }
    }
}

fn command(spec: SshCommandSpec, environment: &SshProcessEnvironment) -> Command {
    let mut command = Command::new(spec.executable());
    command
        .args(spec.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    environment.apply(&mut command);
    command
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute() && !path.as_os_str().as_bytes().iter().any(u8::is_ascii_control)
}

fn spawn_owned_command(mut command: Command) -> io::Result<NativeSshChild> {
    command.process_group(0);
    let child = command.spawn()?;
    let process_group = child.id().try_into().map_err(|_| {
        io::Error::other("SSH child process identifier did not fit the platform process type")
    })?;
    Ok(NativeSshChild {
        child: Some(child),
        process_group,
    })
}

async fn spawn_owned_command_off_thread(command: Command) -> io::Result<NativeSshChild> {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("spaceterm-ssh-spawn".to_owned())
        .spawn(move || {
            let result = spawn_owned_command(command);
            let _ = sender.send_blocking(result);
        })?;
    receiver.recv().await.map_err(|_| {
        io::Error::other("SSH process launch worker ended before returning child ownership")
    })?
}

fn signal_process_group(process_group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: the process group is the positive PID returned by a child we launched with a new
    // process group; kill does not dereference pointers.
    let result = unsafe { libc::kill(-process_group, signal) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

impl Drop for NativeSshChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = signal_process_group(self.process_group, libc::SIGKILL);
        let _ = std::thread::Builder::new()
            .name("spaceterm-ssh-reaper".to_owned())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::Arc;

    use super::*;

    struct TestAuthenticationOverlay;

    impl SshAuthenticationOverlay for TestAuthenticationOverlay {
        fn entries(&self) -> Vec<(&'static str, &OsStr)> {
            vec![("SPACETERM_TEST_AUTH", OsStr::new("available"))]
        }
    }

    #[test]
    fn launch_environment_should_clear_unknown_variables_and_use_captured_home_as_cwd() {
        let home = PathBuf::from(format!(
            "/private/tmp/spaceterm-process-environment-{}",
            std::process::id()
        ));
        fs::create_dir_all(&home).unwrap();
        let environment =
            SshProcessEnvironment::new(home.clone(), Arc::new(TestAuthenticationOverlay), None)
                .unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "pwd; /usr/bin/env"])
            .env("SPACETERM_UNKNOWN", "secret")
            .stdout(Stdio::piped());

        environment.apply(&mut command);
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        assert!(
            stdout.starts_with(&format!("{}\n", home.display()))
                && stdout.contains("HOME=")
                && stdout.contains("SPACETERM_TEST_AUTH=available")
                && !stdout.contains("SPACETERM_UNKNOWN")
        );
        fs::remove_dir(home).unwrap();
    }

    #[test]
    fn launch_environment_should_reject_a_relative_home() {
        let error = SshProcessEnvironment::new(
            PathBuf::from("relative-home"),
            Arc::new(TestAuthenticationOverlay),
            None,
        )
        .err();

        assert!(matches!(
            error,
            Some(SshProcessEnvironmentError::UnsafeHome)
        ));
    }

    #[test]
    fn native_child_drop_should_terminate_its_private_process_group_descendants() {
        let pid_file = PathBuf::from(format!(
            "/private/tmp/spaceterm-process-descendant-{}.pid",
            std::process::id()
        ));
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);

        let child = spawn_owned_command(command).unwrap();
        for _ in 0..100 {
            if pid_file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let descendant: i32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        drop(child);

        let terminated = (0..100).any(|_| {
            // SAFETY: signal zero performs a process-existence check and dereferences no pointers.
            let missing = unsafe { libc::kill(descendant, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if !missing {
                std::thread::sleep(Duration::from_millis(10));
            }
            missing
        });
        let _ = fs::remove_file(pid_file);

        assert!(terminated);
    }
}
