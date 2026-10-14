//! Coordinator-owned cross-track ports. Producer-owned modules retain all native authority.

pub use super::authority::{
    NativeTransactionAuthority, NativeTransactionLease, UnavailableNativeTransactionAuthority,
};
pub use super::contracts::{
    AuthorizedNativeRequest, BootCredentialAdapter, CredentialReleasePermit,
    NativeStartupAuthority, OneShotLauncher, PendingStartup, PostExecProof,
};
pub use super::publication::{
    InstallationPublisher, StagedInstallation, UnavailableInstallationPublisher,
};
pub use crate::installation::{
    ApprovedInstallationRetention, MigrationProgress, QuiescenceProgress, RetainedRegistration,
    ServerDataCompatibilityPort, ServerLifecyclePort, ServerMigrationPort,
};
pub use crate::native_management::facade::{
    GuiNativeManagement, NativeAction, NativeAssessment, NativeManagement, NativeOperation,
    NativeProgress, NativeStatus, UnavailableNativeManagement,
};
pub use crate::native_receipts::{
    NativeCleanupBinding, NativeEffectBinding, NativeReceipt, NativeReceiptStore,
    UnavailableNativeReceiptStore,
};

use super::contracts::{BootCredentialBinding, NativeFailure, PendingStartupContext};

/// Retained permit-to-native binding lookup, shared by custody, launcher, and Keychain owners.
/// A decoded credential binding or comparison context must never provide this implementation.
pub trait CredentialReleaseBindings: Send + Sync {
    /// Returns the exact retained startup context under the permit's live release fence.
    fn context<'a>(
        &self,
        permit: &'a CredentialReleasePermit,
    ) -> Result<&'a PendingStartupContext, NativeFailure>;
    /// Returns the fixed credential binding retained by this permit, not a caller selector.
    fn credential_binding<'a>(
        &self,
        permit: &'a CredentialReleasePermit,
    ) -> Result<&'a BootCredentialBinding, NativeFailure>;
}

/// Unavailable permit binding until A3 and B2 supply retained release state.
#[derive(Default)]
pub struct UnavailableCredentialReleaseBindings;

impl CredentialReleaseBindings for UnavailableCredentialReleaseBindings {
    fn context<'a>(
        &self,
        _permit: &'a CredentialReleasePermit,
    ) -> Result<&'a PendingStartupContext, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
    fn credential_binding<'a>(
        &self,
        _permit: &'a CredentialReleasePermit,
    ) -> Result<&'a BootCredentialBinding, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}

impl PendingStartup {
    #[expect(
        dead_code,
        reason = "native-authority A1 supplies retained Linux custody"
    )]
    pub(crate) fn linux_custody(
        &self,
    ) -> Result<&crate::linux_custody::NativeLinuxCustody, NativeFailure> {
        super::authority::pending_linux_custody(self)
    }

    #[expect(
        dead_code,
        reason = "native-authority A2 supplies retained server execution"
    )]
    pub(crate) fn retained_server(&self) -> Result<&std::fs::File, NativeFailure> {
        super::authority::pending_retained_server(self)
    }

    #[expect(
        dead_code,
        reason = "native-authority A2 supplies retained credential tool execution"
    )]
    pub(crate) fn retained_credential_tool(&self) -> Result<&std::fs::File, NativeFailure> {
        super::authority::pending_retained_credential_tool(self)
    }
}

impl PostExecProof {
    #[expect(
        dead_code,
        reason = "native-handoff B1 supplies retained post-exec observation"
    )]
    pub(crate) fn from_linux(lease: crate::linux_launcher::NativeLinuxPostExecLease) -> Self {
        lease.into_proof()
    }
}

impl CredentialReleasePermit {
    #[expect(
        dead_code,
        reason = "native-authority A3 supplies retained release custody"
    )]
    pub(crate) fn linux_custody(
        &self,
    ) -> Result<&crate::linux_custody::NativeLinuxCustody, NativeFailure> {
        super::authority::release_linux_custody(self)
    }

    #[expect(
        dead_code,
        reason = "native-authority A3 supplies retained release tool execution"
    )]
    pub(crate) fn retained_credential_tool(&self) -> Result<&std::fs::File, NativeFailure> {
        super::authority::release_retained_credential_tool(self)
    }
}
