use super::Key;
use spaceterm_core::{
    authority::*,
    identity::*,
    operation_ledger::{canonical::CanonicalCodec, model::*},
    operation_ledger_contracts::*,
    operations::*,
    organization::ServerDirectory,
    pairing::{
        policy::{EnrollmentMutation, PairingPolicy, PairingSnapshot},
        transition::PairingTransition,
        *,
    },
    storage::*,
};
use spaceterm_host::{
    organization_store::{bootstrap::*, *},
    startup::{AccountBuildKey, BuildIdentity},
    storage::Database,
};
use spaceterm_protocol::{
    authentication::SigningKey,
    pairing_adapter::{EnrollmentAuthorization, ReceiptCodec},
};
use spaceterm_server::{
    adapters::core::CommitCursor, adapters::store::production_services,
    pairing::store_projection::append_pairing_writes,
};
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        let root = PathBuf::from("/private/tmp");
        // /var/tmp is persistent temporary storage, separate from build output
        // on the Linux test host. /tmp may be tmpfs and is not admitted by SQLite.
        #[cfg(target_os = "linux")]
        let root = PathBuf::from("/var/tmp");
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = root.join(format!(
            "spaceterm-pairing-{:032x}",
            u128::from_be_bytes(nonce)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn identity() -> StoreIdentity {
    StoreIdentity {
        server: ServerId::new([11; 16]),
        account: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        server_public_key: Key::new(71).public_key(),
    }
}
pub fn services() -> StoreServices {
    production_services(
        identity(),
        ServerDirectory::new(identity().server, b"/fixture-home".to_vec()).unwrap(),
    )
    .unwrap()
}
pub fn reopen(path: &Path) -> Result<SqliteOrganizationStore, StoreFailure> {
    let db = Database::open_existing_for_application(path, migrations(), schema::APPLICATION_ID)
        .map_err(|_| StoreFailure::Unavailable)?;
    SqliteOrganizationStore::open_existing(db, identity(), StoreLimits::production(), services())
}

pub struct Fixture {
    pub store: SqliteOrganizationStore,
    pub ledger: OperationLedger,
}
impl Fixture {
    pub fn new(path: &Path) -> Self {
        let database =
            Database::open_for_application(path, migrations(), schema::APPLICATION_ID).unwrap();
        let mut store = match SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity: identity(),
                origin: NewServerOrigin::LocalHeadless,
            },
            StoreLimits::production(),
            services(),
        ) {
            InitializationDisposition::Durable { store, .. } => store,
            value => panic!("initialization failed: {value:?}"),
        };
        let mut parts = CommitCursor::from_recovered(&store.recover().unwrap())
            .parts()
            .unwrap();
        parts.resulting_epoch = ServerEpoch::new(1);
        let transaction = DurableTransaction::new_with_validation(parts, &services()).unwrap();
        assert!(matches!(
            store.commit(&transaction),
            CommitDisposition::Durable(_)
        ));
        let ledger =
            OperationLedger::recover(identity().server, store.recover().unwrap().ledger.unwrap())
                .unwrap();
        Self { store, ledger }
    }
    pub fn recovered(&mut self) -> RecoveredState {
        self.store.recover().unwrap()
    }
    pub fn snapshot(&mut self) -> PairingSnapshot {
        let r = self.recovered();
        PairingSnapshot::new(
            r.organization.server,
            r.last_epoch,
            r.store_revision,
            r.high_water,
            r.pairing.unwrap(),
        )
        .unwrap()
    }
    pub fn context(&mut self) -> TransactionContext {
        CommitCursor::from_recovered(&self.recovered())
            .context()
            .unwrap()
    }
    pub fn parts(&mut self) -> TransactionParts {
        CommitCursor::from_recovered(&self.recovered())
            .parts()
            .unwrap()
    }
    pub fn transaction(
        &mut self,
        prepared: &PreparedLedgerTransition,
        transition: Option<&PairingTransition>,
    ) -> DurableTransaction {
        let mut parts = self.parts();
        if let Some(transition) = transition {
            parts.resulting_high_water = transition.high_water;
            append_pairing_writes(&mut parts, transition).unwrap();
        }
        for write in prepared.projection().writes() {
            if let LedgerWrite::CompleteOperation {
                basis: CompletionRevisionBasis::OwnedReduction(value),
                ..
            } = write
            {
                parts
                    .operation_writes
                    .push(OperationWrite::ReductionBasis(*value));
            }
        }
        prepared.projection().append_to(&mut parts).unwrap();
        DurableTransaction::new_with_validation(parts, &services()).unwrap()
    }
    pub fn commit(
        &mut self,
        prepared: PreparedLedgerTransition,
        transition: Option<&PairingTransition>,
    ) {
        let tx = self.transaction(&prepared, transition);
        let disposition = self.store.commit(&tx);
        assert!(
            matches!(disposition, CommitDisposition::Durable(_)),
            "commit failed: {disposition:?}"
        );
        self.ledger.resolve_commit(prepared, disposition).unwrap();
    }
    pub fn reserve(&mut self, device: u8) -> (EnrollmentReservation, EnrollmentAuthorization) {
        let state = self.snapshot();
        let server = state.server();
        let marks = state.high_water();
        let pairing = PairingId::new(server, marks.pairing + 1).unwrap();
        let client = ClientId::new(server, marks.client + 1).unwrap();
        let stream = OperationStreamId::new(
            OperationStreamKey::new(server, marks.operation_stream + 1).unwrap(),
            pairing,
            client,
        )
        .unwrap();
        let ticket = EnrollmentTicketId::new(EnrollmentTicketParts {
            server,
            nonce: [device; 16],
        })
        .unwrap();
        let claim_digest = RequestDigest::from_bytes([device; 32]);
        let command = Command::Pairing(PairingCommand::Enroll {
            ticket: ticket.clone(),
            claim_digest,
        });
        let request = CanonicalOperationRequest::new(
            OperationId::new(stream, OperationSequence::new(1).unwrap()),
            command,
            &CanonicalCodec,
        )
        .unwrap();
        let secret = EnrollmentAuthorization::generate().unwrap();
        // The uncontrolled owner boundary is fixture input, not a production credential.
        let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
            ticket,
            target: PairingTarget::new(PairingTargetParts {
                server,
                pairing,
                device: DeviceId::new([device; 16]),
            })
            .unwrap(),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: Key::new(device).public_key(),
                pairing_authentication_public_key: Key::new(device + 1).public_key(),
            })
            .unwrap(),
            initial_client: client,
            enrollment_stream: stream,
            claim_digest,
            authorization_digest: secret.digest(),
            enrollment_request_digest: request.binding().digest,
            issuer: EnrollmentIssuer::AccountOwner {
                native_request: [7; 16],
                native_request_digest: RequestDigest::from_bytes([9; 32]),
            },
            issued_epoch: state.epoch(),
            issued_revision: state.revision().checked_successor().unwrap(),
            issued_at_millis: 1000,
            expires_at_millis: 301000,
            state: EnrollmentReservationState::Reserved,
        })
        .unwrap();
        let transition = PairingPolicy::default()
            .prepare_reservation(&state, reservation.clone())
            .unwrap();
        let context = self.context();
        let prepared = self
            .ledger
            .prepare_register_stream_with_purpose(
                StreamBinding::from_stream(stream),
                StreamPurpose::Enrollment { claim_digest },
                context,
            )
            .unwrap();
        self.commit(prepared, Some(&transition));
        (reservation, secret)
    }
    pub fn enroll_request(reservation: &EnrollmentReservation) -> CanonicalOperationRequest {
        let e = reservation.parts();
        CanonicalOperationRequest::new(
            OperationId::new(e.enrollment_stream, OperationSequence::new(1).unwrap()),
            Command::Pairing(PairingCommand::Enroll {
                ticket: e.ticket.clone(),
                claim_digest: e.claim_digest,
            }),
            &CanonicalCodec,
        )
        .unwrap()
    }
    pub fn admit(&mut self, request: &CanonicalOperationRequest) {
        let context = self.context();
        let AdmissionPreparation::Prepared(prepared) = self
            .ledger
            .prepare_admission(
                &StreamBinding::from_stream(request.binding().id.stream()),
                request.binding().id,
                request.command(),
                PendingKind::AtomicMutation,
                context,
            )
            .unwrap()
        else {
            panic!("not new admission")
        };
        self.commit(prepared, None);
    }
    pub fn streams(&mut self, pairing: PairingId, client: ClientId) -> PairingClientStreams {
        let marks = self.recovered().high_water;
        let stream = |value| {
            OperationStreamId::new(
                OperationStreamKey::new(pairing.server(), value).unwrap(),
                pairing,
                client,
            )
            .unwrap()
        };
        PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client,
                pairing,
            },
            ordinary: stream(marks.operation_stream + 1),
            self_revocation: stream(marks.operation_stream + 2),
        })
        .unwrap()
    }
    pub fn prepare_enrollment(
        &mut self,
        reservation: &EnrollmentReservation,
        secret: &EnrollmentAuthorization,
    ) -> (
        PreparedLedgerTransition,
        PairingTransition,
        SignedPairingReceipt,
    ) {
        let request = Self::enroll_request(reservation);
        let state = self.snapshot();
        let e = reservation.parts();
        let client = self.streams(e.target.parts().pairing, e.initial_client);
        let key = Key::new(71);
        let receipt = ReceiptCodec::new(key.pin())
            .sign(
                PairingReceiptBody::Enrollment {
                    target: e.target.clone(),
                    keys: e.keys.clone(),
                    client: client.clone(),
                    enrollment: request.binding(),
                    revision: state.revision().checked_successor().unwrap(),
                },
                &key,
            )
            .unwrap();
        let transition = PairingPolicy::default()
            .prepare_enrollment(
                &state,
                EnrollmentMutation {
                    ticket: e.ticket.clone(),
                    enrollment: request.binding(),
                    claim_digest: e.claim_digest,
                    authorization_digest: secret.digest(),
                    now_millis: 1001,
                    client,
                    receipt: receipt.clone(),
                },
            )
            .unwrap();
        let prepared = self.completion(request.binding(), &transition);
        (prepared, transition, receipt)
    }
    pub fn completion(
        &mut self,
        binding: OperationBinding,
        transition: &PairingTransition,
    ) -> PreparedLedgerTransition {
        let r = self.recovered();
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding,
            base: r.organization.revision,
            resulting: r.organization.revision,
        })
        .unwrap();
        let context = TransactionContext::new(
            r.store_revision,
            r.organization.revision,
            r.organization.revision,
            Some(basis),
        )
        .unwrap();
        let outcome = Outcome::Pairing(Box::new(transition.outcome.clone()));
        let mut prepared = self
            .ledger
            .prepare_completion(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                binding.digest,
                CompactOutcome::new(&outcome, &CanonicalCodec).unwrap(),
                r.organization.revision,
                context,
            )
            .unwrap();
        // Retired restricted headers still permit receipt lookup and cumulative
        // acknowledgement. They cannot admit another self-revocation operation.
        let retired_pairings: Vec<_> = transition
            .writes
            .parts()
            .writes
            .iter()
            .filter_map(|write| match write {
                PairingWrite::PutAuthorization { value, .. }
                    if matches!(value.parts().record.state, PairingState::Revoked { .. }) =>
                {
                    Some(value.parts().record.id)
                }
                _ => None,
            })
            .collect();
        let additions = transition
            .register_streams
            .iter()
            .cloned()
            .map(LedgerWrite::RegisterStream)
            .chain(
                r.ledger
                    .as_ref()
                    .unwrap()
                    .streams
                    .iter()
                    .filter(|header| {
                        retired_pairings.contains(&header.binding.pairing())
                            && header.status == StreamStatus::Active
                    })
                    .map(|header| LedgerWrite::RetireStream {
                        binding: header.binding,
                        reason: StreamRetirementReason::PairingRevoked,
                    }),
            )
            .collect();
        self.ledger
            .extend_prepared(&mut prepared, additions)
            .unwrap();
        prepared
    }
    pub fn enroll(&mut self, device: u8) -> (EnrollmentReservation, SignedPairingReceipt) {
        let (reservation, secret) = self.reserve(device);
        self.admit(&Self::enroll_request(&reservation));
        let (prepared, transition, receipt) = self.prepare_enrollment(&reservation, &secret);
        self.commit(prepared, Some(&transition));
        (reservation, receipt)
    }
    pub fn prepare_revocation(
        &mut self,
        request: &CanonicalOperationRequest,
        target: &PairingTarget,
    ) -> (
        PreparedLedgerTransition,
        PairingTransition,
        SignedPairingReceipt,
    ) {
        let state = self.snapshot();
        let a = state.authorization(target.parts().pairing).unwrap().parts();
        let key = Key::new(71);
        let proof = ReceiptCodec::new(key.pin())
            .sign(
                PairingReceiptBody::Revocation {
                    receipt: RevocationReceipt {
                        server: state.server(),
                        pairing: target.parts().pairing,
                        device: target.parts().device,
                        operation: request.binding().id,
                        revision: state.revision().checked_successor().unwrap(),
                        receipt_digest: RequestDigest::from_bytes([0; 32]),
                    },
                    authentication_key_digest:
                        spaceterm_core::pairing::policy::authentication_key_digest_for(&a.keys),
                    revocation_event: [29; 16],
                },
                &key,
            )
            .unwrap();
        let transition = PairingPolicy::default()
            .prepare_self_revocation(&state, target, request.binding(), proof.clone())
            .unwrap();
        let prepared = self.completion(request.binding(), &transition);
        (prepared, transition, proof)
    }
    pub fn revoke_request(receipt: &SignedPairingReceipt) -> CanonicalOperationRequest {
        let PairingReceiptBody::Enrollment { target, client, .. } = &receipt.parts().claims else {
            panic!("not enrollment")
        };
        CanonicalOperationRequest::new(
            OperationId::new(
                client.parts().self_revocation,
                OperationSequence::new(1).unwrap(),
            ),
            Command::Pairing(PairingCommand::SelfRevoke {
                target: target.clone(),
            }),
            &CanonicalCodec,
        )
        .unwrap()
    }
    pub fn acknowledge(&mut self, binding: OperationBinding) {
        let context = self.context();
        let prepared = self
            .ledger
            .prepare_acknowledgement(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id.sequence().get(),
                context,
            )
            .unwrap()
            .unwrap();
        self.commit(prepared, None);
    }
}
