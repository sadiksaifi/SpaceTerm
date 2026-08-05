use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::{
    KeyInput, PointerInput, SessionError, SessionEvent, StartedTerminalSession,
    TerminalSessionFactory, TerminalSessionHandle, WheelInput,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordedSessionStart {
    pub(crate) session_id: usize,
    pub(crate) geometry: TerminalGeometry,
    pub(crate) working_directory: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecordedSessionCommand {
    Key(KeyInput),
    Resize(TerminalGeometry),
    Pointer(PointerInput),
    Wheel(WheelInput),
    ScrollTo(u64),
    Paste(String),
    RequestSelectionText,
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
    selection_response: Result<Option<String>, String>,
}

impl TestTerminalSessionFactory {
    pub(crate) fn new(records: TestTerminalSessionRecords) -> Self {
        Self {
            records,
            next_session_id: Cell::new(1),
            fallback_title: "Terminal".to_owned(),
            start_failure: None,
            selection_response: Ok(None),
        }
    }

    pub(crate) fn with_fallback_title(mut self, title: impl Into<String>) -> Self {
        self.fallback_title = title.into();
        self
    }

    pub(crate) fn with_start_failure(mut self, message: impl Into<String>) -> Self {
        self.start_failure = Some(message.into());
        self
    }

    pub(crate) fn with_selection_response(
        mut self,
        response: Result<Option<String>, String>,
    ) -> Self {
        self.selection_response = response;
        self
    }
}

impl TerminalSessionFactory for TestTerminalSessionFactory {
    fn start(
        &self,
        geometry: TerminalGeometry,
        working_directory: &Path,
    ) -> Result<StartedTerminalSession, SessionError> {
        let session_id = self.next_session_id.get();
        self.next_session_id.set(session_id + 1);
        self.records.starts.borrow_mut().push(RecordedSessionStart {
            session_id,
            geometry,
            working_directory: working_directory.to_path_buf(),
        });

        if let Some(message) = &self.start_failure {
            return Err(SessionError::EmulatorStartup(message.clone()));
        }

        let (event_sender, events) = async_channel::unbounded();
        self.records
            .event_senders
            .borrow_mut()
            .insert(session_id, event_sender);

        Ok(StartedTerminalSession {
            handle: Box::new(TestTerminalSessionHandle {
                session_id,
                records: self.records.clone(),
                selection_response: self.selection_response.clone(),
            }),
            events,
        })
    }

    fn fallback_title(&self) -> String {
        self.fallback_title.clone()
    }
}

struct TestTerminalSessionHandle {
    session_id: usize,
    records: TestTerminalSessionRecords,
    selection_response: Result<Option<String>, String>,
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

    fn resize(&self, geometry: TerminalGeometry) {
        self.record(RecordedSessionCommand::Resize(geometry));
    }

    fn pointer(&self, input: PointerInput) {
        self.record(RecordedSessionCommand::Pointer(input));
    }

    fn wheel(&self, input: WheelInput) {
        self.record(RecordedSessionCommand::Wheel(input));
    }

    fn scroll_to(&self, offset_rows: u64) {
        self.record(RecordedSessionCommand::ScrollTo(offset_rows));
    }

    fn paste(&self, text: String) {
        self.record(RecordedSessionCommand::Paste(text));
    }

    fn request_selection_text(&self) -> async_channel::Receiver<Result<Option<String>, String>> {
        self.record(RecordedSessionCommand::RequestSelectionText);
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(self.selection_response.clone());
        receiver
    }
}
