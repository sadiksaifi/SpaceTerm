use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use super::geometry::TerminalGeometry;
use super::{
    FindDirection, FindQueryGeneration, KeyInput, OptionAsAltPolicy, PasteConfirmationId,
    PasteDecision, PasteRequestOutcome, PasteResolution, PointerInput, PresentationGeneration,
    SelectionCopy, SelectionCopyError, SessionError, SessionEvent, StartedTerminalSession,
    TerminalAccessibilityModel, TerminalKeyInputAdapter, TerminalKeyInputAdapterFactory,
    TerminalLaunchPlan, TerminalSessionFactory, TerminalSessionHandle, WheelInput,
};
use crate::domain::{LocalDirectoryIdentity, ValidatedLocalDirectory};

pub(crate) use super::emulator::TerminalEmulator;
pub(crate) use super::graphics::test_lock as graphics_test_lock;

pub(crate) fn test_local_directory(path: PathBuf) -> ValidatedLocalDirectory {
    ValidatedLocalDirectory::new(path, LocalDirectoryIdentity::for_test(0))
}

pub(crate) fn test_accessibility_viewport_models(
    generation: PresentationGeneration,
) -> (
    Arc<TerminalAccessibilityModel>,
    Arc<TerminalAccessibilityModel>,
) {
    use super::accessibility::{
        AccessibilityCell, AccessibilityCellRef, AccessibilityRowId, AccessibilityRowUpdate,
        AccessibilityScreen, AccessibilityUpdate, TerminalAccessibilityState,
    };
    let rows = [0, 1].map(|page_row| AccessibilityRowId {
        screen: AccessibilityScreen::Primary,
        screen_generation: 1,
        node_serial: 1,
        page_row,
    });
    let cursor = Some(AccessibilityCellRef {
        row: rows[0],
        row_revision: 1,
        column: 0,
    });
    let mut state = TerminalAccessibilityState::default();
    let initial = state
        .apply(
            AccessibilityUpdate {
                revision: 1,
                screen: AccessibilityScreen::Primary,
                screen_generation: 1,
                complete: true,
                more: false,
                topology: Some(rows.to_vec()),
                visible_lines: 0..1,
                cursor,
                selection: None,
                changed_rows: rows
                    .into_iter()
                    .map(|id| AccessibilityRowUpdate {
                        id,
                        revision: 1,
                        soft_wrapped: false,
                        cells: vec![AccessibilityCell::at_column("x", 0, 1)],
                    })
                    .collect(),
            },
            generation,
        )
        .unwrap();
    let scrolled = state
        .apply(
            AccessibilityUpdate {
                revision: 2,
                screen: AccessibilityScreen::Primary,
                screen_generation: 1,
                complete: true,
                more: false,
                topology: None,
                visible_lines: 1..2,
                cursor,
                selection: None,
                changed_rows: Vec::new(),
            },
            generation,
        )
        .unwrap();
    assert!(initial.shares_document(&scrolled));
    assert_eq!(
        initial.selected_or_cursor_range(),
        scrolled.selected_or_cursor_range()
    );
    (initial, scrolled)
}

