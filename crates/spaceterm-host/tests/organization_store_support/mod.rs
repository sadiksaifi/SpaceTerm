//! Real VFS uncertainty and original-identity recovery through the public store.
//! Test-only composition through real authority, ledger and auxiliary SQL.

use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger::canonical::CanonicalCodec;
use spaceterm_core::operation_ledger::model::*;
use spaceterm_core::operation_ledger_contracts::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::storage::*;
use spaceterm_host::organization_store::{bootstrap::*, *};
use spaceterm_host::startup::{AccountBuildKey, BuildIdentity};
use spaceterm_host::storage::{Database, testing::TestControl};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;

pub fn identity() -> StoreIdentity {
    StoreIdentity {
        server: ServerId::new([11; 16]),
        account: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        server_public_key: signing_key().public_key().as_ref().try_into().unwrap(),
    }
}
pub fn private_directory() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}
pub fn reopen(directory: &std::path::Path) -> SqliteOrganizationStore {
    SqliteOrganizationStore::open_existing_with_services(
        Database::open_existing_for_application(directory, migrations(), schema::APPLICATION_ID)
            .unwrap(),
        identity(),
        StoreLimits::production(),
        services(),
    )
    .unwrap()
}
pub struct Fixture {
    pub store: SqliteOrganizationStore,
    pub ledger: OperationLedger,
    pub binding: StreamBinding,
    git_directory: std::path::PathBuf,
    git_worktree: std::path::PathBuf,
}
impl Fixture {
    pub fn new(directory: &std::path::Path) -> Self {
        let database =
            Database::open_for_application(directory, migrations(), schema::APPLICATION_ID)
                .unwrap();
        let store = match SqliteOrganizationStore::initialize_new(
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
        let server = identity().server;
        let binding = StreamBinding::from_stream(
            OperationStreamId::new(
                OperationStreamKey::new(server, 1).unwrap(),
                PairingId::new(server, 1).unwrap(),
                ClientId::new(server, 1).unwrap(),
            )
            .unwrap(),
        );
        let mut fixture = Self {
            store,
            ledger: OperationLedger::empty(server, LedgerLimits::production_v1()).unwrap(),
            binding,
            git_directory: directory.join("repository"),
            git_worktree: directory.join("worktree"),
        };
        let context = fixture.context();
        let mut prepared = fixture
            .ledger
            .prepare_register_stream(binding, context)
            .unwrap();
        let restricted = StreamRecord::new(
            fixture.restricted(),
            StreamPurpose::SelfRevocation,
            StreamStatus::Active,
            0,
            0,
            Some(OutcomeReservation::full(
                ReservationClass::DedicatedSelfRevocation,
            )),
        )
        .unwrap();
        fixture
            .ledger
            .extend_prepared(&mut prepared, vec![LedgerWrite::RegisterStream(restricted)])
            .unwrap();
        let extra = vec![OperationWrite::Pairing(Box::new(
            PairingWrite::PutAuthorization {
                expected_authorization_version: None,
                value: fixture.authorization(),
            },
        ))];
        let transaction = fixture.proposal(&prepared, extra, vec![], None);
        assert!(matches!(
            fixture.commit_proposal(prepared, transaction),
            TransitionResolution::StreamRegistered
        ));
        fixture
    }
    pub fn context(&mut self) -> TransactionContext {
        let before = self.store.recover().unwrap();
        TransactionContext::new(
            before.store_revision,
            before.organization.revision,
            before.organization.revision,
            None,
        )
        .unwrap()
    }
    pub fn operation(&self, sequence: u64) -> OperationId {
        OperationId::new(
            self.binding.stream(),
            OperationSequence::new(sequence).unwrap(),
        )
    }
    pub fn command(&self) -> Command {
        Command::SetWorktreeLocation {
            server: identity().server,
            expected_version: FieldVersion::new(1),
            template: WorktreeLocationTemplate::new("~/sensitive-sentinel/{branch}".into())
                .unwrap(),
        }
    }
    pub fn prepare_admission(&mut self, sequence: u64) -> PreparedLedgerTransition {
        let context = self.context();
        let AdmissionPreparation::Prepared(prepared) = self
            .ledger
            .prepare_admission(
                &self.binding,
                self.operation(sequence),
                &self.command(),
                PendingKind::AtomicMutation,
                context,
            )
            .unwrap()
        else {
            panic!("expected new operation")
        };
        prepared
    }
    fn parts(
        &mut self,
        prepared: &PreparedLedgerTransition,
        change_settings: bool,
    ) -> TransactionParts {
        let before = self.store.recover().unwrap();
        let mut parts = TransactionParts {
            server: identity().server,
            expected_store_revision: before.store_revision,
            resulting_store_revision: before.store_revision.checked_successor().unwrap(),
            expected_organization_revision: before.organization.revision,
            resulting_organization_revision: before.organization.revision,
            expected_epoch: before.last_epoch,
            resulting_epoch: before.last_epoch,
            expected_high_water: before.high_water,
            resulting_high_water: before.high_water,
            organization_writes: vec![],
            operation_writes: vec![],
        };
        parts.resulting_high_water.pairing = parts.resulting_high_water.pairing.max(1);
        parts.resulting_high_water.client = parts.resulting_high_water.client.max(1);
        parts.resulting_high_water.operation_stream =
            parts.resulting_high_water.operation_stream.max(2);
        parts.resulting_organization_revision = prepared.projection().resulting_organization();
        if change_settings {
            let mut settings = before.organization.settings;
            settings.worktree_location.value =
                WorktreeLocationTemplate::new("~/sensitive-sentinel/{branch}".into()).unwrap();
            settings.worktree_location.version = settings
                .worktree_location
                .version
                .checked_successor()
                .unwrap();
            parts
                .organization_writes
                .push(OrganizationWrite::SetServerSettings(settings));
        }
        for write in prepared.projection().writes() {
            if let LedgerWrite::CompleteOperation {
                basis: CompletionRevisionBasis::OwnedReduction(basis),
                ..
            } = write
            {
                parts
                    .operation_writes
                    .push(OperationWrite::ReductionBasis(*basis));
            }
        }
        prepared.projection().append_to(&mut parts).unwrap();
        parts
    }
    pub fn transaction(
        &mut self,
        prepared: &PreparedLedgerTransition,
        change: bool,
    ) -> DurableTransaction {
        let parts = self.parts(prepared, change);
        DurableTransaction::new_with_validation(parts, &services()).unwrap()
    }
    pub fn commit(
        &mut self,
        prepared: PreparedLedgerTransition,
        change_settings: bool,
    ) -> TransitionResolution {
        let transaction = self.transaction(&prepared, change_settings);
        let disposition = self.store.commit(&transaction);
        self.ledger.resolve_commit(prepared, disposition).unwrap()
    }
    pub fn admit(&mut self, sequence: u64) {
        let prepared = self.prepare_admission(sequence);
        assert!(matches!(
            self.commit(prepared, false),
            TransitionResolution::Admitted(_)
        ));
    }
    pub fn completion(&mut self, sequence: u64, change: bool) -> PreparedLedgerTransition {
        let before = self.store.recover().unwrap();
        let id = self.operation(sequence);
        let digest = spaceterm_core::operation_ledger::canonical::canonical_request_digest(
            &self.binding,
            id,
            &self.command(),
        )
        .unwrap();
        let resulting = if change {
            before.organization.revision.checked_successor().unwrap()
        } else {
            before.organization.revision
        };
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding: OperationBinding { id, digest },
            base: before.organization.revision,
            resulting,
        })
        .unwrap();
        let context = TransactionContext::new(
            before.store_revision,
            before.organization.revision,
            resulting,
            Some(basis),
        )
        .unwrap();
        self.ledger
            .prepare_completion(
                &self.binding,
                id,
                digest,
                CompactOutcome::new(&Outcome::Applied, &CanonicalCodec).unwrap(),
                resulting,
                context,
            )
            .unwrap()
    }
    pub fn complete(&mut self, sequence: u64) {
        let prepared = self.completion(sequence, false);
        assert!(matches!(
            self.commit(prepared, false),
            TransitionResolution::Completed
        ));
    }
    pub fn control(&self) -> TestControl {
        self.store.authority_test_control()
    }
}

