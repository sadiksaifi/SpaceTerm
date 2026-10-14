//! Production ports over the terminal worker and the host's pull-reader PTY parts.

use super::{
    AccountShellLaunch, LaunchGate, LaunchPreparationPort, PreparedLaunchPort, RuntimeCleanupPort,
};
use crate::{contracts::*, failure::failure};
use spaceterm_core::{
    authority::*,
    content_queries::*,
    identity::ServerId,
    native_requests::NativeCompletion,
    observations::*,
    operations::OperationFailure,
    organization::FailureClass,
    retirement::{RetirementRequest, SessionCleanupObservation},
    runtime_authority::*,
};
use spaceterm_host::{
    pty::*, server_directories::native::NativeDirectoryProvider, shell_launch::ShellLaunchPlanner,
};
use spaceterm_terminal::{
    content::runtime::SemanticRuntimeEngine,
    local_filesystem::{
        LocalFilesystemAuthority, LocalFilesystemError, LocalIdentityObservation,
        LocalIdentitySource, LocalObjectIdentity, LocalObjectKind, classify_io_error,
    },
    local_path::LocalPathSemantics,
    metadata::{LocalMachine, PromptOwner, TerminalMetadataContext},
    runtime::{
        budget::RuntimeLimits,
        ingress::{ResourceStream, RuntimeReply},
        native_requests::NativeRequestSubscription,
        owner::{RuntimeFactory, RuntimeHandle, RuntimeOwner},
        ports::*,
        presentation::PresentationSubscription,
    },
    runtime_contracts::{
        FencedRuntimeCommand, RuntimeCommand, RuntimeMetadataSnapshot, RuntimeStartRequest,
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

/// Account facts come from the owning-machine account provider, never client data or HOME.
/// The planner retains the account shell, integration resources and launch policy.
pub struct AccountLaunchFacts {
    pub uid: u32,
    pub home: PathBuf,
    pub planner: ShellLaunchPlanner,
    pub machine: LocalMachine,
}
pub trait AccountLaunchProvider: Send + Sync {
    fn account(&self) -> Result<AccountLaunchFacts, ServerFailure>;
}

/// The actor and terminal workers share this exact monotonic millisecond origin.
pub struct NativeRuntimeClock(Instant);
impl Default for NativeRuntimeClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl NativeRuntimeClock {
    fn tick(&self) -> Result<MonotonicTick, ObservationError> {
        u64::try_from(self.0.elapsed().as_millis())
            .map(MonotonicTick::new)
            .map_err(|_| ObservationError::ZeroCounter)
    }
}
impl ServerClock for NativeRuntimeClock {
    fn now(&self) -> Result<MonotonicTick, ServerFailure> {
        self.tick()
            .map_err(|_| failure(ServerFailureClass::Exhausted))
    }
}
impl RuntimeClock for NativeRuntimeClock {
    fn now(&self) -> Result<MonotonicTick, ObservationError> {
        self.tick()
    }
}

/// Install this same Arc as the factory, launch preparation and retained cleanup provider.
/// Native construction stays on the terminal worker. Filesystem preparation and owner joins
/// run on blocking workers. Handles and subscriptions never own retirement authority.
pub struct NativeRuntime {
    server: ServerId,
    uid: u32,
    account: Arc<dyn AccountLaunchProvider>,
    factory: RuntimeFactory,
    pty: Arc<dyn NativePtyAdapterFactory>,
    this: Weak<Self>,
    handles: Mutex<BTreeMap<SessionKey, Weak<NativeHandle>>>,
}
impl NativeRuntime {
    pub fn new(
        server: ServerId,
        uid: u32,
        account: Arc<dyn AccountLaunchProvider>,
        clock: Arc<dyn RuntimeClock>,
        limits: RuntimeLimits,
    ) -> Result<Arc<Self>, ServerFailure> {
        #[cfg(target_os = "macos")]
        let host = spaceterm_host::pty::macos_pty_host::MacosPtyHost;
        #[cfg(target_os = "linux")]
        let host = spaceterm_host::pty::linux_pty_host::LinuxPtyHost;
        Self::with_pty_factory(
            server,
            uid,
            account,
            clock,
            limits,
            Arc::new(UnixNativePtyAdapterFactory::new(Arc::new(host))),
        )
    }

    /// Replaces only native process resources. Launch preparation, admission,
    /// the terminal engine and retained runtime ownership remain unchanged.
    pub fn with_pty_factory(
        server: ServerId,
        uid: u32,
        account: Arc<dyn AccountLaunchProvider>,
        clock: Arc<dyn RuntimeClock>,
        limits: RuntimeLimits,
        pty: Arc<dyn NativePtyAdapterFactory>,
    ) -> Result<Arc<Self>, ServerFailure> {
        let factory = RuntimeFactory::new(limits, clock).map_err(runtime_failure)?;
        Ok(Arc::new_cyclic(|this| Self {
            server,
            uid,
            account,
            factory,
            pty,
            this: this.clone(),
            handles: Mutex::default(),
        }))
    }
    fn prepare_account(
        &self,
        request: &RuntimeStartRequest,
    ) -> Result<PreparedAccountLaunch, ServerFailure> {
        if request.reservation().session().pane.server() != self.server {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        let account = self.account.account()?;
        // Startup supplies the validated registration UID. This adapter never switches accounts.
        if account.uid != self.uid
            || self.uid == 0
            || self.uid == u32::MAX
            || !account.home.is_absolute()
        {
            return Err(failure(ServerFailureClass::NativeUnavailable));
        }
        let directories = NativeDirectoryProvider::new(self.server, account.home.clone())
            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
        let prompt_owner = PromptOwner::generate()
            .ok_or_else(|| failure(ServerFailureClass::NativeUnavailable))?;
        let mut launch =
            AccountShellLaunch::prepare(&account.planner, &directories, request, &prompt_owner)?;
        // A service manager's inherited HOME is not the owning account's home.
        launch.launch = launch.launch.with_environment_value(
            "HOME",
            account
                .home
                .to_str()
                .ok_or_else(|| failure(ServerFailureClass::RuntimeUnavailable))?,
        );
        let directory = std::str::from_utf8(request.directory().bytes())
            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
        let machine = LocalMachine::new(
            account.machine.user(),
            account.machine.hostname(),
            account.home.to_str(),
        );
        let metadata =
            TerminalMetadataContext::local(LocalPathSemantics::Posix, directory, machine)
                .with_prompt_owner(prompt_owner);
        Ok(PreparedAccountLaunch {
            runtime: self
                .this
                .upgrade()
                .ok_or_else(|| failure(ServerFailureClass::RuntimeUnavailable))?,
            request: request.clone(),
            launch,
            engine: SemanticRuntimeEngine::launch(
                metadata,
                LocalFilesystemAuthority::new(
                    LocalPathSemantics::Posix,
                    Arc::new(UnixLocalIdentity),
                ),
            ),
        })
    }
    async fn start_prepared(
        &self,
        prepared: PreparedAccountLaunch,
        gate: Arc<LaunchGate>,
    ) -> Result<Box<dyn RuntimeOwnerPort>, ServerFailure> {
        let mut owner = self
            .factory
            .start(
                prepared.request,
                prepared.launch.runtime_launch(gate, self.pty.clone()),
                prepared.engine,
            )
            .map_err(runtime_failure)?;
        let native_subscription = owner.take_native_requests().map_err(runtime_failure)?;
        // The actor installs authority as soon as this port returns. Starting owners
        // still compare the original startup incarnation, so do not race that installation
        // with the worker's process-creation boundary. Failure still returns its retained
        // terminal owner and cleanup facts, never a post-admission absence claim.
        let owner = tokio::task::spawn_blocking(move || {
            let _ = owner.wait_until_running();
            owner
        })
        .await
        .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
        let handle = Arc::new(NativeHandle(owner.handle(), Mutex::default()));
        let session = owner.session();
        let mut handles = self
            .handles
            .lock()
            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
        handles.retain(|_, handle| handle.strong_count() != 0);
        handles.insert(session, Arc::downgrade(&handle));
        drop(handles);
        Ok(Box::new(NativeOwner {
            owner: Some(owner),
            joining: None,
            handle,
            native: Arc::new(Mutex::new(NativeRoute {
                subscription: Some(native_subscription),
                fence: None,
            })),
            runtime: self.this.clone(),
        }))
    }
}
impl RuntimeFactoryPort for NativeRuntime {
    fn start<'a>(
        &'a self,
        request: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async move {
            let prepared = self
                .prepare(request)
                .await
                .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
            let factory: Arc<dyn RuntimeFactoryPort> = self
                .this
                .upgrade()
                .ok_or_else(|| failure(ServerFailureClass::RuntimeUnavailable))?;
            prepared
                .start(factory, request.clone(), Arc::new(LaunchGate::default()))
                .await
        })
    }
}
impl LaunchPreparationPort for NativeRuntime {
    fn prepare<'a>(
        &'a self,
        request: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn PreparedLaunchPort>, OperationFailure>> {
        let runtime = self.this.upgrade();
        let request = request.clone();
        Box::pin(async move {
            let prepare = tokio::task::spawn_blocking(move || {
                runtime
                    .ok_or_else(|| failure(ServerFailureClass::RuntimeUnavailable))?
                    .prepare_account(&request)
            });
            let result = prepare
                .await
                .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))
                .and_then(|v| v);
            result
                .map(|v| Box::new(v) as Box<dyn PreparedLaunchPort>)
                .map_err(preparation_failure)
        })
    }
}
struct PreparedAccountLaunch {
    runtime: Arc<NativeRuntime>,
    request: RuntimeStartRequest,
    launch: AccountShellLaunch,
    engine: EngineLaunchContext,
}
impl PreparedLaunchPort for PreparedAccountLaunch {
    fn start(
        self: Box<Self>,
        factory: Arc<dyn RuntimeFactoryPort>,
        request: RuntimeStartRequest,
        gate: Arc<LaunchGate>,
    ) -> ServerFuture<'static, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async move {
            let expected: Arc<dyn RuntimeFactoryPort> = self.runtime.clone();
            if !Arc::ptr_eq(&factory, &expected) || request != self.request {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            }
            let runtime = self.runtime.clone();
            runtime.start_prepared(*self, gate).await
        })
    }
}
impl RuntimeCleanupPort for NativeRuntime {
    fn abandon(
        &self,
        owner: Box<dyn RuntimeOwnerPort>,
    ) -> ServerFuture<'static, Result<SessionCleanupObservation, ServerFailure>> {
        let handle = self
            .handles
            .lock()
            .ok()
            .and_then(|v| v.get(&owner.session()).and_then(Weak::upgrade));
        let matches = handle.as_ref().is_some_and(|handle| {
            let expected: Arc<dyn RuntimeHandlePort> = handle.clone();
            Arc::ptr_eq(&owner.handle(), &expected)
        });
        Box::pin(async move {
            let handle = tokio::task::spawn_blocking(move || {
                // RuntimeOwner Drop seals authority and joins the worker, reader and native cleanup.
                // It uses RuntimeFailure, not invented management or durable-operation authority.
                drop(owner);
                if !matches {
                    return Err(failure(ServerFailureClass::ScopeMismatch));
                }
                handle.ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))
            })
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))??;
            handle.finalized_cleanup().await
        })
    }
}

