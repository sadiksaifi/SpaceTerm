#![cfg(any(target_os = "macos", target_os = "linux"))]

//! Selected-server acceptance through signed IPC, the real actor/reducer/ledger,
//! SQLite, native Git discovery, native PTYs and the semantic terminal worker.
//! Native startup authority and initial pairing are test composition only.

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::*,
    content::{ContentBase, ContentPage, Screen},
    content_queries::*,
    control::*,
    geometry::*,
    identity::*,
    native_management::*,
    native_requests::NativeCompletion,
    observations::*,
    operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*,
    operations::*,
    organization::*,
    organization_retained::{
        OrganizationRetainedFacts, RepositoryInventoryVersion, RetainedOrganizationWrite,
        WorktreeInventoryFacts,
    },
    pairing::{policy::PairingSnapshot, *},
    retirement::*,
    runtime_authority::*,
    storage::*,
    worktrees::intent::{GitRepositoryMutation, RepositoryLocator, RepositoryLocatorParts},
};
use spaceterm_host::{
    account_service::ServiceSupervisor,
    git,
    ipc::{IpcListener, IpcTransport, PrivateIpcDirectory},
    organization_store::{bootstrap::*, *},
    pty::*,
    server_directories::native::NativeDirectoryProvider,
    shell_launch::ShellLaunchPlanner,
    startup::{AccountBuildKey, BuildIdentity},
    storage::Database,
    transport::{AdmissionLimits, MessageLimit, MessageTransport},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, SigningKey},
    codec::Payload,
    connection::AuthenticatedConnection,
    convert::operations,
    wire::{v1, v2},
};
use spaceterm_server::{
    adapters::{
        core::{CommitCursor, CorePorts},
        store::production_services,
        terminal::*,
    },
    api::*,
    connection::{AuthenticatedPeer, AuthenticationConfig, Authenticator, PeerConnection},
    contracts::*,
    extensions::ExtensionObserverPort,
    failure::failure,
    git::host::NativeServerGitHost,
    pairing::protocol_lookup::{PairingProtocolLookup, PairingSnapshotPort},
    protocol_dispatch::{ActorOrdinaryDispatch, OrdinaryDispatchPort},
    service::RecoveredServer,
    shutdown::*,
};
use spaceterm_terminal::{
    appearance::{Appearance, AppearanceGeneration},
    content::runtime::SemanticRuntimeEngine,
    local_filesystem::{
        LocalFilesystemAuthority, LocalFilesystemError, LocalIdentityObservation,
        LocalIdentitySource,
    },
    local_path::LocalPathSemantics,
    metadata::{LocalMachine, PromptOwner, TerminalMetadataContext},
    runtime::{
        budget::RuntimeLimits,
        ingress::ResourceStream,
        owner::{RuntimeFactory, RuntimeHandle, RuntimeOwner},
        ports::*,
    },
    runtime_contracts::*,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Notify, Semaphore};

const DEADLINE: Duration = Duration::from_secs(15);

struct Key(Ed25519KeyPair);
impl Key {
    fn new() -> Arc<Self> {
        let mut seed = [0; 32];
        getrandom::fill(&mut seed).unwrap();
        Arc::new(Self(Ed25519KeyPair::from_seed_unchecked(&seed).unwrap()))
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
impl ServerCredentialPort for Key {}

struct Clock(Instant);
impl ServerClock for Clock {
    fn now(&self) -> Result<MonotonicTick, ServerFailure> {
        Ok(MonotonicTick::new(
            self.0.elapsed().as_millis().try_into().unwrap(),
        ))
    }
}
impl RuntimeClock for Clock {
    fn now(&self) -> Result<MonotonicTick, ObservationError> {
        Ok(MonotonicTick::new(
            self.0.elapsed().as_millis().try_into().unwrap(),
        ))
    }
}

// This fixture never claims stale native process cleanup: fresh recovery and
// orderly reopen must have no unresolved Session ownership.
struct NativeAuthority(StoreIdentity);
impl StartupAuthorityPort for NativeAuthority {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        Ok(self.0.server)
    }
    fn validate_credentials(
        &self,
        key: &dyn ServerCredentialPort,
        server: ServerId,
    ) -> Result<(), ServerFailure> {
        assert_eq!(server, self.0.server);
        assert_eq!(key.public_key(), self.0.server_public_key);
        Ok(())
    }
}
impl HostManagementPort for NativeAuthority {
    fn previous_instance_cleanup<'a>(
        &'a self,
        server: ServerId,
        _: ServerEpoch,
        expected: &'a [SessionKey],
    ) -> ServerFuture<'a, Result<Vec<SessionCleanupObservation>, ServerFailure>> {
        Box::pin(async move {
            assert_eq!(server, self.0.server);
            assert!(
                expected.is_empty(),
                "orderly fixture reopen has unresolved cleanup"
            );
            Ok(Vec::new())
        })
    }
}
impl StoppedAuthorizationPort for NativeAuthority {
    fn verify_stopped(
        &self,
        server: ServerId,
        _: ServerEpoch,
    ) -> Result<(NativeManagementRequestRef, NativeRetirementAction), ServerFailure> {
        assert_eq!(server, self.0.server);
        Ok((
            NativeManagementRequestRef::new(server, [9; 16], RequestDigest::from_bytes([8; 32]))
                .unwrap(),
            NativeRetirementAction::Stop,
        ))
    }
}
struct NoExternalWork;
impl ExternalWorkPort for NoExternalWork {
    fn cancel_reads(&self) {}
    fn finish_started_writes(&self) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async { Ok(()) })
    }
    fn reconcile<'a>(
        &'a self,
        state: &'a RecoveredState,
    ) -> ServerFuture<'a, Result<RecoveryEdits, ServerFailure>> {
        Box::pin(async move {
            assert!(state.git_intents.is_empty());
            Ok(RecoveryEdits {
                transactions: Vec::new(),
            })
        })
    }
}
struct NoExtensions;
impl ExtensionObserverPort for NoExtensions {
    fn admitted(
        &self,
        _: CanonicalOperationRequest,
        _: CommitReceipt,
        _: ServerHandle,
    ) -> Result<(), ServerFailure> {
        Err(failure(ServerFailureClass::ScopeMismatch))
    }
    fn pairing_committed(&self, _: &PairingWrites, _: CommitReceipt) -> Result<(), ServerFailure> {
        panic!("fixture does not mutate pairing")
    }
    fn confirmed_directory_committed(&self, _: &ConfirmedDirectoryObservation, _: CommitReceipt) {}
}
struct Outcomes;
impl ReliableOutcomePort for Outcomes {
    fn reserve(
        &self,
        _: VerifiedPrincipal,
        _: OperationBinding,
    ) -> Result<Box<dyn ReliableOutcomePermit>, ServerFailure> {
        Ok(Box::new(Self))
    }
}
impl ReliableOutcomePermit for Outcomes {
    fn publish(&mut self, _: &OperationResponse) -> Result<(), ServerFailure> {
        Ok(())
    }
}
struct Lookup(PairingSnapshot);
impl authentication::PairingLookup for Lookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<authentication::PairingRecord, ProtocolError> {
        struct Borrowed<'a>(&'a Lookup);
        impl PairingSnapshotPort for Borrowed<'_> {
            fn snapshot(&self) -> Result<PairingSnapshot, ProtocolError> {
                Ok(self.0.0.clone())
            }
        }
        PairingProtocolLookup::new(Borrowed(self)).lookup(server, pairing, client)
    }
}
impl PairingAuthorizationPort for Lookup {}

fn runtime_failure(_: impl Sized) -> ServerFailure {
    failure(ServerFailureClass::RuntimeUnavailable)
}
fn pty_failure() -> RuntimeFailure {
    RuntimeFailure::new(FailureClass::RuntimeUnavailable, RuntimeStage::Cleanup)
}