pub(crate) fn test_terminal_key_input_adapter() -> Box<dyn TerminalKeyInputAdapter> {
    super::key_input::GpuiTerminalKeyInputAdapterFactory::new(OptionAsAltPolicy::default()).create()
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

    pub(crate) fn local_working_directory(&self) -> Option<&ValidatedLocalDirectory> {
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
    PointerAndCopySelection(PointerInput),
    Wheel(WheelInput),
    ScrollTo(u64, PresentationGeneration),
    SetFindQuery(FindQueryGeneration, String),
    NavigateFind(FindQueryGeneration, FindDirection),
    EndFind(FindQueryGeneration),
    RequestPaste(String),
    ResolvePaste(PasteConfirmationId, PasteDecision),
    RequestSelectionCopy,
    RequestSelectionCopyAt(PresentationGeneration),
    SetPresentable(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordedSessionCall {
    pub(crate) session_id: usize,
    pub(crate) command: RecordedSessionCommand,
}

#[derive(Clone, Default)]
pub(crate) struct TestTerminalSessionRecords {
    starts: Rc<RefCell<Vec<RecordedSessionStart>>>,
    selection_copies: Rc<RefCell<VecDeque<Option<SelectionCopy>>>>,
    selection_receivers:
        Rc<RefCell<BTreeMap<usize, super::session::RecordingAccessibilitySelectionReceiver>>>,
    event_senders: Rc<RefCell<BTreeMap<usize, async_channel::Sender<SessionEvent>>>>,
    directory_snapshots: Rc<RefCell<BTreeMap<usize, super::SessionDirectorySnapshot>>>,
    accessibility_senders:
        Rc<RefCell<BTreeMap<usize, async_channel::Sender<Arc<TerminalAccessibilityModel>>>>>,
    dropped_session_ids: Rc<RefCell<Vec<usize>>>,
    commands: Rc<RefCell<Vec<RecordedSessionCall>>>,
}

impl TestTerminalSessionRecords {
    pub(crate) fn report_directory(
        &self,
        session_id: usize,
        current: Option<crate::domain::CurrentDirectory>,
    ) {
        let mut snapshots = self.directory_snapshots.borrow_mut();
        let revision = snapshots
            .get(&session_id)
            .map_or(1, |snapshot| snapshot.revision + 1);
        snapshots.insert(
            session_id,
            super::SessionDirectorySnapshot { revision, current },
        );
        self.event_sender(session_id)
            .expect("session must be live")
            .try_send(SessionEvent::CurrentDirectoryChanged)
            .expect("directory event must fit");
    }

    pub(crate) fn queue_selection_copy(&self, copy: Option<SelectionCopy>) {
        self.selection_copies.borrow_mut().push_back(copy);
    }

    pub(crate) fn accessibility_selection_requests(
        &self,
        session_id: usize,
    ) -> Vec<super::accessibility::AccessibilitySelectionRequest> {
        self.selection_receivers
            .borrow()
            .get(&session_id)
            .map_or_else(Vec::new, |receiver| receiver.drain())
    }

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
            .filter(|input| {
                matches!(
                    input.command,
                    RecordedSessionCommand::Pointer(_)
                        | RecordedSessionCommand::PointerAndCopySelection(_)
                )
            })
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

        let (selection_sender, selection_receiver) =
            super::AccessibilitySelectionSender::recording_channel();
        self.records
            .selection_receivers
            .borrow_mut()
            .insert(session_id, selection_receiver);
        Ok(StartedTerminalSession {
            handle: Box::new(TestTerminalSessionHandle {
                session_id,
                selection_sender,
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
    selection_sender: super::AccessibilitySelectionSender,
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
            .selection_receivers
            .borrow_mut()
            .remove(&self.session_id);
        self.records
            .dropped_session_ids
            .borrow_mut()
            .push(self.session_id);
    }
}

impl TerminalSessionHandle for TestTerminalSessionHandle {
    fn directory_snapshot(&self) -> Option<super::SessionDirectorySnapshot> {
        self.records
            .directory_snapshots
            .borrow()
            .get(&self.session_id)
            .cloned()
    }
    fn accessibility_selection_sender(&self) -> Option<super::AccessibilitySelectionSender> {
        Some(self.selection_sender.clone())
    }

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

    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedSessionCommand::PointerAndCopySelection(input));
        self.selection_response.clone()
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
        text: crate::terminal::native_services::PastePayload,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>> {
        self.record(RecordedSessionCommand::RequestPaste(text.into_text()));
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

    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedSessionCommand::RequestSelectionCopy);
        self.records
            .selection_copies
            .borrow_mut()
            .pop_front()
            .map_or_else(|| self.selection_response.clone(), Ok)
    }

    fn copy_selection_at(
        &self,
        generation: PresentationGeneration,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedSessionCommand::RequestSelectionCopyAt(generation));
        self.selection_response.clone()
    }

    fn set_presentable(&self, presentable: bool) {
        self.record(RecordedSessionCommand::SetPresentable(presentable));
    }
}

/// Isolated resource layout supplied explicitly to portable launch policy.
pub(crate) struct ShellResourcesFixture(PathBuf);
impl ShellResourcesFixture {
    pub(crate) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spaceterm-resources-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        for relative in [
            "bash/spaceterm.bash",
            "elvish/lib/spaceterm-integration.elv",
            "fish/vendor_conf.d/spaceterm-shell-integration.fish",
            "nushell/vendor/autoload/spaceterm.nu",
            "zsh/.zshenv",
        ] {
            let path = root.join("shell-integration").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        Self(root)
    }
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for ShellResourcesFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RemoteDirectory, SshDestination};
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};
    use crate::terminal::{RemoteTerminalLaunchPlan, TerminalLaunchPlan};

    #[test]
    fn test_factory_should_record_typed_remote_launch_context() {
        let records = TestTerminalSessionRecords::default();
        let factory = TestTerminalSessionFactory::new(records.clone());
        let destination = SshDestination::new("user@remote".to_owned()).unwrap();
        let directory = RemoteDirectory::new("~/project".to_owned()).unwrap();
        let prepared = SshCommandContext::new(
            crate::ssh::command::OpenSshExecutable::for_test(),
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
                    test_local_directory(PathBuf::from("/Users/local")),
                    crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                        destination.clone(),
                        directory.clone(),
                    ),
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
