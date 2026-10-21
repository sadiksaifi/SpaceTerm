//! Private connection and operation intent, separate from shipped Settings and saved connections.
use std::{collections::BTreeSet, ffi::OsStr, sync::Arc};

use serde::{Deserialize, Serialize};
use spaceterm_server::service::ServiceStatus;
use spaceterm_server::service::maintenance::{Action, MaintenanceRefusal, RemoteBuild};

use super::server_connection::{SshIntentStoreError, SshServerContinuity};
use super::server_maintenance::{MaintenanceTarget, SubmittedKind};
use super::server_operations::{RemoteOperationStore, RetainedOutcome, RetainedRemoteOperation};
use crate::{
    domain::SshDestination,
    platform::{
        app_directories::AppDirectoryRoot,
        app_paths::AppPaths,
        secure_filesystem::{PrivateFileSnapshot, SecureCommitOutcome, SecureFilesystemError},
    },
};

const FILE: &str = "ssh-remote-operations.json";
const MAX_OPERATIONS: usize = 64;
const MAX_BYTES: usize = 64 * 1024;
const MAX_DIGEST: usize = 128;

/// Construction is inert. Only explicit owner operations read or retain operation intent.
pub(crate) struct PrivateRemoteOperationRecords {
    paths: Arc<AppPaths>,
}

/// The decoded document: this client's identifier and the operations it has not retired.
struct Retained {
    client: [u8; 32],
    operations: Vec<RetainedRemoteOperation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u8,
    /// This client's nonsecret identifier, created on first use. Possession grants no authority.
    client: [u8; 32],
    operations: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    destination: String,
    continuity: SshServerContinuity,
    client: [u8; 32],
    operation: [u8; 32],
    kind: RecordKind,
    outcome: Option<RecordOutcome>,
    removed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RecordKind {
    Start,
    Service { action: Action },
    Update { candidate: String },
    Return,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum RecordOutcome {
    Completed {
        status: Option<ServiceStatus>,
        build: Option<RemoteBuild>,
    },
    Refused { refusal: MaintenanceRefusal },
    Failed { can_return: bool },
}

impl PrivateRemoteOperationRecords {
    pub(crate) fn new(paths: Arc<AppPaths>) -> Self {
        Self { paths }
    }

    fn snapshot(&self) -> Result<Option<PrivateFileSnapshot>, SshIntentStoreError> {
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| SshIntentStoreError::Unavailable)?
        else {
            return Ok(None);
        };
        self.paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(FILE), MAX_BYTES)
            .map_err(|_| SshIntentStoreError::Unavailable)
    }

    /// Reads the retained document, or `None` when this client has retained nothing yet.
    fn decode(
        snapshot: Option<&PrivateFileSnapshot>,
    ) -> Result<Option<Retained>, SshIntentStoreError> {
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        if snapshot.bytes.len() > MAX_BYTES {
            return Err(SshIntentStoreError::Malformed);
        }
        let document: Document =
            serde_json::from_slice(&snapshot.bytes).map_err(|_| SshIntentStoreError::Malformed)?;
        if document.schema != 1
            || document.client == [0; 32]
            || document.operations.len() > MAX_OPERATIONS
        {
            return Err(SshIntentStoreError::Malformed);
        }
        let mut identities = BTreeSet::new();
        let operations = document
            .operations
            .into_iter()
            .map(|record| {
                if !identities.insert(record.operation) {
                    return Err(SshIntentStoreError::Malformed);
                }
                let destination = SshDestination::new(record.destination)
                    .map_err(|_| SshIntentStoreError::Malformed)?;
                let operation = RetainedRemoteOperation {
                    target: MaintenanceTarget {
                        destination,
                        continuity: record.continuity,
                    },
                    client: record.client,
                    operation: record.operation,
                    kind: match record.kind {
                        RecordKind::Start => SubmittedKind::Start,
                        RecordKind::Service { action } => SubmittedKind::Service(action),
                        RecordKind::Update { candidate } => SubmittedKind::Update { candidate },
                        RecordKind::Return => SubmittedKind::Return,
                    },
                    outcome: record.outcome.map(|outcome| match outcome {
                        RecordOutcome::Completed { status, build } => {
                            RetainedOutcome::Completed { status, build }
                        }
                        RecordOutcome::Refused { refusal } => RetainedOutcome::Refused(refusal),
                        RecordOutcome::Failed { can_return } => {
                            RetainedOutcome::Failed { can_return }
                        }
                    }),
                    removed: record.removed,
                };
                Self::validate(&operation)?;
                Ok(operation)
            })
            .collect::<Result<_, _>>()?;
        Ok(Some(Retained {
            client: document.client,
            operations,
        }))
    }