// Only adapt the existing native provider's trait vocabulary. No process,
// settlement, terminal content, or authority policy is simulated here.
struct Process(Box<dyn NativePtyAdapter>);
impl Write for Process {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
impl PtyProcess for Process {
    fn take_reader(&mut self) -> Result<Box<dyn Read + Send>, RuntimeFailure> {
        self.0.take_reader().map_err(|_| pty_failure())
    }
    fn resize(&self, size: PtySize) -> Result<(), RuntimeFailure> {
        self.0
            .resize(NativePtySize {
                rows: size.rows,
                columns: size.columns,
                pixel_width: size.pixel_width,
                pixel_height: size.pixel_height,
            })
            .map_err(|_| pty_failure())
    }
    fn hidden_input(&self) -> Result<bool, RuntimeFailure> {
        self.0.hidden_input().map_err(|_| pty_failure())
    }
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<PtyExit, RuntimeFailure> {
        self.0
            .wait_for_exit(timeout)
            .map(|_| PtyExit::Exited { status: None })
            .map_err(|_| pty_failure())
    }
}
struct Termination(Arc<dyn NativePtyTermination>);
impl PtyTermination for Termination {
    fn request_termination(&self) -> Result<(), RuntimeFailure> {
        self.0.request_termination().map_err(|_| pty_failure())?;
        if self.0.settle() == NativePtySettlement::Settled {
            Ok(())
        } else {
            Err(pty_failure())
        }
    }
}
// Terminal content grants no local file actions in this headless fixture.
struct NoLocalFiles;
impl LocalIdentitySource for NoLocalFiles {
    fn identify(&self, _: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
        Err(LocalFilesystemError::PermissionDenied)
    }
}
struct Handle(RuntimeHandle);
impl RuntimeHandlePort for Handle {
    fn try_submit(
        &self,
        command: FencedRuntimeCommand,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        let reply = self
            .0
            .try_submit(command.fence, command.command)
            .map_err(runtime_failure)?;
        Ok(Box::pin(async {
            reply.receive().await.map_err(runtime_failure)
        }))
    }
    fn complete_native(
        &self,
        completion: NativeCompletion,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        self.try_submit(FencedRuntimeCommand {
            fence: completion.fence(),
            command: RuntimeCommand::CompleteNativeRequest(completion),
        })
    }
    fn read_page(
        &self,
        request: ContentPageRequest,
    ) -> Result<ServerFuture<'_, Result<ContentPageResult, ServerFailure>>, ServerFailure> {
        let reply = self.0.read_page(request).map_err(runtime_failure)?;
        Ok(Box::pin(async {
            reply.receive().await.map_err(runtime_failure)
        }))
    }
    fn read_topology(
        &self,
        request: TopologyPageRequest,
    ) -> Result<ServerFuture<'_, Result<TopologyPageResult, ServerFailure>>, ServerFailure> {
        let reply = self.0.read_topology(request).map_err(runtime_failure)?;
        Ok(Box::pin(async {
            reply.receive().await.map_err(runtime_failure)
        }))
    }
    fn prepare_copy(
        &self,
        request: CopyRequest,
    ) -> Result<Box<dyn RuntimeStream<CopyStreamItem>>, ServerFailure> {
        Ok(Box::new(Stream(
            self.0.prepare_copy(request).map_err(runtime_failure)?,
        )))
    }
    fn find(
        &self,
        request: FindRequest,
    ) -> Result<Box<dyn RuntimeStream<FindStreamItem>>, ServerFailure> {
        Ok(Box::new(Stream(
            self.0.find(request).map_err(runtime_failure)?,
        )))
    }
    fn read_graphics(
        &self,
        request: GraphicsRequest,
    ) -> Result<Box<dyn RuntimeStream<GraphicsChunk>>, ServerFailure> {
        Ok(Box::new(Stream(
            self.0.read_graphics(request).map_err(runtime_failure)?,
        )))
    }
}
struct Stream<T>(ResourceStream<T>);
impl<T: Send + 'static> RuntimeStream<T> for Stream<T> {
    fn next(&mut self) -> ServerFuture<'_, Option<Result<T, ServerFailure>>> {
        Box::pin(async {
            match self.0.next().await {
                Ok(Some(item)) => Some(Ok(item)),
                Ok(None) => None,
                Err(e) => Some(Err(runtime_failure(e))),
            }
        })
    }
}
struct Owner {
    runtime: RuntimeOwner,
    cleanup: Arc<Mutex<Option<SessionCleanupObservation>>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Retain the worker's result only after an explicit native cleanup and
        // join. Dropping an actor handle by itself proves nothing.
        self.runtime.retire(RetirementReason::RuntimeFailure);
        if let Ok(observation) = self.runtime.join() {
            *self.cleanup.lock().unwrap() = Some(observation);
        }
    }
}
impl RuntimeObservationSource for Owner {
    fn session(&self) -> SessionKey {
        self.runtime.session()
    }
    fn display_directory_snapshot(&self) -> Option<DisplayDirectoryObservation> {
        self.runtime.display_directory_snapshot()
    }
    fn confirmed_directory_snapshot(&self) -> Option<ConfirmedDirectoryObservation> {
        self.runtime.confirmed_directory_snapshot()
    }
    fn close_work_snapshot(&self) -> Option<CloseWorkObservation> {
        self.runtime.close_work_snapshot()
    }
    fn cleanup_snapshot(&self) -> Option<SessionCleanupObservation> {
        self.runtime.cleanup_snapshot()
    }
}
impl RuntimeOwnerPort for Owner {
    fn handle(&self) -> Arc<dyn RuntimeHandlePort> {
        Arc::new(Handle(self.runtime.handle()))
    }
    fn subscribe(
        &self,
        _: EffectFence,
    ) -> Result<Box<dyn RuntimeStream<RuntimePresentationEvent>>, ServerFailure> {
        Err(failure(ServerFailureClass::RuntimeUnavailable))
    }
    fn lifecycle_snapshot(&self) -> RuntimeLifecycle {
        self.runtime.lifecycle_snapshot()
    }
    fn metadata_snapshot(&self) -> Option<RuntimeMetadataSnapshot> {
        self.runtime.metadata_snapshot()
    }
    fn transition_authority(
        &mut self,
        transition: RuntimeAuthorityTransition,
    ) -> Result<ServerFuture<'_, Result<AuthorityApplied, ServerFailure>>, ServerFailure> {
        let reply = self
            .runtime
            .transition_authority(transition)
            .map_err(runtime_failure)?;
        Ok(Box::pin(async {
            reply.receive().await.map_err(runtime_failure)
        }))
    }
    fn retire<'a>(
        &'a mut self,
        request: RetirementRequest<'a>,
    ) -> ServerFuture<'a, Result<SessionCleanupObservation, ServerFailure>> {
        let ticket = self.runtime.retire(request.reason());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || ticket.wait_blocking())
                .await
                .unwrap()
                .map_err(runtime_failure)?;
            // The terminal's cleanup ticket is retained independently. Joining
            // here proves the worker thread also finished before actor completion.
            self.runtime.join().map_err(runtime_failure)
        })
    }
}
struct Hold {
    entered: Notify,
    release: Semaphore,
}
impl Hold {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
        })
    }
}
// Panic and timeout must release an owner already launched behind the test gate.
struct HoldRelease(Arc<Hold>);
impl Drop for HoldRelease {
    fn drop(&mut self) {
        self.0.release.add_permits(1);
    }
}
struct Runtimes {
    factory: RuntimeFactory,
    planner: ShellLaunchPlanner,
    directories: NativeDirectoryProvider,
    root: PathBuf,
    handles: Mutex<BTreeMap<SessionKey, Arc<Handle>>>,
    initial_authorities: Mutex<BTreeMap<SessionKey, RuntimeAuthority>>,
    cleanup_evidence: Mutex<BTreeMap<SessionKey, Arc<Mutex<Option<SessionCleanupObservation>>>>>,
    hold: Mutex<Option<Arc<Hold>>>,
}
impl RuntimeFactoryPort for Runtimes {
    fn start<'a>(
        &'a self,
        request: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async move {
            let prompt = PromptOwner::generate().unwrap();
            let retained = self.directories.retain(request.directory()).unwrap();
            let session = request.reservation().session();
            let launch = self
                .planner
                .local(retained.path())
                .unwrap()
                .with_environment_value(PromptOwner::ENVIRONMENT, prompt.value())
                .with_environment_value(
                    "SPACETERM_TEST_PID_FILE",
                    self.pid_file(session).to_str().unwrap(),
                );
            let metadata = TerminalMetadataContext::local(
                LocalPathSemantics::Posix,
                retained.path().to_str().unwrap(),
                LocalMachine::new(None, Some("localhost"), None),
            )
            .with_prompt_owner(prompt);
            #[cfg(target_os = "macos")]
            let host = spaceterm_host::pty::macos_pty_host::MacosPtyHost;
            #[cfg(target_os = "linux")]
            let host = spaceterm_host::pty::linux_pty_host::LinuxPtyHost;
            let native = UnixNativePtyAdapterFactory::new(Arc::new(host));
            let owner = self
                .factory
                .start(
                    request.clone(),
                    Box::new(move |size| {
                        retained.revalidate().map_err(|_| pty_failure())?;
                        let parts = native
                            .create(
                                launch,
                                NativePtySize {
                                    rows: size.rows,
                                    columns: size.columns,
                                    pixel_width: size.pixel_width,
                                    pixel_height: size.pixel_height,
                                },
                            )
                            .map_err(|_| pty_failure())?;
                        Ok(PtyParts {
                            process: Box::new(Process(parts.adapter)),
                            termination: Arc::new(Termination(parts.termination)),
                        })
                    }),
                    SemanticRuntimeEngine::launch(
                        metadata,
                        LocalFilesystemAuthority::new(
                            LocalPathSemantics::Posix,
                            Arc::new(NoLocalFiles),
                        ),
                    ),
                )
                .map_err(runtime_failure)?;
            let owner = tokio::task::spawn_blocking(move || {
                owner.wait_until_running().unwrap();
                owner
            })
            .await
            .unwrap();
            // Each exact captured fence demonstrably admits a real PTY effect
            // before retirement. The delayed-effect assertions are not based on
            // an initially invalid or unrelated Pane fence.
            assert_eq!(
                owner
                    .handle()
                    .try_submit(
                        authority_fence(request.authority(), None),
                        RuntimeCommand::Resize(request.reservation().grid())
                    )
                    .unwrap()
                    .receive()
                    .await
                    .unwrap(),
                EffectDisposition::Applied
            );
            self.handles
                .lock()
                .unwrap()
                .insert(session, Arc::new(Handle(owner.handle())));
            self.initial_authorities
                .lock()
                .unwrap()
                .insert(session, request.authority());
            let cleanup = Arc::new(Mutex::new(None));
            self.cleanup_evidence
                .lock()
                .unwrap()
                .insert(session, cleanup.clone());
            Ok(Box::new(Owner {
                runtime: owner,
                cleanup,
            }) as Box<dyn RuntimeOwnerPort>)
        })
    }
}
impl Runtimes {
    fn pid_file(&self, session: SessionKey) -> PathBuf {
        self.root.join(format!(
            "pid-{}-{}",
            session.pane.local(),
            session.generation.get()
        ))
    }
    async fn pid(&self, session: SessionKey) -> u32 {
        tokio::time::timeout(DEADLINE, async {
            loop {
                if let Some(pid) = fs::read_to_string(self.pid_file(session))
                    .ok()
                    .and_then(|value| value.trim().parse().ok())
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned shell did not publish its process identity")
    }
    fn handle(&self, session: SessionKey) -> Arc<Handle> {
        self.handles.lock().unwrap()[&session].clone()
    }
}
impl RuntimeCleanupPort for Runtimes {
    fn abandon(
        &self,
        owner: Box<dyn RuntimeOwnerPort>,
    ) -> ServerFuture<'static, Result<SessionCleanupObservation, ServerFailure>> {
        let evidence = self.cleanup_evidence.lock().unwrap()[&owner.session()].clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || drop(owner))
                .await
                .map_err(runtime_failure)?;
            evidence
                .lock()
                .unwrap()
                .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))
        })
    }
}
struct Prepared;
impl PreparedLaunchPort for Prepared {
    fn start(
        self: Box<Self>,
        factory: Arc<dyn RuntimeFactoryPort>,
        request: RuntimeStartRequest,
        gate: Arc<LaunchGate>,
    ) -> ServerFuture<'static, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async move {
            gate.admit_process()?;
            factory.start(&request).await
        })
    }
}
struct HeldPrepared(Option<Arc<Hold>>);
impl PreparedLaunchPort for HeldPrepared {
    fn start(
        self: Box<Self>,
        factory: Arc<dyn RuntimeFactoryPort>,
        request: RuntimeStartRequest,
        gate: Arc<LaunchGate>,
    ) -> ServerFuture<'static, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async move {
            let owner = Box::new(Prepared).start(factory, request, gate).await?;
            if let Some(hold) = self.0 {
                hold.entered.notify_one();
                hold.release.acquire().await.unwrap().forget();
            }
            Ok(owner)
        })
    }
}
impl LaunchPreparationPort for Runtimes {
    fn prepare<'a>(
        &'a self,
        _: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn PreparedLaunchPort>, OperationFailure>> {
        Box::pin(async {
            Ok(Box::new(HeldPrepared(self.hold.lock().unwrap().take()))
                as Box<dyn PreparedLaunchPort>)
        })
    }
}

