//! Exact account/build startup admission without credential access or process execution.

use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Isolated SpaceTerm application identity, independent of installation or runtime version.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildIdentity {
    /// The shipped application identity.
    Release,
    /// The preflight application identity.
    Preflight,
    /// The development application identity.
    Development,
}

/// Exact native registration lookup key. UID zero and u32::MAX are rejected during admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountBuildKey {
    /// The owning operating-system account's UID, not the launcher's effective UID.
    pub account_uid: u32,
    /// The isolated application identity.
    pub build_identity: BuildIdentity,
}

/// A protected installation's logical identity, not an executable path or startup authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstallationId(
    /// The native installation identifier within its account/build registration.
    pub NonZeroU64,
);

/// A bounded snapshot returned only by the injected protected registration adapter.
///
/// These fields are facts to validate, not authority to execute caller-selected code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtectedRegistration {
    /// The account/build identity retained in the registration.
    pub key: AccountBuildKey,
    /// The retained registration generation.
    pub generation: NonZeroU64,
    /// Whether native supervision is deliberately enabled or suspended.
    pub desired_state: DesiredServerState,
    /// The installation approved for boot, recovery, and explicit Start Server.
    pub approved_installation: InstallationId,
    /// A staged candidate. Its presence never supplies startup authority.
    pub staged_installation: Option<InstallationId>,
}

/// Durable native supervision intent, distinct from whether a process is currently alive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesiredServerState {
    /// Startup may proceed after exact registration validation.
    Running,
    /// Startup must stop before installation preparation or credential access.
    Stopped,
}

/// Content-free failures from protected startup admission.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum StartupError {
    /// Native startup registration support is unavailable for this host.
    #[error("native startup admission is unavailable")]
    NativeUnavailable,
    /// The requested owning account is root.
    #[error("a server cannot be admitted for the root account")]
    RootAccount,
    /// The fixed protected registration is absent.
    #[error("the protected server registration is missing")]
    RegistrationMissing,
    /// Registration storage is locked or otherwise unavailable, not missing.
    #[error("the protected server registration is unavailable")]
    RegistrationUnavailable,
    /// The requested UID is invalid, or the native adapter could not prove protection
    /// or decode valid registration data.
    #[error("the protected server registration is unsafe")]
    UnsafeRegistration,
    /// The registration belongs to another account or build identity.
    #[error("the server registration identity does not match")]
    IdentityMismatch,
    /// The registration generation differs from the exact requested generation.
    #[error("the server registration generation does not match")]
    GenerationMismatch,
    /// The exact approved installation could not be retained safely.
    #[error("the approved server installation is unavailable")]
    InstallationUnavailable,
}

pub(crate) fn validate_requested_key(key: AccountBuildKey) -> Result<(), StartupError> {
    match key.account_uid {
        0 => Err(StartupError::RootAccount),
        u32::MAX => Err(StartupError::UnsafeRegistration),
        _ => Ok(()),
    }
}

/// Native protected registration and installation retention, with no launch operation.
///
/// The composition root supplies this adapter. It must use fixed host-managed storage,
/// bounded decoding, and retained native protection. Paths, XDG environment overrides,
/// installation scans, credentials, and native error text are not caller inputs.
pub trait NativeStartupRegistration {
    /// Native retained installation authority owned by a successful admission ticket.
    /// It must preserve the approved executable and resource identity against replacement.
    type RetainedInstallation;

    /// Reads the fixed protected registration for exactly this account/build key.
    fn read_fixed_registration(
        &self,
        key: AccountBuildKey,
    ) -> Result<ProtectedRegistration, StartupError>;

    /// Retains only the exact approved installation under this registration generation.
    ///
    /// This operation must not load credentials or start a process. It must fail if the
    /// registration changed or the installation cannot be proven protected.
    fn retain_approved_installation(
        &self,
        key: AccountBuildKey,
        generation: NonZeroU64,
        installation: InstallationId,
    ) -> Result<Self::RetainedInstallation, StartupError>;
}

/// Noncloneable, opaque admission for a future one-shot native launcher.
///
/// No public constructor, executable path, environment, credential, or launch method is
/// exposed. Admission alone does not start a server.
#[must_use]
pub struct AdmissionTicket<Installation> {
    _key: AccountBuildKey,
    _generation: NonZeroU64,
    _installation: Installation,
}

impl<Installation> fmt::Debug for AdmissionTicket<Installation> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdmissionTicket(..)")
    }
}

