use super::*;
use crate::credentials::adapter::NativeKeyStore;
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalLimits, ReceiptVerifier},
    pairing::{
        bootstrap::{BuildChannel, ForegroundBootstrap},
        credentials::{CredentialScope, NativeCredentialAccess},
        enrollment::PairingTransport,
    },
};
use spaceterm_core::{operation_ledger::canonical::CanonicalCodec, pairing::SignedPairingReceipt};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::ReceiptCodec};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use zeroize::Zeroizing;

struct Verify;
impl ReceiptVerifier for Verify {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}
struct Native {
    journal: Arc<NativeClientJournal>,
    entries: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
    writes: AtomicUsize,
    locked: AtomicBool,
}
impl NativeKeyStore for Native {
    fn read<'a>(
        &'a self,
        scope: &'a CredentialScope,
        _: NativeCredentialAccess,
    ) -> PairingFuture<'a, Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>> {
        Box::pin(async move {
            if self.locked.load(Ordering::Acquire) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(self
                .entries
                .lock()
                .unwrap()
                .get(&DeviceCredentials::locator(scope))
                .cloned())
        })
    }
    fn insert<'a>(
        &'a self,
        scope: &'a CredentialScope,
        secret: &'a [u8],
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            if self.locked.load(Ordering::Acquire) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            let recovered = JournalPairingStore::new(Arc::clone(&self.journal))
                .recover()
                .await?;
            assert!(
                recovered
                    .credential_preparations
                    .iter()
                    .any(|p| p.scope == *scope)
            );
            let mut entries = self.entries.lock().unwrap();
            let name = DeviceCredentials::locator(scope);
            if entries.contains_key(&name) {
                return Err(PairingFailure::CredentialIntegrity);
            }
            entries.insert(name, Zeroizing::new(secret.to_vec()));
            self.writes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    }
    fn remove<'a>(
        &'a self,
        _: &'a CredentialScope,
        _: &'a [u8],
    ) -> PairingFuture<'a, Result<bool, PairingFailure>> {
        Box::pin(async { Err(PairingFailure::AuthorizationDenied) })
    }
}
#[derive(Clone)]
struct Connector;
impl EnrollmentConnector for Connector {
    fn open<'a>(
        &'a self,
        _: ServerPin,
        _: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { Err(PairingFailure::Offline) })
    }
}
struct Owner(Arc<AtomicUsize>);
impl BootstrapOwnerExchange for Owner {
    fn lookup<'a>(
        &'a self,
        _: &'a BootstrapServer,
        _: [u8; 16],
        _: &'a EnrollmentProposal,
    ) -> PairingFuture<'a, Result<BootstrapOwnerDelivery, PairingFailure>> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Err(PairingFailure::Offline) })
    }
    fn connect<'a>(
        &'a self,
        _: &'a BootstrapServer,
        _: &'a SavedPairing,
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Err(PairingFailure::Offline) })
    }
}
fn route() -> BootstrapRoute {
    BootstrapRoute::capture([1; 32], [2; 32], [3; 32], 501, BuildChannel::Development).unwrap()
}
fn server() -> BootstrapServer {
    BootstrapServer {
        route: route(),
        pin: ServerPin {
            identity: [4; 16],
            public_key: [5; 32],
        },
        registration_generation: 1,
        approved_artifact_identity: [6; 32],
        protocol_majors: [1, 2],
        running: true,
    }
}
fn provider(
    journal: Arc<NativeClientJournal>,
    native: Arc<Native>,
    owner: Arc<AtomicUsize>,
) -> NativeBootstrapEnrollment<Connector, Owner> {
    NativeBootstrapEnrollment::new(
        journal,
        DeviceCredentials::with_store(native),
        Connector,
        Owner(owner),
        CredentialNamespace::new("dev.spaceterm.bootstrap-test".into()).unwrap(),
        route(),
        RevocationRouteReference::new([7; 16]),
    )
    .unwrap()
}
fn private_root() -> tempfile::TempDir {
    let parent = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&parent).unwrap();
    tempfile::Builder::new()
        .prefix("bootstrap-enrollment-")
        .tempdir_in(parent)
        .unwrap()
}
async fn journal(path: &std::path::Path, create: bool) -> Arc<NativeClientJournal> {
    if create {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    let result = if create {
        NativeClientJournal::create(
            path.to_owned(),
            Arc::new(CanonicalCodec),
            Arc::new(Verify),
            JournalLimits::default(),
        )
        .await
    } else {
        NativeClientJournal::open_existing(
            path.to_owned(),
            Arc::new(CanonicalCodec),
            Arc::new(Verify),
            JournalLimits::default(),
        )
        .await
    };
    Arc::new(result.unwrap())
}

#[tokio::test]
async fn bootstrap_native_owner_proposal_survives_journal_and_provider_restart() {
    let root = private_root();
    let path = root.path().join("client");
    let first_journal = journal(&path, true).await;
    let native = Arc::new(Native {
        journal: Arc::clone(&first_journal),
        entries: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        locked: AtomicBool::new(false),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let first = provider(
        Arc::clone(&first_journal),
        Arc::clone(&native),
        Arc::clone(&calls),
    );
    let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
    let attempt = intent.cancellation();
    first.preflight(route(), &attempt).await.unwrap();
    let original = first.prepare_owner(&server(), &attempt).await.unwrap();
    drop(first);
    let reopened = provider(
        journal(&path, false).await,
        Arc::clone(&native),
        Arc::clone(&calls),
    );
    reopened.preflight(route(), &attempt).await.unwrap();
    assert_eq!(
        reopened
            .prepare_owner(&server(), &attempt)
            .await
            .unwrap()
            .parts()
            .claim_nonce,
        original.parts().claim_nonce
    );
    assert_eq!(
        reopened
            .prepare_owner(&server(), &attempt)
            .await
            .unwrap()
            .parts()
            .keys,
        original.parts().keys
    );
    assert_eq!(native.writes.load(Ordering::Acquire), 2);
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn bootstrap_locked_native_custody_fails_before_owner_or_remote_install() {
    let root = private_root();
    let journal = journal(&root.path().join("client"), true).await;
    let native = Arc::new(Native {
        journal: Arc::clone(&journal),
        entries: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        locked: AtomicBool::new(true),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = provider(journal, Arc::clone(&native), Arc::clone(&calls));
    let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
    let failure = provider
        .preflight(route(), &intent.cancellation())
        .await
        .unwrap_err();
    assert_eq!(
        failure.class,
        spaceterm_client::pairing::bootstrap::BootstrapFailureClass::Pairing(
            PairingFailure::CredentialUnavailable
        )
    );
    assert_eq!(native.writes.load(Ordering::Acquire), 0);
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn bootstrap_cancelled_attempt_and_unverified_binding_cannot_connect() {
    let root = private_root();
    let journal = journal(&root.path().join("client"), true).await;
    let native = Arc::new(Native {
        journal: Arc::clone(&journal),
        entries: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        locked: AtomicBool::new(false),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = provider(journal, Arc::clone(&native), Arc::clone(&calls));
    let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
    let attempt = intent.cancellation();
    assert!(provider.connect(&server(), &attempt).await.is_err());
    attempt.cancel();
    assert!(provider.preflight(route(), &attempt).await.is_err());
    assert_eq!(native.writes.load(Ordering::Acquire), 0);
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn bootstrap_lost_enrollment_ack_blocks_connect_and_resumes_original_ack() {
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use spaceterm_client::pairing::{
        enrollment::verify_binding,
        journal::{
            EnrollmentAcknowledgementWork, PairingJournalChange, PairingJournalTransaction,
            require_durable,
        },
    };
    use spaceterm_core::{
        authority::{ServerEpoch, StoreRevision},
        identity::*,
        pairing::*,
    };
    use spaceterm_protocol::{
        ProtocolError,
        authentication::SigningKey,
        pairing_adapter::{EnrollmentAuthorization, reservation_to_wire},
    };
    struct Signer(Ed25519KeyPair);
    impl SigningKey for Signer {
        fn public_key(&self) -> [u8; 32] {
            self.0.public_key().as_ref().try_into().unwrap()
        }
        fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
            Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
        }
    }
    #[derive(Clone)]
    struct AckConnector(Arc<AtomicUsize>);
    impl EnrollmentConnector for AckConnector {
        fn open<'a>(
            &'a self,
            _: ServerPin,
            _: RevocationRouteReference,
        ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Err(PairingFailure::Offline) })
        }
    }
    struct ConnectOwner(Arc<AtomicUsize>);
    impl BootstrapOwnerExchange for ConnectOwner {
        fn lookup<'a>(
            &'a self,
            _: &'a BootstrapServer,
            _: [u8; 16],
            _: &'a EnrollmentProposal,
        ) -> PairingFuture<'a, Result<BootstrapOwnerDelivery, PairingFailure>> {
            Box::pin(async { panic!("acknowledgement recovery must not enroll again") })
        }
        fn connect<'a>(
            &'a self,
            _: &'a BootstrapServer,
            _: &'a SavedPairing,
        ) -> PairingFuture<'a, Result<(), PairingFailure>> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }
    }
    let root = private_root();
    let journal = journal(&root.path().join("client"), true).await;
    let native = Arc::new(Native {
        journal: journal.clone(),
        entries: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        locked: AtomicBool::new(false),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let acknowledgements = Arc::new(AtomicUsize::new(0));
    let provider = NativeBootstrapEnrollment::new(
        journal,
        DeviceCredentials::with_store(native),
        AckConnector(acknowledgements.clone()),
        ConnectOwner(calls.clone()),
        CredentialNamespace::new("dev.spaceterm.bootstrap-test".into()).unwrap(),
        route(),
        RevocationRouteReference::new([7; 16]),
    )
    .unwrap();
    let signer = Signer(Ed25519KeyPair::from_seed_unchecked(&[81; 32]).unwrap());
    let mut server = server();
    server.pin.public_key = signer.public_key();
    let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
    let attempt = intent.cancellation();
    let (generation, device, credential, proposal) = provider.candidate(&server).await.unwrap();
    let id = ServerId::new(server.pin.identity);
    let pairing = PairingId::new(id, 1).unwrap();
    let client = ClientId::new(id, 1).unwrap();
    let stream = |local| {
        OperationStreamId::new(OperationStreamKey::new(id, local).unwrap(), pairing, client)
            .unwrap()
    };
    let enrollment = OperationBinding {
        id: OperationId::new(stream(1), OperationSequence::new(1).unwrap()),
        digest: RequestDigest::from_bytes([9; 32]),
    };
    let target = PairingTarget::new(PairingTargetParts {
        server: id,
        device: proposal.parts().device,
        pairing,
    })
    .unwrap();
    let authorization = EnrollmentAuthorization::generate().unwrap();
    let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server: id,
            nonce: [8; 16],
        })
        .unwrap(),
        target: target.clone(),
        keys: proposal.parts().keys.clone(),
        initial_client: client,
        enrollment_stream: enrollment.id.stream(),
        claim_digest: enrollment_proposal_digest(&proposal).unwrap(),
        authorization_digest: authorization.digest(),
        enrollment_request_digest: enrollment.digest,
        issuer: EnrollmentIssuer::AccountOwner {
            native_request: [6; 16],
            native_request_digest: RequestDigest::from_bytes([7; 32]),
        },
        issued_epoch: ServerEpoch::new(1),
        issued_revision: StoreRevision::new(1),
        issued_at_millis: 0,
        expires_at_millis: 1000,
        state: EnrollmentReservationState::Reserved,
    })
    .unwrap();
    let prepared = PreparedEnrollment {
        local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
            server: id,
            pairing,
            generation,
        })
        .unwrap(),
        device: proposal.parts().device,
        pin: server.pin,
        keys: proposal.parts().keys.clone(),
        client,
        credential,
        route: provider.reference,
        enrollment,
        reservation: Some(
            PublicEnrollmentReservation::from_wire(&reservation_to_wire(&reservation)).unwrap(),
        ),
    };
    let receipt = ReceiptCodec::new(server.pin)
        .sign(
            PairingReceiptBody::Enrollment {
                target,
                keys: proposal.parts().keys.clone(),
                client: PairingClientStreams::new(PairingClientStreamsParts {
                    client: ClientRecord {
                        id: client,
                        pairing,
                    },
                    ordinary: stream(2),
                    self_revocation: stream(3),
                })
                .unwrap(),
                enrollment,
                revision: StoreRevision::new(1),
            },
            &signer,
        )
        .unwrap();
    let saved = verify_binding(&prepared, receipt.clone(), false).unwrap();
    let controller = spaceterm_client::journal::JournalController::recover(
        provider.journal.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(Verify),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    controller
        .transact(vec![
            spaceterm_client::journal::ClientJournalChange::BindCredentialRoute {
                reference: saved.credential().scope.reference(),
                route_reference: provider.reference,
                route: spaceterm_client::api::RouteDescription::new(
                    spaceterm_client::api::RouteParts::Ipc {
                        directory: "/trusted/bootstrap".into(),
                        name: "server".into(),
                    },
                )
                .unwrap(),
            },
        ])
        .await
        .unwrap();
    let store = provider.store();
    for change in [
        PairingJournalChange::PrepareEnrollment(Box::new(prepared.clone())),
        PairingJournalChange::SaveEnrollment {
            saved: Box::new(saved.clone()),
            acknowledgement: Box::new(EnrollmentAcknowledgementWork {
                prepared,
                device,
                receipt,
            }),
        },
    ] {
        let expected_revision = store.recover().await.unwrap().revision;
        require_durable(
            store
                .commit(&PairingJournalTransaction {
                    expected_revision,
                    change,
                })
                .await,
            expected_revision,
        )
        .unwrap();
    }
    provider
        .connections
        .lock()
        .unwrap()
        .push((attempt.clone(), saved.key().clone()));
    assert!(
        provider.connect(&server, &attempt).await.is_err(),
        "a durable receipt with an unsettled acknowledgement is not ordinary authority"
    );
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert_eq!(
        acknowledgements.load(Ordering::Acquire),
        1,
        "connect resumes the exact pending acknowledgement"
    );
    let pending = store.recover().await.unwrap();
    assert_eq!(pending.enrollment_acknowledgements.len(), 1);
    assert_eq!(
        pending.enrollment_acknowledgements[0].prepared.enrollment,
        enrollment
    );
    let expected_revision = pending.revision;
    require_durable(
        store
            .commit(&PairingJournalTransaction {
                expected_revision,
                change: PairingJournalChange::FinishEnrollmentAcknowledgement {
                    operation: enrollment.id,
                },
            })
            .await,
        expected_revision,
    )
    .unwrap();
    provider.connect(&server, &attempt).await.unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(acknowledgements.load(Ordering::Acquire), 1);
}