fn directory(server: ServerId, path: &Path) -> ServerDirectory {
    ServerDirectory::new(server, path.as_os_str().as_encoded_bytes().to_vec()).unwrap()
}
fn stream(server: ServerId, client: u64, key: u64) -> OperationStreamId {
    OperationStreamId::new(
        OperationStreamKey::new(server, key).unwrap(),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, client).unwrap(),
    )
    .unwrap()
}
fn local(n: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&n.to_be_bytes());
    bytes
}
fn appearance() -> RuntimeAppearance {
    let black = spaceterm_core::content::Rgb { r: 0, g: 0, b: 0 };
    RuntimeAppearance {
        generation: AppearanceGeneration::INITIAL,
        foreground: spaceterm_core::content::Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        background: black,
        palette: [black; 256],
        bold_as_bright: false,
        appearance: Appearance::Dark,
    }
}
fn git(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let mut command = std::process::Command::new("/usr/bin/git");
    command
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(arguments)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = spaceterm_host::process::spawn(&mut command)
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "fixture Git command failed");
    output.stdout
}
fn process_alive(pid: u32) -> bool {
    let mut command = std::process::Command::new("/bin/kill");
    command
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    spaceterm_host::process::spawn(&mut command)
        .unwrap()
        .wait()
        .unwrap()
        .success()
}

