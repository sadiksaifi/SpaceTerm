//! Client presentation context, outside Settings and saved connection authority.
use std::{collections::BTreeSet, ffi::OsStr, sync::Arc};

use serde::{Deserialize, Serialize};
use spaceterm_server::repository::GitToolStatus;

use crate::{
    domain::SshDestination,
    platform::{
        app_directories::AppDirectoryRoot,
        app_paths::AppPaths,
        secure_filesystem::{PrivateFileSnapshot, SecureCommitOutcome, SecureFilesystemError},
    },
};

const FILE: &str = "server-context.json";
const MAX_BYTES: usize = 512 * 1024;
const MAX_SERVERS: usize = 257;

/// Identifies a picker entry without duplicating a saved connection or granting authority.
#[derive(Clone, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) enum ServerKey {
    #[default]
    Local,
    Ssh(String),
    Direct([u8; 32]),
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local => "ServerKey::Local",
            Self::Ssh(_) => "ServerKey::Ssh(..)",
            Self::Direct(_) => "ServerKey::Direct(..)",
        })
    }
}

impl ServerKey {
    fn validate(&self) -> Result<(), ContextError> {
        match self {
            Self::Local => Ok(()),
            Self::Ssh(destination) if destination.len() <= 255 => {
                SshDestination::new(destination.clone())
                    .map(drop)
                    .map_err(|_| ContextError::Malformed)
            }
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
        Ok(())
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

    /// Only the connection/status owner records an observation. Older arrivals cannot replace it.
    pub(crate) fn remember(
        &mut self,
        key: ServerKey,
        facts: LastKnownFacts,
    ) -> Result<(), ContextError> {
        key.validate()?;
        facts.validate()?;
        self.update(|document| {
            if let Some(record) = document.facts.iter_mut().find(|record| record.key == key) {
                if facts.observed_at >= record.facts.observed_at {
                    record.facts = facts.clone();
                }
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
        // Presentation follows the person's choice even if its retention fails.
        let mut displayed = self.document.clone();
        change(&mut displayed);
        displayed.validate()?;
        self.document = displayed;
        for _ in 0..8 {
            let snapshot = self.snapshot()?;
            let mut document = Self::decode(snapshot.as_ref())?;
            let previous = document.clone();
            change(&mut document);
            document.validate()?;
            if document == previous {
                self.document = document;
                return Ok(());
            }
            let bytes = serde_json::to_vec(&document).map_err(|_| ContextError::Unavailable)?;
            if bytes.len() > MAX_BYTES {
                return Err(ContextError::Malformed);
            }
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
            match result.outcome {
                SecureCommitOutcome::Conflict => continue,
                SecureCommitOutcome::Committed => {
                    self.document = document;
                    return Ok(());
                }
                SecureCommitOutcome::CommittedButUnsynced => {
                    self.document = document;
                    return Err(ContextError::DurabilityUncertain);
                }
            }
        }
        Err(ContextError::Unavailable)
    }
}

#[cfg(test)]
mod tests;
