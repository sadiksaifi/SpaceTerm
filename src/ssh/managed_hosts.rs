use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU16;

use thiserror::Error;

use super::destination::SshHostAlias;
use super::host_config::{DiscoveredSshHost, HostConfigSource};
use crate::platform::app_paths::{AppPathRoot, AppPaths, AppPathsError};
use crate::platform::secure_filesystem::{
    PrivateFileSnapshot, SecureCommitOutcome, SecureDirectory, SecureEntryIdentity,
    SecureFilesystemError,
};

const HEADER: &str = "# This file is managed by SpaceTerm.\n\n";
const PRECEDENCE_TAIL: &str = concat!(
    "Host *\n",
    "  Include ~/.ssh/config\n",
    "Host *\n",
    "  Include /etc/ssh/ssh_config\n",
);
const TOKEN_BYTES: usize = 255;
const IDENTITY_FILE_BYTES: usize = 1024;
const MANAGED_CONFIG_BYTES: usize = 1024 * 1024;
const TEMP_CREATION_ATTEMPTS: usize = 128;
const MUTATION_ATTEMPTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSshHostField {
    Alias,
    HostName,
    User,
    IdentityFile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSshHostValueError {
    Required,
    TooLong { maximum: usize },
    Pattern,
    Negated,
    Whitespace,
    Control,
    LeadingOption,
    ReservedKeyword,
    Unsafe,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid {field:?}: {kind:?}")]
pub(crate) struct ManagedSshHostValidationError {
    pub(crate) field: ManagedSshHostField,
    pub(crate) kind: ManagedSshHostValueError,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ManagedSshHost {
    alias: SshHostAlias,
    host_name: String,
    user: Option<String>,
    port: Option<NonZeroU16>,
    identity_file: Option<String>,
}

impl fmt::Debug for ManagedSshHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedSshHost(<redacted>)")
    }
}

impl ManagedSshHost {
    pub(crate) fn new(
        alias: String,
        host_name: String,
        user: Option<String>,
        port: Option<NonZeroU16>,
        identity_file: Option<String>,
    ) -> Result<Self, ManagedSshHostValidationError> {
        validate_alias(&alias)?;
        validate_host_name(&host_name)?;
        if let Some(user) = user.as_deref() {
            validate_user(user)?;
        }
        if let Some(identity_file) = identity_file.as_deref() {
            validate_identity_file(identity_file)?;
        }
        let alias = SshHostAlias::new(alias).map_err(|_| ManagedSshHostValidationError {
            field: ManagedSshHostField::Alias,
            kind: ManagedSshHostValueError::Unsafe,
        })?;
        Ok(Self {
            alias,
            host_name,
            user,
            port,
            identity_file,
        })
    }

    pub(crate) const fn alias(&self) -> &SshHostAlias {
        &self.alias
    }

    pub(crate) fn host_name(&self) -> &str {
        &self.host_name
    }

    pub(crate) fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub(crate) const fn port(&self) -> Option<NonZeroU16> {
        self.port
    }

    pub(crate) fn identity_file(&self) -> Option<&str> {
        self.identity_file.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ManagedHostsFormatError {
    #[error("the managed SSH config is not in SpaceTerm's canonical format")]
    NonCanonical,
}

#[derive(Debug, Error)]
pub(crate) enum ManagedHostsError {
    #[error("the SSH alias is already configured")]
    AliasCollision,
    #[error("the managed SSH alias does not exist")]
    Missing,
    #[error("the managed SSH config is not in SpaceTerm's canonical format")]
    NonCanonical,
    #[error(
        "the managed SSH config was committed but durable synchronization failed; reload before retrying"
    )]
    CommittedButUnsynced,
    #[error("managed SSH storage is unavailable")]
    StorageUnavailable,
    #[error("managed SSH config changed too frequently; reload before retrying")]
    ConcurrentMutation,
    #[error(transparent)]
    Paths(#[from] AppPathsError),
}

/// Canonical store for SpaceTerm-owned concrete SSH host stanzas.
///
/// The store rejects unknown or noncanonical content rather than rewriting it, serializes hosts
/// deterministically, preserves OpenSSH include precedence, and never mutates user or system SSH
/// files. Mutations perform fresh collision checks before a single atomic replacement.
pub(crate) struct ManagedHostsStore<'a> {
    paths: &'a AppPaths,
}

impl<'a> ManagedHostsStore<'a> {
    /// Binds the store to application paths carrying the selected secure filesystem.
    pub(crate) const fn new(paths: &'a AppPaths) -> Self {
        Self { paths }
    }

    /// Loads only the bounded canonical app-owned format.
    pub(crate) fn load(&self) -> Result<Vec<ManagedSshHost>, ManagedHostsError> {
        let Some(snapshot) = self.read_snapshot()? else {
            return Ok(Vec::new());
        };
        parse_managed_hosts(&snapshot.bytes).map_err(|_| ManagedHostsError::NonCanonical)
    }

    /// Ensures OpenSSH's explicit `-F` target exists in the canonical app-owned format.
    ///
    /// Read-only aliases discovered from user or system configuration still execute through this
    /// file so managed aliases retain deterministic precedence. The first connection therefore
    /// publishes an empty canonical file without replacing a concurrently created configuration.
    pub(crate) fn ensure_exists(&self) -> Result<(), ManagedHostsError> {
        for _ in 0..MUTATION_ATTEMPTS {
            if let Some(snapshot) = self.read_snapshot()? {
                return parse_managed_hosts(&snapshot.bytes)
                    .map(|_| ())
                    .map_err(|_| ManagedHostsError::NonCanonical);
            }
            let directory = self.paths.ensure_secure_root(AppPathRoot::Config)?;
            let bytes = serialize_managed_hosts(&[]);
            match self.commit(&directory, bytes.as_bytes(), None)? {
                SecureCommitOutcome::Committed => return Ok(()),
                SecureCommitOutcome::CommittedButUnsynced => {
                    return Err(ManagedHostsError::CommittedButUnsynced);
                }
                SecureCommitOutcome::Conflict => continue,
            }
        }
        Err(ManagedHostsError::ConcurrentMutation)
    }

    /// Inserts or edits one host after collision checks against fresh discovered provenance.
    ///
    /// The exact edited managed declaration is the sole collision exemption. Pre-commit failure
    /// preserves the original bytes; a directory-sync failure is reported as committed state.
    pub(crate) fn upsert(
        &self,
        host: ManagedSshHost,
        configured_hosts: &[DiscoveredSshHost],
        editing_alias: Option<&SshHostAlias>,
    ) -> Result<(), ManagedHostsError> {
        if configured_hosts
            .iter()
            .any(|configured| configured_host_collides(configured, host.alias(), editing_alias))
        {
            return Err(ManagedHostsError::AliasCollision);
        }
        for _ in 0..MUTATION_ATTEMPTS {
            let snapshot = self.read_snapshot()?;
            let mut hosts = snapshot
                .as_ref()
                .map(|snapshot| parse_managed_hosts(&snapshot.bytes))
                .transpose()
                .map_err(|_| ManagedHostsError::NonCanonical)?
                .unwrap_or_default();
            if let Some(editing_alias) = editing_alias {
                let position = hosts
                    .iter()
                    .position(|existing| existing.alias() == editing_alias)
                    .ok_or(ManagedHostsError::Missing)?;
                if editing_alias != host.alias()
                    && hosts
                        .iter()
                        .any(|existing| existing.alias() == host.alias())
                {
                    return Err(ManagedHostsError::AliasCollision);
                }
                hosts.remove(position);
            } else if hosts
                .iter()
                .any(|existing| existing.alias() == host.alias())
            {
                return Err(ManagedHostsError::AliasCollision);
            }
            hosts.push(host.clone());
            match self.write(&hosts, snapshot.as_ref().map(|snapshot| &snapshot.identity))? {
                SecureCommitOutcome::Committed => return Ok(()),
                SecureCommitOutcome::CommittedButUnsynced => {
                    return Err(ManagedHostsError::CommittedButUnsynced);
                }
                SecureCommitOutcome::Conflict => continue,
            }
        }
        Err(ManagedHostsError::ConcurrentMutation)
    }

    /// Deletes one existing managed alias using the same atomic mutation contract.
    pub(crate) fn delete(&self, alias: &SshHostAlias) -> Result<(), ManagedHostsError> {
        for _ in 0..MUTATION_ATTEMPTS {
            let snapshot = self.read_snapshot()?;
            let mut hosts = snapshot
                .as_ref()
                .map(|snapshot| parse_managed_hosts(&snapshot.bytes))
                .transpose()
                .map_err(|_| ManagedHostsError::NonCanonical)?
                .unwrap_or_default();
            let position = hosts
                .iter()
                .position(|host| host.alias() == alias)
                .ok_or(ManagedHostsError::Missing)?;
            hosts.remove(position);
            match self.write(&hosts, snapshot.as_ref().map(|snapshot| &snapshot.identity))? {
                SecureCommitOutcome::Committed => return Ok(()),
                SecureCommitOutcome::CommittedButUnsynced => {
                    return Err(ManagedHostsError::CommittedButUnsynced);
                }
                SecureCommitOutcome::Conflict => continue,
            }
        }
        Err(ManagedHostsError::ConcurrentMutation)
    }

    fn write(
        &self,
        hosts: &[ManagedSshHost],
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<SecureCommitOutcome, ManagedHostsError> {
        let directory = self.paths.ensure_secure_root(AppPathRoot::Config)?;
        let bytes = serialize_managed_hosts(hosts);
        if bytes.len() > MANAGED_CONFIG_BYTES {
            return Err(ManagedHostsError::StorageUnavailable);
        }
        self.commit(&directory, bytes.as_bytes(), expected)
    }

    fn read_snapshot(&self) -> Result<Option<PrivateFileSnapshot>, ManagedHostsError> {
        let Some(directory) = self.paths.open_secure_root(AppPathRoot::Config)? else {
            return Ok(None);
        };
        let target = self.paths.managed_ssh_config();
        let name = target
            .file_name()
            .ok_or(ManagedHostsError::StorageUnavailable)?;
        self.paths
            .filesystem()
            .read_private_file(&directory, name, MANAGED_CONFIG_BYTES)
            .map_err(map_filesystem_error)
    }

    fn commit(
        &self,
        directory: &SecureDirectory,
        bytes: &[u8],
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<SecureCommitOutcome, ManagedHostsError> {
        let target = self.paths.managed_ssh_config();
        let name = target
            .file_name()
            .ok_or(ManagedHostsError::StorageUnavailable)?;
        for _ in 0..TEMP_CREATION_ATTEMPTS {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|_| ManagedHostsError::StorageUnavailable)?;
            match self
                .paths
                .filesystem()
                .prepare_private_file(directory, name, bytes, nonce)
            {
                Ok(prepared) => {
                    return self
                        .paths
                        .filesystem()
                        .commit_private_file(prepared, expected)
                        .map(|result| result.outcome)
                        .map_err(map_filesystem_error);
                }
                Err(SecureFilesystemError::AlreadyExists) => continue,
                Err(error) => return Err(map_filesystem_error(error)),
            }
        }
        Err(ManagedHostsError::StorageUnavailable)
    }
}

fn map_filesystem_error(_: SecureFilesystemError) -> ManagedHostsError {
    ManagedHostsError::StorageUnavailable
}

fn configured_host_collides(
    configured: &DiscoveredSshHost,
    candidate: &SshHostAlias,
    editing_alias: Option<&SshHostAlias>,
) -> bool {
    if configured.alias() != candidate {
        return false;
    }
    if editing_alias != Some(candidate) || configured.is_ambiguous() {
        return true;
    }
    let mut excluded_edited_declaration = false;
    for provenance in configured.provenances() {
        if !excluded_edited_declaration && provenance.source() == HostConfigSource::Managed {
            excluded_edited_declaration = true;
        } else {
            return true;
        }
    }
    !excluded_edited_declaration
}

fn validate_alias(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::Alias, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | ':' | '[' | ']')
    })
}

