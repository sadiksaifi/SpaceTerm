use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::domain::{LocalDirectoryIdentity, ValidatedLocalDirectory};
use crate::terminal::geometry::TerminalGeometry;
use crate::terminal::{
    FindDirection, FindQueryGeneration, KeyInput, PasteConfirmationId, PasteDecision,
    PasteRequestOutcome, PasteResolution, PointerInput, PresentationGeneration, ScrollbackMovement,
    SelectionCopy, SelectionCopyError, StartedTerminalSession, TerminalAccessibilityModel,
    TerminalAppearanceUpdate, TerminalLaunchPlan, TerminalSessionError, TerminalSessionEvent,
    TerminalSessionFactory, TerminalSessionHandle, WheelInput,
};

#[cfg(feature = "desktop")]
pub(crate) use crate::terminal::emulator::TerminalEmulator;
#[cfg(feature = "desktop")]
pub(crate) use crate::terminal::graphics::test_lock as graphics_test_lock;

pub(crate) fn test_local_directory(path: PathBuf) -> ValidatedLocalDirectory {
    ValidatedLocalDirectory::new(path, LocalDirectoryIdentity::for_test(0))
}

pub(crate) fn test_accessibility_viewport_models(
    generation: PresentationGeneration,
) -> (
    Arc<TerminalAccessibilityModel>,
    Arc<TerminalAccessibilityModel>,
) {
    use crate::terminal::accessibility::{
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

#[cfg(feature = "desktop")]
pub(crate) fn test_terminal_key_input_adapter() -> Box<dyn crate::terminal::TerminalKeyInputAdapter>
{
    use crate::terminal::TerminalKeyInputAdapterFactory as _;
    crate::terminal::key_input::GpuiTerminalKeyInputAdapterFactory::new(
        crate::terminal::OptionAsAltPolicy::default(),
    )
    .create()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordedStart {
    pub(crate) session_id: usize,
    pub(crate) geometry: TerminalGeometry,
    launch_plan: TerminalLaunchPlan,
    pub(crate) initial_appearance: TerminalAppearanceUpdate,
}

impl RecordedStart {
    pub(crate) const fn local_launch_plan(
        &self,
    ) -> Option<&crate::terminal::LocalTerminalLaunchPlan> {
        match &self.launch_plan {
            TerminalLaunchPlan::Local(plan) => Some(plan),
            TerminalLaunchPlan::Remote(_) => None,
        }
    }

    pub(crate) const fn remote_launch_plan(
        &self,
    ) -> Option<&crate::terminal::RemoteTerminalLaunchPlan> {
        match &self.launch_plan {
            TerminalLaunchPlan::Local(_) => None,
            TerminalLaunchPlan::Remote(plan) => Some(plan),
        }
    }

    pub(crate) fn local_working_directory(&self) -> Option<&ValidatedLocalDirectory> {
        self.local_launch_plan()
            .map(crate::terminal::LocalTerminalLaunchPlan::working_directory)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecordedCommand {
    Key(KeyInput),
    ClearScreenAndScrollback,
    Focus(bool),
    Resize(TerminalGeometry),
    Pointer(PointerInput),
    PointerAndCopySelection(PointerInput),
    Wheel(WheelInput),
    ScrollTo(u64, PresentationGeneration),
    ScrollScrollback(ScrollbackMovement),
    SetFindQuery(FindQueryGeneration, String),
    NavigateFind(FindQueryGeneration, FindDirection),
    EndFind(FindQueryGeneration),
    RequestPaste(String),
    ResolvePaste(PasteConfirmationId, PasteDecision),
    CopyOrForward(crate::terminal::InputModifiers),
    RequestSelectionCopy,
    RequestSelectionCopyAt(PresentationGeneration),
    SetPresentable(bool),
    UpdateAppearance(TerminalAppearanceUpdate),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordedCall {
    pub(crate) session_id: usize,
    pub(crate) command: RecordedCommand,
}

#[derive(Clone, Default)]
pub(crate) struct TestTerminalSessionRecords {
    starts: Rc<RefCell<Vec<RecordedStart>>>,
    selection_copies: Rc<RefCell<VecDeque<Option<SelectionCopy>>>>,
    selection_receivers: Rc<
        RefCell<BTreeMap<usize, crate::terminal::session::RecordingAccessibilitySelectionReceiver>>,
    >,
    event_senders: Rc<RefCell<BTreeMap<usize, async_channel::Sender<TerminalSessionEvent>>>>,
    metadata_snapshots:
        Rc<RefCell<BTreeMap<usize, Arc<crate::terminal::metadata::TerminalMetadataSnapshot>>>>,
    permission_requests:
        Rc<RefCell<BTreeMap<usize, crate::terminal::permission_request::PermissionRequest>>>,
    accessibility_senders:
        Rc<RefCell<BTreeMap<usize, async_channel::Sender<Arc<TerminalAccessibilityModel>>>>>,
    interactive_availability: Rc<RefCell<BTreeMap<usize, bool>>>,
    input_generations: Rc<RefCell<BTreeMap<usize, u64>>>,
    unconfirmed_pastes: Rc<RefCell<BTreeMap<usize, bool>>>,
    dropped_session_ids: Rc<RefCell<Vec<usize>>>,
    commands: Rc<RefCell<Vec<RecordedCall>>>,
}

impl TestTerminalSessionRecords {
    pub(crate) fn retain_unconfirmed_paste(&self, session_id: usize) {
        self.unconfirmed_pastes
            .borrow_mut()
            .insert(session_id, true);
    }

    pub(crate) fn advance_input_generation(&self, session_id: usize) {
        *self
            .input_generations
            .borrow_mut()
            .entry(session_id)
            .or_default() += 1;
    }

    pub(crate) fn set_interactive_input_available(&self, session_id: usize, available: bool) {
        self.interactive_availability
            .borrow_mut()
            .insert(session_id, available);
    }

    pub(crate) fn retain_permission_request(
        &self,
        session_id: usize,
        request: crate::terminal::permission_request::PermissionRequest,
    ) {
        self.permission_requests
            .borrow_mut()
            .insert(session_id, request);
    }

    pub(crate) fn report_directory(
        &self,
        session_id: usize,
        current: Option<crate::domain::CurrentDirectory>,
    ) {
        use crate::terminal::metadata::{
            MetadataFreshness, RemoteTerminalMetadataContext, TerminalMetadataContext,
        };
        self.report_metadata(session_id, |metadata| match current {
            Some(crate::domain::CurrentDirectory::Local(path)) => {
                let path = path.to_string_lossy();
                metadata.context = TerminalMetadataContext::local(
                    crate::local_path::LocalPathSemantics::Posix,
                    &path,
                    Default::default(),
                );
                metadata.directory.path = Arc::from(path.as_ref());
                metadata.freshness = MetadataFreshness::Live;
            }
            Some(crate::domain::CurrentDirectory::Remote(directory)) => {
                metadata.context =
                    TerminalMetadataContext::Remote(RemoteTerminalMetadataContext::new(
                        crate::domain::SshDestination::new("tester@remote".to_owned())
                            .expect("the test destination is valid"),
                        directory.clone(),
                    ));
                metadata.directory.path = Arc::from(directory.as_str());
                metadata.freshness = MetadataFreshness::Live;
            }
            None => metadata.freshness = MetadataFreshness::Stale,
        });
    }

    /// Retains the next Terminal Metadata revision for one Terminal Session, as a hidden one would.
    pub(crate) fn report_metadata(
        &self,
        session_id: usize,
        change: impl FnOnce(&mut crate::terminal::metadata::TerminalMetadataSnapshot),
    ) {
        let mut snapshots = self.metadata_snapshots.borrow_mut();
        let mut metadata = snapshots.get(&session_id).map_or_else(
            || {
                Arc::unwrap_or_clone(
                    crate::terminal::metadata::MetadataTracker::new(
                        crate::local_path::LocalPathSemantics::Posix,
                        "",
                        "",
                        Default::default(),
                        std::time::Instant::now(),
                    )
                    .snapshot(),
                )
            },
            |snapshot| (**snapshot).clone(),
        );
        metadata.revision += 1;
        change(&mut metadata);
        snapshots.insert(session_id, Arc::new(metadata));
        self.event_sender(session_id)
            .expect("session must be live")
            .try_send(crate::terminal::session::tests::metadata_changed())
            .expect("metadata event must fit");
    }

    pub(crate) fn queue_selection_copy(&self, copy: Option<SelectionCopy>) {
        self.selection_copies.borrow_mut().push_back(copy);
    }

    pub(crate) fn accessibility_selection_requests(
        &self,
        session_id: usize,
    ) -> Vec<crate::terminal::accessibility::AccessibilitySelectionRequest> {
        self.selection_receivers
            .borrow()
            .get(&session_id)
            .map_or_else(Vec::new, |receiver| receiver.drain())
    }

    pub(crate) fn starts(&self) -> Vec<RecordedStart> {
        self.starts.borrow().clone()
    }

    pub(crate) fn event_sender(
        &self,
        session_id: usize,
    ) -> Option<async_channel::Sender<TerminalSessionEvent>> {
        self.event_senders.borrow().get(&session_id).cloned()
    }

    pub(crate) fn last_event_sender(&self) -> Option<async_channel::Sender<TerminalSessionEvent>> {
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

    pub(crate) fn commands(&self) -> Vec<RecordedCall> {
        self.commands.borrow().clone()
    }

    pub(crate) fn pointer_count(&self) -> usize {
        self.commands
            .borrow()
            .iter()
            .filter(|input| {
                matches!(
                    input.command,
                    RecordedCommand::Pointer(_) | RecordedCommand::PointerAndCopySelection(_)
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
    pending_paste_response: Option<async_channel::Receiver<Result<PasteRequestOutcome, String>>>,
    paste_resolution: Result<PasteResolution, String>,
    pending_paste_resolution: Option<async_channel::Receiver<Result<PasteResolution, String>>>,
    server_backed: bool,
    client_files: Option<crate::platform::local_filesystem::LocalFilesystemAuthority>,
    interactive_input_available: bool,
    reading_available: bool,
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
            pending_paste_response: None,
            paste_resolution: Ok(PasteResolution::Written),
            pending_paste_resolution: None,
            server_backed: false,
            client_files: None,
            interactive_input_available: true,
            reading_available: true,
        }
    }

    /// Starts Terminal Sessions that report themselves as running in a SpaceTerm Server.
    pub(crate) fn server_backed(mut self) -> Self {
        self.server_backed = true;
        self.reading_available = false;
        self
    }

    pub(crate) fn with_client_filesystem(
        mut self,
        filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority,
    ) -> Self {
        self.client_files = Some(filesystem);
        self
    }

    /// Sessions a server opens for another machine's client, which grants no file authority.
    pub(crate) fn without_client_filesystem(mut self) -> Self {
        self.client_files = None;
        self
    }

    pub(crate) fn with_reading_available(mut self, available: bool) -> Self {
        self.reading_available = available;
        self
    }

    pub(crate) fn with_interactive_input_available(mut self, available: bool) -> Self {
        self.interactive_input_available = available;
        self
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

    pub(crate) fn with_pending_paste_response(
        mut self,
        receiver: async_channel::Receiver<Result<PasteRequestOutcome, String>>,
    ) -> Self {
        self.pending_paste_response = Some(receiver);
        self
    }

    pub(crate) fn with_pending_paste_resolution(
        mut self,
        receiver: async_channel::Receiver<Result<PasteResolution, String>>,
    ) -> Self {
        self.pending_paste_resolution = Some(receiver);
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
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        let session_id = self.next_session_id.get();
        self.next_session_id.set(session_id + 1);
        self.records.starts.borrow_mut().push(RecordedStart {
            session_id,
            geometry,
            launch_plan,
            initial_appearance,
        });

        if let Some(message) = &self.start_failure
            && self
                .start_failure_session_id
                .is_none_or(|failure_session_id| failure_session_id == session_id)
        {
            return Err(TerminalSessionError::EmulatorStartup(message.clone()));
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
            crate::terminal::AccessibilitySelectionSender::recording_channel();
        self.records
            .selection_receivers
            .borrow_mut()
            .insert(session_id, selection_receiver);
        let (_, clipboard) = async_channel::bounded(1);
        self.records
            .set_interactive_input_available(session_id, self.interactive_input_available);
        Ok(StartedTerminalSession {
            clipboard,
            handle: Box::new(TestTerminalSessionHandle {
                session_id,
                reading_available: self.reading_available,
                selection_sender,
                records: self.records.clone(),
                selection_response: self.selection_response.clone(),
                paste_response: self.paste_response.clone(),
                pending_paste_response: self.pending_paste_response.clone(),
                paste_resolution: self.paste_resolution.clone(),
                pending_paste_resolution: self.pending_paste_resolution.clone(),
            }),
            events,
            accessibility,
        })
    }

    fn fallback_title(&self) -> String {
        self.fallback_title.clone()
    }

    fn is_server_backed(&self) -> bool {
        self.server_backed
    }
    fn allows_local_file_insertion(&self) -> bool {
        self.server_backed
    }
    fn local_file_authority(
        &self,
    ) -> Option<&crate::platform::local_filesystem::LocalFilesystemAuthority> {
        self.client_files.as_ref()
    }
}

struct TestTerminalSessionHandle {
    reading_available: bool,
    session_id: usize,
    selection_sender: crate::terminal::AccessibilitySelectionSender,
    records: TestTerminalSessionRecords,
    selection_response: Result<Option<SelectionCopy>, SelectionCopyError>,
    paste_response: Result<PasteRequestOutcome, String>,
    pending_paste_response: Option<async_channel::Receiver<Result<PasteRequestOutcome, String>>>,
    paste_resolution: Result<PasteResolution, String>,
    pending_paste_resolution: Option<async_channel::Receiver<Result<PasteResolution, String>>>,
}

impl TestTerminalSessionHandle {
    fn record(&self, command: RecordedCommand) {
        self.records.commands.borrow_mut().push(RecordedCall {
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
    fn reading_available(&self) -> bool {
        self.reading_available
    }
    fn take_paste_unconfirmed(&self) -> bool {
        self.records
            .unconfirmed_pastes
            .borrow_mut()
            .remove(&self.session_id)
            .unwrap_or(false)
    }

    fn input_generation(&self) -> u64 {
        self.records
            .input_generations
            .borrow()
            .get(&self.session_id)
            .copied()
            .unwrap_or(0)
    }

    fn interactive_input_available(&self) -> bool {
        self.records
            .interactive_availability
            .borrow()
            .get(&self.session_id)
            .copied()
            .unwrap_or(true)
    }

    fn permission_request(&self) -> Option<crate::terminal::permission_request::PermissionRequest> {
        self.records
            .permission_requests
            .borrow()
            .get(&self.session_id)
            .cloned()
    }

    fn metadata_snapshot(
        &self,
    ) -> Option<Arc<crate::terminal::metadata::TerminalMetadataSnapshot>> {
        self.records
            .metadata_snapshots
            .borrow()
            .get(&self.session_id)
            .cloned()
    }
    fn accessibility_selection_sender(
        &self,
    ) -> Option<crate::terminal::AccessibilitySelectionSender> {
        Some(self.selection_sender.clone())
    }

    fn key(&self, input: KeyInput) {
        self.record(RecordedCommand::Key(input));
    }

    fn clear_screen_and_scrollback(&self) {
        self.record(RecordedCommand::ClearScreenAndScrollback);
    }

    fn focus(&self, focused: bool) {
        self.record(RecordedCommand::Focus(focused));
    }

    fn resize(&self, geometry: TerminalGeometry) {
        self.record(RecordedCommand::Resize(geometry));
    }

    fn pointer(&self, input: PointerInput) {
        self.record(RecordedCommand::Pointer(input));
    }

    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedCommand::PointerAndCopySelection(input));
        self.selection_response.clone()
    }

    fn wheel(&self, input: WheelInput) {
        self.record(RecordedCommand::Wheel(input));
    }

    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration) {
        self.record(RecordedCommand::ScrollTo(offset_rows, generation));
    }

    fn scroll_scrollback(&self, movement: ScrollbackMovement) {
        self.record(RecordedCommand::ScrollScrollback(movement));
    }

    fn set_find_query(&self, generation: FindQueryGeneration, query: String) {
        self.record(RecordedCommand::SetFindQuery(generation, query));
    }

    fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection) {
        self.record(RecordedCommand::NavigateFind(generation, direction));
    }

    fn end_find(&self, generation: FindQueryGeneration) {
        self.record(RecordedCommand::EndFind(generation));
    }

    fn request_paste(
        &self,
        text: crate::terminal::native_services::PastePayload,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>> {
        self.record(RecordedCommand::RequestPaste(text.into_text()));
        if let Some(receiver) = &self.pending_paste_response {
            return receiver.clone();
        }
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(self.paste_response.clone());
        receiver
    }

    fn resolve_paste(
        &self,
        id: PasteConfirmationId,
        decision: PasteDecision,
    ) -> async_channel::Receiver<Result<PasteResolution, String>> {
        self.record(RecordedCommand::ResolvePaste(id, decision));
        if let Some(receiver) = &self.pending_paste_resolution {
            return receiver.clone();
        }
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(self.paste_resolution.clone());
        receiver
    }

    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedCommand::RequestSelectionCopy);
        self.records
            .selection_copies
            .borrow_mut()
            .pop_front()
            .map_or_else(|| self.selection_response.clone(), Ok)
    }

    fn copy_or_forward(
        &self,
        modifiers: crate::terminal::InputModifiers,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedCommand::CopyOrForward(modifiers));
        self.copy_selection()
    }

    fn copy_selection_at(
        &self,
        generation: PresentationGeneration,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.record(RecordedCommand::RequestSelectionCopyAt(generation));
        self.selection_response.clone()
    }

    fn set_presentable(&self, presentable: bool) {
        self.record(RecordedCommand::SetPresentable(presentable));
    }

    fn update_appearance(&self, update: TerminalAppearanceUpdate) {
        self.record(RecordedCommand::UpdateAppearance(update));
    }
}
