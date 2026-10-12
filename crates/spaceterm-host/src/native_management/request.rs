//! Bounded management transport. Decoding does not authorize an operation.

use crate::native_deployment::contracts::NativeFailure;
use crate::startup::{AccountBuildKey, BuildIdentity};

/// Exact request frame bound.
pub const REQUEST_BYTES: usize = 160;

/// Wire operation tags, not a second operation policy or authority type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OperationTag {
    /// Initial installation.
    Install = 1,
    /// Immutable staging.
    Stage = 2,
    /// Assessed activation.
    Activate = 3,
    /// Explicit Start Server.
    Start = 4,
    /// Assessed Stop Server.
    Stop = 5,
    /// Assessed restart of the approved runtime.
    Restart = 6,
    /// Assessed removal.
    Uninstall = 7,
    /// Assessed registration repair.
    Repair = 8,
    /// Assessed generation collection.
    Collect = 9,
    /// Scoped remote-access authority.
    RemoteAccess = 10,
    /// Scoped owner-enrollment authority.
    OwnerEnrollment = 11,
}

/// Bounded comparison facts. The native authority must independently reconstruct
/// the canonical confirmed action, account incarnation, and fresh assessment.
#[derive(Clone, Eq, PartialEq)]
pub struct ManagementRequest {
    key: AccountBuildKey,
    operation: OperationTag,
    request: [u8; 16],
    registration_generation: u64,
    lifecycle_generation: u64,
    account_binding: [u8; 32],
    action_digest: [u8; 32],
    assessment_binding: [u8; 32],
}

impl std::fmt::Debug for ManagementRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ManagementRequest(..)")
    }
}

impl ManagementRequest {
    /// Decode a single exact frame without reading any external resource.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeFailure> {
        if bytes.len() != REQUEST_BYTES || &bytes[..4] != b"STMG" {
            return Err(NativeFailure::InvalidRecord);
        }
        if bytes[4] != 1 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        if bytes[7] != 0
            || bytes[48..56]
                .iter()
                .chain(&bytes[152..])
                .any(|byte| *byte != 0)
            || bytes[8..12] != (REQUEST_BYTES as u32).to_be_bytes()
        {
            return Err(NativeFailure::InvalidRecord);
        }
        let operation = OperationTag::decode(bytes[5])?;
        let build_identity = match bytes[6] {
            1 => BuildIdentity::Release,
            2 => BuildIdentity::Preflight,
            3 => BuildIdentity::Development,
            _ => return Err(NativeFailure::InvalidRecord),
        };
        let account_uid = u32::from_be_bytes(copy_array(&bytes[12..16]));
        let registration_generation = u64::from_be_bytes(copy_array(&bytes[32..40]));
        let lifecycle_generation = u64::from_be_bytes(copy_array(&bytes[40..48]));
        let request = copy_array(&bytes[16..32]);
        let account_binding = copy_array(&bytes[56..88]);
        let action_digest = copy_array(&bytes[88..120]);
        let assessment_binding = copy_array(&bytes[120..152]);
        if matches!(account_uid, 0 | u32::MAX)
            || registration_generation == 0
            || lifecycle_generation == 0
            || request == [0; 16]
            || account_binding == [0; 32]
            || action_digest == [0; 32]
            || assessment_binding == [0; 32]
        {
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self {
            key: AccountBuildKey {
                account_uid,
                build_identity,
            },
            operation,
            request,
            registration_generation,
            lifecycle_generation,
            account_binding,
            action_digest,
            assessment_binding,
        })
    }
    /// Encode comparison facts in the versioned transport.
    pub fn encode(&self) -> [u8; REQUEST_BYTES] {
        let mut bytes = [0; REQUEST_BYTES];
        bytes[..4].copy_from_slice(b"STMG");
        bytes[4] = 1;
        bytes[5] = self.operation as u8;
        bytes[6] = match self.key.build_identity {
            BuildIdentity::Release => 1,
            BuildIdentity::Preflight => 2,
            BuildIdentity::Development => 3,
        };
        bytes[8..12].copy_from_slice(&(REQUEST_BYTES as u32).to_be_bytes());
        bytes[12..16].copy_from_slice(&self.key.account_uid.to_be_bytes());
        bytes[16..32].copy_from_slice(&self.request);
        bytes[32..40].copy_from_slice(&self.registration_generation.to_be_bytes());
        bytes[40..48].copy_from_slice(&self.lifecycle_generation.to_be_bytes());
        bytes[56..88].copy_from_slice(&self.account_binding);
        bytes[88..120].copy_from_slice(&self.action_digest);
        bytes[120..152].copy_from_slice(&self.assessment_binding);
        bytes
    }
    /// Owning nonroot account and build.
    pub fn key(&self) -> AccountBuildKey {
        self.key
    }
    /// Requested fixed operation.
    pub fn operation(&self) -> OperationTag {
        self.operation
    }
    /// Original request identity for retained resolution.
    pub fn request_bytes(&self) -> &[u8; 16] {
        &self.request
    }
    /// Exact expected registration generation.
    pub fn registration_generation(&self) -> u64 {
        self.registration_generation
    }
    /// Exact expected lifecycle generation.
    pub fn lifecycle_generation(&self) -> u64 {
        self.lifecycle_generation
    }
    /// Comparison digest of the native account binding.
    pub fn account_binding(&self) -> &[u8; 32] {
        &self.account_binding
    }
    /// Digest of the independently reconstructed confirmed operation.
    pub fn action_digest(&self) -> &[u8; 32] {
        &self.action_digest
    }
    /// Native assessment reference. It grants no authority.
    pub fn assessment_binding(&self) -> &[u8; 32] {
        &self.assessment_binding
    }
}

impl OperationTag {
    /// Recognize only the eleven frozen operation tags.
    pub fn decode(tag: u8) -> Result<Self, NativeFailure> {
        match tag {
            1 => Ok(Self::Install),
            2 => Ok(Self::Stage),
            3 => Ok(Self::Activate),
            4 => Ok(Self::Start),
            5 => Ok(Self::Stop),
            6 => Ok(Self::Restart),
            7 => Ok(Self::Uninstall),
            8 => Ok(Self::Repair),
            9 => Ok(Self::Collect),
            10 => Ok(Self::RemoteAccess),
            11 => Ok(Self::OwnerEnrollment),
            _ => Err(NativeFailure::InvalidRecord),
        }
    }
    /// Frozen right/action suffix.
    pub fn right_suffix(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Stage => "stage",
            Self::Activate => "activate",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Uninstall => "uninstall",
            Self::Repair => "repair",
            Self::Collect => "collect",
            Self::RemoteAccess => "remote-access",
            Self::OwnerEnrollment => "owner-enrollment",
        }
    }
}

fn copy_array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut value = [0; N];
    value.copy_from_slice(bytes);
    value
}

/// Stable server namespace shared by Authorization Services and polkit.
pub fn server_identifier(build: BuildIdentity) -> &'static str {
    match build {
        BuildIdentity::Release => "io.github.sadiksaifi.spaceterm.server",
        BuildIdentity::Preflight => "io.github.sadiksaifi.spaceterm-preflight.server",
        BuildIdentity::Development => "io.github.sadiksaifi.spaceterm-development.server",
    }
}

/// Reconstruct an operation-specific right from the fixed build namespace.
/// Account and caller binding are separate mandatory native checks.
pub fn authorization_right(build: BuildIdentity, operation: OperationTag) -> String {
    format!(
        "{}.manage.{}",
        server_identifier(build),
        operation.right_suffix()
    )
}