fn validate_host_name(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::HostName, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | ':' | '[' | ']')
    })
}

fn validate_user(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::User, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | '@' | '+')
    })
}

fn validate_token(
    field: ManagedSshHostField,
    value: &str,
    allowed: impl Fn(char) -> bool,
) -> Result<(), ManagedSshHostValidationError> {
    let kind = if value.is_empty() {
        Some(ManagedSshHostValueError::Required)
    } else if value.len() > TOKEN_BYTES {
        Some(ManagedSshHostValueError::TooLong {
            maximum: TOKEN_BYTES,
        })
    } else if value.chars().any(char::is_control) {
        Some(ManagedSshHostValueError::Control)
    } else if value.chars().any(char::is_whitespace) {
        Some(ManagedSshHostValueError::Whitespace)
    } else if value.starts_with('-') {
        Some(ManagedSshHostValueError::LeadingOption)
    } else if value.starts_with('!') {
        Some(ManagedSshHostValueError::Negated)
    } else if value.contains(['*', '?']) {
        Some(ManagedSshHostValueError::Pattern)
    } else if is_reserved_keyword(value) {
        Some(ManagedSshHostValueError::ReservedKeyword)
    } else if !value.chars().all(allowed) {
        Some(ManagedSshHostValueError::Unsafe)
    } else {
        None
    };
    if let Some(kind) = kind {
        Err(ManagedSshHostValidationError { field, kind })
    } else {
        Ok(())
    }
}

