//! Public SSH producer tests over the real paired account worker and signed
//! native journal. The active flow uses NativeServiceProcesses; only the external
//! service manager is supplied by the existing isolated account fixture.

use super::*;
use spaceterm_host::account_service::{
    NativeLifecycleRequest, ServiceOutcome, ServicePhase as NativePhase, ServiceProgress,
    ServiceRequestState, ServiceTime,
    remote_wire::{NativeLifecycleAccountReply as Reply, NativeLifecycleAccountRequest as Request},
    runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply},
    test_support::AccountServiceFixture,
};
use spaceterm_server::account_native_lifecycle::{NativeLifecycleOriginalOwners, handle};

#[tokio::test]
async fn wrong_account_build_or_pin_is_refused_for_every_verb_before_native_effects() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    // No manager or runtime is present. An attempted external observation or
    // native effect would fail differently, rather than passing this identity gate.
    let supervisor = fixture.reopen();
    let request = supervisor.allocate_request().unwrap();
    let original = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let progress = ServiceProgress {
        request,
        request_digest: RequestDigest::from_bytes([9; 32]),
        phase: NativePhase::Completed,
        state: ServiceRequestState::Completed(ServiceOutcome::Stopped),
    };
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    let mut owners = NativeLifecycleOriginalOwners::new(Arc::new(supervisor.clone()));
    let key = registration.key();
    let pin = registration.server_pin();
    let mut wrong_uid = key;
    wrong_uid.account_uid += 1;
    let mut wrong_build = key;
    wrong_build.build_identity = BuildIdentity::Preflight;
    let mut wrong_identity = pin;
    wrong_identity.identity = [21; 16];
    let mut wrong_public_key = pin;
    wrong_public_key.public_key = [22; 32];
    for (expected_key, expected_pin) in [
        (wrong_uid, pin),
        (wrong_build, pin),
        (key, wrong_identity),
        (key, wrong_public_key),
    ] {
        let requests = [
            Request::Observe {
                expected_key,
                expected_pin,
            },
            Request::Reserve {
                expected_key,
                expected_pin,
            },
            Request::Capture {
                expected_key,
                expected_pin,
                request,
                action: ServiceAction::Stop,
            },
            Request::Restore {
                expected_key,
                expected_pin,
                original,
                assessment: None,
            },
            Request::Resolve {
                expected_key,
                expected_pin,
                request,
            },
            Request::Acknowledge {
                expected_key,
                expected_pin,
                progress: progress.clone(),
            },
        ];
        for request in requests {
            assert_eq!(
                handle(&supervisor, &request).await,
                Reply::Refused(ServiceFailure::IdentityMismatch)
            );
            assert_eq!(
                owners.handle(&request).await,
                Reply::Refused(ServiceFailure::IdentityMismatch)
            );
        }
    }
    owners.finish().await.unwrap();
    assert!(
        fs::read(fixture.anchor().join("requests")).unwrap() == before,
        "native journal changed",
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().generation(),
        registration.generation()
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
}

async fn paired_call(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    call: AccountRuntimeCall,
) -> AccountRuntimeReply {
    let payload = native_lifecycle_payload(2, call);
    transport
        .send(&codec.encode(&payload).unwrap())
        .await
        .unwrap();
    // Readiness/publication traffic can precede the correlated lifecycle reply.
    // It is not evidence of lifecycle admission and must not replace the reply.
    for _ in 0..64 {
        let reply = codec
            .decode(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        if let Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeReply(reply)),
        }) = reply
            && reply.request == 31
            && reply.request_generation == 7
        {
            return serde_json::from_slice(&reply.body).unwrap();
        }
    }
    panic!("bounded paired lifecycle reply was not received");
}