impl AccountShellLaunch {
    fn runtime_launch(
        self,
        gate: Arc<LaunchGate>,
        pty: Arc<dyn NativePtyAdapterFactory>,
    ) -> PreparedRuntimeLaunch {
        Box::new(move |size| {
            if native_size(size) != self.size {
                return Err(RuntimeFailure::new(
                    FailureClass::InvalidRequest,
                    RuntimeStage::Launch,
                ));
            }
            self.directory.revalidate().map_err(|_| {
                RuntimeFailure::new(FailureClass::DirectoryUnavailable, RuntimeStage::Launch)
            })?;
            gate.admit_process().map_err(|_| {
                RuntimeFailure::new(FailureClass::StaleAuthority, RuntimeStage::Launch)
            })?;
            let parts = pty
                .create(self.launch, native_size(size))
                .map_err(|error| {
                    RuntimeFailure::new(
                        FailureClass::LaunchUnavailable,
                        match error {
                            NativePtyAdapterConstructionFailure::ProcessOwnershipFailed
                            | NativePtyAdapterConstructionFailure::InputOutputUnavailable => {
                                RuntimeStage::Cleanup
                            }
                            _ => RuntimeStage::Launch,
                        },
                    )
                })?;
            Ok(PtyParts {
                process: Box::new(NativeProcess(parts.adapter)),
                termination: Arc::new(NativeTermination(parts.termination)),
            })
        })
    }
}
fn native_size(size: PtySize) -> NativePtySize {
    NativePtySize {
        rows: size.rows,
        columns: size.columns,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}
struct NativeProcess(Box<dyn NativePtyAdapter>);
impl Write for NativeProcess {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
impl PtyProcess for NativeProcess {
    fn take_reader(&mut self) -> Result<Box<dyn Read + Send>, RuntimeFailure> {
        self.0
            .take_reader()
            .map_err(|_| native_failure(RuntimeStage::Reader))
    }
    fn resize(&self, size: PtySize) -> Result<(), RuntimeFailure> {
        self.0
            .resize(native_size(size))
            .map_err(|_| native_failure(RuntimeStage::Resize))
    }
    fn hidden_input(&self) -> Result<bool, RuntimeFailure> {
        self.0
            .hidden_input()
            .map_err(|_| native_failure(RuntimeStage::Engine))
    }
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<PtyExit, RuntimeFailure> {
        match self.0.wait_for_exit(timeout) {
            Ok(exit) => Ok(PtyExit::Exited {
                status: match exit {
                    NativePtyExit::Success => Some(0),
                    NativePtyExit::ExitCode(code) => i32::try_from(code).ok(),
                    _ => None,
                },
            }),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(PtyExit::StillRunning),
            Err(_) => Err(native_failure(RuntimeStage::Cleanup)),
        }
    }
}
struct NativeTermination(Arc<dyn NativePtyTermination>);
impl PtyTermination for NativeTermination {
    fn request_termination(&self) -> Result<(), RuntimeFailure> {
        self.0
            .request_termination()
            .map_err(|_| native_failure(RuntimeStage::Cleanup))?;
        self.retry_cleanup()
    }
    fn retry_cleanup(&self) -> Result<(), RuntimeFailure> {
        match self.0.settle() {
            NativePtySettlement::Settled => Ok(()),
            NativePtySettlement::StillRunning | NativePtySettlement::Unavailable => {
                Err(native_failure(RuntimeStage::Cleanup))
            }
        }
    }
}
fn native_failure(stage: RuntimeStage) -> RuntimeFailure {
    RuntimeFailure::new(FailureClass::RuntimeUnavailable, stage)
}
fn runtime_failure(error: RuntimeFailure) -> ServerFailure {
    failure(match error.class {
        FailureClass::StaleAuthority => ServerFailureClass::StaleAuthority,
        FailureClass::ResourceLimit => ServerFailureClass::Capacity,
        FailureClass::AuthorizationDenied => ServerFailureClass::NativeUnavailable,
        _ if error.stage == RuntimeStage::Cleanup => ServerFailureClass::CleanupIncomplete,
        _ => ServerFailureClass::RuntimeUnavailable,
    })
}
fn preparation_failure(error: ServerFailure) -> OperationFailure {
    match error.class {
        ServerFailureClass::StaleAuthority => OperationFailure::StaleAuthority,
        ServerFailureClass::Capacity => OperationFailure::Capacity,
        ServerFailureClass::NativeUnavailable | ServerFailureClass::ScopeMismatch => {
            OperationFailure::Runtime(FailureClass::AuthorizationDenied)
        }
        _ => OperationFailure::DirectoryUnavailable,
    }
}

const CLEANUP_FINALIZATION_TIMEOUT: Duration = Duration::from_secs(60);

// The factory retains failed foreground cleanup and re-proves the same native
// Session. Snapshots and explicit joins must share one deadline and final result
// so the actor cannot record a transient failure or conflicting late evidence.
#[derive(Default)]
struct CleanupFinalization {
    deadline: Option<Instant>,
    initial: Option<SessionCleanupObservation>,
    finalized: Option<Result<SessionCleanupObservation, ServerFailure>>,
}
impl CleanupFinalization {
    fn begin(&mut self, now: Instant) {
        self.deadline
            .get_or_insert(now + CLEANUP_FINALIZATION_TIMEOUT);
    }
    fn observe(
        &mut self,
        current: Option<SessionCleanupObservation>,
        now: Instant,
    ) -> Result<Option<SessionCleanupObservation>, ServerFailure> {
        use spaceterm_core::retirement::CleanupResult;
        if let Some(finalized) = self.finalized {
            return finalized.map(Some);
        }
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            let finalized = self
                .initial
                .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete));
            self.finalized = Some(finalized);
            return finalized.map(Some);
        }
        let Some(current) = current else {
            return Ok(None);
        };
        if self.initial.is_some_and(|initial| {
            current.session() != initial.session()
                || current.first_reason() != initial.first_reason()
        }) {
            let error = failure(ServerFailureClass::ScopeMismatch);
            self.finalized = Some(Err(error));
            return Err(error);
        }
        if matches!(current.result(), CleanupResult::Confirmed(_)) {
            self.finalized = Some(Ok(current));
            return Ok(Some(current));
        }
        self.initial.get_or_insert(current);
        self.begin(now);
        Ok(None)
    }
}