use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use sha2::{Digest, Sha256};
use spaceterm_core::organization_retained::*;
use spaceterm_core::pairing::*;
use spaceterm_core::worktrees::{intent::*, types::*};
use spaceterm_host::organization_store::pairing::PairingReceiptReader;
use std::sync::Arc;

// Pairing's production receipt codec is not on this base. This fixed, bounded
// test domain signs exact bytes and recomputes claims/digest on every read. It is
// supplied only by integration tests, never by a production StoreServices default.
const RECEIPT_DOMAIN: &[u8] = b"spaceterm-test-revocation/v1\0";
fn signing_key() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[71; 32]).unwrap()
}
pub fn home() -> ServerDirectory {
    ServerDirectory::new(identity().server, b"/fixture-home".to_vec()).unwrap()
}
pub fn services() -> StoreServices {
    StoreServices::production(home(), Arc::new(TestReceipts))
}
pub struct TestReceipts;
impl PairingReceiptReader for TestReceipts {
    fn read_receipt(
        &self,
        schema: u16,
        body: &[u8],
        signature: &[u8],
    ) -> Result<SignedPairingReceipt, StoreFailure> {
        let invalid = StoreFailure::InvalidDurableState;
        if body.starts_with(ENROLLMENT_DOMAIN) {
            return read_enrollment(schema, body, signature);
        }
        if schema != 1
            || body.len() != RECEIPT_DOMAIN.len() + 120
            || !body.starts_with(RECEIPT_DOMAIN)
        {
            return Err(invalid);
        }
        UnparsedPublicKey::new(&ED25519, identity().server_public_key)
            .verify(body, signature)
            .map_err(|_| invalid)?;
        let bytes = &body[RECEIPT_DOMAIN.len()..];
        let u64_at = |at| u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap());
        let server = ServerId::new(bytes[..16].try_into().unwrap());
        if server != identity().server {
            return Err(invalid);
        }
        let pairing = PairingId::new(server, u64_at(16)).map_err(|_| invalid)?;
        let stream = OperationStreamId::new(
            OperationStreamKey::new(server, u64_at(40)).map_err(|_| invalid)?,
            pairing,
            ClientId::new(server, u64_at(48)).map_err(|_| invalid)?,
        )
        .map_err(|_| invalid)?;
        SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema,
            claims: PairingReceiptBody::Revocation {
                receipt: RevocationReceipt {
                    server,
                    pairing,
                    device: DeviceId::new(bytes[24..40].try_into().unwrap()),
                    operation: OperationId::new(
                        stream,
                        OperationSequence::new(u64_at(56)).map_err(|_| invalid)?,
                    ),
                    revision: StoreRevision::new(u64_at(64)),
                    receipt_digest: RequestDigest::from_bytes(Sha256::digest(body).into()),
                },
                authentication_key_digest: RequestDigest::from_bytes(
                    bytes[72..104].try_into().unwrap(),
                ),
                revocation_event: bytes[104..120].try_into().unwrap(),
            },
            canonical_body: body.to_vec(),
            signature: signature.to_vec(),
        })
        .map_err(|_| invalid)
    }
}
impl PairingReceiptValidator for TestReceipts {
    fn validate_receipt(&self, receipt: &SignedPairingReceipt) -> Result<(), PairingContractError> {
        let p = receipt.parts();
        if self
            .read_receipt(p.schema, &p.canonical_body, &p.signature)
            .map_err(|_| PairingContractError::Invalid)?
            == *receipt
        {
            Ok(())
        } else {
            Err(PairingContractError::Invalid)
        }
    }
}
fn signed_revocation(binding: OperationBinding, revision: StoreRevision) -> SignedPairingReceipt {
    let mut body = RECEIPT_DOMAIN.to_vec();
    body.extend_from_slice(&identity().server.bytes());
    body.extend_from_slice(&binding.id.stream().pairing().local().to_be_bytes());
    body.extend_from_slice(&[3; 16]);
    body.extend_from_slice(&binding.id.stream().key().local().to_be_bytes());
    body.extend_from_slice(&binding.id.stream().client().local().to_be_bytes());
    body.extend_from_slice(&binding.id.sequence().get().to_be_bytes());
    body.extend_from_slice(&revision.get().to_be_bytes());
    body.extend_from_slice(&Sha256::digest([5; 32]));
    body.extend_from_slice(&[6; 16]);
    TestReceipts
        .read_receipt(1, &body, signing_key().sign(&body).as_ref())
        .unwrap()
}
impl Fixture {
    pub fn restricted(&self) -> StreamBinding {
        StreamBinding::from_stream(
            OperationStreamId::new(
                OperationStreamKey::new(identity().server, 2).unwrap(),
                self.binding.pairing(),
                self.binding.client(),
            )
            .unwrap(),
        )
    }
    fn authorization(&self) -> PairingAuthorization {
        PairingAuthorization::new(PairingAuthorizationParts {
            record: PairingRecord {
                id: self.binding.pairing(),
                device: DeviceId::new([3; 16]),
                state: PairingState::Authorized,
            },
            authorization_version: FieldVersion::new(1),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: [4; 32],
                pairing_authentication_public_key: [5; 32],
            })
            .unwrap(),
            clients: vec![
                PairingClientStreams::new(PairingClientStreamsParts {
                    client: ClientRecord {
                        id: self.binding.client(),
                        pairing: self.binding.pairing(),
                    },
                    ordinary: self.binding.stream(),
                    self_revocation: self.restricted().stream(),
                })
                .unwrap(),
            ],
            revocation_receipt: None,
        })
        .unwrap()
    }
    pub fn proposal(
        &mut self,
        prepared: &PreparedLedgerTransition,
        extra: Vec<OperationWrite>,
        organization: Vec<OrganizationWrite>,
        marks: Option<IdentityHighWater>,
    ) -> DurableTransaction {
        let mut parts = self.parts(prepared, false);
        parts.organization_writes = organization;
        parts.operation_writes.extend(extra);
        if let Some(marks) = marks {
            parts.resulting_high_water = marks;
        }
        DurableTransaction::new_with_validation(parts, &services()).unwrap()
    }
    pub fn commit_proposal(
        &mut self,
        prepared: PreparedLedgerTransition,
        transaction: DurableTransaction,
    ) -> TransitionResolution {
        let disposition = self.store.commit(&transaction);
        self.ledger.resolve_commit(prepared, disposition).unwrap()
    }
    pub fn admit_command(
        &mut self,
        binding: StreamBinding,
        command: &Command,
        kind: PendingKind,
    ) -> OperationBinding {
        let state = self.store.recover().unwrap();
        let sequence = state
            .ledger
            .as_ref()
            .unwrap()
            .streams
            .iter()
            .find(|s| s.binding == binding)
            .unwrap()
            .admitted_through
            .checked_add(1)
            .unwrap();
        let id = OperationId::new(binding.stream(), OperationSequence::new(sequence).unwrap());
        let context = self.context();
        let AdmissionPreparation::Prepared(prepared) = self
            .ledger
            .prepare_admission(&binding, id, command, kind, context)
            .unwrap()
        else {
            panic!("expected new admission")
        };
        let transaction = self.proposal(&prepared, vec![], vec![], None);
        let TransitionResolution::Admitted(permit) = self.commit_proposal(prepared, transaction)
        else {
            panic!("expected durable admission")
        };
        assert_eq!(permit.binding().id, id);
        permit.binding()
    }
    pub fn acknowledgement(&mut self) -> (PreparedLedgerTransition, DurableTransaction) {
        let context = self.context();
        let prepared = self
            .ledger
            .prepare_acknowledgement(&self.binding, 1, context)
            .unwrap()
            .unwrap();
        let transaction = self.transaction(&prepared, false);
        (prepared, transaction)
    }
    pub fn revocation(
        &mut self,
        binding: OperationBinding,
    ) -> (
        PreparedLedgerTransition,
        DurableTransaction,
        SignedPairingReceipt,
    ) {
        let before = self.store.recover().unwrap();
        let proof = signed_revocation(binding, before.store_revision.checked_successor().unwrap());
        let mut authorization = self.authorization().parts().clone();
        authorization.record.state = PairingState::Revoked {
            revision: before.store_revision.checked_successor().unwrap(),
        };
        authorization.authorization_version = FieldVersion::new(2);
        authorization.revocation_receipt = Some(proof.clone());
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding,
            base: before.organization.revision,
            resulting: before.organization.revision,
        })
        .unwrap();
        let context = TransactionContext::new(
            before.store_revision,
            before.organization.revision,
            before.organization.revision,
            Some(basis),
        )
        .unwrap();
        let mut prepared = self
            .ledger
            .prepare_completion(
                &self.restricted(),
                binding.id,
                binding.digest,
                CompactOutcome::new(
                    &Outcome::Pairing(Box::new(PairingOutcome::Revoked(proof.clone()))),
                    &CanonicalCodec,
                )
                .unwrap(),
                before.organization.revision,
                context,
            )
            .unwrap();
        self.ledger
            .extend_prepared(
                &mut prepared,
                vec![
                    LedgerWrite::RetireStream {
                        binding: self.binding,
                        reason: StreamRetirementReason::PairingRevoked,
                    },
                    LedgerWrite::RetireStream {
                        binding: self.restricted(),
                        reason: StreamRetirementReason::PairingRevoked,
                    },
                ],
            )
            .unwrap();
        let transaction = self.proposal(
            &prepared,
            vec![OperationWrite::Pairing(Box::new(
                PairingWrite::PutAuthorization {
                    expected_authorization_version: Some(FieldVersion::new(1)),
                    value: PairingAuthorization::new(authorization).unwrap(),
                },
            ))],
            vec![],
            None,
        );
        (prepared, transaction, proof)
    }
    pub fn git_admit(&mut self) -> OperationBinding {
        let command = Command::CreateWorktree {
            scope: RepositoryScope {
                server: identity().server,
                repository: RepositoryId::new(identity().server, 1).unwrap(),
            },
            branch: BranchName::new("fault-fixture".into()).unwrap(),
        };
        self.admit_command(
            self.binding,
            &command,
            PendingKind::External {
                effect: ExternalEffectKind::GitWrite,
            },
        )
    }
    pub fn git_progress(
        &mut self,
        binding: OperationBinding,
        phase: GitIntentPhase,
    ) -> (PreparedLedgerTransition, DurableTransaction) {
        let before = self.store.recover().unwrap();
        let context = self.context();
        let repository = RepositoryId::new(identity().server, 1).unwrap();
        let scope = RepositoryScope {
            server: identity().server,
            repository,
        };
        let external_phase = match phase {
            GitIntentPhase::Prepared => ExternalIntentPhase::Prepared,
            GitIntentPhase::Started => ExternalIntentPhase::Started,
            GitIntentPhase::ExternalFinished => ExternalIntentPhase::ExternalFinished,
        };
        let progress = PendingExternalProgress::new(PendingExternalProgressParts {
            binding,
            effect: ExternalEffectKind::GitWrite,
            state: ExternalProgressState::RecordedIntent {
                reference: ExternalIntentReference::new(ExternalIntentReferenceParts {
                    binding,
                    scope: ExternalIntentScope::Git(scope),
                    phase: external_phase,
                    recorded_at: before.store_revision.checked_successor().unwrap(),
                })
                .unwrap(),
                retirement: None,
            },
        })
        .unwrap();
        let prepared = self
            .ledger
            .prepare_external_progress(&self.binding, binding.id, binding.digest, progress, context)
            .unwrap();
        let mut marks = before.high_water;
        let extra = match phase {
            GitIntentPhase::Prepared => {
                marks.repository = 1;
                marks.worktree = 1;
                let directory = ServerDirectory::new(
                    identity().server,
                    self.git_directory.as_os_str().as_bytes().to_vec(),
                )
                .unwrap();
                let locator = RepositoryLocator::new(RepositoryLocatorParts {
                    scope,
                    main_root: directory.clone(),
                    common_directory: ServerDirectory::new(
                        identity().server,
                        self.git_directory
                            .join(".git")
                            .as_os_str()
                            .as_bytes()
                            .to_vec(),
                    )
                    .unwrap(),
                })
                .unwrap();
                let allocation = GitAllocationReservation::new(GitAllocationReservationParts {
                    binding,
                    expected_high_water: before.high_water,
                    resulting_high_water: marks,
                    repository: Some(repository),
                    worktree: Some(WorktreeId::new(identity().server, 1).unwrap()),
                    fallbacks: vec![],
                })
                .unwrap();
                let intent = GitWriteIntent::new(GitWriteIntentParts {
                    binding,
                    repository: locator.clone(),
                    action: RetainedGitAction::Create {
                        worktree: WorktreeId::new(identity().server, 1).unwrap(),
                        directory: ServerDirectory::new(
                            identity().server,
                            self.git_worktree.as_os_str().as_bytes().to_vec(),
                        )
                        .unwrap(),
                        branch: WorktreeBranchChoice::Existing {
                            name: BranchName::new("fault-fixture".into()).unwrap(),
                        },
                        target: CreationTargetObservation::Absent,
                        template_version: None,
                    },
                    phase,
                    assessment_digest: GitAssessmentDigest::from_bytes([8; 32]),
                    affected: RetainedRemovalScope::empty(identity().server),
                    allocation: binding,
                    external_result: None,
                })
                .unwrap();
                vec![
                    OperationWrite::GitAllocation(GitAllocationMutation::Reserve(Box::new(
                        allocation,
                    ))),
                    OperationWrite::GitRepository(GitRepositoryMutation::Put(locator)),
                    OperationWrite::GitIntent(GitIntentMutation::Insert(Box::new(intent))),
                ]
            }
            GitIntentPhase::Started | GitIntentPhase::ExternalFinished => {
                vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                    binding,
                    expected_phase: if phase == GitIntentPhase::Started {
                        GitIntentPhase::Prepared
                    } else {
                        GitIntentPhase::Started
                    },
                    next: phase,
                    result: (phase == GitIntentPhase::ExternalFinished).then_some(
                        RetainedExternalResult {
                            disposition: ExternalDisposition::DesiredStateObserved,
                            failure: None,
                            ended_sessions: 0,
                        },
                    ),
                })]
            }
        };
        let transaction = self.proposal(&prepared, extra, vec![], Some(marks));
        (prepared, transaction)
    }
    pub fn persist_git_progress(&mut self, binding: OperationBinding, phase: GitIntentPhase) {
        let (prepared, transaction) = self.git_progress(binding, phase);
        assert!(matches!(
            self.commit_proposal(prepared, transaction),
            TransitionResolution::ExternalProgressRecorded
        ));
    }
    pub fn git_completion(
        &mut self,
        binding: OperationBinding,
    ) -> (PreparedLedgerTransition, DurableTransaction) {
        self.git_settlement(binding, false)
    }
    pub fn git_reconciled_completion(
        &mut self,
        binding: OperationBinding,
    ) -> (PreparedLedgerTransition, DurableTransaction) {
        self.git_settlement(binding, true)
    }
    fn git_settlement(
        &mut self,
        binding: OperationBinding,
        recovered: bool,
    ) -> (PreparedLedgerTransition, DurableTransaction) {
        let before = self.store.recover().unwrap();
        let resulting = before.organization.revision.checked_successor().unwrap();
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding,
            base: before.organization.revision,
            resulting,
        })
        .unwrap();
        let context = TransactionContext::new(
            before.store_revision,
            before.organization.revision,
            resulting,
            Some(basis),
        )
        .unwrap();
        let worktree = WorktreeId::new(identity().server, 1).unwrap();
        let repository = RepositoryId::new(identity().server, 1).unwrap();
        let outcome = Outcome::Git(
            GitMutationOutcome::new(if recovered {
                GitMutationOutcomeParts::Recovered {
                    server: identity().server,
                    observation: RecoveryObservation::DesiredStateObserved,
                    revision: Some(resulting),
                }
            } else {
                GitMutationOutcomeParts::Created {
                    worktree,
                    revision: resulting,
                }
            })
            .unwrap(),
        );
        let prepared = self
            .ledger
            .prepare_completion(
                &self.binding,
                binding.id,
                binding.digest,
                CompactOutcome::new(&outcome, &CanonicalCodec).unwrap(),
                resulting,
                context,
            )
            .unwrap();
        let extra = vec![
            OperationWrite::GitIntent(GitIntentMutation::Complete { binding }),
            OperationWrite::GitAllocation(GitAllocationMutation::Consume { binding }),
        ];
        let organization = vec![
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetRepositoryInventoryVersion(
                RepositoryInventoryVersion::new(repository, FieldVersion::new(1)),
            )),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetWorktreeInventoryFacts(
                WorktreeInventoryFacts::new(worktree, true, FieldVersion::new(1)),
            )),
            OrganizationWrite::PutRepository(RepositoryRecord {
                id: repository,
                directory: ServerDirectory::new(
                    identity().server,
                    self.git_directory.as_os_str().as_bytes().to_vec(),
                )
                .unwrap(),
            }),
            OrganizationWrite::PutWorktree(WorktreeRecord {
                id: worktree,
                repository,
                directory: ServerDirectory::new(
                    identity().server,
                    self.git_worktree.as_os_str().as_bytes().to_vec(),
                )
                .unwrap(),
                main: false,
                missing: false,
            }),
        ];
        let transaction = self.proposal(&prepared, extra, organization, None);
        (prepared, transaction)
    }
}