/// Admission either respects deliberate Stop or owns an exact approved installation.
#[must_use]
pub enum StartupAdmission<Installation> {
    /// The validated registration deliberately suspends startup.
    Stopped,
    /// A future launcher may consume this ticket after its own final native checks.
    Ready(AdmissionTicket<Installation>),
}

impl<Installation> fmt::Debug for StartupAdmission<Installation> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("StartupAdmission::Stopped"),
            Self::Ready(ticket) => formatter
                .debug_tuple("StartupAdmission::Ready")
                .field(ticket)
                .finish(),
        }
    }
}

/// Admits only the exact account/build key and nonzero registration generation.
///
/// Required ordering is requested UID validation, fixed protected read, exact identity
/// and generation validation, then the deliberate Stopped guard. Only Running may
/// retain the approved installation. A staged installation is never selected.
///
/// # Errors
/// Rejects UID zero with [`StartupError::RootAccount`] and the native UID sentinel
/// u32::MAX with [`StartupError::UnsafeRegistration`] before checking platform support.
/// Rejects mismatched registration identities or generations and native registration
/// or installation failures. Unsupported hosts return [`StartupError::NativeUnavailable`]
/// before accessing the adapter. No credentials, groups, pipes, or processes are accessed.
pub fn admit_startup<Native: NativeStartupRegistration + ?Sized>(
    key: AccountBuildKey,
    generation: NonZeroU64,
    native: &Native,
) -> Result<StartupAdmission<Native::RetainedInstallation>, StartupError> {
    validate_requested_key(key)?;
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(StartupError::NativeUnavailable);
    }
    let registration = native.read_fixed_registration(key)?;
    if registration.key != key {
        return Err(StartupError::IdentityMismatch);
    }
    if registration.generation != generation {
        return Err(StartupError::GenerationMismatch);
    }
    if registration.desired_state == DesiredServerState::Stopped {
        return Ok(StartupAdmission::Stopped);
    }
    let installation =
        native.retain_approved_installation(key, generation, registration.approved_installation)?;
    Ok(StartupAdmission::Ready(AdmissionTicket {
        _key: key,
        _generation: generation,
        _installation: installation,
    }))
}

use crate::{
    approved_installation::RetainedApprovedInstallation,
    boot_credentials::CredentialReleasePermit,
    installation::RetainedRegistration,
    native_deployment::{
        account::AccountBinding,
        contracts::{DataGenerationId, NativeFailure, NativeRequestId},
        profile::RegistrationProfile,
    },
    native_registration::{RegistrationHeadV2, StartupBarrier, validate_registration_head},
};

/// Exact native process incarnation, not a PID-name match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    /// Positive native process identifier bounded to i32::MAX.
    pub pid: u32,
    /// Native process start seconds.
    pub start_seconds: u64,
    /// Native process start microseconds, below one million.
    pub start_microseconds: u32,
}

/// Comparison-only startup context; a populated value is not pending or release authority.
#[derive(Clone, Eq, PartialEq)]
pub struct PendingStartupContext {
    /// Exact registered account/build key.
    pub key: AccountBuildKey,
    /// Registered primary GID.
    pub primary_gid: u32,
    /// Exact registration generation.
    pub registration_generation: NonZeroU64,
    /// Exact lifecycle intent generation.
    pub lifecycle_generation: NonZeroU64,
    /// Committed data generation.
    pub data_generation: DataGenerationId,
    /// Committed installation ID.
    pub installation: InstallationId,
    /// Canonical core ServerId bytes.
    pub server_id: [u8; 16],
    /// Native incarnation binding.
    pub account_binding_sha256: [u8; 32],
    /// Full approved installation binding.
    pub installation_binding: [u8; 32],
    /// Exact admitted main server file digest.
    pub server_artifact_sha256: [u8; 32],
    /// Exact installation manifest digest.
    pub manifest_sha256: [u8; 32],
    /// Fixed native credential binding digest.
    pub credential_binding_sha256: [u8; 32],
    /// Fresh native cryptographic startup nonce.
    pub startup_nonce: [u8; 32],
    /// Original service-tracked launcher, preserved across exec.
    pub original_process: ProcessIdentity,
    /// Exact short-lived privileged retriever child.
    pub retriever_process: ProcessIdentity,
    /// Validated native journal request when admission is restricted.
    pub journal_request: Option<NativeRequestId>,
}
impl fmt::Debug for PendingStartupContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PendingStartupContext(..)")
    }
}