struct NativeHandle(RuntimeHandle, Mutex<CleanupFinalization>);
impl NativeHandle {
    fn cleanup_snapshot(&self) -> Result<Option<SessionCleanupObservation>, ServerFailure> {
        self.1
            .lock()
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?
            .observe(self.0.cleanup_snapshot(), Instant::now())
    }
    async fn finalized_cleanup(&self) -> Result<SessionCleanupObservation, ServerFailure> {
        self.1
            .lock()
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?
            .begin(Instant::now());
        loop {
            if let Some(current) = self.cleanup_snapshot()? {
                return Ok(current);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
fn reply<T: Send + 'static>(
    reply: RuntimeReply<T>,
) -> ServerFuture<'static, Result<T, ServerFailure>> {
    Box::pin(async move { reply.receive().await.map_err(runtime_failure) })
}
impl RuntimeHandlePort for NativeHandle {
    fn try_submit(
        &self,
        command: FencedRuntimeCommand,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        self.0
            .try_submit(command.fence, command.command)
            .map(reply)
            .map_err(runtime_failure)
    }
    fn complete_native(
        &self,
        completion: NativeCompletion,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        self.0
            .try_submit(
                completion.fence(),
                RuntimeCommand::CompleteNativeRequest(completion),
            )
            .map(reply)
            .map_err(runtime_failure)
    }
    fn read_page(
        &self,
        request: ContentPageRequest,
    ) -> Result<ServerFuture<'_, Result<ContentPageResult, ServerFailure>>, ServerFailure> {
        self.0
            .read_page(request)
            .map(reply)
            .map_err(runtime_failure)
    }
    fn read_topology(
        &self,
        request: TopologyPageRequest,
    ) -> Result<ServerFuture<'_, Result<TopologyPageResult, ServerFailure>>, ServerFailure> {
        self.0
            .read_topology(request)
            .map(reply)
            .map_err(runtime_failure)
    }
    fn prepare_copy(
        &self,
        request: CopyRequest,
    ) -> Result<Box<dyn RuntimeStream<CopyStreamItem>>, ServerFailure> {
        self.0
            .prepare_copy(request)
            .map(|v| Box::new(NativeResourceStream(v)) as Box<dyn RuntimeStream<_>>)
            .map_err(runtime_failure)
    }
    fn find(
        &self,
        request: FindRequest,
    ) -> Result<Box<dyn RuntimeStream<FindStreamItem>>, ServerFailure> {
        self.0
            .find(request)
            .map(|v| Box::new(NativeResourceStream(v)) as Box<dyn RuntimeStream<_>>)
            .map_err(runtime_failure)
    }
    fn read_graphics(
        &self,
        request: GraphicsRequest,
    ) -> Result<Box<dyn RuntimeStream<GraphicsChunk>>, ServerFailure> {
        self.0
            .read_graphics(request)
            .map(|v| Box::new(NativeResourceStream(v)) as Box<dyn RuntimeStream<_>>)
            .map_err(runtime_failure)
    }
}
struct NativeResourceStream<T>(ResourceStream<T>);
impl<T: Send + 'static> RuntimeStream<T> for NativeResourceStream<T> {
    fn next(&mut self) -> ServerFuture<'_, Option<Result<T, ServerFailure>>> {
        Box::pin(async move {
            self.0
                .next()
                .await
                .map(|v| v.map(Ok))
                .unwrap_or_else(|e| Some(Err(runtime_failure(e))))
        })
    }
}