async fn reserve_and_capture(
    supervisor: &spaceterm_host::account_service::ServiceSupervisor,
    owners: &mut NativeLifecycleOriginalOwners,
    action: ServiceAction,
) -> NativeLifecycleRequest {
    let registration = supervisor.discover().unwrap().unwrap();
    let expected_key = registration.key();
    let expected_pin = registration.server_pin();
    let Reply::Reserved(request) = handle(
        supervisor,
        &Request::Reserve {
            expected_key,
            expected_pin,
        },
    )
    .await
    else {
        panic!("guest reservation");
    };
    assert_eq!(
        handle(
            supervisor,
            &Request::Resolve {
                expected_key,
                expected_pin,
                request
            }
        )
        .await,
        Reply::Refused(ServiceFailure::Pending),
    );
    let capture_request = Request::Capture {
        expected_key,
        expected_pin,
        request,
        action,
    };
    let Reply::Captured(original) = owners.handle(&capture_request).await else {
        panic!("guest capture");
    };
    // The first response may be lost. The retry uses the same ID and exact action.
    assert_eq!(
        owners.handle(&capture_request).await,
        Reply::Captured(original)
    );
    assert_eq!(
        handle(
            supervisor,
            &Request::Capture {
                expected_key,
                expected_pin,
                request,
                action: ServiceAction::Restart
            }
        )
        .await,
        Reply::Refused(ServiceFailure::RequestConflict),
    );
    assert_eq!(
        owners
            .handle(&Request::Restore {
                expected_key,
                expected_pin,
                original,
                assessment: None,
            })
            .await,
        Reply::Restored {
            original,
            assessment: None
        },
    );
    original
}

#[tokio::test]
async fn public_native_consumer_preserves_signed_original_across_capture_effect_and_ack_reply_loss()
{
    original_native_flow(ServiceAction::Stop, ServiceOutcome::Stopped).await;
}

#[tokio::test]
async fn paired_uninstall_preserves_native_identity_and_exact_original_terminal_recovery() {
    original_native_flow(ServiceAction::Uninstall, ServiceOutcome::Uninstalled).await;
}

