//! Client presentation context, outside Settings and saved connection authority.
use std::{collections::BTreeSet, ffi::OsStr, net::SocketAddr, sync::Arc};

use serde::{Deserialize, Serialize};
use spaceterm_server::{direct::NetworkAddresses, repository::GitToolStatus};

use crate::platform::{
    app_directories::AppDirectoryRoot,
    app_paths::AppPaths,
    secure_filesystem::{PrivateFileSnapshot, SecureCommitOutcome, SecureFilesystemError},
};

const FILE: &str = "server-context.json";
const MAX_BYTES: usize = 512 * 1024;
const MAX_SERVERS: usize = 257;
const MAX_DEVICES: usize = 256;
const MAX_ADDRESSES: usize = 256;
const MAX_CONNECTION_TIME: u64 = 253_402_300_799;

/// Identifies a picker entry without duplicating a saved connection or granting authority.
#[derive(Clone, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) enum ServerKey {
    #[default]
    Local,
    Direct([u8; 32]),
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local => "ServerKey::Local",
            Self::Direct(_) => "ServerKey::Direct(..)",
        })
    }
}

impl ServerKey {
    fn validate(&self) -> Result<(), ContextError> {
        match self {
            Self::Local => Ok(()),
            Self::Direct(id) if *id != [0; 32] => Ok(()),
            _ => Err(ContextError::Malformed),
        }
    }
}

/// Historical server state. Reading it never asserts that a connection is live now.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum ObservedServerStatus {
    #[default]
    Unknown,
    Running,
    Stopped,
    NotInstalled,
    NeedsUpdate,
    Unavailable,
}