    fn encode(operation: RetainedRemoteOperation) -> Record {
        Record {
            destination: operation.target.destination.as_str().to_owned(),
            continuity: operation.target.continuity,
            client: operation.client,
            operation: operation.operation,
            kind: match operation.kind {
                SubmittedKind::Start => RecordKind::Start,
                SubmittedKind::Service(action) => RecordKind::Service { action },
                SubmittedKind::Update { candidate } => RecordKind::Update { candidate },
                SubmittedKind::Return => RecordKind::Return,
            },
            outcome: operation.outcome.map(|outcome| match outcome {
                RetainedOutcome::Completed { status, build } => {
                    RecordOutcome::Completed { status, build }
                }
                RetainedOutcome::Refused(refusal) => RecordOutcome::Refused { refusal },
                RetainedOutcome::Failed { can_return } => RecordOutcome::Failed { can_return },
            }),
            removed: operation.removed,
        }
    }

    /// Rejects a record that cannot name one bound original, so damaged intent is never retained
    /// and never resolved.
    fn validate(operation: &RetainedRemoteOperation) -> Result<(), SshIntentStoreError> {
        let continuity = &operation.target.continuity;
        if operation.target.destination.as_str().len() > 255
            || operation.operation == [0; 32]
            || operation.client == [0; 32]
            || continuity.route == [0; 32]
            || continuity.server == [0; 32]
            || continuity.account == 0
            || continuity.build.is_empty()
            || continuity.build.len() > 128
            || !continuity
                .build
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(SshIntentStoreError::Malformed);
        }
        if let SubmittedKind::Update { candidate } = &operation.kind
            && (candidate.is_empty()
                || candidate.len() > MAX_DIGEST
                || !candidate
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            return Err(SshIntentStoreError::Malformed);
        }
        Ok(())
    }

    /// Applies `change` to the retained operations and commits them, creating this client's
    /// identifier on first use. Malformed or unavailable storage is never replaced.
    fn update(
        &self,
        mut change: impl FnMut(&mut Vec<RetainedRemoteOperation>),
    ) -> Result<[u8; 32], SshIntentStoreError> {
        for _ in 0..8 {
            let snapshot = self.snapshot()?;
            let retained = Self::decode(snapshot.as_ref())?;
            let client = match &retained {
                Some(retained) => retained.client,
                None => {
                    let mut created = [0; 32];
                    getrandom::fill(&mut created).map_err(|_| SshIntentStoreError::Unavailable)?;
                    created
                }
            };
            let mut operations = retained
                .map(|retained| retained.operations)
                .unwrap_or_default();
            change(&mut operations);
            if operations.len() > MAX_OPERATIONS {
                return Err(SshIntentStoreError::Unavailable);
            }
            operations.sort_by_key(|operation| operation.operation);
            let document = Document {
                schema: 1,
                client,
                operations: operations.into_iter().map(Self::encode).collect(),
            };
            let bytes =
                serde_json::to_vec(&document).map_err(|_| SshIntentStoreError::Unavailable)?;
            if bytes.len() > MAX_BYTES {
                return Err(SshIntentStoreError::Unavailable);
            }
            let directory = self
                .paths
                .ensure_secure_root(AppDirectoryRoot::Data)
                .map_err(|_| SshIntentStoreError::Unavailable)?;
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| SshIntentStoreError::Unavailable)?;
            let prepared = match self.paths.filesystem().prepare_private_file(
                &directory,
                OsStr::new(FILE),
                &bytes,
                nonce,
            ) {
                Ok(prepared) => prepared,
                Err(SecureFilesystemError::AlreadyExists) => continue,
                Err(_) => return Err(SshIntentStoreError::Unavailable),
            };
            let result = self
                .paths
                .filesystem()
                .commit_private_file(prepared, snapshot.as_ref().map(|value| &value.identity))
                .map_err(|_| SshIntentStoreError::Unavailable)?;
            match result.outcome {
                SecureCommitOutcome::Committed => return Ok(client),
                SecureCommitOutcome::Conflict => continue,
                SecureCommitOutcome::CommittedButUnsynced => {
                    return Err(SshIntentStoreError::Unavailable);
                }
            }
        }
        Err(SshIntentStoreError::Unavailable)
    }
}

