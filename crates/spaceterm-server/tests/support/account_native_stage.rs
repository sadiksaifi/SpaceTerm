//! Public running Stage tests over the real paired actor, native journal,
//! signed installation store and, for preservation, production IPC and PTY.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use spaceterm_distribution::{
    AccountServerFeedV2, BuildChannel, ServerArchiveVerifier, UpdateKey,
    VerifiedAccountServerArtifact, verify_account_server_feed,
};
use spaceterm_host::account_service::{
    LingerState, NativeStageAccountReply as Reply, NativeStageAccountRequest as Request,
    NativeStageAssessmentRef, NativeStageHandoff as Handoff, NativeStageRequest, ServiceClock,
    ServiceManager, ServiceOutcome, ServicePhase as NativePhase, ServiceProgress,
    ServiceRequestState, ServiceSupervisor, ServiceTime, SupervisorState,
    runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RetainedRuntimeProcesses},
    test_support::AccountServiceFixture,
};
use spaceterm_server::account_native_stage::{NativeStageOriginalOwners, handle as stage};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[path = "account_artifact.rs"]
mod package;

struct Publisher {
    key: UpdateKey,
    signer: Ed25519KeyPair,
}
impl Publisher {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let signer = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key = UpdateKey::from_base64(&STANDARD.encode(signer.public_key().as_ref())).unwrap();
        Self { key, signer }
    }
    fn artifact(&self, version: &str) -> VerifiedAccountServerArtifact {
        // Reuse the bounded native layout, then sign the package with this test's
        // newly generated publisher key. No host artifact or credential is read.
        let (_, original) = package::fixture(version, 91, package::native_machine());
        let mut feed: AccountServerFeedV2 =
            serde_json::from_slice(original.feed().bytes()).unwrap();
        feed.archive.signature = self
            .signer
            .sign(original.bytes())
            .as_ref()
            .try_into()
            .unwrap();
        let bytes = serde_json::to_vec(&feed).unwrap();
        let signature = STANDARD.encode(self.signer.sign(&bytes).as_ref());
        let feed = verify_account_server_feed(
            &self.key,
            BuildChannel::Development,
            feed.platform,
            &bytes,
            signature.as_bytes(),
            1,
        )
        .unwrap();
        ServerArchiveVerifier::new()
            .verify_account_owned(&self.key, feed, original.bytes().to_vec())
            .unwrap()
    }
    fn provision(&self, registration: &RegisteredServer) {
        let store = spaceterm_host::server_installation::test_support::store(
            registration,
            self.key.clone(),
        )
        .unwrap();
        let original = self.artifact("1.2.3");
        let approved = store.stage(&original).unwrap();
        assert_eq!(
            Some(approved),
            registration
                .approved_installation()
                .map(|status| status.installation)
        );
        store
            .activate(
                approved,
                store.snapshot().unwrap().generation,
                spaceterm_host::organization_store::schema::SCHEMA_VERSION,
            )
            .unwrap();
    }
}
#[derive(Default)]
struct Manager {
    effects: AtomicUsize,
}
impl Manager {
    fn unexpected(&self) -> Result<(), ServiceFailure> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Err(ServiceFailure::SupervisorUnavailable)
    }
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async { Ok(SupervisorState::Running) })
    }
    fn scoped_processes<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, Vec<u32>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(Some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { self.unexpected() })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { self.unexpected() })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { self.unexpected() })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { self.unexpected() })
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Ok(())
    }
}
struct Clock(AtomicU64);
impl Clock {
    fn new() -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(10)))
    }
}
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: self.0.load(Ordering::SeqCst),
        })
    }
}
fn stage_supervisor(
    root: &std::path::Path,
    publisher: &Publisher,
    manager: &Arc<Manager>,
    clock: &Arc<Clock>,
) -> ServiceSupervisor {
    let account = AccountServiceFixture::new(root, BuildIdentity::Development).unwrap();
    account.with_installation_trust(
        account.with_providers(
            manager.clone(),
            Arc::new(RetainedRuntimeProcesses),
            clock.clone(),
            None,
        ),
        publisher.key.clone(),
    )
}
fn capture(
    supervisor: &ServiceSupervisor,
    artifact: &VerifiedAccountServerArtifact,
) -> NativeStageRequest {
    let registration = supervisor.discover().unwrap().unwrap();
    let request = supervisor.allocate_request().unwrap();
    let Reply::Captured(original) = stage(
        supervisor,
        &Request::Capture {
            expected_key: registration.key(),
            expected_pin: registration.server_pin(),
            request,
            artifact_sha256: artifact.artifact_sha256(),
        },
        Some(artifact),
    ) else {
        panic!("native Stage capture");
    };
    original
}
fn resolve_request(
    supervisor: &ServiceSupervisor,
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
) -> Request {
    let registration = supervisor.discover().unwrap().unwrap();
    Request::Resolve {
        expected_key: registration.key(),
        expected_pin: registration.server_pin(),
        request,
    }
}
async fn paired_stage(
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    call: AccountRuntimeCall,
) -> AccountRuntimeReply {
    transport
        .send(&codec.encode(&native_lifecycle_payload(2, call)).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let payload = codec
                .decode(&transport.recv().await.unwrap().unwrap())
                .unwrap();
            if let Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(reply)),
            }) = payload
                && reply.request == 31
                && reply.request_generation == 7
            {
                return serde_json::from_slice(&reply.body).unwrap();
            }
        }
    })
    .await
    .unwrap()
}
async fn assessed(
    runtime: &Arc<spaceterm_server::account_runtime::AccountRuntimeProcesses>,
    supervisor: &ServiceSupervisor,
    session: &spaceterm_server::api::AuthenticatedSession,
    major: u32,
    original: NativeStageRequest,
) -> NativeStageAssessmentRef {
    let reply = native_lifecycle_call(
        runtime,
        major,
        supervisor.key().account_uid,
        session,
        AccountRuntimeCall::AssessNativeStage(original),
    )
    .await;
    let AccountRuntimeReply::NativeStageAssessment(reference) = reply else {
        panic!("paired Stage assessment: {reply:?}");
    };
    reference
}
async fn close_fixture(fixture: Fixture) {
    fixture
        .server
        .handle()
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    fixture.server.wait().await.unwrap();
}

