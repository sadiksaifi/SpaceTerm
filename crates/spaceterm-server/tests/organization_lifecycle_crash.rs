//! Owned-child SIGKILLs at the real SQLite authority's transaction boundaries.
//! Account registration, keys, actor, reducer, ledger and store are real. Runtime
//! ports come from the existing test support and must never launch: these cases
//! cover retained hierarchy and retirement with no live Sessions. They do not
//! establish native process cleanup or sudden-power-loss durability.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "support/job_a.rs"]
mod native;

use std::{
    collections::BTreeSet,
    fs,
    io::{Read as _, Write as _},
    os::unix::{fs::PermissionsExt as _, process::ExitStatusExt as _},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

use spaceterm_core::{
    authority::*,
    identity::*,
    operation_ledger::{canonical::CanonicalCodec, model::OperationLedger},
    operation_ledger_contracts::*,
    operations::*,
    organization::*,
    organization_retained::*,
    pairing::{policy::PairingSnapshot, *},
    retirement::*,
    storage::*,
};
use spaceterm_host::{
    account_service::{RegisteredServer, test_support::AccountServiceFixture},
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    startup::{BuildIdentity, DesiredServerState},
    storage::{
        Database,
        testing::{FaultPoint, TestControl},
    },
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, SigningKey},
};
use spaceterm_server::{
    account_startup::AccountStartup,
    adapters::{
        core::{CommitCursor, CorePorts},
        store::production_services,
    },
    api::*,
    contracts::*,
    pairing::protocol_lookup::{PairingProtocolLookup, PairingSnapshotPort},
    service::RecoveredServer,
};

const CHILD_ROOT: &str = "SPACETERM_ORGANIZATION_CRASH_ROOT";
const CHILD_CASE: &str = "SPACETERM_ORGANIZATION_CRASH_CASE";
const CHILD_STAGE: &str = "SPACETERM_ORGANIZATION_CRASH_STAGE";
const BARRIER: &[u8] = b"ORGANIZATION-CRASH-BARRIER\n";
// Shared-host full-sync waits can exceed the lightweight actor fixture's limit.
const DEADLINE: Duration = Duration::from_secs(120);
const NEXT_SEQUENCE: u64 = 8;

fn home(root: &Path, server: ServerId) -> ServerDirectory {
    ServerDirectory::new(server, root.as_os_str().as_encoded_bytes().to_vec()).unwrap()
}
fn stream(server: ServerId, key: u64) -> OperationStreamId {
    OperationStreamId::new(
        OperationStreamKey::new(server, key).unwrap(),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
    )
    .unwrap()
}
fn request(server: ServerId, sequence: u64, command: Command) -> CanonicalOperationRequest {
    CanonicalOperationRequest::new(
        OperationId::new(stream(server, 1), OperationSequence::new(sequence).unwrap()),
        command,
        &CanonicalCodec,
    )
    .unwrap()
}
fn account(root: &Path) -> AccountServiceFixture {
    AccountServiceFixture::new(root, BuildIdentity::Development).unwrap()
}
fn registration(root: &Path) -> RegisteredServer {
    account(root).reopen().discover().unwrap().unwrap()
}
fn identity(registration: &RegisteredServer) -> StoreIdentity {
    StoreIdentity {
        server: ServerId::new(registration.server_pin().identity),
        account: registration.key(),
        server_public_key: registration.server_pin().public_key,
    }
}
fn reopen(root: &Path) -> SqliteOrganizationStore {
    let startup = AccountStartup::open(registration(root)).unwrap();
    let server = ServerId::new(startup.registration().server_pin().identity);
    startup.open_store(home(root, server)).unwrap().store
}
fn recovered(root: &Path) -> RecoveredState {
    reopen(root).recover().unwrap()
}

struct AccountAuthority(RegisteredServer);
impl StartupAuthorityPort for AccountAuthority {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        assert_eq!(self.0.desired_state(), DesiredServerState::Running);
        Ok(ServerId::new(self.0.server_pin().identity))
    }
    fn validate_credentials(
        &self,
        credentials: &dyn ServerCredentialPort,
        server: ServerId,
    ) -> Result<(), ServerFailure> {
        assert_eq!(server.bytes(), self.0.server_pin().identity);
        assert_eq!(credentials.public_key(), self.0.server_pin().public_key);
        Ok(())
    }
}
struct Snapshot(PairingSnapshot);
impl PairingSnapshotPort for Snapshot {
    fn snapshot(&self) -> Result<PairingSnapshot, ProtocolError> {
        Ok(self.0.clone())
    }
}
struct MissingCleanup;
impl HostManagementPort for MissingCleanup {
    fn previous_instance_cleanup<'a>(
        &'a self,
        _: ServerId,
        _: ServerEpoch,
        _: &'a [SessionKey],
    ) -> ServerFuture<'a, Result<Vec<SessionCleanupObservation>, ServerFailure>> {
        Box::pin(async {
            Err(spaceterm_server::failure::failure(
                ServerFailureClass::CleanupIncomplete,
            ))
        })
    }
}