fn validate_identity_file(value: &str) -> Result<(), ManagedSshHostValidationError> {
    let field = ManagedSshHostField::IdentityFile;
    let kind = if value.is_empty() {
        Some(ManagedSshHostValueError::Required)
    } else if value.len() > IDENTITY_FILE_BYTES {
        Some(ManagedSshHostValueError::TooLong {
            maximum: IDENTITY_FILE_BYTES,
        })
    } else if value.chars().any(char::is_control) {
        Some(ManagedSshHostValueError::Control)
    } else if value.starts_with('-') {
        Some(ManagedSshHostValueError::LeadingOption)
    } else if value.starts_with('!') {
        Some(ManagedSshHostValueError::Negated)
    } else if value.contains(['*', '?']) {
        Some(ManagedSshHostValueError::Pattern)
    } else if !concrete_identity_path(value) {
        Some(ManagedSshHostValueError::Unsafe)
    } else {
        None
    };
    if let Some(kind) = kind {
        Err(ManagedSshHostValidationError { field, kind })
    } else {
        Ok(())
    }
}

fn concrete_identity_path(value: &str) -> bool {
    let relative = if let Some(relative) = value.strip_prefix("~/") {
        relative
    } else if let Some(relative) = value.strip_prefix('/') {
        relative
    } else {
        return false;
    };
    !relative.is_empty()
        && !relative.ends_with('/')
        && relative
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn is_reserved_keyword(value: &str) -> bool {
    [
        "host",
        "hostname",
        "user",
        "port",
        "identityfile",
        "include",
        "match",
    ]
    .iter()
    .any(|keyword| value.eq_ignore_ascii_case(keyword))
}

fn serialize_managed_hosts(hosts: &[ManagedSshHost]) -> String {
    let mut ordered = hosts.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.alias.cmp(&right.alias));
    let mut serialized = String::from(HEADER);
    for host in ordered {
        serialized.push_str("Host ");
        serialized.push_str(host.alias.as_str());
        serialized.push('\n');
        serialized.push_str("  HostName ");
        serialized.push_str(&host.host_name);
        serialized.push('\n');
        if let Some(user) = &host.user {
            serialized.push_str("  User ");
            serialized.push_str(user);
            serialized.push('\n');
        }
        if let Some(port) = host.port {
            serialized.push_str("  Port ");
            serialized.push_str(&port.to_string());
            serialized.push('\n');
        }
        if let Some(identity_file) = &host.identity_file {
            serialized.push_str("  IdentityFile ");
            quote_argument(identity_file, &mut serialized);
            serialized.push('\n');
        }
        serialized.push('\n');
    }
    serialized.push_str(PRECEDENCE_TAIL);
    serialized
}

