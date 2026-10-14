//! Real account startup, SQLite, native shells and IPC. Only the service-manager
//! and previous-instance evidence boundaries are injected into this fixture.

use super::*;
use spaceterm_core::{
    authority::*, content::ContentBase, content_queries::*, control::*, geometry::*,
    native_management::*, retirement::*,
};
use spaceterm_host::account_service::{
    NativeServiceProcessTable, RegisteredServer, RuntimeAssessment, ServiceAction, ServiceFailure,
    ServiceFuture, ServiceProcesses,
};
use spaceterm_server::{
    account_startup::{RecoveredAccountServer, StartupFailure, test_support},
    adapters::terminal::AccountLaunchFacts,
};
use spaceterm_terminal::{metadata::LocalMachine, runtime_contracts::*};
use std::sync::Mutex;

#[path = "account_publications.rs"]
mod account_publications;
use account_publications::{PublicationCodec, handshake, query_status, reconcile, round_trip};

struct Seed {
    root: tempfile::TempDir,
    registration: RegisteredServer,
    identity: Arc<spaceterm_host::server_keys::ServerIdentity>,
    streams: PairingClientStreams,
    principal: authentication::Principal,
    key: Arc<DeviceKey>,
}
impl Seed {
    async fn new() -> Self {
        let fixture = Fixture::new_registered(true).await;
        let (streams, principal, key) = fixture.enroll(2).await;
        let handle = fixture.server.handle();
        let custody = spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
            &fixture.registration,
            handle.snapshot().epoch(),
            fixture.identity.clone(),
        )
        .unwrap();
        let (request, action) =
            spaceterm_server::shutdown::StoppedAuthorizationPort::verify_stopped(
                fixture.stop.as_ref(),
                handle.snapshot().state().server,
                handle.snapshot().epoch(),
            )
            .unwrap();
        let assessment = RuntimeAssessment {
            server: handle.snapshot().state().server,
            epoch: Some(handle.snapshot().epoch()),
            generation: 1,
            sessions: Vec::new(),
            token: [1; 32],
        };
        handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        fixture.server.wait().await.unwrap();
        custody
            .completed(&fixture.registration, request, action, &assessment, &[])
            .unwrap();
        let root = fixture._root;
        fs::write(root.path().join("shell"), "#!/bin/sh\nprintf '%s' \"$$\" > shell-pid\necho ACCOUNT-SHELL-READY\nexec /bin/sh \"$@\"\n").unwrap();
        fs::set_permissions(root.path().join("shell"), fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            registration: fixture.registration,
            identity: fixture.identity,
            streams,
            principal,
            key,
        }
    }
    fn supervisor(
        &self,
        processes: Arc<dyn ServiceProcesses>,
    ) -> Arc<spaceterm_host::account_service::ServiceSupervisor> {
        test_support::supervisor(self.root.path(), processes).unwrap()
    }
    fn launch(&self) -> AccountLaunchFacts {
        AccountLaunchFacts {
            uid: self.registration.key().account_uid,
            home: self.root.path().to_owned(),
            planner: spaceterm_host::shell_launch::ShellLaunchPlanner::for_test(
                self.root.path().join("shell"),
                self.root.path().to_owned(),
            ),
            machine: LocalMachine::default(),
        }
    }
    fn request(&self, sequence: u64, command: Command) -> CanonicalOperationRequest {
        CanonicalOperationRequest::new(
            OperationId::new(
                self.streams.parts().ordinary,
                OperationSequence::new(sequence).unwrap(),
            ),
            command,
            &CanonicalCodec,
        )
        .unwrap()
    }
}

/// The acceptance fixture knows exactly which shell it started. Cleanup success
/// requires that native process to disappear and the real endpoint to be removed.
struct ObservedProcesses {
    epoch: ServerEpoch,
    sessions: Vec<SessionKey>,
    pid: Option<u32>,
    reserved: Mutex<Option<RuntimeAssessment>>,
    original: Mutex<Option<RuntimeAssessment>>,
}
impl ObservedProcesses {
    fn new(epoch: ServerEpoch, sessions: Vec<SessionKey>, pid: Option<u32>) -> Arc<Self> {
        Arc::new(Self {
            epoch,
            sessions,
            pid,
            reserved: Mutex::default(),
            original: Mutex::default(),
        })
    }
    fn absent(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        if registration.endpoint()?.path().exists()
            || self.pid.is_some_and(|pid| {
                NativeServiceProcessTable::current()
                    .unwrap()
                    .observe(pid)
                    .unwrap()
                    .is_some()
            })
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        Ok(())
    }
}
impl ServiceProcesses for ObservedProcesses {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            if let Some(original) = self.original.lock().unwrap().clone() {
                return Ok(original);
            }
            Ok(RuntimeAssessment {
                server: ServerId::new(registration.server_pin().identity),
                epoch: Some(self.epoch),
                generation: 1,
                sessions: self.sessions.clone(),
                token: [9; 32],
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        _: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if assessment.epoch != Some(self.epoch) || assessment.sessions != self.sessions {
                return Err(ServiceFailure::StaleAssessment);
            }
            *self.reserved.lock().unwrap() = Some(assessment.clone());
            Ok(())
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        _: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if self
                .reserved
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|value| value != assessment)
            {
                return Err(ServiceFailure::StaleAssessment);
            }
            self.reserved.lock().unwrap().take();
            Ok(())
        })
    }
    fn quiesce<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        _: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            if assessment.epoch != Some(self.epoch) || assessment.sessions != self.sessions {
                return Err(ServiceFailure::StaleAssessment);
            }
            tokio::time::timeout(Duration::from_secs(10), async {
                while self.absent(registration).is_err() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
            assessment
                .sessions
                .iter()
                .map(|session| {
                    SessionCleanupObservation::new(
                        *session,
                        action.reason(),
                        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                    )
                    .map_err(|_| ServiceFailure::InvalidRecord)
                })
                .collect()
        })
    }
    fn verify_absent<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move { self.absent(registration) })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        _: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            self.absent(registration)?;
            if sessions.is_empty() {
                Ok(Vec::new())
            } else {
                Err(ServiceFailure::CleanupUnresolved)
            }
        })
    }
}
async fn stop(seed: &Seed, epoch: ServerEpoch, sessions: Vec<SessionKey>, pid: Option<u32>) {
    retire(seed, epoch, sessions, pid, ServiceAction::Stop).await;
}

async fn retire(
    seed: &Seed,
    epoch: ServerEpoch,
    sessions: Vec<SessionKey>,
    pid: Option<u32>,
    requested: ServiceAction,
) {
    use spaceterm_host::account_service::runtime_inventory::{
        AccountRuntimeCall, AccountRuntimeReply,
    };
    let transport = spaceterm_host::ipc::IpcTransport::connect(
        &seed.registration.endpoint().unwrap(),
        spaceterm_host::transport::MessageLimit::envelope(),
    )
    .await
    .unwrap();
    let mut codec = handshake(
        &transport,
        seed.identity.pin(),
        seed.principal,
        seed.key.clone(),
        2,
    )
    .await;
    let payload = |request, call| {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeRequest(
                spaceterm_protocol::wire::common::AccountRuntimeMessage {
                    request,
                    request_generation: request,
                    body: serde_json::to_vec(&call).unwrap(),
                },
            )),
        })
    };
    let supervisor = seed.supervisor(ObservedProcesses::new(epoch, sessions.clone(), pid));
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, requested)
        .unwrap();
    let assessment = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = round_trip(
                &transport,
                &mut codec,
                payload(1, AccountRuntimeCall::AssessNativeLifecycle(capture)),
            )
            .await;
            let Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(value)),
            }) = response
            else {
                panic!("runtime reply");
            };
            assert_eq!(value.request_generation, 1);
            match serde_json::from_slice::<AccountRuntimeReply>(&value.body).unwrap() {
                AccountRuntimeReply::NativeLifecycleAssessment(assessment) => {
                    assert_eq!(assessment.original, capture);
                    assert_eq!(assessment.epoch, epoch);
                    assert_eq!(assessment.affected_sessions as usize, sessions.len());
                    break assessment;
                }
                AccountRuntimeReply::Failed(
                    ServiceFailure::Pending | ServiceFailure::StaleAssessment,
                ) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                other => panic!("native assessment: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    // The owner cannot turn an assessment into accepted native work by resolving
    // it. Explicit confirmation is sent over the original authenticated connection.
    assert_eq!(
        supervisor.resolve(request).await.unwrap().phase,
        spaceterm_host::account_service::ServicePhase::Reserved
    );
    let confirmed = assessment.confirm();
    let response = round_trip(
        &transport,
        &mut codec,
        payload(
            2,
            AccountRuntimeCall::AdmitNativeLifecycle(confirmed.into_reference()),
        ),
    )
    .await;
    let Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::AccountRuntimeReply(value)),
    }) = response
    else {
        panic!("native acceptance reply");
    };
    assert_eq!(value.request_generation, 2);
    let AccountRuntimeReply::NativeLifecycleAccepted(accepted) =
        serde_json::from_slice::<AccountRuntimeReply>(&value.body).unwrap()
    else {
        panic!("native acceptance");
    };
    assert_eq!(accepted.request, request);
    assert_eq!(
        accepted.phase,
        spaceterm_host::account_service::ServicePhase::Accepted
    );
    // No server-side manager operation may wait for this server to terminate.
    // Native advancement happens only after acceptance, without a guest client key.
    transport.close().await.unwrap();
    let progress = supervisor.resolve(request).await.unwrap();
    assert_eq!(
        progress.phase,
        spaceterm_host::account_service::ServicePhase::Completed
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        if requested == ServiceAction::Stop {
            DesiredServerState::Stopped
        } else {
            DesiredServerState::Running
        }
    );
}