struct Composition {
    config: ServerConfig,
    dependencies: ServerDependencies,
    principal: VerifiedPrincipal,
    stop: Arc<native::Host>,
    runtime: Arc<native::Factory>,
    launch: Arc<native::Launch>,
    faults: TestControl,
}
impl Composition {
    fn open(root: &Path, missing_cleanup: bool) -> Self {
        let registration = registration(root);
        let store_identity = identity(&registration);
        let mut established = AccountStartup::open(registration)
            .unwrap()
            .open_store(home(root, store_identity.server))
            .unwrap();
        let recovered = established.store.recover().unwrap();
        let pin = established.registration.server_pin();
        let source = Snapshot(
            PairingSnapshot::new(
                recovered.organization.server,
                recovered.last_epoch,
                recovered.store_revision,
                recovered.high_water,
                recovered.pairing.clone().unwrap(),
            )
            .unwrap(),
        );
        let lookup = Arc::new(PairingProtocolLookup::new(source));
        let principal = authentication::Principal {
            server: pin.identity,
            device: [3; 16],
            pairing: native::local(1),
            client: native::local(1),
        };
        let (peer, hello) = authentication::ClientHandshake::begin(
            pin,
            principal,
            authentication::Purpose::Ordinary,
            authentication::HandshakeConfig::default(),
            native::TestKey::new(43),
        )
        .unwrap();
        let (host, reply) = authentication::ServerHandshake::accept(
            pin.identity,
            recovered.last_epoch.get() + 1,
            authentication::HandshakeConfig::default(),
            established.credentials.clone(),
            lookup.as_ref(),
            &hello,
        )
        .unwrap();
        let (peer, proof) = peer.receive_server(&reply).unwrap();
        let (authenticated, ready) = host.receive_device(lookup.as_ref(), &proof).unwrap();
        peer.receive_ready(&ready).unwrap();
        let authentication::Established::Ordinary(codec) = authenticated else {
            panic!("ordinary handshake")
        };
        let principal =
            VerifiedPrincipal::from_authenticated(&codec, ConnectionGeneration::new(1)).unwrap();
        let support = native::Fixture::new();
        let mut config = support.config;
        config.host.authority = Arc::new(AccountAuthority(established.registration));
        config.core = CorePorts::production(
            store_identity,
            home(root, recovered.organization.server),
            Arc::new(native::Clock),
        )
        .unwrap();
        let faults = established.store.authority_test_control();
        let mut dependencies = support.dependencies;
        dependencies.store = Box::new(established.store);
        dependencies.credentials = established.credentials;
        dependencies.pairing = lookup;
        if missing_cleanup {
            dependencies.host_management = Arc::new(MissingCleanup);
        }
        Self {
            config,
            dependencies,
            principal,
            stop: support.host,
            runtime: support.factory,
            launch: support.launch,
            faults,
        }
    }
}

