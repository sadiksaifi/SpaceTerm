use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::Task;
use thiserror::Error;

use super::geometry::TerminalGeometry;
use super::metadata::{RemoteTerminalMetadataContext, TerminalLocalFileCapabilities};
use super::session::{
    LocalTerminalLaunchPlan, RemoteTerminalLaunchPlan, SessionError, StartedTerminalSession,
    TerminalLaunchPlan, TerminalSessionFactory,
};
use crate::domain::{ValidatedWorkspaceDirectory, WorkspaceDirectoryIdentity};
use crate::platform::workspace_directory::{WorkspaceDirectoryError, validate_workspace_directory};
use crate::ssh::command::PreparedSshPaneChannelCommand;

#[derive(Clone)]
enum WorkspaceTerminalLaunchContext {
    Local(LocalTerminalLaunchPlan),
    Remote(RemoteWorkspaceTerminalLaunchContext),
}

#[derive(Clone)]
struct RemoteWorkspaceTerminalLaunchContext {
    local_home: ValidatedWorkspaceDirectory,
    metadata_context: RemoteTerminalMetadataContext,
    fallback_title: String,
    channel_provider: Arc<dyn RemoteTerminalChannelProvider>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("the remote Terminal Session channel is unavailable")]
/// A content-free failure to reserve one Remote Terminal Session channel.
///
/// No hierarchy mutation may occur after this error and before a fresh revalidation succeeds.
pub(crate) struct RemoteChannelUnavailable;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// A content-free reason that a Remote child launch could not be authorized.
///
/// These errors intentionally carry no destination, path, socket, command, or authentication data.
pub(crate) enum RemoteChannelRevalidationError {
    #[error("the remote Terminal Session connection is unavailable")]
    ConnectionUnavailable,
    #[error("the remote workspace directory could not be revalidated")]
    DirectoryUnavailable,
    #[error("the remote workspace directory identity changed")]
    IdentityChanged,
}

/// Workspace-owned authority for reserving single-use Remote Terminal Session channels.
///
/// Each successful `revalidate` grants at most one immediately following `prepare`. The provider
/// binds that grant to the current Control Connection generation and pinned physical directory
/// identity. Callers must revalidate before hierarchy mutation and treat cancellation or a stale
/// grant as no mutation. Implementations must not reinterpret the remote directory as a local path.
pub(crate) trait RemoteTerminalChannelProvider: Send + Sync {
    /// Reports whether the owning Control Connection can currently accept child channels.
    fn is_ready(&self) -> bool;

    /// Revalidates the pinned remote physical identity and authorizes one subsequent preparation.
    fn revalidate(&self) -> Task<Result<(), RemoteChannelRevalidationError>>;

    /// Consumes the current revalidation grant into one prepared OpenSSH channel command.
    fn prepare(&self) -> Result<PreparedSshPaneChannelCommand, RemoteChannelUnavailable>;
}

#[cfg(test)]
impl<F> RemoteTerminalChannelProvider for F
where
    F: Fn() -> Result<PreparedSshPaneChannelCommand, RemoteChannelUnavailable> + Send + Sync,
{
    fn is_ready(&self) -> bool {
        true
    }

    fn revalidate(&self) -> Task<Result<(), RemoteChannelRevalidationError>> {
        Task::ready(Ok(()))
    }

    fn prepare(&self) -> Result<PreparedSshPaneChannelCommand, RemoteChannelUnavailable> {
        self()
    }
}

#[derive(Debug)]
/// A move-only child-launch reservation prepared before hierarchy mutation.
///
/// A Remote token owns one single-use channel command. Passing it to `start` transfers that
/// command to exactly one Pane; dropping it abandons the reservation without starting a session.
pub(crate) struct PreparedWorkspaceTerminalLaunch {
    launch_plan: TerminalLaunchPlan,
}

