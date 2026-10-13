//! Non-cloneable lifetime ownership. Handles, replies and subscriptions are inert on Drop.
use super::authority::AuthorityCell;
use super::budget::{Budget, BudgetLease, FactoryBudget, RuntimeLimits};
use super::cleanup::CleanupRegistry;
use super::ingress::{
    Envelope, Ingress, QueryRegistry, ReadCheck, ReplySender, Request, ResourceStream,
    RuntimeReply, StreamFailure, StreamSink, checked_reply, reply,
};
use super::ports::*;
use super::presentation::{Presentation, PresentationSubscription};
use super::schedules::Wake;
use crate::runtime_contracts::*;
use spaceterm_core::authority::{ContentRevision, EffectFence, SessionKey};
use spaceterm_core::content_queries::*;
use spaceterm_core::observations::*;
use spaceterm_core::organization::{FailureClass, RetirementReason};
use spaceterm_core::retirement::SessionCleanupObservation;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub struct RuntimeFactory {
    budget: Arc<FactoryBudget>,
    clock: Arc<dyn RuntimeClock>,
    cleanup: CleanupRegistry,
}
impl RuntimeFactory {
    pub fn new(
        limits: RuntimeLimits,
        clock: Arc<dyn RuntimeClock>,
    ) -> Result<Self, RuntimeFailure> {
        let limits = limits.validate()?;
        Ok(Self {
            budget: FactoryBudget::new(limits),
            clock,
            cleanup: CleanupRegistry::new(limits.owners)?,
        })
    }
    /// Composition supplies only an already admitted, durably published measured reservation.
    /// Neither a wire measurement nor this shape-checked DTO creates launch authority.
    pub fn start(
        &self,
        request: RuntimeStartRequest,
        launch: PreparedRuntimeLaunch,
        engine: EngineLaunchContext,
    ) -> Result<RuntimeOwner, RuntimeFailure> {
        let session = request.reservation().session();
        let registration = self
            .budget
            .register(session.pane, request.reservation().grid())?;
        let tick = self
            .clock
            .now()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
        let close_work = CloseWorkObservation::new(
            session,
            RuntimeObservationSequence::new(1)
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?,
            CloseWorkGeneration::new(0),
            tick,
            CloseWorkState::Unknown,
        )
        .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
        let (native_sender, native_receiver) = async_channel::bounded(2);
        let shared = Arc::new(Shared {
            session,
            authority: AuthorityCell::new(request.authority()),
            lifecycle: Mutex::new(RuntimeLifecycle::Starting),
            lifecycle_changed: Condvar::new(),
            observations: Mutex::new(Observations {
                close_work: Some(close_work),
                ..Observations::default()
            }),
            ingress: Ingress::default(),
            authority_mailbox: Mutex::new(None),
            wake: Wake::default(),
            presentation: Presentation::default(),
            revision: AtomicU64::new(0),
            factory: Arc::clone(&self.budget),
            queue_budget: Budget::new(
                self.budget.limits.commands,
                self.budget.limits.command_bytes,
            ),
            result_budget: Budget::new(self.budget.limits.commands, 0),
            input_budget: Budget::new(
                self.budget.limits.commands + 32,
                self.budget.limits.pending_input_bytes,
            ),
            client_input_budget: Budget::new(
                0,
                self.budget.limits.pending_input_bytes - super::budget::REPLY_INPUT_RESERVE,
            ),
            native_sender,
            native_drain: native_receiver.clone(),
            base: Mutex::new(None),
            native_routed: AtomicBool::new(false),
            queries: Arc::default(),
            clock: Arc::clone(&self.clock),
        });
        let worker_shared = Arc::clone(&shared);
        let clock = Arc::clone(&self.clock);
        let cleanup = self.cleanup.clone();
        let worker = std::thread::Builder::new()
            .name("terminal-runtime".into())
            .spawn(move || {
                super::worker::run(
                    worker_shared,
                    registration,
                    request,
                    launch,
                    engine,
                    clock,
                    cleanup,
                )
            })
            .map_err(|_| {
                RuntimeFailure::new(FailureClass::RuntimeUnavailable, RuntimeStage::Launch)
            })?;
        Ok(RuntimeOwner {
            shared,
            worker: Some(worker),
            native_receiver: Some(native_receiver),
        })
    }
}
#[derive(Default)]
pub(crate) struct Observations {
    pub metadata: Option<RuntimeMetadataSnapshot>,
    pub display: Option<DisplayDirectoryObservation>,
    pub confirmed: Option<ConfirmedDirectoryObservation>,
    pub close_work: Option<CloseWorkObservation>,
    pub cleanup: Option<SessionCleanupObservation>,
}
pub(crate) struct AuthorityMessage {
    pub transition: RuntimeAuthorityTransition,
    pub reply: ReplySender<AuthorityApplied>,
}
pub(crate) struct Shared {
    pub session: SessionKey,
    pub authority: AuthorityCell,
    pub lifecycle: Mutex<RuntimeLifecycle>,
    pub lifecycle_changed: Condvar,
    pub observations: Mutex<Observations>,
    pub ingress: Ingress,
    pub authority_mailbox: Mutex<Option<AuthorityMessage>>,
    pub wake: Wake,
    pub presentation: Presentation,
    pub revision: AtomicU64,
    pub factory: Arc<FactoryBudget>,
    queue_budget: Arc<Budget>,
    result_budget: Arc<Budget>,
    pub input_budget: Arc<Budget>,
    client_input_budget: Arc<Budget>,
    pub native_drain: async_channel::Receiver<
        Result<super::native_requests::RoutedNativeRequest, RuntimeFailure>,
    >,
    pub base: Mutex<Option<spaceterm_core::content::ContentBase>>,
    pub native_routed: AtomicBool,
    pub queries: Arc<QueryRegistry>,
    pub clock: Arc<dyn RuntimeClock>,
    pub native_sender:
        async_channel::Sender<Result<super::native_requests::RoutedNativeRequest, RuntimeFailure>>,
}
impl Shared {
    pub fn revision(&self) -> ContentRevision {
        ContentRevision::new(self.revision.load(Ordering::Acquire))
    }
    pub fn set_lifecycle(&self, lifecycle: RuntimeLifecycle) {
        *self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = lifecycle;
        self.lifecycle_changed.notify_all();
    }
    pub fn terminal(
        &self,
        status: RuntimeTerminalStatus,
    ) -> Result<SessionTerminalResult, RuntimeFailure> {
        let mut authority = self
            .authority
            .0
            .write()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Authority))?;
        let mut lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        if let Some(terminal) = lifecycle.terminal_result() {
            return Ok(terminal);
        }
        let terminal = SessionTerminalResult::new(self.session, status)
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        authority.sealed = true;
        *lifecycle = RuntimeLifecycle::Retiring { terminal };
        drop(lifecycle);
        drop(authority);
        self.presentation.clear();
        self.wake.notify();
        self.lifecycle_changed.notify_all();
        Ok(terminal)
    }
    fn queue_lease(&self, bytes: usize) -> Result<BudgetLease, RuntimeFailure> {
        let local = self
            .queue_budget
            .reserve(1, bytes, RuntimeStage::Admission)?;
        Ok(local.combine(
            self.factory
                .ingress
                .reserve(0, bytes, RuntimeStage::Admission)?,
        ))
    }
    fn result_lease(&self) -> Result<BudgetLease, RuntimeFailure> {
        self.result_budget.reserve(1, 0, RuntimeStage::Admission)
    }
    pub fn reserve_input(&self, bytes: usize, client: bool) -> Result<BudgetLease, RuntimeFailure> {
        if bytes == 0 {
            return Ok(BudgetLease::default());
        }
        let local = self.input_budget.reserve(1, bytes, RuntimeStage::Input)?;
        let global = self.factory.input.reserve(0, bytes, RuntimeStage::Input)?;
        let lease = local.combine(global);
        if client {
            Ok(lease
                .combine(
                    self.client_input_budget
                        .reserve(0, bytes, RuntimeStage::Input)?,
                )
                .combine(
                    self.factory
                        .client_input
                        .reserve(0, bytes, RuntimeStage::Input)?,
                ))
        } else {
            Ok(lease)
        }
    }
    fn enqueue(&self, envelope: Envelope) -> Result<(), RuntimeFailure> {
        self.ingress
            .0
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Admission))?
            .push_back(envelope);
        self.wake.notify();
        Ok(())
    }
    pub fn set_base(
        &self,
        base: spaceterm_core::content::ContentBase,
    ) -> Result<(), RuntimeFailure> {
        let mut current = self
            .base
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Content))?;
        if base.session != self.session
            || base.screen_generation == 0
            || base.topology_revision == 0
            || current.is_some_and(|current| base.revision < current.revision)
        {
            return Err(RuntimeFailure::internal(RuntimeStage::Counter));
        }
        self.revision.store(base.revision.get(), Ordering::Release);
        *current = Some(base);
        Ok(())
    }
    pub fn validate_read(&self, stamp: QueryStamp, version: u64) -> Result<(), RuntimeFailure> {
        let authority = self.authority.read()?;
        if authority.version != version
            || !authority.permits_read(stamp.fence())
            || !self.queries.current(stamp)
        {
            return Err(RuntimeFailure::stale());
        }
        Ok(())
    }
    fn read_result<T>(
        &self,
        stamp: QueryStamp,
        version: u64,
        result: ContentReadResult<T>,
    ) -> Result<ContentReadResult<T>, RuntimeFailure> {
        self.validate_read(stamp, version)?;
        let current = self
            .base
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Content))?
            .ok_or(RuntimeFailure::internal(RuntimeStage::Content))?;
        if stamp.base() != current {
            Ok(ContentReadResult::ResyncRequired {
                request: stamp,
                current,
            })
        } else {
            Ok(result)
        }
    }
    pub fn matches_base(&self, stamp: QueryStamp) -> Result<bool, RuntimeFailure> {
        Ok(*self
            .base
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Content))?
            == Some(stamp.base()))
    }
    pub fn validate_command_base(&self, command: &RuntimeCommand) -> Result<(), RuntimeFailure> {
        let stamp = match command {
            RuntimeCommand::Pointer(pointer) => Some(pointer.stamp),
            RuntimeCommand::Wheel(wheel) => Some(wheel.stamp),
            _ => None,
        };
        if let Some(stamp) = stamp
            && !self.matches_base(stamp)?
        {
            return Err(RuntimeFailure::stale());
        }
        Ok(())
    }
    pub fn validate_query(&self, stamp: QueryStamp, version: u64) -> Result<(), RuntimeFailure> {
        let authority = self.authority.read()?;
        if !authority.permits_stamp(
            super::authority::AuthorityStamp {
                fence: stamp.fence(),
                version,
            },
            None,
            Some(self.revision()),
        ) || !self.queries.current(stamp)
            || !self.matches_base(stamp)?
        {
            return Err(RuntimeFailure::stale());
        }
        Ok(())
    }
    pub fn retain_observations(
        &self,
        incoming: EngineObservations,
    ) -> Result<bool, RuntimeFailure> {
        let mut observations = self
            .observations
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Engine))?;
        let error = RuntimeFailure::internal(RuntimeStage::Counter);
        let mut changed = false;
        if let Some(value) = incoming.metadata {
            if value.session() != self.session {
                return Err(error);
            }
            if observations.metadata.as_ref().is_none_or(|old| {
                value.parts().sequence > old.parts().sequence
                    && value.parts().observed_at >= old.parts().observed_at
            }) {
                observations.metadata = Some(value);
                changed = true;
            }
        }
        if let Some(value) = incoming.display_directory {
            if value.session() != self.session {
                return Err(error);
            }
            if observations.display.as_ref().is_none_or(|old| {
                value.sequence() > old.sequence() && value.observed_at() >= old.observed_at()
            }) {
                observations.display = Some(value);
                changed = true;
            }
        }
        if let Some(value) = incoming.confirmed_directory {
            if value.session() != self.session {
                return Err(error);
            }
            if observations.confirmed.as_ref().is_none_or(|old| {
                value.sequence() > old.sequence() && value.observed_at() >= old.observed_at()
            }) {
                observations.confirmed = Some(value);
            }
        }
        if let Some(value) = incoming.close_work {
            if value.session() != self.session {
                return Err(error);
            }
            if observations.close_work.is_none_or(|old| {
                value.sequence() > old.sequence()
                    && value.generation() >= old.generation()
                    && value.observed_at() >= old.observed_at()
            }) {
                if observations.close_work.is_some_and(|old| {
                    value.state() != old.state() && value.generation() <= old.generation()
                }) {
                    return Err(error);
                }
                observations.close_work = Some(value);
            }
        }
        Ok(changed)
    }
}
pub struct RuntimeOwner {
    pub(crate) shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    native_receiver: Option<
        async_channel::Receiver<
            Result<super::native_requests::RoutedNativeRequest, RuntimeFailure>,
        >,
    >,
}
impl RuntimeOwner {
    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            shared: Arc::clone(&self.shared),
        }
    }
    pub fn subscribe(
        &self,
        fence: EffectFence,
    ) -> Result<PresentationSubscription, RuntimeFailure> {
        self.shared
            .presentation
            .subscribe(Arc::clone(&self.shared), fence)
    }
    /// Install the server's native route once. Without a route, requests are denied.
    pub fn take_native_requests(
        &mut self,
    ) -> Result<super::native_requests::NativeRequestSubscription, RuntimeFailure> {
        let receiver = self
            .native_receiver
            .take()
            .ok_or(RuntimeFailure::capacity(RuntimeStage::NativeRequest))?;
        self.shared.native_routed.store(true, Ordering::Release);
        Ok(super::native_requests::NativeRequestSubscription {
            receiver,
            shared: Arc::clone(&self.shared),
        })
    }
    pub fn transition_authority(
        &mut self,
        transition: RuntimeAuthorityTransition,
    ) -> Result<RuntimeReply<AuthorityApplied>, RuntimeFailure> {
        let mut mailbox = self
            .shared
            .authority_mailbox
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Authority))?;
        if mailbox.is_some() {
            return Err(RuntimeFailure::capacity(RuntimeStage::Authority));
        }
        let mut authority = self
            .shared
            .authority
            .0
            .write()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Authority))?;
        if authority.sealed
            || authority.current != transition.expected()
            || transition.next().session() != self.shared.session
        {
            return Err(RuntimeFailure::stale());
        }
        let (sender, result) = reply(BudgetLease::default());
        authority.version = authority
            .version
            .checked_add(1)
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?;
        authority.current = transition.next();
        *mailbox = Some(AuthorityMessage {
            transition,
            reply: sender,
        });
        drop(authority);
        drop(mailbox);
        self.shared.wake.notify();
        Ok(result)
    }
    pub fn retire(&mut self, reason: RetirementReason) -> RetirementTicket {
        let status = match reason {
            RetirementReason::OrdinaryExit | RetirementReason::RuntimeFailure => {
                RuntimeTerminalStatus::RuntimeFailed {
                    class: FailureClass::InvalidRequest,
                }
            }
            reason => RuntimeTerminalStatus::Retired { reason },
        };
        let _ = self.shared.terminal(status);
        RetirementTicket {
            shared: Arc::clone(&self.shared),
        }
    }
    pub fn lifecycle_snapshot(&self) -> RuntimeLifecycle {
        *self
            .shared
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    pub fn metadata_snapshot(&self) -> Option<RuntimeMetadataSnapshot> {
        self.shared
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .metadata
            .clone()
    }
    /// Blocking only. Lifecycle and cleanup remain independently observable.
    pub fn join(&mut self) -> Result<SessionCleanupObservation, RuntimeFailure> {
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        }
        self.cleanup_snapshot()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Cleanup))
    }
    pub fn wait_until_running(&self) -> Result<(), RuntimeFailure> {
        let mut lifecycle = self
            .shared
            .lifecycle
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Launch))?;
        while *lifecycle == RuntimeLifecycle::Starting {
            lifecycle = self
                .shared
                .lifecycle_changed
                .wait(lifecycle)
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Launch))?;
        }
        if *lifecycle == RuntimeLifecycle::Running {
            Ok(())
        } else {
            Err(RuntimeFailure::new(
                FailureClass::LaunchUnavailable,
                RuntimeStage::Launch,
            ))
        }
    }
}
impl RuntimeObservationSource for RuntimeOwner {
    fn session(&self) -> SessionKey {
        self.shared.session
    }
    fn display_directory_snapshot(&self) -> Option<DisplayDirectoryObservation> {
        self.shared
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .display
            .clone()
    }
    fn confirmed_directory_snapshot(&self) -> Option<ConfirmedDirectoryObservation> {
        self.shared
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .confirmed
            .clone()
    }
    fn close_work_snapshot(&self) -> Option<CloseWorkObservation> {
        self.shared
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .close_work
    }
    fn cleanup_snapshot(&self) -> Option<SessionCleanupObservation> {
        self.shared
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cleanup
    }
}
impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.shared.terminal(RuntimeTerminalStatus::RuntimeFailed {
                class: FailureClass::RuntimeUnavailable,
            });
            let _ = self.join();
        }
    }
}
pub struct RetirementTicket {
    shared: Arc<Shared>,
}
impl RetirementTicket {
    pub fn snapshot(&self) -> Option<SessionCleanupObservation> {
        self.shared
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cleanup
    }
    pub fn wait_blocking(&self) -> Result<SessionCleanupObservation, RuntimeFailure> {
        let mut lifecycle = self
            .shared
            .lifecycle
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        while !matches!(*lifecycle, RuntimeLifecycle::Finished(_)) {
            lifecycle = self
                .shared
                .lifecycle_changed
                .wait(lifecycle)
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        }
        match *lifecycle {
            RuntimeLifecycle::Finished(completion) => Ok(completion.cleanup()),
            _ => Err(RuntimeFailure::internal(RuntimeStage::Cleanup)),
        }
    }
}
#[derive(Clone)]
pub struct RuntimeHandle {
    pub(crate) shared: Arc<Shared>,
}
impl RuntimeHandle {
    pub fn try_submit(
        &self,
        fence: EffectFence,
        mut command: RuntimeCommand,
    ) -> Result<RuntimeReply<EffectDisposition>, RuntimeFailure> {
        // Match the legacy application-wheel encoder before reserving or encoding.
        if let RuntimeCommand::Wheel(wheel) = &mut command {
            wheel.horizontal_steps = wheel.horizontal_steps.clamp(-100, 100);
            wheel.vertical_steps = wheel.vertical_steps.clamp(-100, 100);
        }
        let authority = self.shared.authority.read()?;
        if !authority.permits(fence, Some(&command), Some(self.shared.revision())) {
            return Err(RuntimeFailure::stale());
        }
        super::input::validate_command_fence(fence, &command)?;
        self.shared.validate_command_base(&command)?;
        let (bytes, encoded) = super::input::command_cost(&command)?;
        let queue = self.shared.queue_lease(bytes)?;
        let input = self.shared.reserve_input(encoded, true)?;
        let (sender, result) = reply(self.shared.result_lease()?);
        self.shared.enqueue(Envelope {
            request: Request::Command {
                stamp: authority.capture(fence),
                command,
                reply: sender,
                input,
            },
            _lease: queue,
        })?;
        Ok(result)
    }
    pub fn read_page(
        &self,
        request: ContentPageRequest,
    ) -> Result<RuntimeReply<ContentPageResult>, RuntimeFailure> {
        let authority = self.shared.authority.read()?;
        if !authority.permits_read(request.stamp().fence()) {
            return Err(RuntimeFailure::stale());
        }
        let result_lease = self
            .shared
            .result_lease()?
            .combine(self.shared.factory.query.reserve(
                0,
                request.limits().bytes() as usize,
                RuntimeStage::Content,
            )?);
        let (sender, result) = checked_reply(
            result_lease,
            Some(self.read_check(request.stamp(), authority.version)?),
        );
        self.shared.enqueue(Envelope {
            request: Request::Page(request, sender, authority.version),
            _lease: self.shared.queue_lease(0)?,
        })?;
        Ok(result)
    }
    pub fn read_topology(
        &self,
        request: TopologyPageRequest,
    ) -> Result<RuntimeReply<TopologyPageResult>, RuntimeFailure> {
        let authority = self.shared.authority.read()?;
        if !authority.permits_read(request.stamp().fence()) {
            return Err(RuntimeFailure::stale());
        }
        let lease = self
            .shared
            .result_lease()?
            .combine(self.shared.factory.query.reserve(
                0,
                request.limits().bytes() as usize,
                RuntimeStage::Content,
            )?);
        let (sender, result) = checked_reply(
            lease,
            Some(self.read_check(request.stamp(), authority.version)?),
        );
        self.shared.enqueue(Envelope {
            request: Request::Topology(request, sender, authority.version),
            _lease: self.shared.queue_lease(0)?,
        })?;
        Ok(result)
    }
    fn read_check<T: 'static>(
        &self,
        stamp: QueryStamp,
        version: u64,
    ) -> Result<ReadCheck<T>, RuntimeFailure> {
        let registration = self.shared.queries.register(stamp, version)?;
        let shared = Arc::clone(&self.shared);
        Ok(Arc::new(move |value| {
            let _keep = &registration;
            shared.read_result(stamp, version, value)
        }))
    }
    fn resource<T: 'static>(
        &self,
        request: ResourceRequest,
        sink: impl FnOnce(
            async_channel::Sender<Result<T, RuntimeFailure>>,
            Arc<StreamFailure>,
            async_channel::Receiver<Result<T, RuntimeFailure>>,
        ) -> StreamSink,
    ) -> Result<ResourceStream<T>, RuntimeFailure> {
        let authority = self.shared.authority.read()?;
        if !authority.permits(request.stamp().fence(), None, Some(self.shared.revision()))
            || !self.shared.matches_base(request.stamp())?
        {
            return Err(RuntimeFailure::stale());
        }
        let lease = Arc::new(
            self.shared
                .factory
                .query
                .reserve(0, 1024 * 1024, RuntimeStage::Content)?
                .combine(self.shared.result_lease()?),
        );
        let (sender, receiver) = async_channel::bounded(2);
        let failure = Arc::default();
        let stamp = request.stamp();
        let version = authority.version;
        let registration = self.shared.queries.register(stamp, version)?;
        let shared = Arc::clone(&self.shared);
        let delivery_registration = Arc::clone(&registration);
        let check = Some(Box::new(move |_: &T| {
            let _keep = &delivery_registration;
            shared.validate_query(stamp, version)
        })
            as Box<dyn Fn(&T) -> Result<(), RuntimeFailure> + Send + Sync>);
        self.shared.enqueue(Envelope {
            request: Request::Resource(
                request,
                sink(sender, Arc::clone(&failure), receiver.clone()),
                Arc::clone(&lease),
                registration,
            ),
            _lease: self.shared.queue_lease(0)?,
        })?;
        Ok(ResourceStream {
            receiver,
            _lease: lease,
            failure,
            check,
            cancelled: false,
        })
    }
    pub fn prepare_copy(
        &self,
        request: CopyRequest,
    ) -> Result<ResourceStream<CopyStreamItem>, RuntimeFailure> {
        self.resource(ResourceRequest::Copy(request), StreamSink::Copy)
    }
    pub fn find(
        &self,
        request: FindRequest,
    ) -> Result<ResourceStream<FindStreamItem>, RuntimeFailure> {
        self.resource(ResourceRequest::Find(request), StreamSink::Find)
    }
    pub fn read_graphics(
        &self,
        request: GraphicsRequest,
    ) -> Result<ResourceStream<GraphicsChunk>, RuntimeFailure> {
        self.resource(ResourceRequest::Graphics(request), StreamSink::Graphics)
    }
}