fn quote_argument(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            output.push('\\');
        }
        output.push(character);
    }
    output.push('"');
}

fn parse_managed_hosts(bytes: &[u8]) -> Result<Vec<ManagedSshHost>, ManagedHostsFormatError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ManagedHostsFormatError::NonCanonical)?;
    let body = text
        .strip_prefix(HEADER)
        .and_then(|text| text.strip_suffix(PRECEDENCE_TAIL))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let mut hosts = Vec::new();
    let mut aliases = BTreeSet::new();
    if !body.is_empty() {
        let stanzas = body
            .strip_suffix("\n\n")
            .ok_or(ManagedHostsFormatError::NonCanonical)?;
        for stanza in stanzas.split("\n\n") {
            let host = parse_stanza(stanza)?;
            if !aliases.insert(host.alias.as_str().to_owned()) {
                return Err(ManagedHostsFormatError::NonCanonical);
            }
            hosts.push(host);
        }
    }
    if serialize_managed_hosts(&hosts) != text {
        return Err(ManagedHostsFormatError::NonCanonical);
    }
    Ok(hosts)
}

fn parse_stanza(stanza: &str) -> Result<ManagedSshHost, ManagedHostsFormatError> {
    let mut lines = stanza.lines().peekable();
    let alias = lines
        .next()
        .and_then(|line| line.strip_prefix("Host "))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let host_name = lines
        .next()
        .and_then(|line| line.strip_prefix("  HostName "))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let user = take_prefixed(&mut lines, "  User ").map(str::to_owned);
    let port = take_prefixed(&mut lines, "  Port ")
        .map(|value| value.parse::<NonZeroU16>())
        .transpose()
        .map_err(|_| ManagedHostsFormatError::NonCanonical)?;
    let identity_file = take_prefixed(&mut lines, "  IdentityFile ")
        .map(parse_quoted_argument)
        .transpose()?;
    if lines.next().is_some() {
        return Err(ManagedHostsFormatError::NonCanonical);
    }
    ManagedSshHost::new(
        alias.to_owned(),
        host_name.to_owned(),
        user,
        port,
        identity_file,
    )
    .map_err(|_| ManagedHostsFormatError::NonCanonical)
}

