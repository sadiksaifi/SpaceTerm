use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    identity::*,
    operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*,
    operations::*,
    organization::*,
    storage::*,
    worktrees::{queries::GitQueryResult, *},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, SigningKey},
};
use spaceterm_server::{
    adapters::core::CorePorts,
    api::*,
    contracts::*,
    git::{authority::*, host::*, writes::*},
    service::RecoveredServer,
    shutdown::TrustedShutdownRequest,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
#[path = "../support/job_a.rs"]
pub(super) mod native;
#[allow(dead_code)]
#[path = "../store_pairing_composition/support.rs"]
pub(super) mod physical;

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
    fn pin(&self) -> authentication::ServerPin {
        authentication::ServerPin {
            identity: [11; 16],
            public_key: self.public_key(),
        }
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
struct Authority;
impl StartupAuthorityPort for Authority {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        Ok(physical::identity().server)
    }
    fn validate_credentials(
        &self,
        credentials: &dyn ServerCredentialPort,
        _: ServerId,
    ) -> Result<(), ServerFailure> {
        assert_eq!(
            credentials.public_key(),
            physical::identity().server_public_key
        );
        Ok(())
    }
}
struct Lookup(spaceterm_core::pairing::policy::PairingSnapshot);
impl authentication::PairingLookup for Lookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<authentication::PairingRecord, ProtocolError> {
        spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(self)
            .lookup(server, pairing, client)
    }
}
impl spaceterm_server::pairing::protocol_lookup::PairingSnapshotPort for &Lookup {
    fn snapshot(&self) -> Result<spaceterm_core::pairing::policy::PairingSnapshot, ProtocolError> {
        Ok(self.0.clone())
    }
}
impl PairingAuthorizationPort for Lookup {}
pub(super) struct Runner {
    pub(super) writes: AtomicUsize,
    pub(super) after_write: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl spaceterm_host::git::RepositoryProgramRunner for Runner {
    fn run(
        &self,
        request: &spaceterm_host::git::ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &GitReadCancellation,
    ) -> Result<spaceterm_host::git::ProgramExit, spaceterm_host::git::ProgramError> {
        if request
            .arguments
            .windows(2)
            .any(|v| v[0] == "worktree" && (v[1] == "add" || v[1] == "remove"))
        {
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
        let result = spaceterm_host::git::UnixRepositoryProgramRunner::new().run(
            request,
            stdout,
            cancellation,
        );
        if result.is_ok()
            && request
                .arguments
                .windows(2)
                .any(|v| v[0] == "worktree" && (v[1] == "add" || v[1] == "remove"))
            && let Some(action) = self.after_write.lock().unwrap().take()
        {
            action();
        }
        result
    }
}
struct NativeAccount(std::path::PathBuf);
impl NativeAccount {
    fn uid(&self) -> u32 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(&self.0).unwrap().uid()
    }
}
impl spaceterm_server::adapters::terminal::AccountLaunchProvider for NativeAccount {
    fn account(
        &self,
    ) -> Result<spaceterm_server::adapters::terminal::AccountLaunchFacts, ServerFailure> {
        Ok(spaceterm_server::adapters::terminal::AccountLaunchFacts {
            uid: self.uid(),
            home: self.0.clone(),
            planner: spaceterm_host::shell_launch::ShellLaunchPlanner::for_test(
                self.0.join("shell"),
                self.0.clone(),
            ),
            machine: spaceterm_terminal::metadata::LocalMachine::default(),
        })
    }
}
pub(super) struct Fixture {
    pub(super) directory: physical::Directory,
    pub(super) server: RecoveredServer,
    pub(super) handle: ServerHandle,
    pub(super) git: GitService,
    pub(super) principal: VerifiedPrincipal,
    pub(super) git_principal: AuthenticatedGitPrincipal,
    pub(super) stream: OperationStreamId,
    pub(super) stop: Arc<native::Host>,
    pub(super) runner: Arc<Runner>,
    pub(super) host: Arc<NativeServerGitHost>,
    pub(super) faults: spaceterm_host::storage::testing::TestControl,
    pub(super) workspace: WorkspaceId,
    pub(super) authenticator: spaceterm_server::connection::Authenticator,
    key: Arc<Key>,
    claim: authentication::Principal,
    initial_wire: Option<Wire>,
}
pub(super) fn run_git(directory: &Path, args: &[&str]) {
    let status = std::process::Command::new("/usr/bin/git")
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
}
impl Fixture {
    pub(super) async fn new() -> Self {
        Self::new_with_runtime(false).await
    }
    pub(super) async fn new_native_runtime() -> Self {
        Self::new_with_runtime(true).await
    }
    async fn new_with_runtime(native_runtime: bool) -> Self {
        let directory = physical::Directory::new();
        let root = directory.0.join("repository");
        std::fs::create_dir(&root).unwrap();
        run_git(&root, &["init", "--quiet", "--initial-branch=main"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "Fixture",
            ],
        );
        let mut storage = physical::Fixture::new(&directory.0);
        storage.enroll(42);
        Self::from_storage(directory, storage, native_runtime).await
    }
    pub(super) async fn reopen(directory: physical::Directory) -> Self {
        let mut store = physical::reopen(&directory.0).unwrap();
        let recovered = store.recover().unwrap();
        let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
            recovered.organization.server,
            recovered.ledger.unwrap(),
        )
        .unwrap();
        Self::from_storage(directory, physical::Fixture { store, ledger }, false).await
    }
    async fn from_storage(
        directory: physical::Directory,
        mut storage: physical::Fixture,
        native_runtime: bool,
    ) -> Self {
        let root = directory.0.join("repository");
        let home = ServerDirectory::new(
            physical::identity().server,
            directory.0.as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap();
        let provider = Arc::new(
            spaceterm_host::server_directories::native::NativeDirectoryProvider::new(
                home.server(),
                directory.0.clone(),
            )
            .unwrap(),
        );
        let runner = Arc::new(Runner {
            writes: AtomicUsize::new(0),
            after_write: std::sync::Mutex::new(None),
        });
        let host = Arc::new(NativeServerGitHost::new(
            runner.clone(),
            "/usr/bin/git".into(),
            directory.0.clone(),
            spaceterm_host::git::MINIMUM_GIT_VERSION,
            provider,
        ));
        let recovered = storage.recovered();
        let authorization = recovered.pairing.as_ref().unwrap().parts().authorizations[0].parts();
        let stream = authorization.clients[0].parts().ordinary;
        let lookup = Arc::new(Lookup(storage.snapshot()));
        let key = Arc::new(Key::new(71));
        let p = authentication::Principal {
            server: home.server().bytes(),
            device: authorization.record.device.bytes(),
            pairing: native::local(stream.pairing().local()),
            client: native::local(stream.client().local()),
        };
        let native = native::Fixture::new();
        let stop = native.host.clone();
        let mut config = native.config;
        config.host.authority = Arc::new(Authority);
        config.core = CorePorts::production_with_git_host(
            physical::identity(),
            home,
            Arc::new(native::Clock),
            host.clone(),
        )
        .unwrap();
        let faults = storage.store.authority_test_control();
        let external = Arc::new(
            spaceterm_server::git::recovery::GitExternalWork::new(
                physical::identity(),
                config.core.reducer.git_home().unwrap(),
                Arc::new(native::Clock),
                host.clone(),
            )
            .unwrap(),
        );
        let mut dependencies = native.dependencies;
        dependencies.external_work = external.clone();
        dependencies.store = Box::new(storage.store);
        dependencies.credentials = key.clone();
        dependencies.pairing = lookup.clone();
        if native_runtime {
            use spaceterm_server::adapters::terminal::{NativeRuntime, NativeRuntimeClock};
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(
                directory.0.join("shell"),
                "#!/bin/sh\nprintf '%s' \"$$\" > runtime-pid\nexec /bin/sh \"$@\"\n",
            )
            .unwrap();
            std::fs::set_permissions(
                directory.0.join("shell"),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            let account = Arc::new(NativeAccount(directory.0.clone()));
            let clock = Arc::new(NativeRuntimeClock::default());
            let runtime = NativeRuntime::new(
                physical::identity().server,
                account.uid(),
                account,
                clock.clone(),
                spaceterm_terminal::runtime::budget::RuntimeLimits::default(),
            )
            .unwrap();
            config.launch = runtime.clone();
            config.cleanup = runtime.clone();
            dependencies.runtime = runtime;
            dependencies.clock = clock;
        }
        let server = RecoveredServer::recover(config, dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        let authenticator = spaceterm_server::connection::Authenticator::new(
            spaceterm_server::connection::AuthenticationConfig {
                server: stream.server(),
                epoch: handle.snapshot().epoch(),
                owning_uid: {
                    use std::os::unix::fs::MetadataExt;
                    std::fs::metadata(&directory.0).unwrap().uid()
                },
                handshake: git_handshake(None),
                credentials: key.clone(),
                pairing: lookup,
                authority: Arc::new(handle.clone()),
                unauthenticated_limit: 16,
                authenticated_limit: 32,
                deadline: std::time::Duration::from_secs(5),
            },
        )
        .unwrap();
        let initial_wire = Wire::connect(&directory.0, &authenticator, &key, p, 2).await;
        let spaceterm_server::connection::PeerConnection::Ordinary(session) =
            &initial_wire.peer.connection
        else {
            panic!("ordinary")
        };
        let principal = session.principal;
        handle
            .reconcile(principal, handle.snapshot().state().revision)
            .await
            .unwrap();
        if handle.snapshot().state().workspaces.is_empty() {
            let request = CanonicalOperationRequest::new(
                OperationId::new(stream, OperationSequence::new(1).unwrap()),
                Command::CreateWorkspace {
                    server: stream.server(),
                    source: WorkspaceCreationSource::Directory(
                        ServerDirectory::new(
                            stream.server(),
                            root.as_os_str().as_encoded_bytes().to_vec(),
                        )
                        .unwrap(),
                    ),
                    name: NameDraft::Automatic,
                },
                &CanonicalCodec,
            )
            .unwrap();
            assert!(matches!(
                handle.submit(principal, request).await.unwrap(),
                OperationResponse::Completed { .. }
            ));
        }
        let workspace = handle.snapshot().state().workspaces[0].id;
        let access = handle
            .reconcile(principal, handle.snapshot().state().revision)
            .await
            .unwrap();
        let git_principal = AuthenticatedGitPrincipal::from_connection_access(&access).unwrap();
        let git = GitService::new(
            stream.server(),
            principal.epoch(),
            host.clone(),
            Arc::new(handle.clone()),
            GitLimits::default(),
        )
        .unwrap();
        git.recover(vec![]).await.unwrap();
        external.bind(git.clone()).unwrap();
        Self {
            directory,
            server,
            handle,
            git,
            principal,
            git_principal,
            stream,
            stop,
            runner,
            host,
            faults,
            workspace,
            authenticator,
            key,
            claim: p,
            initial_wire: Some(initial_wire),
        }
    }
    pub(super) async fn replace_client(&mut self, major: u32) {
        if let Some(wire) = self.initial_wire.take() {
            wire.close().await;
        }
        self.handle.disconnect(self.principal).await.unwrap();
        let wire = self.wire(major).await;
        let spaceterm_server::connection::PeerConnection::Ordinary(session) = &wire.peer.connection
        else {
            panic!("ordinary replacement client")
        };
        self.principal = session.principal;
        let access = self
            .handle
            .reconcile(self.principal, self.handle.snapshot().state().revision)
            .await
            .unwrap();
        self.git_principal = AuthenticatedGitPrincipal::from_connection_access(&access).unwrap();
        self.initial_wire = Some(wire);
    }
    pub(super) async fn wire(&self, major: u32) -> Wire {
        Wire::connect(
            &self.directory.0,
            &self.authenticator,
            &self.key,
            self.claim,
            major,
        )
        .await
    }
    pub(super) async fn submit_when_available(
        &self,
        request: CanonicalOperationRequest,
    ) -> OperationResponse {
        self.handle.submit(self.principal, request).await.unwrap()
    }
    pub(super) fn operation(&self, sequence: u64) -> OperationId {
        OperationId::new(self.stream, OperationSequence::new(sequence).unwrap())
    }
    pub(super) async fn inventory(&self) -> RepositoryScope {
        let result = self
            .git
            .query(
                self.git_principal,
                GitQuery::Inventory {
                    workspace: self.workspace,
                    cursor: None,
                },
            )
            .await
            .unwrap();
        let GitQueryResult::Inventory(page) = result else {
            panic!("inventory")
        };
        {
            assert!(!page.records.is_empty());
            self.handle
                .git_authority_snapshot()
                .await
                .unwrap()
                .repositories[0]
                .scope()
        }
    }
    pub(super) async fn preview(&self, repository: RepositoryScope, directory: &str) -> GitCommand {
        let target = self.directory.0.join(directory);
        let result = self
            .git
            .query(
                self.git_principal,
                GitQuery::PreviewCreate {
                    request: WorktreeCreateDraft::new(
                        repository,
                        WorktreeBranchChoice::New {
                            name: BranchName::new(directory.into()).unwrap(),
                            base: BranchReference::Local(BranchName::new("main".into()).unwrap()),
                        },
                        WorktreeLocationChoice::Explicit(
                            ServerDirectory::new(
                                self.stream.server(),
                                target.as_os_str().as_encoded_bytes().to_vec(),
                            )
                            .unwrap(),
                        ),
                    )
                    .unwrap(),
                },
            )
            .await
            .unwrap();
        let GitQueryResult::Assessment(page) = result else {
            panic!("preview")
        };
        GitCommand::CreateConfirmed {
            assessment: page.token,
        }
    }

    pub(super) async fn reserve(&self, repository: RepositoryScope, name: &str) -> GitReservation {
        let basis = self.handle.git_authority_snapshot().await.unwrap();
        let draft = WorktreeCreateDraft::new(
            repository,
            WorktreeBranchChoice::New {
                name: BranchName::new(name.into()).unwrap(),
                base: BranchReference::Local(BranchName::new("main".into()).unwrap()),
            },
            WorktreeLocationChoice::Explicit(
                ServerDirectory::new(
                    self.stream.server(),
                    self.directory
                        .0
                        .join(name)
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let host = self.host.clone();
        let (plan, _) = tokio::task::spawn_blocking(move || {
            host.creation_assessment(draft, &basis, &GitReadCancellation::default())
        })
        .await
        .unwrap()
        .unwrap();
        let token = GitAssessmentToken::new(GitAssessmentTokenParts {
            server: self.stream.server(),
            epoch: self.principal.epoch(),
            pairing: self.stream.pairing(),
            client: self.stream.client(),
            kind: GitAssessmentKind::Create,
            random: [53; 32],
        })
        .unwrap();
        let command = GitCommand::CreateConfirmed { assessment: token };
        let binding = CanonicalOperationRequest::new(
            self.operation(2),
            Command::Worktree(command.clone()),
            &CanonicalCodec,
        )
        .unwrap()
        .binding();
        self.handle
            .reserve_confirmed_git(ConfirmedGitAdmission {
                principal: self.git_principal,
                binding,
                command,
                plan: plan.as_ref().clone(),
            })
            .await
            .unwrap()
    }
    pub(super) async fn create_linked(&self, name: &str, sequence: u64) -> WorktreeId {
        let repository = self.inventory().await;
        let command = self.preview(repository, name).await;
        self.git
            .submit(self.git_principal, self.operation(sequence), command)
            .await
            .unwrap();
        let created = self.git.wait(self.operation(sequence)).await.unwrap();
        let GitMutationOutcomeParts::Created { worktree, .. } = created.parts() else {
            panic!("created")
        };
        *worktree
    }
    pub(super) async fn create_tab(&self, worktree: WorktreeId, sequence: u64) -> PaneId {
        let request = CanonicalOperationRequest::new(
            self.operation(sequence),
            Command::CreateTab {
                workspace: self.workspace,
                source: TabCreationSource::Worktree(worktree),
            },
            &CanonicalCodec,
        )
        .unwrap();
        let OperationResponse::Completed {
            outcome: Outcome::CreatedWithControl(created),
            ..
        } = self.handle.submit(self.principal, request).await.unwrap()
        else {
            panic!("created tab")
        };
        let CreatedEntities::Tab { pane, .. } = created.entities() else {
            panic!("tab entities")
        };
        pane
    }
    pub(super) async fn reserve_remove(
        &self,
        worktree: WorktreeId,
        sequence: u64,
    ) -> GitReservation {
        let basis = self.handle.git_authority_snapshot().await.unwrap();
        let host = self.host.clone();
        let (plan, _) = tokio::task::spawn_blocking(move || {
            host.removal_assessment(worktree, &basis, &GitReadCancellation::default())
        })
        .await
        .unwrap()
        .unwrap();
        let command = GitCommand::RemoveConfirmed {
            assessment: GitAssessmentToken::new(GitAssessmentTokenParts {
                server: self.stream.server(),
                epoch: self.principal.epoch(),
                pairing: self.stream.pairing(),
                client: self.stream.client(),
                kind: GitAssessmentKind::Remove,
                random: [54; 32],
            })
            .unwrap(),
        };
        let binding = CanonicalOperationRequest::new(
            self.operation(sequence),
            Command::Worktree(command.clone()),
            &CanonicalCodec,
        )
        .unwrap()
        .binding();
        self.handle
            .reserve_confirmed_git(ConfirmedGitAdmission {
                principal: self.git_principal,
                binding,
                command,
                plan: plan.as_ref().clone(),
            })
            .await
            .unwrap()
    }
    pub(super) async fn start_pane(
        &self,
        pane: PaneId,
        sequence: u64,
    ) -> spaceterm_core::authority::SessionKey {
        use spaceterm_core::{authority::*, control::*, geometry::*};
        let snapshot = self.handle.snapshot();
        let runtime = snapshot.runtime().iter().find(|v| v.pane == pane).unwrap();
        let ControlRecord::Held { client, generation } = runtime.control else {
            panic!("held")
        };
        let fence = EffectFence::new(
            SessionKey {
                pane,
                epoch: snapshot.epoch(),
                generation: runtime.session_generation,
            },
            ControlGrant::new(pane, client, generation).unwrap(),
            self.principal.binding(),
            ContextGeneration::new(1),
            None,
        )
        .unwrap();
        let intent =
            StartupIntent::new(self.operation(sequence), fence, StartupSource::StartSession)
                .unwrap();
        let request = CanonicalOperationRequest::new(
            self.operation(sequence),
            Command::BeginStartup(intent),
            &CanonicalCodec,
        )
        .unwrap();
        self.submit_when_available(request).await;
        let grid = GridSize::new(
            31,
            93,
            CellPixels::new(9, 19).unwrap(),
            SUPPORTED_GRID_LIMITS,
        )
        .unwrap();
        let request = CanonicalOperationRequest::new(
            self.operation(sequence + 1),
            Command::CompleteStartup(StartupMeasurement {
                intent: intent.id(),
                fence,
                grid,
            }),
            &CanonicalCodec,
        )
        .unwrap();
        let OperationResponse::Completed {
            outcome: Outcome::SessionStarted(session),
            ..
        } = self.submit_when_available(request).await
        else {
            panic!("started")
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if self.handle.snapshot().runtime().iter().any(|v| v.pane == pane && matches!(v.phase, SessionPhase::Running { generation } if generation == session.generation)) { break; }
                let _ = self.handle.refresh_runtime_observations().await;
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        session
    }
    pub(super) async fn stop(mut self) -> (physical::Directory, OperationStreamId) {
        if let Some(wire) = self.initial_wire.take() {
            wire.close().await;
        }
        self.authenticator.close().await.unwrap();
        self.git.shutdown().await.unwrap();
        self.handle
            .shutdown(TrustedShutdownRequest {
                authority: self.stop,
            })
            .await
            .unwrap();
        self.server.wait().await.unwrap();
        (self.directory, self.stream)
    }
}

pub(super) struct Wire {
    pub(super) peer: spaceterm_server::connection::AuthenticatedPeer,
    pub(super) client: Arc<spaceterm_host::ipc::IpcTransport>,
    pub(super) codec: spaceterm_protocol::connection::AuthenticatedConnection,
    listener: spaceterm_host::ipc::IpcListener,
}
fn git_handshake(major: Option<u32>) -> authentication::HandshakeConfig {
    authentication::HandshakeConfig {
        majors: major
            .map(|v| vec![v])
            .unwrap_or_else(|| spaceterm_protocol::negotiation::SUPPORTED_MAJORS.to_vec()),
        capabilities: vec![
            spaceterm_protocol::negotiation::CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
            spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
        ],
        ..Default::default()
    }
}
impl Wire {
    async fn connect(
        directory: &Path,
        authenticator: &spaceterm_server::connection::Authenticator,
        key: &Arc<Key>,
        claim: authentication::Principal,
        major: u32,
    ) -> Self {
        use spaceterm_host::{ipc::*, transport::*};
        let private = PrivateIpcDirectory::open(directory).unwrap();
        let mut random = [0; 4];
        getrandom::fill(&mut random).unwrap();
        let endpoint = private
            .endpoint(&format!("g{:08x}", u32::from_be_bytes(random)))
            .unwrap();
        let listener = IpcListener::bind(
            endpoint.clone(),
            MessageLimit::envelope(),
            AdmissionLimits::new(4, std::time::Duration::from_secs(5)).unwrap(),
        )
        .unwrap();
        let client = Arc::new(
            IpcTransport::connect(&endpoint, MessageLimit::envelope())
                .await
                .unwrap(),
        );
        let pending = listener.accept().await.unwrap();
        let permit = authenticator.try_admit().unwrap();
        let (peer, codec) = tokio::join!(authenticator.authenticate(pending, permit), async {
            let (handshake, hello) = authentication::ClientHandshake::begin(
                key.pin(),
                claim,
                authentication::Purpose::Ordinary,
                git_handshake(Some(major)),
                Arc::new(Key::new(43)),
            )
            .unwrap();
            client.send(&hello).await.unwrap();
            let reply = client.recv().await.unwrap().unwrap();
            let (handshake, proof) = handshake.receive_server(&reply).unwrap();
            client.send(&proof).await.unwrap();
            let ready = client.recv().await.unwrap().unwrap();
            let authentication::Established::Ordinary(codec) =
                handshake.receive_ready(&ready).unwrap()
            else {
                panic!("ordinary client")
            };
            codec
        });
        assert_eq!(codec.major(), major);
        Self {
            peer: peer.unwrap(),
            client,
            codec,
            listener,
        }
    }
    pub(super) async fn close(self) {
        use spaceterm_host::transport::MessageTransport;
        let Self {
            peer,
            client,
            codec: _,
            listener,
        } = self;
        client.close().await.unwrap();
        peer.close().await.unwrap();
        drop(client);
        listener.close().unwrap();
    }
    pub(super) fn into_parts(
        self,
    ) -> (
        spaceterm_server::connection::AuthenticatedPeer,
        Arc<spaceterm_host::ipc::IpcTransport>,
        spaceterm_protocol::connection::AuthenticatedConnection,
        spaceterm_host::ipc::IpcListener,
    ) {
        (self.peer, self.client, self.codec, self.listener)
    }
}
