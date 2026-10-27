use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use spaceterm_server::direct::{PairingId, ServerPin};

use super::{DirectRoute, UnpairKey};
use crate::platform::{
    app_directories::AppDirectoryRoot,
    app_paths::AppPaths,
    secure_filesystem::{SecureCommitOutcome, SecureEntryIdentity},
};

const FILE: &str = "direct-device-connections.json";
const MAX_BYTES: usize = 512 * 1024;
const CAPACITY: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedRoute {
    pub(super) route: DirectRoute,
    pub(super) credential: [u8; 32],
    pub(super) active: bool,
}

/// Account-local authority for resolving an exact retained revocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum AccountRoute {
    Local,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) enum Authority {
    Direct(SavedRoute),
    Account {
        route: AccountRoute,
        pin: ServerPin,
    },
    Enrollment {
        endpoint: String,
        pin: ServerPin,
        claim: [u8; 32],
        credential: [u8; 32],
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingRecord {
    pub(super) operation: UnpairKey,
    pub(super) label: String,
    pub(super) target: PairingId,
    pub(super) authority: Authority,
    pub(super) dismissed: bool,
    /// A validated exact receipt was retained before retiring the exact native item.
    pub(super) confirmed: bool,
    pub(super) cancel_requested: bool,
}

impl PendingRecord {
    pub(super) fn direct(operation: UnpairKey, actor: SavedRoute, target: PairingId) -> Self {
        Self {
            operation,
            label: actor.route.label.clone(),
            target,
            authority: Authority::Direct(actor),
            dismissed: false,
            confirmed: false,
            cancel_requested: false,
        }
    }

    pub(super) fn account(
        operation: UnpairKey,
        label: String,
        pin: ServerPin,
        route: AccountRoute,
        target: PairingId,
    ) -> Self {
        Self {
            operation,
            label,
            target,
            authority: Authority::Account { route, pin },
            dismissed: false,
            confirmed: false,
            cancel_requested: false,
        }
    }

    pub(super) fn enrollment(
        operation: UnpairKey,
        label: String,
        endpoint: String,
        pin: ServerPin,
        claim: [u8; 32],
        credential: [u8; 32],
        cancelled: bool,
    ) -> Self {
        Self {
            operation,
            label,
            target: [0; 32],
            authority: Authority::Enrollment {
                endpoint,
                pin,
                claim,
                credential,
            },
            dismissed: false,
            confirmed: false,
            cancel_requested: cancelled,
        }
    }

    pub(super) fn server(&self) -> [u8; 32] {
        match &self.authority {
            Authority::Direct(actor) => actor.route.paired.pin.server,
            Authority::Account { pin, .. } => pin.server,
            Authority::Enrollment { pin, .. } => pin.server,
        }
    }

    pub(super) fn credential_key(&self) -> Option<[u8; 32]> {
        match &self.authority {
            Authority::Direct(actor) => Some(actor.credential),
            Authority::Enrollment { credential, .. } => Some(*credential),
            Authority::Account { .. } => None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    schema: u8,
    pub(super) routes: Vec<SavedRoute>,
    pub(super) pending: Vec<PendingRecord>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            schema: 1,
            routes: Vec::new(),
            pending: Vec::new(),
        }
    }
}

impl Document {
    pub(super) fn ensure_capacity(&self) -> Result<(), spaceterm_server::direct::DirectError> {
        if self.routes.len().saturating_add(self.pending.len()) >= CAPACITY {
            Err(spaceterm_server::direct::DirectError::Capacity)
        } else {
            Ok(())
        }
    }

    fn validate(&self) -> Result<(), ()> {
        if self.schema != 1 || self.routes.len().saturating_add(self.pending.len()) > CAPACITY {
            return Err(());
        }
        let mut routes = BTreeSet::new();
        let mut operations = BTreeSet::new();
        for saved in &self.routes {
            validate_route(saved)?;
            if !routes.insert(saved.route.id) {
                return Err(());
            }
        }
        for pending in &self.pending {
            if pending.operation == [0; 32]
                || (pending.target == [0; 32]
                    && !matches!(pending.authority, Authority::Enrollment { .. }))
                || pending.label.len() > 512
                || !operations.insert(pending.operation)
            {
                return Err(());
            }
            match &pending.authority {
                Authority::Direct(actor) => validate_route(actor)?,
                Authority::Enrollment {
                    endpoint,
                    pin,
                    claim,
                    credential,
                } => {
                    validate_pin(pin)?;
                    if *claim == [0; 32]
                        || *credential == [0; 32]
                        || endpoint.len() > 512
                        || !endpoint.starts_with("quic://")
                    {
                        return Err(());
                    }
                }
                Authority::Account { pin, .. } => validate_pin(pin)?,
            }
        }
        Ok(())
    }
}

fn validate_pin(pin: &ServerPin) -> Result<(), ()> {
    if pin.server == [0; 32]
        || pin.public_key == [0; 32]
        || pin.certificate == [0; 32]
        || pin.build.is_empty()
        || pin.build.len() > 128
        || !pin
            .build
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(());
    }
    Ok(())
}

fn validate_route(saved: &SavedRoute) -> Result<(), ()> {
    validate_pin(&saved.route.paired.pin)?;
    if saved.route.id == [0; 32]
        || saved.credential == [0; 32]
        || saved.route.paired.pairing == [0; 32]
        || saved.route.paired.client == [0; 32]
        || saved.route.label.len() > 512
        || saved.route.endpoint.len() > 512
        || !saved.route.endpoint.starts_with("quic://")
        || saved.route.endpoint.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(())
}

pub(super) trait Records: Send + Sync {
    fn load(&self) -> Result<Document, ()>;
    fn save(&self, document: &Document) -> Result<Durability, SaveError>;
}

/// Publication succeeded in either case. Uncertain synchronization is not a rollback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Durability {
    Synchronized,
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SaveError {
    /// Failure before commit, or an identity conflict that published no candidate.
    NotPublished,
    /// Commit or rollback failed; further writes require an explicitly loaded snapshot.
    PublicationIndeterminate,
}

#[derive(Default)]
struct WriteAuthority {
    identity: Option<SecureEntryIdentity>,
    /// A commit error may follow publication or a failed rollback. Only an explicit
    /// load can establish a new observed snapshot; retries must not guess its identity.
    blocked: bool,
}

pub(super) struct PrivateRecords {
    paths: Arc<AppPaths>,
    expected: Mutex<WriteAuthority>,
}

impl PrivateRecords {
    pub(super) fn new(paths: Arc<AppPaths>) -> Self {
        Self {
            paths,
            expected: Mutex::default(),
        }
    }
}

impl Records for PrivateRecords {
    fn load(&self) -> Result<Document, ()> {
        let mut expected = self.expected.lock().map_err(|_| ())?;
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| ())?
        else {
            *expected = WriteAuthority::default();
            return Ok(Document::default());
        };
        let snapshot = self
            .paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(FILE), MAX_BYTES)
            .map_err(|_| ())?;
        let Some(snapshot) = snapshot else {
            *expected = WriteAuthority::default();
            return Ok(Document::default());
        };
        let document: Document = serde_json::from_slice(&snapshot.bytes).map_err(|_| ())?;
        document.validate()?;
        *expected = WriteAuthority {
            identity: Some(snapshot.identity),
            blocked: false,
        };
        Ok(document)
    }

    fn save(&self, document: &Document) -> Result<Durability, SaveError> {
        document.validate().map_err(|_| SaveError::NotPublished)?;
        let bytes = serde_json::to_vec(document).map_err(|_| SaveError::NotPublished)?;
        if bytes.len() > MAX_BYTES {
            return Err(SaveError::NotPublished);
        }
        let mut expected = self.expected.lock().map_err(|_| SaveError::NotPublished)?;
        if expected.blocked {
            return Err(SaveError::PublicationIndeterminate);
        }
        let directory = self
            .paths
            .ensure_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| SaveError::NotPublished)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| SaveError::NotPublished)?;
        let prepared = self
            .paths
            .filesystem()
            .prepare_private_file(&directory, OsStr::new(FILE), &bytes, nonce)
            .map_err(|_| SaveError::NotPublished)?;
        let result = match self
            .paths
            .filesystem()
            .commit_private_file(prepared, expected.identity.as_ref())
        {
            Ok(result) => result,
            Err(_) => {
                expected.blocked = true;
                return Err(SaveError::PublicationIndeterminate);
            }
        };
        let durability = match result.outcome {
            SecureCommitOutcome::Conflict => return Err(SaveError::NotPublished),
            SecureCommitOutcome::Committed => Durability::Synchronized,
            SecureCommitOutcome::CommittedButUnsynced => Durability::Uncertain,
        };
        expected.blocked = result.published_identity.is_none();
        expected.identity = result.published_identity;
        Ok(durability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::app_directories::AppDirectoryEnvironment;
    use crate::platform::testing::{RecordingFilesystem, resolve_app_paths};

    fn storage() -> (PrivateRecords, Arc<RecordingFilesystem>) {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = resolve_app_paths(
            &AppDirectoryEnvironment {
                home: Some("/home/test".into()),
                ..Default::default()
            },
            Some("/runtime".into()),
            200,
            filesystem.clone(),
        )
        .unwrap();
        (PrivateRecords::new(Arc::new(paths)), filesystem)
    }

    #[test]
    fn an_uncertain_publication_retains_its_identity_for_the_next_synchronized_write() {
        let (records, filesystem) = storage();
        let mut document = Document::default();
        assert_eq!(records.save(&document), Ok(Durability::Synchronized));
        document.routes.push(super::super::tests::saved(1, 5));
        filesystem.files.lock().unwrap().commit_outcome =
            Some(SecureCommitOutcome::CommittedButUnsynced);

        assert_eq!(records.save(&document), Ok(Durability::Uncertain));

        filesystem.files.lock().unwrap().commit_outcome = None;
        document.routes[0].route.label = "Saved again".to_owned();
        assert_eq!(records.save(&document), Ok(Durability::Synchronized));
        assert_eq!(records.load().unwrap().routes[0].route.label, "Saved again");
    }

    #[test]
    fn an_uncertain_publication_never_captures_a_competing_successors_identity() {
        let (records, filesystem) = storage();
        let mut document = Document::default();
        document.routes.push(super::super::tests::saved(1, 5));
        {
            let mut files = filesystem.files.lock().unwrap();
            files.commit_outcome = Some(SecureCommitOutcome::CommittedButUnsynced);
            files.successor_after_commit = Some(serde_json::to_vec(&document).unwrap());
        }
        assert_eq!(records.save(&document), Ok(Durability::Uncertain));
        filesystem.files.lock().unwrap().commit_outcome = None;
        document.routes[0].route.label = "Must not replace the successor".to_owned();

        assert_eq!(records.save(&document), Err(SaveError::NotPublished));
        assert_eq!(records.load().unwrap().routes[0].route.label, "Server 1");
    }
}