type OwnerJoin = tokio::task::JoinHandle<(
    RuntimeOwner,
    Result<SessionCleanupObservation, RuntimeFailure>,
)>;

struct NativeOwner {
    owner: Option<RuntimeOwner>,
    joining: Option<OwnerJoin>,
    handle: Arc<NativeHandle>,
    native: Arc<Mutex<NativeRoute>>,
    runtime: Weak<NativeRuntime>,
}
impl Drop for NativeOwner {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade()
            && let Ok(mut handles) = runtime.handles.lock()
        {
            handles.remove(&self.session());
        }
    }
}
impl RuntimeObservationSource for NativeOwner {
    fn session(&self) -> SessionKey {
        self.handle.0.session()
    }
    fn display_directory_snapshot(&self) -> Option<DisplayDirectoryObservation> {
        self.handle.0.display_directory_snapshot()
    }
    fn confirmed_directory_snapshot(&self) -> Option<ConfirmedDirectoryObservation> {
        self.handle.0.confirmed_directory_snapshot()
    }
    fn close_work_snapshot(&self) -> Option<CloseWorkObservation> {
        self.handle.0.close_work_snapshot()
    }
    fn cleanup_snapshot(&self) -> Option<SessionCleanupObservation> {
        self.handle.cleanup_snapshot().ok().flatten()
    }
}
impl RuntimeOwnerPort for NativeOwner {
    fn handle(&self) -> Arc<dyn RuntimeHandlePort> {
        self.handle.clone()
    }
    fn subscribe(
        &self,
        fence: EffectFence,
    ) -> Result<Box<dyn RuntimeStream<RuntimePresentationEvent>>, ServerFailure> {
        let owner = self
            .owner
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))?;
        let presentation = owner.subscribe(fence).map_err(runtime_failure)?;
        let mut native = self
            .native
            .lock()
            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
        native.fence = Some(fence);
        drop(native);
        Ok(Box::new(NativePresentationStream {
            presentation,
            handle: self.handle.0.clone(),
            native: self.native.clone(),
            fence,
            pending: VecDeque::new(),
        }))
    }
    fn lifecycle_snapshot(&self) -> RuntimeLifecycle {
        self.handle.0.lifecycle_snapshot()
    }
    fn metadata_snapshot(&self) -> Option<RuntimeMetadataSnapshot> {
        self.handle.0.metadata_snapshot()
    }
    fn transition_authority(
        &mut self,
        transition: RuntimeAuthorityTransition,
    ) -> Result<ServerFuture<'_, Result<AuthorityApplied, ServerFailure>>, ServerFailure> {
        // Install the persistent native route before native effects become eligible.
        let owner = self
            .owner
            .as_mut()
            .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))?;
        let mut route = self
            .native
            .lock()
            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
        if route.subscription.is_none() {
            route.subscription = Some(owner.take_native_requests().map_err(runtime_failure)?);
        }
        route.fence = None;
        drop(route);
        owner
            .transition_authority(transition)
            .map(reply)
            .map_err(runtime_failure)
    }
    fn retire<'a>(
        &'a mut self,
        request: RetirementRequest<'a>,
    ) -> ServerFuture<'a, Result<SessionCleanupObservation, ServerFailure>> {
        let reason = request
            .sessions()
            .iter()
            .find(|v| v.session() == self.session())
            .map(|v| v.first_reason());
        Box::pin(async move {
            let reason = reason.ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?;
            if self.joining.is_none() {
                let mut owner = self
                    .owner
                    .take()
                    .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))?;
                owner.retire(reason);
                // Retain the join on the port, not in its cancellable waiter.
                self.joining = Some(tokio::task::spawn_blocking(move || {
                    let result = owner.join();
                    (owner, result)
                }));
            }
            let joined = self
                .joining
                .as_mut()
                .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))?
                .await;
            self.joining = None;
            let (owner, result) =
                joined.map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
            self.owner = Some(owner);
            result.map_err(runtime_failure)?;
            self.handle.finalized_cleanup().await
        })
    }
}

