//! Fixed native management facade, owned by native-management C1.

use super::{
    controller::{AuthorizationRoute, ManagementProgress},
    request::{ManagementRequest, OperationTag},
};
use crate::native_deployment::profile::RegisteredRoots;
use sha2::{Digest, Sha256};
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

#[path = "authorization.rs"]
pub mod authorization;
#[path = "backend.rs"]
pub mod backend;
#[path = "caller.rs"]
pub mod caller;
#[path = "entry.rs"]
pub mod entry;
#[path = "socket_caller.rs"]
pub mod socket_caller;
#[path = "transaction.rs"]
pub mod transaction;
#[path = "transport.rs"]
pub mod transport;

use super::SudoMode;
use spaceterm_host::native_deployment::contracts::{
    AccountBuildKey, InstallationId, NativeFailure, NativeManagementFuture, NativeRequestId,
    VerifiedCandidateId,
};

/// Initial account-data directory proposal. It supplies no installation authority.
#[derive(Debug)]
pub struct InitialRootApproval(RegisteredRoots);
impl InitialRootApproval {
    /// Retains the explicitly selected account-data roots, never privileged roots.
    pub fn new(roots: RegisteredRoots) -> Self {
        Self(roots)
    }
    /// Returns only account-data directories for the installation owner to approve.
    pub fn roots(&self) -> &RegisteredRoots {
        &self.0
    }
}

macro_rules! reference {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $name([u8; 32]);
        impl $name {
            /// Checks an opaque reference to the owning policy's retained assessment.
            /// This is comparison data. The native owner must resolve and revalidate it.
            pub fn new(reference: [u8; 32]) -> Result<Self, NativeFailure> {
                if reference == [0; 32] {
                    return Err(NativeFailure::InvalidRecord);
                }
                Ok(Self(reference))
            }
            /// Returns the exact reference, not a new runtime or pairing identity.
            pub fn reference(&self) -> &[u8; 32] {
                &self.0
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "(..)"))
            }
        }
    };
}
reference!(
    RuntimeAssessment,
    "Reference to the owning runtime's exact retained assessment."
);
reference!(
    ActivationAssessment,
    "Reference to the exact installation activation assessment."
);
reference!(
    RepairAssessment,
    "Reference to the installation owner's repair assessment."
);
reference!(
    CollectionAssessment,
    "Reference to the installation owner's collection assessment."
);
reference!(
    RemoteAccessAssessment,
    "Reference to the pairing owner's remote-access assessment."
);
reference!(
    OwnerEnrollmentClaim,
    "Reference to the pairing owner's owner-enrollment claim."
);

