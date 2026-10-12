//! Shared content-free failures and checked native comparison facts.

use std::{fmt, num::NonZeroU64};

pub use crate::startup::{
    AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId, NativeStartupRegistration,
    ProtectedRegistration, StartupError,
};

/// A native boundary failure. No variant contains native text or sensitive data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NativeFailure {
    /// Unsupported Host.
    UnsupportedHost = 1,
    /// Native Unavailable.
    NativeUnavailable = 2,
    /// Custody Unavailable.
    CustodyUnavailable = 3,
    /// Account Identity Unavailable.
    AccountIdentityUnavailable = 4,
    /// Registration Missing.
    RegistrationMissing = 5,
    /// Storage Locked.
    StorageLocked = 6,
    /// Storage Unavailable.
    StorageUnavailable = 7,
    /// Unsafe Native State.
    UnsafeNativeState = 8,
    /// Invalid Record.
    InvalidRecord = 9,
    /// Unsupported Schema.
    UnsupportedSchema = 10,
    /// Legacy Registration Requires Migration.
    LegacyRegistrationRequiresMigration = 11,
    /// Identity Mismatch.
    IdentityMismatch = 12,
    /// Generation Mismatch.
    GenerationMismatch = 13,
    /// Stale Assessment.
    StaleAssessment = 14,
    /// Confirmation Required.
    ConfirmationRequired = 15,
    /// Authorization Denied.
    AuthorizationDenied = 16,
    /// Authorization Cancelled.
    AuthorizationCancelled = 17,
    /// Authorization Unavailable.
    AuthorizationUnavailable = 18,
    /// Installation Unavailable.
    InstallationUnavailable = 19,
    /// Artifact Rejected.
    ArtifactRejected = 20,
    /// Credential Unavailable.
    CredentialUnavailable = 21,
    /// Credential Policy Rejected.
    CredentialPolicyRejected = 22,
    /// Interaction Required.
    InteractionRequired = 23,
    /// Supervisor Unavailable.
    SupervisorUnavailable = 24,
    /// Background Items Suppressed.
    BackgroundItemsSuppressed = 25,
    /// Process Identity Mismatch.
    ProcessIdentityMismatch = 26,
    /// Privilege Drop Unproven.
    PrivilegeDropUnproven = 27,
    /// Execution Binding Unproven.
    ExecutionBindingUnproven = 28,
    /// Timed Out.
    TimedOut = 29,
    /// Limit Exceeded.
    LimitExceeded = 30,
    /// Outcome Pending.
    OutcomePending = 31,
    /// Indeterminate.
    Indeterminate = 32,
    /// Cleanup Unresolved.
    CleanupUnresolved = 33,
    /// Io Unavailable.
    IoUnavailable = 34,
    /// Ask an administrator to require authentication for this account's sudo commands.
    LinuxPasswordlessSudo = 35,
    /// Use an account outside the docker group, or use rootless containers.
    LinuxDockerMembership = 36,
    /// Use an account outside the lxd group.
    LinuxLxdMembership = 37,
    /// Remove this account's access to the rootful container service.
    LinuxContainerBrokerAccess = 38,
    /// Remove this account's raw-device permissions or use another account.
    LinuxRawDiskAccess = 39,
    /// Remove the delegated file or administrator bypass authority.
    LinuxDelegatedDacAuthority = 40,
    /// Remove delegated tracing access for this account.
    LinuxDelegatedTracing = 41,
    /// Remove delegated profiling access for this account.
    LinuxDelegatedProfiling = 42,
    /// Ask an administrator to restrict system-wide profiling.
    LinuxSystemWideProfiling = 43,
    /// Use a supported sudo and polkit policy.
    LinuxUnsupportedAuthorizationPolicy = 44,
    /// Ask an administrator to configure the supported broker profile.
    LinuxUnsupportedPrivilegedBroker = 45,
    /// Restore the required native policy checks, then retry.
    LinuxPolicyCheckUnavailable = 46,
    // 47 is reserved and unused. Password-authenticated transient caches are not refusals.
    /// Reboot after removing the bypass rights, then retry.
    LinuxPriorBypassHistoryUnproven = 48,
    /// Repair the protected server installation through native management.
    LinuxPublicationUnavailable = 49,
    /// Move the protected installation to supported host storage.
    LinuxMountWitnessUnavailable = 50,
    /// Restore a supported dumpability configuration and unreadable server executable.
    LinuxUnsafeDumpableTransition = 51,
    /// Ask an administrator to install the authenticated native publisher.
    BootstrapTrustUnavailable = 52,
}

impl fmt::Display for NativeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native failure {}", *self as u32)
    }
}
impl std::error::Error for NativeFailure {}