#[derive(Default)]
struct NativeRoute {
    subscription: Option<NativeRequestSubscription>,
    fence: Option<EffectFence>,
}
struct NativePresentationStream {
    presentation: PresentationSubscription,
    handle: RuntimeHandle,
    native: Arc<Mutex<NativeRoute>>,
    fence: EffectFence,
    pending: VecDeque<RuntimePresentationEvent>,
}
impl NativePresentationStream {
    fn native_event(&self) -> Result<Option<RuntimePresentationEvent>, ServerFailure> {
        let mut route = self
            .native
            .lock()
            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
        if route.fence != Some(self.fence) {
            return Ok(None);
        }
        if let Some(subscription) = &mut route.subscription {
            while let Some(request) = subscription.try_next().map_err(runtime_failure)? {
                if request.fence() == self.fence {
                    return Ok(Some(RuntimePresentationEvent::NativeRequest(request)));
                }
            }
        }
        Ok(None)
    }
    fn snapshots(&mut self) {
        self.pending.push_back(RuntimePresentationEvent::Lifecycle(
            self.handle.lifecycle_snapshot(),
        ));
        if let Some(v) = self.handle.metadata_snapshot() {
            self.pending
                .push_back(RuntimePresentationEvent::Metadata(v));
        }
        if let Some(v) = self.handle.display_directory_snapshot() {
            self.pending
                .push_back(RuntimePresentationEvent::DisplayDirectory(v));
        }
        if let Some(v) = self.handle.confirmed_directory_snapshot() {
            self.pending
                .push_back(RuntimePresentationEvent::ConfirmedDirectory(v));
        }
        if let Some(v) = self.handle.close_work_snapshot() {
            self.pending
                .push_back(RuntimePresentationEvent::CloseWork(v));
        }
    }
}
impl RuntimeStream<RuntimePresentationEvent> for NativePresentationStream {
    fn next(
        &mut self,
    ) -> ServerFuture<'_, Option<Result<RuntimePresentationEvent, ServerFailure>>> {
        Box::pin(async move {
            loop {
                if let Err(error) = self.presentation.check_current() {
                    return Some(Err(runtime_failure(error)));
                }
                if let Some(event) = self.pending.pop_front() {
                    return Some(Ok(event));
                }
                match self.native_event() {
                    Ok(Some(v)) => return Some(Ok(v)),
                    Err(e) => return Some(Err(e)),
                    Ok(None) => {}
                }
                tokio::select! {
                    update = self.presentation.next() => match update {
                        Ok(Some(_)) => {
                            // Retained content is a comparison base, never a subscriber viewport.
                            if let Some(base) = self.handle.content_base_snapshot() {
                                self.snapshots();
                                return Some(Ok(RuntimePresentationEvent::ContentInvalidated(base)));
                            }
                        }
                        Ok(None) => return None,
                        Err(e) => return Some(Err(runtime_failure(e))),
                    },
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                }
            }
        })
    }
}