use spaceterm_core::reducer::*;

/// Real reducer, ledger and SQLite composition. Runtime facts are injected by the
/// test actor; these helpers never launch a shell or supply native cleanup proof.
pub struct ReducedFixture {
    pub fixture: Fixture,
    pub reducer: OrganizationReducer,
}
impl ReducedFixture {
    pub fn from_recovered(
        store: SqliteOrganizationStore,
        ledger: OperationLedger,
        directory: &std::path::Path,
    ) -> Self {
        let binding = StreamBinding::from_stream(
            OperationStreamId::new(
                OperationStreamKey::new(identity().server, 1).unwrap(),
                PairingId::new(identity().server, 1).unwrap(),
                ClientId::new(identity().server, 1).unwrap(),
            )
            .unwrap(),
        );
        let fixture = Fixture {
            store,
            ledger,
            binding,
            git_directory: directory.join("repository"),
            git_worktree: directory.join("worktree"),
        };
        Self::recover_fixture(fixture)
    }
    fn recover_fixture(mut fixture: Fixture) -> Self {
        let before = fixture.store.recover().unwrap();
        let mut parts = unchanged_parts(&before);
        parts.resulting_epoch = before.last_epoch.checked_successor().unwrap();
        let transaction = DurableTransaction::new_with_validation(parts, &services()).unwrap();
        assert!(matches!(
            fixture.store.commit(&transaction),
            CommitDisposition::Durable(_)
        ));
        let before = fixture.store.recover().unwrap();
        let retirements = before
            .ledger
            .as_ref()
            .unwrap()
            .operations
            .iter()
            .filter_map(|v| match v {
                RecoveredOperationRecord::Pending(v) => {
                    match v.external_progress.as_ref().map(|v| v.state()) {
                        Some(ExternalProgressState::AcceptedRetirement(v)) => {
                            Some(v.as_ref().clone())
                        }
                        Some(ExternalProgressState::RecordedIntent {
                            retirement: Some(v),
                            ..
                        }) => Some(v.as_ref().clone()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        let retained = RetainedOrganization::new(before.organization, before.high_water, home())
            .with_pending_retirements(retirements)
            .with_native_retirements(before.native_retirements)
            .with_git_allocations(before.git_allocations);
        let reducer = OrganizationReducer::from_recovered(
            retained,
            before.store_revision,
            before.last_epoch,
            OrganizationLimits::default(),
        )
        .unwrap();
        Self { fixture, reducer }
    }
    pub fn new(directory: &std::path::Path) -> Self {
        Self::recover_fixture(Fixture::new(directory))
    }
    pub fn execute(&mut self, command: Command, kind: PendingKind) -> CommittedReduction {
        let binding = self.fixture.binding;
        let sequence = self
            .fixture
            .store
            .recover()
            .unwrap()
            .ledger
            .unwrap()
            .streams
            .iter()
            .find(|s| s.binding == binding)
            .unwrap()
            .admitted_through
            + 1;
        let operation =
            OperationId::new(binding.stream(), OperationSequence::new(sequence).unwrap());
        let context = self.fixture.context();
        let AdmissionPreparation::Prepared(admission) = self
            .fixture
            .ledger
            .prepare_admission(&binding, operation, &command, kind, context)
            .unwrap()
        else {
            panic!("new admission required");
        };
        let transaction = self.fixture.transaction(&admission, false);
        let disposition = self.fixture.store.commit(&transaction);
        let CommitDisposition::Durable(receipt) = disposition else {
            panic!("admission must be durable");
        };
        let TransitionResolution::Admitted(permit) = self
            .fixture
            .ledger
            .resolve_commit(admission, disposition)
            .unwrap()
        else {
            panic!("admission permit required");
        };
        self.reducer.observe_ledger_commit(&receipt).unwrap();
        let mut context = ReductionContext {
            principal: MutationPrincipal {
                pairing: binding.pairing(),
                client: binding.client(),
            },
            operation,
            request_digest: permit.binding().digest,
            now: ReducerTick::new(10),
            facts: ReductionFacts::default(),
        };
        context.facts.retirement_outcome =
            Some(AppliedRetirementOutcome::new(&CanonicalCodec).unwrap());
        if matches!(
            command,
            Command::CreateWorkspace { .. }
                | Command::CreateTab { .. }
                | Command::CreatePane { .. }
                | Command::RepairPaneDirectory { .. }
                | Command::SetPinnedDirectory { .. }
        ) {
            let basis = self.reducer.directory_basis(&command).unwrap();
            let worktree = basis.worktree.as_ref().map(|v| v.worktree.id);
            let fact = |directory| DirectoryFact {
                directory,
                available: true,
                physically_inside: worktree,
            };
            context.facts.directories = Some(DirectoryValidation {
                binding: context.binding(),
                home: fact(home()),
                pin: basis.pin.as_ref().and_then(|v| v.value.clone()).map(fact),
                source: basis.source.as_ref().map(|v| fact(v.directory.clone())),
                requested: basis.requested.clone().map(fact),
                worktree: basis
                    .worktree
                    .as_ref()
                    .map(|v| fact(v.worktree.directory.clone())),
                worktree_bare: false,
                basis,
            });
        }
        let reduction = self.reducer.prepare_command(&command, &context).unwrap();
        self.commit_reduction(reduction, Some(permit.binding()))
    }
    pub fn commit_reduction(
        &mut self,
        reduction: PreparedReduction,
        binding: Option<OperationBinding>,
    ) -> CommittedReduction {
        let before = self.fixture.store.recover().unwrap();
        let mut parts = unchanged_parts(&before);
        parts.resulting_organization_revision = reduction.resulting_organization_revision();
        parts.resulting_high_water = reduction.writes().resulting_high_water();
        reduction.writes().append_to(&mut parts).unwrap();
        let basis = parts.operation_writes.iter().find_map(|w| {
            if let OperationWrite::ReductionBasis(v) = w {
                Some(*v)
            } else {
                None
            }
        });
        let context = TransactionContext::new(
            before.store_revision,
            before.organization.revision,
            parts.resulting_organization_revision,
            basis,
        )
        .unwrap();
        let transition = match reduction.resolution() {
            Resolution::Completed(outcome) if binding.is_some() => {
                let binding = binding.unwrap();
                Some(
                    self.fixture
                        .ledger
                        .prepare_completion(
                            &self.fixture.binding,
                            binding.id,
                            binding.digest,
                            CompactOutcome::new(outcome, &CanonicalCodec).unwrap(),
                            parts.resulting_organization_revision,
                            context,
                        )
                        .unwrap(),
                )
            }
            Resolution::AwaitingRetirement(plan) => {
                let binding = plan.descriptor.binding();
                let progress = PendingExternalProgress::new(PendingExternalProgressParts {
                    binding,
                    effect: ExternalEffectKind::SessionLifecycle,
                    state: ExternalProgressState::AcceptedRetirement(Box::new(
                        plan.descriptor.clone(),
                    )),
                })
                .unwrap();
                Some(
                    self.fixture
                        .ledger
                        .prepare_external_progress(
                            &StreamBinding::from_stream(binding.id.stream()),
                            binding.id,
                            binding.digest,
                            progress,
                            context,
                        )
                        .unwrap(),
                )
            }
            Resolution::RetirementProgress { operations, native } => {
                assert_eq!(operations.len(), 1);
                assert!(native.is_empty());
                let descriptor = &operations[0];
                let binding = descriptor.binding();
                let progress = PendingExternalProgress::new(PendingExternalProgressParts {
                    binding,
                    effect: ExternalEffectKind::SessionLifecycle,
                    state: ExternalProgressState::AcceptedRetirement(Box::new(descriptor.clone())),
                })
                .unwrap();
                Some(
                    self.fixture
                        .ledger
                        .prepare_external_progress(
                            &StreamBinding::from_stream(binding.id.stream()),
                            binding.id,
                            binding.digest,
                            progress,
                            context,
                        )
                        .unwrap(),
                )
            }
            Resolution::RetirementCompleted(plan) => {
                let binding = plan.descriptor.binding();
                Some(
                    self.fixture
                        .ledger
                        .prepare_completion(
                            &StreamBinding::from_stream(binding.id.stream()),
                            binding.id,
                            binding.digest,
                            plan.descriptor.parts().eventual_result.clone(),
                            plan.descriptor.acceptance_revision(),
                            context,
                        )
                        .unwrap(),
                )
            }
            _ if binding.is_none() => None,
            _ => panic!("unsupported identified reduction"),
        };
        if let Some(transition) = &transition {
            transition.projection().append_to(&mut parts).unwrap();
        }
        let transaction = DurableTransaction::new_with_validation(parts, &services()).unwrap();
        let disposition = self.fixture.store.commit(&transaction);
        let CommitDisposition::Durable(receipt) = disposition else {
            panic!("reduction must be durable: {disposition:?}");
        };
        if let Some(transition) = transition {
            self.fixture
                .ledger
                .resolve_commit(transition, disposition)
                .unwrap();
        }
        self.reducer.apply_committed(reduction, &receipt).unwrap()
    }
    pub fn event(&mut self, event: OrganizationEvent) -> CommittedReduction {
        let reduction = self.reducer.prepare_event(&event).unwrap();
        self.commit_reduction(reduction, None)
    }
    pub fn create(&mut self) -> (WorkspaceId, TabId, PaneId) {
        let result = self.execute(
            Command::CreateWorkspace {
                server: identity().server,
                source: WorkspaceCreationSource::ServerHome {
                    server: identity().server,
                },
                name: NameDraft::Automatic,
            },
            PendingKind::AtomicMutation,
        );
        match result.resolution {
            Resolution::Completed(Outcome::CreatedWithControl(v)) => match v.entities() {
                CreatedEntities::Workspace {
                    workspace,
                    tab,
                    pane,
                } => (workspace, tab, pane),
                _ => panic!("workspace creation required"),
            },
            _ => panic!("created outcome required"),
        }
    }
    pub fn start(&mut self, pane: PaneId) -> SessionKey {
        let runtime = *self
            .reducer
            .snapshot()
            .runtime()
            .iter()
            .find(|v| v.pane == pane)
            .unwrap();
        let session = SessionKey {
            pane,
            epoch: self.reducer.snapshot().epoch(),
            generation: runtime.session_generation.checked_successor().unwrap(),
        };
        let reservation = StartupReservation::new(
            pane,
            runtime.session_generation,
            runtime.control_generation,
            runtime.control,
            session,
        )
        .unwrap();
        self.event(OrganizationEvent::StartupReserved(reservation));
        self.event(OrganizationEvent::RuntimeStarted { session });
        session
    }
}
pub fn unchanged_parts(before: &RecoveredState) -> TransactionParts {
    TransactionParts {
        server: before.organization.server,
        expected_store_revision: before.store_revision,
        resulting_store_revision: before.store_revision.checked_successor().unwrap(),
        expected_organization_revision: before.organization.revision,
        resulting_organization_revision: before.organization.revision,
        expected_epoch: before.last_epoch,
        resulting_epoch: before.last_epoch,
        expected_high_water: before.high_water,
        resulting_high_water: before.high_water,
        organization_writes: vec![],
        operation_writes: vec![],
    }
}

const ENROLLMENT_DOMAIN: &[u8] = b"spaceterm-test-enrollment/v1\0";
fn read_enrollment(
    schema: u16,
    body: &[u8],
    signature: &[u8],
) -> Result<SignedPairingReceipt, StoreFailure> {
    let invalid = StoreFailure::InvalidDurableState;
    if schema != 1 || body.len() != ENROLLMENT_DOMAIN.len() + 192 {
        return Err(invalid);
    }
    UnparsedPublicKey::new(&ED25519, identity().server_public_key)
        .verify(body, signature)
        .map_err(|_| invalid)?;
    let bytes = &body[ENROLLMENT_DOMAIN.len()..];
    let u64_at = |at| u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap());
    let server = ServerId::new(bytes[..16].try_into().unwrap());
    if server != identity().server || bytes[184..] != [0; 8] {
        return Err(invalid);
    }
    let pairing = PairingId::new(server, u64_at(16)).map_err(|_| invalid)?;
    let client = ClientId::new(server, u64_at(104)).map_err(|_| invalid)?;
    let stream = |at| {
        OperationStreamId::new(
            OperationStreamKey::new(server, u64_at(at)).map_err(|_| invalid)?,
            pairing,
            client,
        )
        .map_err(|_| invalid)
    };
    SignedPairingReceipt::new(SignedPairingReceiptParts {
        schema,
        claims: PairingReceiptBody::Enrollment {
            target: PairingTarget::new(PairingTargetParts {
                server,
                pairing,
                device: DeviceId::new(bytes[24..40].try_into().unwrap()),
            })
            .map_err(|_| invalid)?,
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: bytes[40..72].try_into().unwrap(),
                pairing_authentication_public_key: bytes[72..104].try_into().unwrap(),
            })
            .map_err(|_| invalid)?,
            client: PairingClientStreams::new(PairingClientStreamsParts {
                client: ClientRecord {
                    id: client,
                    pairing,
                },
                ordinary: stream(112)?,
                self_revocation: stream(120)?,
            })
            .map_err(|_| invalid)?,
            enrollment: OperationBinding {
                id: OperationId::new(
                    stream(128)?,
                    OperationSequence::new(u64_at(136)).map_err(|_| invalid)?,
                ),
                digest: RequestDigest::from_bytes(bytes[144..176].try_into().unwrap()),
            },
            revision: StoreRevision::new(u64_at(176)),
        },
        canonical_body: body.to_vec(),
        signature: signature.to_vec(),
    })
    .map_err(|_| invalid)
}
pub fn signed_enrollment(
    reservation: &EnrollmentReservation,
    streams: &PairingClientStreams,
    binding: OperationBinding,
    revision: StoreRevision,
) -> SignedPairingReceipt {
    let p = reservation.parts();
    let mut body = ENROLLMENT_DOMAIN.to_vec();
    body.extend_from_slice(&p.target.parts().server.bytes());
    body.extend_from_slice(&p.target.parts().pairing.local().to_be_bytes());
    body.extend_from_slice(&p.target.parts().device.bytes());
    body.extend_from_slice(&p.keys.parts().device_identity_public_key);
    body.extend_from_slice(&p.keys.parts().pairing_authentication_public_key);
    body.extend_from_slice(&streams.parts().client.id.local().to_be_bytes());
    body.extend_from_slice(&streams.parts().ordinary.key().local().to_be_bytes());
    body.extend_from_slice(&streams.parts().self_revocation.key().local().to_be_bytes());
    body.extend_from_slice(&binding.id.stream().key().local().to_be_bytes());
    body.extend_from_slice(&binding.id.sequence().get().to_be_bytes());
    body.extend_from_slice(&binding.digest.bytes());
    body.extend_from_slice(&revision.get().to_be_bytes());
    // Reserved fixed bytes distinguish this test-only format from any producer codec.
    body.extend_from_slice(&[0; 8]);
    TestReceipts
        .read_receipt(1, &body, signing_key().sign(&body).as_ref())
        .unwrap()
}