/// Noncloneable pending native authority. Facts cannot construct it.
pub struct PendingStartup {
    context: Box<PendingStartupContext>,
    _registration: Box<RetainedRegistration>,
    _installation: Box<RetainedApprovedInstallation>,
}
impl PendingStartup {
    /// Returns the exact bounded exchange comparison context.
    pub fn context(&self) -> &PendingStartupContext {
        &self.context
    }
    pub(crate) fn retained_installation(&self) -> &RetainedApprovedInstallation {
        &self._installation
    }
}

/// Native post-exec observation authority, not receiver declarations or wire matches.
pub struct PostExecProof {
    _authority: crate::native_deployment::contracts::NativeAuthorization,
}

/// Unprivileged storage waiting facts. They contain no path or replacement identity.
pub struct UnprivilegedWaitPlan {
    retry: Box<RegisteredStorageRetry>,
}
impl UnprivilegedWaitPlan {
    /// Retains the original classified wait without credentials, code-retention authority or new roots.
    pub fn begin(
        key: AccountBuildKey,
        generation: NonZeroU64,
        native: &dyn StartupInspection,
    ) -> Result<Self, NativeFailure> {
        Ok(Self {
            retry: Box::new(RegisteredStorageRetry::begin(key, generation, native)?),
        })
    }
    /// Returns the exact original registration key.
    pub fn key(&self) -> AccountBuildKey {
        self.retry.head.key
    }
    /// Returns the generation retained before dropping privileges to wait.
    pub fn generation(&self) -> NonZeroU64 {
        self.retry.head.generation
    }
    /// Distinguishes locked storage from unavailable or unmounted storage.
    pub fn failure(&self) -> NativeFailure {
        self.retry.failure
    }
    /// Returns original account, roots and storage comparison bindings. This is not launch authority.
    pub fn profile(&self) -> &RegistrationProfile {
        &self.retry.profile
    }
    /// Returns the original approved installation/data and generations, never a replacement selection.
    pub fn head(&self) -> &RegistrationHeadV2 {
        &self.retry.head
    }
    /// Rechecks the same registration. A newer Stop cancels before application storage inspection.
    pub fn retry(&self, native: &dyn StartupInspection) -> Result<StartupPreflight, NativeFailure> {
        self.retry.retry(native)
    }
}

/// Native admission with an explicit unprivileged waiting route.
pub enum NativeStartupAdmission {
    /// Deliberate Stopped, before retention, credentials, pipes or fork.
    Stopped,
    /// Existing registered storage is unavailable; do not recreate identity or data.
    WaitingStorage(UnprivilegedWaitPlan),
    /// Exact native authority owned by the one-shot launcher.
    Ready(PendingStartup),
}

/// Privileged startup authority chosen by trusted composition, without caller executable inputs.
pub trait NativeStartupAuthority: Send + Sync {
    /// Validates fixed registration and native custody before startup.
    fn admit(
        &self,
        key: AccountBuildKey,
        generation: NonZeroU64,
    ) -> Result<NativeStartupAdmission, NativeFailure>;
    /// Acquires live release authority only after independent native post-exec proof.
    fn authorize_release(
        &self,
        pending: &PendingStartup,
        proof: &PostExecProof,
    ) -> Result<CredentialReleasePermit, NativeFailure>;
    /// Rechecks current Running intent, generations, journal and exact process under release fence.
    fn recheck_release(&self, permit: &CredentialReleasePermit) -> Result<(), NativeFailure>;
}

/// Fixed production startup authority. Unavailable native providers never become synthetic authority.
#[derive(Default)]
pub struct SystemNativeStartupAuthority;
impl NativeStartupAuthority for SystemNativeStartupAuthority {
    fn admit(
        &self,
        key: AccountBuildKey,
        _generation: NonZeroU64,
    ) -> Result<NativeStartupAdmission, NativeFailure> {
        crate::native_deployment::contracts::validate_key(key)?;
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(NativeFailure::UnsupportedHost);
        }
        // Retained sampling in the base does not establish immutable authenticated
        // publication or account incarnation. Do not inspect credentials to paper over it.
        Err(NativeFailure::CustodyUnavailable)
    }
    fn authorize_release(
        &self,
        _pending: &PendingStartup,
        _proof: &PostExecProof,
    ) -> Result<CredentialReleasePermit, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
    fn recheck_release(&self, _permit: &CredentialReleasePermit) -> Result<(), NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}

