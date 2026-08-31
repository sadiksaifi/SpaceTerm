use std::path::PathBuf;
use std::rc::Rc;

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
    prepare_pane_channel: Rc<dyn Fn() -> PreparedSshPaneChannelCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceChildLaunchValidation {
    Local(ValidatedWorkspaceDirectory),
    Remote,
}

#[derive(Clone)]
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    launch_context: WorkspaceTerminalLaunchContext,
}

impl WorkspaceTerminalSessionFactory {
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

    pub(crate) fn new_remote(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home: ValidatedWorkspaceDirectory,
        metadata_context: RemoteTerminalMetadataContext,
        fallback_title: String,
        prepare_pane_channel: Rc<dyn Fn() -> PreparedSshPaneChannelCommand>,
    ) -> Self {
        Self {
            session_factory,
            launch_context: WorkspaceTerminalLaunchContext::Remote(
                RemoteWorkspaceTerminalLaunchContext {
                    local_home,
                    metadata_context,
                    fallback_title,
                    prepare_pane_channel,
                },
            ),
        }
    }

    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
    ) -> Result<StartedTerminalSession, SessionError> {
        let launch_plan = match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(plan) => TerminalLaunchPlan::Local(plan.clone()),
            WorkspaceTerminalLaunchContext::Remote(context) => {
                TerminalLaunchPlan::Remote(RemoteTerminalLaunchPlan::new(
                    context.local_home.clone(),
                    context.metadata_context.destination().clone(),
                    context.metadata_context.initial_directory().clone(),
                    context.fallback_title.clone(),
                    (context.prepare_pane_channel)(),
                ))
            }
        };
        self.session_factory.start(geometry, launch_plan)
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

    pub(crate) fn local_working_directory(&self) -> Option<&std::path::Path> {
        match &self.launch_context {
            WorkspaceTerminalLaunchContext::Local(plan) => Some(plan.working_directory().path()),
            WorkspaceTerminalLaunchContext::Remote(_) => None,
        }
    }

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
    use std::cell::Cell;

    use super::*;
    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };
    use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

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

        let _started = factory.start(geometry).unwrap();

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
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let destination = SshDestination::new("tester@remote".to_owned()).unwrap();
        let remote_directory = RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap();
        let metadata_context =
            RemoteTerminalMetadataContext::new(destination.clone(), remote_directory.clone());
        let command_context = Rc::new(
            SshCommandContext::new(
                PathBuf::from("/private/config/spaceterm/ssh_config"),
                destination.clone(),
                PathBuf::from("/private/runtime/spaceterm/master.sock"),
            )
            .unwrap(),
        );
        let preparations = Rc::new(Cell::new(0));
        let prepare_pane_channel = {
            let command_context = Rc::clone(&command_context);
            let preparations = Rc::clone(&preparations);
            Rc::new(move || {
                preparations.set(preparations.get() + 1);
                command_context.prepare_pane_channel(
                    ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
                )
            })
        };
        let factory = WorkspaceTerminalSessionFactory::new_remote(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/local/home/used-only-as-process-cwd",
            )),
            metadata_context,
            "project on remote".to_owned(),
            prepare_pane_channel,
        );
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(8.0, 20.0),
            BackingScale::ONE,
        );

        let _first = factory.start(geometry).unwrap();
        let _second = factory.start(geometry).unwrap();

        assert_eq!(preparations.get(), 2);
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
}