async fn original_native_flow(action: ServiceAction, expected_outcome: ServiceOutcome) {
    tokio::time::timeout(Duration::from_secs(45), async {
        let seed = Seed::new().await;
        let supervisor = test_support::retained_supervisor(seed.root.path()).unwrap();
        // The signed native instance owns an OS process, not merely an actor.
        // An in-process actor leaves this test process live and cannot supply
        // exact native-root absence after Stop or Uninstall.
        let mut binary = Binary::start(&seed);
        let transport = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if binary.0.try_wait().unwrap().is_some() {
                    panic!("native fixture process exited before listener readiness");
                }
                if let Ok(transport) = IpcTransport::connect(
                    &seed.registration.endpoint().unwrap(),
                    MessageLimit::envelope(),
                )
                .await
                {
                    break transport;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut codec = lifecycle_handshake(&transport, &seed).await;
        let expected_key = supervisor.key();
        let expected_pin = seed.identity.pin();
        let mut owners = NativeLifecycleOriginalOwners::new(supervisor.clone());
        let original = reserve_and_capture(&supervisor, &mut owners, action).await;
        let observed = handle(
            &supervisor,
            &Request::Observe {
                expected_key,
                expected_pin,
            },
        )
        .await;
        let Reply::Observed(observed) = observed else {
            panic!("native observation");
        };
        let AccountRuntimeReply::NativeLifecycleAssessment(reference) = paired_call(
            &transport,
            &mut codec,
            AccountRuntimeCall::AssessNativeLifecycle(original),
        )
        .await
        else {
            panic!("actual paired assessment");
        };
        assert_eq!(reference.original, original);
        assert_eq!(reference.affected_sessions, 0);
        // The native owner can observe the signed Reserved record. It cannot
        // promote it, refresh its expiry, or supply an assessment of its own.
        let resolve = Request::Resolve {
            expected_key,
            expected_pin,
            request: original.request,
        };
        let Reply::Progress(reserved) = handle(&supervisor, &resolve).await else {
            panic!("reserved progress");
        };
        assert_eq!(reserved.phase, NativePhase::Reserved);
        assert_eq!(reserved.state, ServiceRequestState::Pending);
        assert_eq!(reserved.request_digest, reference.request_digest);
        assert_eq!(
            supervisor.discover().unwrap().unwrap().generation(),
            observed.registration_generation.get()
        );
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );

        // Reconstruct with the real native boot clock. The original fixture
        // assessment belongs to a different boot, so it is expired for admission.
        // Restoration remains a comparison and returns every original field.
        let fixture =
            AccountServiceFixture::new(seed.root.path(), BuildIdentity::Development).unwrap();
        let reconstructed = fixture.reopen();
        assert_ne!(
            reference.expires_at.boot,
            ServiceTime::current().unwrap().boot
        );
        let restore = Request::Restore {
            expected_key,
            expected_pin,
            original,
            assessment: Some(reference.clone()),
        };
        assert_eq!(
            handle(&reconstructed, &restore).await,
            Reply::Restored {
                original,
                assessment: Some(reference.clone())
            },
        );
        assert_eq!(
            handle(
                &reconstructed,
                &Request::Capture {
                    expected_key,
                    expected_pin,
                    request: original.request,
                    action: original.action
                }
            )
            .await,
            Reply::Captured(original),
        );
        let mut changed = reference.clone();
        changed.expires_at.millis += 1;
        assert_eq!(
            handle(
                &reconstructed,
                &Request::Restore {
                    expected_key,
                    expected_pin,
                    original,
                    assessment: Some(changed)
                }
            )
            .await,
            Reply::Refused(ServiceFailure::RequestConflict),
        );
        // Send the actual paired admission, but never read its reply. Native
        // progress is only the barrier proving the reply can now be lost.
        transport
            .send(
                &codec
                    .encode(&native_lifecycle_payload(
                        2,
                        AccountRuntimeCall::AdmitNativeLifecycle(reference.clone()),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let admitted = loop {
            match supervisor.request_progress(original.request) {
                Ok(progress) if progress.phase != NativePhase::Reserved => break progress,
                Ok(_) | Err(ServiceFailure::StorageLocked | ServiceFailure::Pending) => {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(classification) => panic!("native admission refused: {classification:?}"),
            }
        };
        assert_eq!(admitted.request, original.request);
        assert_eq!(admitted.request_digest, reference.request_digest);
        transport.close().await.unwrap();
        // EOF joins this outside-server owner without a new ordinary handshake
        // or Client key. The fixture is the child's real parent, so it must reap
        // concurrently: Linux retains a zombie's exact signed PID/birth until
        // wait, and native absence must continue refusing that retained root.
        let (finished, exit) = tokio::join!(owners.finish(), binary.wait());
        finished.unwrap();
        assert!(exit.success());
        let Reply::Progress(completed) = handle(&supervisor, &resolve).await else {
            panic!("original owner did not retain exact terminal progress");
        };
        assert_eq!(completed.phase, NativePhase::Completed);
        assert_eq!(completed.request_digest, reference.request_digest);
        assert_eq!(
            completed.state,
            ServiceRequestState::Completed(expected_outcome)
        );
        let reopened = test_support::retained_supervisor(seed.root.path()).unwrap();
        // Discarding the first native effect response cannot replace the result.
        assert_eq!(
            handle(&reopened, &resolve).await,
            Reply::Progress(completed.clone())
        );
        assert_eq!(
            reopened.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Stopped
        );

        let mut changed = completed.clone();
        changed.request_digest = RequestDigest::from_bytes([23; 32]);
        assert_eq!(
            handle(
                &reopened,
                &Request::Acknowledge {
                    expected_key,
                    expected_pin,
                    progress: changed
                }
            )
            .await,
            Reply::Refused(ServiceFailure::RequestConflict),
        );
        let mut changed = completed.clone();
        changed.state = ServiceRequestState::Completed(ServiceOutcome::Restarted);
        assert_eq!(
            handle(
                &reopened,
                &Request::Acknowledge {
                    expected_key,
                    expected_pin,
                    progress: changed
                }
            )
            .await,
            Reply::Refused(ServiceFailure::RequestConflict),
        );
        assert_eq!(
            handle(&reopened, &resolve).await,
            Reply::Progress(completed.clone())
        );
        let ack = Request::Acknowledge {
            expected_key,
            expected_pin,
            progress: completed.clone(),
        };
        assert_eq!(
            handle(&reopened, &ack).await,
            Reply::Acknowledged {
                progress: completed.clone()
            }
        );
        // A lost ACK response retries the exact native receipt, not a new ID.
        assert_eq!(
            handle(&fixture.reopen(), &ack).await,
            Reply::Acknowledged {
                progress: completed
            }
        );
        assert_eq!(
            handle(&reopened, &resolve).await,
            Reply::Refused(ServiceFailure::RequestRetired)
        );
        assert_eq!(
            handle(&reopened, &restore).await,
            Reply::Refused(ServiceFailure::RequestRetired)
        );
        assert!(reopened.retained_service_requests().unwrap().is_empty());
        assert!(!seed.registration.endpoint().unwrap().path().exists());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn owner_bound_original_has_no_signed_lifecycle_progress_and_cannot_be_promoted() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let supervisor = fixture.reopen();
    let request = supervisor.allocate_request().unwrap();
    let expected_key = registration.key();
    let expected_pin = registration.server_pin();
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server: ServerId::new(expected_pin.identity),
        device: DeviceId::new([31; 16]),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [32; 32],
            pairing_authentication_public_key: [33; 32],
        })
        .unwrap(),
        claim_nonce: [34; 32],
    })
    .unwrap();
    let owner = spaceterm_server::account_startup::AccountOwnerEnrollment::new(Arc::new(
        supervisor.clone(),
    ));
    owner.record_authorization(request, &proposal).unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        handle(
            &supervisor,
            &Request::Resolve {
                expected_key,
                expected_pin,
                request
            }
        )
        .await,
        Reply::Refused(ServiceFailure::Pending),
    );
    assert!(
        fs::read(fixture.anchor().join("requests")).unwrap() == before,
        "native journal changed",
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().generation(),
        registration.generation()
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
}

async fn lifecycle_handshake(
    transport: &dyn MessageTransport,
    seed: &Seed,
) -> spaceterm_protocol::connection::AuthenticatedConnection {
    use spaceterm_protocol::negotiation::{
        CAPABILITY_ACCOUNT_RUNTIME, CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
    };
    let (client, hello) = authentication::ClientHandshake::begin(
        seed.identity.pin(),
        seed.principal,
        authentication::Purpose::Ordinary,
        authentication::HandshakeConfig {
            majors: vec![2],
            capabilities: vec![
                CAPABILITY_CANONICAL_DOMAIN,
                CAPABILITY_ACCOUNT_RUNTIME,
                CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
            ],
            ..Default::default()
        },
        seed.key.clone(),
    )
    .unwrap();
    transport.send(&hello).await.unwrap();
    let (client, proof) = client
        .receive_server(&transport.recv().await.unwrap().unwrap())
        .unwrap();
    transport.send(&proof).await.unwrap();
    let authentication::Established::Ordinary(codec) = client
        .receive_ready(&transport.recv().await.unwrap().unwrap())
        .unwrap()
    else {
        panic!("paired lifecycle handshake");
    };
    assert!(codec.capabilities().contains(&CAPABILITY_ACCOUNT_RUNTIME));
    assert!(
        codec
            .capabilities()
            .contains(&CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
    );
    codec
}

#[tokio::test]
async fn unsigned_owner_on_eof_waits_for_native_expiry_and_preserves_original_comparison() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let seed = Seed::new().await;
        let issued = ServiceTime::current().unwrap();
        test_support::set_retained_clock(seed.root.path(), issued).unwrap();
        let supervisor = test_support::retained_supervisor(seed.root.path()).unwrap();
        let mut binary = Binary::start(&seed);
        let transport = connect_lifecycle(&mut binary, &seed).await;
        // No paired assessment or admission is made. The running binary and
        // outside-server helper read the same fixture-owned native clock.
        let mut owners = NativeLifecycleOriginalOwners::new(supervisor.clone());
        let original = reserve_and_capture(&supervisor, &mut owners, ServiceAction::Stop).await;
        let fixture =
            AccountServiceFixture::new(seed.root.path(), BuildIdentity::Development).unwrap();
        let before = fs::read(fixture.anchor().join("requests")).unwrap();
        transport.close().await.unwrap();
        let finish = tokio::spawn(owners.finish());
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            !finish.is_finished(),
            "wall time is not native expiry proof"
        );
        test_support::set_retained_clock(
            seed.root.path(),
            ServiceTime {
                boot: issued.boot,
                millis: issued.millis + 30_000,
            },
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), finish)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            supervisor.restore_runtime_request(original.request),
            Ok(original)
        );
        assert_eq!(
            supervisor.restore_runtime_lifecycle_reference(original),
            Err(ServiceFailure::RequestConflict)
        );
        assert!(
            fs::read(fixture.anchor().join("requests")).unwrap() == before,
            "unsigned owner changed the native journal",
        );
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
        assert!(binary.0.try_wait().unwrap().is_none());
        drop(binary);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn revoked_paired_client_cannot_admit_ready_original_and_owner_seals_only_native_expiry() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let fixture = Fixture::new_registered(true).await;
        let issued = ServiceTime::current().unwrap();
        test_support::set_retained_clock(fixture._root.path(), issued).unwrap();
        let supervisor = test_support::retained_supervisor(fixture._root.path()).unwrap();
        let (_, comparison, key) = fixture.enroll(2).await;
        let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
        let actor = fixture.server.handle();
        actor.register(session.principal).await.unwrap();
        let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
            actor.clone(),
            supervisor.as_ref().clone(),
            fixture.identity.clone(),
        )
        .unwrap();
        let mut owners = NativeLifecycleOriginalOwners::new(supervisor.clone());
        let original = reserve_and_capture(&supervisor, &mut owners, ServiceAction::Stop).await;
        let AccountRuntimeReply::NativeLifecycleAssessment(reference) = native_lifecycle_call(
            &runtime,
            2,
            supervisor.key().account_uid,
            &session,
            AccountRuntimeCall::AssessNativeLifecycle(original),
        )
        .await
        else {
            panic!("actual paired assessment");
        };
        actor
            .revoke_clients(vec![session.principal.client()])
            .await
            .unwrap();
        assert_eq!(
            native_lifecycle_call(
                &runtime,
                2,
                supervisor.key().account_uid,
                &session,
                AccountRuntimeCall::AdmitNativeLifecycle(reference.clone()),
            )
            .await,
            AccountRuntimeReply::Failed(ServiceFailure::IdentityMismatch)
        );
        let reserved = supervisor.request_progress(original.request).unwrap();
        assert_eq!(reserved.phase, NativePhase::Reserved);
        assert_eq!(reserved.state, ServiceRequestState::Pending);
        let generation = supervisor.discover().unwrap().unwrap().generation();
        let finish = tokio::spawn(owners.finish());
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            !finish.is_finished(),
            "revocation is not original expiry proof"
        );
        test_support::set_retained_clock(fixture._root.path(), reference.expires_at).unwrap();
        tokio::time::timeout(Duration::from_secs(5), finish)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let progress = supervisor.request_progress(original.request).unwrap();
        assert_eq!(progress.request_digest, reference.request_digest);
        assert_eq!(
            progress.state,
            ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment)
        );
        assert_eq!(
            supervisor.restore_runtime_lifecycle_reference(original),
            Ok(reference)
        );
        let registration = supervisor.discover().unwrap().unwrap();
        assert_eq!(registration.generation(), generation);
        assert_eq!(registration.desired_state(), DesiredServerState::Running);
        assert!(actor.assess_account_runtime().await.is_ok());
        actor
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        fixture.server.wait().await.unwrap();
    })
    .await
    .unwrap();
}