fn initialize(root: &Path) {
    let registration = account(root)
        .initialize(DesiredServerState::Running)
        .unwrap();
    fs::create_dir(registration.organization_directory()).unwrap();
    fs::set_permissions(
        registration.organization_directory(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let identity = identity(&registration);
    let services = production_services(identity, home(root, identity.server)).unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    let mut store = match SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity,
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        services,
    ) {
        InitializationDisposition::Durable { store, .. } => store,
        _ => panic!("fixture initialization failed"),
    };
    // The public-key binding is isolated test admission, not enrollment or a
    // production credential adapter. Both streams and their capacity are real.
    let before = store.recover().unwrap();
    let cursor = CommitCursor::from_recovered(&before);
    let mut ledger = OperationLedger::recover(identity.server, before.ledger.unwrap()).unwrap();
    let mut prepared = ledger
        .prepare_register_stream(
            StreamBinding::from_stream(stream(identity.server, 1)),
            cursor.context().unwrap(),
        )
        .unwrap();
    ledger
        .extend_prepared(
            &mut prepared,
            vec![LedgerWrite::RegisterStream(
                StreamRecord::new(
                    StreamBinding::from_stream(stream(identity.server, 2)),
                    StreamPurpose::SelfRevocation,
                    StreamStatus::Active,
                    0,
                    0,
                    Some(OutcomeReservation::full(
                        ReservationClass::DedicatedSelfRevocation,
                    )),
                )
                .unwrap(),
            )],
        )
        .unwrap();
    let authorization = PairingAuthorization::new(PairingAuthorizationParts {
        record: spaceterm_core::pairing::PairingRecord {
            id: PairingId::new(identity.server, 1).unwrap(),
            device: DeviceId::new([3; 16]),
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: native::TestKey::new(42).public_key(),
            pairing_authentication_public_key: native::TestKey::new(43).public_key(),
        })
        .unwrap(),
        clients: vec![
            PairingClientStreams::new(PairingClientStreamsParts {
                client: ClientRecord {
                    id: ClientId::new(identity.server, 1).unwrap(),
                    pairing: PairingId::new(identity.server, 1).unwrap(),
                },
                ordinary: stream(identity.server, 1),
                self_revocation: stream(identity.server, 2),
            })
            .unwrap(),
        ],
        revocation_receipt: None,
    })
    .unwrap();
    let mut parts = cursor.parts().unwrap();
    parts.resulting_epoch = ServerEpoch::new(1);
    parts.resulting_high_water.client = 1;
    parts.resulting_high_water.pairing = 1;
    parts.resulting_high_water.operation_stream = 2;
    prepared.projection().append_to(&mut parts).unwrap();
    parts
        .operation_writes
        .push(OperationWrite::Pairing(Box::new(
            PairingWrite::PutAuthorization {
                expected_authorization_version: None,
                value: authorization,
            },
        )));
    let transaction = DurableTransaction::new_with_validation(
        parts,
        &production_services(identity, home(root, identity.server)).unwrap(),
    )
    .unwrap();
    let disposition = store.commit(&transaction);
    assert!(matches!(disposition, CommitDisposition::Durable(_)));
    ledger.resolve_commit(prepared, disposition).unwrap();
}

