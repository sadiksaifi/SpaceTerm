//! Real protocol authentication with an in-crate fake of the frozen worker port.
//! Actor dispatch, journal durability and major-specific status conversion require
//! the client-core/server-core/pairing implementation handoff, not another server.

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_client::management_cli::*;
use spaceterm_client_native::cli::{bindings::*, output::*, runner::*, *};
use spaceterm_core::{
    authority::*, identity::*, management::*, observations::*, operations::*,
    organization::Versioned, remote_access::RemoteAccessPolicy,
};
use spaceterm_host::startup::*;
use spaceterm_protocol::{ProtocolError, authentication::*, connection::AuthenticatedConnection};
use std::{
    num::NonZeroU64,
    sync::{Arc, Mutex},
};

struct Key(Ed25519KeyPair);
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, data: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(data).as_ref().try_into().unwrap())
    }
}
struct Pairings {
    record: PairingRecord,
}
impl PairingLookup for Pairings {
    fn lookup(
        &self,
        s: [u8; 16],
        p: [u8; 16],
        c: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        if (s, p, c)
            != (
                self.record.principal.server,
                self.record.principal.pairing,
                self.record.principal.client,
            )
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(self.record)
    }
}
fn local(v: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&v.to_be_bytes());
    bytes
}
fn authenticated(
    major: u32,
    active: bool,
    pin_matches: bool,
) -> Result<AuthenticatedConnection, ProtocolError> {
    let server: Arc<dyn SigningKey> =
        Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[51; 32]).unwrap()));
    let device: Arc<dyn SigningKey> =
        Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[52; 32]).unwrap()));
    let principal = Principal {
        server: [1; 16],
        device: [2; 16],
        pairing: local(3),
        client: local(4),
    };
    let pairings = Pairings {
        record: PairingRecord {
            principal,
            device_public_key: device.public_key(),
            active,
        },
    };
    let pin = ServerPin {
        identity: principal.server,
        public_key: if pin_matches {
            server.public_key()
        } else {
            [9; 32]
        },
    };
    let (client, hello) = ClientHandshake::begin(
        pin,
        principal,
        Purpose::Ordinary,
        HandshakeConfig {
            majors: vec![major],
            ..Default::default()
        },
        device,
    )?;
    let (server, reply) = ServerHandshake::accept(
        principal.server,
        7,
        HandshakeConfig::default(),
        server,
        &pairings,
        &hello,
    )?;
    let (client, proof) = client.receive_server(&reply)?;
    let (_, ready) = server.receive_device(&pairings, &proof)?;
    match client.receive_ready(&ready)? {
        Established::Ordinary(c) => Ok(c),
        _ => Err(ProtocolError::Authentication),
    }
}
fn stream() -> OperationStreamId {
    let s = ServerId::new([1; 16]);
    OperationStreamId::new(
        OperationStreamKey::new(s, 5).unwrap(),
        PairingId::new(s, 3).unwrap(),
        ClientId::new(s, 4).unwrap(),
    )
    .unwrap()
}
fn operation(n: u64) -> OperationId {
    OperationId::new(stream(), OperationSequence::new(n).unwrap())
}
fn assessment() -> ManagementAssessment {
    let server = stream().server();
    ManagementAssessment::new(
        AssessmentToken::new("token_1".to_owned()).unwrap(),
        ManagementAction::EndAllSessions { server },
        AssessmentEvidence::EndAllSessions(
            CloseAssessment::new(
                AssessmentRef {
                    operation: operation(1),
                    digest: RequestDigest::from_bytes([1; 32]),
                },
                ServerEpoch::new(7),
                AssessmentSummary::new(AssessmentTarget::EndAllSessions(server), 1, 1, 1, 1, true)
                    .unwrap(),
            )
            .unwrap(),
        ),
        MonotonicTick::new(0),
        MonotonicTick::new(30_000),
    )
    .unwrap()
}
struct Clock;
impl RuntimeClock for Clock {
    fn now(&self) -> Result<MonotonicTick, ObservationError> {
        Ok(MonotonicTick::new(100))
    }
}
struct Backend {
    context: AuthenticatedManagementContext,
    submitted: Arc<Mutex<Vec<Command>>>,
    resolved: Arc<Mutex<Vec<OperationId>>>,
}
fn progress(state: ManagementProgressState) -> ManagementProgress {
    ManagementProgress {
        request: ManagementRequestReference::Operation(OperationBinding {
            id: operation(2),
            digest: RequestDigest::from_bytes([8; 32]),
        }),
        state,
        completed_units: u32::from(state == ManagementProgressState::Completed),
        total_units: 1,
    }
}
impl ManagementBackend for Backend {
    fn context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        Ok(self.context)
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async {
            Ok(ServerStatus::new(ServerStatusParts {
                server: stream().server(),
                epoch: ServerEpoch::new(7),
                revision: NonZeroU64::new(1).unwrap(),
                running: None,
                approved: None,
                staged: None,
                native_verification: NativeVerification::Unverified,
                capabilities: vec![],
                remote_access: Versioned {
                    version: FieldVersion::new(1),
                    value: RemoteAccessPolicy::disabled(stream().server()),
                },
                listeners: vec![],
                progress: vec![],
            })
            .unwrap())
        })
    }
    fn assess(&self, _: ManagementAction) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async { Ok(assessment()) })
    }
    fn restore_assessment(
        &self,
        _: ManagementAction,
        _: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async { Ok(assessment()) })
    }
    fn submit(&self, command: Command) -> ManagementFuture<'_, ManagementProgress> {
        self.submitted.lock().unwrap().push(command);
        Box::pin(async { Ok(progress(ManagementProgressState::WaitingForCleanup)) })
    }
    fn resolve(&self, id: OperationId) -> ManagementFuture<'_, ManagementProgress> {
        self.resolved.lock().unwrap().push(id);
        Box::pin(async { Ok(progress(ManagementProgressState::Completed)) })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async { Err(ManagementFailure::NativeUnavailable) })
    }
}
#[derive(Default)]
struct Io {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    read_terminal: usize,
}
impl CliIo for Io {
    fn write_stdout(&mut self, b: &[u8]) -> Result<(), CliFailure> {
        self.stdout.extend_from_slice(b);
        Ok(())
    }
    fn write_stderr(&mut self, b: &[u8]) -> Result<(), CliFailure> {
        self.stderr.extend_from_slice(b);
        Ok(())
    }
    fn read_confirmation(
        &mut self,
        p: &ConfirmationPrompt,
    ) -> Result<ConfirmationReply, CliFailure> {
        self.read_terminal += 1;
        p.reply(b"confirm")
    }
}
type Trace<T> = Arc<Mutex<Vec<T>>>;
fn dependencies(major: u32) -> (CliDependencies, Trace<Command>, Trace<OperationId>) {
    let context = AuthenticatedManagementContext::from_connection(
        &authenticated(major, true, true).unwrap(),
        stream(),
        ManagementClientKind::Cli,
    )
    .unwrap();
    let submitted = Arc::new(Mutex::new(vec![]));
    let resolved = Arc::new(Mutex::new(vec![]));
    let facade = TrackedProtocolManagement::new(
        Backend {
            context,
            submitted: submitted.clone(),
            resolved: resolved.clone(),
        },
        Arc::new(Clock),
    );
    (
        CliDependencies::new(
            Ok(AccountBuildKey {
                account_uid: 501,
                build_identity: BuildIdentity::Development,
            }),
            Arc::new(facade),
            Arc::new(UnavailableNativeBindings::default()),
            Arc::new(UnavailablePairingBindings),
        ),
        submitted,
        resolved,
    )
}
async fn run(args: &[&str], deps: &CliDependencies) -> (u8, Io) {
    let mut io = Io::default();
    let Dispatch::Exit(exit) = dispatch(
        parse(args.iter().map(std::ffi::OsString::from)).unwrap(),
        deps,
        &mut io,
    )
    .await
    else {
        panic!("daemon")
    };
    (exit.code(), io)
}
#[test]
fn pinned_authentication_rejects_wrong_server_and_revoked_pairing_in_both_majors() {
    for major in [1, 2] {
        assert!(authenticated(major, true, true).is_ok());
        assert!(matches!(
            authenticated(major, true, false),
            Err(ProtocolError::Authentication)
        ));
        assert!(matches!(
            authenticated(major, false, true),
            Err(ProtocolError::PairingDenied)
        ));
    }
}
#[tokio::test]
async fn cli_dispatch_resolves_original_pending_operation_without_reexecution_in_both_majors() {
    for major in [1, 2] {
        let (deps, submitted, resolved) = dependencies(major);
        let (code, io) = run(
            &[
                "--json",
                "--non-interactive",
                "end-all-sessions",
                "--confirm",
                "token_1",
            ],
            &deps,
        )
        .await;
        assert_eq!(code, 75);
        assert_eq!(io.read_terminal, 0);
        let result: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
        let original = result["result"]["request"]["operation"].as_str().unwrap();
        assert_eq!(original, operation_text(operation(2)));
        assert_eq!(*submitted.lock().unwrap(), [assessment().command()]);
        let (code, _) = run(&["status", "--operation", original], &deps).await;
        assert_eq!(code, 0);
        assert_eq!(*resolved.lock().unwrap(), [operation(2)]);
        assert_eq!(submitted.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn assessment_only_and_wrong_confirmation_have_no_effect_in_both_majors() {
    for major in [1, 2] {
        let (deps, submitted, _) = dependencies(major);
        assert_eq!(run(&["end-all-sessions", "--assess"], &deps).await.0, 0);
        assert_eq!(
            run(
                &[
                    "--non-interactive",
                    "end-all-sessions",
                    "--confirm",
                    "different"
                ],
                &deps
            )
            .await
            .0,
            2
        );
        assert!(submitted.lock().unwrap().is_empty());
    }
}

struct RegisteredNative;
impl NativeCommandBindings for RegisteredNative {
    fn inspect(&self, _: AccountBuildKey) -> ManagementFuture<'_, NativeObservation> {
        Box::pin(async {
            Ok(NativeObservation {
                availability: RegistrationAvailability::Registered,
                desired: Some(DesiredServerState::Running),
                process: ProcessObservation::Observed,
                verified: false,
                approved: None,
                staged: None,
            })
        })
    }
    fn run<'a>(
        &'a self,
        _: AccountBuildKey,
        _: NativeCliCommand,
        _: ConfirmationMode,
        _: spaceterm_host::native_management::SudoMode,
        _: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        panic!("status must not invoke native management")
    }
}

#[tokio::test]
async fn cli_status_reports_the_authenticated_negotiated_major_not_an_observed_process_version() {
    for major in [1, 2] {
        let facade = TrackedProtocolManagement::new(
            Backend {
                context: AuthenticatedManagementContext::from_connection(
                    &authenticated(major, true, true).unwrap(),
                    stream(),
                    ManagementClientKind::Cli,
                )
                .unwrap(),
                submitted: Arc::new(Mutex::new(vec![])),
                resolved: Arc::new(Mutex::new(vec![])),
            },
            Arc::new(Clock),
        );
        let deps = CliDependencies::new(
            Ok(AccountBuildKey {
                account_uid: 501,
                build_identity: BuildIdentity::Development,
            }),
            Arc::new(facade),
            Arc::new(RegisteredNative),
            Arc::new(UnavailablePairingBindings),
        );
        let (code, io) = run(&["--json", "status"], &deps).await;
        assert_eq!(code, 0);
        let output: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
        assert_eq!(output["result"]["negotiated_protocol_major"], major);
        assert_eq!(output["result"]["native"]["verified"], false);
    }
}

struct CompanionBackend {
    context: AuthenticatedManagementContext,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}
impl StatusBackend for CompanionBackend {
    fn context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        Ok(self.context)
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(ManagementFailure::Offline) })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(ManagementFailure::Offline) })
    }
}