impl RemoteOperationStore for PrivateRemoteOperationRecords {
    fn load(&self) -> Result<Vec<RetainedRemoteOperation>, SshIntentStoreError> {
        Ok(Self::decode(self.snapshot()?.as_ref())
            .map(|retained| retained.map(|retained| retained.operations))?
            .unwrap_or_default())
    }

    fn retain(&self, operation: RetainedRemoteOperation) -> Result<(), SshIntentStoreError> {
        Self::validate(&operation)?;
        self.update(|operations| {
            operations.retain(|current| current.operation != operation.operation);
            operations.push(operation.clone());
        })
        .map(|_| ())
    }

    fn retire(&self, operation: [u8; 32]) -> Result<(), SshIntentStoreError> {
        // Retiring an original this client no longer holds writes nothing, so acknowledgement
        // never creates a document for a client that has retained no operation.
        if !self
            .load()?
            .iter()
            .any(|retained| retained.operation == operation)
        {
            return Ok(());
        }
        self.update(|operations| operations.retain(|current| current.operation != operation))
            .map(|_| ())
    }

    fn client(&self) -> Result<[u8; 32], SshIntentStoreError> {
        match Self::decode(self.snapshot()?.as_ref())? {
            Some(retained) => Ok(retained.client),
            None => self.update(|_| {}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{
        app_directories::AppDirectoryEnvironment,
        testing::{RecordingFilesystem, resolve_app_paths},
    };

    fn fixture() -> (Arc<AppPaths>, Arc<RecordingFilesystem>) {
        let fs = Arc::new(RecordingFilesystem::default());
        let paths = resolve_app_paths(
            &AppDirectoryEnvironment {
                home: Some("/home/test".into()),
                ..Default::default()
            },
            Some("/runtime".into()),
            200,
            fs.clone(),
        )
        .unwrap();
        (Arc::new(paths), fs)
    }

    fn operation(name: &str, identity: u8, kind: SubmittedKind) -> RetainedRemoteOperation {
        RetainedRemoteOperation {
            target: MaintenanceTarget {
                destination: SshDestination::new(name.to_owned()).unwrap(),
                continuity: SshServerContinuity {
                    route: [2; 32],
                    account: 1000,
                    build: "spaceterm-development".into(),
                    server: [3; 32],
                },
            },
            client: [4; 32],
            operation: [identity; 32],
            kind,
            outcome: None,
            removed: false,
        }
    }

    #[test]
    fn a_submitted_operation_and_an_awaiting_result_survive_relaunch() {
        let (paths, _) = fixture();
        let store = PrivateRemoteOperationRecords::new(paths.clone());
        let client = store.client().unwrap();
        store
            .retain(operation(
                "penguin",
                7,
                SubmittedKind::Service(Action::Stop),
            ))
            .unwrap();
        let mut resolved = operation(
            "other",
            8,
            SubmittedKind::Update {
                candidate: "abc123".into(),
            },
        );
        resolved.outcome = Some(RetainedOutcome::Completed {
            status: None,
            build: None,
        });
        resolved.removed = true;
        store.retain(resolved.clone()).unwrap();

        let reopened = PrivateRemoteOperationRecords::new(paths);
        assert_eq!(reopened.client().unwrap(), client, "the identifier is kept");
        let records = reopened.load().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records
                .iter()
                .find(|record| record.operation == [7; 32])
                .unwrap()
                .kind,
            SubmittedKind::Service(Action::Stop)
        );
        assert_eq!(
            *records
                .iter()
                .find(|record| record.operation == [8; 32])
                .unwrap(),
            resolved,
            "a validated result and its removal stay retained"
        );
    }

    #[test]
    fn retiring_an_acknowledged_operation_keeps_the_others() {
        let (paths, _) = fixture();
        let store = PrivateRemoteOperationRecords::new(paths.clone());
        store
            .retain(operation("penguin", 7, SubmittedKind::Start))
            .unwrap();
        store
            .retain(operation("other", 8, SubmittedKind::Return))
            .unwrap();
        store.retire([7; 32]).unwrap();
        let records = PrivateRemoteOperationRecords::new(paths).load().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].operation, [8; 32]);
        store.retire([7; 32]).unwrap();
        assert_eq!(store.load().unwrap().len(), 1, "retiring again is inert");
    }

