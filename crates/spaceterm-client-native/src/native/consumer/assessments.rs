//! Short-lived, single-use confirmations shared by separate CLI processes.
//! SQLite owns durability, private-directory validation and process contention.

use serde::{Deserialize, Serialize};
use spaceterm_client::journal::ClientJournalRevision;
use spaceterm_client::management_cli::{
    AssessmentEvidence, AssessmentToken, AuthenticatedManagementContext, ManagementAction,
    ManagementAssessment, ManagementFailure,
};
use spaceterm_core::{
    authority::{FieldVersion, ServerEpoch},
    identity::{ClientId, DeviceId, OperationStreamId, ServerId},
    observations::MonotonicTick,
    operations::CloseAssessment,
    pairing::{LocalPairingKey, PairingTarget, UnpairAssessment},
    remote_access::RemoteAccessPolicy,
};
use spaceterm_host::storage::{CommitDisposition, Database, Migration, StorageFailure};
use spaceterm_protocol::authentication::ServerPin;
use std::path::{Path, PathBuf};

const APPLICATION_ID: u32 = 0x53544153;
const MAX_RECORD_BYTES: usize = 65_536;
const MIGRATIONS: &[Migration<'_>] = &[Migration {
    sql: "CREATE TABLE assessments(\
        token TEXT PRIMARY KEY CHECK(length(token)=64),\
        issued INTEGER NOT NULL CHECK(issued>=0),\
        expires INTEGER NOT NULL CHECK(expires>issued AND expires-issued<=30000),\
        payload BLOB NOT NULL CHECK(length(payload) BETWEEN 1 AND 65536)\
    ) STRICT, WITHOUT ROWID",
}];

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    boot: [u8; 32],
    server: [u8; 16],
    public_key: [u8; 32],
    stream: OperationStreamId,
    device: DeviceId,
    epoch: ServerEpoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<ClientJournalRevision>,
}
impl Scope {
    pub(super) fn local(
        stream: OperationStreamId,
        device: DeviceId,
        pin: ServerPin,
        revision: ClientJournalRevision,
    ) -> Result<Self, ManagementFailure> {
        if pin.identity != stream.server().bytes() {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(Self {
            boot: spaceterm_host::account_service::ServiceTime::current()
                .map_err(super::classify_service)?
                .boot,
            server: pin.identity,
            public_key: pin.public_key,
            stream,
            device,
            // Local removal depends on retained pairing authority, not a live server epoch.
            epoch: ServerEpoch::new(0),
            revision: Some(revision),
        })
    }
    pub(super) fn new(
        context: AuthenticatedManagementContext,
        pin: ServerPin,
    ) -> Result<Self, ManagementFailure> {
        if pin.identity != context.server().bytes() {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(Self {
            boot: spaceterm_host::account_service::ServiceTime::current()
                .map_err(super::classify_service)?
                .boot,
            server: pin.identity,
            public_key: pin.public_key,
            stream: context.stream(),
            device: context.device(),
            epoch: context.epoch(),
            revision: None,
        })
    }
    fn same_owner(&self, other: &Self) -> bool {
        self.boot == other.boot
            && self.server == other.server
            && self.public_key == other.public_key
            && self.stream == other.stream
            && self.device == other.device
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    token: String,
    scope: Scope,
    action: Action,
    evidence: Evidence,
    issued: u64,
    expires: u64,
}
#[derive(Deserialize, Serialize)]
enum Action {
    EndAllSessions(ServerId),
    Unpair(PairingTarget),
    RemoteAccess(RemoteAccessPolicy),
}
#[derive(Deserialize, Serialize)]
enum Evidence {
    EndAllSessions(CloseAssessment),
    Unpair(UnpairAssessment),
    LocalSelfUnpair {
        pairing: LocalPairingKey,
        client: ClientId,
    },
    RemoteAccess(FieldVersion),
}
impl Record {
    fn new(
        scope: Scope,
        assessment: &ManagementAssessment,
        issued: MonotonicTick,
        expires: MonotonicTick,
    ) -> Result<Self, ManagementFailure> {
        let action = match assessment.action() {
            ManagementAction::EndAllSessions { server } => Action::EndAllSessions(*server),
            ManagementAction::Unpair { target } => Action::Unpair(target.clone()),
            ManagementAction::RemoteAccess { policy } => Action::RemoteAccess(policy.clone()),
            _ => return Err(ManagementFailure::AuthorizationDenied),
        };
        let evidence = match assessment.evidence() {
            AssessmentEvidence::EndAllSessions(value) => Evidence::EndAllSessions(*value),
            AssessmentEvidence::Unpair(value) => Evidence::Unpair(value.clone()),
            AssessmentEvidence::LocalSelfUnpair { pairing, client } => Evidence::LocalSelfUnpair {
                pairing: pairing.clone(),
                client: *client,
            },
            AssessmentEvidence::RemoteAccess { current_version } => {
                Evidence::RemoteAccess(*current_version)
            }
            _ => return Err(ManagementFailure::AuthorizationDenied),
        };
        Ok(Self {
            token: assessment.token().as_str().to_owned(),
            scope,
            action,
            evidence,
            issued: issued.get(),
            expires: expires.get(),
        })
    }
    fn assessment(
        self,
        scope: &Scope,
        token: &AssessmentToken,
        now: MonotonicTick,
    ) -> Result<ManagementAssessment, ManagementFailure> {
        if self.scope != *scope || self.token != token.as_str() {
            return Err(ManagementFailure::StaleAssessment);
        }
        let action = match self.action {
            Action::EndAllSessions(server) => ManagementAction::EndAllSessions { server },
            Action::Unpair(target) => ManagementAction::Unpair { target },
            Action::RemoteAccess(policy) => ManagementAction::RemoteAccess { policy },
        };
        let evidence = match self.evidence {
            Evidence::EndAllSessions(value) => AssessmentEvidence::EndAllSessions(value),
            Evidence::Unpair(value) => AssessmentEvidence::Unpair(value),
            Evidence::LocalSelfUnpair { pairing, client } => {
                AssessmentEvidence::LocalSelfUnpair { pairing, client }
            }
            Evidence::RemoteAccess(current_version) => {
                AssessmentEvidence::RemoteAccess { current_version }
            }
        };
        let assessment = ManagementAssessment::new(
            token.clone(),
            action,
            evidence,
            MonotonicTick::new(self.issued),
            MonotonicTick::new(self.expires),
        )?;
        if assessment.action().server().bytes() != scope.server
            || assessment.epoch().is_some_and(|epoch| epoch != scope.epoch)
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        assessment.validate_at(now)?;
        Ok(assessment)
    }
}
fn classify(failure: StorageFailure) -> ManagementFailure {
    match failure {
        StorageFailure::UnsafeStorage | StorageFailure::InvalidState => {
            ManagementFailure::UnsafeNativeState
        }
        StorageFailure::RecoveryRequired => ManagementFailure::Indeterminate,
        _ => ManagementFailure::StorageUnavailable,
    }
}
fn durable<T>(result: CommitDisposition<T>) -> Result<T, ManagementFailure> {
    match result {
        CommitDisposition::Durable(value) => Ok(value),
        CommitDisposition::NotApplied(failure) => Err(classify(failure)),
        CommitDisposition::Indeterminate => Err(ManagementFailure::Indeterminate),
    }
}
fn open(directory: &Path, create: bool) -> Result<Database, ManagementFailure> {
    if create {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ManagementFailure::StorageUnavailable),
        }
        Database::open_for_application(directory, MIGRATIONS, APPLICATION_ID).map_err(classify)
    } else {
        if matches!(std::fs::symlink_metadata(directory), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        Database::open_existing_for_application(directory, MIGRATIONS, APPLICATION_ID)
            .map_err(classify)
    }
}
fn decode(
    bytes: Option<Vec<u8>>,
    scope: &Scope,
    token: &AssessmentToken,
    now: MonotonicTick,
) -> Result<ManagementAssessment, ManagementFailure> {
    let bytes = bytes.ok_or(ManagementFailure::StaleAssessment)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(ManagementFailure::UnsafeNativeState);
    }
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
    record.assessment(scope, token, now)
}
fn tick(now: MonotonicTick) -> Result<i64, ManagementFailure> {
    i64::try_from(now.get()).map_err(|_| ManagementFailure::LimitExceeded)
}

pub(super) async fn retain(
    directory: PathBuf,
    scope: Scope,
    assessment: ManagementAssessment,
    issued: MonotonicTick,
    expires: MonotonicTick,
) -> Result<(), ManagementFailure> {
    tokio::task::spawn_blocking(move || {
        let record = Record::new(scope, &assessment, issued, expires)?;
        let bytes =
            serde_json::to_vec(&record).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(ManagementFailure::LimitExceeded);
        }
        let issued = tick(issued)?;
        let expires = tick(expires)?;
        let mut database = open(&directory, true)?;
        let admitted = durable(database.transaction(|tx| {
            tx.execute(
                "DELETE FROM assessments WHERE expires<=?1 OR issued>?1",
                [issued],
            )?;
            let count: i64 =
                tx.query_row("SELECT count(*) FROM assessments", (), |row| row.get(0))?;
            if count >= 32 {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO assessments(token,issued,expires,payload) VALUES(?1,?2,?3,?4)",
                (record.token.as_str(), issued, expires, bytes.as_slice()),
            )?;
            Ok(true)
        }))?;
        if admitted {
            Ok(())
        } else {
            Err(ManagementFailure::LimitExceeded)
        }
    })
    .await
    .map_err(|_| ManagementFailure::Indeterminate)?
}
pub(super) async fn restore(
    directory: PathBuf,
    scope: Scope,
    token: AssessmentToken,
    now: MonotonicTick,
) -> Result<ManagementAssessment, ManagementFailure> {
    tokio::task::spawn_blocking(move || {
        let database = open(&directory, false)?;
        let bytes: Option<Vec<u8>> = database.query_row(
            "SELECT (SELECT payload FROM assessments WHERE token=?1 AND issued<=?2 AND expires>?2)",
            (token.as_str(), tick(now)?), |row| row.get(0),
        ).map_err(classify)?;
        decode(bytes, &scope, &token, now)
    })
    .await
    .map_err(|_| ManagementFailure::Indeterminate)?
}
/// Loads from retained local pairing authority without dialing. Protocol actions
/// keep their original epoch and must revalidate it against the live connection
/// before execution. Local removal additionally keeps its original journal revision.
pub(super) async fn restore_owned(
    directory: PathBuf,
    owner: Scope,
    token: AssessmentToken,
    now: MonotonicTick,
) -> Result<ManagementAssessment, ManagementFailure> {
    tokio::task::spawn_blocking(move || {
        let database = open(&directory, false)?;
        let bytes: Option<Vec<u8>> = database.query_row(
            "SELECT (SELECT payload FROM assessments WHERE token=?1 AND issued<=?2 AND expires>?2)",
            (token.as_str(), tick(now)?), |row| row.get(0),
        ).map_err(classify)?;
        let bytes = bytes.ok_or(ManagementFailure::StaleAssessment)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if !record.scope.same_owner(&owner)
            || (matches!(record.evidence, Evidence::LocalSelfUnpair { .. })
                && (record.scope.revision.is_none() || record.scope.revision != owner.revision))
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        let original_scope = record.scope.clone();
        record.assessment(&original_scope, &token, now)
    })
    .await
    .map_err(|_| ManagementFailure::Indeterminate)?
}