#[tokio::test]
async fn running_stage_keeps_live_pty_running_approved_code_and_listeners_without_manager_effects_or_session_barrier()
 {
    tokio::time::timeout(Duration::from_secs(45), async {
        let seed = Seed::new().await;
        let publisher = Publisher::new();
        publisher.provision(&seed.registration);
        let manager = Arc::new(Manager::default());
        let clock = Clock::new();
        let supervisor = Arc::new(stage_supervisor(
            seed.root.path(),
            &publisher,
            &manager,
            &clock,
        ));
        let before = supervisor.discover().unwrap().unwrap();
        let approved = before.approved_installation().cloned();
        let store = spaceterm_host::server_installation::test_support::existing_store(
            &before,
            publisher.key.clone(),
        )
        .unwrap();
        let approved_selection = store.snapshot().unwrap().approved;
        let startup = AccountStartup::admit(&supervisor).unwrap();
        let server = RecoveredAccountServer::recover(supervisor.clone(), startup, seed.launch())
            .await
            .unwrap();
        let handle = server.handle();
        let run = tokio::spawn(server.run_until(std::future::pending()));
        while handle.phase() != ServicePhase::Serving {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let endpoint = seed.registration.endpoint().unwrap();
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
        let generation = handle
            .snapshot()
            .runtime()
            .iter()
            .find(|record| record.pane == pane)
            .unwrap()
            .session_generation;
        let session = start_shell(
            &seed,
            &transport,
            &mut codec,
            handle.snapshot().epoch(),
            generation,
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
        let mut sessions = handle.assess_account_runtime().await.unwrap().sessions;
        assert_eq!(sessions, vec![session]);
        let artifact = publisher.artifact("1.2.4");
        let original = capture(&supervisor, &artifact);
        let AccountRuntimeReply::NativeStageAssessment(reference) = paired_stage(
            &transport,
            &mut codec,
            AccountRuntimeCall::AssessNativeStage(original),
        )
        .await
        else {
            panic!("actual paired Stage proof");
        };
        // SSH can inspect Reserved but cannot promote it or acquire a Session barrier.
        let resolve = resolve_request(&supervisor, original.request);
        let Reply::Progress(reserved) = stage(&supervisor, &resolve, Some(&artifact)) else {
            panic!("signed Reserved Stage");
        };
        assert_eq!(reserved.phase, NativePhase::Reserved);
        assert_eq!(
            supervisor.discover().unwrap().unwrap().generation(),
            before.generation()
        );
        assert_eq!(
            handle.assess_account_runtime().await.unwrap().sessions,
            sessions
        );
        // A new live Session can start after the Stage proof. Stage authority
        // covers code selection, not a frozen or destructive all-Session scope.
        let second =
            start_additional_stage_shell(&seed, &transport, &mut codec, &handle, connection).await;
        sessions = handle.assess_account_runtime().await.unwrap().sessions;
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&session) && sessions.contains(&second));
        let AccountRuntimeReply::NativeStageAccepted(accepted) = paired_stage(
            &transport,
            &mut codec,
            AccountRuntimeCall::AdmitNativeStage(reference),
        )
        .await
        else {
            panic!("actual paired Stage admission");
        };
        assert_eq!(accepted.request, original.request);
        assert_eq!(accepted.phase, NativePhase::Accepted);
        assert_eq!(
            handle.assess_account_runtime().await.unwrap().sessions,
            sessions
        );
        let Reply::Progress(completed) = stage(&supervisor, &resolve, Some(&artifact)) else {
            panic!("original native Stage effect");
        };
        let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) = &completed.state
        else {
            panic!("exact Stage result");
        };
        assert_eq!(staged.artifact_sha256, original.artifact_sha256);
        let current = supervisor.discover().unwrap().unwrap();
        assert_eq!(current.desired_state(), DesiredServerState::Running);
        assert_eq!(current.approved_installation(), approved.as_ref());
        assert_eq!(current.staged_installation(), Some(staged));
        assert_eq!(store.snapshot().unwrap().approved, approved_selection);
        assert_eq!(
            handle.assess_account_runtime().await.unwrap().sessions,
            sessions
        );
        assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
        assert!(endpoint.path().exists());
        assert!(
            NativeServiceProcessTable::current()
                .unwrap()
                .observe(pid)
                .unwrap()
                .is_some()
        );
        // Ordinary shared mutations and actual input remain usable after admission.
        let name = handle
            .snapshot()
            .state()
            .workspaces
            .iter()
            .find(|record| record.id == workspace)
            .unwrap()
            .name
            .version;
        assert!(matches!(
            operation(
                &seed,
                &transport,
                &mut codec,
                7,
                Command::RenameWorkspace {
                    workspace,
                    expected_version: name,
                    draft: NameDraft::Custom(NameText::new("Stage preserved work".into()).unwrap()),
                }
            )
            .await,
            Outcome::Applied
        ));
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
                        BoundedInputMethodText::new(
                            "echo RUNNING-$(printf STAGE)-PRESERVED".into(),
                        )
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
            "RUNNING-STAGE-PRESERVED",
        )
        .await;
        let ack = Request::Acknowledge {
            expected_key: current.key(),
            expected_pin: current.server_pin(),
            progress: completed.clone(),
        };
        assert_eq!(
            stage(&supervisor, &ack, None),
            Reply::Acknowledged {
                progress: completed.clone()
            }
        );
        let recreated = stage_supervisor(seed.root.path(), &publisher, &manager, &clock);
        assert_eq!(
            stage(&recreated, &ack, None),
            Reply::Acknowledged {
                progress: completed
            }
        );
        assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
        transport.close().await.unwrap();
        stop(&seed, session.epoch, sessions, Some(pid)).await;
        run.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stage_original_survives_lost_proof_admission_and_effect_replies_without_replacement_or_expiry_refresh()
 {
    for major in [1, 2] {
        let fixture = Fixture::new_registered(true).await;
        let publisher = Publisher::new();
        publisher.provision(&fixture.registration);
        let manager = Arc::new(Manager::default());
        let clock = Clock::new();
        let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
        let (_, comparison, key) = fixture.enroll(major).await;
        let session = native_authenticated_session(&fixture, comparison, key, major, 1);
        let principal = session.principal;
        let handle = fixture.server.handle();
        handle.register(principal).await.unwrap();
        let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
            handle.clone(),
            native.clone(),
            fixture.identity.clone(),
        )
        .unwrap();
        let artifact = publisher.artifact("1.2.4");
        let original = capture(&native, &artifact);
        let reference = assessed(&runtime, &native, &session, major, original).await;
        let recreated = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
        let restore = Request::Restore {
            expected_key: native.key(),
            expected_pin: fixture.identity.pin(),
            request: original.request,
        };
        assert_eq!(
            stage(&recreated, &restore, None),
            Reply::Restored {
                original,
                assessment: Some(reference.clone()),
                progress: None
            }
        );
        assert_eq!(
            assessed(&runtime, &native, &session, major, original).await,
            reference
        );
        let admitted = native_lifecycle_call(
            &runtime,
            major,
            native.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeStage(reference.clone()),
        )
        .await;
        let AccountRuntimeReply::NativeStageAccepted(accepted) = admitted else {
            panic!("original Stage Accepted");
        };
        assert_eq!(accepted.request, original.request);
        let resolve = resolve_request(&recreated, original.request);
        let Reply::Progress(pending) = stage(&recreated, &resolve, None) else {
            panic!("Accepted input pending");
        };
        assert_eq!(pending, accepted);
        let wrong = publisher.artifact("1.2.5");
        assert_eq!(
            stage(&recreated, &resolve, Some(&wrong)),
            Reply::Refused(ServiceFailure::RequestConflict)
        );
        // Expiry gates admission, not the effect of an already Accepted original.
        clock
            .0
            .store(reference.expires_at.millis + 1, Ordering::SeqCst);
        let original_again = publisher.artifact("1.2.4");
        let Reply::Progress(completed) = stage(&recreated, &resolve, Some(&original_again)) else {
            panic!("re-supplied exact original package");
        };
        assert_eq!(completed.request, original.request);
        assert_eq!(completed.request_digest, reference.request_digest);
        let recreated_again = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
        assert_eq!(
            stage(&recreated_again, &resolve, None),
            Reply::Progress(completed.clone())
        );
        assert_eq!(
            stage(&recreated_again, &restore, None),
            Reply::Restored {
                original,
                assessment: Some(reference),
                progress: Some(completed)
            }
        );
        assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
        close_fixture(fixture).await;
    }
}

