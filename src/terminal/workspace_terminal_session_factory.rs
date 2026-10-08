#[cfg(test)]
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::Task;
use thiserror::Error;

use super::geometry::TerminalGeometry;
use super::metadata::CurrentDirectory;
use super::metadata::{RemoteTerminalMetadataContext, TerminalLocalFileCapabilities};
use super::session::{
    LocalTerminalLaunchPlan, RemoteTerminalLaunchPlan, StartedTerminalSession, TerminalLaunchPlan,
    TerminalSessionError, TerminalSessionFactory,
};
use crate::domain::{
    PinnedDirectory, RemoteDirectory, RemoteDirectoryIdentity, ValidatedLocalDirectory,
};
use crate::platform::local_filesystem::{
    LocalFilesystemAuthority, LocalFilesystemError as LocalDirectoryError,
};
use crate::ssh::command::PreparedSshTerminalSessionChannelCommand;

#[derive(Clone)]
enum WorkspaceTerminalLaunchContext {
    Local(LocalTerminalLaunchPlan),
    Remote(RemoteWorkspaceTerminalLaunchContext),
}

#[derive(Clone)]
struct RemoteWorkspaceTerminalLaunchContext {
    local_home: ValidatedLocalDirectory,
    metadata_context: RemoteTerminalMetadataContext,
    fallback_title: String,
    channel_provider: Arc<dyn TerminalSessionChannelProvider>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("the Terminal Session Channel is unavailable")]
/// A content-free failure to reserve one Terminal Session Channel.
///
/// No hierarchy mutation may occur after this error and before a fresh revalidation succeeds.
pub(crate) struct TerminalSessionChannelUnavailable;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// A content-free reason that a Remote child launch could not be authorized.
///
/// These errors intentionally carry no destination, path, socket, command, or authentication data.
pub(crate) enum TerminalSessionChannelRevalidationError {
    #[error("the Control Connection is unavailable")]
    ConnectionUnavailable,
    #[error("the remote starting directory could not be revalidated")]
    DirectoryUnavailable,
    #[error("the remote starting directory identity changed")]
    IdentityChanged,
}

/// Workspace-owned authority for reserving single-use Terminal Session Channels.
///
/// Each successful `revalidate` grants at most one immediately following `prepare`. The provider
/// binds that grant to the current Control Connection generation and selected physical directory
/// identity. Callers must revalidate before hierarchy mutation and treat cancellation or a stale
/// grant as no mutation. Implementations must not reinterpret the remote directory as a local path.
pub(crate) trait TerminalSessionChannelProvider: Send + Sync {
    /// Reports whether the owning Control Connection can accept Terminal Session Channels now.
    fn is_ready(&self) -> bool;

    /// Revalidates the selected remote directory and authorizes one subsequent preparation.
    fn revalidate(
        &self,
        directory: RemoteDirectory,
        expected_identity: Option<RemoteDirectoryIdentity>,
    ) -> Task<Result<(), TerminalSessionChannelRevalidationError>>;

    /// Consumes the current revalidation grant into one prepared OpenSSH channel command.
    fn prepare(
        &self,
        directory: &RemoteDirectory,
    ) -> Result<PreparedSshTerminalSessionChannelCommand, TerminalSessionChannelUnavailable>;
}

#[cfg(test)]
impl<F> TerminalSessionChannelProvider for F
where
    F: Fn() -> Result<PreparedSshTerminalSessionChannelCommand, TerminalSessionChannelUnavailable>
        + Send
        + Sync,
{
    fn is_ready(&self) -> bool {
        true
    }

    fn revalidate(
        &self,
        _directory: RemoteDirectory,
        _expected_identity: Option<RemoteDirectoryIdentity>,
    ) -> Task<Result<(), TerminalSessionChannelRevalidationError>> {
        Task::ready(Ok(()))
    }

    fn prepare(
        &self,
        _directory: &RemoteDirectory,
    ) -> Result<PreparedSshTerminalSessionChannelCommand, TerminalSessionChannelUnavailable> {
        self()
    }
}