fn take_prefixed<'a, I>(lines: &mut std::iter::Peekable<I>, prefix: &str) -> Option<&'a str>
where
    I: Iterator<Item = &'a str>,
{
    lines.peek().and_then(|line| line.strip_prefix(prefix))?;
    lines.next().and_then(|line| line.strip_prefix(prefix))
}

fn parse_quoted_argument(value: &str) -> Result<String, ManagedHostsFormatError> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let mut parsed = String::new();
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            let escaped = characters
                .next()
                .filter(|escaped| matches!(escaped, '\\' | '"'))
                .ok_or(ManagedHostsFormatError::NonCanonical)?;
            parsed.push(escaped);
        } else if character == '"' {
            return Err(ManagedHostsFormatError::NonCanonical);
        } else {
            parsed.push(character);
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::platform::app_paths::{AppPathEnvironment, AppPathHostFacts};
    use crate::platform::secure_filesystem::{
        PreparedPrivateFile, SecureCommitResult, SecureFilesystem,
    };

    #[derive(Default)]
    struct RecordingFilesystem {
        state: Mutex<RecordingState>,
    }

    #[derive(Default)]
    struct RecordingState {
        directories: BTreeSet<PathBuf>,
        files: BTreeMap<(PathBuf, String), (Vec<u8>, u64)>,
        conflicts_remaining: usize,
        prepare_collisions_remaining: usize,
        preparation_nonces: Vec<[u8; 16]>,
        next_identity: u64,
        events: Vec<&'static str>,
    }

    #[derive(Clone)]
    struct RecordingDirectory(PathBuf);
    #[derive(Clone, Eq, PartialEq)]
    struct RecordingIdentity(u64);
    struct RecordingPrepared(PathBuf, String, Vec<u8>);

    impl RecordingFilesystem {
        fn directory_path(directory: &SecureDirectory) -> Result<&PathBuf, SecureFilesystemError> {
            directory
                .opaque_ref::<RecordingDirectory>()
                .map(|directory| &directory.0)
                .ok_or(SecureFilesystemError::Unsafe)
        }

        fn directory(path: PathBuf) -> SecureDirectory {
            SecureDirectory::from_opaque(RecordingDirectory(path))
        }

        fn set_conflicts(&self, conflicts: usize) {
            self.state.lock().unwrap().conflicts_remaining = conflicts;
        }

        fn set_prepare_collisions(&self, collisions: usize) {
            self.state.lock().unwrap().prepare_collisions_remaining = collisions;
        }
    }

    impl SecureFilesystem for RecordingFilesystem {
        fn open_private_directory(
            &self,
            path: &Path,
        ) -> Result<Option<SecureDirectory>, SecureFilesystemError> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .directories
                .contains(path)
                .then(|| Self::directory(path.to_path_buf())))
        }

        fn ensure_private_directory(
            &self,
            path: &Path,
        ) -> Result<SecureDirectory, SecureFilesystemError> {
            let mut state = self.state.lock().unwrap();
            state.events.push("ensure-directory");
            state.directories.insert(path.to_path_buf());
            Ok(Self::directory(path.to_path_buf()))
        }

        fn create_private_child(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
        ) -> Result<SecureDirectory, SecureFilesystemError> {
            Err(SecureFilesystemError::Unavailable)
        }
        fn verify_directory(&self, _: &SecureDirectory) -> Result<(), SecureFilesystemError> {
            Ok(())
        }
        fn remove_private_child(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: &SecureDirectory,
        ) -> Result<(), SecureFilesystemError> {
            Ok(())
        }

        fn read_private_file(
            &self,
            directory: &SecureDirectory,
            name: &OsStr,
            _: usize,
        ) -> Result<Option<PrivateFileSnapshot>, SecureFilesystemError> {
            let key = (
                Self::directory_path(directory)?.clone(),
                name.to_string_lossy().into_owned(),
            );
            let mut state = self.state.lock().unwrap();
            state.events.push("read");
            Ok(state
                .files
                .get(&key)
                .map(|(bytes, identity)| PrivateFileSnapshot {
                    bytes: bytes.clone(),
                    identity: SecureEntryIdentity::from_opaque(RecordingIdentity(*identity)),
                }))
        }

        fn prepare_private_file(
            &self,
            directory: &SecureDirectory,
            target: &OsStr,
            bytes: &[u8],
            nonce: [u8; 16],
        ) -> Result<PreparedPrivateFile, SecureFilesystemError> {
            let mut state = self.state.lock().unwrap();
            state.events.push("prepare");
            state.preparation_nonces.push(nonce);
            if state.prepare_collisions_remaining > 0 {
                state.prepare_collisions_remaining -= 1;
                return Err(SecureFilesystemError::AlreadyExists);
            }
            drop(state);
            Ok(PreparedPrivateFile::from_opaque(RecordingPrepared(
                Self::directory_path(directory)?.clone(),
                target.to_string_lossy().into_owned(),
                bytes.to_vec(),
            )))
        }

        fn commit_private_file(
            &self,
            prepared: PreparedPrivateFile,
            expected: Option<&SecureEntryIdentity>,
        ) -> Result<SecureCommitResult, SecureFilesystemError> {
            let RecordingPrepared(path, name, bytes) = *prepared
                .into_opaque::<RecordingPrepared>()
                .map_err(|_| SecureFilesystemError::Unsafe)?;
            let expected = expected
                .map(|identity| {
                    identity
                        .opaque_ref::<RecordingIdentity>()
                        .map(|identity| identity.0)
                        .ok_or(SecureFilesystemError::Unsafe)
                })
                .transpose()?;
            let mut state = self.state.lock().unwrap();
            state.events.push("commit");
            if state.conflicts_remaining > 0 {
                state.conflicts_remaining -= 1;
                return Ok(SecureCommitResult::conflict());
            }
            let key = (path, name);
            if state.files.get(&key).map(|(_, identity)| *identity) != expected {
                return Ok(SecureCommitResult::conflict());
            }
            state.next_identity += 1;
            let identity = state.next_identity;
            state.files.insert(key, (bytes, identity));
            Ok(SecureCommitResult::committed(
                SecureCommitOutcome::Committed,
                SecureEntryIdentity::from_opaque(RecordingIdentity(identity)),
            ))
        }

        fn register_socket(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
        ) -> Result<SecureEntryIdentity, SecureFilesystemError> {
            Err(SecureFilesystemError::Unavailable)
        }
        fn verify_socket(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: &SecureEntryIdentity,
        ) -> Result<(), SecureFilesystemError> {
            Err(SecureFilesystemError::Unavailable)
        }
        fn remove_socket(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: &SecureEntryIdentity,
        ) -> Result<(), SecureFilesystemError> {
            Err(SecureFilesystemError::Unavailable)
        }
        #[cfg(feature = "macos-native-tests")]
        fn create_private_artifact(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
        ) -> Result<(), SecureFilesystemError> {
            Err(SecureFilesystemError::Unavailable)
        }
    }

    fn paths(filesystem: Arc<RecordingFilesystem>) -> AppPaths {
        let environment = AppPathEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        };
        let host = AppPathHostFacts::new("/runtime".into(), 200).unwrap();
        AppPaths::resolve(&environment, &host, filesystem).unwrap()
    }

    fn host(alias: &str, host_name: &str) -> ManagedSshHost {
        ManagedSshHost::new(alias.into(), host_name.into(), None, None, None).unwrap()
    }

    #[test]
    fn store_should_add_edit_delete_and_preserve_canonical_order() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = paths(filesystem);
        let store = ManagedHostsStore::new(&paths);
        store
            .upsert(host("zeta", "zeta.example"), &[], None)
            .unwrap();
        store
            .upsert(host("alpha", "alpha.example"), &[], None)
            .unwrap();
        store
            .upsert(
                host("beta", "beta.example"),
                &[],
                Some(host("zeta", "ignored").alias()),
            )
            .unwrap();
        store.delete(host("alpha", "ignored").alias()).unwrap();

        let loaded = store.load().unwrap();

        assert_eq!(loaded, vec![host("beta", "beta.example")]);
    }

    #[test]
    fn mutation_should_retry_a_concurrent_conflict_from_a_fresh_snapshot() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        filesystem.set_conflicts(2);
        let paths = paths(filesystem.clone());
        let store = ManagedHostsStore::new(&paths);

        store
            .upsert(host("work", "work.example"), &[], None)
            .unwrap();

        let state = filesystem.state.lock().unwrap();
        assert_eq!(
            state
                .events
                .iter()
                .filter(|event| **event == "commit")
                .count(),
            3
        );
        assert_eq!(
            state
                .events
                .iter()
                .filter(|event| **event == "read")
                .count(),
            2
        );
    }

    #[test]
    fn mutation_should_retry_temporary_name_collisions_with_fresh_nonces() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        filesystem.set_prepare_collisions(2);
        let paths = paths(filesystem.clone());
        let store = ManagedHostsStore::new(&paths);

        store
            .upsert(host("work", "work.example"), &[], None)
            .unwrap();

        let state = filesystem.state.lock().unwrap();
        assert_eq!(state.preparation_nonces.len(), 3);
        assert!(
            state
                .preparation_nonces
                .windows(2)
                .all(|pair| pair[0] != pair[1])
        );
    }

    #[test]
    fn mutation_should_stop_after_the_portable_retry_bound() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        filesystem.set_conflicts(MUTATION_ATTEMPTS);
        let paths = paths(filesystem);
        let store = ManagedHostsStore::new(&paths);

        let result = store.upsert(host("work", "work.example"), &[], None);

        assert!(matches!(result, Err(ManagedHostsError::ConcurrentMutation)));
    }

    #[test]
    fn ensure_exists_should_publish_the_canonical_empty_file() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = paths(filesystem);
        let store = ManagedHostsStore::new(&paths);

        store.ensure_exists().unwrap();

        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn parser_should_reject_noncanonical_and_unsafe_content() {
        assert_eq!(
            parse_managed_hosts(b"Host *\n  HostName example\n"),
            Err(ManagedHostsFormatError::NonCanonical)
        );
        assert!(
            ManagedSshHost::new(
                "-oProxyCommand=x".into(),
                "example".into(),
                None,
                None,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn storage_errors_should_not_expose_paths_or_native_failures() {
        assert_eq!(
            format!("{:?}", ManagedHostsError::StorageUnavailable),
            "StorageUnavailable"
        );
        assert_eq!(
            ManagedHostsError::StorageUnavailable.to_string(),
            "managed SSH storage is unavailable"
        );
        assert_eq!(
            ManagedHostsError::AliasCollision.to_string(),
            "the SSH alias is already configured"
        );
    }

    #[test]
    fn managed_host_debug_should_redact_all_connection_values() {
        let host = ManagedSshHost::new(
            "sensitive-alias".into(),
            "sensitive.example".into(),
            Some("sensitive-user".into()),
            None,
            Some("/sensitive/key".into()),
        )
        .unwrap();

        let debug = format!("{host:?}");
        assert_eq!(debug, "ManagedSshHost(<redacted>)");
        assert!(!debug.contains("sensitive"));
    }
}
