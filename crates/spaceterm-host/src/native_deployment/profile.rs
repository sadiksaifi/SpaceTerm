//! Immutable registered roots and credential-selection facts. Reads never resolve new XDG roots.

use super::{
    account::{AccountBinding, NativeAccountId},
    bindings::{self, digest, identity},
    contracts::*,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spaceterm_directories::{AppDirectories, AppDirectoryEnvironment};
use std::{
    fmt,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

const PROFILE_LIMIT: usize = 64 * 1024;

/// Registered application data roots, encoded as Unix pathname bytes in the immutable profile.
#[derive(Clone, Eq, PartialEq)]
pub struct RegisteredRoots {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
}
impl RegisteredRoots {
    /// Resolves the first root proposal through centralized directory policy, without creating it.
    /// Only an authorized Install may approve and publish this proposal.
    pub fn resolve_initial(
        build: BuildIdentity,
        environment: &AppDirectoryEnvironment,
    ) -> Result<Self, NativeFailure> {
        let name = match build {
            BuildIdentity::Release => "spaceterm",
            BuildIdentity::Preflight => "spaceterm-preflight",
            BuildIdentity::Development => "spaceterm-development",
        };
        let dirs = AppDirectories::resolve_xdg(name, environment, None)
            .map_err(|_| NativeFailure::InvalidRecord)?;
        Self::new(dirs.config, dirs.data, dirs.state, dirs.cache)
    }
    /// Checks approved application directory values, not privileged installation paths.
    pub fn new(
        config: PathBuf,
        data: PathBuf,
        state: PathBuf,
        cache: PathBuf,
    ) -> Result<Self, NativeFailure> {
        for p in [&config, &data, &state, &cache] {
            validate_path(p.as_os_str().as_bytes())?;
        }
        Ok(Self {
            config,
            data,
            state,
            cache,
        })
    }
    /// Returns the first-approved config directory.
    pub fn config(&self) -> &Path {
        &self.config
    }
    /// Returns the first-approved data directory.
    pub fn data(&self) -> &Path {
        &self.data
    }
    /// Returns the first-approved state directory.
    pub fn state(&self) -> &Path {
        &self.state
    }
    /// Returns the first-approved cache directory.
    pub fn cache(&self) -> &Path {
        &self.cache
    }
}
impl fmt::Debug for RegisteredRoots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegisteredRoots(..)")
    }
}

/// Supported native host namespaces, not a deployment approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativePlatform {
    /// Apple Silicon macOS.
    Macos,
    /// Linux with independent custody and account-incarnation providers.
    Linux,
}

/// Fixed system-lifecycle endpoint facts. XDG_RUNTIME_DIR never selects this endpoint.
#[derive(Clone, Eq, PartialEq)]
pub struct RegisteredEndpoint {
    platform: NativePlatform,
    address: Vec<u8>,
}
impl RegisteredEndpoint {
    /// Derives the only endpoint for an account/build key on the selected native host.
    pub fn fixed(platform: NativePlatform, key: AccountBuildKey) -> Result<Self, NativeFailure> {
        validate_key(key)?;
        let root = match platform {
            NativePlatform::Macos => "/private/var/run",
            NativePlatform::Linux => "/run",
        };
        Ok(Self {
            platform,
            address: format!(
                "{root}/spaceterm/{}/{}/server.sock",
                build_name(key.build_identity),
                key.account_uid
            )
            .into_bytes(),
        })
    }
    /// Returns the system-lifecycle pathname bytes for an admitted transport adapter.
    pub fn address(&self) -> &[u8] {
        &self.address
    }
}
impl fmt::Debug for RegisteredEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegisteredEndpoint(..)")
    }
}

/// Explicit Linux encryption policy. There is no auto, null, or downgrade mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinuxCredentialMode {
    /// Native host-key binding.
    Host,
    /// Host-key and native TPM2 binding as a conjunction.
    HostAndTpm2,
}