#[tokio::test]
async fn native_companion_rejects_borrowed_cli_identity_and_reports_missing_setup_before_offline() {
    for major in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let account = spaceterm_host::account_service::test_support::AccountServiceFixture::new(
            root.path(),
            BuildIdentity::Development,
        )
        .unwrap();
        let provider =
            spaceterm_client_native::NativeClientProvider::from_supervisor(account.reopen());
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = |kind| CompanionBackend {
            context: AuthenticatedManagementContext::from_connection(
                &authenticated(major, true, true).unwrap(),
                stream(),
                kind,
            )
            .unwrap(),
            reads: reads.clone(),
        };
        assert!(matches!(
            provider.clone().with_companion_backend(
                companion_saved(4, true),
                &ReceiptVerifier,
                backend(ManagementClientKind::Cli)
            ),
            Err(ManagementFailure::AuthorizationDenied)
        ));
        for saved in [companion_saved(4, false), companion_saved(7, true)] {
            assert!(matches!(
                provider.clone().with_companion_backend(
                    saved,
                    &ReceiptVerifier,
                    backend(ManagementClientKind::StatusCompanion)
                ),
                Err(ManagementFailure::AuthorizationDenied)
            ));
        }
        let provider = provider
            .with_companion_backend(
                companion_saved(4, true),
                &ReceiptVerifier,
                backend(ManagementClientKind::StatusCompanion),
            )
            .unwrap();
        let status = provider.companion_status().unwrap();
        assert_eq!(
            status.status().await,
            Err(ManagementFailure::NativeUnavailable)
        );
        assert!(matches!(
            status.subscribe_status().await,
            Err(ManagementFailure::NativeUnavailable)
        ));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(!account.anchor().exists());
    }
}