struct Fixture {
    disk: Option<DiskInventory>,
    root: tempfile::TempDir,
    identity: StoreIdentity,
    key: Arc<Key>,
    device: Arc<Key>,
    lookup: Arc<Lookup>,
    runtime: Arc<Runtimes>,
    server: Option<RecoveredServer>,
    native: Arc<NativeAuthority>,
    git_host: Arc<NativeServerGitHost>,
    clock: Arc<Clock>,
    _ipc_root: tempfile::TempDir,
    listener: IpcListener,
    ipc: PrivateIpcDirectory,
    authenticator: Authenticator,
}
impl Fixture {
    async fn new(tag: u8) -> Self {
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let scratch = target.join("selected-server-fixtures");
        fs::create_dir_all(&scratch).unwrap();
        let root = tempfile::tempdir_in(scratch.canonicalize().unwrap()).unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let key = Key::new();
        let device = Key::new();
        let uid = ServiceSupervisor::current().unwrap().key().account_uid;
        let identity = StoreIdentity {
            server: ServerId::new([tag; 16]),
            account: AccountBuildKey {
                account_uid: uid,
                build_identity: BuildIdentity::Development,
            },
            server_public_key: key.public_key(),
        };
        let store_path = root.path().join("store");
        fs::create_dir(&store_path).unwrap();
        fs::set_permissions(&store_path, fs::Permissions::from_mode(0o700)).unwrap();
        let disk = Self::seed_repository(root.path());
        let home = directory(identity.server, root.path());
        let services = production_services(identity, home.clone()).unwrap();
        let db = Database::open_for_application(&store_path, migrations(), schema::APPLICATION_ID)
            .unwrap();
        let InitializationDisposition::Durable { mut store, .. } =
            SqliteOrganizationStore::initialize_new(
                db,
                NewStoreSeed {
                    identity,
                    origin: NewServerOrigin::LocalHeadless,
                },
                StoreLimits::production(),
                production_services(identity, home.clone()).unwrap(),
            )
        else {
            panic!("new store")
        };
        let recovered = store.recover().unwrap();
        let mut parts = CommitCursor::from_recovered(&recovered).parts().unwrap();
        parts.resulting_epoch = ServerEpoch::new(1);
        parts.resulting_high_water.repository = 1;
        parts.resulting_high_water.worktree = 2;
        parts.resulting_organization_revision = parts
            .expected_organization_revision
            .checked_successor()
            .unwrap();
        let repository = RepositoryId::new(identity.server, 1).unwrap();
        parts
            .organization_writes
            .push(OrganizationWrite::PutRepository(RepositoryRecord {
                id: repository,
                directory: directory(identity.server, &disk.repository),
            }));
        parts.organization_writes.push(OrganizationWrite::Retained(
            RetainedOrganizationWrite::SetRepositoryInventoryVersion(
                RepositoryInventoryVersion::new(repository, FieldVersion::new(1)),
            ),
        ));
        for (n, path, main) in [(1, &disk.repository, true), (2, &disk.worktree, false)] {
            let id = WorktreeId::new(identity.server, n).unwrap();
            parts
                .organization_writes
                .push(OrganizationWrite::PutWorktree(WorktreeRecord {
                    id,
                    repository,
                    directory: directory(identity.server, path),
                    main,
                    missing: false,
                }));
            parts.organization_writes.push(OrganizationWrite::Retained(
                RetainedOrganizationWrite::SetWorktreeInventoryFacts(WorktreeInventoryFacts::new(
                    id,
                    true,
                    FieldVersion::new(1),
                )),
            ));
        }
        parts
            .operation_writes
            .push(OperationWrite::GitRepository(GitRepositoryMutation::Put(
                RepositoryLocator::new(RepositoryLocatorParts {
                    scope: RepositoryScope {
                        server: identity.server,
                        repository,
                    },
                    main_root: directory(identity.server, &disk.repository),
                    common_directory: directory(identity.server, &disk.repository.join(".git")),
                })
                .unwrap(),
            )));
        let inventory_transaction =
            DurableTransaction::new_with_validation(parts, &services).unwrap();
        let disposition = store.commit(&inventory_transaction);
        assert!(
            matches!(disposition, CommitDisposition::Durable(_)),
            "inventory seed: {disposition:?}"
        );
        let mut parts = CommitCursor::from_recovered(&store.recover().unwrap())
            .parts()
            .unwrap();
        parts.resulting_high_water.pairing = 1;
        parts.resulting_high_water.client = 2;
        parts.resulting_high_water.operation_stream = 4;
        let mut clients = Vec::new();
        for n in 1..=2 {
            let streams = PairingClientStreams::new(PairingClientStreamsParts {
                client: ClientRecord {
                    id: ClientId::new(identity.server, n).unwrap(),
                    pairing: PairingId::new(identity.server, 1).unwrap(),
                },
                ordinary: stream(identity.server, n, n * 2 - 1),
                self_revocation: stream(identity.server, n, n * 2),
            })
            .unwrap();
            for (s, purpose, reservation) in [
                (streams.parts().ordinary, StreamPurpose::Ordinary, None),
                (
                    streams.parts().self_revocation,
                    StreamPurpose::SelfRevocation,
                    Some(OutcomeReservation::full(
                        ReservationClass::DedicatedSelfRevocation,
                    )),
                ),
            ] {
                parts.operation_writes.push(OperationWrite::Ledger(Box::new(
                    LedgerWrite::RegisterStream(
                        StreamRecord::new(
                            StreamBinding::from_stream(s),
                            purpose,
                            StreamStatus::Active,
                            0,
                            0,
                            reservation,
                        )
                        .unwrap(),
                    ),
                )));
            }
            clients.push(streams);
        }
        let authorization = PairingAuthorization::new(PairingAuthorizationParts {
            record: spaceterm_core::pairing::PairingRecord {
                id: PairingId::new(identity.server, 1).unwrap(),
                device: DeviceId::new([7; 16]),
                state: PairingState::Authorized,
            },
            authorization_version: FieldVersion::new(1),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: Key::new().public_key(),
                pairing_authentication_public_key: device.public_key(),
            })
            .unwrap(),
            clients,
            revocation_receipt: None,
        })
        .unwrap();
        parts
            .operation_writes
            .push(OperationWrite::Pairing(Box::new(
                PairingWrite::PutAuthorization {
                    expected_authorization_version: None,
                    value: authorization,
                },
            )));
        let transaction = DurableTransaction::new_with_validation(parts, &services).unwrap();
        let disposition = store.commit(&transaction);
        assert!(
            matches!(disposition, CommitDisposition::Durable(_)),
            "fixture seed commit: {disposition:?}"
        );
        let recovered = store.recover().unwrap();
        let lookup = Arc::new(Lookup(
            PairingSnapshot::new(
                identity.server,
                recovered.last_epoch,
                recovered.store_revision,
                recovered.high_water,
                recovered.pairing.unwrap(),
            )
            .unwrap(),
        ));
        let shell = root.path().join("shell");
        fs::write(&shell, "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$SPACETERM_TEST_PID_FILE\"\ni=0; while [ \"$i\" -lt 40 ]; do printf 'retained-output-%s\\n' \"$i\"; i=$((i + 1)); done\nwhile IFS= read -r line; do printf 'received:%s\\n' \"$line\"; done\n").unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        let clock = Arc::new(Clock(Instant::now()));
        let directories =
            NativeDirectoryProvider::new(identity.server, root.path().to_path_buf()).unwrap();
        let runtime = Arc::new(Runtimes {
            factory: RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap(),
            planner: ShellLaunchPlanner::for_test(shell, root.path().to_path_buf()),
            directories,
            root: root.path().to_path_buf(),
            handles: Mutex::new(BTreeMap::new()),
            initial_authorities: Mutex::new(BTreeMap::new()),
            cleanup_evidence: Mutex::new(BTreeMap::new()),
            hold: Mutex::new(None),
        });
        let version = git::tools::parse_git_version(&git(root.path(), &["--version"])).unwrap();
        let git_host = Arc::new(NativeServerGitHost::for_account(
            "/usr/bin/git".into(),
            root.path().to_path_buf(),
            version,
            Arc::new(
                NativeDirectoryProvider::new(identity.server, root.path().to_path_buf()).unwrap(),
            ),
        ));
        let native = Arc::new(NativeAuthority(identity));
        let server = Self::recover(
            store,
            identity,
            home,
            key.clone(),
            lookup.clone(),
            runtime.clone(),
            native.clone(),
            git_host.clone(),
            clock.clone(),
        )
        .await;
        let handle = server.handle();
        // Short private endpoints keep this test independent of checkout length.
        let ipc_root = tempfile::Builder::new()
            .prefix("st-sel-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(ipc_root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let ipc = PrivateIpcDirectory::open(ipc_root.path()).unwrap();
        let listener = IpcListener::bind(
            ipc.endpoint("s").unwrap(),
            MessageLimit::envelope(),
            AdmissionLimits::new(4, DEADLINE).unwrap(),
        )
        .unwrap();
        let authenticator = Authenticator::new(AuthenticationConfig {
            server: identity.server,
            epoch: handle.snapshot().epoch(),
            owning_uid: uid,
            handshake: authentication::HandshakeConfig {
                capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
                ..Default::default()
            },
            credentials: key.clone(),
            pairing: lookup.clone(),
            authority: Arc::new(handle),
            unauthenticated_limit: 4,
            authenticated_limit: 4,
            deadline: Duration::from_secs(5),
        })
        .unwrap();
        Self {
            disk: Some(disk),
            root,
            identity,
            key,
            device,
            lookup,
            runtime,
            server: Some(server),
            native,
            git_host,
            clock,
            _ipc_root: ipc_root,
            listener,
            ipc,
            authenticator,
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The test supplies each production port explicitly."
    )]
    async fn recover(
        store: SqliteOrganizationStore,
        identity: StoreIdentity,
        home: ServerDirectory,
        key: Arc<Key>,
        lookup: Arc<Lookup>,
        runtime: Arc<Runtimes>,
        native: Arc<NativeAuthority>,
        git_host: Arc<NativeServerGitHost>,
        clock: Arc<Clock>,
    ) -> RecoveredServer {
        RecoveredServer::recover(
            ServerConfig {
                limits: ServerLimits::default(),
                host: ServerHostContext {
                    authority: native.clone(),
                },
                core: CorePorts::production_with_git_host(identity, home, clock.clone(), git_host)
                    .unwrap(),
                launch: runtime.clone(),
                cleanup: runtime.clone(),
                extensions: Arc::new(NoExtensions),
                initial_appearance: appearance(),
                outcomes: Arc::new(Outcomes),
            },
            ServerDependencies {
                store: Box::new(store),
                credentials: key,
                pairing: lookup,
                runtime,
                external_work: Arc::new(NoExternalWork),
                host_management: native,
                clock,
            },
        )
        .await
        .unwrap()
    }
    fn handle(&self) -> ServerHandle {
        self.server.as_ref().unwrap().handle()
    }
    fn reopen_store(&self) -> SqliteOrganizationStore {
        let db = Database::open_existing_for_application(
            &self.root.path().join("store"),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        SqliteOrganizationStore::open_existing(
            db,
            self.identity,
            StoreLimits::production(),
            production_services(
                self.identity,
                directory(self.identity.server, self.root.path()),
            )
            .unwrap(),
        )
        .unwrap()
    }
    async fn stop(&mut self) {
        self.authenticator.close().await.unwrap();
        self.handle()
            .shutdown(TrustedShutdownRequest {
                authority: self.native.clone(),
            })
            .await
            .unwrap();
        let report = self.server.take().unwrap().wait().await.unwrap();
        assert!(report.cleanup_confirmed && !report.recovery_required);
    }
    async fn connect(&self, n: u64, major: u32) -> Client {
        let socket =
            IpcTransport::connect(&self.ipc.endpoint("s").unwrap(), MessageLimit::envelope())
                .await
                .unwrap();
        let pending = self.listener.accept().await.unwrap();
        let configuration = authentication::HandshakeConfig {
            majors: vec![major],
            capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
            ..Default::default()
        };
        let (handshake, hello) = authentication::ClientHandshake::begin(
            authentication::ServerPin {
                identity: self.identity.server.bytes(),
                public_key: self.key.public_key(),
            },
            authentication::Principal {
                server: self.identity.server.bytes(),
                device: [7; 16],
                pairing: local(1),
                client: local(n),
            },
            authentication::Purpose::Ordinary,
            configuration,
            self.device.clone(),
        )
        .unwrap();
        let client_side = async {
            socket.send(&hello).await.unwrap();
            let (handshake, proof) = handshake
                .receive_server(&socket.recv().await.unwrap().unwrap())
                .unwrap();
            socket.send(&proof).await.unwrap();
            let authentication::Established::Ordinary(codec) = handshake
                .receive_ready(&socket.recv().await.unwrap().unwrap())
                .unwrap()
            else {
                panic!("ordinary client")
            };
            codec
        };
        let (peer, codec) = tokio::join!(
            self.authenticator
                .authenticate(pending, self.authenticator.try_admit().unwrap()),
            client_side
        );
        let peer = peer.unwrap();
        let PeerConnection::Ordinary(session) = &peer.connection else {
            panic!("ordinary server")
        };
        assert_eq!(session.codec.major(), major);
        self.handle()
            .reconcile(session.principal, self.handle().snapshot().state().revision)
            .await
            .unwrap();
        Client {
            peer,
            codec,
            socket,
            handle: self.handle(),
            stream: stream(self.identity.server, n, n * 2 - 1),
            sequence: 0,
        }
    }
    fn seed_repository(root: &Path) -> DiskInventory {
        let repository = root.join("repository");
        let worktree = root.join("additional-worktree");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "--initial-branch=main"]);
        fs::write(repository.join("tracked"), b"committed contents\n").unwrap();
        git(&repository, &["add", "tracked"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@invalid",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        );
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "additional",
                worktree.to_str().unwrap(),
            ],
        );
        fs::write(
            repository.join("untracked"),
            b"repository work to preserve\n",
        )
        .unwrap();
        fs::write(worktree.join("untracked"), b"worktree work to preserve\n").unwrap();
        DiskInventory::capture(repository, worktree)
    }
}