async fn submit(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    request: CanonicalOperationRequest,
) -> (Outcome, OrganizationRevision) {
    let OperationResponse::Completed {
        binding,
        revision,
        outcome,
    } = tokio::time::timeout(DEADLINE, handle.submit(principal, request.clone()))
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("completed operation")
    };
    assert_eq!(binding, request.binding());
    (outcome, revision)
}
async fn stop(
    server: RecoveredServer,
    host: Arc<native::Host>,
    runtime: Arc<native::Factory>,
    launch: Arc<native::Launch>,
) -> OrganizationState {
    assert!(
        runtime.starts.lock().unwrap().is_empty(),
        "recovery or metadata launched a Session"
    );
    assert_eq!(launch.preparations.load(Ordering::SeqCst), 0);
    let handle = server.handle();
    tokio::time::timeout(
        DEADLINE,
        handle.shutdown(spaceterm_server::shutdown::TrustedShutdownRequest { authority: host }),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(DEADLINE, server.wait())
        .await
        .unwrap()
        .unwrap();
    handle.snapshot().state().clone()
}
async fn baseline() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let parent = PathBuf::from("/private/tmp");
    // Fixtures and every child killed on penguin stay inside the assigned copy.
    #[cfg(target_os = "linux")]
    let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let root = tempfile::tempdir_in(parent).unwrap();
    initialize(root.path());
    let composition = Composition::open(root.path(), false);
    let principal = composition.principal;
    let id = principal.server();
    let server = RecoveredServer::recover(composition.config, composition.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, principal).await;
    let workspace = |n| WorkspaceId::new(id, n).unwrap();
    let pane = |n| PaneId::new(id, n).unwrap();
    let tab = TabId::new(id, 1).unwrap();
    for (sequence, name) in [(1, "primary"), (2, "secondary")] {
        assert!(matches!(
            submit(
                &handle,
                principal,
                request(
                    id,
                    sequence,
                    Command::CreateWorkspace {
                        server: id,
                        source: WorkspaceCreationSource::ServerHome { server: id },
                        name: NameDraft::Custom(NameText::new(name.into()).unwrap())
                    }
                )
            )
            .await
            .0,
            Outcome::CreatedWithControl(_)
        ));
    }
    assert!(matches!(
        submit(
            &handle,
            principal,
            request(
                id,
                3,
                Command::CreateTab {
                    workspace: workspace(1),
                    source: TabCreationSource::Pane(pane(1))
                }
            )
        )
        .await
        .0,
        Outcome::CreatedWithControl(_)
    ));
    for (sequence, source, axis, edge) in [
        (4, pane(1), SplitAxis::Vertical, SplitEdge::After),
        (5, pane(4), SplitAxis::Horizontal, SplitEdge::Before),
    ] {
        assert!(matches!(
            submit(
                &handle,
                principal,
                request(
                    id,
                    sequence,
                    Command::CreatePane {
                        tab,
                        source,
                        axis,
                        edge
                    }
                )
            )
            .await
            .0,
            Outcome::CreatedWithControl(_)
        ));
    }
    let pinned = root.path().join("pinned");
    fs::create_dir(&pinned).unwrap();
    assert_eq!(
        submit(
            &handle,
            principal,
            request(
                id,
                6,
                Command::SetPinnedDirectory {
                    workspace: workspace(1),
                    expected_version: FieldVersion::new(1),
                    value: Some(home(&pinned, id))
                }
            )
        )
        .await
        .0,
        Outcome::Applied
    );
    assert_eq!(
        submit(
            &handle,
            principal,
            request(
                id,
                7,
                Command::RenameWorkspace {
                    workspace: workspace(2),
                    expected_version: FieldVersion::new(1),
                    draft: NameDraft::Custom(NameText::new("renamed secondary".into()).unwrap())
                }
            )
        )
        .await
        .0,
        Outcome::Applied
    );
    assert_eq!(
        (
            handle.snapshot().state().workspaces.len(),
            handle.snapshot().state().tabs.len(),
            handle.snapshot().state().panes.len()
        ),
        (2, 3, 5)
    );
    stop(
        server,
        composition.stop,
        composition.runtime,
        composition.launch,
    )
    .await;
    root
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Tab,
    Split,
    ClosePane,
    CloseTab,
    Kill,
    EndAll,
}
impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Tab => "tab",
            Self::Split => "split",
            Self::ClosePane => "close-pane",
            Self::CloseTab => "close-tab",
            Self::Kill => "kill",
            Self::EndAll => "end-all",
        }
    }
    fn parse(name: &str) -> Self {
        match name {
            "tab" => Self::Tab,
            "split" => Self::Split,
            "close-pane" => Self::ClosePane,
            "close-tab" => Self::CloseTab,
            "kill" => Self::Kill,
            "end-all" => Self::EndAll,
            _ => panic!("unknown child case"),
        }
    }
    fn assessment(self, server: ServerId) -> Option<CanonicalOperationRequest> {
        let command = match self {
            Self::ClosePane => Command::AssessClose {
                target: CloseTarget::Pane(PaneId::new(server, 5).unwrap()),
            },
            Self::CloseTab => Command::AssessClose {
                target: CloseTarget::Tab(TabId::new(server, 1).unwrap()),
            },
            Self::Kill => Command::AssessKillWorkspace {
                workspace: WorkspaceId::new(server, 1).unwrap(),
            },
            Self::EndAll => Command::AssessEndAllSessions { server },
            Self::Tab | Self::Split => return None,
        };
        Some(request(server, NEXT_SEQUENCE, command))
    }
    fn request(self, server: ServerId) -> CanonicalOperationRequest {
        let assessment = self.assessment(server).map(|value| AssessmentRef {
            operation: value.binding().id,
            digest: value.binding().digest,
        });
        let command = match self {
            Self::Tab => Command::CreateTab {
                workspace: WorkspaceId::new(server, 1).unwrap(),
                source: TabCreationSource::Pane(PaneId::new(server, 1).unwrap()),
            },
            Self::Split => Command::CreatePane {
                tab: TabId::new(server, 1).unwrap(),
                source: PaneId::new(server, 5).unwrap(),
                axis: SplitAxis::Vertical,
                edge: SplitEdge::After,
            },
            Self::ClosePane => Command::CloseConfirmed {
                target: CloseTarget::Pane(PaneId::new(server, 5).unwrap()),
                assessment: assessment.unwrap(),
            },
            Self::CloseTab => Command::CloseConfirmed {
                target: CloseTarget::Tab(TabId::new(server, 1).unwrap()),
                assessment: assessment.unwrap(),
            },
            Self::Kill => Command::KillWorkspaceConfirmed {
                workspace: WorkspaceId::new(server, 1).unwrap(),
                assessment: assessment.unwrap(),
            },
            Self::EndAll => Command::EndAllSessions {
                server,
                assessment: assessment.unwrap(),
            },
        };
        request(
            server,
            NEXT_SEQUENCE + u64::from(assessment.is_some()),
            command,
        )
    }
}
const DESTRUCTIVE: [Case; 4] = [Case::ClosePane, Case::CloseTab, Case::Kill, Case::EndAll];

