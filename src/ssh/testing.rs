//! Real Control Connection preparation with deterministic external process and filesystem adapters.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::cancellation::SshCancellationToken;
use super::command::{
    OpenSshExecutable, PreparedSshPaneChannelCommand, SshCommandSpec, ValidatedRemoteShellCommand,
};
use super::control_connection::{ControlConnectionTiming, OpenSshControlConnection};
use super::process::{
    ProcessCleanupCallback, ProcessExit, ProcessRunError, ProcessSignal, SshProcessBackend,
    SshProcessCleanup, SshProcessEnvironment, SshProcessMechanismError,
};
use super::startup_environment::StartupSshEnvironment;
use crate::domain::SshDestination;
use crate::platform::app_directories::AppDirectoryEnvironment;
use crate::platform::testing::{RecordingControlSocketProbe, RecordingFilesystem};

pub(crate) struct SshConnectionFixture {
    connection: OpenSshControlConnection<FixtureProcessBackend>,
}

impl SshConnectionFixture {
    pub(crate) fn new(destination: SshDestination) -> Self {
        Self::with_environment(
            destination,
            std::env::temp_dir(),
            OpenSshExecutable::for_test(),
            &StartupSshEnvironment::default(),
        )
    }

    pub(crate) fn with_environment(
        destination: SshDestination,
        home: PathBuf,
        executable: OpenSshExecutable,
        startup: &StartupSshEnvironment,
    ) -> Self {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let environment = AppDirectoryEnvironment {
            home: Some("/fixture/ssh".into()),
            xdg_runtime_dir: Some("/fixture/ssh/runtime".into()),
            ..AppDirectoryEnvironment::default()
        };
        let paths = crate::platform::testing::resolve_app_paths(
            &environment,
            Some("/fixture/ssh/temporary".into()),
            103,
            filesystem.clone(),
        )
        .unwrap();
        let backend = Arc::new(FixtureProcessBackend {
            filesystem: filesystem.clone(),
            environment: SshProcessEnvironment::new_without_authentication_from_startup(
                home, startup,
            )
            .unwrap(),
        });
        let connection = pollster::block_on(OpenSshControlConnection::connect(
            &paths,
            executable,
            &RecordingControlSocketProbe(filesystem),
            destination,
            backend,
            &SshCancellationToken::default(),
            ControlConnectionTiming::default(),
        ))
        .unwrap();
        Self { connection }
    }

    pub(crate) fn prepare_pane_channel(
        &self,
        command: ValidatedRemoteShellCommand,
    ) -> PreparedSshPaneChannelCommand {
        self.connection.prepare_pane_channel(command).unwrap()
    }
}

struct FixtureProcessBackend {
    filesystem: Arc<RecordingFilesystem>,
    environment: SshProcessEnvironment,
}

impl SshProcessBackend for FixtureProcessBackend {
    type Child = ();
    fn environment(&self) -> &SshProcessEnvironment {
        &self.environment
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    async fn spawn(&self, spec: SshCommandSpec) -> Result<Self::Child, SshProcessMechanismError> {
        let socket = spec
            .arguments()
            .windows(2)
            .find(|pair| pair[0] == "-S")
            .unwrap();
        self.filesystem
            .create_socket(std::path::Path::new(&socket[1]));
        Ok(())
    }
    async fn run(
        &self,
        _spec: SshCommandSpec,
        _cancellation: SshCancellationToken,
        _deadline: Instant,
    ) -> Result<ProcessExit, ProcessRunError> {
        Ok(ProcessExit::successful())
    }
    fn try_wait(
        &self,
        _child: &mut Self::Child,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        Ok(None)
    }
    fn signal_process_group(
        &self,
        _child: &mut Self::Child,
        _signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        Ok(())
    }
    fn begin_cleanup(
        &self,
        _child: Self::Child,
        after: Option<ProcessCleanupCallback>,
    ) -> SshProcessCleanup {
        if let Some(after) = after {
            after();
        }
        SshProcessCleanup::completed()
    }
    async fn delay(&self, _duration: Duration) {}
}
