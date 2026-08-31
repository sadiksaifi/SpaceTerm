use std::path::PathBuf;
use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::session::{
    LocalTerminalLaunchPlan, SessionError, StartedTerminalSession, TerminalLaunchPlan,
    TerminalSessionFactory,
};
use crate::domain::{ValidatedWorkspaceDirectory, WorkspaceDirectoryIdentity};
use crate::platform::workspace_directory::{WorkspaceDirectoryError, validate_workspace_directory};

#[derive(Clone)]
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    launch_plan: LocalTerminalLaunchPlan,
}

impl WorkspaceTerminalSessionFactory {
    pub(crate) fn new_local(
        session_factory: Rc<dyn TerminalSessionFactory>,
        working_directory: ValidatedWorkspaceDirectory,
    ) -> Self {
        Self {
            session_factory,
            launch_plan: LocalTerminalLaunchPlan::new(working_directory),
        }
    }

    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
    ) -> Result<StartedTerminalSession, SessionError> {
        self.session_factory.start(
            geometry,
            TerminalLaunchPlan::Local(self.launch_plan.clone()),
        )
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.session_factory.fallback_title()
    }

    pub(crate) fn working_directory(&self) -> &std::path::Path {
        self.launch_plan.working_directory().path()
    }

    pub(crate) fn validate_working_directory(
        &self,
    ) -> Result<ValidatedWorkspaceDirectory, WorkspaceDirectoryError> {
        #[cfg(test)]
        if self
            .launch_plan
            .working_directory()
            .identity()
            .is_synthetic()
        {
            return Ok(self.launch_plan.working_directory().clone());
        }
        let directory = validate_workspace_directory(self.working_directory())?;
        if self.launch_plan.working_directory().identity() != directory.identity() {
            return Err(WorkspaceDirectoryError::IdentityChanged);
        }
        Ok(directory)
    }

    pub(crate) fn set_working_directory(
        &mut self,
        workspace_root: PathBuf,
        identity: WorkspaceDirectoryIdentity,
    ) {
        self.launch_plan = LocalTerminalLaunchPlan::new(ValidatedWorkspaceDirectory::new(
            workspace_root,
            identity,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