struct ReceiptVerifier;
impl spaceterm_client::journal::ReceiptVerifier for ReceiptVerifier {
    fn verify(
        &self,
        pin: ServerPin,
        receipt: &spaceterm_core::pairing::SignedPairingReceipt,
    ) -> Result<(), spaceterm_client::failure::ClientFailure> {
        let failure = || {
            spaceterm_client::failure::ClientFailure::new(
                spaceterm_client::failure::ClientFailureClass::InvalidPeer,
            )
        };
        let canonical = serde_json::to_vec(&receipt.parts().claims).map_err(|_| failure())?;
        if canonical != receipt.parts().canonical_body {
            return Err(failure());
        }
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, pin.public_key)
            .verify(&canonical, &receipt.parts().signature)
            .map_err(|_| failure())
    }
}
fn companion_saved(client_local: u64, additional: bool) -> spaceterm_client::journal::SavedBinding {
    use spaceterm_client::api::{
        ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts,
    };
    use spaceterm_core::authority::StoreRevision;
    use spaceterm_core::pairing::*;
    let issuer = Ed25519KeyPair::from_seed_unchecked(&[51; 32]).unwrap();
    let original = stream();
    let client = ClientId::new(original.server(), client_local).unwrap();
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing: original.pairing(),
        },
        ordinary: OperationStreamId::new(original.key(), original.pairing(), client).unwrap(),
        self_revocation: OperationStreamId::new(
            OperationStreamKey::new(original.server(), 6).unwrap(),
            original.pairing(),
            client,
        )
        .unwrap(),
    })
    .unwrap();
    let pin = ServerPin {
        identity: original.server().bytes(),
        public_key: issuer.public_key().as_ref().try_into().unwrap(),
    };
    let device = DeviceId::new([2; 16]);
    let target = ConnectionTarget::new(
        pin,
        device,
        original.pairing(),
        client,
        RouteDescription::new(RouteParts::Ipc {
            directory: "/private/local".into(),
            name: "s".into(),
        })
        .unwrap(),
        CredentialReference::new([4; 16]),
        LocalPairingGeneration::new(1).unwrap(),
    )
    .unwrap();
    let pairing = PairingTarget::new(PairingTargetParts {
        server: original.server(),
        device,
        pairing: original.pairing(),
    })
    .unwrap();
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [5; 32],
        pairing_authentication_public_key: [6; 32],
    })
    .unwrap();
    let operation = OperationBinding {
        id: OperationId::new(
            OperationStreamId::new(
                OperationStreamKey::new(original.server(), 8).unwrap(),
                original.pairing(),
                client,
            )
            .unwrap(),
            OperationSequence::new(1).unwrap(),
        ),
        digest: RequestDigest::from_bytes([7; 32]),
    };
    let claims = if additional {
        PairingReceiptBody::ClientBinding {
            target: pairing,
            keys,
            client: streams.clone(),
            operation,
            revision: StoreRevision::new(1),
        }
    } else {
        PairingReceiptBody::Enrollment {
            target: pairing,
            keys,
            client: streams.clone(),
            enrollment: operation,
            revision: StoreRevision::new(1),
        }
    };
    let canonical = serde_json::to_vec(&claims).unwrap();
    let signature = issuer.sign(&canonical).as_ref().to_vec();
    let receipt = SignedPairingReceipt::new(SignedPairingReceiptParts {
        schema: 1,
        claims,
        canonical_body: canonical,
        signature,
    })
    .unwrap();
    spaceterm_client::journal::SavedBinding::verified(
        PairedServerBinding::new(target),
        streams,
        receipt,
        &ReceiptVerifier,
    )
    .unwrap()
}
