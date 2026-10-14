//! Exact interoperability with the native runtime's public claim DTO.
use super::*;
use spaceterm_client::pairing::enrollment::{
    DirectEnrollmentAttempt, EnrollmentAttemptRequest, fingerprint,
};
use spaceterm_client_native::{
    cli::{
        arguments::PublicFingerprint,
        bindings::{UnavailableNativeBindings, UnavailableProtocolManagement},
        runner::{CliDependencies, Dispatch, dispatch},
    },
    pairing_runtime::PublicDeviceClaim,
};
use spaceterm_core::operations::{OperationBinding, OperationId, OperationSequence};
use spaceterm_protocol::pairing_adapter::EnrollmentAuthorization;
use std::os::unix::fs::DirBuilderExt as _;
use zeroize::Zeroizing;

fn public_fingerprint(key: [u8; 32]) -> PublicFingerprint {
    let invocation = parse(
        [
            "--non-interactive",
            "pair",
            "authorize",
            "--claim-stdin",
            "--authorization-stdout",
            "--device-fingerprint",
            &hex(&key),
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    )
    .unwrap();
    let spaceterm_client_native::cli::CliCommand::PairAuthorizeClaim { fingerprint } =
        invocation.command
    else {
        panic!("expected public claim command")
    };
    fingerprint
}

#[derive(Clone, Copy)]
enum Case {
    CliEnrollment,
    InvalidInput,
    LostDelivery,
    MultipleClaims,
    Capacity,
    ObservedAdmission,
    PublicationRecovery(bool),
}

#[tokio::test]
async fn prepared_public_claim_json_enrolls_through_cli_and_callers_own_driver_in_both_majors() {
    for major in [1, 2] {
        exercise_public_dto(major, Case::CliEnrollment).await;
    }
}

#[tokio::test]
async fn prepared_public_claim_rejects_schema_pins_fingerprint_and_both_proofs_without_effects() {
    for major in [1, 2] {
        exercise_public_dto(major, Case::InvalidInput).await;
    }
}

#[tokio::test]
async fn prepared_public_claim_lost_delivery_keeps_original_request_after_reconstruction() {
    for major in [1, 2] {
        exercise_public_dto(major, Case::LostDelivery).await;
    }
}

#[tokio::test]
async fn distinct_prepared_public_claims_retain_each_original_request_after_newer_issuance() {
    for major in [1, 2] {
        exercise_public_dto(major, Case::MultipleClaims).await;
    }
}

#[tokio::test]
async fn public_claim_history_refuses_capacity_without_evicting_or_reallocating_original_requests()
{
    for major in [1, 2] {
        exercise_public_dto(major, Case::Capacity).await;
    }
}

#[tokio::test]
async fn actual_public_claim_records_original_request_before_owner_channel_await_in_both_majors() {
    for major in [1, 2] {
        exercise_public_dto(major, Case::ObservedAdmission).await;
    }
}

#[tokio::test]
async fn public_claim_authorization_recovers_original_metadata_publication_before_recovery_guard() {
    for major in [1, 2] {
        for unsynced in [false, true] {
            exercise_public_dto(major, Case::PublicationRecovery(unsynced)).await;
        }
    }
}

struct PausedOwner(Arc<tokio::sync::Notify>);
impl spaceterm_client_native::native::owner_channel::OwnerChannelProvider for PausedOwner {
    fn open<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_client::ports::ClientFuture<
        'a,
        Arc<dyn spaceterm_host::transport::MessageTransport>,
    > {
        Box::pin(async move {
            self.0.notify_one();
            std::future::pending().await
        })
    }
}

struct UnavailableOwner;
impl spaceterm_client_native::native::owner_channel::OwnerChannelProvider for UnavailableOwner {
    fn open<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_client::ports::ClientFuture<
        'a,
        Arc<dyn spaceterm_host::transport::MessageTransport>,
    > {
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::Unavailable)) })
    }
}

