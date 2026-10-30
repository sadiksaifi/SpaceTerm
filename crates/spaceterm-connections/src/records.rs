use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use spaceterm_server::direct::{PairingId, ServerPin};

use super::{DirectRoute, UnpairKey};
const MAX_BYTES: usize = 512 * 1024;
const CAPACITY: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedRoute {
    pub route: DirectRoute,
    pub credential: [u8; 32],
    pub active: bool,
}

/// Account-local authority for resolving an exact retained revocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AccountRoute {
    Local,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Authority {
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
pub struct PendingRecord {
    pub operation: UnpairKey,
    pub label: String,
    pub target: PairingId,
    pub authority: Authority,
    pub dismissed: bool,
    /// A validated exact receipt was retained before retiring the exact native item.
    pub confirmed: bool,
    pub cancel_requested: bool,
}

impl PendingRecord {
    pub fn direct(operation: UnpairKey, actor: SavedRoute, target: PairingId) -> Self {
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

    pub fn account(
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

    pub fn enrollment(
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

    pub fn server(&self) -> [u8; 32] {
        match &self.authority {
            Authority::Direct(actor) => actor.route.paired.pin.server,
            Authority::Account { pin, .. } => pin.server,
            Authority::Enrollment { pin, .. } => pin.server,
        }
    }

    pub fn credential_key(&self) -> Option<[u8; 32]> {
        match &self.authority {
            Authority::Direct(actor) => Some(actor.credential),
            Authority::Enrollment { credential, .. } => Some(*credential),
            Authority::Account { .. } => None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    schema: u8,
    pub routes: Vec<SavedRoute>,
    pub pending: Vec<PendingRecord>,
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
    pub fn ensure_capacity(&self) -> Result<(), spaceterm_server::direct::DirectError> {
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

pub trait Records: Send + Sync {
    fn load(&self) -> Result<Document, spaceterm_server::direct::DirectError>;
    fn save(&self, document: &Document) -> Result<Durability, SaveError>;
}

/// Publication succeeded in either case. Uncertain synchronization is not a rollback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    Synchronized,
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveError {
    /// Failure before commit, or an identity conflict that published no candidate.
    NotPublished,
    /// Commit or rollback failed; further writes require an explicitly loaded snapshot.
    PublicationIndeterminate,
}

/// Account-private file authority. Implementations bind commits to the last observed file.
pub trait RecordFile: Send + Sync {
    /// Grant write authority only after `accept` validates the observed bytes.
    fn read(
        &self,
        accept: &mut dyn FnMut(&[u8]) -> Result<(), spaceterm_server::direct::DirectError>,
    ) -> Result<(), spaceterm_server::direct::DirectError>;
    fn commit(&self, bytes: &[u8]) -> Result<Durability, SaveError>;
}

pub struct PrivateRecords(Arc<dyn RecordFile>);
impl PrivateRecords {
    pub fn new(file: Arc<dyn RecordFile>) -> Self {
        Self(file)
    }
}
impl Records for PrivateRecords {
    fn load(&self) -> Result<Document, spaceterm_server::direct::DirectError> {
        let mut document = Document::default();
        self.0.read(&mut |bytes| {
            if bytes.len() > MAX_BYTES {
                return Err(spaceterm_server::direct::DirectError::StorageUnavailable);
            }
            document = serde_json::from_slice(bytes)
                .map_err(|_| spaceterm_server::direct::DirectError::StorageUnavailable)?;
            document
                .validate()
                .map_err(|_| spaceterm_server::direct::DirectError::StorageUnavailable)
        })?;
        Ok(document)
    }
    fn save(&self, document: &Document) -> Result<Durability, SaveError> {
        document.validate().map_err(|_| SaveError::NotPublished)?;
        let bytes = serde_json::to_vec(document).map_err(|_| SaveError::NotPublished)?;
        if bytes.len() > MAX_BYTES {
            return Err(SaveError::NotPublished);
        }
        self.0.commit(&bytes)
    }
}