/// Fixed credential-selector facts. No constructor supplies credential-release authority.
#[derive(Clone, Eq, PartialEq)]
pub struct BootCredentialBinding {
    wire: CredentialWire,
}
impl BootCredentialBinding {
    /// Checks the nonreused macOS slot and exact retriever requirement digest.
    pub fn macos(
        slot: [u8; 32],
        retriever_requirement_sha256: [u8; 32],
        account_binding_sha256: [u8; 32],
        server_id: [u8; 16],
    ) -> Result<Self, NativeFailure> {
        let binding = Self {
            wire: CredentialWire::Macos {
                schema: 1,
                slot,
                retriever_requirement_sha256,
                account_binding_sha256,
                server_id,
            },
        };
        binding.validate()?;
        Ok(binding)
    }
    /// Checks an explicit Linux mode and opaque credential slot.
    pub fn linux(
        mode: LinuxCredentialMode,
        slot: [u8; 32],
        account_binding_sha256: [u8; 32],
        server_id: [u8; 16],
    ) -> Result<Self, NativeFailure> {
        let binding = Self {
            wire: CredentialWire::Linux {
                schema: 1,
                mode,
                slot,
                account_binding_sha256,
                server_id,
            },
        };
        binding.validate()?;
        Ok(binding)
    }
    /// Returns a content-free selector digest for the fixed credential name.
    pub fn sha256(&self) -> Result<[u8; 32], NativeFailure> {
        Ok(Sha256::digest(bindings::canonical(&self.wire, 4096)?).into())
    }
    /// Returns Linux's explicit enrollment policy, or rejects a different platform binding.
    pub fn linux_mode(&self) -> Result<LinuxCredentialMode, NativeFailure> {
        match self.wire {
            CredentialWire::Linux { mode, .. } => Ok(mode),
            _ => Err(NativeFailure::IdentityMismatch),
        }
    }
    fn validate(&self) -> Result<(), NativeFailure> {
        let (schema, slot, account, server) = match &self.wire {
            CredentialWire::Macos {
                schema,
                slot,
                retriever_requirement_sha256,
                account_binding_sha256,
                server_id,
            } => {
                nonzero(retriever_requirement_sha256)?;
                (*schema, slot, account_binding_sha256, server_id)
            }
            CredentialWire::Linux {
                schema,
                slot,
                account_binding_sha256,
                server_id,
                ..
            } => (*schema, slot, account_binding_sha256, server_id),
        };
        if schema != 1 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        nonzero(slot)?;
        nonzero(account)?;
        nonzero(server)
    }
    fn matches(&self, platform: NativePlatform, account: &[u8; 32], server: &[u8; 16]) -> bool {
        match &self.wire {
            CredentialWire::Macos {
                account_binding_sha256,
                server_id,
                ..
            } => {
                platform == NativePlatform::Macos
                    && account == account_binding_sha256
                    && server == server_id
            }
            CredentialWire::Linux {
                account_binding_sha256,
                server_id,
                ..
            } => {
                platform == NativePlatform::Linux
                    && account == account_binding_sha256
                    && server == server_id
            }
        }
    }
}
impl fmt::Debug for BootCredentialBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BootCredentialBinding(..)")
    }
}

/// Registered storage identity facts. Existence of a pathname does not establish this binding.
#[derive(Clone, Eq, PartialEq)]
pub struct StorageBinding {
    identity: Vec<u8>,
}
impl StorageBinding {
    /// Checks a bounded native storage identity. OS-backed verification is required separately.
    pub fn new(identity: Vec<u8>) -> Result<Self, NativeFailure> {
        if identity.is_empty() || identity.len() > 512 {
            return Err(NativeFailure::InvalidRecord);
        }
        nonzero(&identity)?;
        Ok(Self { identity })
    }
    /// Returns comparison bytes to the injected native volume observer.
    pub fn identity(&self) -> &[u8] {
        &self.identity
    }
}
impl fmt::Debug for StorageBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StorageBinding(..)")
    }
}