#[cfg(test)]
impl PreparedWorkspaceTerminalLaunch {
    fn take_remote_channel(
        &self,
    ) -> Result<crate::ssh::command::SshCommandSpec, crate::ssh::command::PreparedSshPaneChannelError>
    {
        let TerminalLaunchPlan::Remote(plan) = &self.launch_plan else {
            panic!("the test launch must be remote")
        };
        plan.take_pane_channel()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// The authority required before adding a child to a Workspace hierarchy.
///
/// Local validation returns local filesystem authority. Remote validation never returns a path;
/// its physical identity grant remains encapsulated by `RemoteTerminalChannelProvider`.
pub(crate) enum WorkspaceChildLaunchValidation {
    Local(ValidatedWorkspaceDirectory),
    Remote,
}

#[derive(Clone)]
/// Binds Terminal Session creation to one Workspace's immutable Local or Remote launch context.
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    launch_context: WorkspaceTerminalLaunchContext,
}

impl WorkspaceTerminalSessionFactory {
    /// Creates a factory whose children start from one validated local Workspace Directory.
    pub(crate) fn new_local(
        session_factory: Rc<dyn TerminalSessionFactory>,
        working_directory: ValidatedWorkspaceDirectory,
    ) -> Self {
        Self {
            session_factory,
            launch_context: WorkspaceTerminalLaunchContext::Local(LocalTerminalLaunchPlan::new(
                working_directory,
            )),
        }
    }

    /// Creates a factory whose children consume channels from one Remote Workspace owner.
    ///
    /// `local_home` is only the local OpenSSH process working directory. The metadata directory is
    /// remote startup data and must never be converted to a local `PathBuf`.
    pub(crate) fn new_remote(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home: ValidatedWorkspaceDirectory,
        metadata_context: RemoteTerminalMetadataContext,
        fallback_title: String,
        channel_provider: Arc<dyn RemoteTerminalChannelProvider>,
    ) -> Self {
        Self {
            session_factory,
            launch_context: WorkspaceTerminalLaunchContext::Remote(
                RemoteWorkspaceTerminalLaunchContext {
                    local_home,
                    metadata_context,
                    fallback_title,
                    channel_provider,
                },
            ),
        }
    }

    /// Reserves one launch before the caller mutates its Tab or Pane hierarchy.
    ///
    /// Local plans are clonable directory authority. Remote plans consume the provider's one-shot
    /// grant and fail if readiness changed after revalidation.
    pub(crate) fn prepare_child_launch(
        &self,
    ) -> Result<PreparedWorkspaceTerminalLaunch, RemoteChannelUnavailable> {
        let launch_plan = match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(plan) => TerminalLaunchPlan::Local(plan.clone()),
            WorkspaceTerminalLaunchContext::Remote(context) => {
                if !context.channel_provider.is_ready() {
                    return Err(RemoteChannelUnavailable);
                }
                let pane_channel = context.channel_provider.prepare()?;
                TerminalLaunchPlan::Remote(Box::new(RemoteTerminalLaunchPlan::new(
                    context.local_home.clone(),
                    context.metadata_context.destination().clone(),
                    context.metadata_context.initial_directory().clone(),
                    context.fallback_title.clone(),
                    pane_channel,
                )))
            }
        };
        Ok(PreparedWorkspaceTerminalLaunch { launch_plan })
    }