struct DiskInventory {
    repository: PathBuf,
    worktree: PathBuf,
    files: Vec<(PathBuf, Vec<u8>, u64, u64)>,
    inventory: Vec<u8>,
    status: Vec<Vec<u8>>,
}
impl DiskInventory {
    fn capture(repository: PathBuf, worktree: PathBuf) -> Self {
        use std::os::unix::fs::MetadataExt;
        let files = [
            repository.join("tracked"),
            repository.join("untracked"),
            worktree.join("tracked"),
            worktree.join("untracked"),
            worktree.join(".git"),
            repository.join(".git/HEAD"),
            repository.join(".git/index"),
        ]
        .into_iter()
        .map(|path| {
            let metadata = fs::metadata(&path).unwrap();
            let bytes = fs::read(&path).unwrap();
            (path, bytes, metadata.dev(), metadata.ino())
        })
        .collect();
        let inventory = git(&repository, &["worktree", "list", "--porcelain", "-z"]);
        let status = [&repository, &worktree]
            .into_iter()
            .map(|path| git(path, &["status", "--porcelain=v1", "-z"]))
            .collect();
        Self {
            repository,
            worktree,
            files,
            inventory,
            status,
        }
    }
    fn assert_preserved(&self) {
        use std::os::unix::fs::MetadataExt;
        for (path, bytes, dev, ino) in &self.files {
            let metadata = fs::metadata(path).expect("repository file removed");
            assert!(
                fs::read(path).unwrap() == *bytes,
                "repository file contents changed"
            );
            assert_eq!(
                (metadata.dev(), metadata.ino()),
                (*dev, *ino),
                "repository file replaced"
            );
        }
        assert!(
            git(&self.repository, &["worktree", "list", "--porcelain", "-z"]) == self.inventory,
            "Git inventory changed"
        );
        for (path, status) in [&self.repository, &self.worktree]
            .into_iter()
            .zip(&self.status)
        {
            assert!(
                git(path, &["status", "--porcelain=v1", "-z"]) == *status,
                "Git work changed"
            );
        }
    }
}

