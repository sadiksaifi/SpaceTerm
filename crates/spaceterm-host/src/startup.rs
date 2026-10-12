//! Exact account/build startup admission without credential access or process execution.

use std::fmt;
use std::num::NonZeroU64;

use thiserror::Error;

/// Isolated SpaceTerm application identity, independent of installation or runtime version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildIdentity {
    /// The shipped application identity.
    Release,
    /// The preflight application identity.
    Preflight,
    /// The development application identity.
    Development,
}

/// Exact native registration lookup key. UID zero and u32::MAX are rejected during admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountBuildKey {
    /// The owning operating-system account's UID, not the launcher's effective UID.
    pub account_uid: u32,
    /// The isolated application identity.
    pub build_identity: BuildIdentity,
}

/// A protected installation's logical identity, not an executable path or startup authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