/// Supervisor observation is not authenticated runtime readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeSupervision {
    /// Supervision is disabled.
    Disabled,
    /// Native supervisor configuration is enabled.
    Enabled,
    /// Native observation is unavailable.
    Unavailable,
    /// The OS requires Background Items approval.
    RequiresApproval,
}
/// Bounded native status. It contains no process selectors, paths, or native text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeStatus {
    /// Registered intent, absent only when registration is missing.
    pub desired: Option<crate::startup::DesiredServerState>,
    /// Independently observed supervisor state.
    pub supervision: NativeSupervision,
    /// The authenticated runtime owner's readiness observation.
    pub authenticated_ready: bool,
    /// Approved installation, never the newest directory entry.
    pub approved: Option<InstallationId>,
    /// Authenticated staged candidate, not startup selection.
    pub staged: Option<InstallationId>,
    /// An unresolved original request.
    pub pending: Option<NativeRequestId>,
    /// The native owner's durability uncertainty.
    pub indeterminate: bool,
}
/// Original assessment retained by the native owner, not OS authorization.
pub struct NativeAssessment {
    request: ManagementRequest,
    action: NativeAction,
    issued: Duration,
}
impl NativeAssessment {
    /// Checks the native owner's reply against the exact proposed action.
    /// `issued` is its original boot-bound monotonic issuance time, not prompt completion.
    pub fn from_native(
        request: ManagementRequest,
        action: NativeAction,
        issued: Duration,
    ) -> Result<Self, NativeFailure> {
        if request.operation() != action.tag() || request.action_digest() != &action.digest() {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(Self {
            request,
            action,
            issued,
        })
    }
    /// Original request comparison facts; privileged admission reconstructs them independently.
    pub fn request(&self) -> &ManagementRequest {
        &self.request
    }
    /// Original native monotonic issuance time.
    pub fn issued(&self) -> Duration {
        self.issued
    }
    /// Explicitly confirms only this exact assessment. It does not request OS authority.
    pub fn confirm(self) -> ConfirmedNativeAction {
        ConfirmedNativeAction(self)
    }
}
impl std::fmt::Debug for NativeAssessment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeAssessment(..)")
    }
}
/// Noncloneable exact-action confirmation. It is not an administrator grant.
#[derive(Debug)]
pub struct ConfirmedNativeAction(NativeAssessment);
impl ConfirmedNativeAction {
    /// Returns the exact original assessment for native reconstruction.
    pub fn assessment(&self) -> &NativeAssessment {
        &self.0
    }
}
/// Bounded progress for the original request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeProgress(ManagementProgress);
impl NativeProgress {
    /// Checks correlation before exposing native progress to a client.
    pub fn from_native(
        request: NativeRequestId,
        progress: ManagementProgress,
    ) -> Result<Self, NativeFailure> {
        if progress.request != *request.as_bytes() {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(Self(progress))
    }
    /// Returns the content-free outcome and phase.
    pub fn progress(&self) -> ManagementProgress {
        self.0
    }
}

/// Closed native management operation. It contains no privileged command or executable selector.
pub enum NativeOperation {
    /// Initial authenticated installation with explicitly approved account-data roots.
    Install {
        /// Distribution-retained candidate identity.
        candidate: VerifiedCandidateId,
        /// Explicit account-data root approval.
        roots: InitialRootApproval,
    },
    /// Stage an authenticated candidate without selecting it for execution.
    Stage {
        /// Distribution-retained candidate identity.
        candidate: VerifiedCandidateId,
    },
    /// Activate the assessed installation without silently changing Stopped intent.
    Activate {
        /// Exact assessed installation.
        installation: InstallationId,
        /// Original exact activation assessment.
        assessment: ActivationAssessment,
    },
    /// Start only the approved runtime.
    Start,
    /// Durably stop and retire the exact assessed runtime.
    Stop {
        /// Original exact runtime assessment.
        assessment: RuntimeAssessment,
    },
    /// One Running-only journaled restart of approved code.
    Restart {
        /// Original exact runtime assessment.
        assessment: RuntimeAssessment,
    },
    /// Remove only the exact assessed installation after drainage.
    Uninstall {
        /// Original exact runtime assessment.
        assessment: RuntimeAssessment,
    },
    /// Repair the exact assessed registration.
    Repair {
        /// Original repair assessment.
        assessment: RepairAssessment,
    },
    /// Collect only generations authorized by the exact assessment.
    Collect {
        /// Original collection assessment.
        assessment: CollectionAssessment,
    },
    /// Delegate narrowly scoped remote access to its existing owner.
    AuthorizeRemoteAccess {
        /// Original pairing-owner assessment.
        assessment: RemoteAccessAssessment,
    },
    /// Delegate narrowly scoped owner enrollment to its existing owner.
    AuthorizeOwnerEnrollment {
        /// Original pairing-owner claim.
        claim: OwnerEnrollmentClaim,
    },
}

/// The existing CLI-facing native action vocabulary.
pub type NativeAction = NativeOperation;

/// Fixed native management facade. Facts and parsed requests do not establish authorization.
pub trait NativeManagement: Send + Sync {
    /// Inspects fixed native state without changing intent or requesting credentials.
    fn inspect(&self, key: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus>;
    /// Assesses one exact action without extending its original expiry during authorization.
    fn assess(
        &self,
        key: AccountBuildKey,
        action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment>;
    /// Executes the confirmed action through fixed controlling-terminal or noninteractive sudo.
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
        mode: SudoMode,
    ) -> NativeManagementFuture<'_, NativeProgress>;
    /// Resolves the original request without admitting another operation.
    fn resolve(&self, request: NativeRequestId) -> NativeManagementFuture<'_, NativeProgress>;
}

/// Fixed GUI authorization path, separate from lifecycle confirmation.
pub trait GuiNativeManagement: Send + Sync {
    /// Revalidates the original confirmed action after native GUI authorization.
    fn execute_gui(
        &self,
        confirmed: ConfirmedNativeAction,
    ) -> NativeManagementFuture<'_, NativeProgress>;
}

/// Unavailable facade until C1 supplies actual caller, request, account, and expiry binding.
#[derive(Default)]
pub struct UnavailableNativeManagement;

impl NativeManagement for UnavailableNativeManagement {
    fn inspect(&self, _key: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus> {
        Box::pin(async { Err(NativeFailure::NativeUnavailable) })
    }
    fn assess(
        &self,
        _key: AccountBuildKey,
        _action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment> {
        Box::pin(async { Err(NativeFailure::AuthorizationUnavailable) })
    }
    fn execute(
        &self,
        _confirmed: ConfirmedNativeAction,
        _mode: SudoMode,
    ) -> NativeManagementFuture<'_, NativeProgress> {
        Box::pin(async { Err(NativeFailure::AuthorizationUnavailable) })
    }
    fn resolve(&self, _request: NativeRequestId) -> NativeManagementFuture<'_, NativeProgress> {
        Box::pin(async { Err(NativeFailure::NativeUnavailable) })
    }
}

impl GuiNativeManagement for UnavailableNativeManagement {
    fn execute_gui(
        &self,
        _confirmed: ConfirmedNativeAction,
    ) -> NativeManagementFuture<'_, NativeProgress> {
        Box::pin(async { Err(NativeFailure::AuthorizationUnavailable) })
    }
}

impl NativeOperation {
    /// Fixed wire operation tag. No textual command can enter this mapping.
    pub fn tag(&self) -> OperationTag {
        match self {
            Self::Install { .. } => OperationTag::Install,
            Self::Stage { .. } => OperationTag::Stage,
            Self::Activate { .. } => OperationTag::Activate,
            Self::Start => OperationTag::Start,
            Self::Stop { .. } => OperationTag::Stop,
            Self::Restart { .. } => OperationTag::Restart,
            Self::Uninstall { .. } => OperationTag::Uninstall,
            Self::Repair { .. } => OperationTag::Repair,
            Self::Collect { .. } => OperationTag::Collect,
            Self::AuthorizeRemoteAccess { .. } => OperationTag::RemoteAccess,
            Self::AuthorizeOwnerEnrollment { .. } => OperationTag::OwnerEnrollment,
        }
    }
    /// Canonical action comparison binding. A digest is never authorization.
    pub fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"SpaceTerm native management action v1\0");
        digest.update([self.tag() as u8]);
        match self {
            Self::Install { candidate, roots } => {
                digest.update(candidate.as_bytes());
                for root in [
                    roots.0.config(),
                    roots.0.data(),
                    roots.0.state(),
                    roots.0.cache(),
                ] {
                    let bytes = root.as_os_str().as_bytes();
                    digest.update((bytes.len() as u64).to_be_bytes());
                    digest.update(bytes);
                }
            }
            Self::Stage { candidate } => digest.update(candidate.as_bytes()),
            Self::Activate {
                installation,
                assessment,
            } => {
                digest.update(installation.0.get().to_be_bytes());
                digest.update(assessment.reference());
            }
            Self::Start => (),
            Self::Stop { assessment }
            | Self::Restart { assessment }
            | Self::Uninstall { assessment } => digest.update(assessment.reference()),
            Self::Repair { assessment } => digest.update(assessment.reference()),
            Self::Collect { assessment } => digest.update(assessment.reference()),
            Self::AuthorizeRemoteAccess { assessment } => digest.update(assessment.reference()),
            Self::AuthorizeOwnerEnrollment { claim } => digest.update(claim.reference()),
        }
        digest.finalize().into()
    }
}