async fn operation(
    seed: &Seed,
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    sequence: u64,
    command: Command,
) -> Outcome {
    let request = seed.request(sequence, command);
    transport
        .send(
            &codec
                .encode(&request_payload(&request, codec.major()))
                .unwrap(),
        )
        .await
        .unwrap();
    loop {
        let payload = codec
            .decode(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        match &payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Outcome(_)),
            })
            | Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Outcome(_)),
            }) => return decode_outcome(payload, request.binding().id),
            Payload::Major2(v2::Message {
                kind:
                    Some(
                        v2::message::Kind::OrganizationSnapshot(_)
                        | v2::message::Kind::OrganizationDelta(_),
                    ),
            }) => {}
            _ => panic!("operation reply"),
        }
    }
}
async fn create(
    seed: &Seed,
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
) -> (WorkspaceId, PaneId, ControlGrant) {
    let result = operation(
        seed,
        transport,
        codec,
        1,
        Command::CreateWorkspace {
            server: ServerId::new(seed.identity.pin().identity),
            source: WorkspaceCreationSource::ServerHome {
                server: ServerId::new(seed.identity.pin().identity),
            },
            name: NameDraft::Automatic,
        },
    )
    .await;
    let Outcome::CreatedWithControl(created) = result else {
        panic!("created workspace: {result:?}");
    };
    let CreatedEntities::Workspace {
        workspace, pane, ..
    } = created.entities()
    else {
        panic!("workspace identities");
    };
    (workspace, pane, created.control().grant())
}
async fn start_shell(
    seed: &Seed,
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    epoch: ServerEpoch,
    generation: SessionGeneration,
    connection: ConnectionGeneration,
    grant: ControlGrant,
) -> SessionKey {
    let pane = grant.pane();
    let client = grant.client();
    let fence = EffectFence::new(
        SessionKey {
            pane,
            epoch,
            generation,
        },
        grant,
        ConnectionBinding::new(client, connection),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent = StartupIntent::new(
        OperationId::new(
            seed.streams.parts().ordinary,
            OperationSequence::new(2).unwrap(),
        ),
        fence,
        StartupSource::ExplicitSelection,
    )
    .unwrap();
    assert!(matches!(
        operation(seed, transport, codec, 2, Command::BeginStartup(intent)).await,
        Outcome::StartupAwaitingMeasurement { .. }
    ));
    let Outcome::SessionStarted(session) = operation(
        seed,
        transport,
        codec,
        3,
        Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
            intent: intent.id(),
            fence,
            grid: MeasuredGrid::new(
                24,
                80,
                CellPixels::new(8, 16).unwrap(),
                SUPPORTED_GRID_LIMITS,
            )
            .unwrap(),
        }),
    )
    .await
    else {
        panic!("session started");
    };
    session
}
async fn wait_pid(seed: &Seed) -> u32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = fs::read_to_string(seed.root.path().join("shell-pid"))
                && let Ok(pid) = text.parse()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn fence(
    authority: spaceterm_core::runtime_authority::RuntimeAuthority,
    base: Option<ContentBase>,
) -> EffectFence {
    let p = authority.parts();
    EffectFence::new(
        p.session,
        p.grant.unwrap(),
        p.connection.unwrap(),
        p.context,
        base.map(|base| base.revision),
    )
    .unwrap()
}
async fn assert_output(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    session: SessionKey,
    events: &mut dyn RuntimeStream<RuntimePresentationEvent>,
    expected: &str,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut base = None;
        let mut generation = 0;
        loop {
            if let Some(current) = base {
                generation += 1;
                let access = handle.runtime_access(principal, session).await.unwrap();
                let stamp = QueryStamp::new(
                    fence(access.authority, Some(current)),
                    current,
                    ContentRequestId::new(generation).unwrap(),
                    RequestGeneration::new(generation),
                )
                .unwrap();
                let query = ContentPageRequest::new(
                    stamp,
                    ContentRange::ActiveGrid,
                    None,
                    PageLimits::new(128, 16384, 1024 * 1024).unwrap(),
                )
                .unwrap();
                match access.handle.read_page(query).unwrap().await.unwrap() {
                    ContentReadResult::Ready(reply) => {
                        let text = reply
                            .page()
                            .parts()
                            .rows
                            .iter()
                            .map(|row| {
                                row.cells()
                                    .iter()
                                    .map(|cell| cell.content().text())
                                    .collect::<String>()
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if text.contains(expected) {
                            break;
                        }
                    }
                    ContentReadResult::ResyncRequired { current, .. } => {
                        base = Some(current);
                        continue;
                    }
                    ContentReadResult::Rejected { .. } => panic!("page rejected"),
                }
            }
            if let RuntimePresentationEvent::ContentInvalidated(current) =
                events.next().await.unwrap().unwrap()
            {
                base = Some(current);
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn admitted_account_composes_real_shell_and_holds_instance_until_native_stop() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let seed = Seed::new().await;
        let supervisor = seed.supervisor(Arc::new(test_support::StartupAbsence));
        let startup = AccountStartup::admit(&supervisor).unwrap();
        assert_eq!(
            AccountStartup::admit(&supervisor).unwrap_err(),
            StartupFailure::Account(ServiceFailure::Pending)
        );
        let server = RecoveredAccountServer::recover(supervisor.clone(), startup, seed.launch())
            .await
            .unwrap();
        let handle = server.handle();
        let run = tokio::spawn(server.run_until(std::future::pending()));
        let endpoint = seed.registration.endpoint().unwrap();
        while handle.phase() != ServicePhase::Serving {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let transport = IpcTransport::connect(&endpoint, MessageLimit::envelope())
            .await
            .unwrap();
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let connection =
            ConnectionGeneration::new(reconcile(&transport, &mut codec, &handle).await);
        let principal = VerifiedPrincipal::from_authenticated(&codec, connection).unwrap();
        let (workspace, pane, grant) = create(&seed, &transport, &mut codec).await;
        let record = handle
            .snapshot()
            .runtime()
            .iter()
            .find(|value| value.pane == pane)
            .copied()
            .unwrap();
        let session = start_shell(
            &seed,
            &transport,
            &mut codec,
            handle.snapshot().epoch(),
            record.session_generation,
            connection,
            grant,
        )
        .await;
        let pid = wait_pid(&seed).await;
        while !handle.snapshot().runtime().iter().any(|record| {
            record.pane == pane && matches!(record.phase, SessionPhase::Running { .. })
        }) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle
            .update_client_focus(
                principal,
                ClientFocusUpdate {
                    context: ContextGeneration::new(2),
                    focused_pane: Some(pane),
                    terminal_input_focus: true,
                    native_effects_eligible: true,
                },
            )
            .await
            .unwrap();
        let access = handle.runtime_access(principal, session).await.unwrap();
        let mut events = handle
            .subscribe_runtime(principal, fence(access.authority, None))
            .await
            .unwrap();
        handle
            .runtime_command(
                principal,
                FencedRuntimeCommand {
                    fence: fence(access.authority, None),
                    command: RuntimeCommand::InputMethodCommit(
                        BoundedInputMethodText::new("echo ACCOUNT-$(printf ECHO)-VERIFIED".into())
                            .unwrap(),
                    ),
                },
            )
            .await
            .unwrap();
        let mut enter = spaceterm_terminal::KeyInput::text_input("");
        enter.physical_key = spaceterm_terminal::PhysicalKey::Enter;
        enter.logical_key = "enter".into();
        enter.text = None;
        handle
            .runtime_command(
                principal,
                FencedRuntimeCommand {
                    fence: fence(access.authority, None),
                    command: RuntimeCommand::Key(enter),
                },
            )
            .await
            .unwrap();
        assert_output(
            &handle,
            principal,
            session,
            &mut *events,
            "ACCOUNT-ECHO-VERIFIED",
        )
        .await;
        transport.close().await.unwrap();
        assert!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(pid)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            AccountStartup::admit(&supervisor).unwrap_err(),
            StartupFailure::Account(ServiceFailure::Pending)
        );
        stop(&seed, session.epoch, vec![session], Some(pid)).await;
        let report = run.await.unwrap().unwrap();
        assert!(
            report.cleanup_confirmed && report.listeners_closed && report.started_writes_finished
        );
        assert!(!endpoint.path().exists());
        assert!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(pid)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            AccountStartup::admit(&supervisor).unwrap_err(),
            StartupFailure::DeliberatelyStopped
        );
        assert!(
            handle
                .snapshot()
                .state()
                .workspaces
                .iter()
                .any(|value| value.id == workspace)
        );
    })
    .await
    .unwrap();
}

struct Binary(std::process::Child);
impl Binary {
    fn start(seed: &Seed) -> Self {
        Self(
            std::process::Command::new(env!("CARGO_BIN_EXE_spaceterm-server"))
                .args([
                    std::ffi::OsStr::new("--test-account-root"),
                    seed.root.path().as_os_str(),
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }
    async fn wait(&mut self) -> std::process::ExitStatus {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn connect(&mut self, seed: &Seed) -> IpcTransport {
        self.connect_with_deadline(seed, Duration::from_secs(10))
            .await
    }
    async fn connect_with_deadline(&mut self, seed: &Seed, deadline: Duration) -> IpcTransport {
        let endpoint = seed.registration.endpoint().unwrap();
        tokio::time::timeout(deadline, async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    use std::io::Read;
                    let mut message = String::new();
                    self.0
                        .stderr
                        .take()
                        .unwrap()
                        .read_to_string(&mut message)
                        .unwrap();
                    panic!("account binary exited {status}: {message}");
                }
                if let Ok(transport) =
                    IpcTransport::connect(&endpoint, MessageLimit::envelope()).await
                {
                    break transport;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}
impl Drop for Binary {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            // Let the real owner join its PTY workers even when an assertion fails.
            let _ = std::process::Command::new("/bin/kill")
                .args(["-TERM", &self.0.id().to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while self.0.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.0.try_wait().unwrap().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
}
async fn snapshot(
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    generation: u64,
) -> v2::OrganizationSnapshot {
    codec.request_snapshot(generation);
    let request = Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::OrganizationQuery(
            v2::OrganizationQuery {
                request: generation,
                request_generation: generation,
                known_epoch: codec.epoch(),
                ..Default::default()
            },
        )),
    });
    transport
        .send(&codec.encode(&request).unwrap())
        .await
        .unwrap();
    loop {
        let payload = codec
            .decode(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        match payload {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationSnapshot(value)),
            }) => {
                let part = value.part.as_ref().unwrap();
                assert_eq!(
                    (part.request, part.request_generation),
                    (generation, generation)
                );
                assert_eq!(value.epoch, codec.epoch());
                assert!(value.complete);
                return value;
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationDelta(_)),
            }) => {}
            _ => panic!("organization snapshot"),
        }
    }
}
async fn reconcile_snapshot(
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    snapshot: &v2::OrganizationSnapshot,
) -> ConnectionGeneration {
    let principal = codec.principal();
    let client = ClientId::new(
        ServerId::new(principal.server),
        u64::from_be_bytes(principal.client[8..].try_into().unwrap()),
    )
    .unwrap();
    let request = Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::Reconciliation(v2::Reconciliation {
            request: 1,
            server: snapshot.server.clone(),
            client: Some(spaceterm_protocol::convert::identity::encode_client(client)),
            epoch: snapshot.epoch,
            organization_revision: snapshot.revision,
            ..Default::default()
        })),
    });
    let Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::Reconciliation(value)),
    }) = round_trip(transport, codec, request).await
    else {
        panic!("reconciliation");
    };
    assert!(value.acknowledged);
    ConnectionGeneration::new(value.connection_generation)
}

