use std::future::Future;
use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use gpui::BackgroundExecutor;

use super::command::SshCommandSpec;

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

#[derive(Clone)]
pub(crate) struct NativeSshProcessBackend {
    executor: BackgroundExecutor,
}

impl NativeSshProcessBackend {
    pub(crate) const fn new(executor: BackgroundExecutor) -> Self {
        Self { executor }
    }
}

pub(crate) struct NativeSshChild(Child);

impl SshProcessBackend for NativeSshProcessBackend {
    type Child = NativeSshChild;

    fn spawn(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<Self::Child>> + Send {
        async move { command(spec).spawn().map(NativeSshChild) }
    }

    fn run(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<ProcessExit>> + Send {
        async move { command(spec).status().map(ProcessExit::from) }
    }

    fn try_wait(&self, child: &mut Self::Child) -> io::Result<Option<ProcessExit>> {
        child
            .0
            .try_wait()
            .map(|status| status.map(ProcessExit::from))
    }

    fn terminate_and_reap(&self, child: &mut Self::Child) -> io::Result<()> {
        if child.0.try_wait()?.is_some() {
            return Ok(());
        }
        let kill_error = child
            .0
            .kill()
            .err()
            .filter(|error| error.kind() != io::ErrorKind::InvalidInput);
        let wait_result = child.0.wait();
        if let Some(error) = kill_error {
            return Err(error);
        }
        wait_result.map(|_| ())
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send {
        let executor = self.executor.clone();
        async move { executor.timer(duration).await }
    }
}

fn command(spec: SshCommandSpec) -> Command {
    let mut command = Command::new(spec.executable());
    command
        .args(spec.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