pub(super) async fn consume(
    directory: PathBuf,
    scope: Scope,
    expected: ManagementAssessment,
    now: MonotonicTick,
) -> Result<(), ManagementFailure> {
    tokio::task::spawn_blocking(move || {
        let mut database = open(&directory, false)?;
        let now_tick = tick(now)?;
        durable(database.transaction(|tx| {
            let bytes: Option<Vec<u8>> = tx.query_row(
                "SELECT (SELECT payload FROM assessments WHERE token=?1 AND issued<=?2 AND expires>?2)",
                (expected.token().as_str(), now_tick), |row| row.get(0),
            )?;
            match decode(bytes, &scope, expected.token(), now) {
                Ok(assessment) if assessment == expected => {},
                Ok(_) => return Ok(Err(ManagementFailure::StaleAssessment)),
                Err(failure) => return Ok(Err(failure)),
            }
            tx.execute("DELETE FROM assessments WHERE token=?1", [expected.token().as_str()])?;
            Ok(Ok(()))
        }))?
    }).await.map_err(|_| ManagementFailure::Indeterminate)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::{
        identity::{ClientId, OperationStreamKey, PairingId},
        operations::{
            AssessmentRef, AssessmentSummary, AssessmentTarget, OperationId, OperationSequence,
            RequestDigest,
        },
        pairing::{ClientRecord, PairingTargetParts, UnpairAssessmentParts},
    };

    fn scope() -> Scope {
        let server = ServerId::new([1; 16]);
        Scope {
            boot: [2; 32],
            server: server.bytes(),
            public_key: [3; 32],
            stream: OperationStreamId::new(
                OperationStreamKey::new(server, 4).unwrap(),
                PairingId::new(server, 5).unwrap(),
                ClientId::new(server, 6).unwrap(),
            )
            .unwrap(),
            device: DeviceId::new([7; 16]),
            epoch: ServerEpoch::new(8),
            revision: None,
        }
    }
    fn assessments(scope: &Scope) -> Vec<ManagementAssessment> {
        let server = ServerId::new(scope.server);
        let target = PairingTarget::new(PairingTargetParts {
            server,
            device: scope.device,
            pairing: scope.stream.pairing(),
        })
        .unwrap();
        let reference = AssessmentRef {
            operation: OperationId::new(scope.stream, OperationSequence::new(9).unwrap()),
            digest: RequestDigest::from_bytes([10; 32]),
        };
        let values = [
            (
                ManagementAction::EndAllSessions { server },
                AssessmentEvidence::EndAllSessions(
                    CloseAssessment::new(
                        reference,
                        scope.epoch,
                        AssessmentSummary::new(
                            AssessmentTarget::EndAllSessions(server),
                            1,
                            2,
                            3,
                            3,
                            true,
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                ),
            ),
            (
                ManagementAction::Unpair {
                    target: target.clone(),
                },
                AssessmentEvidence::Unpair(
                    UnpairAssessment::new(UnpairAssessmentParts {
                        reference,
                        target,
                        authorization_version: FieldVersion::new(11),
                        clients: vec![ClientRecord {
                            id: scope.stream.client(),
                            pairing: scope.stream.pairing(),
                        }],
                    })
                    .unwrap(),
                ),
            ),
            (
                ManagementAction::RemoteAccess {
                    policy: RemoteAccessPolicy::disabled(server),
                },
                AssessmentEvidence::RemoteAccess {
                    current_version: FieldVersion::new(12),
                },
            ),
        ];
        values
            .into_iter()
            .enumerate()
            .map(|(index, (action, evidence))| {
                ManagementAssessment::new(
                    AssessmentToken::new(format!("{index:064x}")).unwrap(),
                    action,
                    evidence,
                    MonotonicTick::new(100),
                    MonotonicTick::new(30_100),
                )
                .unwrap()
            })
            .collect()
    }
    fn directory() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .prefix("st-assessment-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/var/tmp")
            .unwrap()
    }
    #[tokio::test]
    async fn persisted_assessments_keep_original_scope_epoch_boot_and_expiry_for_every_action() {
        let root = directory();
        let directory = root.path().canonicalize().unwrap().join("assessments");
        let scope = scope();
        for original in assessments(&scope) {
            retain(
                directory.clone(),
                scope.clone(),
                original.clone(),
                MonotonicTick::new(100),
                MonotonicTick::new(30_100),
            )
            .await
            .unwrap();
            assert_eq!(
                restore(
                    directory.clone(),
                    scope.clone(),
                    original.token().clone(),
                    MonotonicTick::new(30_099)
                )
                .await
                .unwrap(),
                original
            );
            for now in [99, 30_100, 50_000] {
                assert_eq!(
                    restore(
                        directory.clone(),
                        scope.clone(),
                        original.token().clone(),
                        MonotonicTick::new(now)
                    )
                    .await,
                    Err(ManagementFailure::StaleAssessment)
                );
            }
            let mut changed_epoch = scope.clone();
            changed_epoch.epoch = ServerEpoch::new(9);
            let mut changed_boot = scope.clone();
            changed_boot.boot = [9; 32];
            let mut changed_device = scope.clone();
            changed_device.device = DeviceId::new([9; 16]);
            let mut changed_pin = scope.clone();
            changed_pin.public_key = [9; 32];
            let mut changed_client = scope.clone();
            changed_client.stream = OperationStreamId::new(
                scope.stream.key(),
                scope.stream.pairing(),
                ClientId::new(ServerId::new(scope.server), 10).unwrap(),
            )
            .unwrap();
            for changed in [
                changed_epoch,
                changed_boot,
                changed_device,
                changed_pin,
                changed_client,
            ] {
                assert_eq!(
                    restore(
                        directory.clone(),
                        changed,
                        original.token().clone(),
                        MonotonicTick::new(200)
                    )
                    .await,
                    Err(ManagementFailure::StaleAssessment)
                );
            }
        }
    }
    #[tokio::test]
    async fn separate_consumers_atomically_consume_only_the_exact_original_assessment() {
        async fn consume_after_contention(
            directory: PathBuf,
            scope: Scope,
            expected: ManagementAssessment,
        ) -> Result<(), ManagementFailure> {
            // Both nonblocking SQLite openers can temporarily refuse admission.
            // Retry only that refusal, never stale consent or an uncertain commit.
            for _ in 0..100 {
                let result = consume(
                    directory.clone(),
                    scope.clone(),
                    expected.clone(),
                    MonotonicTick::new(200),
                )
                .await;
                if result != Err(ManagementFailure::StorageUnavailable) {
                    return result;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(ManagementFailure::StorageUnavailable)
        }
        let root = directory();
        let directory = root.path().canonicalize().unwrap().join("assessments");
        let scope = scope();
        let assessment = assessments(&scope).pop().unwrap();
        retain(
            directory.clone(),
            scope.clone(),
            assessment.clone(),
            MonotonicTick::new(100),
            MonotonicTick::new(30_100),
        )
        .await
        .unwrap();
        let (first, second) = tokio::join!(
            consume_after_contention(directory.clone(), scope.clone(), assessment.clone()),
            consume_after_contention(directory.clone(), scope.clone(), assessment.clone()),
        );
        assert_eq!(
            usize::from(first.is_ok()) + usize::from(second.is_ok()),
            1,
            "exactly one consumer may take the confirmation: {first:?}, {second:?}"
        );
        let refused = if first.is_err() { first } else { second };
        assert!(matches!(
            refused,
            Err(ManagementFailure::StaleAssessment | ManagementFailure::StorageUnavailable)
        ));
        assert_eq!(
            restore(
                directory,
                scope,
                assessment.token().clone(),
                MonotonicTick::new(201)
            )
            .await,
            Err(ManagementFailure::StaleAssessment)
        );
    }
}
