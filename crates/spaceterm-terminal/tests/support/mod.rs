#![allow(dead_code)]
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use spaceterm_core::{
    authority::*, control::*, geometry::*, identity::*, observations::*, operations::*,
    organization::*,
};
use spaceterm_terminal::appearance::{Appearance, AppearanceGeneration};
use spaceterm_terminal::runtime::{
    budget::RuntimeLimits,
    owner::{RuntimeFactory, RuntimeOwner},
    ports::*,
};
use spaceterm_terminal::runtime_contracts::*;

#[derive(Default)]
pub struct Clock(pub AtomicU64);
impl RuntimeClock for Clock {
    fn now(&self) -> Result<MonotonicTick, ObservationError> {
        Ok(MonotonicTick::new(self.0.load(Ordering::Acquire)))
    }
}
#[derive(Default)]
pub struct State {
    pub output: VecDeque<u8>,
    pub output_chunks: VecDeque<VecDeque<u8>>,
    pub reader_waiting: bool,
    pub continuous_output: Option<Vec<u8>>,
    pub continuous_reads: usize,
    pub written: Vec<u8>,
    pub attempted_write: Vec<u8>,
    pub blocked: bool,
    pub write_limit: Option<usize>,
    pub eof: bool,
    pub terminated: usize,
    pub wait_timeout: bool,
    pub reader_dropped: bool,
    pub process_dropped: bool,
    pub sizes: Vec<PtySize>,
    pub launch_count: usize,
    pub resize_failure: bool,
    pub hidden: bool,
    pub reader_failure: bool,
    pub read_io_failure: bool,
    pub reply_bytes: Option<usize>,
    pub termination_blocked: bool,
    pub termination_completed: bool,
    pub engine_bytes: Vec<u8>,
    pub content_reads: usize,
    pub content_steps: usize,
    pub engine_dropped: bool,
    pub input_commands: usize,
    pub expand_input: bool,
    pub appearance_generations: Vec<u64>,
    pub poll_count: usize,
    pub poll_block_at: Option<usize>,
    pub wheel_bytes: Option<usize>,
    pub wheel_steps: Vec<(i32, i32)>,
}
#[derive(Clone, Default)]
pub struct Pty(pub Arc<(Mutex<State>, Condvar)>);
impl Pty {
    pub fn update(&self, update: impl FnOnce(&mut State)) {
        update(&mut self.0.0.lock().unwrap());
        self.0.1.notify_all();
    }
    pub fn inspect<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(&self.0.0.lock().unwrap())
    }
    pub fn output(&self, bytes: &[u8]) {
        self.update(|state| state.output.extend(bytes));
    }
    /// Each chunk ends a reader call. Waiting for `reader_waiting` with empty
    /// output queues proves the pump published every chunk before its next read.
    pub fn output_chunks(&self, chunks: &[&[u8]]) {
        self.update(|state| {
            state
                .output_chunks
                .extend(chunks.iter().map(|bytes| bytes.iter().copied().collect()));
        });
    }
    pub fn until(&self, predicate: impl Fn(&State) -> bool) {
        let end = Instant::now() + Duration::from_secs(3);
        let mut state = self.0.0.lock().unwrap();
        while !predicate(&state) {
            let Some(remaining) = end.checked_duration_since(Instant::now()) else {
                drop(state);
                panic!("PTY barrier timed out");
            };
            state = self.0.1.wait_timeout(state, remaining).unwrap().0;
        }
    }
    pub fn launch(&self) -> PreparedRuntimeLaunch {
        let pty = self.clone();
        Box::new(move |size| {
            pty.update(|state| {
                state.launch_count += 1;
                state.sizes.push(size);
            });
            Ok(PtyParts {
                process: Box::new(Process(pty.clone())),
                termination: Arc::new(Termination(pty)),
            })
        })
    }
}
struct Process(Pty);
struct Reader(Pty);
struct Termination(Pty);
impl Write for Process {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.0.0.lock().unwrap();
        state.attempted_write.clear();
        state.attempted_write.extend_from_slice(bytes);
        if state.blocked {
            self.0.0.1.notify_all();
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = state
            .write_limit
            .map_or(bytes.len(), |limit| limit.min(bytes.len()));
        state.written.extend_from_slice(&bytes[..count]);
        if state.write_limit.is_some() {
            state.blocked = true;
        }
        self.0.0.1.notify_all();
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl PtyProcess for Process {
    fn take_reader(&mut self) -> Result<Box<dyn Read + Send>, RuntimeFailure> {
        if self.0.inspect(|state| state.reader_failure) {
            return Err(RuntimeFailure::new(
                FailureClass::RuntimeUnavailable,
                RuntimeStage::Reader,
            ));
        }
        Ok(Box::new(Reader(self.0.clone())))
    }
    fn resize(&self, size: PtySize) -> Result<(), RuntimeFailure> {
        if self.0.inspect(|state| state.resize_failure) {
            return Err(RuntimeFailure::new(
                FailureClass::RuntimeFault,
                RuntimeStage::Resize,
            ));
        }
        self.0.update(|state| state.sizes.push(size));
        Ok(())
    }
    fn hidden_input(&self) -> Result<bool, RuntimeFailure> {
        Ok(self.0.inspect(|state| state.hidden))
    }
    fn wait_for_exit(&mut self, _: Duration) -> Result<PtyExit, RuntimeFailure> {
        Ok(if self.0.inspect(|state| state.wait_timeout) {
            PtyExit::StillRunning
        } else {
            PtyExit::Exited { status: Some(0) }
        })
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.0.update(|state| state.process_dropped = true);
    }
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut state = self.0.0.0.lock().unwrap();
        while state.output.is_empty()
            && state.output_chunks.is_empty()
            && state.continuous_output.is_none()
            && !state.eof
            && !state.read_io_failure
        {
            state.reader_waiting = true;
            self.0.0.1.notify_all();
            state = self.0.0.1.wait(state).unwrap();
        }
        state.reader_waiting = false;
        if state.read_io_failure {
            return Err(io::Error::other("native-error-sensitive-sentinel"));
        }
        if state.output.is_empty()
            && state.output_chunks.is_empty()
            && !state.eof
            && let Some(output) = &state.continuous_output
        {
            // Refill on every read without waiting for the worker to consume it.
            // The real pump's bounded lane supplies backpressure for this source.
            assert!(!output.is_empty() && output.len() <= bytes.len());
            let count = output.len();
            bytes[..count].copy_from_slice(output);
            state.continuous_reads += 1;
            self.0.0.1.notify_all();
            return Ok(count);
        }
        let output = if state.output_chunks.is_empty() {
            &mut state.output
        } else {
            state.output_chunks.front_mut().unwrap()
        };
        let count = bytes.len().min(output.len());
        for byte in &mut bytes[..count] {
            *byte = output.pop_front().unwrap();
        }
        if state.output_chunks.front().is_some_and(VecDeque::is_empty) {
            state.output_chunks.pop_front();
        }
        Ok(count)
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.update(|state| state.reader_dropped = true);
    }
}
impl PtyTermination for Termination {
    fn request_termination(&self) -> Result<(), RuntimeFailure> {
        let mut state = self.0.0.0.lock().unwrap();
        state.terminated += 1;
        self.0.0.1.notify_all();
        while state.termination_blocked {
            state = self.0.0.1.wait(state).unwrap();
        }
        state.eof = true;
        state.termination_completed = true;
        self.0.0.1.notify_all();
        Ok(())
    }
}
struct Engine {
    session: SessionKey,
    revision: ContentRevision,
    pty: Pty,
    jobs: std::collections::BTreeMap<u64, (spaceterm_core::content_queries::FindRequest, u64)>,
    next: u64,
    observations: Option<EngineObservations>,
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.pty.update(|state| state.engine_dropped = true);
    }
}
impl RuntimeEngine for Engine {
    fn feed(&mut self, bytes: &[u8]) -> Result<EngineUpdate, RuntimeFailure> {
        self.pty
            .update(|state| state.engine_bytes.extend_from_slice(bytes));
        if bytes == b"engine-failure" {
            return Err(RuntimeFailure::new(
                FailureClass::ResourceLimit,
                RuntimeStage::Engine,
            ));
        }
        self.revision = self.revision.checked_successor().unwrap();
        Ok(EngineUpdate {
            replies: if bytes.ends_with(b"query") {
                self.pty.inspect(|state| {
                    state
                        .reply_bytes
                        .map_or_else(|| b"reply".to_vec(), |bytes| vec![b'r'; bytes])
                })
            } else {
                Vec::new()
            },
            changed: !bytes.is_empty(),
            observations: EngineObservations::default(),
        })
    }
    fn apply(&mut self, command: &RuntimeCommand) -> Result<EngineEffect, RuntimeFailure> {
        if let RuntimeCommand::Appearance(value) = command {
            self.pty
                .update(|state| state.appearance_generations.push(value.generation.get()));
        }
        let bytes = match command {
            RuntimeCommand::Key(key) => key.text.as_deref().unwrap_or("").as_bytes().to_vec(),
            RuntimeCommand::InputMethodCommit(text) => {
                self.pty.update(|state| state.input_commands += 1);
                if self.pty.inspect(|state| state.expand_input) {
                    vec![b'x'; text.as_str().len() * 8 + 128]
                } else {
                    text.as_str().as_bytes().to_vec()
                }
            }
            RuntimeCommand::RequestPaste(text) => text.as_str().as_bytes().to_vec(),
            RuntimeCommand::Wheel(wheel) => {
                self.pty.update(|state| {
                    state
                        .wheel_steps
                        .push((wheel.horizontal_steps, wheel.vertical_steps))
                });
                vec![b'w'; self.pty.inspect(|state| state.wheel_bytes.unwrap_or(0))]
            }
            _ => Vec::new(),
        };
        Ok(EngineEffect {
            bytes,
            changed: matches!(command, RuntimeCommand::ClearScreenAndScrollback),
        })
    }
    fn resize(&mut self, _: MeasuredGrid) -> Result<(), RuntimeFailure> {
        Ok(())
    }
    fn release_input(&mut self) -> Result<Vec<u8>, RuntimeFailure> {
        Ok(Vec::new())
    }
    fn content_base(&self) -> spaceterm_core::content::ContentBase {
        spaceterm_core::content::ContentBase {
            session: self.session,
            revision: self.revision,
            screen: spaceterm_core::content::Screen::Primary,
            screen_generation: 1,
            topology_revision: 1,
        }
    }
    fn content_revision(&self) -> ContentRevision {
        self.revision
    }
    fn poll(&mut self, hidden_input: bool) -> Result<EngineUpdate, RuntimeFailure> {
        let mut state = self.pty.0.0.lock().unwrap();
        state.poll_count += 1;
        self.pty.0.1.notify_all();
        while state.poll_block_at == Some(state.poll_count) {
            state = self.pty.0.1.wait(state).unwrap();
        }
        drop(state);
        let mut observations = self.observations.take().unwrap_or_default();
        if let Some(metadata) = observations.metadata.take() {
            let mut parts = metadata.parts().clone();
            parts.hidden_input = hidden_input;
            observations.metadata = Some(RuntimeMetadataSnapshot::new(parts).unwrap());
        }
        Ok(EngineUpdate {
            replies: Vec::new(),
            changed: false,
            observations,
        })
    }
    fn bracketed_paste(&self) -> Result<bool, RuntimeFailure> {
        Ok(false)
    }
    fn read_topology(
        &mut self,
        request: &spaceterm_core::content_queries::TopologyPageRequest,
    ) -> Result<spaceterm_core::content_queries::TopologyPageResult, RuntimeFailure> {
        self.pty.update(|state| state.content_reads += 1);
        Ok(spaceterm_core::content_queries::ContentReadResult::Ready(
            spaceterm_core::content_queries::TopologyPage::new(request, Vec::new(), None).unwrap(),
        ))
    }
    fn begin_resource(
        &mut self,
        request: &ResourceRequest,
        _: &spaceterm_terminal::runtime::budget::BudgetLease,
    ) -> Result<EngineJobId, RuntimeFailure> {
        let ResourceRequest::Find(request) = request else {
            return Err(RuntimeFailure::new(
                FailureClass::Unsupported,
                RuntimeStage::Content,
            ));
        };
        self.next += 1;
        self.jobs.insert(self.next, (request.clone(), 0));
        Ok(EngineJobId(self.next))
    }
    fn step_resource(
        &mut self,
        job: EngineJobId,
        _: ContentWorkBudget,
    ) -> Result<ResourceStep, RuntimeFailure> {
        let (request, index) = self.jobs.get_mut(&job.0).unwrap();
        self.pty.update(|state| state.content_steps += 1);
        let result = if *index < 3 {
            let chunk =
                spaceterm_core::content_queries::FindChunk::new(request, *index, Vec::new(), None)
                    .unwrap();
            ResourceStep::Item(ResourceItem::Find(Box::new(
                spaceterm_core::content_queries::FindStreamItem::Chunk(Box::new(chunk)),
            )))
        } else if *index == 3 {
            ResourceStep::Item(ResourceItem::Find(Box::new(
                spaceterm_core::content_queries::FindStreamItem::Complete {
                    stamp: request.stamp(),
                    chunks: 3,
                },
            )))
        } else {
            ResourceStep::Complete
        };
        *index += 1;
        Ok(result)
    }
    fn cancel_resource(&mut self, job: EngineJobId) {
        self.jobs.remove(&job.0);
    }
}
pub fn engine() -> EngineLaunchContext {
    engine_for(Pty::default())
}
pub fn engine_for(pty: Pty) -> EngineLaunchContext {
    engine_after(pty, || {})
}
pub fn engine_after(pty: Pty, before: impl FnOnce() + Send + 'static) -> EngineLaunchContext {
    EngineLaunchContext::new(move |context| {
        before();
        Ok(Box::new(Engine {
            session: context.session,
            revision: ContentRevision::new(1),
            pty,
            jobs: std::collections::BTreeMap::new(),
            next: 0,
            observations: None,
        }))
    })
}
pub fn grid(rows: u32, columns: u32) -> MeasuredGrid {
    MeasuredGrid::new(
        rows,
        columns,
        CellPixels::new(8, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap()
}
pub fn request(pane_number: u64, measured: MeasuredGrid) -> RuntimeStartRequest {
    let server = ServerId::new([7; 16]);
    let pane = PaneId::new(server, pane_number).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let old = SessionKey {
        pane,
        epoch: ServerEpoch::new(1),
        generation: SessionGeneration::new(1),
    };
    let session = SessionKey {
        generation: SessionGeneration::new(2),
        ..old
    };
    let grant = ControlGrant::new(pane, client, ControlGeneration::new(1)).unwrap();
    let connection = ConnectionBinding::new(client, ConnectionGeneration::new(1));
    let context = ContextGeneration::new(1);
    let fence = EffectFence::new(old, grant, connection, context, None).unwrap();
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server, 1).unwrap(),
        PairingId::new(server, 1).unwrap(),
        client,
    )
    .unwrap();
    let intent = StartupIntent::new(
        OperationId::new(stream, OperationSequence::new(1).unwrap()),
        fence,
        StartupSource::ExplicitSelection,
    )
    .unwrap();
    let reservation = MeasuredStartupReservation::new(
        intent,
        MeasuredStartupMeasurement {
            intent: intent.id(),
            fence,
            grid: measured,
        },
        session,
    )
    .unwrap();
    let authority = RuntimeAuthority::new(RuntimeAuthorityParts {
        session,
        grant: Some(grant),
        connection: Some(connection),
        context,
        focused_pane: true,
        terminal_input_focus: true,
        native_effects_eligible: true,
    })
    .unwrap();
    RuntimeStartRequest::new(
        reservation,
        ServerDirectory::new(server, b"/owning-server".to_vec()).unwrap(),
        authority,
        RuntimeAppearance {
            generation: AppearanceGeneration::new(1),
            foreground: spaceterm_core::content::Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
            background: spaceterm_core::content::Rgb { r: 0, g: 0, b: 0 },
            palette: [spaceterm_core::content::Rgb { r: 0, g: 0, b: 0 }; 256],
            bold_as_bright: true,
            appearance: Appearance::Dark,
        },
    )
    .unwrap()
}
pub fn fence(authority: RuntimeAuthority) -> EffectFence {
    let parts = authority.parts();
    EffectFence::new(
        parts.session,
        parts.grant.unwrap(),
        parts.connection.unwrap(),
        parts.context,
        None,
    )
    .unwrap()
}
pub fn factory(limits: RuntimeLimits) -> RuntimeFactory {
    RuntimeFactory::new(limits, Arc::new(Clock::default())).unwrap()
}
pub fn running(factory: &RuntimeFactory, request: RuntimeStartRequest, pty: &Pty) -> RuntimeOwner {
    let owner = factory
        .start(request, pty.launch(), engine_for(pty.clone()))
        .unwrap();
    pty.until(|state| state.launch_count == 1);
    owner.wait_until_running().unwrap();
    owner
}

