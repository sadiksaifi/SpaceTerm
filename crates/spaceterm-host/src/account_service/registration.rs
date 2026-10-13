use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use spaceterm_protocol::authentication::ServerPin;

use super::{InstallationStatus, ServiceFailure};
use crate::{
    private_filesystem::Directory,
    startup::{AccountBuildKey, DesiredServerState},
};

pub(super) const REGISTRATION_LIMIT: usize = 32 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registration {
    pub(super) schema: u16,
    pub(super) key: AccountBuildKey,
    pub(super) generation: u64,
    pub(super) desired: DesiredServerState,
    pub(super) server: [u8; 16],
    pub(super) public_key: [u8; 32],
    pub(super) data: PathBuf,
    pub(super) organization: PathBuf,
    pub(super) runtime: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) approved: Option<InstallationStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) staged: Option<InstallationStatus>,
    #[serde(default = "installed_default", skip_serializing_if = "is_installed")]
    pub(super) installed: bool,
}

fn installed_default() -> bool {
    true
}
fn is_installed(value: &bool) -> bool {
    *value
}

impl Registration {
    pub(super) fn validate(&self, key: AccountBuildKey) -> Result<(), ServiceFailure> {
        if self.schema != 1 {
            return Err(ServiceFailure::UnsupportedSchema);
        }
        if self.key != key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if self.generation == 0
            || self.server == [0; 16]
            || self.public_key == [0; 32]
            || self.organization != self.data.join("organization")
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if self
            .approved
            .iter()
            .chain(self.staged.iter())
            .any(|installation| installation.artifact_sha256 == [0; 32])
            || self
                .approved
                .as_ref()
                .zip(self.staged.as_ref())
                .is_some_and(|(a, b)| a.installation == b.installation)
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        for path in [&self.data, &self.organization, &self.runtime] {
            validate_path(path)?;
        }
        Ok(())
    }
}

pub(super) fn validate_path(path: &Path) -> Result<(), ServiceFailure> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.len() > 4096
        || bytes.iter().any(|b| b.is_ascii_control())
        || !path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(())
}

/// Retained registration facts with private storage authority and redacted Debug.
///
/// This value cannot be constructed by decoding caller input. Discovery validates
/// the exact account/build record and opens existing private storage.
pub struct RegisteredServer {
    pub(super) record: Registration,
    pub(crate) directory: Directory,
}

impl RegisteredServer {
    /// Returns the resolved owning account and compiled build identity.
    pub fn key(&self) -> AccountBuildKey {
        self.record.key
    }
    /// Returns the registered public identity, never a signing seed.
    pub fn server_pin(&self) -> ServerPin {
        ServerPin {
            identity: self.record.server,
            public_key: self.record.public_key,
        }
    }
    /// Returns the retained nonzero registration generation.
    pub fn generation(&self) -> u64 {
        self.record.generation
    }
    /// Returns deliberate lifecycle intent, not process presence.
    pub fn desired_state(&self) -> DesiredServerState {
        self.record.desired
    }
    /// Returns the first-approved private server data directory.
    pub fn data_directory(&self) -> &Path {
        &self.record.data
    }
    /// Returns the organization directory, whose store has separate authority.
    pub fn organization_directory(&self) -> &Path {
        &self.record.organization
    }
    /// Returns the exact approved artifact facts, without executable authority.
    pub fn approved_installation(&self) -> Option<&InstallationStatus> {
        self.record.approved.as_ref()
    }
    /// Returns separately staged artifact facts.
    pub fn staged_installation(&self) -> Option<&InstallationStatus> {
        self.record.staged.as_ref()
    }
    /// Reports whether service configuration and installed code remain registered.
    pub fn is_installed(&self) -> bool {
        self.record.installed
    }
    /// Opens the registered private runtime route without creating or removing it.
    pub fn endpoint(&self) -> Result<crate::ipc::IpcEndpoint, ServiceFailure> {
        self.directory.verify()?;
        Directory::open(&self.record.runtime)?
            .ok_or(ServiceFailure::Missing)?
            .verify()?;
        crate::ipc::PrivateIpcDirectory::open(&self.record.runtime)
            .and_then(|directory| directory.endpoint("server"))
            .map_err(|_| ServiceFailure::UnsafeStorage)
    }
}

impl fmt::Debug for RegisteredServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RegisteredServer(..)")
    }
}