/// A checked immutable profile. Its digest is authenticated by the retained registration head.
#[derive(Clone, Eq, PartialEq)]
pub struct RegistrationProfile {
    wire: ProfileWire,
    account: AccountBinding,
    roots: RegisteredRoots,
    endpoint: RegisteredEndpoint,
    boot: BootCredentialBinding,
    storage: StorageBinding,
    encoded: Vec<u8>,
}
impl RegistrationProfile {
    /// Checks the profile and all cross-field native identity bindings.
    pub fn decode(bytes: &[u8], key: AccountBuildKey) -> Result<Self, NativeFailure> {
        if bytes.len() > PROFILE_LIMIT {
            return Err(NativeFailure::LimitExceeded);
        }
        let wire: ProfileWire =
            serde_json::from_slice(bytes).map_err(|_| NativeFailure::InvalidRecord)?;
        let mut profile = Self::from_wire(wire, key)?;
        profile.encoded = bytes.to_vec();
        Ok(profile)
    }
    /// Constructs facts for a first Install proposal. This does not publish or approve them.
    pub fn new(
        key: AccountBuildKey,
        primary_gid: u32,
        generated_uid: [u8; 16],
        server_id: [u8; 16],
        roots: RegisteredRoots,
        boot: BootCredentialBinding,
        storage: StorageBinding,
    ) -> Result<Self, NativeFailure> {
        let wire = ProfileWire {
            schema_version: 1,
            account_uid: key.account_uid,
            primary_gid,
            build_identity: key.build_identity,
            native_account: AccountWire::Macos { generated_uid },
            server_id,
            roots: RootsWire::from_roots(&roots),
            service_label: service_label(NativePlatform::Macos, key),
            endpoint: EndpointWire {
                platform: NativePlatform::Macos,
                address: STANDARD
                    .encode(RegisteredEndpoint::fixed(NativePlatform::Macos, key)?.address()),
            },
            boot_credential: boot.wire,
            storage_binding: StorageWire {
                kind: StorageKind::NativeVolume,
                identity: STANDARD.encode(storage.identity),
            },
        };
        Self::from_wire(wire, key)
    }
    fn from_wire(wire: ProfileWire, key: AccountBuildKey) -> Result<Self, NativeFailure> {
        validate_key(key)?;
        if wire.schema_version != 1 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        let stored_key = AccountBuildKey {
            account_uid: wire.account_uid,
            build_identity: wire.build_identity,
        };
        validate_key(stored_key)?;
        nonzero(&wire.server_id)?;
        let (platform, native) = match &wire.native_account {
            AccountWire::Macos { generated_uid } => (
                NativePlatform::Macos,
                NativeAccountId::macos(*generated_uid)?,
            ),
            AccountWire::Linux {
                issuer,
                incarnation,
            } => (
                NativePlatform::Linux,
                NativeAccountId::linux(
                    &decode_base64(issuer, 256)?,
                    &decode_base64(incarnation, 256)?,
                )?,
            ),
        };
        let account = AccountBinding::new(stored_key, wire.primary_gid, native)?;
        let roots = wire.roots.decode()?;
        let endpoint = RegisteredEndpoint::fixed(platform, stored_key)?;
        if wire.endpoint.platform != platform
            || decode_base64(&wire.endpoint.address, 4096)? != endpoint.address
            || wire.service_label != service_label(platform, stored_key)
        {
            return Err(NativeFailure::InvalidRecord);
        }
        let boot = BootCredentialBinding {
            wire: wire.boot_credential.clone(),
        };
        boot.validate()?;
        if !boot.matches(platform, &account.sha256(), &wire.server_id) {
            return Err(NativeFailure::IdentityMismatch);
        }
        let storage = StorageBinding::new(decode_base64(&wire.storage_binding.identity, 512)?)?;
        if stored_key != key {
            return Err(NativeFailure::IdentityMismatch);
        }
        let encoded = bindings::canonical(&wire, PROFILE_LIMIT)?;
        Ok(Self {
            wire,
            account,
            roots,
            endpoint,
            boot,
            storage,
            encoded,
        })
    }
    /// Returns deterministic profile bytes for a fresh immutable publication.
    pub fn encode(&self) -> Result<Vec<u8>, NativeFailure> {
        Ok(self.encoded.clone())
    }
    /// Returns approved roots without consulting the discovery process's environment.
    pub fn roots(&self) -> &RegisteredRoots {
        &self.roots
    }
    /// Returns the native account comparison binding.
    pub fn account(&self) -> &AccountBinding {
        &self.account
    }
    /// Returns the fixed endpoint facts.
    pub fn endpoint(&self) -> &RegisteredEndpoint {
        &self.endpoint
    }
    /// Returns the fixed native credential facts, containing no secret.
    pub fn boot_credential(&self) -> &BootCredentialBinding {
        &self.boot
    }
    /// Returns registered storage incarnation facts.
    pub fn storage(&self) -> &StorageBinding {
        &self.storage
    }
    /// Returns canonical core ServerId bytes for cross-record comparison.
    pub fn server_id_bytes(&self) -> &[u8; 16] {
        &self.wire.server_id
    }
}
impl fmt::Debug for RegistrationProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegistrationProfile(..)")
    }
}

