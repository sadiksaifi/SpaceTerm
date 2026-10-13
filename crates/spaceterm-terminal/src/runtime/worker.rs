//! OS-thread ownership and bounded scheduling. Only explicit ownership retires a PTY.
use super::authority::AuthorityStamp;
use super::budget::{BudgetLease, RuntimeRegistration};
use super::cleanup::{CleanupObligation, CleanupRegistry, RetainedCapacity};
use super::ingress::{Envelope, Request, StreamSink};
use super::input::{InputCompletion, InputOrigin, PtyInput, eligibility};
use super::native_requests::{NativeRequests, OutputEffect, OutputFilter};
use super::owner::Shared;
use super::ports::*;
use crate::runtime_contracts::*;
use spaceterm_core::authority::EffectFence;
use spaceterm_core::native_requests::{NativeCompletionKind, PasteDecision};
use spaceterm_core::observations::RuntimeClock;
use spaceterm_core::organization::FailureClass;
use spaceterm_core::retirement::{CleanupResult, SessionCleanupObservation};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

const FINAL_WAIT: Duration = Duration::from_secs(2);
struct ContentJob {
    id: EngineJobId,
    request: ResourceRequest,
    sink: StreamSink,
    _lease: Arc<BudgetLease>,
    pending: Option<ResourceItem>,
    _query: Arc<super::ingress::QueryRegistration>,
}
struct Worker {
    shared: Arc<Shared>,
    registration: Option<RuntimeRegistration>,
    cleanup_registry: CleanupRegistry,
    clock: Arc<dyn RuntimeClock>,
    appearance: Option<RuntimeAppearance>,
    engine: Option<Box<dyn RuntimeEngine>>,
    process: Option<Box<dyn PtyProcess>>,
    termination: Option<Arc<dyn PtyTermination>>,
    reader: Option<PtyReaderPump<Option<AuthorityStamp>>>,
    input: PtyInput,
    filter: OutputFilter,
    native: NativeRequests,
    effects: VecDeque<OutputEffect>,
    deferred_commands: VecDeque<Envelope>,
    output_sequence: u64,
    output_barrier: Option<u64>,
    launch_unconfirmed: bool,
    jobs: VecDeque<ContentJob>,
}
pub(crate) fn run(
    shared: Arc<Shared>,
    registration: RuntimeRegistration,
    request: RuntimeStartRequest,
    launch: PreparedRuntimeLaunch,
    engine: EngineLaunchContext,
    clock: Arc<dyn RuntimeClock>,
    cleanup_registry: CleanupRegistry,
) {
    let mut worker = Worker {
        shared,
        registration: Some(registration),
        cleanup_registry,
        clock,
        appearance: None,
        engine: None,
        process: None,
        termination: None,
        reader: None,
        input: PtyInput::default(),
        filter: OutputFilter::default(),
        native: NativeRequests::default(),
        effects: VecDeque::new(),
        deferred_commands: VecDeque::new(),
        output_sequence: 0,
        output_barrier: None,
        launch_unconfirmed: false,
        jobs: VecDeque::new(),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        worker
            .start(request, launch, engine)
            .and_then(|()| worker.work())
    }));
    if let Err(error) = result.unwrap_or_else(|_| {
        Err(RuntimeFailure::new(
            FailureClass::RuntimeFault,
            RuntimeStage::Engine,
        ))
    }) {
        let status = if worker
            .shared
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .eq(&RuntimeLifecycle::Starting)
        {
            RuntimeTerminalStatus::StartupFailed { class: error.class }
        } else {
            RuntimeTerminalStatus::RuntimeFailed { class: error.class }
        };
        let _ = worker.shared.terminal(status);
    }
    worker.cleanup();
}
impl Worker {
    fn engine(&mut self) -> Result<&mut (dyn RuntimeEngine + 'static), RuntimeFailure> {
        self.engine
            .as_deref_mut()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Engine))
    }
    fn start(
        &mut self,
        request: RuntimeStartRequest,
        launch: PreparedRuntimeLaunch,
        engine: EngineLaunchContext,
    ) -> Result<(), RuntimeFailure> {
        if self.shared.authority.read()?.sealed {
            return Ok(());
        }
        self.appearance = Some(request.appearance().clone());
        self.engine = Some((engine.0)(EngineStartContext {
            session: self.shared.session,
            grid: request.reservation().grid(),
            directory: request.directory().clone(),
            appearance: request.appearance().clone(),
            clock: Arc::clone(&self.clock),
        })?);
        let base = self.engine()?.content_base();
        self.shared.set_base(base)?;
        if self.shared.authority.read()?.sealed {
            return Ok(());
        }
        let startup_fence = EffectFence::new(
            self.shared.session,
            request.reservation().intent().fence().grant(),
            request.reservation().intent().fence().connection(),
            request.reservation().intent().fence().context(),
            None,
        )
        .map_err(|_| RuntimeFailure::stale())?;
        if !self.shared.authority.read()?.permits_stamp(
            AuthorityStamp {
                fence: startup_fence,
                version: 0,
            },
            None,
            None,
        ) {
            return Err(RuntimeFailure::stale());
        }
        // An adapter unwind can lose resources before returning ownership.
        // Quarantine the slot before invoking any process effect, including
        // cleanup callbacks; only a confirmed cleanup may release it again.
        self.registration
            .as_mut()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Launch))?
            .quarantine();
        self.launch_unconfirmed = true;
        let parts = match launch(request.reservation().grid().into()) {
            Ok(parts) => parts,
            Err(error) => {
                self.launch_unconfirmed = error.stage == RuntimeStage::Cleanup;
                return Err(error);
            }
        };
        self.launch_unconfirmed = false;
        // Install every resource before the next fallible ownership transition.
        self.process = Some(parts.process);
        self.termination = Some(parts.termination);
        let reader = self
            .process
            .as_mut()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Reader))?
            .take_reader()?;
        let capture = Arc::clone(&self.shared);
        let notify = Arc::clone(&self.shared);
        self.reader = Some(PtyReaderPump::spawn(
            reader,
            move || Ok(capture.authority.read()?.native_stamp()),
            move || notify.wake.notify(),
        )?);
        {
            let authority = self.shared.authority.read()?;
            if authority.sealed {
                return Ok(());
            }
            if !authority.permits_stamp(
                AuthorityStamp {
                    fence: startup_fence,
                    version: 0,
                },
                None,
                None,
            ) {
                return Err(RuntimeFailure::stale());
            }
            self.shared.set_lifecycle(RuntimeLifecycle::Running);
        }
        self.shared.presentation.publish(&self.shared, true);
        Ok(())
    }
    fn work(&mut self) -> Result<(), RuntimeFailure> {
        loop {
            if self.shared.authority.read()?.sealed {
                return Ok(());
            }
            let pending = self.native.clipboard_pending();
            self.authority_transition()?;
            let now = self
                .clock
                .now()
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
            self.native.expire(now, &self.shared, &mut self.input)?;
            if pending && !self.native.clipboard_pending() {
                self.capture_output_barrier()?;
            }
            let output_ready = self.output_work()?;
            if self.shared.authority.read()?.sealed {
                return Ok(());
            }
            if output_ready {
                self.output_barrier = None;
                for _ in 0..16 {
                    let Some(envelope) = self.deferred_commands.pop_front() else {
                        break;
                    };
                    self.handle_request(envelope)?;
                    if self.native.clipboard_pending() {
                        break;
                    }
                }
            }
            for _ in 0..16 {
                let envelope = self
                    .shared
                    .ingress
                    .0
                    .lock()
                    .map_err(|_| RuntimeFailure::internal(RuntimeStage::Admission))?
                    .pop_front();
                let Some(envelope) = envelope else {
                    break;
                };
                self.request(envelope)?;
            }
            self.content_work()?;
            let hidden_input = self
                .process
                .as_ref()
                .ok_or(RuntimeFailure::internal(RuntimeStage::Engine))?
                .hidden_input()?;
            let update = self.engine()?.poll(hidden_input)?;
            self.update(update)?;
            if let Some(process) = self.process.as_deref_mut() {
                self.input.drain(&self.shared, process)?;
            }
            self.shared.wake.wait(Duration::from_millis(10));
        }
    }
    fn authority_transition(&mut self) -> Result<(), RuntimeFailure> {
        let message = self
            .shared
            .authority_mailbox
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Authority))?
            .take();
        let Some(message) = message else {
            return Ok(());
        };
        let shared = Arc::clone(&self.shared);
        self.input.discard_ineligible(&shared)?;
        self.native.expire(
            self.clock
                .now()
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?,
            &shared,
            &mut self.input,
        )?;
        self.cancel_jobs(RuntimeFailure::stale());
        let authority = shared.authority.read()?;
        let bytes = self.engine()?.release_input()?;
        let reservation = shared.reserve_input(bytes.len(), false)?;
        self.input.enqueue(
            bytes,
            InputOrigin::Session,
            reservation,
            Some(InputCompletion::Authority(
                message.reply,
                Box::new(message.transition),
            )),
        )?;
        drop(authority);
        shared.presentation.publish(&shared, false);
        Ok(())
    }
    fn update(&mut self, update: EngineUpdate) -> Result<(), RuntimeFailure> {
        let metadata_changed = self.shared.retain_observations(update.observations)?;
        let base = self.engine()?.content_base();
        self.shared.set_base(base)?;
        if !update.replies.is_empty() {
            let reservation = self.shared.reserve_input(update.replies.len(), false)?;
            self.input
                .enqueue(update.replies, InputOrigin::Session, reservation, None)?;
        }
        if update.changed || metadata_changed {
            self.shared
                .presentation
                .publish(&self.shared, metadata_changed);
        }
        Ok(())
    }
    fn capture_output_barrier(&mut self) -> Result<(), RuntimeFailure> {
        if self.output_barrier.is_none() {
            self.output_barrier = Some(
                self.reader
                    .as_ref()
                    .ok_or(RuntimeFailure::internal(RuntimeStage::Reader))?
                    .high_water_mark(),
            );
        }
        Ok(())
    }
    fn output_barrier_reached(&self) -> bool {
        !self.native.clipboard_pending()
            && self
                .output_barrier
                .is_none_or(|barrier| self.output_sequence >= barrier && self.effects.is_empty())
    }
    // Drain the captured reader high-water mark across bounded quanta. Output
    // read after the native wait cannot extend the barrier or starve input.
    fn output_work(&mut self) -> Result<bool, RuntimeFailure> {
        for _ in 0..128 {
            if self.native.clipboard_pending() {
                return Ok(false);
            }
            if self.output_barrier.is_some() && self.output_barrier_reached() {
                return Ok(true);
            }
            if let Some(effect) = self.effects.pop_front() {
                match effect {
                    OutputEffect::Terminal(bytes, captured) => {
                        let update = self.engine()?.feed(&bytes)?;
                        self.update(update)?;
                        for attention in self.engine()?.take_attention() {
                            let notice = match attention {
                                crate::attention::AttentionEvent::Bell => spaceterm_core::native_requests::NotificationNotice::Bell,
                                crate::attention::AttentionEvent::CommandFinished { exit_status, duration } => spaceterm_core::native_requests::NotificationNotice::CommandFinished {
                                    exit_status, duration_millis: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                                },
                            };
                            let now = self
                                .clock
                                .now()
                                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
                            self.native.notify(notice, captured, now, &self.shared)?;
                        }
                    }
                    OutputEffect::Clipboard(operation, captured) => {
                        let hidden = self
                            .process
                            .as_ref()
                            .ok_or(RuntimeFailure::internal(RuntimeStage::NativeRequest))?
                            .hidden_input()?;
                        self.native.clipboard(
                            operation,
                            captured,
                            self.clock
                                .now()
                                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?,
                            &self.shared,
                            &mut self.input,
                            hidden,
                        )?;
                    }
                    OutputEffect::Permission(notice, captured) => self.native.permission(
                        notice,
                        captured,
                        self.clock
                            .now()
                            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?,
                        &self.shared,
                    )?,
                }
                continue;
            }
            let event = self
                .reader
                .as_ref()
                .ok_or(RuntimeFailure::internal(RuntimeStage::Reader))?
                .try_next()?;
            match event {
                Some(PtyReadEvent::Bytes {
                    bytes,
                    receipt,
                    sequence,
                }) => {
                    self.output_sequence = sequence;
                    self.effects = self.filter.feed(&bytes, receipt)
                }
                Some(PtyReadEvent::Finished) => {
                    self.reader
                        .as_mut()
                        .ok_or(RuntimeFailure::internal(RuntimeStage::Reader))?
                        .join()??;
                    let exit = self
                        .process
                        .as_mut()
                        .ok_or(RuntimeFailure::internal(RuntimeStage::Cleanup))?
                        .wait_for_exit(FINAL_WAIT)?;
                    match exit {
                        PtyExit::Exited { status } => {
                            self.shared.terminal(RuntimeTerminalStatus::OrdinaryExit {
                                exit_status: status,
                            })?;
                        }
                        PtyExit::StillRunning => {
                            return Err(RuntimeFailure::new(
                                FailureClass::RuntimeUnavailable,
                                RuntimeStage::Cleanup,
                            ));
                        }
                    }
                    return Ok(false);
                }
                None => return Ok(self.output_barrier_reached()),
            }
        }
        Ok(self.output_barrier_reached())
    }
    fn request(&mut self, envelope: Envelope) -> Result<(), RuntimeFailure> {
        // Output after OSC 52 remains ordered before input admitted during the
        // native wait. Keep the original count/byte/input leases while parked;
        // completion and owner mailboxes must remain able to make progress.
        if (self.native.clipboard_pending()
            || self.output_barrier.is_some()
            || !self.deferred_commands.is_empty())
            && matches!(&envelope.request, Request::Command { command, .. }
                if !matches!(command, RuntimeCommand::CompleteNativeRequest(_)))
        {
            self.deferred_commands.push_back(envelope);
            return Ok(());
        }
        let pending = self.native.clipboard_pending();
        self.handle_request(envelope)?;
        if pending && !self.native.clipboard_pending() {
            self.capture_output_barrier()?;
        }
        Ok(())
    }
    fn handle_request(&mut self, envelope: Envelope) -> Result<(), RuntimeFailure> {
        let shared = Arc::clone(&self.shared);
        match envelope.request {
            Request::Command {
                stamp,
                command,
                reply,
                input,
            } => {
                if reply.cancelled() {
                    return Ok(());
                }
                let fence = stamp.fence;
                let authority = shared.authority.read()?;
                if !authority.permits_stamp(stamp, Some(&command), Some(shared.revision())) {
                    reply.complete(Ok(if authority.sealed {
                        EffectDisposition::Retired
                    } else {
                        EffectDisposition::StaleAuthority
                    }));
                    return Ok(());
                }
                match command {
                    RuntimeCommand::Resize(next) => {
                        drop(authority);
                        self.resize(stamp, next, reply)?;
                    }
                    RuntimeCommand::CellPixelMetrics(pixels) => {
                        let grid = self
                            .registration
                            .as_ref()
                            .ok_or(RuntimeFailure::internal(RuntimeStage::Resize))?
                            .grid;
                        let next = spaceterm_core::geometry::MeasuredGrid::new(
                            u32::from(grid.rows()),
                            u32::from(grid.columns()),
                            pixels,
                            shared.factory.limits.grid,
                        )
                        .map_err(|_| RuntimeFailure::capacity(RuntimeStage::Resize))?;
                        drop(authority);
                        self.resize(stamp, next, reply)?;
                    }
                    RuntimeCommand::RequestPaste(text) => {
                        let bracketed = self.engine()?.bracketed_paste()?;
                        let now = self
                            .clock
                            .now()
                            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
                        let prepared = self.native.paste(
                            text,
                            stamp,
                            now,
                            bracketed,
                            &shared,
                            (input, reply),
                        )?;
                        drop(authority);
                        if let Some((command, input, reply)) = prepared {
                            self.apply_command(stamp, command, input, reply)?;
                        }
                    }
                    RuntimeCommand::ResolvePaste(approval, decision) => {
                        let now = self
                            .clock
                            .now()
                            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
                        let prepared = self.native.resolve_paste(approval, decision, stamp, now);
                        drop(authority);
                        if let Some((text, input)) = prepared {
                            self.apply_command(
                                stamp,
                                RuntimeCommand::RequestPaste(text),
                                input,
                                reply,
                            )?;
                        } else {
                            reply.complete(Ok(if decision == PasteDecision::Cancel {
                                EffectDisposition::Cancelled
                            } else {
                                EffectDisposition::StaleAuthority
                            }));
                        }
                    }
                    RuntimeCommand::CompleteNativeRequest(completion) => {
                        drop(authority);
                        let hidden = self
                            .process
                            .as_ref()
                            .ok_or(RuntimeFailure::internal(RuntimeStage::NativeRequest))?
                            .hidden_input()?;
                        if completion.fence() != fence {
                            reply.complete(Ok(EffectDisposition::StaleAuthority));
                            return Ok(());
                        }
                        if let NativeCompletionKind::Paste { approval, decision } =
                            completion.kind()
                        {
                            let prepared = self.native.resolve_completion_paste(
                                &completion,
                                stamp,
                                *approval,
                                *decision,
                                self.clock
                                    .now()
                                    .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?,
                            );
                            if let Some((text, input)) = prepared {
                                self.apply_command(
                                    stamp,
                                    RuntimeCommand::RequestPaste(text),
                                    input,
                                    reply,
                                )?;
                            } else {
                                reply.complete(Ok(EffectDisposition::StaleAuthority));
                            }
                        } else {
                            self.native.complete(
                                &completion,
                                self.clock
                                    .now()
                                    .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?,
                                &shared,
                                &mut self.input,
                                reply,
                                hidden,
                            )?;
                        }
                    }
                    command => {
                        drop(authority);
                        self.apply_command(stamp, command, input, reply)?;
                    }
                }
            }
            Request::Page(request, reply, version) => {
                if reply.cancelled() {
                    return Ok(());
                }
                if shared.validate_read(request.stamp(), version).is_err() {
                    reply.complete(Err(RuntimeFailure::stale()));
                    return Ok(());
                }
                let result = self.engine()?.read_page(&request);
                let authority = shared.authority.read()?;
                reply.complete(
                    if authority.version == version
                        && authority.permits_read(request.stamp().fence())
                        && shared.queries.current(request.stamp())
                    {
                        result
                    } else {
                        Err(RuntimeFailure::stale())
                    },
                );
            }
            Request::Topology(request, reply, version) => {
                if reply.cancelled() {
                    return Ok(());
                }
                if shared.validate_read(request.stamp(), version).is_err() {
                    reply.complete(Err(RuntimeFailure::stale()));
                    return Ok(());
                }
                let result = self.engine()?.read_topology(&request);
                let authority = shared.authority.read()?;
                reply.complete(
                    if authority.version == version
                        && authority.permits_read(request.stamp().fence())
                        && shared.queries.current(request.stamp())
                    {
                        result
                    } else {
                        Err(RuntimeFailure::stale())
                    },
                );
            }
            Request::Resource(request, sink, lease, query) => {
                if sink.closed() {
                    return Ok(());
                }
                if self.jobs.len() >= shared.factory.limits.content_jobs {
                    sink.fail(RuntimeFailure::capacity(RuntimeStage::Content));
                    return Ok(());
                }
                if shared
                    .validate_query(request.stamp(), query.version)
                    .is_err()
                {
                    sink.fail(RuntimeFailure::stale());
                    return Ok(());
                }
                match self.engine()?.begin_resource(&request, &lease) {
                    Ok(id) => self.jobs.push_back(ContentJob {
                        id,
                        request,
                        sink,
                        _lease: lease,
                        pending: None,
                        _query: query,
                    }),
                    Err(error) => sink.fail(error),
                }
            }
        }
        Ok(())
    }
    fn resize(
        &mut self,
        stamp: AuthorityStamp,
        next: spaceterm_core::geometry::MeasuredGrid,
        reply: super::ingress::ReplySender<EffectDisposition>,
    ) -> Result<(), RuntimeFailure> {
        let temporary = match self
            .registration
            .as_ref()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Resize))?
            .reserve_resize(next)
        {
            Ok(lease) => lease,
            Err(error) => {
                reply.complete(Err(error));
                return Ok(());
            }
        };
        let shared = Arc::clone(&self.shared);
        let authority = shared.authority.read()?;
        if reply.cancelled() {
            return Ok(());
        }
        if !authority.permits_stamp(stamp, None, Some(shared.revision())) {
            reply.complete(Ok(EffectDisposition::StaleAuthority));
            return Ok(());
        }
        let result = self
            .process
            .as_ref()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Resize))?
            .resize(next.into())
            .and_then(|()| self.engine()?.resize(next));
        if let Err(error) = result {
            reply.complete(Err(error));
            return Err(error);
        }
        self.registration
            .as_mut()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Resize))?
            .commit_resize(next, temporary)?;
        let base = self.engine()?.content_base();
        shared.set_base(base)?;
        reply.complete(Ok(EffectDisposition::Applied));
        drop(authority);
        shared.presentation.publish(&shared, false);
        Ok(())
    }
    fn apply_command(
        &mut self,
        stamp: AuthorityStamp,
        command: RuntimeCommand,
        input: BudgetLease,
        reply: super::ingress::ReplySender<EffectDisposition>,
    ) -> Result<(), RuntimeFailure> {
        let shared = Arc::clone(&self.shared);
        let authority = shared.authority.read()?;
        if reply.cancelled() {
            return Ok(());
        }
        if !authority.permits_stamp(stamp, Some(&command), Some(shared.revision()))
            || shared.validate_command_base(&command).is_err()
        {
            reply.complete(Ok(EffectDisposition::StaleAuthority));
            return Ok(());
        }
        if let RuntimeCommand::Appearance(next) = &command
            && let Some(current) = &self.appearance
        {
            if next.generation < current.generation {
                reply.complete(Ok(EffectDisposition::StaleAuthority));
                return Ok(());
            }
            if next.generation == current.generation && next.as_ref() != current {
                reply.complete(Err(RuntimeFailure::new(
                    FailureClass::InvalidRequest,
                    RuntimeStage::Admission,
                )));
                return Ok(());
            }
        }
        match self.engine()?.apply(&command) {
            Ok(effect) => {
                if let RuntimeCommand::Appearance(next) = &command {
                    self.appearance = Some(next.as_ref().clone());
                }
                let base = self.engine()?.content_base();
                shared.set_base(base)?;
                self.input.enqueue(
                    effect.bytes,
                    InputOrigin::Client(stamp, eligibility(&command)),
                    input,
                    Some(InputCompletion::Effect(reply)),
                )?;
                drop(authority);
                if effect.changed {
                    shared.presentation.publish(&shared, false);
                }
            }
            Err(error) => {
                reply.complete(Err(error));
                if !matches!(
                    error.class,
                    FailureClass::InvalidRequest
                        | FailureClass::Unsupported
                        | FailureClass::StaleAuthority
                ) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn content_work(&mut self) -> Result<(), RuntimeFailure> {
        let count = self.jobs.len();
        for _ in 0..count {
            let Some(mut job) = self.jobs.pop_front() else {
                break;
            };
            let shared = Arc::clone(&self.shared);
            if job.sink.closed()
                || shared
                    .validate_query(job.request.stamp(), job._query.version)
                    .is_err()
            {
                self.engine()?.cancel_resource(job.id);
                job.sink.fail(RuntimeFailure::stale());
                continue;
            }
            if job.sink.full() {
                self.jobs.push_back(job);
                continue;
            }
            let step = if let Some(item) = job.pending.take() {
                Ok(ResourceStep::Item(item))
            } else {
                self.engine()?
                    .step_resource(job.id, ContentWorkBudget::default())
            };
            let authority = shared.authority.read()?;
            if authority.version != job._query.version
                || !authority.permits(job.request.stamp().fence(), None, Some(shared.revision()))
                || !shared.queries.current(job.request.stamp())
                || !shared.matches_base(job.request.stamp())?
            {
                self.engine()?.cancel_resource(job.id);
                job.sink.fail(RuntimeFailure::stale());
                continue;
            }
            match step {
                Ok(ResourceStep::Item(item)) => match job.sink.send(item) {
                    Ok(()) => self.jobs.push_back(job),
                    Err(Some(item)) => {
                        job.pending = Some(item);
                        self.jobs.push_back(job);
                    }
                    Err(None) => {
                        self.engine()?.cancel_resource(job.id);
                        job.sink
                            .fail(RuntimeFailure::internal(RuntimeStage::Content));
                    }
                },
                Ok(ResourceStep::Pending) => self.jobs.push_back(job),
                Ok(ResourceStep::Complete) => self.engine()?.cancel_resource(job.id),
                Err(error) => {
                    self.engine()?.cancel_resource(job.id);
                    job.sink.fail(error);
                }
            }
        }
        Ok(())
    }
    fn cancel_jobs(&mut self, error: RuntimeFailure) {
        for job in self.jobs.drain(..) {
            if let Some(engine) = self.engine.as_deref_mut() {
                engine.cancel_resource(job.id);
            }
            job.sink.fail(error);
        }
    }
    fn cleanup(&mut self) {
        self.input.retire();
        self.cancel_jobs(RuntimeFailure::new(
            FailureClass::RuntimeUnavailable,
            RuntimeStage::Cleanup,
        ));
        self.native.clear();
        self.effects.clear();
        self.filter = OutputFilter::default();
        self.shared.presentation.clear();
        self.shared.native_sender.close();
        while self.shared.native_drain.try_recv().is_ok() {}
        self.engine = None;
        self.appearance = None;
        {
            let mut observations = self
                .shared
                .observations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            observations.metadata = None;
            observations.display = None;
            observations.confirmed = None;
            observations.close_work = None;
        }
        *self
            .shared
            .base
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let queued: Vec<_> = self
            .shared
            .ingress
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .collect();
        for envelope in queued.into_iter().chain(self.deferred_commands.drain(..)) {
            match envelope.request {
                Request::Command { reply, .. } => reply.complete(Ok(EffectDisposition::Retired)),
                Request::Page(_, reply, _) => reply.complete(Err(RuntimeFailure::stale())),
                Request::Topology(_, reply, _) => reply.complete(Err(RuntimeFailure::stale())),
                Request::Resource(_, sink, _, _) => sink.fail(RuntimeFailure::stale()),
            }
        }
        if let Some(message) = self
            .shared
            .authority_mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            message.reply.complete(Err(RuntimeFailure::stale()));
        }
        let mut outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cleanup_pty(&mut self.process, &mut self.termination, &mut self.reader)
        }))
        .unwrap_or(PtyCleanup {
            result: CleanupResult::Failed(FailureClass::RuntimeUnavailable),
            pending: None,
        });
        if self.launch_unconfirmed {
            outcome.result = CleanupResult::Failed(FailureClass::RuntimeUnavailable);
            outcome.pending = None;
        }
        let result = outcome.result;
        let retained = self.registration.take().and_then(|registration| {
            let capacity = RetainedCapacity::server(registration);
            if matches!(result, CleanupResult::Confirmed(_)) {
                capacity.release();
                None
            } else {
                Some(capacity)
            }
        });
        let lifecycle = *self
            .shared
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(terminal) = lifecycle.terminal_result() else {
            return;
        };
        if let Ok(cleanup) = SessionCleanupObservation::new(
            self.shared.session,
            terminal.retirement_reason(),
            result,
        ) {
            let mut observations = self
                .shared
                .observations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            observations.metadata = None;
            observations.display = None;
            observations.confirmed = None;
            observations.close_work = None;
            observations.cleanup = Some(cleanup);
            drop(observations);
            if let Ok(completion) = RuntimeCompletion::new(terminal, cleanup) {
                self.shared
                    .set_lifecycle(RuntimeLifecycle::Finished(completion));
            }
        }
        if let Some(capacity) = retained {
            let observer = Arc::downgrade(&self.shared);
            self.cleanup_registry.retain(CleanupObligation::new(outcome.pending, capacity, move || {
                let Some(shared) = observer.upgrade() else { return; };
                let mut observations = shared.observations.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(previous) = observations.cleanup
                    && let Ok(confirmed) = SessionCleanupObservation::new(
                        shared.session,
                        previous.first_reason(),
                        CleanupResult::Confirmed(spaceterm_core::retirement::ConfirmedCleanupClass::ReapedAndReleased),
                    )
                {
                    // Finished is the completed foreground lifecycle. Only the
                    // independent cleanup observation advances after reproof.
                    observations.cleanup = Some(confirmed);
                    shared.lifecycle_changed.notify_all();
                }
            }));
        }
    }
}
impl StreamSink {
    fn closed(&self) -> bool {
        match self {
            Self::Copy(sender, _, _) => sender.is_closed(),
            Self::Find(sender, _, _) => sender.is_closed(),
            Self::Graphics(sender, _, _) => sender.is_closed(),
        }
    }
    fn full(&self) -> bool {
        match self {
            Self::Copy(sender, _, _) => sender.is_full(),
            Self::Find(sender, _, _) => sender.is_full(),
            Self::Graphics(sender, _, _) => sender.is_full(),
        }
    }
    fn fail(self, error: RuntimeFailure) {
        match self {
            Self::Copy(sender, failure, drain) => {
                *failure.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(error);
                sender.close();
                while drain.try_recv().is_ok() {}
            }
            Self::Find(sender, failure, drain) => {
                *failure.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(error);
                sender.close();
                while drain.try_recv().is_ok() {}
            }
            Self::Graphics(sender, failure, drain) => {
                *failure.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(error);
                sender.close();
                while drain.try_recv().is_ok() {}
            }
        }
    }
    fn send(&self, item: ResourceItem) -> Result<(), Option<ResourceItem>> {
        match (self, item) {
            (Self::Copy(sender, _, _), ResourceItem::Copy(item)) => sender
                .try_send(Ok(*item))
                .map_err(|error| match error.into_inner() {
                    Ok(item) => Some(ResourceItem::Copy(Box::new(item))),
                    Err(_) => None,
                }),
            (Self::Find(sender, _, _), ResourceItem::Find(item)) => sender
                .try_send(Ok(*item))
                .map_err(|error| match error.into_inner() {
                    Ok(item) => Some(ResourceItem::Find(Box::new(item))),
                    Err(_) => None,
                }),
            (Self::Graphics(sender, _, _), ResourceItem::Graphics(item)) => sender
                .try_send(Ok(*item))
                .map_err(|error| match error.into_inner() {
                    Ok(item) => Some(ResourceItem::Graphics(Box::new(item))),
                    Err(_) => None,
                }),
            _ => Err(None),
        }
    }
}