/// Decoded registration/profile comparison facts from a trusted native inspection edge.
/// This is not RetainedRegistration or launch authority.
pub struct StartupSnapshot {
    /// Exact head facts.
    pub head: RegistrationHeadV2,
    /// Protected profile facts selected by the head.
    pub profile: RegistrationProfile,
    /// Matching bounded journal, if present.
    pub journal: Option<crate::activation_journal::ActivationJournalRecord>,
}

/// Read-only preflight inputs. Implementations belong to trusted native composition.
pub trait StartupInspection: Send + Sync {
    /// Reads fixed bounded comparison facts without retaining code or inspecting credentials.
    fn read_snapshot(&self, key: AccountBuildKey) -> Result<StartupSnapshot, NativeFailure>;
    /// Revalidates native account incarnation and primary GID.
    fn observe_account(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure>;
    /// Distinguishes registered storage identity, locked storage, and an unmounted underlay.
    fn check_storage(&self, snapshot: &StartupSnapshot) -> Result<(), NativeFailure>;
}

/// Comparison-only next step. It cannot start a process or release a credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPreflight {
    /// Stopped guard passed before installation or credential work.
    Stopped,
    /// Existing identity waits unprivileged without creating underlay data.
    WaitingStorage(NativeFailure),
    /// Only this exact approved installation may be retained next.
    RetainApproved(InstallationId),
}

/// Checks startup facts in frozen order. Stopped precedes storage, code retention and credentials.
pub fn inspect_startup(
    key: AccountBuildKey,
    generation: NonZeroU64,
    native: &dyn StartupInspection,
) -> Result<StartupPreflight, NativeFailure> {
    crate::native_deployment::contracts::validate_key(key)?;
    let snapshot = native.read_snapshot(key)?;
    inspect_startup_snapshot(key, generation, &snapshot, native)
}

pub(crate) fn inspect_startup_snapshot(
    key: AccountBuildKey,
    generation: NonZeroU64,
    snapshot: &StartupSnapshot,
    native: &dyn StartupInspection,
) -> Result<StartupPreflight, NativeFailure> {
    use sha2::{Digest, Sha256};
    validate_registration_head(&snapshot.head)?;
    let head = &snapshot.head;
    if head.key != key
        || snapshot.profile.account().key() != key
        || snapshot.profile.server_id_bytes() != &head.server_id
    {
        return Err(NativeFailure::IdentityMismatch);
    }
    if head.generation != generation {
        return Err(NativeFailure::GenerationMismatch);
    }
    let digest: [u8; 32] = Sha256::digest(snapshot.profile.encode()?).into();
    if digest != head.profile_sha256 {
        return Err(NativeFailure::InvalidRecord);
    }
    snapshot
        .profile
        .account()
        .compare(&native.observe_account(key)?)?;
    if head.desired_state == DesiredServerState::Stopped {
        return Ok(StartupPreflight::Stopped);
    }
    let journal_selection = snapshot
        .journal
        .as_ref()
        .map(|journal| journal.recovery_selection(head, &snapshot.profile.account().sha256()))
        .transpose()?;
    match head.startup_barrier {
        StartupBarrier::Open => {
            if journal_selection.is_some_and(|s| {
                !matches!(
                    s,
                    crate::activation_journal::RecoverySelection::Original { .. }
                        | crate::activation_journal::RecoverySelection::Committed { .. }
                )
            }) {
                return Err(NativeFailure::UnsafeNativeState);
            }
        }
        StartupBarrier::ApprovedStartOnly => {
            if !matches!(
                journal_selection,
                Some(crate::activation_journal::RecoverySelection::Committed { .. })
            ) {
                return Err(NativeFailure::UnsafeNativeState);
            }
        }
        StartupBarrier::Inhibited => return Err(NativeFailure::OutcomePending),
        StartupBarrier::RecoveryRequired => return Err(NativeFailure::UnsafeNativeState),
    }
    match native.check_storage(snapshot) {
        Ok(()) => Ok(StartupPreflight::RetainApproved(head.approved_installation)),
        Err(failure @ (NativeFailure::StorageLocked | NativeFailure::StorageUnavailable)) => {
            Ok(StartupPreflight::WaitingStorage(failure))
        }
        Err(failure) => Err(failure),
    }
}