/// Returns the fixed native supervisor label, independent of version and installation path.
pub fn service_label(platform: NativePlatform, key: AccountBuildKey) -> String {
    match platform {
        NativePlatform::Linux => format!(
            "spaceterm-server-{}-{}.service",
            build_name(key.build_identity),
            key.account_uid
        ),
        NativePlatform::Macos => {
            let application = match key.build_identity {
                BuildIdentity::Release => "spaceterm",
                BuildIdentity::Preflight => "spaceterm-preflight",
                BuildIdentity::Development => "spaceterm-development",
            };
            format!(
                "io.github.sadiksaifi.{application}.server.{}",
                key.account_uid
            )
        }
    }
}

fn validate_path(bytes: &[u8]) -> Result<(), NativeFailure> {
    if bytes.is_empty()
        || bytes.len() > 4096
        || bytes[0] != b'/'
        || bytes.contains(&0)
        || bytes[1..]
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b"..")
    {
        return Err(NativeFailure::InvalidRecord);
    }
    Ok(())
}
fn decode_base64(text: &str, limit: usize) -> Result<Vec<u8>, NativeFailure> {
    if text.len() > limit.div_ceil(3) * 4 {
        return Err(NativeFailure::LimitExceeded);
    }
    let bytes = STANDARD
        .decode(text)
        .map_err(|_| NativeFailure::InvalidRecord)?;
    if bytes.len() > limit || STANDARD.encode(&bytes) != text {
        return Err(NativeFailure::InvalidRecord);
    }
    Ok(bytes)
}
fn decode_path(text: &str) -> Result<PathBuf, NativeFailure> {
    let bytes = decode_base64(text, 4096)?;
    validate_path(&bytes)?;
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileWire {
    schema_version: u16,
    account_uid: u32,
    primary_gid: u32,
    build_identity: BuildIdentity,
    native_account: AccountWire,
    #[serde(with = "identity")]
    server_id: [u8; 16],
    roots: RootsWire,
    service_label: String,
    endpoint: EndpointWire,
    boot_credential: CredentialWire,
    storage_binding: StorageWire,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "lowercase", deny_unknown_fields)]
enum AccountWire {
    Macos {
        #[serde(with = "identity")]
        generated_uid: [u8; 16],
    },
    Linux {
        issuer: String,
        incarnation: String,
    },
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootsWire {
    config: String,
    data: String,
    state: String,
    cache: String,
}
impl RootsWire {
    fn from_roots(roots: &RegisteredRoots) -> Self {
        let encode = |p: &Path| STANDARD.encode(p.as_os_str().as_bytes());
        Self {
            config: encode(&roots.config),
            data: encode(&roots.data),
            state: encode(&roots.state),
            cache: encode(&roots.cache),
        }
    }
    fn decode(&self) -> Result<RegisteredRoots, NativeFailure> {
        RegisteredRoots::new(
            decode_path(&self.config)?,
            decode_path(&self.data)?,
            decode_path(&self.state)?,
            decode_path(&self.cache)?,
        )
    }
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointWire {
    platform: NativePlatform,
    address: String,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "lowercase", deny_unknown_fields)]
enum CredentialWire {
    Macos {
        schema: u16,
        #[serde(with = "digest")]
        slot: [u8; 32],
        #[serde(with = "digest")]
        retriever_requirement_sha256: [u8; 32],
        #[serde(with = "digest")]
        account_binding_sha256: [u8; 32],
        #[serde(with = "identity")]
        server_id: [u8; 16],
    },
    Linux {
        schema: u16,
        mode: LinuxCredentialMode,
        #[serde(with = "digest")]
        slot: [u8; 32],
        #[serde(with = "digest")]
        account_binding_sha256: [u8; 32],
        #[serde(with = "identity")]
        server_id: [u8; 16],
    },
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StorageKind {
    NativeVolume,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageWire {
    kind: StorageKind,
    identity: String,
}