    #[test]
    fn retiring_the_only_operation_of_an_unused_client_writes_nothing() {
        let (paths, fs) = fixture();
        let store = PrivateRemoteOperationRecords::new(paths.clone());
        store.retire([7; 32]).unwrap();
        let path = paths.directories().data.join(FILE);
        assert!(!fs.files.lock().unwrap().values.contains_key(&path));
    }

    #[test]
    fn malformed_or_unavailable_storage_is_not_replaced_with_new_intent() {
        let (paths, fs) = fixture();
        let store = PrivateRemoteOperationRecords::new(paths.clone());
        store
            .retain(operation("penguin", 7, SubmittedKind::Start))
            .unwrap();
        let path = paths.directories().data.join(FILE);
        let damaged = br#"{"schema":99,"client":[],"operations":[]}"#;
        fs.files.lock().unwrap().values.get_mut(&path).unwrap().0 = damaged.to_vec();
        for attempt in [
            store.retain(operation("other", 8, SubmittedKind::Return)),
            store.retire([7; 32]),
            store.client().map(|_| ()),
            store.load().map(|_| ()),
        ] {
            assert_eq!(attempt, Err(SshIntentStoreError::Malformed));
            assert_eq!(fs.files.lock().unwrap().values[&path].0, damaged);
        }

        fs.files.lock().unwrap().read_failure = Some(SecureFilesystemError::Unavailable);
        assert_eq!(store.load(), Err(SshIntentStoreError::Unavailable));
    }

    #[test]
    fn rejected_records_preserve_readable_intent_and_allow_later_updates() {
        let (paths, fs) = fixture();
        let store = PrivateRemoteOperationRecords::new(paths.clone());
        store
            .retain(operation("penguin", 7, SubmittedKind::Start))
            .unwrap();
        let path = paths.directories().data.join(FILE);
        let original = fs.files.lock().unwrap().values[&path].0.clone();
        let mut unnamed = operation("other", 8, SubmittedKind::Start);
        unnamed.operation = [0; 32];
        let mut unbound = operation("other", 8, SubmittedKind::Start);
        unbound.target.continuity.account = 0;
        for rejected in [
            operation(&"a".repeat(256), 8, SubmittedKind::Start),
            operation(
                "other",
                8,
                SubmittedKind::Update {
                    candidate: "NOTHEX".into(),
                },
            ),
            unnamed,
            unbound,
        ] {
            assert_eq!(store.retain(rejected), Err(SshIntentStoreError::Malformed));
            assert_eq!(fs.files.lock().unwrap().values[&path].0, original);
            let records = PrivateRemoteOperationRecords::new(paths.clone())
                .load()
                .unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].operation, [7; 32]);
        }
        store
            .retain(operation("other", 8, SubmittedKind::Return))
            .unwrap();
        assert_eq!(store.load().unwrap().len(), 2);
    }

    #[test]
    fn more_operations_than_one_client_retains_are_refused_without_losing_intent() {
        let (paths, _) = fixture();
        let store = PrivateRemoteOperationRecords::new(paths);
        for identity in 1..=MAX_OPERATIONS {
            store
                .retain(operation(
                    "penguin",
                    u8::try_from(identity).unwrap(),
                    SubmittedKind::Start,
                ))
                .unwrap();
        }
        assert_eq!(
            store.retain(operation("penguin", 200, SubmittedKind::Start)),
            Err(SshIntentStoreError::Unavailable)
        );
        assert_eq!(store.load().unwrap().len(), MAX_OPERATIONS);
    }
}