async fn exercise_public_dto(major: u32, case: Case) {
    let root = tempfile::Builder::new()
        .prefix("st-exact-public-claim-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Running,
            Some(InstallationStatus {
                installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                artifact_sha256: [8; 32],
            }),
        )
        .unwrap();
    let pin = registration.server_pin();
    let server_id = ServerId::new(pin.identity);
    let identity = StoreIdentity {
        server: server_id,
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let home = spaceterm_core::organization::ServerDirectory::new(
        server_id,
        root.path().as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless
            },
            StoreLimits::production(),
            production_services(identity, home).unwrap(),
        ),
        InitializationDisposition::Durable { .. }
    ));
    let supervisor = supervisor(root.path(), Arc::new(StartupAbsence)).unwrap();
    let server = RecoveredAccountServer::recover(
        supervisor.clone(),
        AccountStartup::admit(&supervisor).unwrap(),
        AccountLaunchFacts {
            uid: registration.key().account_uid,
            home: root.path().to_owned(),
            planner: ShellLaunchPlanner::for_test(
                root.path().join("shell"),
                root.path().to_owned(),
            ),
            machine: Default::default(),
        },
    )
    .await
    .unwrap();
    let handle = server.handle();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run_until(async {
        let _ = stopped.await;
    }));

    let directory = root
        .path()
        .canonicalize()
        .unwrap()
        .join("independent-caller");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = NativeClientJournal::create(
        directory,
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let journal = Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    );
    let credentials = Arc::new(Credentials::new());
    let local = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
    let route = local.description().clone();
    let connector = Arc::new(
        spaceterm_client_native::restricted_routes::NativePairingConnector::new(
            journal.clone(),
            NativeRouteConnector::new(vec![local], None).unwrap(),
        ),
    );
    let runtime = NativePairingRuntime::with_adapters(
        journal.clone(),
        credentials.clone(),
        connector.clone(),
        connector,
        Arc::new(NoInvalidation),
        HandshakeConfig {
            majors: vec![major],
            ..HandshakeConfig::default()
        },
    );
    let namespace = CredentialNamespace::new("spaceterm-native-development".into()).unwrap();
    let claim = runtime
        .prepare_public_claim(
            namespace.clone(),
            pin,
            route.clone(),
            &fingerprint(pin.public_key),
        )
        .await
        .unwrap();
    let encoded = serde_json::to_vec(&claim).unwrap();
    let keys = serde_json::from_slice::<serde_json::Value>(&encoded).unwrap();
    assert!(keys.as_object().unwrap().len() == 6 && keys.get("request").is_none());
    let expected = public_fingerprint(
        claim
            .proposal
            .parts()
            .keys
            .parts()
            .device_identity_public_key,
    );
    let provider = NativeClientProvider::from_supervisor((*supervisor).clone());
    let producer = Arc::new(
        NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(ForbiddenCredentials)),
    );

    match case {
        Case::CliEnrollment => {
            let invocation = parse(
                [
                    "--non-interactive",
                    "pair",
                    "authorize",
                    "--claim-stdin",
                    "--authorization-stdout",
                    "--device-fingerprint",
                    &hex(&expected.bytes()),
                ]
                .into_iter()
                .map(std::ffi::OsString::from),
            )
            .unwrap();
            let dependencies = CliDependencies::new(
                Ok(registration.key()),
                Arc::new(UnavailableProtocolManagement),
                Arc::new(UnavailableNativeBindings::default()),
                producer,
            );
            let mut io = ClaimPipeIo {
                claim: Some(Zeroizing::new(encoded)),
                delivery: None,
                ordinary_writes: 0,
                diagnostics: Vec::new(),
            };
            let result = dispatch(invocation, &dependencies, &mut io).await;
            assert!(matches!(result, Dispatch::Exit(code) if code.code() == 0));
            assert!(io.ordinary_writes == 0 && io.diagnostics.is_empty());
            let (reservation, authorization) =
                EnrollmentAuthorization::from_delivery(io.delivery.take().unwrap()).unwrap();
            let inventory = runtime.inventory().await.unwrap();
            let device = inventory
                .credential_preparations
                .iter()
                .find(|item| item.scope.parts().purpose == CredentialPurpose::DeviceIdentity)
                .unwrap()
                .credential
                .as_ref()
                .unwrap();
            let authentication = inventory
                .credential_preparations
                .iter()
                .find(|item| {
                    item.scope.parts().purpose
                        == CredentialPurpose::PairingAuthentication { server: server_id }
                })
                .unwrap();
            let generation = authentication.generation;
            let authentication = authentication.credential.as_ref().unwrap();
            let reference = journal
                .snapshot()
                .unwrap()
                .pairing()
                .slot(authentication.scope.reference())
                .unwrap()
                .route_reference
                .unwrap();
            let public = reservation.parts();
            let prepared = spaceterm_client::pairing::journal::PreparedEnrollment {
                local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                    server: server_id,
                    pairing: public.target.parts().pairing,
                    generation,
                })
                .unwrap(),
                device: public.target.parts().device,
                pin,
                keys: public.keys.clone(),
                client: public.initial_client,
                credential: authentication.clone(),
                route: reference,
                enrollment: OperationBinding {
                    id: OperationId::new(
                        public.enrollment_stream,
                        OperationSequence::new(1).unwrap(),
                    ),
                    digest: public.enrollment_request_digest,
                },
                reservation: Some(reservation.clone()),
            };
            let attempt = DirectEnrollmentAttempt::new(pin, reference, generation.get()).unwrap();
            let verified = attempt
                .confirm(
                    pin,
                    reference,
                    generation.get(),
                    &fingerprint(pin.public_key),
                )
                .unwrap();
            let saved = runtime
                .enroll(
                    EnrollmentAttemptRequest {
                        verified: &verified,
                        prepared: &prepared,
                        reservation,
                        device,
                        authorization: Some(&authorization),
                        majors: vec![major],
                    },
                    route,
                )
                .await
                .unwrap();
            assert!(
                saved.key() == &prepared.local_pairing
                    && journal.snapshot().unwrap().bindings().len() == 1
            );
        }
        Case::InvalidInput => {
            // The local request journal is observed as opaque bytes, never printed.
            let requests = supervisor
                .client_connections_directory()
                .parent()
                .unwrap()
                .join("requests");
            let before = std::fs::read(&requests).ok();
            for field in [
                "schema",
                "server_identity",
                "server_public_key",
                "device_proof",
                "authentication_proof",
                "proposal",
            ] {
                let mut invalid = serde_json::from_slice::<serde_json::Value>(&encoded).unwrap();
                if field == "schema" {
                    invalid[field] = serde_json::json!(2);
                } else if field == "proposal" {
                    // A coherent foreign identity with valid possession proofs
                    // must still fail the registered local server pin comparison.
                    let mut parts = claim.proposal.parts().clone();
                    parts.server = ServerId::new([99; 16]);
                    let foreign = EnrollmentProposal::new(parts).unwrap();
                    let (device, authentication) =
                        spaceterm_protocol::pairing_adapter::VerifiedDeviceClaim::proofs(
                            &foreign,
                            credentials.device.as_ref(),
                            credentials.authentication.as_ref(),
                        )
                        .unwrap();
                    invalid["server_identity"] = serde_json::to_value([99u8; 16]).unwrap();
                    invalid[field] = serde_json::to_value(foreign).unwrap();
                    invalid["device_proof"] = serde_json::to_value(device.to_vec()).unwrap();
                    invalid["authentication_proof"] =
                        serde_json::to_value(authentication.to_vec()).unwrap();
                } else {
                    let byte = invalid[field][0].as_u64().unwrap();
                    invalid[field][0] = serde_json::json!(byte ^ 1);
                }
                let result = producer
                    .authorize_claim(
                        Zeroizing::new(serde_json::to_vec(&invalid).unwrap()),
                        expected,
                    )
                    .await;
                assert!(
                    matches!(result, Err(error) if error.failure == ManagementFailure::AuthorizationDenied && error.request.is_none())
                );
                assert!(!supervisor.client_connections_directory().exists());
                assert!(std::fs::read(&requests).ok() == before);
            }
            let result = producer
                .authorize_claim(Zeroizing::new(encoded), public_fingerprint([99; 32]))
                .await;
            assert!(
                matches!(result, Err(error) if error.failure == ManagementFailure::AuthorizationDenied && error.request.is_none())
            );
            assert!(!supervisor.client_connections_directory().exists());
            assert!(std::fs::read(&requests).ok() == before);
            assert!(
                handle
                    .pairing_snapshot()
                    .unwrap()
                    .recovery()
                    .parts()
                    .enrollments
                    .is_empty()
            );
        }
        Case::ObservedAdmission | Case::PublicationRecovery(_) => {
            use spaceterm_client_native::cli::bindings::PairingAuthorizationAdmission;
            let opened = Arc::new(tokio::sync::Notify::new());
            let paused = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                .with_credentials(Arc::new(ForbiddenCredentials))
                .with_owner_channels(Arc::new(PausedOwner(opened.clone())));
            let observer = PairingAuthorizationAdmission::default();
            let mut authorizing = paused.authorize_claim_admitted(
                Zeroizing::new(encoded.clone()),
                expected,
                observer.clone(),
            );
            tokio::time::timeout(Duration::from_secs(15), async {
                tokio::select! {
                    () = opened.notified() => (),
                    _ = &mut authorizing => panic!("authorization must pause at owner channel admission"),
                }
            }).await.unwrap();
            let original = observer
                .request()
                .expect("actual allocator request must be visible before await");
            let retained = supervisor.admit_client_provisioning();
            assert!(
                retained.is_err(),
                "pending authorization must hold its provisioning lease"
            );
            drop(authorizing);
            if let Case::PublicationRecovery(unsynced) = case {
                use spaceterm_host::account_service::{ServiceFailure, test_support};
                let admission = supervisor.admit_client_provisioning().unwrap();
                let history = admission.owner_attempt("public-claims").unwrap().unwrap();
                if unsynced {
                    test_support::fail_next_publication_durability();
                } else {
                    test_support::fail_next_publication_before_effect();
                }
                assert_eq!(
                    admission.retain_owner_attempt("public-claims", Some(&history), &history),
                    Err(ServiceFailure::Indeterminate)
                );
                drop(admission);
                assert_eq!(
                    supervisor.retained_account_recovery(),
                    Err(ServiceFailure::Indeterminate)
                );
            }
            let unavailable = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                .with_credentials(Arc::new(ForbiddenCredentials))
                .with_owner_channels(Arc::new(UnavailableOwner));
            let replay_observer = PairingAuthorizationAdmission::default();
            let failed = unavailable
                .authorize_claim_admitted(
                    Zeroizing::new(encoded),
                    expected,
                    replay_observer.clone(),
                )
                .await;
            assert!(
                matches!(failed, Err(error) if error.failure == ManagementFailure::Offline
                && error.request.as_deref().copied() == Some(original))
            );
            assert!(replay_observer.request() == Some(original));
            assert!(
                handle
                    .pairing_snapshot()
                    .unwrap()
                    .recovery()
                    .parts()
                    .enrollments
                    .is_empty()
            );
        }
        Case::LostDelivery | Case::MultipleClaims | Case::Capacity => {
            let lossy = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                .with_credentials(Arc::new(ForbiddenCredentials))
                .with_owner_channels(Arc::new(LoseDelivery(
                    spaceterm_client_native::native::owner_channel::NativeOwnerChannel(
                        provider.management(),
                    ),
                )));
            let original = lossy
                .authorize_claim(Zeroizing::new(encoded.clone()), expected)
                .await;
            let original = match original {
                Err(error) => error,
                Ok(_) => panic!("expected lost delivery"),
            };
            assert!(original.failure == ManagementFailure::Offline && original.request.is_some());
            if matches!(case, Case::MultipleClaims) {
                // Native key custody and journal stay with the caller. A second
                // nonce is another deliberate claim, not a replacement delivery.
                let second: PublicDeviceClaim = runtime
                    .prepare_public_claim(
                        namespace.clone(),
                        pin,
                        route.clone(),
                        &fingerprint(pin.public_key),
                    )
                    .await
                    .unwrap();
                assert!(second.proposal.parts().claim_nonce != claim.proposal.parts().claim_nonce);
                let second = producer
                    .authorize_claim(
                        Zeroizing::new(serde_json::to_vec(&second).unwrap()),
                        expected,
                    )
                    .await
                    .unwrap();
                assert!(Some(Box::new(second.request())) != original.request);
                drop(second);
            }
            if matches!(case, Case::Capacity) {
                let unavailable =
                    NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                        .with_credentials(Arc::new(ForbiddenCredentials))
                        .with_owner_channels(Arc::new(UnavailableOwner));
                let mut originals = vec![original.request.clone()];
                // Fill the bounded admission history through genuine allocator
                // calls. The uncontrolled endpoint fails before bearer issuance.
                for _ in 1..64 {
                    let candidate = runtime
                        .prepare_public_claim(
                            namespace.clone(),
                            pin,
                            route.clone(),
                            &fingerprint(pin.public_key),
                        )
                        .await
                        .unwrap();
                    let result = unavailable
                        .authorize_claim(
                            Zeroizing::new(serde_json::to_vec(&candidate).unwrap()),
                            expected,
                        )
                        .await;
                    let failed = match result {
                        Err(error) => error,
                        Ok(_) => panic!("endpoint should be offline"),
                    };
                    assert!(
                        failed.failure == ManagementFailure::Offline && failed.request.is_some()
                    );
                    assert!(!originals.contains(&failed.request));
                    originals.push(failed.request);
                }
                let candidate = runtime
                    .prepare_public_claim(namespace, pin, route, &fingerprint(pin.public_key))
                    .await
                    .unwrap();
                let requests = supervisor
                    .client_connections_directory()
                    .parent()
                    .unwrap()
                    .join("requests");
                let before = std::fs::read(&requests).unwrap();
                let result = unavailable
                    .authorize_claim(
                        Zeroizing::new(serde_json::to_vec(&candidate).unwrap()),
                        expected,
                    )
                    .await;
                assert!(
                    matches!(result, Err(error) if error.failure == ManagementFailure::LimitExceeded && error.request.is_none())
                );
                assert!(std::fs::read(&requests).unwrap() == before);
            }
            let reconstructed = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                .with_credentials(Arc::new(ForbiddenCredentials));
            let retry = reconstructed
                .authorize_claim(Zeroizing::new(encoded), expected)
                .await;
            assert!(
                matches!(retry, Err(error) if error.failure == ManagementFailure::OutcomePending && error.request == original.request)
            );
            assert!(
                handle
                    .pairing_snapshot()
                    .unwrap()
                    .recovery()
                    .parts()
                    .enrollments
                    .len()
                    == if matches!(case, Case::MultipleClaims) {
                        2
                    } else {
                        1
                    }
            );
        }
    }
    assert!(credentials.creates.load(Ordering::SeqCst) == 2);
    let _ = stop.send(());
    let report = tokio::time::timeout(Duration::from_secs(15), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.cleanup_confirmed && report.listeners_closed && !report.recovery_required);
}