/// Native read/assessment/transport boundaries selected by client composition.
/// Implementations authenticate the native reply and retain the protected helper closure.
pub trait NativeManagementPort: Send + Sync {
    /// Reads fixed registration and bounded supervisor/runtime observations.
    fn inspect(&self, key: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus>;
    /// Retains the original assessment in the installation owner's native request scope.
    fn assess(
        &self,
        key: AccountBuildKey,
        action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment>;
    /// Uses only a fixed OS authorization route and the independently validated original request.
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
        route: AuthorizationRoute,
    ) -> NativeManagementFuture<'_, ManagementProgress>;
    /// Resolves only an existing request in its authenticated caller/account scope.
    fn resolve(&self, request: NativeRequestId) -> NativeManagementFuture<'_, ManagementProgress>;
}
/// Fixed client facade. It cannot substitute an action after confirmation or infer success from exit.
pub struct FixedNativeManagement<P> {
    port: P,
}
impl<P: NativeManagementPort> FixedNativeManagement<P> {
    /// Injects native boundaries, never a helper path, command, or password provider.
    pub fn new(port: P) -> Self {
        Self { port }
    }
    async fn dispatch(
        &self,
        confirmed: ConfirmedNativeAction,
        route: AuthorizationRoute,
    ) -> Result<NativeProgress, NativeFailure> {
        let request = NativeRequestId::new(*confirmed.0.request.request_bytes())?;
        // The native assessment and its retained action remain inseparable through transport.
        if confirmed.0.action.digest() != *confirmed.0.request.action_digest() {
            return Err(NativeFailure::IdentityMismatch);
        }
        NativeProgress::from_native(request, self.port.execute(confirmed, route).await?)
    }
}
impl<P: NativeManagementPort> NativeManagement for FixedNativeManagement<P> {
    fn inspect(&self, key: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus> {
        self.port.inspect(key)
    }
    fn assess(
        &self,
        key: AccountBuildKey,
        action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment> {
        Box::pin(async move {
            let tag = action.tag();
            let digest = action.digest();
            let assessment = self.port.assess(key, action).await?;
            if assessment.request.key() != key
                || assessment.request.operation() != tag
                || assessment.request.action_digest() != &digest
            {
                return Err(NativeFailure::IdentityMismatch);
            }
            Ok(assessment)
        })
    }
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
        mode: SudoMode,
    ) -> NativeManagementFuture<'_, NativeProgress> {
        let route = match mode {
            SudoMode::Interactive => AuthorizationRoute::CliInteractive,
            SudoMode::NonInteractive => AuthorizationRoute::CliNonInteractive,
        };
        Box::pin(self.dispatch(confirmed, route))
    }
    fn resolve(&self, request: NativeRequestId) -> NativeManagementFuture<'_, NativeProgress> {
        Box::pin(
            async move { NativeProgress::from_native(request, self.port.resolve(request).await?) },
        )
    }
}
impl<P: NativeManagementPort> GuiNativeManagement for FixedNativeManagement<P> {
    fn execute_gui(
        &self,
        confirmed: ConfirmedNativeAction,
    ) -> NativeManagementFuture<'_, NativeProgress> {
        Box::pin(self.dispatch(confirmed, AuthorizationRoute::Gui))
    }
}