#[derive(Debug)]
/// A move-only child-launch reservation prepared before hierarchy mutation.
///
/// A Remote token owns one Terminal Session Channel command. `start` transfers that command to
/// one Pane; dropping the token abandons the reservation without starting a Terminal Session.
pub(crate) struct PreparedWorkspaceTerminalLaunch {
    launch_plan: TerminalLaunchPlan,
}

impl PreparedWorkspaceTerminalLaunch {
    /// Initial successor facts, independent of the screen retained during a Remote restart.
    pub(crate) fn initial_remote_metadata(
        &self,
    ) -> Option<Arc<super::metadata::TerminalMetadataSnapshot>> {
        let TerminalLaunchPlan::Remote(plan) = &self.launch_plan else {
            return None;
        };
        Some(
            super::metadata::MetadataTracker::new_with_context(
                super::metadata::TerminalMetadataContext::Remote(plan.metadata_context().clone()),
                plan.fallback_title(),
                std::time::Instant::now(),
            )
            .snapshot(),
        )
    }

    pub(crate) fn starting_directory(&self) -> CurrentDirectory {
        match &self.launch_plan {
            TerminalLaunchPlan::Local(plan) => {
                CurrentDirectory::Local(plan.working_directory().path().to_owned())
            }
            TerminalLaunchPlan::Remote(plan) => {
                CurrentDirectory::Remote(plan.remote_directory().clone())
            }
        }
    }
}

#[cfg(test)]
impl PreparedWorkspaceTerminalLaunch {
    fn take_terminal_session_channel(
        &self,
    ) -> Result<
        crate::ssh::command::SshCommandSpec,
        crate::ssh::command::PreparedSshTerminalSessionChannelError,
    > {
        let TerminalLaunchPlan::Remote(plan) = &self.launch_plan else {
            panic!("the test launch must be remote")
        };
        plan.take_terminal_session_channel()
    }
}

#[derive(Clone)]
/// Owns a Workspace's home and pin policy and captures individual Terminal Session launches.
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    local_filesystem: Option<LocalFilesystemAuthority>,
    launch_context: WorkspaceTerminalLaunchContext,
    pinned_directory: Option<PinnedDirectory>,
    /// The local Worktree root a Worktree's Tabs start inside, as git reports it.
    worktree_root: Option<std::path::PathBuf>,
    expected_remote_identity: Option<RemoteDirectoryIdentity>,
}

impl WorkspaceTerminalSessionFactory {
    #[cfg(test)]
    pub(crate) fn new_local(
        session_factory: Rc<dyn TerminalSessionFactory>,
        home_directory: ValidatedLocalDirectory,
    ) -> Self {
        Self::new_local_with_authority(
            session_factory,
            home_directory,
            LocalFilesystemAuthority::testing(),
        )
    }

    /// Creates a factory whose children start from one validated local home directory.
    pub(crate) fn new_local_with_authority(
        session_factory: Rc<dyn TerminalSessionFactory>,
        home_directory: ValidatedLocalDirectory,
        local_filesystem: LocalFilesystemAuthority,
    ) -> Self {
        Self {
            session_factory,
            pinned_directory: None,
            worktree_root: None,
            expected_remote_identity: None,
            local_filesystem: Some(local_filesystem),
            launch_context: WorkspaceTerminalLaunchContext::Local(LocalTerminalLaunchPlan::new(
                home_directory,
            )),
        }
    }

