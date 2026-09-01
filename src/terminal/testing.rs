use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use super::geometry::TerminalGeometry;
use super::{
    AcceptanceSessionFailure, FindDirection, FindQueryGeneration, KeyInput,
    Osc52AuthorizationDecision, Osc52AuthorizationId, PasteConfirmationId, PasteDecision,
    PasteRequestOutcome, PasteResolution, PointerInput, PresentationGeneration, SelectionCopy,
    SelectionCopyError, SessionError, SessionEvent, StartedTerminalSession,
    TerminalAccessibilityModel, TerminalLaunchPlan, TerminalSessionFactory, TerminalSessionHandle,
    WheelInput,
};
use crate::domain::{ValidatedWorkspaceDirectory, WorkspaceDirectoryIdentity};

pub(crate) fn test_workspace_directory(path: PathBuf) -> ValidatedWorkspaceDirectory {
    ValidatedWorkspaceDirectory::new(path, WorkspaceDirectoryIdentity::new(0, 0))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordedSessionStart {
    pub(crate) session_id: usize,
    pub(crate) geometry: TerminalGeometry,
    launch_plan: TerminalLaunchPlan,
}

impl RecordedSessionStart {
    pub(crate) const fn launch_plan(&self) -> &TerminalLaunchPlan {
        &self.launch_plan
    }

    pub(crate) const fn local_launch_plan(&self) -> Option<&super::LocalTerminalLaunchPlan> {
        match &self.launch_plan {
            TerminalLaunchPlan::Local(plan) => Some(plan),
            TerminalLaunchPlan::Remote(_) => None,
        }
    }

    pub(crate) const fn remote_launch_plan(&self) -> Option<&super::RemoteTerminalLaunchPlan> {
        match &self.launch_plan {
            TerminalLaunchPlan::Local(_) => None,
            TerminalLaunchPlan::Remote(plan) => Some(plan),
        }
    }

    pub(crate) fn local_working_directory(&self) -> Option<&ValidatedWorkspaceDirectory> {
        self.local_launch_plan()
            .map(super::LocalTerminalLaunchPlan::working_directory)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecordedSessionCommand {
    Key(KeyInput),
    Focus(bool),
    Resize(TerminalGeometry),
    Pointer(PointerInput),
    Wheel(WheelInput),
    ScrollTo(u64, PresentationGeneration),
    SetFindQuery(FindQueryGeneration, String),
    NavigateFind(FindQueryGeneration, FindDirection),
    EndFind(FindQueryGeneration),
    RequestPaste(String),
    ResolvePaste(PasteConfirmationId, PasteDecision),
    ResolveOsc52Authorization(Osc52AuthorizationId, Osc52AuthorizationDecision),
    RequestSelectionCopy,
    InjectAcceptanceFailure(AcceptanceSessionFailure),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordedSessionCall {
    pub(crate) session_id: usize,
    pub(crate) command: RecordedSessionCommand,
}

#[derive(Clone, Default)]
pub(crate) struct TestTerminalSessionRecords {
    starts: Rc<RefCell<Vec<RecordedSessionStart>>>,
    event_senders: Rc<RefCell<BTreeMap<usize, async_channel::Sender<SessionEvent>>>>,
    accessibility_senders:
        Rc<RefCell<BTreeMap<usize, async_channel::Sender<Arc<TerminalAccessibilityModel>>>>>,
    dropped_session_ids: Rc<RefCell<Vec<usize>>>,
    commands: Rc<RefCell<Vec<RecordedSessionCall>>>,
}

impl TestTerminalSessionRecords {
    pub(crate) fn starts(&self) -> Vec<RecordedSessionStart> {
        self.starts.borrow().clone()
    }

    pub(crate) fn event_sender(
        &self,
        session_id: usize,
    ) -> Option<async_channel::Sender<SessionEvent>> {
        self.event_senders.borrow().get(&session_id).cloned()
    }

    pub(crate) fn last_event_sender(&self) -> Option<async_channel::Sender<SessionEvent>> {
        self.event_senders
            .borrow()
            .last_key_value()
            .map(|(_, sender)| sender.clone())
    }

    pub(crate) fn last_accessibility_sender(
        &self,
    ) -> Option<async_channel::Sender<Arc<TerminalAccessibilityModel>>> {
        self.accessibility_senders
            .borrow()
            .last_key_value()
            .map(|(_, sender)| sender.clone())
    }

    pub(crate) fn session_count(&self) -> usize {
        self.event_senders.borrow().len()
    }

    pub(crate) fn dropped_session_ids(&self) -> Vec<usize> {
        self.dropped_session_ids.borrow().clone()
    }

    pub(crate) fn commands(&self) -> Vec<RecordedSessionCall> {
        self.commands.borrow().clone()
    }

    pub(crate) fn pointer_count(&self) -> usize {
        self.commands
            .borrow()
            .iter()
            .filter(|input| matches!(input.command, RecordedSessionCommand::Pointer(_)))
            .count()
    }
}

pub(crate) struct TestTerminalSessionFactory {
    records: TestTerminalSessionRecords,
    next_session_id: Cell<usize>,
    fallback_title: String,
    start_failure: Option<String>,
    start_failure_session_id: Option<usize>,
    selection_response: Result<Option<SelectionCopy>, SelectionCopyError>,
    paste_response: Result<PasteRequestOutcome, String>,
    paste_resolution: Result<PasteResolution, String>,
}

impl TestTerminalSessionFactory {
    pub(crate) fn new(records: TestTerminalSessionRecords) -> Self {
        Self {
            records,
            next_session_id: Cell::new(1),
            fallback_title: "Terminal".to_owned(),
            start_failure: None,
            start_failure_session_id: None,
            selection_response: Ok(None),
            paste_response: Ok(PasteRequestOutcome::Written),
            paste_resolution: Ok(PasteResolution::Written),
        }
    }

    pub(crate) fn with_fallback_title(mut self, title: impl Into<String>) -> Self {
        self.fallback_title = title.into();
        self
    }

    pub(crate) fn with_start_failure(mut self, message: impl Into<String>) -> Self {
        self.start_failure = Some(message.into());
        self.start_failure_session_id = None;
        self
    }

    pub(crate) fn with_start_failure_at(
        mut self,
        session_id: usize,
        message: impl Into<String>,
    ) -> Self {
        self.start_failure = Some(message.into());
        self.start_failure_session_id = Some(session_id);
        self
    }

    pub(crate) fn with_selection_copy_response(
        mut self,
        response: Result<Option<SelectionCopy>, SelectionCopyError>,
    ) -> Self {
        self.selection_response = response;
        self
    }

    pub(crate) fn with_paste_response(
        mut self,
        response: Result<PasteRequestOutcome, String>,
    ) -> Self {
        self.paste_response = response;
        self
    }

    pub(crate) fn with_paste_resolution(
        mut self,
        response: Result<PasteResolution, String>,
    ) -> Self {
        self.paste_resolution = response;
        self
    }
}

impl TerminalSessionFactory for TestTerminalSessionFactory {
    fn start(
        &self,
        geometry: TerminalGeometry,
        launch_plan: TerminalLaunchPlan,
    ) -> Result<StartedTerminalSession, SessionError> {
        let session_id = self.next_session_id.get();
        self.next_session_id.set(session_id + 1);
        self.records.starts.borrow_mut().push(RecordedSessionStart {
            session_id,
            geometry,
            launch_plan,
        });

        if let Some(message) = &self.start_failure
            && self
                .start_failure_session_id
                .is_none_or(|failure_session_id| failure_session_id == session_id)
        {
            return Err(SessionError::EmulatorStartup(message.clone()));
        }

        let (event_sender, events) = async_channel::unbounded();
        self.records
            .event_senders
            .borrow_mut()
            .insert(session_id, event_sender);
        let (accessibility_sender, accessibility) = async_channel::bounded(1);
        self.records
            .accessibility_senders
            .borrow_mut()
            .insert(session_id, accessibility_sender);

        Ok(StartedTerminalSession {
            handle: Box::new(TestTerminalSessionHandle {
                session_id,
                records: self.records.clone(),
                selection_response: self.selection_response.clone(),
                paste_response: self.paste_response.clone(),
                paste_resolution: self.paste_resolution.clone(),
            }),
            events,
            accessibility,
        })
    }

    fn fallback_title(&self) -> String {
        self.fallback_title.clone()
    }
}

struct TestTerminalSessionHandle {
    session_id: usize,
    records: TestTerminalSessionRecords,
    selection_response: Result<Option<SelectionCopy>, SelectionCopyError>,
    paste_response: Result<PasteRequestOutcome, String>,
    paste_resolution: Result<PasteResolution, String>,
}

impl TestTerminalSessionHandle {
    fn record(&self, command: RecordedSessionCommand) {
        self.records
            .commands
            .borrow_mut()
            .push(RecordedSessionCall {
                session_id: self.session_id,
                command,
            });
    }
}

impl Drop for TestTerminalSessionHandle {
    fn drop(&mut self) {
        self.records
            .dropped_session_ids
            .borrow_mut()
            .push(self.session_id);
    }
}

impl TerminalSessionHandle for TestTerminalSessionHandle {
    fn key(&self, input: KeyInput) {
        self.record(RecordedSessionCommand::Key(input));
    }

    fn focus(&self, focused: bool) {
        self.record(RecordedSessionCommand::Focus(focused));
    }

    fn resize(&self, geometry: TerminalGeometry) {
        self.record(RecordedSessionCommand::Resize(geometry));
    }

    fn pointer(&self, input: PointerInput) {
        self.record(RecordedSessionCommand::Pointer(input));
    }

    fn wheel(&self, input: WheelInput) {
        self.record(RecordedSessionCommand::Wheel(input));
    }

    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration) {
        self.record(RecordedSessionCommand::ScrollTo(offset_rows, generation));
    }

    fn set_find_query(&self, generation: FindQueryGeneration, query: String) {
        self.record(RecordedSessionCommand::SetFindQuery(generation, query));
    }

    fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection) {
        self.record(RecordedSessionCommand::NavigateFind(generation, direction));
    }

    fn end_find(&self, generation: FindQueryGeneration) {
        self.record(RecordedSessionCommand::EndFind(generation));
    }

    fn request_paste(
        &self,
        text: String,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>> {
        self.record(RecordedSessionCommand::RequestPaste(text));
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(self.paste_response.clone());
        receiver
    }

    fn resolve_paste(
        &self,
        id: PasteConfirmationId,
        decision: PasteDecision,
    ) -> async_channel::Receiver<Result<PasteResolution, String>> {
        self.record(RecordedSessionCommand::ResolvePaste(id, decision));
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(self.paste_resolution.clone());
        receiver
    }

    fn resolve_osc52_authorization(
        &self,
        id: Osc52AuthorizationId,
        decision: Osc52AuthorizationDecision,
    ) {
        self.record(RecordedSessionCommand::ResolveOsc52Authorization(
            id, decision,
        ));
    }

    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedSessionCommand::RequestSelectionCopy);
        self.selection_response.clone()
    }

    fn inject_acceptance_failure(&self, failure: AcceptanceSessionFailure) {
        self.record(RecordedSessionCommand::InjectAcceptanceFailure(failure));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};
    use crate::terminal::{RemoteTerminalLaunchPlan, TerminalLaunchPlan};

    #[test]
    fn test_factory_should_record_typed_remote_launch_context() {
        let records = TestTerminalSessionRecords::default();
        let factory = TestTerminalSessionFactory::new(records.clone());
        let destination = SshDestination::new("user@remote".to_owned()).unwrap();
        let directory = RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap();
        let prepared = SshCommandContext::new(
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            destination.clone(),
            PathBuf::from("/private/runtime/spaceterm/control.sock"),
        )
        .unwrap()
        .prepare_pane_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
        );
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(8.0, 20.0),
            BackingScale::ONE,
        );

        let _started = factory
            .start(
                geometry,
                TerminalLaunchPlan::Remote(Box::new(RemoteTerminalLaunchPlan::new(
                    test_workspace_directory(PathBuf::from("/Users/local")),
                    destination.clone(),
                    directory.clone(),
                    "project on remote".to_owned(),
                    prepared,
                ))),
            )
            .unwrap();

        let starts = records.starts();
        let plan = starts[0]
            .remote_launch_plan()
            .expect("the test factory must preserve the remote plan");
        assert_eq!(plan.destination(), &destination);
        assert_eq!(plan.remote_directory(), &directory);
        assert_eq!(plan.local_home().path(), PathBuf::from("/Users/local"));
        assert!(starts[0].local_working_directory().is_none());
    }
}