// Independent complete expected projection: no reducer, SQL or ledger encoder
// produces the expected hierarchy. Existing fields are preserved by equality.
fn expected(before: &RecoveredState, case: Case) -> (OrganizationState, IdentityHighWater) {
    let mut state = before.organization.clone();
    let mut marks = before.high_water;
    let server = state.server;
    let mut facts = state.retained.take().unwrap().parts().clone();
    state.revision = state.revision.checked_successor().unwrap();
    match case {
        Case::Tab | Case::Split => {
            marks.pane += 1;
            let pane = PaneId::new(server, marks.pane).unwrap();
            let (tab, creation, directory) = if matches!(case, Case::Tab) {
                marks.tab += 1;
                marks.creation_ordinal += 1;
                let tab = TabId::new(server, marks.tab).unwrap();
                state.tabs.push(TabRecord {
                    id: tab,
                    workspace: WorkspaceId::new(server, 1).unwrap(),
                    root_pane: pane,
                    worktree: None,
                    creation_ordinal: CreationOrdinal::new(marks.creation_ordinal),
                });
                facts
                    .creation_defaults
                    .push(TabCreationDefault::new(tab, vec![pane]).unwrap());
                facts
                    .tab_membership_versions
                    .push(TabMembershipVersion::new(tab, FieldVersion::new(1)));
                (
                    tab,
                    PaneCreation::Initial,
                    state.workspaces[0].pinned_directory.value.clone().unwrap(),
                )
            } else {
                let source = PaneId::new(server, 5).unwrap();
                let source_record = state.panes.iter().find(|value| value.id == source).unwrap();
                let tab = source_record.tab;
                let default = facts
                    .creation_defaults
                    .iter_mut()
                    .find(|value| value.tab() == tab)
                    .unwrap();
                let mut replay = default.replay_order().to_vec();
                replay.push(pane);
                *default = TabCreationDefault::new(tab, replay).unwrap();
                (
                    tab,
                    PaneCreation::SplitOf {
                        source,
                        axis: SplitAxis::Vertical,
                        edge: SplitEdge::After,
                    },
                    state.workspaces[0].pinned_directory.value.clone().unwrap(),
                )
            };
            marks.creation_ordinal += 1;
            state.panes.push(PaneRecord {
                id: pane,
                tab,
                creation,
                creation_ordinal: CreationOrdinal::new(marks.creation_ordinal),
                starting_directory: directory,
                confirmed_restoration_directory: Versioned {
                    version: FieldVersion::new(1),
                    value: None,
                },
            });
            facts.pane_generations.push(PaneGenerationHighWater::new(
                pane,
                SessionGeneration::new(0),
                ControlGeneration::new(1),
            ));
        }
        Case::EndAll => {
            for value in &mut facts.pane_generations {
                *value = PaneGenerationHighWater::new(
                    value.pane(),
                    value.session().checked_successor().unwrap(),
                    value.control(),
                );
            }
        }
        Case::ClosePane | Case::CloseTab | Case::Kill => {
            let deleted_tabs: BTreeSet<_> = state
                .tabs
                .iter()
                .filter(|value| match case {
                    Case::CloseTab => value.id.local() == 1,
                    Case::Kill => value.workspace.local() == 1,
                    _ => false,
                })
                .map(|value| value.id)
                .collect();
            state.panes.retain(|value| {
                !(deleted_tabs.contains(&value.tab)
                    || matches!(case, Case::ClosePane) && value.id.local() == 5)
            });
            state.tabs.retain(|value| !deleted_tabs.contains(&value.id));
            if matches!(case, Case::Kill) {
                state.workspaces.retain(|value| value.id.local() != 1);
            }
            if matches!(case, Case::CloseTab) {
                state.workspaces[0].root_tab = TabId::new(server, 3).unwrap();
            }
            let panes: BTreeSet<_> = state.panes.iter().map(|value| value.id).collect();
            facts
                .pane_generations
                .retain(|value| panes.contains(&value.pane()));
            facts
                .creation_defaults
                .retain(|value| !deleted_tabs.contains(&value.tab()));
            facts
                .tab_membership_versions
                .retain(|value| !deleted_tabs.contains(&value.id()));
            facts.workspace_names.retain(|value| {
                state
                    .workspaces
                    .iter()
                    .any(|record| record.id == value.workspace())
            });
            if matches!(case, Case::ClosePane) {
                let default = facts
                    .creation_defaults
                    .iter_mut()
                    .find(|value| value.tab().local() == 1)
                    .unwrap();
                *default = TabCreationDefault::new(
                    default.tab(),
                    default
                        .replay_order()
                        .iter()
                        .copied()
                        .filter(|value| value.local() != 5)
                        .collect(),
                )
                .unwrap();
            }
        }
    }
    state.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    state.validate().unwrap();
    (state, marks)
}

