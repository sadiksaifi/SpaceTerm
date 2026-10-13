//! Worker-local engine and owning PTY ports. Neither port carries wire authority.
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::runtime_contracts::{RuntimeAppearance, RuntimeCommand, RuntimeMetadataSnapshot};
use spaceterm_core::authority::{ContentRevision, SessionKey};
use spaceterm_core::content_queries::*;
use spaceterm_core::geometry::MeasuredGrid;
use spaceterm_core::observations::*;
use spaceterm_core::organization::FailureClass;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStage {
    Admission,
    Launch,
    Engine,
    Reader,
    Input,
    Resize,
    Authority,
    Content,
    NativeRequest,
    Cleanup,
    Counter,
}
/// A closed classification. Native adapters must discard raw error payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("runtime {stage:?} failed ({class:?})")]
pub struct RuntimeFailure {
    pub class: FailureClass,
    pub stage: RuntimeStage,
}
impl RuntimeFailure {
    pub const fn new(class: FailureClass, stage: RuntimeStage) -> Self {
        Self { class, stage }
    }
    pub(crate) const fn capacity(stage: RuntimeStage) -> Self {
        Self::new(FailureClass::ResourceLimit, stage)
    }
    pub(crate) const fn stale() -> Self {
        Self::new(FailureClass::StaleAuthority, RuntimeStage::Authority)
    }
    pub(crate) const fn internal(stage: RuntimeStage) -> Self {
        Self::new(FailureClass::Internal, stage)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
    pub rows: u16,
    pub columns: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}
impl From<MeasuredGrid> for PtySize {
    fn from(grid: MeasuredGrid) -> Self {
        let (pixel_width, pixel_height) = grid.pty_pixels();
        Self {
            rows: grid.rows(),
            columns: grid.columns(),
            pixel_width,
            pixel_height,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyExit {
    Exited { status: Option<i32> },
    StillRunning,
}
/// Writes and flushes must be nonblocking. Termination must release a blocked reader.
pub trait PtyProcess: Write + Send {
    fn take_reader(&mut self) -> Result<Box<dyn Read + Send>, RuntimeFailure>;
    fn resize(&self, size: PtySize) -> Result<(), RuntimeFailure>;
    fn hidden_input(&self) -> Result<bool, RuntimeFailure>;
    /// Respect the timeout. A zero timeout is a nonblocking poll, and an ordinary
    /// timeout returns `StillRunning`, not a mechanical failure.
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<PtyExit, RuntimeFailure>;
}
/// Request process-session cleanup, not just child exit. Cleanup retains and joins this call.
pub trait PtyTermination: Send + Sync {
    fn request_termination(&self) -> Result<(), RuntimeFailure>;
    /// Re-prove cleanup of the original process session without requesting termination again.
    /// Implementations must bound each attempt and retain the original session identity.
    /// Only `Ok(())` certifies absence. Unsupported or inconclusive proof fails closed.
    fn retry_cleanup(&self) -> Result<(), RuntimeFailure> {
        Err(RuntimeFailure::new(
            FailureClass::RuntimeUnavailable,
            RuntimeStage::Cleanup,
        ))
    }
}
pub struct PtyParts {
    pub process: Box<dyn PtyProcess>,
    pub termination: Arc<dyn PtyTermination>,
}
/// An error certifies that no process remains owned by the adapter. If cleanup
/// cannot be confirmed, the adapter must return RuntimeStage::Cleanup instead.
/// Unwinding across this boundary never certifies process absence.
pub type PreparedRuntimeLaunch =
    Box<dyn FnOnce(PtySize) -> Result<PtyParts, RuntimeFailure> + Send>;

/// Original producer observations. The runtime preserves their ticks and separate watermarks.
#[derive(Default)]
pub struct EngineObservations {
    pub metadata: Option<RuntimeMetadataSnapshot>,
    pub display_directory: Option<DisplayDirectoryObservation>,
    pub confirmed_directory: Option<ConfirmedDirectoryObservation>,
    pub close_work: Option<CloseWorkObservation>,
}
pub struct EngineUpdate {
    pub replies: Vec<u8>,
    pub changed: bool,
    pub observations: EngineObservations,
}
pub struct EngineEffect {
    pub bytes: Vec<u8>,
    pub changed: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct ContentWorkBudget {
    pub rows: u16,
    pub cells: u32,
    pub bytes: usize,
}
impl Default for ContentWorkBudget {
    fn default() -> Self {
        Self {
            rows: 256,
            cells: 16_384,
            bytes: 256 * 1024,
        }
    }
}
/// Engine-owned identity. It is never a protocol request or native authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineJobId(pub u64);
pub enum ResourceRequest {
    Copy(CopyRequest),
    Find(FindRequest),
    Graphics(GraphicsRequest),
}
impl ResourceRequest {
    pub fn stamp(&self) -> QueryStamp {
        match self {
            Self::Copy(request) => request.stamp(),
            Self::Find(request) => request.stamp(),
            Self::Graphics(request) => request.stamp(),
        }
    }
}
pub enum ResourceItem {
    Copy(Box<CopyStreamItem>),
    Find(Box<FindStreamItem>),
    Graphics(Box<GraphicsChunk>),
}
pub enum ResourceStep {
    Pending,
    Item(ResourceItem),
    Complete,
}
/// The engine is constructed and used only on the worker thread. Content methods
/// fail closed until a semantic-content provider implements them. Unsupported is
/// never converted to a successful empty page or a completed partial transfer.
pub trait RuntimeEngine {
    fn feed(&mut self, bytes: &[u8]) -> Result<EngineUpdate, RuntimeFailure>;
    fn apply(&mut self, command: &RuntimeCommand) -> Result<EngineEffect, RuntimeFailure>;
    fn resize(&mut self, grid: MeasuredGrid) -> Result<(), RuntimeFailure>;
    fn release_input(&mut self) -> Result<Vec<u8>, RuntimeFailure>;
    fn content_base(&self) -> spaceterm_core::content::ContentBase;
    fn content_revision(&self) -> ContentRevision {
        self.content_base().revision
    }
    fn bracketed_paste(&self) -> Result<bool, RuntimeFailure>;
    /// Native hidden-input state comes from the owned PTY, not shell markers.
    /// Metadata and accessibility producers must use this observation.
    fn poll(&mut self, _hidden_input: bool) -> Result<EngineUpdate, RuntimeFailure> {
        Ok(EngineUpdate {
            replies: Vec::new(),
            changed: false,
            observations: EngineObservations::default(),
        })
    }
    fn read_page(&mut self, _: &ContentPageRequest) -> Result<ContentPageResult, RuntimeFailure> {
        Err(unsupported_content())
    }
    fn read_topology(
        &mut self,
        _: &TopologyPageRequest,
    ) -> Result<TopologyPageResult, RuntimeFailure> {
        Err(unsupported_content())
    }
    fn begin_resource(
        &mut self,
        _: &ResourceRequest,
        _: &super::budget::BudgetLease,
    ) -> Result<EngineJobId, RuntimeFailure> {
        Err(unsupported_content())
    }
    fn step_resource(
        &mut self,
        _: EngineJobId,
        _: ContentWorkBudget,
    ) -> Result<ResourceStep, RuntimeFailure> {
        Err(unsupported_content())
    }
    fn cancel_resource(&mut self, _: EngineJobId) {}
}
fn unsupported_content() -> RuntimeFailure {
    RuntimeFailure::new(FailureClass::Unsupported, RuntimeStage::Content)
}
pub struct EngineStartContext {
    pub session: SessionKey,
    pub grid: MeasuredGrid,
    pub directory: spaceterm_core::organization::ServerDirectory,
    pub appearance: RuntimeAppearance,
    pub clock: Arc<dyn RuntimeClock>,
}
/// The closure may capture native engine context, but creates no engine before
/// geometry admission. Its returned engine deliberately does not require Send.
type PreparedEngineLaunch =
    Box<dyn FnOnce(EngineStartContext) -> Result<Box<dyn RuntimeEngine>, RuntimeFailure> + Send>;
pub struct EngineLaunchContext(pub(crate) PreparedEngineLaunch);
impl EngineLaunchContext {
    pub fn new(
        factory: impl FnOnce(EngineStartContext) -> Result<Box<dyn RuntimeEngine>, RuntimeFailure>
        + Send
        + 'static,
    ) -> Self {
        Self(Box::new(factory))
    }
}

/// Identity-free reader ownership shared by server and legacy workers. Receipt
/// capture is injected; neither legacy launch nor cleanup needs a synthetic Session.
pub(crate) enum PtyReadEvent<T> {
    Bytes {
        bytes: Vec<u8>,
        receipt: T,
        sequence: u64,
    },
    Finished,
}
pub(crate) struct PtyReaderPump<T> {
    output: Option<std::sync::mpsc::Receiver<PtyReadEvent<T>>>,
    high_water: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<Result<(), RuntimeFailure>>>,
    joined: Option<Result<Result<(), RuntimeFailure>, RuntimeFailure>>,
}
impl<T: Send + 'static> PtyReaderPump<T> {
    pub(crate) fn spawn(
        mut reader: Box<dyn Read + Send>,
        capture: impl Fn() -> Result<T, RuntimeFailure> + Send + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self, RuntimeFailure> {
        let (sender, output) = std::sync::mpsc::sync_channel(8);
        let high_water = Arc::new(AtomicU64::new(0));
        let publish = Arc::clone(&high_water);
        let thread = std::thread::Builder::new()
            .name("terminal-reader".into())
            .spawn(move || {
                let mut buffer = [0u8; 16 * 1024];
                let mut sequence = 0u64;
                loop {
                    let count = match reader.read(&mut buffer) {
                        Ok(0) => {
                            // EOF and read failure also belong to the finite barrier.
                            publish.store(
                                sequence
                                    .checked_add(1)
                                    .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?,
                                Ordering::Release,
                            );
                            let _ = sender.send(PtyReadEvent::Finished);
                            notify();
                            return Ok(());
                        }
                        Ok(count) => count,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            // EOF and read failure also belong to the finite barrier.
                            publish.store(
                                sequence
                                    .checked_add(1)
                                    .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?,
                                Ordering::Release,
                            );
                            let _ = sender.send(PtyReadEvent::Finished);
                            notify();
                            return Err(RuntimeFailure::new(
                                FailureClass::RuntimeFault,
                                RuntimeStage::Reader,
                            ));
                        }
                    };
                    let receipt = capture()?;
                    sequence = sequence
                        .checked_add(1)
                        .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?;
                    let event = PtyReadEvent::Bytes {
                        bytes: buffer[..count].to_vec(),
                        receipt,
                        sequence,
                    };
                    // Include the one bounded chunk waiting to enter the lane.
                    // Later reads cannot extend a worker's captured barrier.
                    publish.store(sequence, Ordering::Release);
                    if sender.send(event).is_err() {
                        return Ok(());
                    }
                    notify();
                }
            })
            .map_err(|_| {
                RuntimeFailure::new(FailureClass::RuntimeUnavailable, RuntimeStage::Reader)
            })?;
        Ok(Self {
            output: Some(output),
            high_water,
            thread: Some(thread),
            joined: None,
        })
    }
    pub(crate) fn high_water_mark(&self) -> u64 {
        self.high_water.load(Ordering::Acquire)
    }
    pub(crate) fn try_next(&self) -> Result<Option<PtyReadEvent<T>>, RuntimeFailure> {
        let Some(output) = &self.output else {
            return Ok(None);
        };
        match output.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(RuntimeFailure::new(
                FailureClass::RuntimeFault,
                RuntimeStage::Reader,
            )),
        }
    }
    pub(crate) fn disconnect(&mut self) {
        self.output = None;
    }
    fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
    /// The outer result proves the thread joined. A read failure is a terminal
    /// failure, not failed cleanup of an already joined reader.
    pub(crate) fn join(&mut self) -> Result<Result<(), RuntimeFailure>, RuntimeFailure> {
        if let Some(thread) = self.thread.take() {
            self.joined = Some(thread.join().map_err(|_| {
                RuntimeFailure::new(FailureClass::RuntimeFault, RuntimeStage::Reader)
            }));
        }
        self.joined.unwrap_or(Ok(Ok(())))
    }
}
/// Type erasure removes receipt payloads from retained reader ownership.
trait PendingReader: Send {
    fn try_join(&mut self) -> Option<Result<Result<(), RuntimeFailure>, RuntimeFailure>>;
}
impl<T: Send + 'static> PendingReader for PtyReaderPump<T> {
    fn try_join(&mut self) -> Option<Result<Result<(), RuntimeFailure>, RuntimeFailure>> {
        self.is_finished().then(|| self.join())
    }
}
/// Original resources that could not mechanically finish within foreground cleanup.
/// Dropping this value detaches an unfinished reader; it never joins that reader.
pub(crate) struct PendingPtyCleanup {
    process: Option<Box<dyn PtyProcess>>,
    child_exited: bool,
    termination: Option<Arc<dyn PtyTermination>>,
    reader: Option<Box<dyn PendingReader>>,
    request: Option<std::thread::JoinHandle<Result<(), RuntimeFailure>>>,
    absence_confirmed: bool,
    mechanical_failed: bool,
}
impl PendingPtyCleanup {
    fn poll_request(&mut self) {
        if self
            .request
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            let Some(request) = self.request.take() else {
                return;
            };
            match request.join() {
                Ok(result) => self.absence_confirmed = result.is_ok(),
                Err(_) => self.mechanical_failed = true,
            }
        }
    }
    fn poll_child(&mut self, timeout: Duration) {
        if self.child_exited {
            return;
        }
        let Some(process) = &mut self.process else {
            return;
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process.wait_for_exit(timeout)
        })) {
            // Retain the exited adapter: its destructor can run native proof.
            // Disposal is safe only after a scheduled positive receipt and joins.
            Ok(Ok(PtyExit::Exited { .. })) => self.child_exited = true,
            Ok(Ok(PtyExit::StillRunning)) => {}
            Ok(Err(_)) | Err(_) => self.mechanical_failed = true,
        }
    }
    fn poll_reader(&mut self) {
        let Some(reader) = &mut self.reader else {
            return;
        };
        if let Some(result) = reader.try_join() {
            // A joined reader I/O error is a terminal fault, not unconfirmed mechanics.
            self.mechanical_failed |= result.is_err();
            self.reader = None;
        }
    }
    pub fn poll_mechanics(&mut self) {
        self.poll_request();
        if self.request.is_some() {
            return;
        }
        if !self.mechanical_failed {
            self.poll_child(Duration::ZERO);
        }
        self.poll_reader();
    }
    pub fn can_release(&self) -> bool {
        self.absence_confirmed
            && self.request.is_none()
            && !self.mechanical_failed
            && self.child_exited
            && self.reader.is_none()
    }
    pub fn can_retry(&self) -> bool {
        !self.absence_confirmed
            && self.request.is_none()
            && !self.mechanical_failed
            && self.termination.is_some()
    }
    pub fn fail_mechanics(&mut self) {
        self.mechanical_failed = true;
    }
    pub fn retry(&mut self) {
        if !self.can_retry() {
            return;
        }
        let Some(termination) = &self.termination else {
            return;
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| termination.retry_cleanup()))
        {
            Ok(Ok(())) => self.absence_confirmed = true,
            Ok(Err(_)) => {}
            Err(_) => self.mechanical_failed = true,
        }
    }
}
/// Mechanical cleanup outcome plus explicit retained resources after incomplete cleanup.
pub(crate) struct PtyCleanup {
    pub result: spaceterm_core::retirement::CleanupResult,
    pub pending: Option<PendingPtyCleanup>,
}
/// Shared bounded foreground cleanup. Neither a reader disconnection nor a
/// native certificate substitutes for the actual child exit and reader join.
pub(crate) fn cleanup_pty<T: Send + 'static>(
    process: &mut Option<Box<dyn PtyProcess>>,
    termination: &mut Option<Arc<dyn PtyTermination>>,
    reader: &mut Option<PtyReaderPump<T>>,
) -> PtyCleanup {
    use spaceterm_core::retirement::{CleanupResult, ConfirmedCleanupClass};
    let request_deadline = Instant::now() + Duration::from_secs(2);
    if let Some(reader) = reader {
        reader.disconnect();
    }
    let had_process = process.is_some();
    let mut pending = PendingPtyCleanup {
        process: process.take(),
        child_exited: !had_process,
        termination: termination.take(),
        reader: reader
            .take()
            .map(|reader| Box::new(reader) as Box<dyn PendingReader>),
        request: None,
        absence_confirmed: true,
        mechanical_failed: false,
    };
    if let Some(termination) = &pending.termination {
        pending.absence_confirmed = false;
        let request = Arc::clone(termination);
        match std::thread::Builder::new()
            .name("terminal-termination".into())
            .spawn(move || request.request_termination())
        {
            Ok(thread) => pending.request = Some(thread),
            // Running the callback inline would remove the foreground bound.
            Err(_) => pending.mechanical_failed = true,
        }
        while pending.request.is_some() && Instant::now() < request_deadline {
            pending.poll_request();
            if pending.request.is_some() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        pending.poll_request();
    }
    if pending.request.is_none() && !pending.mechanical_failed {
        let mechanical_deadline = Instant::now() + Duration::from_secs(2);
        // Keep the adapter's established two-second foreground wait. Deferred
        // child polls are zero-time only after the initial request has joined.
        pending.poll_child(Duration::from_secs(2));
        pending.poll_reader();
        // A still-running child needs fresh native settlement, not a blocked reader join.
        while pending.child_exited
            && pending.reader.is_some()
            && !pending.mechanical_failed
            && Instant::now() < mechanical_deadline
        {
            std::thread::sleep(Duration::from_millis(1));
            pending.poll_reader();
        }
    }
    if pending.can_release() {
        PtyCleanup {
            result: CleanupResult::Confirmed(if had_process {
                ConfirmedCleanupClass::ReapedAndReleased
            } else {
                ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership
            }),
            pending: None,
        }
    } else {
        PtyCleanup {
            result: CleanupResult::Failed(FailureClass::RuntimeUnavailable),
            pending: Some(pending),
        }
    }
}
