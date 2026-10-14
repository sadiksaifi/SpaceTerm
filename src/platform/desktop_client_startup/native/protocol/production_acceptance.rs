//! Real account listener and desktop provider acceptance. Only OS resources are injected.

use super::*;
use crate::platform::{
    app_directories::{AppDirectories, AppDirectoryEnvironment},
    client_view_store::NativeViewStore,
    desktop_client_startup::{
        DesktopClientComposition, NativeOwnerFactory,
        native::{Context, Factory, test_composition},
    },
    local_server_client::tests::Root,
    secure_filesystem::{SecureDirectory, SecureFilesystemError},
    unix_secure_filesystem::UnixSecureFilesystem,
};
use spaceterm_client::{
    api::{CompatibilityExplanation, ConnectingPhase},
    journal::{JournalHealth, RetainedOperationState},
    pane_channel::{
        EffectKind, ExplicitUseOutcome, ExplicitUseSource, PaneChannel, PaneContextFacts,
        RuntimeEffect, WorkspaceLease,
    },
    replica::{ReplicaState, ResyncReason},
    view::{ReplicaFacts, ReplicaObservation, ReplicaReadiness, ServerViewStatus},
};
use spaceterm_core::{
    geometry::{CellPixels, MeasuredGrid},
    organization::{CreationTopologyReadModel, OrganizationSnapshot},
};
use spaceterm_host::{pty::*, shell_launch::PreparedShellLaunch};
use spaceterm_server::{
    account_startup::{
        AccountStartup, RecoveredAccountServer,
        test_support::{StartupAbsence, initialize, supervisor},
    },
    adapters::terminal::AccountLaunchFacts,
};
use std::{
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    sync::{atomic::AtomicBool, mpsc},
    time::Duration,
};

type Composition = DesktopClientComposition<NativeViewStore>;

/// Reusable OS boundary for Copy and paste cases. The real owning NativeRuntime
/// encodes every observed byte and consumes every supplied output sequence.
#[derive(Clone, Default)]
pub(super) struct RecordingPty(Arc<Mutex<PtyObservations>>);
#[derive(Default)]
struct PtyObservations {
    launches: Vec<NativePtySize>,
    resizes: Vec<NativePtySize>,
    input: Vec<u8>,
    session_inputs: Vec<Vec<u8>>,
    output: Option<mpsc::Sender<Option<Vec<u8>>>>,
    lifetimes: Vec<Arc<AtomicBool>>,
    outputs: Vec<mpsc::Sender<Option<Vec<u8>>>>,
    hidden: bool,
    blocked: bool,
    write_attempts: usize,
}
impl RecordingPty {
    pub(super) fn launches(&self) -> Vec<NativePtySize> {
        self.0.lock().unwrap().launches.clone()
    }
    pub(super) fn resizes(&self) -> Vec<NativePtySize> {
        self.0.lock().unwrap().resizes.clone()
    }
    pub(super) fn input(&self) -> Vec<u8> {
        self.0.lock().unwrap().input.clone()
    }
    pub(super) fn session_input(&self, index: usize) -> Vec<u8> {
        self.0.lock().unwrap().session_inputs[index].clone()
    }
    pub(super) fn output(&self, bytes: &[u8]) {
        self.0
            .lock()
            .unwrap()
            .output
            .as_ref()
            .unwrap()
            .send(Some(bytes.to_vec()))
            .unwrap();
    }
    pub(super) fn set_hidden(&self, hidden: bool) {
        self.0.lock().unwrap().hidden = hidden;
    }
    pub(super) fn is_running(&self) -> bool {
        let state = self.0.lock().unwrap();
        state
            .lifetimes
            .iter()
            .any(|ended| !ended.load(Ordering::SeqCst))
    }
}
struct Adapter {
    observed: RecordingPty,
    index: usize,
    reader: Option<OutputReader>,
    ended: Arc<AtomicBool>,
}
struct OutputReader {
    receiver: mpsc::Receiver<Option<Vec<u8>>>,
    pending: io::Cursor<Vec<u8>>,
}
impl Read for OutputReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            let read = self.pending.read(bytes)?;
            if read != 0 {
                return Ok(read);
            }
            match self.receiver.recv() {
                Ok(Some(chunk)) => self.pending = io::Cursor::new(chunk),
                Ok(None) | Err(_) => return Ok(0),
            }
        }
    }
}
impl Write for Adapter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.observed.0.lock().unwrap();
        state.write_attempts += 1;
        if state.blocked {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        state.input.extend_from_slice(bytes);
        state.session_inputs[self.index].extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl NativePtyAdapter for Adapter {
    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
        self.reader
            .take()
            .map(|reader| Box::new(reader) as Box<dyn Read + Send>)
            .ok_or_else(|| io::ErrorKind::AlreadyExists.into())
    }
    fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
        self.observed.0.lock().unwrap().resizes.push(size);
        Ok(())
    }
    fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        Ok(self.observed.0.lock().unwrap().hidden)
    }
    fn wait_for_exit(&mut self, _: Duration) -> Result<NativePtyExit, NativePtyWaitFailure> {
        if self.ended.load(Ordering::SeqCst) {
            Ok(NativePtyExit::Success)
        } else {
            Err(NativePtyWaitFailure::new(io::ErrorKind::TimedOut))
        }
    }
}
struct Termination {
    ended: Arc<AtomicBool>,
    output: mpsc::Sender<Option<Vec<u8>>>,
}
impl NativePtyTermination for Termination {
    fn request_termination(&self) -> io::Result<()> {
        if !self.ended.swap(true, Ordering::SeqCst) {
            let _ = self.output.send(None);
        }
        Ok(())
    }
    fn settle(&self) -> NativePtySettlement {
        if self.ended.load(Ordering::SeqCst) {
            NativePtySettlement::Settled
        } else {
            NativePtySettlement::StillRunning
        }
    }
}
impl NativePtyAdapterFactory for RecordingPty {
    fn create(
        &self,
        _: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
        let (output, receiver) = mpsc::channel();
        let mut state = self.0.lock().unwrap();
        let index = state.launches.len();
        state.launches.push(size);
        state.session_inputs.push(Vec::new());
        let ended = Arc::new(AtomicBool::new(false));
        state.output = Some(output.clone());
        state.outputs.push(output.clone());
        state.lifetimes.push(ended.clone());
        Ok(NativePtyAdapterParts {
            adapter: Box::new(Adapter {
                observed: self.clone(),
                index,
                ended: ended.clone(),
                reader: Some(OutputReader {
                    receiver,
                    pending: io::Cursor::new(Vec::new()),
                }),
            }),
            termination: Arc::new(Termination { ended, output }),
        })
    }
}