// Reuse the desktop's UnixLocalIdentity mechanics. O_NONBLOCK prevents a substituted FIFO
// from blocking account launch; opening retains object identity without reading any contents.
struct UnixLocalIdentity;
impl LocalIdentitySource for UnixLocalIdentity {
    fn identify(&self, path: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(path)
            .map_err(classify_io_error)?;
        let metadata = file.metadata().map_err(classify_io_error)?;
        let kind = if metadata.is_dir() {
            LocalObjectKind::Directory
        } else if metadata.is_file() {
            LocalObjectKind::File
        } else {
            LocalObjectKind::Other
        };
        Ok(LocalIdentityObservation {
            identity: LocalObjectIdentity::from_file(file)?,
            kind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::{
        identity::PaneId,
        retirement::{CleanupResult, ConfirmedCleanupClass},
    };

    fn cleanup(result: CleanupResult) -> SessionCleanupObservation {
        SessionCleanupObservation::new(
            SessionKey {
                pane: PaneId::new(ServerId::new([1; 16]), 1).unwrap(),
                epoch: ServerEpoch::new(1),
                generation: SessionGeneration::new(1),
            },
            spaceterm_core::organization::RetirementReason::OrdinaryExit,
            result,
        )
        .unwrap()
    }

    #[test]
    fn cleanup_finalization_withholds_failure_until_original_session_reproof() {
        let mut state = CleanupFinalization::default();
        let now = Instant::now();
        let failed = cleanup(CleanupResult::Failed(FailureClass::RuntimeUnavailable));
        let confirmed = cleanup(CleanupResult::Confirmed(
            ConfirmedCleanupClass::ReapedAndReleased,
        ));
        assert_eq!(state.observe(Some(failed), now), Ok(None));
        assert_eq!(
            state.observe(Some(confirmed), now + Duration::from_secs(4)),
            Ok(Some(confirmed))
        );
        assert_eq!(
            state.observe(Some(failed), now + Duration::from_secs(5)),
            Ok(Some(confirmed))
        );
    }

    #[test]
    fn cleanup_finalization_timeout_keeps_failure_after_late_reproof() {
        let mut state = CleanupFinalization::default();
        let now = Instant::now();
        let failed = cleanup(CleanupResult::Failed(FailureClass::RuntimeUnavailable));
        let confirmed = cleanup(CleanupResult::Confirmed(
            ConfirmedCleanupClass::ReapedAndReleased,
        ));
        assert_eq!(state.observe(Some(failed), now), Ok(None));
        assert_eq!(
            state.observe(Some(failed), now + CLEANUP_FINALIZATION_TIMEOUT),
            Ok(Some(failed))
        );
        assert_eq!(
            state.observe(
                Some(confirmed),
                now + CLEANUP_FINALIZATION_TIMEOUT + Duration::from_secs(1)
            ),
            Ok(Some(failed))
        );
    }

    #[test]
    fn cleanup_finalization_retry_does_not_refresh_deadline() {
        let mut state = CleanupFinalization::default();
        let now = Instant::now();
        let failed = cleanup(CleanupResult::Failed(FailureClass::RuntimeUnavailable));
        assert_eq!(state.observe(Some(failed), now), Ok(None));
        state.begin(now + Duration::from_secs(50));
        assert_eq!(
            state.observe(Some(failed), now + CLEANUP_FINALIZATION_TIMEOUT),
            Ok(Some(failed))
        );
    }

    #[test]
    fn cleanup_finalization_rejects_changed_session_or_first_reason() {
        let failed = cleanup(CleanupResult::Failed(FailureClass::RuntimeUnavailable));
        for (session, reason) in [
            (
                SessionKey {
                    generation: SessionGeneration::new(2),
                    ..failed.session()
                },
                failed.first_reason(),
            ),
            (
                failed.session(),
                spaceterm_core::organization::RetirementReason::RuntimeFailure,
            ),
        ] {
            let mut state = CleanupFinalization::default();
            let now = Instant::now();
            assert_eq!(state.observe(Some(failed), now), Ok(None));
            let changed = SessionCleanupObservation::new(
                session,
                reason,
                CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
            )
            .unwrap();
            assert_eq!(
                state.observe(Some(changed), now),
                Err(failure(ServerFailureClass::ScopeMismatch))
            );
        }
    }
}
