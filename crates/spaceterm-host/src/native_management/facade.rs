//! Fixed native management facade, owned by native-management C1.

use std::convert::Infallible;

use super::SudoMode;
use spaceterm_host::native_deployment::contracts::{
    AccountBuildKey, InstallationId, NativeFailure, NativeManagementFuture, NativeRequestId,
    VerifiedCandidateId,
};

macro_rules! opaque {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $name {
            _unavailable: Infallible,
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(..)"))
            }
        }
    };
}

opaque!(
    InitialRootApproval,
    "Explicit account-data root approval, never a privileged-root selector."
);
opaque!(
    RuntimeAssessment,
    "Binding to the owning runtime's exact assessment, not a new Session or operation identity."
);
opaque!(
    ActivationAssessment,
    "Exact approved/staged lifecycle assessment with its original expiry."
);
opaque!(
    RepairAssessment,
    "Exact original registration-repair assessment."
);
opaque!(
    CollectionAssessment,
    "Exact original generation-collection assessment."
);
opaque!(
    RemoteAccessAssessment,
    "Scoped remote-access assessment supplied by the pairing owner."
);
opaque!(
    OwnerEnrollmentClaim,
    "Scoped owner-enrollment claim supplied by the pairing owner."
);
opaque!(
    ConfirmedNativeAction,
    "Noncloneable confirmation bound to the exact original assessment."
);
opaque!(
    NativeStatus,
    "Bounded intent, supervision, readiness, version, and uncertainty facts."
);
opaque!(
    NativeAssessment,
    "Bounded assessment facts retaining the original thirty-second expiry."
);
opaque!(
    NativeProgress,
    "Bounded progress for the original native request, including durable uncertainty."
);

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