#[tokio::test]
async fn real_binary_authenticates_starts_shell_stops_and_restores_organization() {
    native_restart_after_live_session(false).await;
}

#[tokio::test]
async fn real_binary_restarts_with_live_session_and_reuses_completed_custody() {
    native_restart_after_live_session(true).await;
}

async fn native_restart_after_live_session(restart: bool) {
    tokio::time::timeout(Duration::from_secs(45), async {
        let seed = Seed::new().await;
        let mut binary = Binary::start(&seed);
        let transport = binary.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let before = snapshot(&transport, &mut codec, 1).await;
        let connection = reconcile_snapshot(&transport, &mut codec, &before).await;
        let (workspace, pane, grant) = create(&seed, &transport, &mut codec).await;
        let created = snapshot(&transport, &mut codec, 2).await;
        let (_, record) =
            spaceterm_protocol::convert::organization::v2::decode_pane_record(&created.panes[0])
                .unwrap();
        let session = start_shell(
            &seed,
            &transport,
            &mut codec,
            ServerEpoch::new(created.epoch),
            record.session_generation,
            connection,
            grant,
        )
        .await;
        let pid = wait_pid(&seed).await;
        assert_eq!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(pid)
                .unwrap()
                .unwrap()
                .uid(),
            seed.registration.key().account_uid
        );
        // The second binary cannot reopen the actor's store or replace its endpoint.
        let mut duplicate = Binary::start(&seed);
        assert!(!duplicate.wait().await.success());
        assert!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(pid)
                .unwrap()
                .is_some()
        );
        transport.close().await.unwrap();
        retire(
            &seed,
            session.epoch,
            vec![session],
            Some(pid),
            if restart {
                ServiceAction::Restart
            } else {
                ServiceAction::Stop
            },
        )
        .await;
        assert!(binary.wait().await.success());
        assert!(!seed.registration.endpoint().unwrap().path().exists());
        assert!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(pid)
                .unwrap()
                .is_none()
        );
        if !restart {
            let mut stopped = Binary::start(&seed);
            assert!(!stopped.wait().await.success());
            assert!(!seed.registration.endpoint().unwrap().path().exists());
            let supervisor =
                seed.supervisor(ObservedProcesses::new(session.epoch, Vec::new(), None));
            let request = supervisor.allocate_request().unwrap();
            supervisor.start(request).await.unwrap();
        }
        fs::remove_file(seed.root.path().join("shell-pid")).unwrap();
        let mut restarted = Binary::start(&seed);
        let transport = restarted.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let restored = snapshot(&transport, &mut codec, 1).await;
        assert!(restored.epoch > created.epoch);
        assert_eq!(
            spaceterm_protocol::convert::identity::decode_workspace(
                restored.workspaces[0].id.as_ref().unwrap()
            )
            .unwrap(),
            workspace
        );
        assert_eq!(
            spaceterm_protocol::convert::identity::decode_pane(
                restored.panes[0].id.as_ref().unwrap()
            )
            .unwrap(),
            pane
        );
        assert!(
            !seed.root.path().join("shell-pid").exists(),
            "restored panes must wait for explicit selection"
        );
        transport.close().await.unwrap();
        stop(&seed, ServerEpoch::new(restored.epoch), Vec::new(), None).await;
        assert!(restarted.wait().await.success());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_binary_sigterm_restarts_original_identity_and_organization_with_live_session() {
    signal_and_restart(libc::SIGTERM, false, true).await;
}