#[tokio::test]
async fn stage_rejects_changed_capture_digest_proof_epoch_expiry_and_input_before_acceptance() {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle.clone(),
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let mut bad_capture = original;
    bad_capture.capture_digest = RequestDigest::from_bytes([52; 32]);
    let mut bad_archive = original;
    bad_archive.artifact_sha256 = [53; 32];
    for capture in [bad_capture, bad_archive] {
        assert_eq!(
            native_lifecycle_call(
                &runtime,
                2,
                native.key().account_uid,
                &session,
                AccountRuntimeCall::AssessNativeStage(capture)
            )
            .await,
            AccountRuntimeReply::Failed(ServiceFailure::RequestConflict)
        );
    }
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    let mut digest = reference.clone();
    digest.request_digest = RequestDigest::from_bytes([54; 32]);
    let mut proof = reference.clone();
    proof.proof_digest = RequestDigest::from_bytes([55; 32]);
    let mut epoch = reference.clone();
    epoch.epoch = ServerEpoch::new(reference.epoch.get() + 1);
    let mut expiry = reference.clone();
    expiry.expires_at.millis += 1;
    for reference in [digest, proof, epoch, expiry] {
        assert_eq!(
            native_lifecycle_call(
                &runtime,
                2,
                native.key().account_uid,
                &session,
                AccountRuntimeCall::AdmitNativeStage(reference)
            )
            .await,
            AccountRuntimeReply::Failed(ServiceFailure::RequestConflict)
        );
    }
    let resolve = resolve_request(&native, original.request);
    assert!(matches!(
        stage(&native, &resolve, None),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Reserved,
            ..
        })
    ));
    clock.0.store(reference.expires_at.millis, Ordering::SeqCst);
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeStage(reference.clone())
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        native.restore_runtime_stage(original.request).unwrap(),
        original
    );
    assert_eq!(
        native.restore_runtime_stage_reference(original).unwrap(),
        reference
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_final_actor_admission_rejects_different_caller_replaced_connection_and_revoked_client()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let (_, other_comparison, other_key) = fixture.enroll_with_seed(2, 81).await;
    let session = native_authenticated_session(&fixture, comparison, key.clone(), 2, 1);
    let principal = session.principal;
    let replacement_session = native_authenticated_session(&fixture, comparison, key, 2, 2);
    let replacement = replacement_session.principal;
    let other_session = native_authenticated_session(&fixture, other_comparison, other_key, 2, 1);
    let other = other_session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    handle.register(other).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle.clone(),
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &other_session,
            AccountRuntimeCall::AdmitNativeStage(reference.clone())
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::PairingRequired)
    );
    handle.register(replacement).await.unwrap();
    assert!(matches!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeStage(reference.clone())
        )
        .await,
        AccountRuntimeReply::Failed(_)
    ));
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &replacement_session,
            AccountRuntimeCall::AdmitNativeStage(reference)
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::PairingRequired)
    );
    // A fresh original is needed for the new connection. Revocation between its
    // assessment and admission cannot leave authority in that retained proof.
    let original2 = capture(&native, &artifact);
    let reference2 = assessed(&runtime, &native, &replacement_session, 2, original2).await;
    handle
        .revoke_clients(vec![replacement.client()])
        .await
        .unwrap();
    assert!(matches!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &replacement_session,
            AccountRuntimeCall::AdmitNativeStage(reference2)
        )
        .await,
        AccountRuntimeReply::Failed(_)
    ));
    let resolve = resolve_request(&native, original2.request);
    assert!(matches!(
        stage(&native, &resolve, Some(&artifact)),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Reserved,
            ..
        })
    ));
    assert_eq!(
        native.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn simultaneous_stage_callers_keep_distinct_original_scope_and_second_capture_becomes_stale_after_publication()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let (_, other_comparison, other_key) = fixture.enroll_with_seed(2, 81).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let other_session = native_authenticated_session(&fixture, other_comparison, other_key, 2, 1);
    let other = other_session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    handle.register(other).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let first_artifact = publisher.artifact("1.2.4");
    let second_artifact = publisher.artifact("1.2.5");
    let first = capture(&native, &first_artifact);
    let second = capture(&native, &second_artifact);
    let first_ref = assessed(&runtime, &native, &session, 2, first).await;
    let second_ref = assessed(&runtime, &native, &other_session, 2, second).await;
    assert_ne!(first.request, second.request);
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &other_session,
            AccountRuntimeCall::AdmitNativeStage(first_ref.clone())
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::PairingRequired)
    );
    assert!(matches!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeStage(first_ref)
        )
        .await,
        AccountRuntimeReply::NativeStageAccepted(_)
    ));
    assert!(matches!(
        stage(
            &native,
            &resolve_request(&native, first.request),
            Some(&first_artifact)
        ),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Completed,
            ..
        })
    ));
    assert_eq!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &other_session,
            AccountRuntimeCall::AdmitNativeStage(second_ref.clone())
        )
        .await,
        AccountRuntimeReply::Failed(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        native.restore_runtime_stage(second.request).unwrap(),
        second
    );
    assert_eq!(
        native.restore_runtime_stage_reference(second).unwrap(),
        second_ref
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn wrong_native_identity_and_unauthenticated_guest_uid_cannot_capture_or_assess_stage() {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let artifact = publisher.artifact("1.2.4");
    let request = native.allocate_request().unwrap();
    let key = native.key();
    let pin = fixture.identity.pin();
    let mut wrong_uid = key;
    wrong_uid.account_uid += 1;
    let mut wrong_build = key;
    wrong_build.build_identity = BuildIdentity::Preflight;
    let mut wrong_pin = pin;
    wrong_pin.public_key = [59; 32];
    for (expected_key, expected_pin) in [(wrong_uid, pin), (wrong_build, pin), (key, wrong_pin)] {
        for call in [
            Request::Capture {
                expected_key,
                expected_pin,
                request,
                artifact_sha256: artifact.artifact_sha256(),
            },
            Request::Restore {
                expected_key,
                expected_pin,
                request,
            },
            Request::Resolve {
                expected_key,
                expected_pin,
                request,
            },
        ] {
            assert_eq!(
                stage(&native, &call, Some(&artifact)),
                Reply::Refused(ServiceFailure::IdentityMismatch)
            );
        }
    }
    let original = native.capture_runtime_stage(request, &artifact).unwrap();
    let (_, comparison, pairing_key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, pairing_key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    for uid in [None, Some(key.account_uid + 1)] {
        assert_eq!(
            runtime
                .dispatch(
                    &native_lifecycle_payload(2, AccountRuntimeCall::AssessNativeStage(original)),
                    uid,
                    &session
                )
                .await
                .unwrap_err()
                .class,
            spaceterm_server::contracts::ServerFailureClass::Authentication
        );
    }
    assert_eq!(
        native
            .restore_runtime_stage_reference(original)
            .unwrap_err(),
        ServiceFailure::Pending
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_final_serialized_turn_rechecks_current_authorization_version_and_connection_after_preparation()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key.clone(), 2, 1);
    let principal = session.principal;
    let replacement_session = native_authenticated_session(&fixture, comparison, key, 2, 2);
    let replacement = replacement_session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let custody = spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
        &fixture.registration,
        handle.snapshot().epoch(),
        fixture.identity.clone(),
    )
    .unwrap();
    let (authorized, version) = handle.authorize_account_lifecycle(principal).await.unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    // Native proof comparison alone cannot override the actor's current durable
    // authorization version. The final actor turn independently supplies it.
    let wrong = custody
        .native_lifecycle_authority(authorized, FieldVersion::new(version.get() + 1))
        .unwrap();
    let reference = native
        .assess_runtime_stage(original, &wrong, principal.binding().generation)
        .unwrap();
    let admission = native
        .prepare_runtime_stage_admission(reference, wrong, principal.binding().generation)
        .unwrap();
    assert_eq!(
        handle
            .admit_account_stage(principal, admission)
            .await
            .unwrap_err(),
        ServiceFailure::PairingRequired
    );
    assert!(matches!(
        stage(&native, &resolve_request(&native, original.request), None),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Reserved,
            ..
        })
    ));

    let original2 = capture(&native, &artifact);
    let authority = custody
        .native_lifecycle_authority(authorized, version)
        .unwrap();
    let reference2 = native
        .assess_runtime_stage(original2, &authority, principal.binding().generation)
        .unwrap();
    let prepared = native
        .prepare_runtime_stage_admission(
            reference2.clone(),
            authority,
            principal.binding().generation,
        )
        .unwrap();
    handle.register(replacement).await.unwrap();
    assert_eq!(
        handle
            .admit_account_stage(principal, prepared)
            .await
            .unwrap_err(),
        ServiceFailure::PairingRequired
    );
    assert_eq!(
        native.restore_runtime_stage_reference(original2).unwrap(),
        reference2
    );
    assert!(matches!(
        stage(&native, &resolve_request(&native, original2.request), None),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Reserved,
            ..
        })
    ));
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn uncertain_stage_accepted_publication_recovers_only_that_original_without_reassessment_or_owner_promotion()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let custody = spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
        &fixture.registration,
        handle.snapshot().epoch(),
        fixture.identity.clone(),
    )
    .unwrap();
    let (authorized, version) = handle.authorize_account_lifecycle(principal).await.unwrap();
    let authority = custody
        .native_lifecycle_authority(authorized, version)
        .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = native
        .assess_runtime_stage(original, &authority, principal.binding().generation)
        .unwrap();
    let admission = native
        .prepare_runtime_stage_admission(
            reference.clone(),
            authority,
            principal.binding().generation,
        )
        .unwrap();
    spaceterm_host::account_service::test_support::fail_next_publication_durability();
    assert_eq!(
        handle
            .admit_account_stage(principal, admission)
            .await
            .unwrap_err(),
        ServiceFailure::Indeterminate
    );
    assert_eq!(
        native.discover().unwrap_err(),
        ServiceFailure::Indeterminate
    );
    let reconstructed = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let restore = Request::Restore {
        expected_key: native.key(),
        expected_pin: fixture.identity.pin(),
        request: original.request,
    };
    let Reply::Restored {
        original: recovered,
        assessment: Some(recovered_ref),
        progress: Some(accepted),
    } = stage(&reconstructed, &restore, None)
    else {
        panic!("recovered original Accepted publication");
    };
    assert_eq!(recovered, original);
    assert_eq!(recovered_ref, reference);
    assert_eq!(accepted.request, original.request);
    assert_eq!(accepted.phase, NativePhase::Accepted);
    assert_eq!(accepted.request_digest, reference.request_digest);
    assert_eq!(accepted.state, ServiceRequestState::Pending);
    // Storage recovery clears this original observer's latch, not its expiry.
    assert_eq!(
        stage(&native, &restore, None),
        Reply::Restored {
            original,
            assessment: Some(reference),
            progress: Some(accepted)
        }
    );
    let resolve = resolve_request(&reconstructed, original.request);
    assert!(matches!(
        stage(&reconstructed, &resolve, Some(&artifact)),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Completed,
            ..
        })
    ));
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_final_turn_waits_for_in_flight_durable_pairing_write_instead_of_accepting_old_authorization()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let custody = spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
        &fixture.registration,
        handle.snapshot().epoch(),
        fixture.identity.clone(),
    )
    .unwrap();
    let (authorized, version) = handle.authorize_account_lifecycle(principal).await.unwrap();
    let authority = custody
        .native_lifecycle_authority(authorized, version)
        .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = native
        .assess_runtime_stage(original, &authority, principal.binding().generation)
        .unwrap();
    let admission = native
        .prepare_runtime_stage_admission(reference, authority, principal.binding().generation)
        .unwrap();
    let gate = native::StoreGate::new();
    *fixture.write_gate.lock().unwrap() = Some(gate.clone());
    let writer = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.revoke_clients(vec![principal.client()]).await })
    };
    gate.entered().await;
    assert_eq!(
        handle
            .admit_account_stage(principal, admission)
            .await
            .unwrap_err(),
        ServiceFailure::Pending
    );
    assert!(matches!(
        stage(&native, &resolve_request(&native, original.request), None),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Reserved,
            ..
        })
    ));
    gate.release();
    writer.await.unwrap().unwrap();
    assert!(handle.authorize_account_lifecycle(principal).await.is_err());
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn native_stage_copy_finishes_the_original_after_foreground_disconnect_and_provider_recreation()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    // Raw owner Start and resolution cannot turn this signed Reserved Stage into
    // native authority. They cannot substitute another operation for its scope.
    assert_eq!(
        native.start(original.request).await.unwrap_err(),
        ServiceFailure::PairingRequired
    );
    assert_eq!(
        native.resolve(original.request).await.unwrap_err(),
        ServiceFailure::Pending
    );
    assert!(matches!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeStage(reference.clone())
        )
        .await,
        AccountRuntimeReply::NativeStageAccepted(_)
    ));
    let gate = native::StoreGate::new();
    let (reply, foreground) = tokio::sync::oneshot::channel();
    let worker = {
        let gate = gate.clone();
        let native = native.clone();
        let request = resolve_request(&native, original.request);
        tokio::task::spawn_blocking(move || {
            // The native owner retains both the original archive and lifecycle
            // lease until copy and its journal publications have finished.
            spaceterm_host::account_service::test_support::after_next_phase(
                NativePhase::Publishing,
                move || {
                    gate.block();
                    Ok(())
                },
            );
            let result = stage(&native, &request, Some(&artifact));
            reply.send(result).is_err()
        })
    };
    gate.entered().await;
    drop(foreground);
    let reconstructed = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let restore = Request::Restore {
        expected_key: native.key(),
        expected_pin: fixture.identity.pin(),
        request: original.request,
    };
    let Reply::Restored {
        original: retained,
        assessment: Some(retained_ref),
        progress: Some(copying),
    } = stage(&reconstructed, &restore, None)
    else {
        panic!("original in-flight Stage copy");
    };
    assert_eq!(retained, original);
    assert_eq!(retained_ref, reference);
    assert_eq!(copying.request_digest, reference.request_digest);
    assert_eq!(copying.phase, NativePhase::Publishing);
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    gate.release();
    assert!(
        worker.await.unwrap(),
        "the native owner finished despite losing its foreground reply"
    );
    let resolve = resolve_request(&reconstructed, original.request);
    let Reply::Progress(completed) = stage(&reconstructed, &resolve, None) else {
        panic!("original completed Stage effect");
    };
    assert_eq!(completed.request, original.request);
    assert_eq!(completed.request_digest, reference.request_digest);
    assert_eq!(completed.phase, NativePhase::Completed);
    assert_eq!(
        native.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

async fn start_additional_stage_shell(
    seed: &Seed,
    transport: &dyn MessageTransport,
    codec: &mut PublicationCodec,
    handle: &ServerHandle,
    connection: ConnectionGeneration,
) -> SessionKey {
    let result = operation(
        seed,
        transport,
        codec,
        4,
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
        panic!("additional Workspace while Stage Reserved");
    };
    let grant = created.control().grant();
    let pane = grant.pane();
    let generation = handle
        .snapshot()
        .runtime()
        .iter()
        .find(|record| record.pane == pane)
        .unwrap()
        .session_generation;
    let fence = EffectFence::new(
        SessionKey {
            pane,
            epoch: handle.snapshot().epoch(),
            generation,
        },
        grant,
        ConnectionBinding::new(grant.client(), connection),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent = StartupIntent::new(
        OperationId::new(
            seed.streams.parts().ordinary,
            OperationSequence::new(5).unwrap(),
        ),
        fence,
        StartupSource::ExplicitSelection,
    )
    .unwrap();
    assert!(matches!(
        operation(seed, transport, codec, 5, Command::BeginStartup(intent)).await,
        Outcome::StartupAwaitingMeasurement { .. }
    ));
    let Outcome::SessionStarted(session) = operation(
        seed,
        transport,
        codec,
        6,
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
        panic!("additional live Session while Stage Reserved");
    };
    while !handle
        .snapshot()
        .runtime()
        .iter()
        .any(|record| record.pane == pane && matches!(record.phase, SessionPhase::Running { .. }))
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    session
}

#[tokio::test]
async fn stage_of_different_existing_candidate_is_refused_before_capture_or_acceptance_without_discard_authority()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let first = publisher.artifact("1.2.4");
    let original = capture(&native, &first);
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    assert!(matches!(
        native_lifecycle_call(
            &runtime,
            2,
            native.key().account_uid,
            &session,
            AccountRuntimeCall::AdmitNativeStage(reference)
        )
        .await,
        AccountRuntimeReply::NativeStageAccepted(_)
    ));
    let resolve = resolve_request(&native, original.request);
    assert!(matches!(
        stage(&native, &resolve, Some(&first)),
        Reply::Progress(ServiceProgress {
            phase: NativePhase::Completed,
            ..
        })
    ));
    let before = native.discover().unwrap().unwrap();
    let request = native.allocate_request().unwrap();
    let account =
        AccountServiceFixture::new(fixture._root.path(), BuildIdentity::Development).unwrap();
    let journal = fs::read(account.anchor().join("requests")).unwrap();
    let next = publisher.artifact("1.2.5");
    assert_eq!(
        stage(
            &native,
            &Request::Capture {
                expected_key: before.key(),
                expected_pin: before.server_pin(),
                request,
                artifact_sha256: next.artifact_sha256(),
            },
            Some(&next)
        ),
        Reply::Refused(ServiceFailure::RequestConflict)
    );
    assert_eq!(
        fs::read(account.anchor().join("requests")).unwrap(),
        journal
    );
    assert_eq!(
        native.restore_runtime_stage(request).unwrap_err(),
        ServiceFailure::RequestConflict
    );
    let current = native.discover().unwrap().unwrap();
    assert_eq!(current.generation(), before.generation());
    assert_eq!(current.staged_installation(), before.staged_installation());
    assert_eq!(
        current.approved_installation(),
        before.approved_installation()
    );
    assert_eq!(current.desired_state(), DesiredServerState::Running);
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

struct FixedTime(ServiceTime);
impl ServiceClock for FixedTime {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(self.0)
    }
}
fn stage_supervisor_at(
    root: &std::path::Path,
    publisher: &Publisher,
    manager: &Arc<Manager>,
    time: ServiceTime,
) -> ServiceSupervisor {
    let account = AccountServiceFixture::new(root, BuildIdentity::Development).unwrap();
    account.with_installation_trust(
        account.with_providers(
            manager.clone(),
            Arc::new(RetainedRuntimeProcesses),
            Arc::new(FixedTime(time)),
            None,
        ),
        publisher.key.clone(),
    )
}

#[tokio::test]
async fn stage_handoff_preserves_unsigned_capture_and_requires_valid_native_expiry_observation() {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let key = native.key();
    let pin = fixture.identity.pin();
    let account =
        AccountServiceFixture::new(fixture._root.path(), BuildIdentity::Development).unwrap();
    let before = fs::read(account.anchor().join("requests")).unwrap();
    let Handoff::Awaiting { expires_at } = native
        .poll_runtime_stage_handoff(key, pin, original)
        .unwrap()
    else {
        panic!("unsigned Stage awaiting original admission");
    };
    for time in [
        ServiceTime {
            boot: [0; 32],
            millis: expires_at.millis,
        },
        ServiceTime {
            boot: expires_at.boot,
            millis: 9,
        },
    ] {
        let observer = stage_supervisor_at(fixture._root.path(), &publisher, &manager, time);
        assert_eq!(
            observer
                .poll_runtime_stage_handoff(key, pin, original)
                .unwrap_err(),
            ServiceFailure::SupervisorUnavailable
        );
        assert_eq!(fs::read(account.anchor().join("requests")).unwrap(), before);
    }
    clock.0.store(expires_at.millis, Ordering::SeqCst);
    assert_eq!(
        native
            .poll_runtime_stage_handoff(key, pin, original)
            .unwrap(),
        Handoff::ExpiredNonAdmission {
            original,
            progress: None
        }
    );
    assert_eq!(
        native.restore_runtime_stage(original.request).unwrap(),
        original
    );
    assert_eq!(
        native
            .restore_runtime_stage_reference(original)
            .unwrap_err(),
        ServiceFailure::Pending
    );
    assert_eq!(fs::read(account.anchor().join("requests")).unwrap(), before);
    let rebooted = stage_supervisor_at(
        fixture._root.path(),
        &publisher,
        &manager,
        ServiceTime {
            boot: [2; 32],
            millis: 0,
        },
    );
    assert_eq!(
        rebooted
            .poll_runtime_stage_handoff(key, pin, original)
            .unwrap(),
        Handoff::ExpiredNonAdmission {
            original,
            progress: None
        }
    );
    assert_eq!(fs::read(account.anchor().join("requests")).unwrap(), before);
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_handoff_seals_only_signed_never_admitted_expiry_and_cached_admission_cannot_promote_it()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let custody = spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
        &fixture.registration,
        handle.snapshot().epoch(),
        fixture.identity.clone(),
    )
    .unwrap();
    let (authorized, version) = handle.authorize_account_lifecycle(principal).await.unwrap();
    let authority = custody
        .native_lifecycle_authority(authorized, version)
        .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = native
        .assess_runtime_stage(original, &authority, principal.binding().generation)
        .unwrap();
    let cached = native
        .prepare_runtime_stage_admission(
            reference.clone(),
            authority,
            principal.binding().generation,
        )
        .unwrap();
    let before = native.discover().unwrap().unwrap();
    assert_eq!(
        native
            .poll_runtime_stage_handoff(before.key(), before.server_pin(), original)
            .unwrap(),
        Handoff::Awaiting {
            expires_at: reference.expires_at
        }
    );
    clock.0.store(reference.expires_at.millis, Ordering::SeqCst);
    let Handoff::ExpiredNonAdmission {
        original: sealed,
        progress: Some(terminal),
    } = native
        .poll_runtime_stage_handoff(before.key(), before.server_pin(), original)
        .unwrap()
    else {
        panic!("exact signed Stage non-admission receipt");
    };
    assert_eq!(sealed, original);
    assert_eq!(terminal.request, original.request);
    assert_eq!(terminal.request_digest, reference.request_digest);
    assert_eq!(terminal.phase, NativePhase::Completed);
    assert_eq!(
        terminal.state,
        ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        native.restore_runtime_stage_reference(original).unwrap(),
        reference
    );
    let restored = Request::Restore {
        expected_key: before.key(),
        expected_pin: before.server_pin(),
        request: original.request,
    };
    assert_eq!(
        stage(&native, &restored, None),
        Reply::Restored {
            original,
            assessment: Some(reference),
            progress: Some(terminal.clone())
        }
    );
    // The journal's sealed decision is irreversible, even if a later clock
    // observation incorrectly looks fresh again. No effects were ever admitted.
    clock.0.store(10, Ordering::SeqCst);
    assert_eq!(
        handle.admit_account_stage(principal, cached).await.unwrap(),
        terminal
    );
    assert_eq!(
        native
            .resolve_runtime_stage_exact(original.request, Some(&artifact))
            .unwrap(),
        terminal
    );
    assert_eq!(
        native
            .poll_runtime_stage_handoff(before.key(), before.server_pin(), original)
            .unwrap(),
        Handoff::ResolveOriginal {
            progress: terminal.clone()
        }
    );
    let current = native.discover().unwrap().unwrap();
    assert_eq!(current.generation(), before.generation());
    assert_eq!(
        current.approved_installation(),
        before.approved_installation()
    );
    assert_eq!(current.staged_installation(), before.staged_installation());
    assert_eq!(current.desired_state(), DesiredServerState::Running);
    let store = spaceterm_host::server_installation::test_support::existing_store(
        &current,
        publisher.key.clone(),
    )
    .unwrap();
    assert!(store.snapshot().unwrap().staged.is_none());
    let ack = Request::Acknowledge {
        expected_key: before.key(),
        expected_pin: before.server_pin(),
        progress: terminal.clone(),
    };
    assert_eq!(
        stage(&native, &ack, None),
        Reply::Acknowledged {
            progress: terminal.clone()
        }
    );
    let recreated = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    assert_eq!(
        stage(&recreated, &ack, None),
        Reply::Acknowledged {
            progress: terminal.clone()
        }
    );
    assert_eq!(
        recreated
            .poll_runtime_stage_handoff(before.key(), before.server_pin(), original)
            .unwrap(),
        Handoff::ResolveOriginal { progress: terminal }
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_handoff_never_reclassifies_accepted_input_pending_work_after_expiry_or_invalid_clock()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    let AccountRuntimeReply::NativeStageAccepted(accepted) = native_lifecycle_call(
        &runtime,
        2,
        native.key().account_uid,
        &session,
        AccountRuntimeCall::AdmitNativeStage(reference.clone()),
    )
    .await
    else {
        panic!("original Stage Accepted");
    };
    for time in [
        ServiceTime {
            boot: reference.expires_at.boot,
            millis: reference.expires_at.millis,
        },
        ServiceTime {
            boot: [2; 32],
            millis: 1,
        },
        ServiceTime {
            boot: [0; 32],
            millis: 0,
        },
    ] {
        let observer = stage_supervisor_at(fixture._root.path(), &publisher, &manager, time);
        assert_eq!(
            observer
                .poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), original)
                .unwrap(),
            Handoff::ResolveOriginal {
                progress: accepted.clone()
            }
        );
        assert_eq!(
            observer
                .resolve_runtime_stage_exact(original.request, None)
                .unwrap(),
            accepted
        );
    }
    clock
        .0
        .store(reference.expires_at.millis + 1, Ordering::SeqCst);
    let completed = native
        .resolve_runtime_stage_exact(original.request, Some(&artifact))
        .unwrap();
    assert_eq!(completed.phase, NativePhase::Completed);
    assert!(matches!(
        completed.state,
        ServiceRequestState::Completed(ServiceOutcome::Staged(_))
    ));
    assert_eq!(
        native
            .poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), original)
            .unwrap(),
        Handoff::ResolveOriginal {
            progress: completed
        }
    );
    assert_eq!(
        native.restore_runtime_stage_reference(original).unwrap(),
        reference
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_handoff_recovers_uncertain_non_admission_publication_as_only_the_same_terminal_original()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        native.clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    clock.0.store(reference.expires_at.millis, Ordering::SeqCst);
    spaceterm_host::account_service::test_support::fail_next_publication_durability();
    assert_eq!(
        native
            .poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), original)
            .unwrap_err(),
        ServiceFailure::Indeterminate
    );
    assert_eq!(
        native.discover().unwrap_err(),
        ServiceFailure::Indeterminate
    );
    let recreated = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let Handoff::ResolveOriginal { progress: terminal } = recreated
        .poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), original)
        .unwrap()
    else {
        panic!("recovered original sealed Stage expiry");
    };
    assert_eq!(terminal.request, original.request);
    assert_eq!(terminal.request_digest, reference.request_digest);
    assert_eq!(terminal.phase, NativePhase::Completed);
    assert_eq!(
        terminal.state,
        ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        recreated.restore_runtime_stage(original.request).unwrap(),
        original
    );
    assert_eq!(
        recreated.restore_runtime_stage_reference(original).unwrap(),
        reference
    );
    assert_eq!(
        native
            .poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), original)
            .unwrap(),
        Handoff::ResolveOriginal {
            progress: terminal.clone()
        }
    );
    assert_eq!(
        recreated
            .resolve_runtime_stage_exact(original.request, None)
            .unwrap(),
        terminal
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

struct PausingClock {
    inner: Arc<Clock>,
    next: Mutex<Option<Arc<native::StoreGate>>>,
}
impl ServiceClock for PausingClock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        let gate = self
            .next
            .lock()
            .map_err(|_| ServiceFailure::SupervisorUnavailable)?
            .take();
        if let Some(gate) = gate {
            gate.block();
        }
        self.inner.now()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stage_handoff_cannot_seal_expiry_while_final_actor_owns_the_admission_lease() {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let time = Clock::new();
    let clock = Arc::new(PausingClock {
        inner: time.clone(),
        next: Mutex::new(None),
    });
    let account =
        AccountServiceFixture::new(fixture._root.path(), BuildIdentity::Development).unwrap();
    let native = account.with_installation_trust(
        account.with_providers(
            manager.clone(),
            Arc::new(RetainedRuntimeProcesses),
            clock.clone(),
            None,
        ),
        publisher.key.clone(),
    );
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let principal = session.principal;
    let handle = fixture.server.handle();
    handle.register(principal).await.unwrap();
    let custody = spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
        &fixture.registration,
        handle.snapshot().epoch(),
        fixture.identity.clone(),
    )
    .unwrap();
    let (authorized, version) = handle.authorize_account_lifecycle(principal).await.unwrap();
    let authority = custody
        .native_lifecycle_authority(authorized, version)
        .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = native
        .assess_runtime_stage(original, &authority, principal.binding().generation)
        .unwrap();
    let admission = native
        .prepare_runtime_stage_admission(
            reference.clone(),
            authority,
            principal.binding().generation,
        )
        .unwrap();
    time.0
        .store(reference.expires_at.millis - 1, Ordering::SeqCst);
    let gate = native::StoreGate::new();
    *clock.next.lock().unwrap() = Some(gate.clone());
    let accepting =
        tokio::spawn(async move { handle.admit_account_stage(principal, admission).await });
    gate.entered().await;
    let observer = stage_supervisor_at(
        fixture._root.path(),
        &publisher,
        &manager,
        reference.expires_at,
    );
    let contested =
        observer.poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), original);
    gate.release();
    let accepted = accepting.await.unwrap().unwrap();
    assert_eq!(contested.unwrap_err(), ServiceFailure::Pending);
    assert_eq!(accepted.phase, NativePhase::Accepted);
    assert_eq!(accepted.request_digest, reference.request_digest);
    assert_eq!(
        observer
            .poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), original)
            .unwrap(),
        Handoff::ResolveOriginal {
            progress: accepted.clone()
        }
    );
    assert_eq!(
        observer
            .resolve_runtime_stage_exact(original.request, None)
            .unwrap(),
        accepted
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_owner_ready_capture_retains_exact_input_and_eof_waits_for_native_unsigned_expiry() {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = Arc::new(stage_supervisor(
        fixture._root.path(),
        &publisher,
        &manager,
        &clock,
    ));
    let before = native.discover().unwrap().unwrap();
    let artifact = Arc::new(publisher.artifact("1.2.4"));
    let retained_input = Arc::downgrade(&artifact);
    let request = native.allocate_request().unwrap();
    let mut owners = NativeStageOriginalOwners::new(native.clone());
    let Reply::Captured(original) = owners
        .handle(
            &Request::Capture {
                expected_key: before.key(),
                expected_pin: before.server_pin(),
                request,
                artifact_sha256: artifact.artifact_sha256(),
            },
            Some(artifact.clone()),
        )
        .await
    else {
        panic!("owned exact Stage capture");
    };
    drop(artifact);
    assert!(retained_input.upgrade().is_some());
    assert_eq!(native.restore_runtime_stage(request).unwrap(), original);
    assert_eq!(
        native.inspect_install_request(request).unwrap_err(),
        ServiceFailure::UnknownRequest
    );
    assert_eq!(
        native
            .restore_runtime_stage_reference(original)
            .unwrap_err(),
        ServiceFailure::Pending
    );
    let joined = tokio::spawn(owners.finish());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !joined.is_finished(),
        "EOF is not native expiry or Stage admission"
    );
    assert!(retained_input.upgrade().is_some());
    assert!(before.staged_installation().is_none());
    clock.0.store(30_010, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), joined)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(retained_input.upgrade().is_none());
    assert_eq!(native.restore_runtime_stage(request).unwrap(), original);
    assert_eq!(
        native.inspect_install_request(request).unwrap_err(),
        ServiceFailure::UnknownRequest
    );
    let current = native.discover().unwrap().unwrap();
    assert_eq!(current.generation(), before.generation());
    assert_eq!(current.desired_state(), DesiredServerState::Running);
    assert_eq!(
        current.approved_installation(),
        before.approved_installation()
    );
    assert!(current.staged_installation().is_none());
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_owner_keeps_original_archive_after_capture_reply_loss_and_finishes_accepted_work_after_eof()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = Arc::new(stage_supervisor(
        fixture._root.path(),
        &publisher,
        &manager,
        &clock,
    ));
    let before = native.discover().unwrap().unwrap();
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let handle = fixture.server.handle();
    handle.register(session.principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        (*native).clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let artifact = Arc::new(publisher.artifact("1.2.4"));
    let retained_input = Arc::downgrade(&artifact);
    let request = native.allocate_request().unwrap();
    let capture_request = Request::Capture {
        expected_key: before.key(),
        expected_pin: before.server_pin(),
        request,
        artifact_sha256: artifact.artifact_sha256(),
    };
    let mut owners = NativeStageOriginalOwners::new(native.clone());
    let (reply, foreground) = tokio::sync::oneshot::channel();
    drop(foreground);
    assert!(
        reply
            .send(
                owners
                    .handle(&capture_request, Some(artifact.clone()))
                    .await
            )
            .is_err()
    );
    drop(artifact);
    let original = native.restore_runtime_stage(request).unwrap();
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    assert!(retained_input.upgrade().is_some());
    let joined = tokio::spawn(owners.finish());
    let admitted = native_lifecycle_call(
        &runtime,
        2,
        native.key().account_uid,
        &session,
        AccountRuntimeCall::AdmitNativeStage(reference.clone()),
    )
    .await;
    assert!(matches!(
        admitted,
        AccountRuntimeReply::NativeStageAccepted(_)
    ));
    tokio::time::timeout(Duration::from_secs(5), joined)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(retained_input.upgrade().is_none());
    let recreated = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let Reply::Progress(completed) = stage(&recreated, &resolve_request(&recreated, request), None)
    else {
        panic!("owned original Stage effect completed after EOF");
    };
    assert_eq!(completed.request, request);
    assert_eq!(completed.request_digest, reference.request_digest);
    assert_eq!(completed.phase, NativePhase::Completed);
    assert!(matches!(
        completed.state,
        ServiceRequestState::Completed(ServiceOutcome::Staged(_))
    ));
    assert_eq!(recreated.restore_runtime_stage(request).unwrap(), original);
    assert_eq!(
        recreated.restore_runtime_stage_reference(original).unwrap(),
        reference
    );
    let current = recreated.discover().unwrap().unwrap();
    assert_eq!(current.desired_state(), DesiredServerState::Running);
    assert_eq!(
        current.approved_installation(),
        before.approved_installation()
    );
    assert_eq!(
        current.staged_installation().unwrap().artifact_sha256,
        original.artifact_sha256
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_recreated_owners_keep_accepted_missing_input_pending_until_exact_original_resupply()
{
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = Arc::new(stage_supervisor(
        fixture._root.path(),
        &publisher,
        &manager,
        &clock,
    ));
    let (_, comparison, key) = fixture.enroll(2).await;
    let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
    let handle = fixture.server.handle();
    handle.register(session.principal).await.unwrap();
    let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
        handle,
        (*native).clone(),
        fixture.identity.clone(),
    )
    .unwrap();
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let reference = assessed(&runtime, &native, &session, 2, original).await;
    let AccountRuntimeReply::NativeStageAccepted(accepted) = native_lifecycle_call(
        &runtime,
        2,
        native.key().account_uid,
        &session,
        AccountRuntimeCall::AdmitNativeStage(reference.clone()),
    )
    .await
    else {
        panic!("original Stage admission");
    };
    drop(artifact);
    clock
        .0
        .store(reference.expires_at.millis + 1, Ordering::SeqCst);
    let recreated = Arc::new(stage_supervisor(
        fixture._root.path(),
        &publisher,
        &manager,
        &clock,
    ));
    let mut missing_input = NativeStageOriginalOwners::new(recreated.clone());
    let restore = Request::Restore {
        expected_key: native.key(),
        expected_pin: fixture.identity.pin(),
        request: original.request,
    };
    assert_eq!(
        missing_input.handle(&restore, None).await,
        Reply::Restored {
            original,
            assessment: Some(reference.clone()),
            progress: Some(accepted.clone()),
        }
    );
    let joined_without_input = tokio::spawn(missing_input.finish());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!joined_without_input.is_finished());
    assert_eq!(
        stage(
            &recreated,
            &resolve_request(&recreated, original.request),
            None
        ),
        Reply::Progress(accepted)
    );
    let mut supplied = NativeStageOriginalOwners::new(recreated.clone());
    let resolve = resolve_request(&recreated, original.request);
    assert_eq!(
        supplied
            .handle(&resolve, Some(Arc::new(publisher.artifact("1.2.5"))))
            .await,
        Reply::Refused(ServiceFailure::RequestConflict)
    );
    assert!(!joined_without_input.is_finished());
    let result = supplied
        .handle(&resolve, Some(Arc::new(publisher.artifact("1.2.4"))))
        .await;
    assert!(
        matches!(
            result,
            Reply::Progress(_) | Reply::Refused(ServiceFailure::Pending)
        ),
        "{result:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), supplied.finish())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), joined_without_input)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Reply::Progress(completed) = stage(&recreated, &resolve, None) else {
        panic!("exact resupply effect");
    };
    assert_eq!(completed.request, original.request);
    assert_eq!(completed.request_digest, reference.request_digest);
    assert_eq!(completed.phase, NativePhase::Completed);
    assert_eq!(
        recreated.restore_runtime_stage(original.request).unwrap(),
        original
    );
    assert_eq!(
        recreated.restore_runtime_stage_reference(original).unwrap(),
        reference
    );
    let token = recreated
        .native_stage_owner_token(native.key(), fixture.identity.pin(), original)
        .unwrap();
    let ack = Request::Acknowledge {
        expected_key: native.key(),
        expected_pin: fixture.identity.pin(),
        progress: completed.clone(),
    };
    assert_eq!(
        stage(&native, &ack, None),
        Reply::Acknowledged {
            progress: completed.clone()
        }
    );
    assert_eq!(
        recreated.poll_owned_runtime_stage_handoff(&token).unwrap(),
        Handoff::ResolveOriginal {
            progress: completed
        }
    );
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_owned_pressure_retirement_is_drain_only_and_finish_joins_other_originals_before_returning_error()
 {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = Arc::new(stage_supervisor(
        fixture._root.path(),
        &publisher,
        &manager,
        &clock,
    ));
    let artifact = Arc::new(publisher.artifact("1.2.4"));
    let mut owners = NativeStageOriginalOwners::new(native.clone());
    let mut originals = Vec::new();
    for _ in 0..2 {
        let request = native.allocate_request().unwrap();
        let Reply::Captured(original) = owners
            .handle(
                &Request::Capture {
                    expected_key: native.key(),
                    expected_pin: fixture.identity.pin(),
                    request,
                    artifact_sha256: artifact.artifact_sha256(),
                },
                Some(artifact.clone()),
            )
            .await
        else {
            panic!("two original owners");
        };
        originals.push(original);
    }
    let first = originals[0];
    let second = originals[1];
    let token = native
        .native_stage_owner_token(native.key(), fixture.identity.pin(), first)
        .unwrap();
    let mut retired = false;
    // Pressure runs in this serialized test turn, before either owner polls.
    for _ in 0..300 {
        native.allocate_request().unwrap();
        if native.restore_runtime_stage(first.request) == Err(ServiceFailure::RequestRetired) {
            retired = true;
            break;
        }
    }
    assert!(retired);
    assert_eq!(
        native.restore_runtime_stage(second.request).unwrap(),
        second
    );
    assert_eq!(
        native.poll_runtime_stage_handoff(native.key(), fixture.identity.pin(), first),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        native.poll_owned_runtime_stage_handoff(&token).unwrap(),
        Handoff::RetiredOriginal { original: first }
    );
    assert!(matches!(
        native.native_stage_owner_token(native.key(), fixture.identity.pin(), first),
        Err(ServiceFailure::RequestRetired)
    ));
    let joined = tokio::spawn(owners.finish());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !joined.is_finished(),
        "the first drain error cannot abandon the second original"
    );
    clock.0.store(30_010, Ordering::SeqCst);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), joined)
            .await
            .unwrap()
            .unwrap(),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        native.restore_runtime_stage(second.request).unwrap(),
        second
    );
    assert_eq!(
        native.inspect_install_request(second.request).unwrap_err(),
        ServiceFailure::UnknownRequest
    );
    let registration = native.discover().unwrap().unwrap();
    assert_eq!(registration.desired_state(), DesiredServerState::Running);
    assert!(registration.staged_installation().is_none());
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_owner_token_rejects_forwarded_scope_foreign_provider_and_replaced_native_object() {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let pin = fixture.identity.pin();
    let token = native
        .native_stage_owner_token(native.key(), pin, original)
        .unwrap();
    let mut forwarded = original;
    forwarded.capture_digest = RequestDigest::from_bytes([81; 32]);
    assert!(matches!(
        native.native_stage_owner_token(native.key(), pin, forwarded),
        Err(ServiceFailure::RequestConflict)
    ));
    forwarded = original;
    forwarded.artifact_sha256 = [82; 32];
    assert!(matches!(
        native.native_stage_owner_token(native.key(), pin, forwarded),
        Err(ServiceFailure::RequestConflict)
    ));
    forwarded = original;
    forwarded.request = native.allocate_request().unwrap();
    assert!(matches!(
        native.native_stage_owner_token(native.key(), pin, forwarded),
        Err(ServiceFailure::RequestConflict)
    ));
    let foreign_root = tempfile::tempdir().unwrap();
    let foreign = stage_supervisor(foreign_root.path(), &publisher, &manager, &clock);
    assert_eq!(
        foreign.poll_owned_runtime_stage_handoff(&token),
        Err(ServiceFailure::IdentityMismatch)
    );
    let account =
        AccountServiceFixture::new(fixture._root.path(), BuildIdentity::Development).unwrap();
    let retained = account
        .anchor()
        .with_file_name("retained-stage-owner-old-anchor");
    fs::rename(account.anchor(), &retained).unwrap();
    fs::create_dir(account.anchor()).unwrap();
    assert_eq!(
        native.poll_owned_runtime_stage_handoff(&token),
        Err(ServiceFailure::UnsafeStorage)
    );
    fs::remove_dir(account.anchor()).unwrap();
    fs::rename(&retained, account.anchor()).unwrap();
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}