/// Unprivileged retry of one existing registration's storage. This holds comparison facts only,
/// never credentials or launch authority. The native storage peer must check the registered volume
/// before touching application directories, so locked/unmounted storage cannot expose an underlay.
pub struct RegisteredStorageRetry {
    head: RegistrationHeadV2,
    profile: RegistrationProfile,
    failure: NativeFailure,
}
impl fmt::Debug for RegisteredStorageRetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegisteredStorageRetry(..)")
    }
}
impl RegisteredStorageRetry {
    /// Retains original identity, roots and approved data only after a classified storage wait.
    pub fn begin(
        key: AccountBuildKey,
        generation: NonZeroU64,
        native: &dyn StartupInspection,
    ) -> Result<Self, NativeFailure> {
        crate::native_deployment::contracts::validate_key(key)?;
        let snapshot = native.read_snapshot(key)?;
        let failure = match inspect_startup_snapshot(key, generation, &snapshot, native)? {
            StartupPreflight::WaitingStorage(failure) => failure,
            _ => return Err(NativeFailure::UnsafeNativeState),
        };
        Ok(Self {
            head: snapshot.head,
            profile: snapshot.profile,
            failure,
        })
    }

    /// Rechecks native account and storage without rediscovery, replacement identity or XDG resolution.
    /// A newer Stop cancels the wait before storage inspection. A changed approval requires new admission.
    pub fn retry(&self, native: &dyn StartupInspection) -> Result<StartupPreflight, NativeFailure> {
        let snapshot = native.read_snapshot(self.head.key)?;
        if snapshot.head.key != self.head.key
            || snapshot.head.server_id != self.head.server_id
            || snapshot.head.profile_sha256 != self.head.profile_sha256
            || snapshot.profile.encode()? != self.profile.encode()?
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        if snapshot.head.generation < self.head.generation
            || snapshot.head.lifecycle_generation < self.head.lifecycle_generation
            || snapshot.head.approved_installation != self.head.approved_installation
            || snapshot.head.approved_data_generation != self.head.approved_data_generation
            || snapshot.head.approved_descriptor_sha256 != self.head.approved_descriptor_sha256
            || snapshot.head.approved_manifest_sha256 != self.head.approved_manifest_sha256
            || snapshot.head.installation_high_water < self.head.installation_high_water
            || snapshot.head.data_high_water < self.head.data_high_water
        {
            return Err(NativeFailure::GenerationMismatch);
        }
        inspect_startup_snapshot(self.head.key, snapshot.head.generation, &snapshot, native)
    }
}

/// Owned one-shot execution edge. Successful same-PID exec cannot return.
pub trait OneShotLauncher {
    /// Consumes the original wait plan in the native unprivileged driver. The driver must
    /// drop privileges before waiting and reenter the fixed protected startup route on unlock.
    /// Default refusal preserves fail-closed composition when that driver is absent.
    fn wait_for_storage(
        &mut self,
        plan: UnprivilegedWaitPlan,
        _authority: &dyn NativeStartupAuthority,
        _credentials: &dyn crate::boot_credentials::BootCredentialAdapter,
    ) -> Result<(), NativeFailure> {
        Err(plan.failure())
    }
    /// Consumes pending ownership on every path and preserves cleanup uncertainty.
    fn launch(
        &mut self,
        pending: PendingStartup,
        authority: &dyn NativeStartupAuthority,
        credentials: &dyn crate::boot_credentials::BootCredentialAdapter,
    ) -> Result<std::convert::Infallible, NativeFailure>;
}

/// Fixed fail-closed execution edge until the native exchange owner supplies its implementation.
#[derive(Default)]
pub struct UnavailableOneShotLauncher;
impl OneShotLauncher for UnavailableOneShotLauncher {
    fn launch(
        &mut self,
        _pending: PendingStartup,
        _authority: &dyn NativeStartupAuthority,
        _credentials: &dyn crate::boot_credentials::BootCredentialAdapter,
    ) -> Result<std::convert::Infallible, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}

/// Frozen single-threaded launcher composition. Stopped cannot reach the execution adapter.
pub fn launch_once(
    key: AccountBuildKey,
    generation: NonZeroU64,
    authority: &dyn NativeStartupAuthority,
    credentials: &dyn crate::boot_credentials::BootCredentialAdapter,
    launcher: &mut dyn OneShotLauncher,
) -> Result<(), NativeFailure> {
    crate::native_deployment::contracts::validate_key(key)?;
    match authority.admit(key, generation)? {
        NativeStartupAdmission::Stopped => Ok(()),
        NativeStartupAdmission::WaitingStorage(plan) => {
            launcher.wait_for_storage(plan, authority, credentials)
        }
        NativeStartupAdmission::Ready(pending) => {
            match launcher.launch(pending, authority, credentials) {
                Ok(never) => match never {},
                Err(failure) => Err(failure),
            }
        }
    }
}