#[tokio::test]
async fn real_binary_sigkill_recovers_original_identity_and_organization_with_live_session() {
    signal_and_restart(libc::SIGKILL, false, true).await;
}

#[tokio::test]
async fn account_startup_reboot_recovery_reopens_original_organization_and_identity() {
    signal_and_restart(libc::SIGKILL, true, true).await;
}

struct RebootClock([u8; 32]);
impl spaceterm_host::account_service::ServiceClock for RebootClock {
    fn now(&self) -> Result<spaceterm_host::account_service::ServiceTime, ServiceFailure> {
        Ok(spaceterm_host::account_service::ServiceTime {
            boot: self.0,
            millis: 1,
        })
    }
}

#[tokio::test]
async fn real_binary_sigkill_recovers_with_no_live_sessions() {
    signal_and_restart(libc::SIGKILL, false, false).await;
}

async fn signal_and_restart(signal: i32, reboot: bool, live: bool) {
    tokio::time::timeout(Duration::from_secs(45), async {
        let seed = Seed::new().await;
        let mut binary = Binary::start(&seed);
        let transport = binary.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let before = snapshot(&transport, &mut codec, 1).await;
        let connection = reconcile_snapshot(&transport, &mut codec, &before).await;
        let (workspace, pane, grant) = create(&seed, &transport, &mut codec).await;
        let created = snapshot(&transport, &mut codec, 2).await;
        let (_, record) =
            spaceterm_protocol::convert::organization::v2::decode_pane_record(&created.panes[0])
                .unwrap();
        let pid = if live {
            start_shell(
                &seed,
                &transport,
                &mut codec,
                ServerEpoch::new(created.epoch),
                record.session_generation,
                connection,
                grant,
            )
            .await;
            Some(wait_pid(&seed).await)
        } else {
            None
        };
        transport.close().await.unwrap();
        // This is an OS signal, not a native Stop or a synthetic retirement.
        assert_eq!(unsafe { libc::kill(binary.0.id() as i32, signal) }, 0);
        let status = binary.wait().await;
        let mut diagnostic = String::new();
        std::io::Read::read_to_string(binary.0.stderr.as_mut().unwrap(), &mut diagnostic).unwrap();
        assert_eq!(
            status.success(),
            signal == libc::SIGTERM,
            "{status}: {diagnostic}"
        );
        if let Some(pid) = pid {
            assert!(
                NativeServiceProcessTable::current()
                    .unwrap()
                    .observe(pid)
                    .unwrap()
                    .is_none()
            );
        }
        let supervisor = test_support::retained_supervisor(seed.root.path()).unwrap();
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
        if live {
            fs::remove_file(seed.root.path().join("shell-pid")).unwrap();
        }
        if reboot {
            let signed: serde_json::Value = serde_json::from_slice(
                &fs::read(seed.registration.data_directory().join("runtime-custody")).unwrap(),
            )
            .unwrap();
            let bytes: Vec<u8> = serde_json::from_value(signed["body"].clone()).unwrap();
            let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let mut boot: [u8; 32] =
                serde_json::from_value(state["owner"]["boot"].clone()).unwrap();
            boot[0] ^= 1;
            let supervisor = test_support::retained_supervisor_with_clock(
                seed.root.path(),
                Some(Arc::new(RebootClock(boot))),
            )
            .unwrap();
            let startup = AccountStartup::admit(&supervisor).unwrap();
            let server =
                RecoveredAccountServer::recover(supervisor.clone(), startup, seed.launch())
                    .await
                    .unwrap();
            let restored = server.handle().snapshot();
            assert!(restored.epoch().get() > created.epoch);
            assert!(
                restored
                    .state()
                    .workspaces
                    .iter()
                    .any(|value| value.id == workspace)
            );
            assert!(restored.state().panes.iter().any(|value| value.id == pane));
            assert_eq!(
                supervisor.discover().unwrap().unwrap().server_pin(),
                seed.identity.pin()
            );
            assert!(!seed.root.path().join("shell-pid").exists());
            let report = server.run_until(async {}).await.unwrap();
            assert!(
                report.cleanup_confirmed
                    && report.started_writes_finished
                    && report.listeners_closed
            );
            assert_eq!(
                supervisor.discover().unwrap().unwrap().desired_state(),
                DesiredServerState::Running
            );
            return;
        }
        let mut restarted = Binary::start(&seed);
        let transport = restarted.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let restored = snapshot(&transport, &mut codec, 1).await;
        assert!(restored.epoch > created.epoch);
        assert_eq!(
            spaceterm_protocol::convert::identity::decode_workspace(
                restored.workspaces[0].id.as_ref().unwrap()
            )
            .unwrap(),
            workspace
        );
        assert_eq!(
            spaceterm_protocol::convert::identity::decode_pane(
                restored.panes[0].id.as_ref().unwrap()
            )
            .unwrap(),
            pane
        );
        assert!(!seed.root.path().join("shell-pid").exists());
        transport.close().await.unwrap();
        stop(&seed, ServerEpoch::new(restored.epoch), Vec::new(), None).await;
        assert!(restarted.wait().await.success());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_binary_retains_only_explicit_remote_scope_across_stop_start_and_disable() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let seed = Seed::new().await;
        let server_id = ServerId::new(seed.identity.pin().identity);
        let mut binary = Binary::start(&seed);
        let transport = binary.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let before = snapshot(&transport, &mut codec, 1).await;
        reconcile_snapshot(&transport, &mut codec, &before).await;
        let initial = query_status(&transport, &mut codec).await;
        assert!(!initial.parts().remote_access.value.is_enabled());
        assert!(initial.parts().listeners.is_empty());
        let reservation = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut endpoints = vec![reservation.local_addr().unwrap()];
        drop(reservation);
        match tokio::net::TcpListener::bind("[::1]:0").await {
            Ok(reservation) => endpoints.push(reservation.local_addr().unwrap()),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EAFNOSUPPORT | libc::EADDRNOTAVAIL)
                ) => {}
            Err(error) => panic!("IPv6 loopback probe failed: {error}"),
        }
        let policy = RemoteAccessPolicy::enabled(server_id, endpoints.clone()).unwrap();
        operation(
            &seed,
            &transport,
            &mut codec,
            1,
            Command::SetRemoteAccess {
                server: server_id,
                expected_version: initial.parts().remote_access.version,
                value: policy.clone(),
            },
        )
        .await;
        let enabled = query_status(&transport, &mut codec).await;
        assert_eq!(enabled.parts().remote_access.value, policy);
        assert_eq!(enabled.parts().listeners.len(), endpoints.len());
        let addresses: Vec<_> = enabled
            .parts()
            .listeners
            .iter()
            .map(|fact| fact.endpoint)
            .collect();
        for address in &addresses {
            assert!(tokio::net::TcpStream::connect(address).await.is_ok());
        }
        transport.close().await.unwrap();
        let live_wss = WssTransport::connect(
            addresses[0],
            "localhost",
            EnrolledServerKey(seed.identity.pin().public_key),
            MessageLimit::envelope(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let mut remote_codec = handshake(
            &live_wss,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        assert_eq!(
            query_status(&live_wss, &mut remote_codec)
                .await
                .parts()
                .remote_access,
            enabled.parts().remote_access
        );
        stop(&seed, ServerEpoch::new(before.epoch), Vec::new(), None).await;
        assert!(binary.wait().await.success());
        assert_eq!(live_wss.recv().await.unwrap(), None);
        for address in &addresses {
            assert!(tokio::net::TcpStream::connect(address).await.is_err());
        }
        let mut deliberately_stopped = Binary::start(&seed);
        assert!(!deliberately_stopped.wait().await.success());
        assert!(!seed.registration.endpoint().unwrap().path().exists());
        let supervisor = seed.supervisor(ObservedProcesses::new(
            ServerEpoch::new(before.epoch),
            Vec::new(),
            None,
        ));
        supervisor
            .start(supervisor.allocate_request().unwrap())
            .await
            .unwrap();
        let mut restarted = Binary::start(&seed);
        let transport = restarted.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let restored = snapshot(&transport, &mut codec, 2).await;
        reconcile_snapshot(&transport, &mut codec, &restored).await;
        let status = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = query_status(&transport, &mut codec).await;
                assert!(
                    status.parts().listeners.iter().all(|fact| matches!(
                        fact.state,
                        ListenerState::Binding | ListenerState::Listening
                    )),
                    "restored listener failure: {:?}",
                    status.parts().listeners
                );
                // Binding already publishes every retained endpoint. The count
                // alone does not mean the restarted listeners are ready.
                if status.parts().listeners.len() == endpoints.len()
                    && status
                        .parts()
                        .listeners
                        .iter()
                        .all(|fact| fact.state == ListenerState::Listening)
                {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(status.parts().remote_access, enabled.parts().remote_access);
        assert!(status.parts().epoch > enabled.parts().epoch);
        assert_eq!(status.parts().listeners.len(), endpoints.len());
        assert!(
            status
                .parts()
                .listeners
                .iter()
                .all(|fact| fact.state == ListenerState::Listening),
            "restored listener states: {:?}",
            status.parts().listeners
        );
        operation(
            &seed,
            &transport,
            &mut codec,
            2,
            Command::SetRemoteAccess {
                server: server_id,
                expected_version: status.parts().remote_access.version,
                value: RemoteAccessPolicy::disabled(server_id),
            },
        )
        .await;
        let disabled = query_status(&transport, &mut codec).await;
        assert!(!disabled.parts().remote_access.value.is_enabled());
        assert!(disabled.parts().listeners.is_empty());
        for listener in &status.parts().listeners {
            assert!(
                tokio::net::TcpStream::connect(listener.endpoint)
                    .await
                    .is_err()
            );
        }
        transport.close().await.unwrap();
        stop(&seed, ServerEpoch::new(restored.epoch), Vec::new(), None).await;
        assert!(restarted.wait().await.success());
        supervisor
            .start(supervisor.allocate_request().unwrap())
            .await
            .unwrap();
        let mut disabled_restart = Binary::start(&seed);
        let transport = disabled_restart.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let final_status = query_status(&transport, &mut codec).await;
        assert_eq!(
            final_status.parts().remote_access,
            disabled.parts().remote_access
        );
        assert!(final_status.parts().listeners.is_empty());
        transport.close().await.unwrap();
        stop(&seed, final_status.parts().epoch, Vec::new(), None).await;
        assert!(disabled_restart.wait().await.success());
    })
    .await
    .unwrap();
}

#[path = "listener_fixes.rs"]
mod listener_fixes;

struct RetiredInstanceObservation {
    pid: u32,
    birth: u64,
    descendant_live: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
impl spaceterm_host::account_service::InstanceProcessObserver for RetiredInstanceObservation {
    fn verify_absent(&self, pid: u32, birth: u64, _: &[[u64; 2]]) -> Result<(), ServiceFailure> {
        assert_eq!((pid, birth), (self.pid, self.birth));
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .descendant_live
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            Err(ServiceFailure::CleanupUnresolved)
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn killed_account_server_restarts_after_native_observation_confirms_instance_absent() {
    injected_crash_recovery(false).await;
}

#[tokio::test]
async fn killed_account_server_refuses_recovery_while_a_descendant_is_observed_alive() {
    injected_crash_recovery(true).await;
}

async fn injected_crash_recovery(descendant_live: bool) {
    tokio::time::timeout(Duration::from_secs(90), async {
        let seed = Seed::new().await;
        let mut binary = Binary::start(&seed);
        let transport = binary
            .connect_with_deadline(&seed, Duration::from_secs(30))
            .await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let before = snapshot(&transport, &mut codec, 1).await;
        transport.close().await.unwrap();
        let stale_endpoint = seed.registration.endpoint().unwrap();
        assert_eq!(
            unsafe { libc::kill(binary.0.id() as i32, libc::SIGKILL) },
            0
        );
        assert!(!binary.wait().await.success());
        let record_path = seed.registration.data_directory().join("runtime-custody");
        let original = fs::read(&record_path).unwrap();
        let signed: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let body: Vec<u8> = serde_json::from_value(signed["body"].clone()).unwrap();
        let state: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(state["clean"], false);
        assert!(stale_endpoint.path().exists());
        let observer = Arc::new(RetiredInstanceObservation {
            pid: binary.0.id(),
            birth: state["owner"]["birth"].as_u64().unwrap(),
            descendant_live: std::sync::atomic::AtomicBool::new(descendant_live),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let supervisor =
            test_support::retained_supervisor_with_observer(seed.root.path(), observer.clone())
                .unwrap();
        if descendant_live {
            let startup = AccountStartup::admit(&supervisor).unwrap();
            let result =
                RecoveredAccountServer::recover(supervisor.clone(), startup, seed.launch()).await;
            assert!(matches!(
                result,
                Err(spaceterm_server::StartupFailure::Runtime(
                    spaceterm_server::contracts::ServerFailure {
                        class: spaceterm_server::contracts::ServerFailureClass::CleanupIncomplete,
                        ..
                    }
                ))
            ));
            assert_eq!(fs::read(&record_path).unwrap(), original);
            observer
                .descendant_live
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        let startup = AccountStartup::admit(&supervisor).unwrap();
        let server = RecoveredAccountServer::recover(supervisor.clone(), startup, seed.launch())
            .await
            .unwrap();
        assert!(server.handle().snapshot().epoch().get() > before.epoch);
        assert_eq!(
            supervisor.discover().unwrap().unwrap().server_pin(),
            seed.identity.pin()
        );
        let report = server.run_until(async {}).await.unwrap();
        assert!(
            report.cleanup_confirmed
                && report.listeners_closed
                && report.started_writes_finished
                && !report.recovery_required
        );
        assert!(observer.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        assert!(!seed.registration.endpoint().unwrap().path().exists());
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
    })
    .await
    .unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_crash_recovery_does_not_cleanup_a_checkpointed_unrelated_orphan() {
    use spaceterm_host::pty::macos_pty_host::MacosPtyHost;
    use std::io::BufRead as _;
    tokio::time::timeout(Duration::from_secs(45), async {
        let seed = Seed::new().await;
        let mut binary = Binary::start(&seed);
        let transport = binary.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let before = snapshot(&transport, &mut codec, 1).await;
        transport.close().await.unwrap();
        let mut unrelated = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 12 & echo $!; wait"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(unrelated.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let orphan: u32 = line.trim().parse().unwrap();
        let (parent_birth, original_parent) = MacosPtyHost
            .process_lineage(unrelated.id() as i32)
            .unwrap()
            .unwrap();
        let (orphan_birth, orphan_parent) = MacosPtyHost
            .process_lineage(orphan as i32)
            .unwrap()
            .unwrap();
        assert_eq!(orphan_parent, parent_birth);
        let ledger_path = seed
            .registration
            .data_directory()
            .join("runtime-history/lineage");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = fs::read(&ledger_path) {
                    let signed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    let body = signed["body"].as_str().unwrap();
                    let ledger: serde_json::Value = serde_json::from_str(body).unwrap();
                    let ancestors: Vec<[u64; 2]> =
                        serde_json::from_value(ledger["ancestors"].clone()).unwrap();
                    if ancestors.contains(&[parent_birth, original_parent])
                        && ancestors.contains(&[orphan_birth, parent_birth])
                    {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        assert!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(orphan)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            unsafe { libc::kill(binary.0.id() as i32, libc::SIGKILL) },
            0
        );
        assert!(!binary.wait().await.success());
        let mut restarted = Binary::start(&seed);
        let transport = restarted.connect(&seed).await;
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let restored = snapshot(&transport, &mut codec, 1).await;
        assert!(restored.epoch > before.epoch);
        assert!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(orphan)
                .unwrap()
                .is_some(),
            "server recovery must not kill unrelated processes"
        );
        transport.close().await.unwrap();
        stop(&seed, ServerEpoch::new(restored.epoch), Vec::new(), None).await;
        assert!(restarted.wait().await.success());
        // The unrelated fixture exits on its own. The server never gains signal authority over it.
        tokio::time::timeout(Duration::from_secs(15), async {
            while MacosPtyHost
                .process_lineage(orphan as i32)
                .unwrap()
                .is_some_and(|(birth, _)| birth == orphan_birth)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    })
    .await
    .unwrap();
}

fn native_authenticated_session(
    fixture: &Fixture,
    principal: authentication::Principal,
    key: Arc<DeviceKey>,
    major: u32,
    generation: u64,
) -> spaceterm_server::api::AuthenticatedSession {
    use spaceterm_protocol::negotiation::{
        CAPABILITY_ACCOUNT_RUNTIME, CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
    };
    native_authenticated_codecs(
        fixture,
        principal,
        key,
        major,
        generation,
        vec![
            CAPABILITY_ACCOUNT_RUNTIME,
            CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
        ],
        vec![
            CAPABILITY_ACCOUNT_RUNTIME,
            CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
        ],
    )
    .0
}

fn native_authenticated_codecs(
    fixture: &Fixture,
    principal: authentication::Principal,
    key: Arc<DeviceKey>,
    major: u32,
    generation: u64,
    client_capabilities: Vec<u32>,
    server_capabilities: Vec<u32>,
) -> (
    spaceterm_server::api::AuthenticatedSession,
    spaceterm_protocol::connection::AuthenticatedConnection,
) {
    let lookup = PairingProtocolLookup::new(fixture.server.handle());
    let client_config = authentication::HandshakeConfig {
        majors: vec![major],
        capabilities: client_capabilities,
        ..Default::default()
    };
    let server_config = authentication::HandshakeConfig {
        majors: vec![major],
        capabilities: server_capabilities,
        ..Default::default()
    };
    let (client, hello) = authentication::ClientHandshake::begin(
        fixture.identity.pin(),
        principal,
        authentication::Purpose::Ordinary,
        client_config,
        key,
    )
    .unwrap();
    let (server, reply) = authentication::ServerHandshake::accept(
        principal.server,
        fixture.server.handle().snapshot().epoch().get(),
        server_config,
        fixture.identity.clone(),
        &lookup,
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (established, ready) = server.receive_device(&lookup, &proof).unwrap();
    let authentication::Established::Ordinary(client_codec) = client.receive_ready(&ready).unwrap()
    else {
        panic!("ordinary native runtime client");
    };
    let authentication::Established::Ordinary(codec) = established else {
        panic!("ordinary native runtime principal");
    };
    (
        spaceterm_server::api::AuthenticatedSession::new(
            codec,
            ConnectionGeneration::new(generation),
        )
        .unwrap(),
        client_codec,
    )
}

fn native_lifecycle_payload(
    major: u32,
    call: spaceterm_host::account_service::runtime_inventory::AccountRuntimeCall,
) -> Payload {
    let message = spaceterm_protocol::wire::common::AccountRuntimeMessage {
        request: 31,
        request_generation: 7,
        body: serde_json::to_vec(&call).unwrap(),
    };
    if major == 1 {
        Payload::Major1(spaceterm_protocol::wire::v1::Message {
            kind: Some(spaceterm_protocol::wire::v1::message::Kind::AccountRuntimeRequest(message)),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeRequest(message)),
        })
    }
}

async fn native_lifecycle_call(
    runtime: &Arc<spaceterm_server::account_runtime::AccountRuntimeProcesses>,
    major: u32,
    uid: u32,
    session: &spaceterm_server::api::AuthenticatedSession,
    call: spaceterm_host::account_service::runtime_inventory::AccountRuntimeCall,
) -> spaceterm_host::account_service::runtime_inventory::AccountRuntimeReply {
    let response = runtime
        .dispatch(&native_lifecycle_payload(major, call), Some(uid), session)
        .await
        .unwrap()
        .unwrap();
    let message = match response {
        Payload::Major1(spaceterm_protocol::wire::v1::Message {
            kind: Some(spaceterm_protocol::wire::v1::message::Kind::AccountRuntimeReply(message)),
        }) => message,
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
        }) => message,
        _ => panic!("native lifecycle reply"),
    };
    assert_eq!(message.request, 31);
    assert_eq!(message.request_generation, 7);
    serde_json::from_slice(&message.body).unwrap()
}

#[tokio::test]
async fn paired_native_dispatch_requires_actual_private_guest_uid_in_both_protocol_majors() {
    use spaceterm_host::account_service::runtime_inventory::{
        AccountRuntimeCall, AccountRuntimeReply,
    };
    for major in [1, 2] {
        let fixture = Fixture::new_registered(true).await;
        let (_, comparison, key) = fixture.enroll(major).await;
        let session = native_authenticated_session(&fixture, comparison, key, major, 1);
        let principal = session.principal;
        let handle = fixture.server.handle();
        handle.register(principal).await.unwrap();
        let supervisor = test_support::supervisor(
            fixture._root.path(),
            ObservedProcesses::new(handle.snapshot().epoch(), Vec::new(), None),
        )
        .unwrap();
        let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
            handle.clone(),
            supervisor.as_ref().clone(),
            fixture.identity.clone(),
        )
        .unwrap();
        let request = supervisor.allocate_request().unwrap();
        let capture = supervisor
            .capture_runtime_request(request, ServiceAction::Stop)
            .unwrap();
        let payload =
            native_lifecycle_payload(major, AccountRuntimeCall::AssessNativeLifecycle(capture));
        assert_eq!(
            runtime
                .dispatch(&payload, None, &session)
                .await
                .unwrap_err()
                .class,
            spaceterm_server::contracts::ServerFailureClass::Authentication
        );
        assert_eq!(
            runtime
                .dispatch(&payload, Some(supervisor.key().account_uid + 1), &session)
                .await
                .unwrap_err()
                .class,
            spaceterm_server::contracts::ServerFailureClass::Authentication
        );
        let response = native_lifecycle_call(
            &runtime,
            major,
            supervisor.key().account_uid,
            &session,
            AccountRuntimeCall::AssessNativeLifecycle(capture),
        )
        .await;
        assert!(matches!(
            response,
            AccountRuntimeReply::NativeLifecycleAssessment(_)
        ));
        assert_eq!(
            supervisor.request_progress(request).unwrap().phase,
            spaceterm_host::account_service::ServicePhase::Reserved
        );
        handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        fixture.server.wait().await.unwrap();
    }
}

#[tokio::test]
async fn paired_native_dispatch_does_not_admit_a_disconnected_or_different_authenticated_caller() {
    use spaceterm_host::account_service::runtime_inventory::{
        AccountRuntimeCall, AccountRuntimeReply,
    };
    let fixture = Fixture::new_registered(true).await;
    let (_, comparison, key) = fixture.enroll(2).await;
    let (_, other_comparison, other_key) = fixture.enroll_with_seed(2, 81).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let other_session = native_authenticated_session(&fixture, other_comparison, other_key, 2, 1);
    let other = other_session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    handle.register(other).await.unwrap();
    let supervisor = test_support::supervisor(
        fixture._root.path(),
        ObservedProcesses::new(handle.snapshot().epoch(), Vec::new(), None),
    )
    .unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle.clone(),
        supervisor.as_ref().clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let AccountRuntimeReply::NativeLifecycleAssessment(reference) = native_lifecycle_call(
        &runtime,
        2,
        supervisor.key().account_uid,
        &session,
        AccountRuntimeCall::AssessNativeLifecycle(capture),
    )
    .await
    else {
        panic!("native assessment");
    };
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            supervisor.key().account_uid,
            &other_session,
            AccountRuntimeCall::AdmitNativeLifecycle(reference.clone())
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::PairingRequired)
    );
    handle.disconnect(principal).await.unwrap();
    assert!(matches!(
        native_lifecycle_call(
            &runtime,
            2,
            supervisor.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeLifecycle(reference)
        )
        .await,
        AccountRuntimeReply::Failed(_)
    ));
    assert_eq!(
        supervisor.resolve(request).await.unwrap().phase,
        spaceterm_host::account_service::ServicePhase::Reserved
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    fixture.server.wait().await.unwrap();
}

#[tokio::test]
async fn native_retirement_requires_full_original_assessment_not_only_server_and_epoch() {
    use spaceterm_host::account_service::{
        ServicePhase as NativePhase,
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply},
    };
    let fixture = Fixture::new_registered(true).await;
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let epoch = handle.snapshot().epoch();
    let supervisor = test_support::supervisor(
        fixture._root.path(),
        ObservedProcesses::new(epoch, Vec::new(), None),
    )
    .unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle.clone(),
        supervisor.as_ref().clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let AccountRuntimeReply::NativeLifecycleAssessment(reference) = native_lifecycle_call(
        &runtime,
        2,
        supervisor.key().account_uid,
        &session,
        AccountRuntimeCall::AssessNativeLifecycle(capture),
    )
    .await
    else {
        panic!("native assessment");
    };
    // This second scope really was issued by the actor for the same Server,
    // epoch, generation and Sessions. Its token still belongs to another scope.
    let replacement = handle.assess_account_runtime().await.unwrap();
    let AccountRuntimeReply::NativeLifecycleAccepted(accepted) = native_lifecycle_call(
        &runtime,
        2,
        supervisor.key().account_uid,
        &session,
        AccountRuntimeCall::AdmitNativeLifecycle(reference),
    )
    .await
    else {
        panic!("native acceptance");
    };
    spaceterm_host::account_service::test_support::interrupt_after_phase(
        NativePhase::IntentCommitted,
    );
    assert_eq!(
        supervisor.resolve(request).await,
        Err(ServiceFailure::SupervisorUnavailable)
    );
    let registration = supervisor.discover().unwrap().unwrap();
    let original = supervisor
        .retirement_runtime_assessment(handle.snapshot().state().server, epoch)
        .unwrap();
    assert_eq!(replacement.server, original.server);
    assert_eq!(replacement.epoch, original.epoch);
    assert_eq!(replacement.generation, original.generation);
    assert_eq!(replacement.sessions, original.sessions);
    assert_ne!(replacement.token, original.token);
    let comparison = NativeManagementRequestRef::new(
        original.server,
        *request.as_bytes(),
        accepted.request_digest,
    )
    .unwrap();
    assert_eq!(
        runtime
            .quiesce(
                &registration,
                comparison,
                NativeRetirementAction::Stop,
                &replacement
            )
            .await,
        Err(ServiceFailure::RequestConflict)
    );
    let mut changed_generation = original.clone();
    changed_generation.generation += 1;
    assert_eq!(
        runtime
            .quiesce(
                &registration,
                comparison,
                NativeRetirementAction::Stop,
                &changed_generation
            )
            .await,
        Err(ServiceFailure::RequestConflict)
    );
    let mut changed_sessions = original.clone();
    changed_sessions.sessions.push(SessionKey {
        epoch,
        pane: PaneId::new(original.server, 1).unwrap(),
        generation: SessionGeneration::new(1),
    });
    assert_eq!(
        runtime
            .quiesce(
                &registration,
                comparison,
                NativeRetirementAction::Stop,
                &changed_sessions
            )
            .await,
        Err(ServiceFailure::RequestConflict)
    );
    runtime.start_original_retirement().await.unwrap();
    runtime.finish_retirements().await.unwrap();
    fixture.server.wait().await.unwrap();
    assert_eq!(
        supervisor.resolve(request).await.unwrap().state,
        spaceterm_host::account_service::ServiceRequestState::Completed(
            spaceterm_host::account_service::ServiceOutcome::Stopped
        )
    );
}

#[tokio::test]
async fn native_non_admission_monitor_retires_only_original_actor_barrier_and_keeps_reserved_origin_unaccepted()
 {
    use spaceterm_host::account_service::{
        ServicePhase as NativePhase,
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RetainedRuntimeProcesses},
    };
    let fixture = Fixture::new_registered(true).await;
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let supervisor =
        test_support::supervisor(fixture._root.path(), Arc::new(RetainedRuntimeProcesses)).unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle.clone(),
        supervisor.as_ref().clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let AccountRuntimeReply::NativeLifecycleAssessment(reference) = native_lifecycle_call(
        &runtime,
        2,
        supervisor.key().account_uid,
        &session,
        AccountRuntimeCall::AssessNativeLifecycle(capture),
    )
    .await
    else {
        panic!("native assessment");
    };
    // Interrupt before Accepted. The durable reserving record is exactly the
    // crash state that the original native owner, not a new caller, must resolve.
    spaceterm_host::account_service::test_support::interrupt_after_phase(NativePhase::Reserved);
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            supervisor.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeLifecycle(reference.clone())
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::SupervisorUnavailable)
    );
    assert_eq!(
        supervisor.resolve(request).await,
        Err(ServiceFailure::Pending)
    );
    let server = handle.snapshot().state().server;
    let (original, scope) = supervisor
        .runtime_origin_abort_request(server, handle.snapshot().epoch())
        .unwrap()
        .unwrap();
    // The uncontrolled runtime boundary models a crash after actor reservation
    // but before its journal publication. It does not perform a native effect.
    handle.reserve_account_runtime(scope.clone()).await.unwrap();
    assert!(handle.assess_account_runtime().await.is_err());
    runtime.resolve_original_non_admission().await.unwrap();
    let registration = supervisor.discover().unwrap().unwrap();
    assert_eq!(
        RetainedRuntimeProcesses
            .abort_native_origin(&registration, original, &scope)
            .await,
        Ok(())
    );
    assert_eq!(
        supervisor.resolve(request).await.unwrap().phase,
        NativePhase::Reserved
    );
    assert_eq!(
        handle
            .reserve_account_runtime(scope)
            .await
            .unwrap_err()
            .class,
        spaceterm_server::contracts::ServerFailureClass::StaleAuthority
    );
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            supervisor.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeLifecycle(reference)
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        supervisor.request_progress(request).unwrap().phase,
        NativePhase::Reserved
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    let fresh = handle.assess_account_runtime().await.unwrap();
    runtime.resolve_original_non_admission().await.unwrap();
    handle.reserve_account_runtime(fresh.clone()).await.unwrap();
    handle.abort_account_runtime(fresh).await.unwrap();
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    fixture.server.wait().await.unwrap();
}

#[tokio::test]
async fn production_monitor_completes_original_non_admission_without_a_guest_client_key_or_native_effect()
 {
    use spaceterm_host::account_service::{
        ServicePhase as NativePhase,
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RetainedRuntimeProcesses},
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        let seed = Seed::new().await;
        let startup_supervisor = seed.supervisor(Arc::new(test_support::StartupAbsence));
        let startup = AccountStartup::admit(&startup_supervisor).unwrap();
        let server = RecoveredAccountServer::recover(startup_supervisor, startup, seed.launch())
            .await
            .unwrap();
        let handle = server.handle();
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let stop = shutdown.clone();
        let run = tokio::spawn(server.run_until(async move { stop.notified().await }));
        while handle.phase() != ServicePhase::Serving {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let transport = IpcTransport::connect(
            &seed.registration.endpoint().unwrap(),
            MessageLimit::envelope(),
        )
        .await
        .unwrap();
        let mut codec = handshake(
            &transport,
            seed.identity.pin(),
            seed.principal,
            seed.key.clone(),
            2,
        )
        .await;
        let supervisor = seed.supervisor(Arc::new(RetainedRuntimeProcesses));
        let request = supervisor.allocate_request().unwrap();
        let capture = supervisor
            .capture_runtime_request(request, ServiceAction::Stop)
            .unwrap();
        let reply = round_trip(
            &transport,
            &mut codec,
            native_lifecycle_payload(2, AccountRuntimeCall::AssessNativeLifecycle(capture)),
        )
        .await;
        let Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeReply(reply)),
        }) = reply
        else {
            panic!("native assessment reply");
        };
        let AccountRuntimeReply::NativeLifecycleAssessment(reference) =
            serde_json::from_slice::<AccountRuntimeReply>(&reply.body).unwrap()
        else {
            panic!("native assessment");
        };
        spaceterm_host::account_service::test_support::interrupt_after_phase(NativePhase::Reserved);
        let reply = round_trip(
            &transport,
            &mut codec,
            native_lifecycle_payload(2, AccountRuntimeCall::AdmitNativeLifecycle(reference)),
        )
        .await;
        let Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeReply(reply)),
        }) = reply
        else {
            panic!("native admission reply");
        };
        assert_eq!(
            serde_json::from_slice::<AccountRuntimeReply>(&reply.body).unwrap(),
            AccountRuntimeReply::Failed(ServiceFailure::SupervisorUnavailable)
        );
        transport.close().await.unwrap();
        // From this point only the original native journal and pinned signed
        // custody are available. The guest owner has no Client credential.
        let mut pending = false;
        loop {
            match supervisor.resolve(request).await {
                Ok(progress) => {
                    assert_eq!(progress.phase, NativePhase::Reserved);
                    break;
                }
                Err(ServiceFailure::Pending) => {
                    pending = true;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                other => panic!("original non-admission: {other:?}"),
            }
        }
        assert!(pending);
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
        assert!(!seed.root.path().join("shell-pid").exists());
        assert!(handle.assess_account_runtime().await.is_ok());
        shutdown.notify_one();
        let report = run.await.unwrap().unwrap();
        assert!(
            report.cleanup_confirmed && report.listeners_closed && report.started_writes_finished
        );
    })
    .await
    .unwrap();
}