/// The original durability classification is never upgraded by a later synchronization.
#[derive(Debug, Eq, PartialEq)]
pub enum PublicationDisposition<T> {
    /// The operation and its result were durably committed.
    Durable(T),
    /// No effect was admitted.
    NotApplied(NativeFailure),
    /// Effects or persistence are uncertain; admission must close until validated recovery.
    Indeterminate,
}

/// A nonzero native request identifier, separate from client operation identities.
#[derive(Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NativeRequestId([u8; 16]);
impl NativeRequestId {
    /// Checks comparison bytes without creating authorization.
    pub fn new(bytes: [u8; 16]) -> Result<Self, NativeFailure> {
        nonzero(&bytes)?;
        Ok(Self(bytes))
    }
    /// Returns the exact canonical identifier bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}
impl fmt::Debug for NativeRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeRequestId(..)")
    }
}

/// Immutable candidate input identity, not artifact authentication or installation authority.
#[derive(Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct VerifiedCandidateId(NativeRequestId);
impl VerifiedCandidateId {
    /// Checks comparison bytes. The distribution controller separately retains the verified candidate.
    pub fn new(bytes: [u8; 16]) -> Result<Self, NativeFailure> {
        NativeRequestId::new(bytes).map(Self)
    }
    /// Returns the exact candidate identity, without truncating an artifact digest.
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}
impl fmt::Debug for VerifiedCandidateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerifiedCandidateId(..)")
    }
}

/// An owning-account data generation identifier, not a storage handle.
#[derive(Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct DataGenerationId(NonZeroU64);
impl DataGenerationId {
    /// Checks a nonzero allocated identifier.
    pub fn new(value: u64) -> Result<Self, NativeFailure> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(NativeFailure::InvalidRecord)
    }
    /// Returns the full allocated value.
    pub fn get(self) -> u64 {
        self.0.get()
    }
}
impl fmt::Debug for DataGenerationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataGenerationId(..)")
    }
}

pub(crate) fn nonzero(bytes: &[u8]) -> Result<(), NativeFailure> {
    if bytes.iter().all(|b| *b == 0) {
        Err(NativeFailure::InvalidRecord)
    } else {
        Ok(())
    }
}
pub(crate) fn validate_key(key: AccountBuildKey) -> Result<(), NativeFailure> {
    if matches!(key.account_uid, 0 | u32::MAX) {
        Err(NativeFailure::InvalidRecord)
    } else {
        Ok(())
    }
}
pub(crate) fn build_code(build: BuildIdentity) -> u8 {
    match build {
        BuildIdentity::Release => 1,
        BuildIdentity::Preflight => 2,
        BuildIdentity::Development => 3,
    }
}
pub(crate) fn build_name(build: BuildIdentity) -> &'static str {
    match build {
        BuildIdentity::Release => "release",
        BuildIdentity::Preflight => "preflight",
        BuildIdentity::Development => "development",
    }
}

impl TryFrom<String> for NativeRequestId {
    type Error = NativeFailure;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(super::bindings::unhex(&value)?)
    }
}
impl From<NativeRequestId> for String {
    fn from(value: NativeRequestId) -> Self {
        super::bindings::hex(value.as_bytes())
    }
}

/// Bounded asynchronous management result supplied by the owning runtime/store.
pub type NativeManagementFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, NativeFailure>> + Send + 'a>>;

// OS-authenticated authorization integration was not seeded in this checkout. There is
// deliberately no public constructor, decode route, or caller assertion for authority.
pub(crate) enum NativeAuthorization {}

/// Noncloneable native administrator authority, independently authenticated by the manager.
pub struct AuthorizedNativeRequest {
    _authority: NativeAuthorization,
}

pub use super::account::{AccountBinding, NativeAccountId};
pub use super::profile::{BootCredentialBinding, RegisteredEndpoint, RegisteredRoots};
pub use crate::boot_credentials::{
    BootCredentialAdapter, CredentialPolicyFacts, CredentialReleasePermit, LockedSecret32,
};
pub use crate::startup::{
    NativeStartupAdmission, NativeStartupAuthority, OneShotLauncher, PendingStartup,
    PendingStartupContext, PostExecProof, ProcessIdentity, UnprivilegedWaitPlan,
};

pub use crate::installation::RetainedRegistration;
pub use crate::native_deployment::publication::{InstallationPublisher, StagedInstallation};
pub use crate::native_management::facade::{
    ActivationAssessment, CollectionAssessment, ConfirmedNativeAction, InitialRootApproval,
    NativeAction, NativeAssessment, NativeOperation, NativeProgress, NativeStatus,
    OwnerEnrollmentClaim, RemoteAccessAssessment, RepairAssessment, RuntimeAssessment,
};