#[tokio::test]
async fn stage_owned_retirement_cannot_clear_uncertain_publication_or_mint_from_it() {
    let fixture = Fixture::new_registered(true).await;
    let publisher = Publisher::new();
    publisher.provision(&fixture.registration);
    let manager = Arc::new(Manager::default());
    let clock = Clock::new();
    let native = stage_supervisor(fixture._root.path(), &publisher, &manager, &clock);
    let artifact = publisher.artifact("1.2.4");
    let original = capture(&native, &artifact);
    let pin = fixture.identity.pin();
    let token = native
        .native_stage_owner_token(native.key(), pin, original)
        .unwrap();
    for _ in 0..255 {
        native.allocate_request().unwrap();
    }
    spaceterm_host::account_service::test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        native.allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert!(matches!(
        native.native_stage_owner_token(native.key(), pin, original),
        Err(ServiceFailure::Indeterminate)
    ));
    assert!(matches!(
        native.poll_owned_runtime_stage_handoff(&token),
        Err(ServiceFailure::Indeterminate | ServiceFailure::RequestRetired)
    ));
    assert!(matches!(
        native.discover(),
        Err(ServiceFailure::Indeterminate)
    ));
    assert_eq!(manager.effects.load(Ordering::SeqCst), 0);
    close_fixture(fixture).await;
}
