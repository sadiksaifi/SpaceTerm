//! Worker-local engine and owning PTY ports. Neither port carries wire authority.
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<PtyExit, RuntimeFailure>;
}
/// Request process-session cleanup, not just child exit. The worker joins this call.
pub trait PtyTermination: Send + Sync {
    fn request_termination(&self) -> Result<(), RuntimeFailure>;
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
/// Shared mechanical cleanup. The caller disconnects presentation and clears
/// engine content first. This function has no organization or server identity.
pub(crate) fn cleanup_pty<T: Send + 'static>(
    process: &mut Option<Box<dyn PtyProcess>>,
    termination: &mut Option<Arc<dyn PtyTermination>>,
    reader: &mut Option<PtyReaderPump<T>>,
) -> spaceterm_core::retirement::CleanupResult {
    use spaceterm_core::retirement::{CleanupResult, ConfirmedCleanupClass};
    if let Some(reader) = reader {
        reader.disconnect();
    }
    let had_process = process.is_some();
    let mut confirmed = true;
    if let Some(termination) = termination.take() {
        let request = Arc::clone(&termination);
        match std::thread::Builder::new()
            .name("terminal-termination".into())
            .spawn(move || request.request_termination())
        {
            Ok(thread) => confirmed &= matches!(thread.join(), Ok(Ok(()))),
            Err(_) => confirmed &= termination.request_termination().is_ok(),
        }
    }
    if let Some(process) = process.as_deref_mut() {
        confirmed &= matches!(
            process.wait_for_exit(Duration::from_secs(2)),
            Ok(PtyExit::Exited { .. })
        );
    }
    *process = None;
    if let Some(reader) = reader {
        confirmed &= reader.join().is_ok();
    }
    *reader = None;
    if confirmed {
        CleanupResult::Confirmed(if had_process {
            ConfirmedCleanupClass::ReapedAndReleased
        } else {
            ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership
        })
    } else {
        CleanupResult::Failed(FailureClass::RuntimeUnavailable)
    }
}