/// A historical display of the server's listener, never a live reachability assertion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum NetworkListenerFacts {
    Off,
    Listening(SocketAddr),
    AddressGone,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkAccessFacts {
    pub(crate) observed_at: u64,
    pub(crate) server_name: String,
    /// An observed bind preference, not a connection route or authority to change the server.
    pub(crate) policy: Option<SocketAddr>,
    pub(crate) listener: NetworkListenerFacts,
    pub(crate) addresses: Option<NetworkAddresses>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairedDeviceFacts {
    /// Display identity only. This record carries no credentials or revocation authority.
    pub(crate) pairing: [u8; 32],
    pub(crate) name: String,
    pub(crate) fingerprint: String,
    pub(crate) last_connected_at: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairedDevicesFacts {
    pub(crate) observed_at: u64,
    /// A successful empty reading is distinct from an unknown device list.
    pub(crate) devices: Vec<PairedDeviceFacts>,
}

fn valid_display_name(value: &str, max_bytes: usize, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max_bytes
        && spaceterm_server::pane::metadata::sanitize(value, max_chars) == value
}

impl NetworkAccessFacts {
    fn validate(&self) -> Result<(), ContextError> {
        if !valid_display_name(&self.server_name, 512, 512)
            || self.policy.is_some_and(|address| {
                spaceterm_server::direct::NetworkPolicy::new(address).is_err()
            })
            || !match (self.policy, self.listener) {
                (None, NetworkListenerFacts::Off) => true,
                (Some(policy), NetworkListenerFacts::Listening(address)) => policy == address,
                (Some(_), NetworkListenerFacts::AddressGone | NetworkListenerFacts::Failed) => true,
                _ => false,
            }
        {
            return Err(ContextError::Malformed);
        }
        if let Some(addresses) = &self.addresses {
            let mut unique = BTreeSet::new();
            if addresses.available.len() > MAX_ADDRESSES
                || addresses.available.iter().any(|address| {
                    address.is_unspecified() || address.is_multicast() || !unique.insert(*address)
                })
                || addresses
                    .suggested
                    .is_some_and(|address| !unique.contains(&address))
            {
                return Err(ContextError::Malformed);
            }
        }
        Ok(())
    }
}

impl PairedDevicesFacts {
    fn validate(&self) -> Result<(), ContextError> {
        if self.devices.len() > MAX_DEVICES {
            return Err(ContextError::Malformed);
        }
        let mut pairings = BTreeSet::new();
        for device in &self.devices {
            if device.pairing == [0; 32]
                || !pairings.insert(device.pairing)
                || !valid_display_name(&device.name, 192, 48)
                || device.fingerprint.len() != 71
                || !device.fingerprint.bytes().enumerate().all(|(index, byte)| {
                    if index % 9 == 8 {
                        byte == b' '
                    } else {
                        byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)
                    }
                })
                || device
                    .last_connected_at
                    .is_some_and(|time| time > MAX_CONNECTION_TIME)
            {
                return Err(ContextError::Malformed);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LastKnownFacts {
    /// Unix seconds supplied by the connection/status owner, not the time Settings opened.
    pub(crate) observed_at: u64,
    pub(crate) status: ObservedServerStatus,
    pub(crate) machine: Option<String>,
    pub(crate) operating_system: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) starts_automatically: Option<bool>,
    pub(crate) running_sessions: Option<u32>,
    pub(crate) git: GitToolStatus,
    /// Last observed display copy. Edits and writability checks require the owning server.
    #[serde(default)]
    pub(crate) worktree_location: Option<String>,
    #[serde(default)]
    pub(crate) network_access: Option<NetworkAccessFacts>,
    #[serde(default)]
    pub(crate) paired_devices: Option<PairedDevicesFacts>,
}

impl std::fmt::Debug for LastKnownFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LastKnownFacts(..)")
    }
}

impl LastKnownFacts {
    fn validate(&self) -> Result<(), ContextError> {
        if [&self.machine, &self.operating_system, &self.version]
            .into_iter()
            .flatten()
            .any(|value| value.len() > 512 || value.chars().any(char::is_control))
        {
            return Err(ContextError::Malformed);
        }
        if let Some(template) = &self.worktree_location
            && spaceterm_server::worktree::path_template::validate(template).is_err()
        {
            return Err(ContextError::Malformed);
        }
        if let Some(network) = &self.network_access {
            network.validate()?;
        }
        if let Some(devices) = &self.paired_devices {
            devices.validate()?;
        }
        Ok(())
    }

    /// Merges independent observations without changing either input. Equal timestamps prefer
    /// the incoming value; absent network/device readings never erase a successful observation.
    pub(crate) fn merged(&self, incoming: &Self) -> Self {
        let mut merged = if incoming.observed_at >= self.observed_at {
            incoming.clone()
        } else {
            self.clone()
        };
        merged.network_access = incoming
            .network_access
            .as_ref()
            .filter(|next| {
                self.network_access
                    .as_ref()
                    .is_none_or(|previous| next.observed_at >= previous.observed_at)
            })
            .or(self.network_access.as_ref())
            .cloned();
        merged.paired_devices = incoming
            .paired_devices
            .as_ref()
            .filter(|next| {
                self.paired_devices
                    .as_ref()
                    .is_none_or(|previous| next.observed_at >= previous.observed_at)
            })
            .or(self.paired_devices.as_ref())
            .cloned();
        merged
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextError {
    Malformed,
    Unavailable,
    /// The selection/facts were published, but synchronization could not be confirmed.
    DurabilityUncertain,
}

#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    key: ServerKey,
    facts: LastKnownFacts,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u8,
    selected: ServerKey,
    facts: Vec<Record>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            schema: 1,
            selected: ServerKey::Local,
            facts: Vec::new(),
        }
    }
}

impl Document {
    fn validate(&self) -> Result<(), ContextError> {
        if self.schema != 1 || self.facts.len() > MAX_SERVERS {
            return Err(ContextError::Malformed);
        }
        self.selected.validate()?;
        let mut keys = BTreeSet::new();
        for record in &self.facts {
            record.key.validate()?;
            record.facts.validate()?;
            if !keys.insert(&record.key) {
                return Err(ContextError::Malformed);
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| ContextError::Malformed)?
            .len()
            > MAX_BYTES
        {
            return Err(ContextError::Malformed);
        }
        Ok(())
    }
}

/// One app-owned context shared by Servers and Git. Accessors do no I/O or discovery.
/// Load and changes use private storage and should run off the UI thread.
pub(crate) struct ServerContext {
    paths: Arc<AppPaths>,
    document: Document,
}

impl ServerContext {
    /// An inert local fallback. Storage errors never authorize replacing malformed data.
    pub(crate) fn new(paths: Arc<AppPaths>) -> Self {
        Self {
            paths,
            document: Document::default(),
        }
    }

    pub(crate) fn load(paths: Arc<AppPaths>) -> Result<Self, ContextError> {
        let mut context = Self::new(paths);
        context.document = Self::decode(context.snapshot()?.as_ref())?;
        Ok(context)
    }

    pub(crate) fn selected(&self) -> &ServerKey {
        &self.document.selected
    }

    pub(crate) fn facts(&self, key: &ServerKey) -> Option<&LastKnownFacts> {
        self.document
            .facts
            .iter()
            .find(|record| &record.key == key)
            .map(|record| &record.facts)
    }

    /// Changes presentation only. The caller supplies a key from its saved-connection inventory.
    pub(crate) fn select(&mut self, key: ServerKey) -> Result<(), ContextError> {
        key.validate()?;
        self.update(|document| document.selected = key.clone())
    }

    /// Call after saved connections finish loading and whenever one is removed.
    /// Local is always available; stale selection and facts cannot recreate a removed route.
    pub(crate) fn reconcile(&mut self, available: &[ServerKey]) -> Result<(), ContextError> {
        let available: BTreeSet<_> = available
            .iter()
            .chain([&ServerKey::Local])
            .cloned()
            .collect();
        if available.len() > MAX_SERVERS {
            return Err(ContextError::Malformed);
        }
        for key in &available {
            key.validate()?;
        }
        self.update(|document| {
            if !available.contains(&document.selected) {
                document.selected = ServerKey::Local;
            }
            document
                .facts
                .retain(|record| available.contains(&record.key));
        })
    }

    /// The connection/status owner records observations. General, network and device facts each
    /// advance on their own timestamp; a missing network or device reading preserves known facts.
    pub(crate) fn remember(
        &mut self,
        key: ServerKey,
        facts: LastKnownFacts,
    ) -> Result<(), ContextError> {
        key.validate()?;
        facts.validate()?;
        self.update(|document| {
            if let Some(record) = document.facts.iter_mut().find(|record| record.key == key) {
                record.facts = record.facts.merged(&facts);
            } else {
                document.facts.push(Record {
                    key: key.clone(),
                    facts: facts.clone(),
                });
            }
        })
    }

    fn snapshot(&self) -> Result<Option<PrivateFileSnapshot>, ContextError> {
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| ContextError::Unavailable)?
        else {
            return Ok(None);
        };
        self.paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(FILE), MAX_BYTES)
            .map_err(|_| ContextError::Unavailable)
    }

    fn decode(snapshot: Option<&PrivateFileSnapshot>) -> Result<Document, ContextError> {
        let Some(snapshot) = snapshot else {
            return Ok(Document::default());
        };
        if snapshot.bytes.len() > MAX_BYTES {
            return Err(ContextError::Malformed);
        }
        let document: Document =
            serde_json::from_slice(&snapshot.bytes).map_err(|_| ContextError::Malformed)?;
        document.validate()?;
        Ok(document)
    }

    fn update(&mut self, change: impl Fn(&mut Document)) -> Result<(), ContextError> {
        let mut displayed = self.document.clone();
        change(&mut displayed);
        displayed.validate()?;
        let retained = (|| {
            for _ in 0..8 {
                let snapshot = self.snapshot()?;
                let mut document = Self::decode(snapshot.as_ref())?;
                let previous = document.clone();
                change(&mut document);
                document.validate()?;
                displayed = document.clone();
                if document == previous {
                    return Ok((document, SecureCommitOutcome::Committed));
                }
                let bytes = serde_json::to_vec(&document).map_err(|_| ContextError::Unavailable)?;
                let directory = self
                    .paths
                    .ensure_secure_root(AppDirectoryRoot::Data)
                    .map_err(|_| ContextError::Unavailable)?;
                let mut nonce = [0; 16];
                getrandom::fill(&mut nonce).map_err(|_| ContextError::Unavailable)?;
                let prepared = match self.paths.filesystem().prepare_private_file(
                    &directory,
                    OsStr::new(FILE),
                    &bytes,
                    nonce,
                ) {
                    Ok(prepared) => prepared,
                    Err(SecureFilesystemError::AlreadyExists) => continue,
                    Err(_) => return Err(ContextError::Unavailable),
                };
                let result = self
                    .paths
                    .filesystem()
                    .commit_private_file(
                        prepared,
                        snapshot.as_ref().map(|snapshot| &snapshot.identity),
                    )
                    .map_err(|_| ContextError::Unavailable)?;
                if result.outcome != SecureCommitOutcome::Conflict {
                    return Ok((document, result.outcome));
                }
            }
            Err(ContextError::Unavailable)
        })();
        match retained {
            Ok((document, durability)) => {
                self.document = document;
                if durability == SecureCommitOutcome::CommittedButUnsynced {
                    Err(ContextError::DurabilityUncertain)
                } else {
                    Ok(())
                }
            }
            Err(ContextError::Unavailable) => {
                // Storage failure still applies the person's validated presentation change.
                // Malformed input or retained data applies no part of its candidate.
                self.document = displayed;
                Err(ContextError::Unavailable)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests;