#[path = "account_native_lifecycle.rs"]
mod native_lifecycle_wire;

#[path = "account_native_stage.rs"]
mod native_stage_wire;

#[tokio::test]
async fn paired_native_receiver_requires_both_negotiated_capabilities_in_both_majors_before_dispatch()
 {
    use spaceterm_host::account_service::{
        NativeLifecycleAssessmentRef, NativeStageAssessmentRef, NativeStageRequest, ServiceTime,
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply},
    };
    use spaceterm_protocol::negotiation::{
        CAPABILITY_ACCOUNT_RUNTIME as ACCOUNT, CAPABILITY_REMOTE_NATIVE_LIFECYCLE as NATIVE,
    };
    use spaceterm_server::contracts::ServerFailureClass;
    for major in [1, 2] {
        let fixture = Fixture::new_registered(true).await;
        let (_, comparison, key) = fixture.enroll(major).await;
        let handle = fixture.server.handle();
        let native_account =
            spaceterm_host::account_service::test_support::AccountServiceFixture::new(
                fixture._root.path(),
                BuildIdentity::Development,
            )
            .unwrap();
        let supervisor = test_support::supervisor(
            fixture._root.path(),
            ObservedProcesses::new(handle.snapshot().epoch(), Vec::new(), None),
        )
        .unwrap();
        let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
            handle.clone(),
            supervisor.as_ref().clone(),
            fixture.identity.clone(),
        )
        .unwrap();
        let request = supervisor.allocate_request().unwrap();
        let capture = supervisor
            .capture_runtime_request(request, ServiceAction::Stop)
            .unwrap();
        let reference = NativeLifecycleAssessmentRef {
            original: capture,
            request_digest: RequestDigest::from_bytes([30; 32]),
            proof_digest: RequestDigest::from_bytes([31; 32]),
            epoch: handle.snapshot().epoch(),
            affected_sessions: 0,
            expires_at: ServiceTime {
                boot: [1; 32],
                millis: 30_100,
            },
        };
        let stage = NativeStageRequest {
            request,
            artifact_sha256: [32; 32],
            capture_digest: RequestDigest::from_bytes([33; 32]),
        };
        let stage_reference = NativeStageAssessmentRef {
            original: stage,
            request_digest: RequestDigest::from_bytes([34; 32]),
            proof_digest: RequestDigest::from_bytes([35; 32]),
            epoch: handle.snapshot().epoch(),
            expires_at: reference.expires_at,
        };
        for (index, (client_caps, server_caps)) in [
            (vec![ACCOUNT], vec![ACCOUNT, NATIVE]),
            (vec![ACCOUNT, NATIVE], vec![ACCOUNT]),
            (vec![NATIVE], vec![ACCOUNT, NATIVE]),
            (vec![ACCOUNT, NATIVE], vec![NATIVE]),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut session, mut client_codec) = native_authenticated_codecs(
                &fixture,
                comparison,
                key.clone(),
                major,
                index as u64 + 1,
                client_caps,
                server_caps,
            );
            handle.register(session.principal).await.unwrap();
            let before = fs::read(native_account.anchor().join("requests")).unwrap();
            let before_revision = handle.snapshot().state().revision;
            for call in [
                AccountRuntimeCall::AssessNativeLifecycle(capture),
                AccountRuntimeCall::AdmitNativeLifecycle(reference.clone()),
                AccountRuntimeCall::AssessNativeStage(stage),
                AccountRuntimeCall::AdmitNativeStage(stage_reference.clone()),
            ] {
                let payload = native_lifecycle_payload(major, call);
                let received = if session.codec.capabilities().contains(&ACCOUNT) {
                    // The old 507 envelope grammar admits these opaque bodies.
                    // The native receiver must independently require negotiated 514.
                    let frame = client_codec.encode(&payload).unwrap();
                    session.codec.decode(&frame).unwrap()
                } else {
                    assert!(client_codec.encode(&payload).is_err());
                    // Exercise the receiver's own gate even when the outer codec
                    // would already refuse an unnegotiated 507 envelope.
                    payload
                };
                assert_eq!(
                    runtime
                        .dispatch(&received, Some(supervisor.key().account_uid), &session)
                        .await
                        .unwrap_err()
                        .class,
                    ServerFailureClass::ScopeMismatch
                );
                assert_eq!(
                    fs::read(native_account.anchor().join("requests")).unwrap(),
                    before
                );
                assert_eq!(handle.snapshot().state().revision, before_revision);
                assert_eq!(
                    supervisor.restore_runtime_request(request).unwrap(),
                    capture
                );
                assert_eq!(
                    supervisor.restore_runtime_lifecycle_reference(capture),
                    Err(ServiceFailure::RequestConflict)
                );
            }
        }
        let (mut session, mut client_codec) = native_authenticated_codecs(
            &fixture,
            comparison,
            key,
            major,
            5,
            vec![ACCOUNT, NATIVE],
            vec![ACCOUNT, NATIVE],
        );
        handle.register(session.principal).await.unwrap();
        let payload =
            native_lifecycle_payload(major, AccountRuntimeCall::AssessNativeLifecycle(capture));
        let received = session
            .codec
            .decode(&client_codec.encode(&payload).unwrap())
            .unwrap();
        let response = runtime
            .dispatch(&received, Some(supervisor.key().account_uid), &session)
            .await
            .unwrap()
            .unwrap();
        let reply = match response {
            Payload::Major1(spaceterm_protocol::wire::v1::Message {
                kind: Some(spaceterm_protocol::wire::v1::message::Kind::AccountRuntimeReply(reply)),
            })
            | Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(reply)),
            }) => reply,
            _ => panic!("exact native assessment reply"),
        };
        assert_eq!(reply.request, 31);
        assert_eq!(reply.request_generation, 7);
        assert!(matches!(
            serde_json::from_slice::<AccountRuntimeReply>(&reply.body).unwrap(),
            AccountRuntimeReply::NativeLifecycleAssessment(_)
        ));
        assert_eq!(
            supervisor.request_progress(request).unwrap().phase,
            spaceterm_host::account_service::ServicePhase::Reserved
        );
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
        handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        fixture.server.wait().await.unwrap();
    }
}