struct CountingRoutes {
    inner: Arc<dyn RouteConnector>,
    opens: Arc<AtomicU64>,
}
impl RouteConnector for CountingRoutes {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.inner.open(target, purpose)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureReopenFailure {
    MissingStorage,
    ChangedStorage,
    IdentityUnavailable,
    Open(ClientFailureClass),
}

#[derive(Clone)]
struct FixtureJournalIdentity {
    directory: SecureDirectory,
    database: (u64, u64),
    // Keep the same native provider and retain the existing journal owner, not
    // a new database descriptor. Its live inode cannot be recycled on relaunch.
    context: Arc<Context>,
}
impl FixtureJournalIdentity {
    fn capture(paths: &AppPaths, context: Arc<Context>) -> Result<Self, FixtureReopenFailure> {
        let directory = paths
            .filesystem()
            .open_private_directory(&fixture_journal_directory(paths))
            .map_err(fixture_identity_failure)?
            .ok_or(FixtureReopenFailure::MissingStorage)?;
        paths
            .filesystem()
            .verify_directory(&directory)
            .map_err(fixture_identity_failure)?;
        Ok(Self {
            directory,
            database: fixture_database_identity(paths)?,
            context,
        })
    }
    fn verify(&self, paths: &AppPaths) -> Result<(), FixtureReopenFailure> {
        paths
            .filesystem()
            .verify_directory(&self.directory)
            .map_err(fixture_identity_failure)?;
        if fixture_database_identity(paths)? != self.database {
            return Err(FixtureReopenFailure::ChangedStorage);
        }
        Ok(())
    }
}
fn fixture_journal_directory(paths: &AppPaths) -> std::path::PathBuf {
    paths
        .directories()
        .root(crate::platform::app_directories::AppDirectoryRoot::State)
        .join("client-connections")
        .join("desktop")
}
fn fixture_identity_failure(failure: SecureFilesystemError) -> FixtureReopenFailure {
    match failure {
        SecureFilesystemError::Missing => FixtureReopenFailure::MissingStorage,
        SecureFilesystemError::Unavailable => FixtureReopenFailure::IdentityUnavailable,
        _ => FixtureReopenFailure::ChangedStorage,
    }
}
fn fixture_database_identity(paths: &AppPaths) -> Result<(u64, u64), FixtureReopenFailure> {
    // stat does not open/close a second live database descriptor, which could
    // release process-wide POSIX byte-range locks held by its existing owner.
    let metadata = std::fs::symlink_metadata(
        fixture_journal_directory(paths).join(spaceterm_host::storage::DATABASE_FILE_NAME),
    )
    .map_err(|failure| match failure.kind() {
        io::ErrorKind::NotFound => FixtureReopenFailure::MissingStorage,
        _ => FixtureReopenFailure::IdentityUnavailable,
    })?;
    if !metadata.is_file() {
        return Err(FixtureReopenFailure::ChangedStorage);
    }
    Ok((metadata.dev(), metadata.ino()))
}
async fn existing_fixture_context(
    paths: &AppPaths,
    original: &Context,
) -> Result<Arc<Context>, ClientFailure> {
    // Factory::context may initialize an absent directory. Relaunch has
    // existing-only authority and keeps the initial native provider unchanged.
    let journal = crate::platform::client_journal::open(
        paths,
        crate::platform::client_journal::JournalOpenMode::Existing,
        Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
        Arc::new(crate::platform::desktop_client_startup::native::Verifier),
        spaceterm_client::journal::JournalLimits::default(),
    )
    .await?;
    Ok(Arc::new(Context {
        journal,
        native: original.native.clone(),
        native_failure: original.native_failure,
    }))
}
async fn reopen_retained_context(
    major: u32,
    paths: &AppPaths,
    original: &FixtureJournalIdentity,
    mut open: impl AsyncFnMut() -> Result<Arc<Context>, ClientFailure>,
) -> Result<Arc<Context>, FixtureReopenFailure> {
    for attempt in 1..=5 {
        let verify = || {
            original.verify(paths).inspect_err(|failure| {
                eprintln!(
                    "fixture context reopen refused: major={major} reopen_attempt={attempt} failure={failure:?}"
                );
            })
        };
        verify()?;
        let result = open().await;
        // Missing or replaced storage cannot inherit retry permission from an
        // earlier StorageUnavailable result. No attempt may initialize storage.
        verify()?;
        match result {
            Ok(context) => {
                eprintln!("fixture context reopened: major={major} reopen_attempt={attempt}");
                return Ok(context);
            }
            Err(failure) => {
                let class = failure.class();
                eprintln!(
                    "fixture context reopen failed: major={major} reopen_attempt={attempt} failure={class:?}"
                );
                if class != ClientFailureClass::StorageUnavailable || attempt == 5 {
                    return Err(FixtureReopenFailure::Open(class));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    unreachable!("the fixed final reopen attempt always returns")
}

pub(super) struct ProductionFixture {
    major: u32,
    root: Root,
    pub(super) pty: RecordingPty,
    pub(super) server: spaceterm_server::api::ServerHandle,
    paths: Arc<AppPaths>,
    credentials: test_composition::MemoryCredentials,
    original_journal: Mutex<Option<FixtureJournalIdentity>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    running: tokio::task::JoinHandle<
        Result<
            spaceterm_server::shutdown::ShutdownReport,
            spaceterm_server::account_startup::StartupFailure,
        >,
    >,
}
impl ProductionFixture {
    pub(super) async fn new(major: u32) -> Self {
        let root = Root::new();
        initialize(&root.0).unwrap();
        let supervisor = supervisor(&root.0, Arc::new(StartupAbsence)).unwrap();
        let pty = RecordingPty::default();
        let recovered = RecoveredAccountServer::recover_with_native_pty(
            supervisor.clone(),
            AccountStartup::admit(&supervisor).unwrap(),
            AccountLaunchFacts {
                uid: supervisor.key().account_uid,
                home: root.0.clone(),
                planner: spaceterm_host::shell_launch::ShellLaunchPlanner::for_test(
                    root.0.join("shell"),
                    root.0.clone(),
                ),
                machine: spaceterm_terminal::metadata::LocalMachine::default(),
            },
            Arc::new(pty.clone()),
            vec![major],
        )
        .await
        .unwrap();
        let server = recovered.handle();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(recovered.run_until(async {
            let _ = stopped.await;
        }));
        wait_for("production account listener did not enter Serving", || {
            (server.phase() == spaceterm_server::api::ServicePhase::Serving).then_some(())
        })
        .await;
        let directories = AppDirectories::resolve_xdg(
            "production-acceptance-development",
            &AppDirectoryEnvironment {
                home: Some(root.0.clone().into_os_string()),
                xdg_state_home: Some(root.0.join("client-state").into_os_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let paths = Arc::new(
            AppPaths::from_directories(directories, 104, Arc::new(UnixSecureFilesystem)).unwrap(),
        );
        Self {
            major,
            root,
            pty,
            server,
            paths,
            credentials: Default::default(),
            original_journal: Mutex::new(None),
            stop: Some(stop),
            running,
        }
    }
    pub(super) async fn client(&self, initialize: bool) -> (Composition, Arc<AtomicU64>) {
        self.client_with_state(
            self.paths.clone(),
            self.credentials.clone(),
            initialize,
            false,
        )
        .await
    }
    async fn independent_client(&self) -> (Composition, Arc<AtomicU64>) {
        let directories = AppDirectories::resolve_xdg(
            "production-acceptance-second-client",
            &AppDirectoryEnvironment {
                home: Some(self.root.0.clone().into_os_string()),
                xdg_state_home: Some(self.root.0.join("client-state").into_os_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let paths = Arc::new(
            AppPaths::from_directories(directories, 104, Arc::new(UnixSecureFilesystem)).unwrap(),
        );
        // The first explicit owner enrollment already admits separate native
        // Clients over its authenticated ordinary connection. Restore one unused
        // additional Client's own acknowledged journal, not another owner device
        // or the primary desktop's identity. Both Clients use this device's keys.
        self.client_with_state(paths, self.credentials.clone(), true, true)
            .await
    }
    async fn client_with_state(
        &self,
        paths: Arc<AppPaths>,
        credentials: test_composition::MemoryCredentials,
        initialize: bool,
        restore_additional_client: bool,
    ) -> (Composition, Arc<AtomicU64>) {
        let mut fixture = test_composition::Fixture::from_arguments([
            "--test-account-root".into(),
            self.root.0.clone().into_os_string(),
        ])
        .unwrap()
        .unwrap();
        fixture.credentials = credentials.clone();
        let mut factory = Factory {
            paths: paths.clone(),
            enrollment: Arc::default(),
            ssh: crate::platform::server_client_route::SshStartup {
                executable: "/usr/bin/ssh".into(),
                home: self.root.0.clone(),
                environment: vec![],
            },
            askpass: crate::platform::server_client_route::askpass::DesktopTunnelAskPass::new(
                paths.clone(),
                Arc::new(crate::platform::unix_askpass_transport::AskPassWindowFactory::new(None)),
            ),
            legacy: std::sync::Mutex::new(None),
            context: tokio::sync::Mutex::new(None),
            fixture: Some(fixture),
        };
        assert!(factory.foreground_setup().await.unwrap().is_none());
        let context = if !initialize && !restore_additional_client {
            let original = self
                .original_journal
                .lock()
                .unwrap()
                .clone()
                .expect("same-original relaunch requires the initial fixture journal");
            let context = reopen_retained_context(self.major, &paths, &original, async || {
                existing_fixture_context(&paths, &original.context).await
            })
            .await
            .unwrap_or_else(|failure| {
                panic!(
                    "same-original fixture context reopen failed: major={} failure={failure:?}",
                    self.major
                )
            });
            *factory.context.lock().await = Some(context.clone());
            context
        } else {
            // Initial and additional-client setup still exercise normal Factory
            // context creation. Existing-only recovery belongs to relaunch alone.
            let context = factory.context().await.unwrap();
            if !restore_additional_client {
                let original = FixtureJournalIdentity::capture(&paths, context.clone()).unwrap();
                let mut retained = self.original_journal.lock().unwrap();
                if let Some(retained) = retained.as_ref() {
                    retained.verify(&paths).unwrap();
                } else {
                    *retained = Some(original);
                }
            }
            context
        };
        if restore_additional_client {
            use spaceterm_client::journal::JournalLimits;
            use spaceterm_client_native::journal::NativeClientJournal;
            use spaceterm_core::operation_ledger::canonical::CanonicalCodec;

            let native = context.native.as_ref().unwrap();
            let directory = native
                .management()
                .supervisor()
                .client_connections_directory()
                .join("cli");
            let codec = Arc::new(CanonicalCodec);
            let verifier = Arc::new(crate::platform::desktop_client_startup::native::Verifier);
            let limits = JournalLimits::default();
            let storage = NativeClientJournal::open_existing(
                directory,
                codec.clone(),
                verifier.clone(),
                limits,
            )
            .await
            .unwrap();
            let journal = Arc::new(
                JournalController::recover(Arc::new(storage), codec, verifier, limits)
                    .await
                    .unwrap(),
            );
            let saved = journal.snapshot().unwrap();
            let [additional] = saved.bindings() else {
                panic!("explicit native provisioning must retain one additional Client");
            };
            assert!(matches!(
                &additional.proof().parts().claims,
                spaceterm_core::pairing::PairingReceiptBody::ClientBinding { .. }
            ));
            assert!(
                journal
                    .ordinary_use_allowed(&additional.binding().unwrap())
                    .unwrap()
            );
            *factory.context.lock().await = Some(Arc::new(
                crate::platform::desktop_client_startup::native::Context {
                    journal,
                    native: context.native.clone(),
                    native_failure: context.native_failure,
                },
            ));
        } else if initialize {
            factory.fixture.as_ref().unwrap().enroll(
                context.native.as_ref().unwrap(), context.journal.clone(),
            ).await.unwrap_or_else(|error| {
                panic!("native owner enrollment failed (server task finished {}, PTY launches {}): {error:?}", self.running.is_finished(), self.pty.launches().len())
            });
        }
        // Keep the admitted account provider, but recover the original binding on
        // later launches rather than re-enrolling the accessibility fixture.
        factory.fixture = None;
        let mut owners = factory.recover().await.unwrap();
        owners.services.credentials = Arc::new(
            crate::platform::desktop_client_startup::native::Credentials {
                store: spaceterm_client::pairing::journal::JournalPairingStore::new(
                    owners.journal.clone(),
                ),
                credentials,
            },
        );
        let opens = Arc::new(AtomicU64::new(0));
        owners.services.routes = Arc::new(CountingRoutes {
            inner: owners.services.routes,
            opens: opens.clone(),
        });
        let composition =
            crate::platform::desktop_client_startup::recover(owners, NativeViewStore::new(paths))
                .await
                .unwrap();
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "recovery must not connect or start shells"
        );
        let composition = if initialize {
            test_composition::initialize_workspace(composition)
                .await
                .unwrap()
        } else {
            composition
        };
        (composition, opens)
    }
    pub(super) async fn close(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.running.await.unwrap().unwrap();
        assert!(
            !self.pty.is_running(),
            "the owning runtime must join native cleanup"
        );
    }
}

async fn wait_for<T>(reason: &str, mut observe: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(value) = observe() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{reason}"))
}
#[derive(Clone, Copy, Debug)]
pub(super) enum ReadyStage {
    MeasuredInitial,
    MeasuredInitialAttachment,
    MeasuredRelaunch,
    MeasuredReattachment,
    MeasuredRestoredAttachment,
    CopyAttachment,
    UnsafePasteAttachment,
    UnsafePasteReattachment,
    EqualPasteFirstAttachment,
    EqualPasteFirstObservation,
    EqualPasteSecondObservation,
    EqualPasteSecondAttachment,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadyCheckpoint {
    BeforeConnect,
    AfterQueueAdmission,
    Poll,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadyConnection {
    Connecting(ConnectingPhase),
    Connected {
        epoch: u64,
        revision: u64,
        attempt: u64,
        generation: u64,
    },
    Offline {
        failure: ClientFailureClass,
        failures: u32,
        automatic_retry: bool,
    },
    Incompatible(CompatibilityExplanation),
    Stopped,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadyReplica {
    Empty,
    Ready {
        epoch: u64,
        revision: u64,
    },
    Stale {
        epoch: u64,
        revision: u64,
        reason: ResyncReason,
    },
    Resynchronizing {
        retained: Option<(u64, u64)>,
    },
}
#[derive(Debug)]
struct ReadyTransition {
    checkpoint: ReadyCheckpoint,
    connection: ReadyConnection,
    replica: ReadyReplica,
    route_opens: u64,
    journal_health: JournalHealth,
}
#[derive(Default)]
struct ReadyTrace {
    transitions: Vec<ReadyTransition>,
    dropped: usize,
}
impl ReadyTrace {
    fn record(
        &mut self,
        checkpoint: ReadyCheckpoint,
        composition: &Composition,
        handle: &ServerHandle,
        opens: &AtomicU64,
    ) -> (ConnectionState, ReplicaState) {
        let state = handle.state().borrow().clone();
        let replica = handle.replica().borrow().clone();
        let connection = match state {
            ConnectionState::Connecting { phase } => ReadyConnection::Connecting(phase),
            ConnectionState::Connected { info } => ReadyConnection::Connected {
                epoch: info.epoch.get(),
                revision: info.organization_revision.get(),
                attempt: info.attempt,
                generation: info.binding.generation.get(),
            },
            ConnectionState::Offline { failure, retry } => ReadyConnection::Offline {
                failure: failure.class(),
                failures: retry.failures,
                automatic_retry: retry.delay.is_some(),
            },
            ConnectionState::Incompatible { explanation } => {
                ReadyConnection::Incompatible(explanation)
            }
            ConnectionState::Stopped => ReadyConnection::Stopped,
        };
        let replica_phase = match &replica {
            ReplicaState::Empty => ReadyReplica::Empty,
            ReplicaState::Ready { snapshot } => ReadyReplica::Ready {
                epoch: snapshot.cursor.epoch.get(),
                revision: snapshot.cursor.revision.get(),
            },
            ReplicaState::Stale { retained, reason } => ReadyReplica::Stale {
                epoch: retained.cursor.epoch.get(),
                revision: retained.cursor.revision.get(),
                reason: *reason,
            },
            ReplicaState::Resynchronizing { retained } => ReadyReplica::Resynchronizing {
                retained: retained
                    .as_ref()
                    .map(|snapshot| (snapshot.cursor.epoch.get(), snapshot.cursor.revision.get())),
            },
        };
        let route_opens = opens.load(Ordering::SeqCst);
        let journal_health = *composition.tracker.journal().health().borrow();
        let unchanged = self.transitions.last().is_some_and(|previous| {
            previous.checkpoint != ReadyCheckpoint::BeforeConnect
                && previous.connection == connection
                && previous.replica == replica_phase
                && previous.route_opens == route_opens
                && previous.journal_health == journal_health
        });
        if checkpoint != ReadyCheckpoint::Poll || !unchanged {
            // Keep the two admission checkpoints and the latest fourteen changes.
            if self.transitions.len() == 16 {
                self.transitions.remove(2);
                self.dropped = self.dropped.saturating_add(1);
            }
            self.transitions.push(ReadyTransition {
                checkpoint,
                connection,
                replica: replica_phase,
                route_opens,
                journal_health,
            });
        }
        (state, replica)
    }
}
async fn ready(
    composition: &Composition,
    fixture: &ProductionFixture,
    opens: &AtomicU64,
    stage: ReadyStage,
) -> (ServerHandle, ConnectedInfo, Arc<ReplicaSnapshot>) {
    let handle = composition
        .registry
        .server(composition.servers[0].server)
        .unwrap();
    let mut trace = ReadyTrace::default();
    trace.record(ReadyCheckpoint::BeforeConnect, composition, &handle, opens);
    let admission = handle.connect().await;
    trace.record(
        ReadyCheckpoint::AfterQueueAdmission,
        composition,
        &handle,
        opens,
    );
    admission.unwrap_or_else(|failure| {
        panic!(
            "production connect admission failed: major={} stage={stage:?} failure={:?} transitions={:?}",
            fixture.major, failure.class(), trace.transitions
        )
    });
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (state, replica) = trace.record(ReadyCheckpoint::Poll, composition, &handle, opens);
            match (state, replica) {
                (ConnectionState::Connected { info }, ReplicaState::Ready { snapshot })
                    if info.organization_revision == snapshot.cursor.revision =>
                {
                    return (info, snapshot);
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let (info, snapshot) = result.unwrap_or_else(|_| {
        trace.record(ReadyCheckpoint::Poll, composition, &handle, opens);
        let pending_operations = composition.tracker.journal().snapshot().map(|state| {
            state.operations().iter().filter(|record| {
                matches!(record.state, RetainedOperationState::Pending)
            }).count()
        }).map_err(ClientFailure::class);
        panic!(
            "authenticated production replica did not become Ready: major={} stage={stage:?} transitions={:?} dropped={} server_phase={:?} server_task_finished={} pty_launches={} pty_resizes={} attached_workspaces={} registered_servers={} pending_operations={pending_operations:?}",
            fixture.major,
            trace.transitions,
            trace.dropped,
            fixture.server.phase(),
            fixture.running.is_finished(),
            fixture.pty.launches().len(),
            fixture.pty.resizes().len(),
            composition.view.workspaces().len(),
            composition.registry.servers().len(),
        )
    });
    eprintln!(
        "production replica Ready: major={} stage={stage:?} transitions={:?} dropped={}",
        fixture.major, trace.transitions, trace.dropped
    );
    (handle, info, snapshot)
}
fn binding(composition: &Composition) -> PairedServerBinding {
    composition.tracker.journal().snapshot().unwrap().bindings()[0]
        .binding()
        .unwrap()
}
fn observe<'a>(
    snapshot: &'a ReplicaSnapshot,
    binding: &PairedServerBinding,
    info: ConnectedInfo,
) -> ReplicaObservation<'a> {
    ReplicaObservation::new(
        &snapshot.organization,
        snapshot.accepted_creations(),
        ReplicaFacts {
            pairing: binding.target().pairing(),
            client: binding.target().client(),
            connection: Some(info.binding),
            readiness: ReplicaReadiness::Ready,
            status: ServerViewStatus::Connected,
        },
    )
    .unwrap()
}
pub(super) async fn attach(
    composition: &mut Composition,
    fixture: &ProductionFixture,
    opens: &AtomicU64,
    stage: ReadyStage,
) -> (WorkspaceLease, PaneChannel) {
    attach_workspace(composition, 0, fixture, opens, stage).await
}
async fn attach_workspace(
    composition: &mut Composition,
    index: usize,
    fixture: &ProductionFixture,
    opens: &AtomicU64,
    stage: ReadyStage,
) -> (WorkspaceLease, PaneChannel) {
    let (_, info, snapshot) = ready(composition, fixture, opens, stage).await;
    let binding = binding(composition);
    let observation = observe(&snapshot, &binding, info);
    composition.view.reconcile(&observation).unwrap();
    let workspace = snapshot.organization.state().workspaces[index].id;
    let (lease, _) = spaceterm_client::view::launch::attach(
        &mut composition.view,
        &observation,
        workspace,
        &binding,
        composition.tracker.journal(),
        &composition.channels,
    )
    .await
    .unwrap();
    composition
        .persistence
        .save(&composition.view.persisted().unwrap())
        .unwrap();
    let pane_id = snapshot
        .organization
        .state()
        .tabs
        .iter()
        .find(|tab| tab.workspace == workspace)
        .unwrap()
        .root_pane;
    let pane = wait_for("Pane publication did not reach the real attachment", || {
        lease.pane(pane_id).ok()
    })
    .await;
    (lease, pane)
}
pub(super) fn focus(pane: &PaneChannel) {
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
}
pub(super) fn measured_grid() -> MeasuredGrid {
    use spaceterm_terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, LogicalSize, TerminalGeometry,
    };
    let geometry = TerminalGeometry::from_viewport(
        LogicalSize::new(508.5, 333.0),
        LogicalCellSize::new(4.5, 9.0),
        BackingScale::new(2.0).unwrap(),
        CellGridSize::new(1, 1),
    );
    let cells = geometry.grid();
    let pixels = geometry.backing_cell_size();
    MeasuredGrid::new(
        u32::from(cells.rows),
        u32::from(cells.cols),
        CellPixels::new(
            pixels.width.try_into().unwrap(),
            pixels.height.try_into().unwrap(),
        )
        .unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap()
}
pub(super) async fn start(pane: &PaneChannel, pty: &RecordingPty) {
    let prior_launches = pty.launches().len();
    focus(pane);
    let token = pane
        .explicit_use(ExplicitUseSource::ExplicitSelection)
        .await
        .unwrap()
        .startup()
        .unwrap();
    assert_idle(pty, prior_launches).await;
    let ticket = pane.complete_intent(&token, measured_grid()).await.unwrap();
    completed(ticket).await;
    wait_for("measured Session did not become Running", || {
        pane.state()
            .borrow()
            .runtime
            .filter(|runtime| matches!(runtime.phase, SessionPhase::Running { .. }))
    })
    .await;
    live_input_authority(pane).await;
}
async fn live_input_authority(pane: &PaneChannel) {
    // Running publication precedes fresh content reconciliation. Neither it nor
    // a completed startup operation opens the Pane's effect-admission gate.
    wait_for(
        "native Pane did not acquire reconciled input authority",
        || {
            let frame = pane.current_frame()?;
            if !frame.permits_native_content_use() {
                return None;
            }
            pane.permit(EffectKind::Key).ok()
        },
    )
    .await;
}
async fn completed(ticket: OperationTicket) {
    let mut status = ticket.status();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let current = status.borrow_and_update().clone();
            match current {
                OperationStatus::Completed { outcome, .. } => {
                    assert!(
                        matches!(outcome, Outcome::SessionStarted(_)),
                        "measured startup failed: {outcome:?}"
                    );
                    return;
                }
                OperationStatus::Retired => panic!("production operation retired"),
                _ => status.changed().await.unwrap(),
            }
        }
    })
    .await
    .unwrap();
}
pub(super) async fn screen_contains(pane: &PaneChannel, text: &str) {
    wait_for(
        "native terminal output did not reach the desktop projection",
        || {
            let lease = pane.current_frame()?;
            let frame = lease.frame()?;
            let rendered: String = frame
                .pages()
                .iter()
                .flat_map(|page| page.page().parts().rows.iter())
                .flat_map(|row| row.cells())
                .map(|cell| cell.content().text())
                .collect();
            rendered.contains(text).then_some(())
        },
    )
    .await;
}
fn session(
    snapshot: &OrganizationSnapshot,
) -> (
    spaceterm_core::authority::ServerEpoch,
    PaneId,
    SessionGeneration,
) {
    let pane = &snapshot.state().panes[0];
    assert!(matches!(
        snapshot.runtime()[0].phase,
        SessionPhase::Running { .. }
    ));
    (
        snapshot.epoch(),
        pane.id,
        snapshot.runtime()[0].session_generation,
    )
}
pub(super) async fn quit(composition: Composition) {
    composition.runtime.shutdown().await.unwrap();
}
async fn assert_idle(pty: &RecordingPty, launches: usize) {
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(pty.launches().len(), launches);
    assert!(
        pty.resizes().is_empty(),
        "repaint/reconnect must not supply startup dimensions"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_native_production_listener_measured_startup_and_reattachment_both_majors() {
    for major in [1, 2] {
        let fixture = ProductionFixture::new(major).await;
        let (mut client, opens) = fixture.client(true).await;
        let (_, info, _) = ready(&client, &fixture, &opens, ReadyStage::MeasuredInitial).await;
        assert_eq!(info.major, major);
        assert_eq!(info.attempt, 1);
        let client_identity = (info.server, info.pairing, info.client);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_idle(&fixture.pty, 0).await;
        let (lease, pane) = attach(
            &mut client,
            &fixture,
            &opens,
            ReadyStage::MeasuredInitialAttachment,
        )
        .await;
        focus(&pane);
        assert_idle(&fixture.pty, 0).await;
        start(&pane, &fixture.pty).await;
        assert_eq!(
            fixture.pty.launches(),
            vec![NativePtySize {
                rows: 37,
                columns: 113,
                pixel_width: 1017,
                pixel_height: 666
            }]
        );
        assert_idle(&fixture.pty, 1).await;
        let original = session(&fixture.server.snapshot());
        fixture.pty.output(b"\x1b[2J\x1b[Hproduction-output");
        screen_contains(&pane, "production-output").await;
        let permit = pane.permit(EffectKind::ImeCommit).unwrap();
        pane.try_effect(
            &permit,
            RuntimeEffect::InputMethodCommit(
                spaceterm_terminal::runtime_contracts::BoundedInputMethodText::new(
                    "production-input".into(),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        wait_for(
            "production input did not reach the owning native PTY",
            || (fixture.pty.input() == b"production-input").then_some(()),
        )
        .await;
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "ordinary startup must not reconnect"
        );

        let binding = binding(&client);
        let workspace = client.view.workspaces()[0].workspace();
        let attachment = client.view.workspace(workspace).unwrap().attachment();
        let plan = client.view.prepare_detach(workspace).unwrap();
        client
            .pending
            .detach_attachment(&binding, attachment)
            .await
            .unwrap();
        client.view.apply_detach(plan).unwrap();
        client
            .persistence
            .save(&client.view.persisted().unwrap())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while !client
                .pending
                .drain_detach(&binding, workspace, client.operations.as_ref())
                .await
                .unwrap()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            pane.permit(EffectKind::ImeCommit).is_err(),
            "detached lease must lose input authority"
        );
        drop(lease);
        quit(client).await;
        assert!(fixture.pty.is_running());
        assert_eq!(session(&fixture.server.snapshot()), original);

        let (mut relaunched, opens) = fixture.client(false).await;
        assert!(
            relaunched.view.workspaces().is_empty(),
            "durable detach must survive desktop relaunch"
        );
        let (_, info, _) = ready(&relaunched, &fixture, &opens, ReadyStage::MeasuredRelaunch).await;
        assert_eq!(info.major, major);
        assert_eq!(info.attempt, 1);
        assert_eq!((info.server, info.pairing, info.client), client_identity);
        assert_idle(&fixture.pty, 1).await;
        let (_lease, reattached) = attach(
            &mut relaunched,
            &fixture,
            &opens,
            ReadyStage::MeasuredReattachment,
        )
        .await;
        focus(&reattached);
        assert_idle(&fixture.pty, 1).await;
        assert!(matches!(
            reattached
                .explicit_use(ExplicitUseSource::ExplicitSelection)
                .await
                .unwrap(),
            ExplicitUseOutcome::ControlReady(_)
        ));
        screen_contains(&reattached, "production-output").await;
        fixture.pty.output(b"\r\nreattached-output");
        screen_contains(&reattached, "reattached-output").await;
        assert_eq!(session(&fixture.server.snapshot()), original);
        assert_idle(&fixture.pty, 1).await;
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        quit(relaunched).await;

        let (mut quit_relaunch, opens) = fixture.client(false).await;
        assert_eq!(quit_relaunch.view.workspaces().len(), 1);
        let (_lease, restored) = attach(
            &mut quit_relaunch,
            &fixture,
            &opens,
            ReadyStage::MeasuredRestoredAttachment,
        )
        .await;
        focus(&restored);
        screen_contains(&restored, "reattached-output").await;
        assert_idle(&fixture.pty, 1).await;
        assert_eq!(session(&fixture.server.snapshot()), original);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        quit(quit_relaunch).await;
        fixture.close().await;
    }
}

#[derive(Clone, Copy, Debug)]
enum InputBarrierStage {
    CopyLegacy,
    CopyControl,
    CopyPlatform,
    CopyHidden,
    CopyDisabled,
    PasteReconnect,
}
async fn input_barrier(
    pane: &PaneChannel,
    pty: &RecordingPty,
    marker: &str,
    major: u32,
    stage: InputBarrierStage,
) {
    let permit = pane.permit(EffectKind::ImeCommit).unwrap();
    pane.try_effect(
        &permit,
        RuntimeEffect::InputMethodCommit(
            spaceterm_terminal::runtime_contracts::BoundedInputMethodText::new(marker.into())
                .unwrap(),
        ),
    )
    .unwrap();
    wait_for(
        &format!("ordered native input did not reach its PTY: major={major} stage={stage:?}"),
        || pty.input().ends_with(marker.as_bytes()).then_some(()),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_native_enhanced_copy_is_never_legacy_input() {
    use spaceterm_client::pane_channel::PaneRuntimeEffect;
    use spaceterm_terminal::InputModifiers;
    for major in [1, 2] {
        let fixture = ProductionFixture::new(major).await;
        let (mut client, opens) = fixture.client(true).await;
        let (_lease, pane) =
            attach(&mut client, &fixture, &opens, ReadyStage::CopyAttachment).await;
        start(&pane, &fixture.pty).await;
        let modifiers = InputModifiers {
            control: true,
            shift: true,
            ..Default::default()
        };
        let permit = pane.permit(EffectKind::Key).unwrap();
        let before = fixture.pty.input().len();
        pane.try_effect(&permit, PaneRuntimeEffect::Copy(modifiers))
            .unwrap();
        input_barrier(
            &pane,
            &fixture.pty,
            "legacy-barrier",
            major,
            InputBarrierStage::CopyLegacy,
        )
        .await;
        assert!(
            fixture.pty.input()[before..] == *b"legacy-barrier",
            "unavailable enhanced reporting must suppress Copy, not encode Ctrl+C"
        );
        fixture.pty.output(b"\x1b[>11uCOPY-ENHANCED");
        screen_contains(&pane, "COPY-ENHANCED").await;
        for (modifiers, expected, marker, stage) in [
            (
                modifiers,
                b"\x1b[99;6u\x1b[99;6:3u".as_slice(),
                "copy-control-barrier",
                InputBarrierStage::CopyControl,
            ),
            (
                InputModifiers {
                    platform: true,
                    ..Default::default()
                },
                b"\x1b[99;9u\x1b[99;9:3u".as_slice(),
                "copy-platform-barrier",
                InputBarrierStage::CopyPlatform,
            ),
        ] {
            let before = fixture.pty.input().len();
            pane.try_effect(&permit, PaneRuntimeEffect::Copy(modifiers))
                .unwrap();
            input_barrier(&pane, &fixture.pty, marker, major, stage).await;
            let input = fixture.pty.input();
            let mut bytes = expected.to_vec();
            bytes.extend_from_slice(marker.as_bytes());
            assert!(
                input[before..] == bytes,
                "Copy must reach the owning native runtime as enhanced press and release only"
            );
        }
        fixture.pty.set_hidden(true);
        let before = fixture.pty.input().len();
        pane.try_effect(&permit, PaneRuntimeEffect::Copy(modifiers))
            .unwrap();
        input_barrier(
            &pane,
            &fixture.pty,
            "hidden-barrier",
            major,
            InputBarrierStage::CopyHidden,
        )
        .await;
        assert!(
            fixture.pty.input()[before..] == *b"hidden-barrier",
            "hidden input must suppress native Copy forwarding"
        );
        fixture.pty.set_hidden(false);
        fixture.pty.output(b"\x1b[<uCOPY-DISABLED");
        screen_contains(&pane, "COPY-DISABLED").await;
        let before = fixture.pty.input().len();
        pane.try_effect(&permit, PaneRuntimeEffect::Copy(modifiers))
            .unwrap();
        input_barrier(
            &pane,
            &fixture.pty,
            "disabled-barrier",
            major,
            InputBarrierStage::CopyDisabled,
        )
        .await;
        assert!(fixture.pty.input()[before..] == *b"disabled-barrier");
        assert_eq!(fixture.pty.launches().len(), 1);
        quit(client).await;
        fixture.close().await;
    }
}

async fn next_paste_event(
    events: &mut spaceterm_client::pane_channel::PaneEvents,
) -> spaceterm_client::pane_channel::PaneEvent {
    use spaceterm_client::pane_channel::PaneEvent;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let event = events
                .next()
                .await
                .unwrap()
                .expect("native event subscription must remain open");
            match event {
                PaneEvent::Metadata { .. } | PaneEvent::Native { .. } => continue,
                other => return other,
            }
        }
    })
    .await
    .expect("correlated native paste event must arrive")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_native_unsafe_paste_preserves_runtime_and_native_request_correlations() {
    use spaceterm_client::{failure::ClientFailureClass, pane_channel::*};
    use spaceterm_core::native_requests::*;
    for major in [1, 2] {
        let fixture = ProductionFixture::new(major).await;
        let (mut client, opens) = fixture.client(true).await;
        let (_lease, pane) = attach(
            &mut client,
            &fixture,
            &opens,
            ReadyStage::UnsafePasteAttachment,
        )
        .await;
        start(&pane, &fixture.pty).await;
        let mut events = pane.events().unwrap();
        // Make the originating transport identity visibly distinct from the
        // Session-local first native request without writing ordinary input.
        let sequence = pane.permit(EffectKind::FocusReport).unwrap();
        pane.try_effect(&sequence, RuntimeEffect::FocusReport(true))
            .unwrap();
        pane.try_effect(&sequence, RuntimeEffect::FocusReport(true))
            .unwrap();
        let permit = pane.permit(EffectKind::Paste).unwrap();
        let origin = pane
            .try_effect(
                &permit,
                RuntimeEffect::RequestPaste(NativeText::new("one\r\ntwo\x03".into()).unwrap()),
            )
            .unwrap()
            .correlation()
            .unwrap();
        let PaneEvent::PasteApproval {
            correlation,
            request,
            timing,
        } = next_paste_event(&mut events).await
        else {
            panic!("correlated unsafe paste approval");
        };
        assert_eq!(correlation, origin);
        assert_eq!(request.fence(), origin.fence());
        assert_eq!(request.runtime_request().unwrap().get(), origin.request());
        assert_ne!(request.id().get(), origin.request());
        assert!(timing.expires_after.unwrap() <= Duration::from_secs(30));
        assert!(
            fixture.pty.input().is_empty(),
            "an approval must not report or perform a PTY write"
        );
        let NativeRequestKind::UnsafePaste { approval, text } = request.kind() else {
            panic!("unsafe paste kind");
        };
        assert!(text.as_str() == "one\ntwo");
        let approval = *approval;
        let wrong = NativeRequest::clone(&request)
            .with_runtime_request(origin.request() + 1)
            .unwrap();
        let wrong = NativeCompletion::new(
            &wrong,
            NativeCompletionKind::Paste {
                approval,
                decision: PasteDecision::Confirm,
            },
        )
        .unwrap();
        let native = pane.permit(EffectKind::NativeCompletion).unwrap();
        assert_eq!(
            pane.complete_native(&native, wrong).unwrap_err().class(),
            ClientFailureClass::StaleAuthority
        );
        assert!(request.is_current());
        let completion = NativeCompletion::new(
            &request,
            NativeCompletionKind::Paste {
                approval,
                decision: PasteDecision::Confirm,
            },
        )
        .unwrap();
        let resolved = pane
            .complete_native(&native, completion.clone())
            .unwrap()
            .correlation()
            .unwrap();
        assert_ne!(resolved.request(), origin.request());
        assert_ne!(resolved.request(), request.id().get());
        assert_eq!(resolved.fence(), request.fence());
        assert_eq!(
            pane.complete_native(&native, completion.clone())
                .unwrap_err()
                .class(),
            ClientFailureClass::StaleAuthority
        );
        let PaneEvent::PasteResolved {
            correlation,
            approval: id,
            decision,
            result,
        } = next_paste_event(&mut events).await
        else {
            panic!("exact native completion receipt");
        };
        assert_eq!(correlation, resolved);
        assert_eq!(id, approval);
        assert_eq!(decision, PasteDecision::Confirm);
        assert!(result.is_ok());
        assert!(fixture.pty.input() == b"one\rtwo");

        let origin = pane
            .try_effect(
                &permit,
                RuntimeEffect::RequestPaste(NativeText::new("cancel\npaste".into()).unwrap()),
            )
            .unwrap()
            .correlation()
            .unwrap();
        let PaneEvent::PasteApproval {
            correlation,
            request,
            ..
        } = next_paste_event(&mut events).await
        else {
            panic!("second exact approval");
        };
        assert_eq!(correlation, origin);
        assert_eq!(request.runtime_request().unwrap().get(), origin.request());
        assert_eq!(
            pane.complete_native(&native, completion)
                .unwrap_err()
                .class(),
            ClientFailureClass::StaleAuthority
        );
        assert!(
            request.is_current(),
            "duplicate completion must not resolve the next pending paste"
        );
        let NativeRequestKind::UnsafePaste { approval, .. } = request.kind() else {
            panic!("unsafe paste kind");
        };
        let cancelled = pane
            .try_effect(
                &permit,
                RuntimeEffect::ResolvePaste(*approval, PasteDecision::Cancel),
            )
            .unwrap()
            .correlation()
            .unwrap();
        let PaneEvent::PasteResolved {
            correlation,
            decision,
            result,
            ..
        } = next_paste_event(&mut events).await
        else {
            panic!("cancelled native paste receipt");
        };
        assert_eq!(correlation, cancelled);
        assert_eq!(decision, PasteDecision::Cancel);
        assert_eq!(result.unwrap_err().class(), ClientFailureClass::Cancelled);
        assert!(fixture.pty.input() == b"one\rtwo");

        let origin = pane
            .try_effect(
                &permit,
                RuntimeEffect::RequestPaste(NativeText::new("reconnect\npaste".into()).unwrap()),
            )
            .unwrap()
            .correlation()
            .unwrap();
        let PaneEvent::PasteApproval {
            correlation,
            request,
            ..
        } = next_paste_event(&mut events).await
        else {
            panic!("approval before reconnect");
        };
        assert_eq!(correlation, origin);
        let NativeRequestKind::UnsafePaste { approval, .. } = request.kind() else {
            panic!("unsafe paste kind");
        };
        let stale = NativeCompletion::new(
            &request,
            NativeCompletionKind::Paste {
                approval: *approval,
                decision: PasteDecision::Confirm,
            },
        )
        .unwrap();
        let original_session = session(&fixture.server.snapshot());
        quit(client).await;
        assert_eq!(
            pane.complete_native(&native, stale.clone())
                .unwrap_err()
                .class(),
            ClientFailureClass::StaleAuthority
        );
        let (mut client, opens) = fixture.client(false).await;
        let (_lease, reattached) = attach(
            &mut client,
            &fixture,
            &opens,
            ReadyStage::UnsafePasteReattachment,
        )
        .await;
        focus(&reattached);
        assert!(matches!(
            reattached
                .explicit_use(ExplicitUseSource::ExplicitSelection)
                .await
                .unwrap(),
            ExplicitUseOutcome::ControlReady(_)
        ));
        live_input_authority(&reattached).await;
        let native = reattached.permit(EffectKind::NativeCompletion).unwrap();
        assert_eq!(
            reattached
                .complete_native(&native, stale)
                .unwrap_err()
                .class(),
            ClientFailureClass::StaleAuthority
        );
        input_barrier(
            &reattached,
            &fixture.pty,
            "reconnect-barrier",
            major,
            InputBarrierStage::PasteReconnect,
        )
        .await;
        assert!(
            fixture.pty.input() == b"one\rtworeconnect-barrier",
            "reconnect must never replay the pending unsafe paste"
        );
        assert_eq!(session(&fixture.server.snapshot()), original_session);
        assert_eq!(fixture.pty.launches().len(), 1);
        quit(client).await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_native_equal_paste_request_ids_remain_session_local_both_majors() {
    use spaceterm_client::{failure::ClientFailureClass, pane_channel::*};
    use spaceterm_core::native_requests::*;
    for major in [1, 2] {
        let fixture = ProductionFixture::new(major).await;
        let (mut first_client, first_opens) = fixture.client(true).await;
        let (_first_lease, first) = attach(
            &mut first_client,
            &fixture,
            &first_opens,
            ReadyStage::EqualPasteFirstAttachment,
        )
        .await;
        start(&first, &fixture.pty).await;
        // One connection has one focused Pane. Keep both approvals live under
        // independent paired clients instead of claiming two input foci at once.
        let (mut second_client, second_opens) = fixture.independent_client().await;
        let (_, first_info, _) = ready(
            &first_client,
            &fixture,
            &first_opens,
            ReadyStage::EqualPasteFirstObservation,
        )
        .await;
        let (_, second_info, _) = ready(
            &second_client,
            &fixture,
            &second_opens,
            ReadyStage::EqualPasteSecondObservation,
        )
        .await;
        assert_eq!(first_info.server, second_info.server);
        assert_eq!(first_info.pairing, second_info.pairing);
        assert_ne!(first_info.client, second_info.client);
        let (_second_lease, second) = attach_workspace(
            &mut second_client,
            1,
            &fixture,
            &second_opens,
            ReadyStage::EqualPasteSecondAttachment,
        )
        .await;
        start(&second, &fixture.pty).await;
        live_input_authority(&first).await;
        let mut first_events = first.events().unwrap();
        let mut second_events = second.events().unwrap();
        let first_permit = first.permit(EffectKind::Paste).unwrap();
        let second_permit = second.permit(EffectKind::Paste).unwrap();
        let first_origin = first
            .try_effect(
                &first_permit,
                RuntimeEffect::RequestPaste(NativeText::new("first\npaste".into()).unwrap()),
            )
            .unwrap()
            .correlation()
            .unwrap();
        let PaneEvent::PasteApproval {
            correlation,
            request: first_request,
            ..
        } = next_paste_event(&mut first_events).await
        else {
            panic!("first Session approval");
        };
        assert_eq!(correlation, first_origin);
        let second_origin = second
            .try_effect(
                &second_permit,
                RuntimeEffect::RequestPaste(NativeText::new("second\npaste".into()).unwrap()),
            )
            .unwrap()
            .correlation()
            .unwrap();
        let PaneEvent::PasteApproval {
            correlation,
            request: second_request,
            ..
        } = next_paste_event(&mut second_events).await
        else {
            panic!("second Session approval");
        };
        assert_eq!(correlation, second_origin);
        assert_eq!(
            first_request.id(),
            second_request.id(),
            "native identifiers are allocated independently by each owning Session"
        );
        assert_ne!(
            first_request.fence().session(),
            second_request.fence().session()
        );
        assert_ne!(first_origin, second_origin);
        let NativeRequestKind::UnsafePaste {
            approval: first_approval,
            ..
        } = first_request.kind()
        else {
            panic!("first unsafe paste");
        };
        let NativeRequestKind::UnsafePaste {
            approval: second_approval,
            ..
        } = second_request.kind()
        else {
            panic!("second unsafe paste");
        };
        assert_eq!(first_approval, second_approval);
        first
            .try_effect(
                &first_permit,
                RuntimeEffect::ResolvePaste(*first_approval, PasteDecision::Cancel),
            )
            .unwrap();
        let PaneEvent::PasteResolved {
            correlation,
            result,
            ..
        } = next_paste_event(&mut first_events).await
        else {
            panic!("first cancellation");
        };
        assert_eq!(correlation.fence(), first_request.fence());
        assert_eq!(result.unwrap_err().class(), ClientFailureClass::Cancelled);
        assert!(
            second_request.is_current(),
            "cancelling one Session must not erase another Session's equal native ID"
        );
        assert!(fixture.pty.input().is_empty());
        second
            .try_effect(
                &second_permit,
                RuntimeEffect::ResolvePaste(*second_approval, PasteDecision::Confirm),
            )
            .unwrap();
        let PaneEvent::PasteResolved {
            correlation,
            result,
            ..
        } = next_paste_event(&mut second_events).await
        else {
            panic!("second confirmation");
        };
        assert_eq!(correlation.fence(), second_request.fence());
        assert!(result.is_ok());
        assert!(fixture.pty.input() == b"second\rpaste");
        assert!(fixture.pty.session_input(0).is_empty());
        assert!(fixture.pty.session_input(1) == b"second\rpaste");
        assert_eq!(fixture.pty.launches().len(), 2);
        quit(first_client).await;
        quit(second_client).await;
        fixture.close().await;
    }
}

mod retained_context_reopen_tests {
    use super::*;
    use std::cell::Cell;

    async fn original() -> (Root, Arc<AppPaths>, Arc<Context>, FixtureJournalIdentity) {
        let root = Root::new();
        let directories = AppDirectories::resolve_xdg(
            "production-acceptance-reopen",
            &AppDirectoryEnvironment {
                home: Some(root.0.clone().into_os_string()),
                xdg_state_home: Some(root.0.join("client-state").into_os_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let paths = Arc::new(
            AppPaths::from_directories(directories, 104, Arc::new(UnixSecureFilesystem)).unwrap(),
        );
        let journal = crate::platform::client_journal::open(
            &paths,
            crate::platform::client_journal::JournalOpenMode::Initialize,
            Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
            Arc::new(crate::platform::desktop_client_startup::native::Verifier),
            spaceterm_client::journal::JournalLimits::default(),
        )
        .await
        .unwrap();
        let context = Arc::new(Context {
            journal,
            native: None,
            native_failure: None,
        });
        let identity = FixtureJournalIdentity::capture(&paths, context.clone()).unwrap();
        (root, paths, context, identity)
    }

    #[tokio::test]
    async fn retained_context_reopen_retries_only_transient_storage_failure() {
        let (_root, paths, expected, identity) = original().await;
        let attempts = Cell::new(0);
        let recovered = reopen_retained_context(1, &paths, &identity, async || {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(ClientFailure::new(ClientFailureClass::StorageUnavailable))
            } else {
                existing_fixture_context(&paths, &expected).await
            }
        })
        .await
        .unwrap();
        assert_eq!(attempts.get(), 3);
        assert!(!Arc::ptr_eq(&recovered.journal, &expected.journal));
        assert_eq!(
            recovered.journal.snapshot().unwrap().revision(),
            expected.journal.snapshot().unwrap().revision(),
        );
        identity.verify(&paths).unwrap();
    }

    #[tokio::test]
    async fn retained_context_reopen_exhausts_after_five_attempts() {
        let (_root, paths, _context, identity) = original().await;
        let attempts = Cell::new(0);
        let result = reopen_retained_context(2, &paths, &identity, async || {
            attempts.set(attempts.get() + 1);
            Err(ClientFailure::new(ClientFailureClass::StorageUnavailable))
        })
        .await;
        assert_eq!(attempts.get(), 5);
        assert_eq!(
            result.err().unwrap(),
            FixtureReopenFailure::Open(ClientFailureClass::StorageUnavailable)
        );
    }

    #[tokio::test]
    async fn retained_context_reopen_refuses_every_other_failure_without_retry() {
        let (_root, paths, _context, identity) = original().await;
        for class in [
            ClientFailureClass::Unavailable,
            ClientFailureClass::AuthorizationDenied,
            ClientFailureClass::PinMismatch,
            ClientFailureClass::Incompatible,
            ClientFailureClass::MissingCapability,
            ClientFailureClass::InvalidPeer,
            ClientFailureClass::UnsafeEndpoint,
            ClientFailureClass::StaleAuthority,
            ClientFailureClass::TargetNotFound,
            ClientFailureClass::ResourceLimit,
            ClientFailureClass::UnsafeStorage,
            ClientFailureClass::InvalidStorage,
            ClientFailureClass::UnknownSchema,
            ClientFailureClass::RecoveryRequired,
            ClientFailureClass::CounterExhausted,
            ClientFailureClass::Cancelled,
        ] {
            let attempts = Cell::new(0);
            let result = reopen_retained_context(1, &paths, &identity, async || {
                attempts.set(attempts.get() + 1);
                Err(ClientFailure::new(class))
            })
            .await;
            assert_eq!(attempts.get(), 1, "{class:?}");
            assert_eq!(result.err().unwrap(), FixtureReopenFailure::Open(class));
        }
    }

    #[tokio::test]
    async fn retained_context_reopen_refuses_missing_storage_after_an_unavailable_attempt() {
        let (_root, paths, _context, identity) = original().await;
        let database =
            fixture_journal_directory(&paths).join(spaceterm_host::storage::DATABASE_FILE_NAME);
        let retained = database.with_file_name("original-fixture-database");
        let attempts = Cell::new(0);
        let result = reopen_retained_context(2, &paths, &identity, async || {
            attempts.set(attempts.get() + 1);
            // Fault injection preserves the original bytes and inode. Recovery
            // must not create a replacement after the name disappears.
            std::fs::rename(&database, &retained).unwrap();
            Err(ClientFailure::new(ClientFailureClass::StorageUnavailable))
        })
        .await;
        assert_eq!(attempts.get(), 1);
        assert_eq!(result.err().unwrap(), FixtureReopenFailure::MissingStorage);
        let existing_only = existing_fixture_context(&paths, &identity.context).await;
        // The retained native owner knows this original inode. Its disappearance
        // is unsafe storage, not a missing database that this process never saw.
        assert_eq!(
            existing_only.err().unwrap().class(),
            ClientFailureClass::UnsafeStorage
        );
        assert!(
            !database.exists(),
            "existing-only reopen must not initialize missing storage"
        );
        assert!(retained.exists());
    }

    #[tokio::test]
    async fn retained_context_reopen_refuses_changed_database_identity() {
        let (_root, paths, _context, identity) = original().await;
        let database =
            fixture_journal_directory(&paths).join(spaceterm_host::storage::DATABASE_FILE_NAME);
        let retained = database.with_file_name("original-fixture-database");
        let replacement = database.with_file_name("replacement-fixture-entry");
        std::fs::write(&replacement, b"").unwrap();
        let attempts = Cell::new(0);
        let result = reopen_retained_context(1, &paths, &identity, async || {
            attempts.set(attempts.get() + 1);
            // Replace only the test name with a distinct pre-existing inode.
            // The original database is retained and never opened by this check.
            std::fs::rename(&database, &retained).unwrap();
            std::fs::rename(&replacement, &database).unwrap();
            Err(ClientFailure::new(ClientFailureClass::StorageUnavailable))
        })
        .await;
        assert_eq!(attempts.get(), 1);
        assert_eq!(result.err().unwrap(), FixtureReopenFailure::ChangedStorage);
        assert!(retained.exists());
    }
}