struct Client {
    peer: AuthenticatedPeer,
    codec: AuthenticatedConnection,
    socket: IpcTransport,
    handle: ServerHandle,
    stream: OperationStreamId,
    sequence: u64,
}
impl Client {
    fn principal(&self) -> VerifiedPrincipal {
        let PeerConnection::Ordinary(session) = &self.peer.connection else {
            unreachable!()
        };
        session.principal
    }
    fn request(&mut self, command: Command) -> CanonicalOperationRequest {
        self.sequence += 1;
        CanonicalOperationRequest::new(
            OperationId::new(self.stream, OperationSequence::new(self.sequence).unwrap()),
            command,
            &CanonicalCodec,
        )
        .unwrap()
    }
    async fn dispatch(
        &mut self,
        request: &CanonicalOperationRequest,
    ) -> Result<Outcome, ServerFailure> {
        let payload = match self.codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Operation(
                    operations::v1::encode_request(request).unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Operation(
                    operations::v2::encode_request(request).unwrap(),
                )),
            }),
        };
        let mut reply = self.exchange(payload).await?;
        tokio::time::timeout(DEADLINE, async {
            loop {
                let lookup = match reply {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::Outcome(value)),
                    }) => {
                        return Ok(operations::v1::decode_outcome(&value, request.binding().id)
                            .unwrap()
                            .1);
                    }
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::Outcome(value)),
                    }) => {
                        return Ok(operations::v2::decode_outcome(&value, request.binding().id)
                            .unwrap()
                            .1);
                    }
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::OperationLookupResult(value)),
                    }) => operations::v1::decode_lookup_result(&value, request.binding()).unwrap(),
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::OperationLookupResult(value)),
                    }) => operations::v2::decode_lookup_result(&value, request.binding()).unwrap(),
                    _ => panic!("operation response had the wrong type"),
                };
                match lookup {
                    OperationLookup::Completed { result, .. } => return Ok(result),
                    OperationLookup::Pending => {}
                    _ => panic!("operation completion lookup changed disposition"),
                }
                // Lookup uses the original identity and digest. It never admits
                // another operation while the native cleanup is pending.
                tokio::time::sleep(Duration::from_millis(10)).await;
                let payload = match self.codec.major() {
                    1 => Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::OperationLookupRequest(
                            operations::v1::encode_lookup_request(request.binding()),
                        )),
                    }),
                    _ => Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::OperationLookupRequest(
                            operations::v2::encode_lookup_request(request.binding()),
                        )),
                    }),
                };
                reply = self.exchange(payload).await?;
            }
        })
        .await
        .expect("operation cleanup did not complete")
    }
    async fn exchange(&mut self, payload: Payload) -> Result<Payload, ServerFailure> {
        let principal = self.principal();
        self.socket
            .send(&self.codec.encode(&payload).unwrap())
            .await
            .unwrap();
        let PeerConnection::Ordinary(session) = &mut self.peer.connection else {
            unreachable!()
        };
        let request_payload = session
            .codec
            .decode(&self.peer.transport.recv().await.unwrap().unwrap())
            .unwrap();
        let reply = ActorOrdinaryDispatch::new(Arc::new(CanonicalCodec))
            .dispatch(principal, request_payload, self.handle.clone())
            .await?
            .unwrap();
        self.peer
            .transport
            .send(&session.codec.encode(&reply).unwrap())
            .await
            .unwrap();
        Ok(self
            .codec
            .decode(&self.socket.recv().await.unwrap().unwrap())
            .unwrap())
    }
    async fn command(&mut self, command: Command) -> Outcome {
        let request = self.request(command);
        self.dispatch(&request).await.unwrap()
    }
    async fn create_workspace(&mut self, path: &Path) -> CreatedOrganization {
        let server = self.principal().server();
        let Outcome::CreatedWithControl(created) = self
            .command(Command::CreateWorkspace {
                server,
                source: WorkspaceCreationSource::Directory(directory(server, path)),
                name: NameDraft::Custom(NameText::new("retained workspace".into()).unwrap()),
            })
            .await
        else {
            panic!("workspace creation")
        };
        created
    }
    async fn start(&mut self, pane: PaneId) -> SessionKey {
        let snapshot = self.handle.snapshot();
        let runtime = snapshot
            .runtime()
            .iter()
            .find(|record| record.pane == pane)
            .unwrap();
        let ControlRecord::Held { client, generation } = runtime.control else {
            panic!("controlled pane")
        };
        let grant = ControlGrant::new(pane, client, generation).unwrap();
        let principal = self.principal();
        let session = SessionKey {
            pane,
            epoch: principal.epoch(),
            generation: runtime.session_generation,
        };
        let fence = EffectFence::new(
            session,
            grant,
            principal.binding(),
            ContextGeneration::new(1),
            None,
        )
        .unwrap();
        let id = OperationId::new(
            self.stream,
            OperationSequence::new(self.sequence + 1).unwrap(),
        );
        let intent = StartupIntent::new(id, fence, StartupSource::StartSession).unwrap();
        assert!(matches!(
            self.command(Command::BeginStartup(intent)).await,
            Outcome::StartupAwaitingMeasurement { .. }
        ));
        let Outcome::SessionStarted(session) = self
            .command(Command::CompleteMeasuredStartup(
                MeasuredStartupMeasurement {
                    intent: id,
                    fence,
                    grid: MeasuredGrid::new(
                        24,
                        80,
                        CellPixels::new(8, 16).unwrap(),
                        SUPPORTED_GRID_LIMITS,
                    )
                    .unwrap(),
                },
            ))
            .await
        else {
            panic!("startup reservation")
        };
        session
    }
    async fn assess_end_all(&mut self) -> CloseAssessment {
        let server = self.principal().server();
        let Outcome::Assessment(assessment) =
            self.command(Command::AssessEndAllSessions { server }).await
        else {
            panic!("End All assessment")
        };
        *assessment
    }
}
async fn running(handle: &ServerHandle, session: SessionKey) {
    let mut snapshots = handle.subscribe_snapshot();
    tokio::time::timeout(DEADLINE, async {
        loop {
            let running = snapshots.borrow().runtime().iter().any(|record| {
                record.pane == session.pane
                    && matches!(record.phase, SessionPhase::Running { generation } if generation == session.generation)
            });
            if running { return; }
            snapshots.changed().await.unwrap();
        }
    }).await.expect("Session did not run");
}
fn fence(access: &RuntimeAccess, revision: Option<ContentRevision>) -> EffectFence {
    authority_fence(access.authority, revision)
}
fn authority_fence(authority: RuntimeAuthority, revision: Option<ContentRevision>) -> EffectFence {
    EffectFence::new(
        authority.session(),
        authority.parts().grant.unwrap(),
        authority.parts().connection.unwrap(),
        authority.parts().context,
        revision,
    )
    .unwrap()
}
#[derive(Eq, PartialEq)]
struct ContentEvidence {
    grid: Vec<ContentPage>,
    history: Vec<ContentPage>,
}
async fn content(access: &RuntimeAccess) -> ContentEvidence {
    tokio::time::timeout(DEADLINE, async {
        let mut base = ContentBase {
            session: access.authority.session(),
            revision: ContentRevision::new(1),
            screen: Screen::Primary,
            screen_generation: 1,
            topology_revision: 1,
        };
        let limits = PageLimits::new(256, 16_384, 256 * 1024).unwrap();
        let mut request = 1;
        'snapshot: loop {
            let stamp = QueryStamp::new(
                fence(access, Some(base.revision)),
                base,
                ContentRequestId::new(request).unwrap(),
                RequestGeneration::new(1),
            )
            .unwrap();
            request += 1;
            match access
                .handle
                .read_topology(TopologyPageRequest::new(stamp, None, limits).unwrap())
                .unwrap()
                .await
                .unwrap()
            {
                ContentReadResult::Ready(page) => base = page.stamp().base(),
                ContentReadResult::ResyncRequired { current, .. } => {
                    base = current;
                    continue;
                }
                ContentReadResult::Rejected { reason, .. } => {
                    panic!("content topology rejected: {reason:?}")
                }
            }
            let mut ranges = Vec::new();
            for range in [
                ContentRange::ActiveGrid,
                ContentRange::History {
                    first: None,
                    last: None,
                },
            ] {
                let stamp = QueryStamp::new(
                    fence(access, Some(base.revision)),
                    base,
                    ContentRequestId::new(request).unwrap(),
                    RequestGeneration::new(1),
                )
                .unwrap();
                request += 1;
                let mut continuation = None;
                let mut pages = Vec::new();
                loop {
                    match access
                        .handle
                        .read_page(
                            ContentPageRequest::new(stamp, range, continuation, limits).unwrap(),
                        )
                        .unwrap()
                        .await
                        .unwrap()
                    {
                        ContentReadResult::Ready(reply) => {
                            continuation = reply.continuation().unwrap();
                            pages.push(reply.page().clone());
                            if continuation.is_none() {
                                break;
                            }
                        }
                        ContentReadResult::ResyncRequired { current, .. } => {
                            base = current;
                            continue 'snapshot;
                        }
                        ContentReadResult::Rejected { reason, .. } => {
                            panic!("content page rejected: {reason:?}")
                        }
                    }
                }
                ranges.push(pages);
            }
            let history = ranges.pop().unwrap();
            let grid = ranges.pop().unwrap();
            return ContentEvidence { grid, history };
        }
    })
    .await
    .expect("content did not stabilize at one exact revision")
}
async fn populated(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    session: SessionKey,
) -> (RuntimeAccess, ContentEvidence) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let access = handle.runtime_access(principal, session).await.unwrap();
            let page = content(&access).await;
            let text: String = page
                .grid
                .iter()
                .flat_map(|page| &page.parts().rows)
                .flat_map(|row| row.cells())
                .map(|cell| cell.content().text())
                .collect();
            if text.contains("retained-output-39") {
                return (access, page);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("PTY output did not reach semantic content")
}