    /// Revalidates the pinned physical identity and grants one subsequent Remote child launch.
    ///
    /// Local child launches have no remote authority to revalidate, so callers can keep their
    /// synchronous path by branching on `None`. Dropping the task or receiving an error authorizes
    /// no hierarchy mutation; the grant must be consumed immediately after successful completion.
    pub(crate) fn revalidate_remote_child_launch(
        &self,
    ) -> Option<Task<Result<(), RemoteChannelRevalidationError>>> {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(_) => None,
            WorkspaceTerminalLaunchContext::Remote(context) => {
                Some(context.channel_provider.revalidate())
            }
        }
    }

    /// Transfers a prepared launch token into one newly started Terminal Session.
    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
    ) -> Result<StartedTerminalSession, SessionError> {
        self.session_factory
            .start(geometry, prepared_launch.launch_plan)
    }

    pub(crate) fn fallback_title(&self) -> String {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(_) => self.session_factory.fallback_title(),
            WorkspaceTerminalLaunchContext::Remote(context) => context.fallback_title.clone(),
        }
    }

    pub(crate) const fn local_file_capabilities(&self) -> TerminalLocalFileCapabilities {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(_) => TerminalLocalFileCapabilities::Enabled,
            WorkspaceTerminalLaunchContext::Remote(_) => TerminalLocalFileCapabilities::Disabled,
        }
    }

    pub(crate) const fn is_remote(&self) -> bool {
        matches!(
            &self.launch_context,
            WorkspaceTerminalLaunchContext::Remote(_)
        )
    }

    pub(crate) fn remote_channel_is_ready(&self) -> Option<bool> {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(_) => None,
            WorkspaceTerminalLaunchContext::Remote(context) => {
                Some(context.channel_provider.is_ready())
            }
        }
    }

    /// Returns local filesystem authority only for a Local launch context.
    ///
    /// Remote Workspace directories are intentionally unavailable through this API.
    pub(crate) fn local_working_directory(&self) -> Option<&std::path::Path> {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(plan) => Some(plan.working_directory().path()),
            WorkspaceTerminalLaunchContext::Remote(_) => None,
        }
    }

    /// Revalidates Local directory identity without applying local validation to Remote values.
    ///
    /// A Remote result carries no path authority; callers must separately use the provider's
    /// asynchronous physical-identity grant before reserving a channel.
    pub(crate) fn validate_child_launch(
        &self,
    ) -> Result<WorkspaceChildLaunchValidation, WorkspaceDirectoryError> {
        let WorkspaceTerminalLaunchContext::Local(plan) = &self.launch_context else {
            return Ok(WorkspaceChildLaunchValidation::Remote);
        };
        #[cfg(test)]
        if plan.working_directory().identity().is_synthetic() {
            return Ok(WorkspaceChildLaunchValidation::Local(
                plan.working_directory().clone(),
            ));
        }
        let directory = validate_workspace_directory(plan.working_directory().path())?;
        if plan.working_directory().identity() != directory.identity() {
            return Err(WorkspaceDirectoryError::IdentityChanged);
        }
        Ok(WorkspaceChildLaunchValidation::Local(directory))
    }

    pub(crate) fn set_working_directory(
        &mut self,
        workspace_root: PathBuf,
        identity: WorkspaceDirectoryIdentity,
    ) {
        let WorkspaceTerminalLaunchContext::Local(plan) = &mut self.launch_context else {
            return;
        };
        *plan = LocalTerminalLaunchPlan::new(ValidatedWorkspaceDirectory::new(
            workspace_root,
            identity,
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };
    use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

    struct TestRemoteChannelProvider {
        ready: AtomicBool,
        preparations: AtomicUsize,
        results: Mutex<VecDeque<Result<PreparedSshPaneChannelCommand, RemoteChannelUnavailable>>>,
    }

    impl TestRemoteChannelProvider {
        fn new(
            ready: bool,
            results: impl IntoIterator<
                Item = Result<PreparedSshPaneChannelCommand, RemoteChannelUnavailable>,
            >,
        ) -> Self {
            Self {
                ready: AtomicBool::new(ready),
                preparations: AtomicUsize::new(0),
                results: Mutex::new(results.into_iter().collect()),
            }
        }
    }

    impl RemoteTerminalChannelProvider for TestRemoteChannelProvider {
        fn is_ready(&self) -> bool {
            self.ready.load(Ordering::Acquire)
        }

        fn revalidate(&self) -> Task<Result<(), RemoteChannelRevalidationError>> {
            if self.is_ready() {
                Task::ready(Ok(()))
            } else {
                Task::ready(Err(RemoteChannelRevalidationError::ConnectionUnavailable))
            }
        }

        fn prepare(&self) -> Result<PreparedSshPaneChannelCommand, RemoteChannelUnavailable> {
            self.preparations.fetch_add(1, Ordering::AcqRel);
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(RemoteChannelUnavailable))
        }
    }

    fn prepared_channel(
        destination: &SshDestination,
        command: &str,
    ) -> PreparedSshPaneChannelCommand {
        SshCommandContext::new(
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            destination.clone(),
            PathBuf::from("/private/runtime/spaceterm/master.sock"),
        )
        .unwrap()
        .prepare_pane_channel(ValidatedRemoteShellCommand::new(command.to_owned()).unwrap())
    }

    fn remote_factory(
        records: TestTerminalSessionRecords,
        provider: Arc<dyn RemoteTerminalChannelProvider>,
    ) -> WorkspaceTerminalSessionFactory {
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let remote_directory = RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap();
        WorkspaceTerminalSessionFactory::new_remote(
            Rc::new(TestTerminalSessionFactory::new(records)),
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/local/home/used-only-as-process-cwd",
            )),
            RemoteTerminalMetadataContext::new(destination, remote_directory),
            "project on remote".to_owned(),
            provider,
        )
    }

    #[test]
    fn local_factory_should_forward_the_validated_launch_plan() {
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let directory = ValidatedWorkspaceDirectory::new(
            PathBuf::from("/typed-local-workspace"),
            WorkspaceDirectoryIdentity::new(7, 11),
        );
        let factory =
            WorkspaceTerminalSessionFactory::new_local(session_factory, directory.clone());
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(8.0, 20.0),
            BackingScale::ONE,
        );

        let launch = factory.prepare_child_launch().unwrap();
        let _started = factory.start(geometry, launch).unwrap();

        let starts = records.starts();
        assert!(matches!(
            starts[0].launch_plan(),
            TerminalLaunchPlan::Local(_)
        ));
        assert_eq!(
            starts[0]
                .local_working_directory()
                .expect("the local factory must record a local plan"),
            &directory
        );
    }

    #[test]
    fn remote_factory_should_preserve_context_and_prepare_one_channel_per_child() {
        let records = TestTerminalSessionRecords::default();
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let remote_directory = RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap();
        let provider = Arc::new(TestRemoteChannelProvider::new(
            true,
            [
                Ok(prepared_channel(&destination, "exec /bin/zsh -l")),
                Ok(prepared_channel(&destination, "exec /bin/zsh -l")),
            ],
        ));
        let factory = remote_factory(records.clone(), provider.clone());
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(8.0, 20.0),
            BackingScale::ONE,
        );

        let first = factory.prepare_child_launch().unwrap();
        let second = factory.prepare_child_launch().unwrap();
        let _first = factory.start(geometry, first).unwrap();
        let _second = factory.start(geometry, second).unwrap();

        assert_eq!(provider.preparations.load(Ordering::Acquire), 2);
        assert_eq!(factory.local_working_directory(), None);
        assert_eq!(
            factory.validate_child_launch().unwrap(),
            WorkspaceChildLaunchValidation::Remote
        );
        assert_eq!(
            factory.local_file_capabilities(),
            TerminalLocalFileCapabilities::Disabled
        );
        assert_eq!(factory.fallback_title(), "project on remote");
        assert!(records.starts().iter().all(|start| {
            start.remote_launch_plan().is_some_and(|plan| {
                plan.destination() == &destination && plan.remote_directory() == &remote_directory
            })
        }));
    }

    #[test]
    fn remote_factory_should_reject_launch_when_provider_is_not_ready() {
        let records = TestTerminalSessionRecords::default();
        let provider = Arc::new(TestRemoteChannelProvider::new(false, []));
        let factory = remote_factory(records.clone(), provider.clone());

        let error = factory.prepare_child_launch().unwrap_err();

        assert_eq!(error, RemoteChannelUnavailable);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 0);
        assert!(records.starts().is_empty());
    }

    #[test]
    fn remote_factory_should_propagate_master_death_race_during_reservation() {
        let records = TestTerminalSessionRecords::default();
        let provider = Arc::new(TestRemoteChannelProvider::new(
            true,
            [Err(RemoteChannelUnavailable)],
        ));
        let factory = remote_factory(records.clone(), provider.clone());

        let error = factory.prepare_child_launch().unwrap_err();

        assert_eq!(error, RemoteChannelUnavailable);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 1);
        assert!(records.starts().is_empty());
    }

    #[test]
    fn prepared_remote_launches_should_be_distinct_and_single_use() {
        let records = TestTerminalSessionRecords::default();
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(TestRemoteChannelProvider::new(
            true,
            [
                Ok(prepared_channel(&destination, "exec first")),
                Ok(prepared_channel(&destination, "exec second")),
            ],
        ));
        let factory = remote_factory(records, provider);
        let first = factory.prepare_child_launch().unwrap();
        let second = factory.prepare_child_launch().unwrap();

        let first_command = first.take_remote_channel().unwrap();
        let second_command = second.take_remote_channel().unwrap();
        let error = match first.take_remote_channel() {
            Ok(_) => panic!("a prepared remote launch must be single-use"),
            Err(error) => error,
        };

        assert_ne!(first_command.arguments(), second_command.arguments());
        assert!(matches!(
            error,
            crate::ssh::command::PreparedSshPaneChannelError::AlreadyConsumed
        ));
    }
}
