use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use crate::terminal::geometry::TerminalGeometry;
use crate::terminal::{
    FindDirection, FindQueryGeneration, KeyInput, PasteConfirmationId, PasteDecision,
    PasteRequestOutcome, PasteResolution, PointerInput, PresentationGeneration, ScrollbackMovement,
    SelectionCopy, SelectionCopyError, StartedTerminalSession, TerminalAccessibilityModel,
    TerminalAppearanceUpdate, TerminalSessionError, TerminalSessionEvent, TerminalSessionFactory,
    TerminalSessionHandle, WheelInput,
};

#[cfg(feature = "desktop")]
pub(crate) use crate::terminal::emulator::TerminalEmulator;
#[cfg(feature = "desktop")]
pub(crate) use crate::terminal::graphics::test_lock as graphics_test_lock;

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
    pub(crate) initial_appearance: TerminalAppearanceUpdate,
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
    SetPreparationDemand(bool),
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
    accessibility_senders:
        Rc<RefCell<BTreeMap<usize, async_channel::Sender<Arc<TerminalAccessibilityModel>>>>>,
    interactive_availability: Rc<RefCell<BTreeMap<usize, crate::terminal::InteractiveInput>>>,
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

    /// The connection accepts interactive terminal input, or has not yet.
    pub(crate) fn set_interactive_input_available(&self, session_id: usize, available: bool) {
        self.set_interactive_input(
            session_id,
            if available {
                crate::terminal::InteractiveInput::Accepted
            } else {
                crate::terminal::InteractiveInput::Pending
            },
        );
    }

    pub(crate) fn set_interactive_input(
        &self,
        session_id: usize,
        input: crate::terminal::InteractiveInput,
    ) {
        self.interactive_availability
            .borrow_mut()
            .insert(session_id, input);
    }

    pub(crate) fn queue_selection_copy(&self, copy: Option<SelectionCopy>) {
        self.selection_copies.borrow_mut().push_back(copy);
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
    selection_response: Result<Option<SelectionCopy>, SelectionCopyError>,
    paste_response: Result<PasteRequestOutcome, String>,
    pending_paste_response: Option<async_channel::Receiver<Result<PasteRequestOutcome, String>>>,
    paste_resolution: Result<PasteResolution, String>,
    pending_paste_resolution: Option<async_channel::Receiver<Result<PasteResolution, String>>>,
    client_files: Option<crate::platform::local_filesystem::LocalFilesystemAuthority>,
    interactive_input: crate::terminal::InteractiveInput,
    reading_available: bool,
}

impl TestTerminalSessionFactory {
    pub(crate) fn new(records: TestTerminalSessionRecords) -> Self {
        Self {
            records,
            next_session_id: Cell::new(1),
            fallback_title: "Terminal".to_owned(),
            start_failure: None,
            selection_response: Ok(None),
            paste_response: Ok(PasteRequestOutcome::Written),
            pending_paste_response: None,
            paste_resolution: Ok(PasteResolution::Written),
            pending_paste_resolution: None,
            client_files: None,
            interactive_input: crate::terminal::InteractiveInput::Accepted,
            reading_available: true,
        }
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

    /// Starts Terminal Sessions whose connection accepts interactive input, or has not yet.
    pub(crate) fn with_interactive_input_available(self, available: bool) -> Self {
        self.with_interactive_input(if available {
            crate::terminal::InteractiveInput::Accepted
        } else {
            crate::terminal::InteractiveInput::Pending
        })
    }

    pub(crate) fn with_interactive_input(
        mut self,
        input: crate::terminal::InteractiveInput,
    ) -> Self {
        self.interactive_input = input;
        self
    }

    pub(crate) fn with_fallback_title(mut self, title: impl Into<String>) -> Self {
        self.fallback_title = title.into();
        self
    }

    pub(crate) fn with_start_failure(mut self, message: impl Into<String>) -> Self {
        self.start_failure = Some(message.into());
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
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        let session_id = self.next_session_id.get();
        self.next_session_id.set(session_id + 1);
        self.records.starts.borrow_mut().push(RecordedStart {
            session_id,
            geometry,
            initial_appearance,
        });

        if let Some(message) = &self.start_failure {
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
            .set_interactive_input(session_id, self.interactive_input);
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

    fn allows_local_file_insertion(&self) -> bool {
        true
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

    fn interactive_input(&self) -> crate::terminal::InteractiveInput {
        self.records
            .interactive_availability
            .borrow()
            .get(&self.session_id)
            .copied()
            .unwrap_or(crate::terminal::InteractiveInput::Accepted)
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

    fn set_preparation_demand(&self, prepare: bool) {
        self.record(RecordedCommand::SetPreparationDemand(prepare));
    }

    fn update_appearance(&self, update: TerminalAppearanceUpdate) {
        self.record(RecordedCommand::UpdateAppearance(update));
    }
}