    /// Creates a factory whose children consume channels from one Remote Workspace owner.
    ///
    /// `local_home` is only the local OpenSSH process working directory. The metadata directory is
    /// remote startup data and must never be converted to a local `PathBuf`.
    pub(crate) fn new_remote(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home: ValidatedLocalDirectory,
        metadata_context: RemoteTerminalMetadataContext,
        initial_directory_identity: RemoteDirectoryIdentity,
        fallback_title: String,
        channel_provider: Arc<dyn TerminalSessionChannelProvider>,
    ) -> Self {
        Self {
            session_factory,
            pinned_directory: None,
            worktree_root: None,
            expected_remote_identity: Some(initial_directory_identity),
            local_filesystem: None,
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
    ) -> Result<PreparedWorkspaceTerminalLaunch, TerminalSessionChannelUnavailable> {
        let launch_plan = match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(plan) => TerminalLaunchPlan::Local(plan.clone()),
            WorkspaceTerminalLaunchContext::Remote(context) => {
                if !context.channel_provider.is_ready() {
                    return Err(TerminalSessionChannelUnavailable);
                }
                let terminal_session_channel = context
                    .channel_provider
                    .prepare(context.metadata_context.initial_directory())?;
                TerminalLaunchPlan::Remote(Box::new(RemoteTerminalLaunchPlan::new(
                    context.local_home.clone(),
                    context.metadata_context.clone(),
                    context.fallback_title.clone(),
                    terminal_session_channel,
                )))
            }
        };
        Ok(PreparedWorkspaceTerminalLaunch { launch_plan })
    }