fn barrier() -> ! {
    std::io::stdout().write_all(BARRIER).unwrap();
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
async fn child_if_requested() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let case = Case::parse(&std::env::var(CHILD_CASE).unwrap());
    let stage = std::env::var(CHILD_STAGE).unwrap();
    let composition = Composition::open(&root, false);
    let server = RecoveredServer::recover(composition.config, composition.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    let principal = composition.principal;
    native::connect(&handle, principal).await;
    if let Some(assessment) = case.assessment(principal.server()) {
        assert!(matches!(
            submit(&handle, principal, assessment).await.0,
            Outcome::Assessment(_)
        ));
    }
    handle
        .reconcile(principal, handle.snapshot().state().revision)
        .await
        .unwrap();
    assert_eq!(
        handle
            .lookup_state(principal, case.request(principal.server()).binding())
            .await
            .unwrap(),
        OperationLookup::NeverAdmitted
    );
    let next = composition.faults.clone();
    // Admission is store-only. Select the next physical commit, not a timer or
    // an assumed application/publication race after the client sees a reply.
    composition
        .faults
        .barrier(FaultPoint::AfterCommit, move || {
            if stage == "admission-after" {
                barrier();
            }
            if stage == "final-before" || stage == "final-after" {
                let final_commit = next.clone();
                next.barrier(FaultPoint::AfterCommit, move || {
                    final_commit.barrier(
                        if stage == "final-before" {
                            FaultPoint::BeforeCommit
                        } else {
                            FaultPoint::AfterCommit
                        },
                        || barrier(),
                    );
                });
            } else {
                next.barrier(
                    if stage == "before" {
                        FaultPoint::BeforeCommit
                    } else {
                        FaultPoint::AfterCommit
                    },
                    || barrier(),
                );
            }
        });
    let result = handle
        .submit(principal, case.request(principal.server()))
        .await
        .unwrap();
    assert!(
        matches!(result, OperationResponse::Pending(_)),
        "the selected commit barrier was missed"
    );
    // Acceptance replies Pending. The actor must retain ownership and reach its
    // later final-outcome transaction independently of the submitting task.
    loop {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct OwnedChild(Child);
impl OwnedChild {
    fn kill_and_wait(&mut self) -> std::process::ExitStatus {
        self.0.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "owned child did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = self.0.kill();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn crash(test: &str, root: &Path, case: Case, stage: &str) {
    let mut child = OwnedChild(
        spaceterm_host::process::spawn(
            ProcessCommand::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(CHILD_ROOT, root)
                .env(CHILD_CASE, case.name())
                .env(CHILD_STAGE, stage)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit()),
        )
        .unwrap(),
    );
    let mut output = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut scanned = Vec::new();
        let mut byte = [0];
        for _ in 0..65_536 {
            if output.read_exact(&mut byte).is_err() {
                return;
            }
            scanned.push(byte[0]);
            if scanned.ends_with(BARRIER) {
                let _ = sender.send(());
                return;
            }
        }
    });
    let reached = receiver.recv_timeout(DEADLINE);
    let status = child.kill_and_wait();
    reader.join().unwrap();
    assert!(
        reached.is_ok(),
        "owned child did not reach authority barrier for {case:?}/{stage}"
    );
    assert_eq!(
        status.signal(),
        Some(9),
        "owned child must be killed, not exit cleanly"
    );
}
fn pending(recovered: &RecoveredState, binding: OperationBinding) -> &PendingOperationRecord {
    let entry = recovered
        .ledger
        .as_ref()
        .unwrap()
        .operations()
        .iter()
        .find(|value| match value {
            RecoveredOperationRecord::Pending(value) => value.binding.id == binding.id,
            RecoveredOperationRecord::Completed(value) => value.binding.id == binding.id,
        })
        .unwrap();
    let RecoveredOperationRecord::Pending(value) = entry else {
        panic!("original operation must remain pending")
    };
    assert_eq!(value.binding, binding);
    assert_eq!(value.progress_recorded_at, recovered.store_revision);
    assert!(value.admitted_at <= value.progress_recorded_at);
    if let Some(progress) = &value.external_progress {
        assert_eq!(progress.binding(), binding);
    }
    value
}
async fn replay(
    root: &Path,
    original: CanonicalOperationRequest,
    state: &OrganizationState,
    marks: IdentityHighWater,
    outcome_revision: OrganizationRevision,
    outcome: Outcome,
) {
    let composition = Composition::open(root, false);
    let server = RecoveredServer::recover(composition.config, composition.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    let principal = composition.principal;
    native::connect(&handle, principal).await;
    assert_eq!(handle.snapshot().state(), state);
    assert!(handle.snapshot().runtime().iter().all(|value| matches!(
        value.phase,
        SessionPhase::Ended { .. }
    ) && value.control
        == ControlRecord::Unclaimed));
    // An unrelated field commit must not rebase the original completion. Workspace
    // 2 survives every destructive case, and this uses the next unused sequence.
    let unrelated = handle
        .snapshot()
        .state()
        .workspaces
        .iter()
        .find(|value| value.id.local() == 2)
        .unwrap()
        .clone();
    let current_revision = state.revision.checked_successor().unwrap();
    assert_eq!(
        submit(
            &handle,
            principal,
            request(
                principal.server(),
                original.binding().id.sequence().get() + 1,
                Command::RenameWorkspace {
                    workspace: unrelated.id,
                    expected_version: unrelated.name.version,
                    draft: NameDraft::Custom(NameText::new("replay guard".into()).unwrap()),
                },
            ),
        )
        .await,
        (Outcome::Applied, current_revision)
    );
    assert_eq!(handle.snapshot().state().revision, current_revision);
    assert_ne!(current_revision, outcome_revision);
    let before = handle.snapshot();
    let sql_before = composition.faults.sql_calls();
    let lookup = handle
        .lookup_state(principal, original.binding())
        .await
        .unwrap();
    assert_eq!(
        lookup,
        OperationLookup::Completed {
            result: outcome.clone(),
            publication_revision: outcome_revision
        }
    );
    assert_eq!(
        submit(&handle, principal, original.clone()).await,
        (outcome.clone(), outcome_revision),
        "submission replay must return the original completion revision"
    );
    assert_eq!(handle.snapshot(), before);
    assert_eq!(
        composition.faults.sql_calls(),
        sql_before,
        "replay repeated a durable mutation"
    );
    assert!(composition.runtime.starts.lock().unwrap().is_empty());
    let stopped = stop(
        server,
        composition.stop,
        composition.runtime,
        composition.launch,
    )
    .await;
    let durable = recovered(root);
    assert_eq!(durable.organization, stopped);
    assert_eq!(durable.high_water, marks);
    let ledger =
        OperationLedger::recover(durable.organization.server, durable.ledger.unwrap()).unwrap();
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(original.binding().id.stream()),
                original.binding().id,
                Some(original.binding().digest)
            )
            .unwrap(),
        OperationLookup::Completed {
            result: outcome,
            publication_revision: outcome_revision,
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hierarchy_crash_before_commit_retains_old_complete_state_and_original_admission() {
    child_if_requested().await;
    for case in [Case::Tab, Case::Split] {
        let root = baseline().await;
        let before = recovered(root.path());
        let original = case.request(before.organization.server);
        crash(
            "hierarchy_crash_before_commit_retains_old_complete_state_and_original_admission",
            root.path(),
            case,
            "before",
        );
        let after = recovered(root.path());
        assert_eq!(after.organization, before.organization);
        assert_eq!(after.high_water, before.high_water);
        assert_eq!(
            pending(&after, original.binding()).kind,
            PendingKind::AtomicMutation
        );
        let (expected, marks) = expected(&before, case);
        // Retry is authorized only by the original durable admission. It allocates
        // once, from the old marks; no pre-COMMIT identity escaped to disk.
        let composition = Composition::open(root.path(), false);
        let server = RecoveredServer::recover(composition.config, composition.dependencies)
            .await
            .unwrap();
        native::connect(&server.handle(), composition.principal).await;
        assert!(matches!(
            submit(&server.handle(), composition.principal, original.clone())
                .await
                .0,
            Outcome::CreatedWithControl(_)
        ));
        assert_eq!(server.handle().snapshot().state(), &expected);
        let stopped = stop(
            server,
            composition.stop,
            composition.runtime,
            composition.launch,
        )
        .await;
        let outcome = completed_outcome(root.path(), original.binding());
        replay(
            root.path(),
            original,
            &stopped,
            marks,
            expected.revision,
            outcome,
        )
        .await;
    }
}
fn completed_outcome(root: &Path, binding: OperationBinding) -> Outcome {
    let durable = recovered(root);
    let entry = durable.ledger.unwrap().operations.into_iter().find(|value| matches!(value, RecoveredOperationRecord::Completed(value) if value.binding == binding)).unwrap();
    let RecoveredOperationRecord::Completed(value) = entry else {
        unreachable!()
    };
    value.outcome.outcome(&CanonicalCodec).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hierarchy_crash_after_commit_before_application_recovers_new_complete_state_once() {
    child_if_requested().await;
    for case in [Case::Tab, Case::Split] {
        let root = baseline().await;
        let before = recovered(root.path());
        let original = case.request(before.organization.server);
        let (expected, marks) = expected(&before, case);
        crash(
            "hierarchy_crash_after_commit_before_application_recovers_new_complete_state_once",
            root.path(),
            case,
            "after",
        );
        let after = recovered(root.path());
        assert_eq!(after.organization, expected);
        assert_eq!(after.high_water, marks);
        let outcome = completed_outcome(root.path(), original.binding());
        assert!(matches!(outcome, Outcome::CreatedWithControl(_)));
        replay(
            root.path(),
            original,
            &expected,
            marks,
            expected.revision,
            outcome,
        )
        .await;
    }
}

async fn destructive_matrix(test: &str, stage: &str) {
    for case in DESTRUCTIVE {
        let root = baseline().await;
        let before = recovered(root.path());
        let original = case.request(before.organization.server);
        let (expected, marks) = expected(&before, case);
        crash(test, root.path(), case, stage);
        let after = recovered(root.path());
        if stage == "admission-after" {
            assert_eq!(after.organization, before.organization);
            assert_eq!(after.high_water, before.high_water);
            assert!(matches!(
                pending(&after, original.binding())
                    .external_progress
                    .as_ref()
                    .unwrap()
                    .state(),
                ExternalProgressState::AwaitingIntent
            ));
            let composition = Composition::open(root.path(), false);
            let server = RecoveredServer::recover(composition.config, composition.dependencies)
                .await
                .unwrap();
            native::connect(&server.handle(), composition.principal).await;
            assert_eq!(server.handle().snapshot().state(), &before.organization);
            assert_eq!(
                submit(&server.handle(), composition.principal, original)
                    .await
                    .0,
                Outcome::Failed(OperationFailure::StaleAuthority)
            );
            stop(
                server,
                composition.stop,
                composition.runtime,
                composition.launch,
            )
            .await;
            continue;
        }
        assert_eq!(after.organization, expected);
        assert_eq!(after.high_water, marks);
        if stage == "final-after" {
            assert_eq!(
                completed_outcome(root.path(), original.binding()),
                Outcome::Applied
            );
        } else {
            let record = pending(&after, original.binding());
            let progress = record.external_progress.as_ref().unwrap();
            assert_eq!(progress.binding(), original.binding());
            let ExternalProgressState::AcceptedRetirement(descriptor) = progress.state() else {
                panic!("accepted retirement must survive")
            };
            assert_eq!(descriptor.binding(), original.binding());
            assert_eq!(descriptor.acceptance_revision(), expected.revision);
            assert_eq!(
                descriptor.parts().assessment,
                case.assessment(before.organization.server)
                    .map(|value| AssessmentRef {
                        operation: value.binding().id,
                        digest: value.binding().digest
                    })
                    .unwrap()
            );
            assert!(
                descriptor.sessions().is_empty(),
                "this fixture has no native cleanup evidence"
            );
            // A fresh registry is not proof of cleanup, even when there are no
            // recorded live Sessions. Refused native readiness changes nothing.
            let composition = Composition::open(root.path(), true);
            let error = RecoveredServer::recover(composition.config, composition.dependencies)
                .await
                .err()
                .unwrap();
            assert_eq!(error.class, ServerFailureClass::CleanupIncomplete);
            assert!(composition.runtime.starts.lock().unwrap().is_empty());
            assert_eq!(recovered(root.path()), after);
        }
        replay(
            root.path(),
            original,
            &expected,
            marks,
            expected.revision,
            Outcome::Applied,
        )
        .await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destructive_crash_after_admission_before_acceptance_keeps_old_hierarchy_without_effect_authority()
 {
    child_if_requested().await;
    destructive_matrix("destructive_crash_after_admission_before_acceptance_keeps_old_hierarchy_without_effect_authority", "admission-after").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destructive_crash_after_acceptance_preserves_deletion_or_ended_generations_and_pending_identity()
 {
    child_if_requested().await;
    destructive_matrix("destructive_crash_after_acceptance_preserves_deletion_or_ended_generations_and_pending_identity", "after").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_retirement_crash_before_final_outcome_preserves_accepted_progress() {
    child_if_requested().await;
    destructive_matrix(
        "empty_retirement_crash_before_final_outcome_preserves_accepted_progress",
        "final-before",
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_retirement_crash_after_final_outcome_replays_original_revision_without_second_deletion()
 {
    child_if_requested().await;
    destructive_matrix("empty_retirement_crash_after_final_outcome_replays_original_revision_without_second_deletion", "final-after").await;
}