#[derive(Clone, Copy, Debug)]
enum Destruction {
    EndAll,
    Kill,
}
impl Destruction {
    fn accepted(self, snapshot: &OrganizationSnapshot) -> bool {
        match self {
            Self::EndAll => {
                !snapshot.runtime().is_empty()
                    && snapshot.runtime().iter().all(|record| {
                        matches!(
                            record.phase,
                            SessionPhase::Ended {
                                reason: EndReason::EndAllSessions
                            }
                        )
                    })
            }
            Self::Kill => {
                snapshot.state().workspaces.is_empty()
                    && snapshot.state().tabs.is_empty()
                    && snapshot.state().panes.is_empty()
            }
        }
    }
}
fn assert_organization_preserved(expected: &OrganizationState, actual: &OrganizationState) {
    let expected_facts = expected.retained.as_ref().unwrap();
    let mut facts = actual.retained.as_ref().unwrap().parts().clone();
    assert_eq!(
        facts.pane_generations.len(),
        expected_facts.pane_generations().len()
    );
    for (current, previous) in facts
        .pane_generations
        .iter()
        .zip(expected_facts.pane_generations())
    {
        assert!(
            current.dominates(*previous),
            "retained generation high-water regressed"
        );
    }
    // Revisions and generation high-water advance intentionally. Membership,
    // order, Roots, settings, directory records and all other facts must survive.
    facts.pane_generations = expected_facts.pane_generations().to_vec();
    let mut normalized = actual.clone();
    normalized.revision = expected.revision;
    normalized.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    assert!(normalized == *expected, "saved organization changed");
}
fn assert_catalog_preserved(expected: &OrganizationState, actual: &OrganizationState) {
    assert!(
        actual.server == expected.server && actual.settings == expected.settings,
        "server settings changed"
    );
    assert!(
        actual.repositories == expected.repositories,
        "Repository records changed"
    );
    assert!(
        actual.worktrees == expected.worktrees,
        "Worktree records changed"
    );
    let expected = expected.retained.as_ref().unwrap();
    let actual = actual.retained.as_ref().unwrap();
    assert!(
        actual.repository_inventory_versions() == expected.repository_inventory_versions(),
        "Repository inventory versions changed"
    );
    assert!(
        actual.worktree_inventory() == expected.worktree_inventory(),
        "Worktree inventory facts changed"
    );
    assert!(
        actual.remote_access() == expected.remote_access(),
        "remote access policy changed"
    );
}
async fn selected_server_isolation(major: u32, destruction: Destruction) {
    let mut a = Fixture::new(31).await;
    let mut b = Fixture::new(32).await;
    let disk = a.disk.take().unwrap();
    let b_disk = b.disk.take().unwrap();
    let mut owner_a = a.connect(1, major).await;
    let mut invoking_a = a.connect(2, major).await;
    let mut owner_b = b.connect(1, major).await;
    let created_a = owner_a.create_workspace(&disk.repository).await;
    let CreatedEntities::Workspace {
        workspace,
        tab,
        pane,
    } = created_a.entities()
    else {
        panic!("workspace identities")
    };
    let worktree = a
        .handle()
        .snapshot()
        .state()
        .worktrees
        .iter()
        .find(|worktree| worktree.directory == directory(a.identity.server, &disk.worktree))
        .expect("real additional Worktree was not discovered")
        .id;
    let Outcome::CreatedWithControl(created_worktree) = owner_a
        .command(Command::CreateTab {
            workspace,
            source: TabCreationSource::Worktree(worktree),
        })
        .await
    else {
        panic!("Worktree Tab")
    };
    let worktree_pane = created_worktree.entities().pane();
    let Outcome::CreatedWithControl(created_starting) = owner_a
        .command(Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        })
        .await
    else {
        panic!("split Pane")
    };
    let starting_pane = created_starting.entities().pane();
    let created_b = owner_b.create_workspace(b.root.path()).await;
    let session_a = owner_a.start(pane).await;
    running(&a.handle(), session_a).await;
    let (old_access, old_page) = populated(&a.handle(), owner_a.principal(), session_a).await;
    assert!(!old_page.grid[0].parts().rows.is_empty());
    assert!(
        old_page
            .history
            .iter()
            .any(|page| !page.parts().rows.is_empty()),
        "native output did not create Scrollback"
    );
    let session_unclaimed = owner_a.start(worktree_pane).await;
    running(&a.handle(), session_unclaimed).await;
    let session_b = owner_b.start(created_b.entities().pane()).await;
    running(&b.handle(), session_b).await;
    let (b_access, b_content) = populated(&b.handle(), owner_b.principal(), session_b).await;
    assert!(
        b_content
            .history
            .iter()
            .any(|page| !page.parts().rows.is_empty()),
        "server B has no retained Scrollback"
    );
    let b_pid = b.runtime.pid(session_b).await;
    assert!(process_alive(b_pid));
    let b_snapshot = b.handle().snapshot();
    let b_phase = b.handle().phase();
    let unclaimed_grant = created_worktree.control().grant();
    assert_eq!(
        owner_a
            .command(Command::ReleaseControl(ReleaseControlRequest {
                epoch: owner_a.principal().epoch(),
                grant: unclaimed_grant
            }))
            .await,
        Outcome::Applied
    );
    let hold = Hold::new();
    let release = HoldRelease(hold.clone());
    *a.runtime.hold.lock().unwrap() = Some(hold.clone());
    let starting_session = owner_a.start(starting_pane).await;
    tokio::time::timeout(DEADLINE, hold.entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        a.handle()
            .snapshot()
            .runtime()
            .iter()
            .find(|r| r.pane == starting_pane)
            .unwrap()
            .phase,
        SessionPhase::Starting { .. }
    ));
    let starting_pid = a.runtime.pid(starting_session).await;
    assert!(process_alive(starting_pid));
    // A disconnected controller's grant and unclaimed live work are both in scope.
    a.handle().disconnect(owner_a.principal()).await.unwrap();
    let before = a.handle().snapshot();
    assert!(
        before
            .runtime()
            .iter()
            .any(|r| r.control == ControlRecord::Unclaimed)
    );
    assert!(before.runtime().iter().all(|r| !matches!(r.control, ControlRecord::Held { client, .. } if client == invoking_a.principal().client())));
    let assessment = match destruction {
        Destruction::EndAll => invoking_a.assess_end_all().await,
        Destruction::Kill => {
            let Outcome::Assessment(assessment) = invoking_a
                .command(Command::AssessKillWorkspace { workspace })
                .await
            else {
                panic!("Kill assessment");
            };
            *assessment
        }
    };
    assert_eq!(assessment.summary().pane_count(), 3);
    assert_eq!(assessment.summary().live_session_count(), 3);
    // Use B's own valid authenticated operation binding with a foreign A scope.
    // Canonical requests reject that mismatch at construction, so tamper only the
    // wire command after encoding a valid B request, then sign it as B.
    let foreign = owner_b.request(Command::EndAllSessions {
        server: b.identity.server,
        assessment: AssessmentRef {
            operation: OperationId::new(
                owner_b.stream,
                OperationSequence::new(owner_b.sequence).unwrap(),
            ),
            digest: RequestDigest::from_bytes([1; 32]),
        },
    });
    let mut foreign_payload = match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Operation(
                operations::v1::encode_request(&foreign).unwrap(),
            )),
        }),
        _ => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Operation(
                operations::v2::encode_request(&foreign).unwrap(),
            )),
        }),
    };
    match &mut foreign_payload {
        Payload::Major1(message) => {
            let Some(v1::message::Kind::Operation(op)) = &mut message.kind else {
                unreachable!()
            };
            let Some(v1::operation::Action::EndAllSessions(value)) = &mut op.action else {
                unreachable!()
            };
            value.server = Some(spaceterm_protocol::convert::identity::encode_global(
                a.identity.server.bytes(),
            ));
        }
        Payload::Major2(message) => {
            let Some(v2::message::Kind::Operation(op)) = &mut message.kind else {
                unreachable!()
            };
            let Some(v2::operation::Action::EndAllSessions(value)) = &mut op.action else {
                unreachable!()
            };
            value.server = Some(spaceterm_protocol::convert::identity::encode_global(
                a.identity.server.bytes(),
            ));
        }
    }
    let foreign_error = owner_b
        .exchange(foreign_payload)
        .await
        .expect_err("foreign-server End All was admitted");
    assert_eq!(foreign_error.class, ServerFailureClass::ScopeMismatch);
    assert!(
        a.handle().snapshot() == before,
        "foreign operation changed server A"
    );
    assert!(
        b.handle().snapshot() == b_snapshot,
        "foreign operation changed server B"
    );
    let command = match destruction {
        Destruction::EndAll => Command::EndAllSessions {
            server: a.identity.server,
            assessment: assessment.reference(),
        },
        Destruction::Kill => Command::KillWorkspaceConfirmed {
            workspace,
            assessment: assessment.reference(),
        },
    };
    let request = invoking_a.request(command);
    let disk = DiskInventory::capture(disk.repository, disk.worktree);
    let b_disk = DiskInventory::capture(b_disk.repository, b_disk.worktree);
    let a_handle = a.handle();
    let principal = invoking_a.principal();
    let binding = request.binding();
    assert_eq!(
        a_handle.lookup_state(principal, binding).await.unwrap(),
        OperationLookup::NeverAdmitted,
        "destructive operation identity is not the next admitted sequence"
    );
    let mut changes = a_handle.subscribe_snapshot();
    let mut ending = tokio::spawn(async move {
        let outcome = invoking_a.dispatch(&request).await;
        (invoking_a, request, outcome)
    });
    let accepted = tokio::time::timeout(DEADLINE, async {
        loop {
            if destruction.accepted(&changes.borrow()) {
                return None;
            }
            tokio::select! {
                result = &mut ending => return Some(result.unwrap()),
                changed = changes.changed() => changed.unwrap(),
            }
        }
    })
    .await;
    let (mut invoking_a, destructive_request, outcome) = match accepted {
        Ok(Some(result)) => {
            assert!(
                result.2.is_err(),
                "destructive request completed while Starting cleanup was held"
            );
            drop(release);
            result
        }
        Ok(None) => {
            // Published acceptance must remain Pending until this live Starting
            // process is released to the actor for retirement and confirmed cleanup.
            assert!(
                process_alive(starting_pid),
                "held Starting process exited before cleanup was released"
            );
            assert_eq!(
                tokio::time::timeout(DEADLINE, a_handle.lookup_state(principal, binding))
                    .await
                    .expect("pending retirement lookup did not respond")
                    .unwrap(),
                OperationLookup::Pending,
                "destructive request completed before Starting cleanup"
            );
            assert!(
                !ending.is_finished(),
                "destructive dispatch completed before Starting cleanup"
            );
            drop(release);
            tokio::time::timeout(DEADLINE, ending)
                .await
                .unwrap()
                .unwrap()
        }
        Err(_) => panic!("{destruction:?} did not publish its acceptance state"),
    };
    if let Err(error) = outcome {
        // Keep native teardown separate from acceptance. A failure cannot turn
        // fixture cleanup into evidence that the destructive request succeeded.
        a.stop().await;
        disk.assert_preserved();
        assert!(
            b.handle().snapshot() == b_snapshot,
            "failed admission changed server B"
        );
        assert!(process_alive(b_pid));
        assert!(
            content(&b_access).await == b_content,
            "failed admission changed server B content"
        );
        b_disk.assert_preserved();
        b.stop().await;
        panic!("{destruction:?} acceptance failed: {:?}", error.class);
    }
    assert_eq!(outcome.unwrap(), Outcome::Applied);
    let accepted = a.handle().snapshot();
    assert!(
        destruction.accepted(&accepted),
        "destructive acceptance state was not published"
    );
    assert_catalog_preserved(before.state(), accepted.state());
    if matches!(destruction, Destruction::EndAll) {
        assert_organization_preserved(before.state(), accepted.state());
        assert_eq!(accepted.runtime().len(), before.runtime().len());
        for (ended, old) in accepted.runtime().iter().zip(before.runtime()) {
            assert_eq!(ended.pane, old.pane);
            assert_eq!(
                ended.control, old.control,
                "End All acquired or released control"
            );
            assert_eq!(ended.control_generation, old.control_generation);
            assert_eq!(
                ended.session_generation,
                old.session_generation.checked_successor().unwrap()
            );
        }
    }
    for session in [session_a, session_unclaimed, starting_session] {
        assert!(
            !process_alive(a.runtime.pid(session).await),
            "destructive request left an owned shell alive"
        );
        for command in [
            RuntimeCommand::Resize(
                MeasuredGrid::new(
                    24,
                    80,
                    CellPixels::new(8, 16).unwrap(),
                    SUPPORTED_GRID_LIMITS,
                )
                .unwrap(),
            ),
            RuntimeCommand::ClearScreenAndScrollback,
        ] {
            assert!(
                a.runtime
                    .handle(session)
                    .try_submit(FencedRuntimeCommand {
                        fence: authority_fence(
                            a.runtime.initial_authorities.lock().unwrap()[&session],
                            None
                        ),
                        command,
                    })
                    .is_err(),
                "retired runtime admitted an exact delayed effect"
            );
        }
    }
    assert!(
        old_access
            .handle
            .read_topology(
                TopologyPageRequest::new(
                    QueryStamp::new(
                        fence(&old_access, Some(old_page.grid[0].parts().base.revision)),
                        old_page.grid[0].parts().base,
                        ContentRequestId::new(8).unwrap(),
                        RequestGeneration::new(2)
                    )
                    .unwrap(),
                    None,
                    PageLimits::new(256, 16_384, 256 * 1024).unwrap()
                )
                .unwrap()
            )
            .is_err(),
        "retired content remains readable"
    );
    assert!(
        a.handle()
            .runtime_access(owner_a.principal(), session_a)
            .await
            .is_err()
    );
    assert_eq!(
        invoking_a.dispatch(&destructive_request).await.unwrap(),
        Outcome::Applied
    );
    assert!(
        a.handle().snapshot() == accepted,
        "old destructive replay revived state"
    );
    assert!(
        b.handle().snapshot() == b_snapshot,
        "server B organization or grants changed"
    );
    assert_eq!(b.handle().phase(), b_phase);
    assert!(process_alive(b_pid));
    assert!(
        content(&b_access).await == b_content,
        "server B terminal content changed"
    );
    disk.assert_preserved();
    b_disk.assert_preserved();
    a.stop().await;
    let mut reopened = a.reopen_store();
    let recovered = reopened.recover().unwrap();
    assert_organization_preserved(accepted.state(), &recovered.organization);
    assert_catalog_preserved(before.state(), &recovered.organization);
    if matches!(destruction, Destruction::Kill) {
        assert!(recovered.organization.workspaces.is_empty());
        assert!(recovered.organization.tabs.is_empty());
        assert!(recovered.organization.panes.is_empty());
    }
    disk.assert_preserved();
    // Reopening the physical store changes epoch, never starts a lazy shell or
    // reconstructs a deleted Workspace from a historical successful outcome.
    a.server = Some(
        Fixture::recover(
            reopened,
            a.identity,
            directory(a.identity.server, a.root.path()),
            a.key.clone(),
            a.lookup.clone(),
            a.runtime.clone(),
            a.native.clone(),
            a.git_host.clone(),
            a.clock.clone(),
        )
        .await,
    );
    assert_organization_preserved(accepted.state(), a.handle().snapshot().state());
    assert!(
        a.handle()
            .snapshot()
            .runtime()
            .iter()
            .all(|record| record.control == ControlRecord::Unclaimed
                && !matches!(
                    record.phase,
                    SessionPhase::Starting { .. } | SessionPhase::Running { .. }
                ))
    );
    assert_eq!(
        a.runtime.handles.lock().unwrap().len(),
        3,
        "recovery launched a replacement Session"
    );
    for session in [session_a, session_unclaimed, starting_session] {
        assert!(
            !process_alive(a.runtime.pid(session).await),
            "recovery revived an old process"
        );
    }
    disk.assert_preserved();
    b_disk.assert_preserved();
    assert!(
        b.handle().snapshot() == b_snapshot,
        "server B changed during server A recovery"
    );
    assert!(process_alive(b_pid));
    assert!(
        content(&b_access).await == b_content,
        "server B content changed during server A recovery"
    );
    a.stop().await;
    b.stop().await;
    assert!(!process_alive(b_pid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn major_1_selected_server_end_all_preserves_files_and_other_server() {
    tokio::time::timeout(
        Duration::from_secs(90),
        selected_server_isolation(1, Destruction::EndAll),
    )
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn major_2_selected_server_end_all_preserves_files_and_other_server() {
    tokio::time::timeout(
        Duration::from_secs(90),
        selected_server_isolation(2, Destruction::EndAll),
    )
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn major_1_selected_server_kill_preserves_files_and_other_server() {
    tokio::time::timeout(
        Duration::from_secs(90),
        selected_server_isolation(1, Destruction::Kill),
    )
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn major_2_selected_server_kill_preserves_files_and_other_server() {
    tokio::time::timeout(
        Duration::from_secs(90),
        selected_server_isolation(2, Destruction::Kill),
    )
    .await
    .unwrap();
}