    /// Revalidates the selected remote directory and grants one subsequent Remote child launch.
    ///
    /// Local child launches have no remote authority to revalidate, so callers can keep their
    /// synchronous path by branching on `None`. Dropping the task or receiving an error authorizes
    /// no hierarchy mutation; the grant must be consumed immediately after successful completion.
    pub(crate) fn revalidate_remote_child_launch(
        &self,
    ) -> Option<Task<Result<(), TerminalSessionChannelRevalidationError>>> {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(_) => None,
            WorkspaceTerminalLaunchContext::Remote(context) => {
                Some(context.channel_provider.revalidate(
                    context.metadata_context.initial_directory().clone(),
                    self.expected_remote_identity.clone(),
                ))
            }
        }
    }

    /// Transfers a prepared launch token into one newly started Terminal Session.
    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        initial_appearance: super::TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        self.session_factory
            .start(geometry, prepared_launch.launch_plan, initial_appearance)
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

    pub(crate) fn terminal_session_channel_is_ready(&self) -> Option<bool> {
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
    #[cfg(test)]
    pub(crate) fn local_working_directory(&self) -> Option<&std::path::Path> {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(plan) => Some(plan.working_directory().path()),
            WorkspaceTerminalLaunchContext::Remote(_) => None,
        }
    }

    /// Revalidates retained local filesystem authority without interpreting Remote values locally.
    fn validate_starting_directory(&self) -> Result<(), LocalDirectoryError> {
        let WorkspaceTerminalLaunchContext::Local(plan) = &self.launch_context else {
            return Ok(());
        };
        #[cfg(test)]
        if plan.working_directory().identity().is_synthetic() {
            return Ok(());
        }
        self.local_filesystem
            .as_ref()
            .ok_or(LocalDirectoryError::Other)?
            .revalidate_directory(plan.working_directory())?;
        Ok(())
    }

    pub(crate) fn set_pinned_directory(&mut self, directory: Option<PinnedDirectory>) {
        self.pinned_directory = directory;
    }

    /// Scopes later launches to one local Worktree, or to the Workspace with `None`.
    ///
    /// Inside a Worktree, a launch starts in the Pinned Directory or the source directory only
    /// when it lies inside the Worktree root, and otherwise in the root itself.
    pub(crate) fn set_worktree_root(&mut self, root: Option<std::path::PathBuf>) {
        self.worktree_root = root;
    }

    /// Captures and validates the starting directory before asynchronous launch work begins.
    /// The workspace factory retains immutable home; this clone owns only one launch selection.
    pub(crate) fn for_source_directory(
        &self,
        source: Option<CurrentDirectory>,
    ) -> Result<Self, LocalDirectoryError> {
        let mut selected = self.clone();
        match &mut selected.launch_context {
            WorkspaceTerminalLaunchContext::Local(plan) => {
                let authority = self
                    .local_filesystem
                    .as_ref()
                    .ok_or(LocalDirectoryError::Other)?;
                if let Some(root) = &self.worktree_root {
                    let directory = match (&self.pinned_directory, source) {
                        (Some(PinnedDirectory::Local(directory)), _)
                            if directory.path().starts_with(root) =>
                        {
                            directory.clone()
                        }
                        (_, Some(CurrentDirectory::Local(path))) if path.starts_with(root) => {
                            authority.validate_directory(&path)?
                        }
                        _ => authority.validate_directory(root)?,
                    };
                    *plan = LocalTerminalLaunchPlan::new(directory);
                    selected.validate_starting_directory()?;
                    return Ok(selected);
                }
                let directory = match (&self.pinned_directory, source) {
                    (Some(PinnedDirectory::Local(directory)), _) => directory.clone(),
                    (Some(PinnedDirectory::Remote { .. }), _)
                    | (None, Some(CurrentDirectory::Remote(_))) => {
                        return Err(LocalDirectoryError::Other);
                    }
                    (None, Some(CurrentDirectory::Local(path)))
                        if path != plan.working_directory().path() =>
                    {
                        authority.validate_directory(&path)?
                    }
                    _ => plan.working_directory().clone(),
                };
                *plan = LocalTerminalLaunchPlan::new(directory);
            }
            WorkspaceTerminalLaunchContext::Remote(context) => {
                let directory = match (&self.pinned_directory, source) {
                    (
                        Some(PinnedDirectory::Remote {
                            directory,
                            identity,
                        }),
                        _,
                    ) => {
                        selected.expected_remote_identity = Some(identity.clone());
                        directory.clone()
                    }
                    (Some(PinnedDirectory::Local(_)), _)
                    | (None, Some(CurrentDirectory::Local(_))) => {
                        return Err(LocalDirectoryError::Other);
                    }
                    (None, Some(CurrentDirectory::Remote(directory))) => {
                        selected.expected_remote_identity = None;
                        directory
                    }
                    (None, None) => context.metadata_context.initial_directory().clone(),
                };
                context.metadata_context.set_initial_directory(directory);
            }
        }
        selected.validate_starting_directory()?;
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::domain::{RemoteDirectory, SshDestination};
    use crate::ssh::command::ValidatedRemoteShellCommand;
    use crate::ssh::testing::SshConnectionFixture;
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };
    use crate::terminal::test_terminal_appearance_update;
    use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

    struct TestTerminalSessionChannelProvider {
        ready: AtomicBool,
        preparations: AtomicUsize,
        revalidations: Mutex<Vec<(RemoteDirectory, Option<RemoteDirectoryIdentity>)>>,
        owners: Mutex<Vec<SshConnectionFixture>>,
        results: Mutex<VecDeque<Result<OwnedChannel, TerminalSessionChannelUnavailable>>>,
    }

    type OwnedChannel = (
        SshConnectionFixture,
        PreparedSshTerminalSessionChannelCommand,
    );

    impl TestTerminalSessionChannelProvider {
        fn new(
            ready: bool,
            results: impl IntoIterator<Item = Result<OwnedChannel, TerminalSessionChannelUnavailable>>,
        ) -> Self {
            Self {
                ready: AtomicBool::new(ready),
                preparations: AtomicUsize::new(0),
                revalidations: Mutex::new(Vec::new()),
                owners: Mutex::new(Vec::new()),
                results: Mutex::new(results.into_iter().collect()),
            }
        }
    }

    impl TerminalSessionChannelProvider for TestTerminalSessionChannelProvider {
        fn is_ready(&self) -> bool {
            self.ready.load(Ordering::Acquire)
        }

        fn revalidate(
            &self,
            directory: RemoteDirectory,
            expected_identity: Option<RemoteDirectoryIdentity>,
        ) -> Task<Result<(), TerminalSessionChannelRevalidationError>> {
            self.revalidations
                .lock()
                .unwrap()
                .push((directory, expected_identity));
            if self.is_ready() {
                Task::ready(Ok(()))
            } else {
                Task::ready(Err(
                    TerminalSessionChannelRevalidationError::ConnectionUnavailable,
                ))
            }
        }

        fn prepare(
            &self,
            _directory: &RemoteDirectory,
        ) -> Result<PreparedSshTerminalSessionChannelCommand, TerminalSessionChannelUnavailable>
        {
            self.preparations.fetch_add(1, Ordering::AcqRel);
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TerminalSessionChannelUnavailable))
                .map(|(owner, channel)| {
                    self.owners.lock().unwrap().push(owner);
                    channel
                })
        }
    }

    fn prepared_channel(destination: &SshDestination, command: &str) -> OwnedChannel {
        let owner = SshConnectionFixture::new(destination.clone());
        let channel = owner.prepare_terminal_session_channel(
            ValidatedRemoteShellCommand::new(command.to_owned()).unwrap(),
        );
        (owner, channel)
    }

    fn remote_factory(
        records: TestTerminalSessionRecords,
        provider: Arc<dyn TerminalSessionChannelProvider>,
    ) -> WorkspaceTerminalSessionFactory {
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let remote_directory = RemoteDirectory::new("~/project".to_owned()).unwrap();
        WorkspaceTerminalSessionFactory::new_remote(
            Rc::new(TestTerminalSessionFactory::new(records)),
            crate::terminal::testing::test_local_directory(PathBuf::from(
                "/local/home/used-only-as-process-cwd",
            )),
            RemoteTerminalMetadataContext::new(destination, remote_directory).with_machine(
                crate::terminal::metadata::RemoteMachine::new(Some("tester"), Some("/home/tester")),
            ),
            RemoteDirectoryIdentity::new("/home/tester/project".to_owned()).unwrap(),
            "project on remote".to_owned(),
            provider,
        )
    }

    #[test]
    fn local_factory_should_forward_the_validated_launch_plan() {
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let directory = ValidatedLocalDirectory::new(
            PathBuf::from("/typed-local-workspace"),
            crate::domain::LocalDirectoryIdentity::for_test(7011),
        );
        let factory =
            WorkspaceTerminalSessionFactory::new_local(session_factory, directory.clone());
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(8.0, 20.0),
            BackingScale::ONE,
        );

        let launch = factory.prepare_child_launch().unwrap();
        let _started = factory
            .start(geometry, launch, test_terminal_appearance_update())
            .unwrap();

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
    fn remote_factory_should_preserve_context_and_prepare_one_terminal_session_channel_per_child() {
        let records = TestTerminalSessionRecords::default();
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let remote_directory = RemoteDirectory::new("~/project".to_owned()).unwrap();
        let provider = Arc::new(TestTerminalSessionChannelProvider::new(
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
        let _first = factory
            .start(geometry, first, test_terminal_appearance_update())
            .unwrap();
        let _second = factory
            .start(geometry, second, test_terminal_appearance_update())
            .unwrap();

        assert_eq!(provider.preparations.load(Ordering::Acquire), 2);
        assert_eq!(factory.local_working_directory(), None);
        assert!(factory.validate_starting_directory().is_ok());
        assert_eq!(
            factory.local_file_capabilities(),
            TerminalLocalFileCapabilities::Disabled
        );
        assert_eq!(factory.fallback_title(), "project on remote");
        let starts = records.starts();
        assert_eq!(starts.len(), 2);
        assert!(
            starts
                .iter()
                .all(|start| start.local_working_directory().is_none())
        );
        assert!(starts.iter().all(|start| {
            start.remote_launch_plan().is_some_and(|plan| {
                assert_eq!(
                    plan.local_home().path(),
                    std::path::Path::new("/local/home/used-only-as-process-cwd")
                );
                plan.metadata_context()
                    == &RemoteTerminalMetadataContext::new(
                        destination.clone(),
                        remote_directory.clone(),
                    )
                    .with_machine(
                        crate::terminal::metadata::RemoteMachine::new(
                            Some("tester"),
                            Some("/home/tester"),
                        ),
                    )
            })
        }));
    }

    #[test]
    fn remote_child_launches_should_preserve_machine_when_selecting_starting_directories() {
        let records = TestTerminalSessionRecords::default();
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(TestTerminalSessionChannelProvider::new(
            true,
            (0..3).map(|_| Ok(prepared_channel(&destination, "exec /bin/zsh -l"))),
        ));
        let mut factory = remote_factory(records.clone(), provider);
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(8.0, 20.0),
            BackingScale::ONE,
        );
        for (source, pin, expected_directory) in [
            (None, None, "~/project"),
            (Some("/srv/source"), None, "/srv/source"),
            (Some("/srv/source"), Some("/srv/pin"), "/srv/pin"),
        ] {
            factory.set_pinned_directory(pin.map(|path| PinnedDirectory::Remote {
                directory: RemoteDirectory::new(path.to_owned()).unwrap(),
                identity: RemoteDirectoryIdentity::new(path.to_owned()).unwrap(),
            }));
            let selected = factory
                .for_source_directory(source.map(|path| {
                    CurrentDirectory::Remote(RemoteDirectory::new(path.to_owned()).unwrap())
                }))
                .unwrap();
            let prepared = selected.prepare_child_launch().unwrap();
            let _started = selected
                .start(geometry, prepared, test_terminal_appearance_update())
                .unwrap();
            let starts = records.starts();
            let plan = starts.last().unwrap().remote_launch_plan().unwrap();
            let expected = RemoteTerminalMetadataContext::new(
                destination.clone(),
                RemoteDirectory::new(expected_directory.to_owned()).unwrap(),
            )
            .with_machine(crate::terminal::metadata::RemoteMachine::new(
                Some("tester"),
                Some("/home/tester"),
            ));
            assert_eq!(plan.metadata_context(), &expected);
        }
    }

    #[test]
    fn remote_factory_should_reject_launch_when_provider_is_not_ready() {
        let records = TestTerminalSessionRecords::default();
        let provider = Arc::new(TestTerminalSessionChannelProvider::new(false, []));
        let factory = remote_factory(records.clone(), provider.clone());

        let error = factory.prepare_child_launch().unwrap_err();

        assert_eq!(error, TerminalSessionChannelUnavailable);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 0);
        assert!(records.starts().is_empty());
    }

    #[test]
    fn remote_factory_propagates_provider_reservation_failure() {
        let records = TestTerminalSessionRecords::default();
        let provider = Arc::new(TestTerminalSessionChannelProvider::new(
            true,
            [Err(TerminalSessionChannelUnavailable)],
        ));
        let factory = remote_factory(records.clone(), provider.clone());

        let error = factory.prepare_child_launch().unwrap_err();

        assert_eq!(error, TerminalSessionChannelUnavailable);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 1);
        assert!(records.starts().is_empty());
    }

    #[test]
    fn prepared_remote_launches_should_be_distinct_and_single_use() {
        let records = TestTerminalSessionRecords::default();
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(TestTerminalSessionChannelProvider::new(
            true,
            [
                Ok(prepared_channel(&destination, "exec first")),
                Ok(prepared_channel(&destination, "exec second")),
            ],
        ));
        let factory = remote_factory(records, provider);
        let first = factory.prepare_child_launch().unwrap();
        let second = factory.prepare_child_launch().unwrap();

        let first_command = first.take_terminal_session_channel().unwrap();
        let second_command = second.take_terminal_session_channel().unwrap();
        let error = match first.take_terminal_session_channel() {
            Ok(_) => panic!("a prepared remote launch must be single-use"),
            Err(error) => error,
        };

        assert_ne!(first_command.arguments(), second_command.arguments());
        assert!(matches!(
            error,
            crate::ssh::command::PreparedSshTerminalSessionChannelError::AlreadyConsumed
        ));
    }
    #[test]
    fn local_launch_policy_should_capture_source_pin_and_home_without_changing_existing_launches() {
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let home = fixture.path().to_path_buf();
        let source = home.join("shell-integration/bash");
        let pin = home.join("shell-integration/zsh");
        let authority = LocalFilesystemAuthority::testing();
        let records = TestTerminalSessionRecords::default();
        let mut factory = WorkspaceTerminalSessionFactory::new_local_with_authority(
            Rc::new(TestTerminalSessionFactory::new(records.clone())),
            authority.validate_directory(&home).unwrap(),
            authority.clone(),
        );
        let captured = factory
            .for_source_directory(Some(CurrentDirectory::Local(source.clone())))
            .unwrap();
        factory.set_pinned_directory(Some(PinnedDirectory::Local(
            authority.validate_directory(&pin).unwrap(),
        )));
        let pinned = factory
            .for_source_directory(Some(CurrentDirectory::Local(source.clone())))
            .unwrap();
        assert_eq!(captured.local_working_directory(), Some(source.as_path()));
        assert_eq!(pinned.local_working_directory(), Some(pin.as_path()));
        factory.set_pinned_directory(None);
        assert_eq!(
            factory
                .for_source_directory(None)
                .unwrap()
                .local_working_directory(),
            Some(home.as_path())
        );
        assert_eq!(
            factory
                .for_source_directory(Some(CurrentDirectory::Local(source.clone())))
                .unwrap()
                .local_working_directory(),
            Some(source.as_path())
        );
        assert!(
            factory
                .for_source_directory(Some(CurrentDirectory::Local(home.join("missing"))))
                .is_err()
        );
        assert!(
            factory
                .for_source_directory(Some(CurrentDirectory::Remote(
                    RemoteDirectory::new("/tmp".into()).unwrap()
                )))
                .is_err()
        );
        assert!(
            records.starts().is_empty(),
            "directory policy must not mutate sessions"
        );
    }

    #[test]
    fn worktree_launches_should_start_inside_their_worktree() {
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let home = fixture.path().to_path_buf();
        let worktree = home.join("shell-integration");
        let inside = worktree.join("bash");
        let outside = home.clone();
        let authority = LocalFilesystemAuthority::testing();
        let mut factory = WorkspaceTerminalSessionFactory::new_local_with_authority(
            Rc::new(TestTerminalSessionFactory::new(
                TestTerminalSessionRecords::default(),
            )),
            authority.validate_directory(&home).unwrap(),
            authority.clone(),
        );
        factory.set_pinned_directory(Some(PinnedDirectory::Local(
            authority.validate_directory(&home).unwrap(),
        )));
        factory.set_worktree_root(Some(worktree.clone()));
        let selected = |factory: &WorkspaceTerminalSessionFactory,
                        source: Option<&std::path::PathBuf>| {
            factory
                .for_source_directory(source.map(|path| CurrentDirectory::Local(path.clone())))
                .unwrap()
                .local_working_directory()
                .map(std::path::Path::to_path_buf)
        };

        assert_eq!(
            (
                selected(&factory, Some(&inside)),
                selected(&factory, Some(&outside)),
                selected(&factory, None),
            ),
            (
                Some(inside.clone()),
                Some(worktree.clone()),
                Some(worktree.clone())
            )
        );
        factory.set_pinned_directory(Some(PinnedDirectory::Local(
            authority.validate_directory(&inside).unwrap(),
        )));
        assert_eq!(selected(&factory, Some(&outside)), Some(inside));
        factory.set_worktree_root(Some(home.join("missing")));
        assert!(factory.for_source_directory(None).is_err());
    }

    #[test]
    fn unavailable_explicit_pin_should_never_fall_back_to_source_or_home() {
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let authority = LocalFilesystemAuthority::testing();
        let pin = fixture.path().join("pinned");
        std::fs::create_dir(&pin).unwrap();
        let mut factory = WorkspaceTerminalSessionFactory::new_local_with_authority(
            Rc::new(TestTerminalSessionFactory::new(
                TestTerminalSessionRecords::default(),
            )),
            authority.validate_directory(fixture.path()).unwrap(),
            authority.clone(),
        );
        factory.set_pinned_directory(Some(PinnedDirectory::Local(
            authority.validate_directory(&pin).unwrap(),
        )));
        std::fs::remove_dir(&pin).unwrap();
        assert!(
            factory
                .for_source_directory(Some(CurrentDirectory::Local(fixture.path().to_owned())))
                .is_err()
        );
    }

    #[test]
    fn remote_launch_policy_should_capture_directory_and_identity_without_local_authority() {
        let provider = Arc::new(TestTerminalSessionChannelProvider::new(true, []));
        let mut factory = remote_factory(TestTerminalSessionRecords::default(), provider.clone());
        let source = RemoteDirectory::new("/srv/frontend".into()).unwrap();
        let pin = RemoteDirectory::new("/srv/pinned".into()).unwrap();
        let identity = RemoteDirectoryIdentity::new("/remote-identity".into()).unwrap();
        let home_identity =
            RemoteDirectoryIdentity::new("/home/tester/project".to_owned()).unwrap();
        let _initial_revalidation = factory.revalidate_remote_child_launch().unwrap();
        let captured = factory
            .for_source_directory(Some(CurrentDirectory::Remote(source.clone())))
            .unwrap();
        let _source_revalidation = captured.revalidate_remote_child_launch().unwrap();
        factory.set_pinned_directory(Some(PinnedDirectory::Remote {
            directory: pin.clone(),
            identity: identity.clone(),
        }));
        let selected = factory
            .for_source_directory(Some(CurrentDirectory::Remote(source.clone())))
            .unwrap();
        let _pin_revalidation = selected.revalidate_remote_child_launch().unwrap();
        let WorkspaceTerminalLaunchContext::Remote(context) = &captured.launch_context else {
            panic!("remote context")
        };
        assert_eq!(context.metadata_context.initial_directory(), &source);
        let WorkspaceTerminalLaunchContext::Remote(context) = &selected.launch_context else {
            panic!("remote context")
        };
        assert_eq!(context.metadata_context.initial_directory(), &pin);
        assert_eq!(selected.expected_remote_identity, Some(identity.clone()));
        assert_eq!(selected.local_working_directory(), None);
        factory.set_pinned_directory(None);
        let home = factory.for_source_directory(None).unwrap();
        let _home_revalidation = home.revalidate_remote_child_launch().unwrap();
        let WorkspaceTerminalLaunchContext::Remote(context) = home.launch_context else {
            panic!("remote context")
        };
        assert_eq!(
            context.metadata_context.initial_directory().as_str(),
            "~/project"
        );
        assert_eq!(
            provider.revalidations.lock().unwrap().as_slice(),
            [
                (
                    RemoteDirectory::new("~/project".to_owned()).unwrap(),
                    Some(home_identity.clone()),
                ),
                (source.clone(), None),
                (pin, Some(identity)),
                (
                    RemoteDirectory::new("~/project".to_owned()).unwrap(),
                    Some(home_identity),
                ),
            ]
        );
        assert!(
            factory
                .for_source_directory(Some(CurrentDirectory::Local(PathBuf::from("/tmp"))))
                .is_err()
        );
    }
}