async fn connect_lifecycle(binary: &mut Binary, seed: &Seed) -> IpcTransport {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if binary.0.try_wait().unwrap().is_some() {
                panic!("native fixture process exited before listener readiness");
            }
            if let Ok(transport) = IpcTransport::connect(
                &seed.registration.endpoint().unwrap(),
                MessageLimit::envelope(),
            )
            .await
            {
                break transport;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn retired_owner_reports_no_outcome_and_finish_still_joins_other_unexpired_originals() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        fixture.initialize(DesiredServerState::Running).unwrap();
        let issued = ServiceTime::current().unwrap();
        test_support::set_retained_clock(root.path(), issued).unwrap();
        let supervisor = test_support::retained_supervisor(root.path()).unwrap();
        let mut owners = NativeLifecycleOriginalOwners::new(supervisor.clone());
        let first = reserve_and_capture(&supervisor, &mut owners, ServiceAction::Stop).await;
        let second = reserve_and_capture(&supervisor, &mut owners, ServiceAction::Uninstall).await;
        // No await lets the native allocator reclaim the first unsubmitted row
        // before its already-ready owner next polls. The token remains opaque.
        for _ in 0..1024 {
            supervisor.allocate_request().unwrap();
            if supervisor.restore_runtime_request(first.request)
                == Err(ServiceFailure::RequestRetired)
            {
                break;
            }
        }
        assert_eq!(
            supervisor.restore_runtime_request(first.request),
            Err(ServiceFailure::RequestRetired)
        );
        assert_eq!(
            supervisor.restore_runtime_request(second.request),
            Ok(second)
        );
        let finish = tokio::spawn(owners.finish());
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            !finish.is_finished(),
            "retirement did not join the second owner"
        );
        test_support::set_retained_clock(
            root.path(),
            ServiceTime {
                boot: issued.boot,
                millis: issued.millis + 30_000,
            },
        )
        .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), finish)
                .await
                .unwrap()
                .unwrap(),
            Err(ServiceFailure::RequestRetired)
        );
        assert_eq!(
            supervisor.restore_runtime_request(second.request),
            Ok(second)
        );
        assert!(supervisor.retained_service_requests().unwrap().is_empty());
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn allocator_retirement_of_expired_signed_original_drains_only_its_verified_owner() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let fixture = Fixture::new_registered(true).await;
        test_support::set_retained_clock(fixture._root.path(), ServiceTime::current().unwrap())
            .unwrap();
        let supervisor = test_support::retained_supervisor(fixture._root.path()).unwrap();
        let (_, comparison, key) = fixture.enroll(2).await;
        let session = native_authenticated_session(&fixture, comparison, key, 2, 1);
        let actor = fixture.server.handle();
        actor.register(session.principal).await.unwrap();
        let runtime = spaceterm_server::account_runtime::AccountRuntimeProcesses::new(
            actor.clone(),
            supervisor.as_ref().clone(),
            fixture.identity.clone(),
        )
        .unwrap();
        let mut owners = NativeLifecycleOriginalOwners::new(supervisor.clone());
        let original = reserve_and_capture(&supervisor, &mut owners, ServiceAction::Stop).await;
        let AccountRuntimeReply::NativeLifecycleAssessment(reference) = native_lifecycle_call(
            &runtime,
            2,
            supervisor.key().account_uid,
            &session,
            AccountRuntimeCall::AssessNativeLifecycle(original),
        )
        .await
        else {
            panic!("actual paired assessment");
        };
        // Both operations are synchronous on this current-thread test runtime.
        // Allocation retires the expired Reserved row before the owner can seal it.
        test_support::set_retained_clock(fixture._root.path(), reference.expires_at).unwrap();
        supervisor.allocate_request().unwrap();
        assert_eq!(
            supervisor.request_progress(original.request),
            Err(ServiceFailure::RequestRetired)
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), owners.finish())
                .await
                .unwrap(),
            Err(ServiceFailure::RequestRetired)
        );
        assert_eq!(
            supervisor.restore_runtime_lifecycle_reference(original),
            Err(ServiceFailure::RequestRetired)
        );
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
        assert!(actor.assess_account_runtime().await.is_ok());
        actor
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        fixture.server.wait().await.unwrap();
    })
    .await
    .unwrap();
}