pub fn ime(text: &str) -> RuntimeCommand {
    RuntimeCommand::InputMethodCommit(BoundedInputMethodText::new(text.into()).unwrap())
}
pub fn takeover(authority: RuntimeAuthority) -> RuntimeAuthorityTransition {
    let mut parts = authority.parts();
    let client = ClientId::new(parts.session.pane.server(), 2).unwrap();
    parts.grant =
        Some(ControlGrant::new(parts.session.pane, client, ControlGeneration::new(2)).unwrap());
    parts.connection = Some(ConnectionBinding::new(client, ConnectionGeneration::new(2)));
    parts.context = ContextGeneration::new(2);
    RuntimeAuthorityTransition::new(authority, RuntimeAuthority::new(parts).unwrap()).unwrap()
}
pub fn stamp(
    authority: RuntimeAuthority,
    revision: u64,
    id: u64,
    generation: u64,
) -> spaceterm_core::content_queries::QueryStamp {
    use spaceterm_core::content::*;
    use spaceterm_core::content_queries::*;
    let parts = authority.parts();
    let revision = ContentRevision::new(revision);
    QueryStamp::new(
        EffectFence::new(
            parts.session,
            parts.grant.unwrap(),
            parts.connection.unwrap(),
            parts.context,
            Some(revision),
        )
        .unwrap(),
        ContentBase {
            session: parts.session,
            revision,
            screen: Screen::Primary,
            screen_generation: 1,
            topology_revision: 1,
        },
        ContentRequestId::new(id).unwrap(),
        RequestGeneration::new(generation),
    )
    .unwrap()
}
pub fn topology(
    stamp: spaceterm_core::content_queries::QueryStamp,
) -> spaceterm_core::content_queries::TopologyPageRequest {
    spaceterm_core::content_queries::TopologyPageRequest::new(
        stamp,
        None,
        spaceterm_core::content_queries::PageLimits::new(256, 16_384, 65_536).unwrap(),
    )
    .unwrap()
}

pub fn engine_with_observations(
    pty: Pty,
    make: impl FnOnce(EngineStartContext) -> EngineObservations + Send + 'static,
) -> EngineLaunchContext {
    EngineLaunchContext::new(move |context| {
        Ok(Box::new(Engine {
            session: context.session,
            revision: ContentRevision::new(1),
            pty,
            jobs: std::collections::BTreeMap::new(),
            next: 0,
            observations: Some(make(context)),
        }))
    })
}
