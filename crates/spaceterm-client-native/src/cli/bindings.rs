//! Bind the CLI to the native/client/pairing owners, never to another journal.

use std::sync::Arc;

use serde::Serialize;
use spaceterm_client::management_cli::{
    AssessmentToken, ConfirmedManagementAction, ManagementAssessment, ManagementFailure,
    ManagementFuture, ProtocolManagement,
};
use spaceterm_core::{
    identity::{DeviceId, PairingId, ServerId},
    management::ManagementProgress,
};
use spaceterm_host::{
    native_deployment::contracts::NativeFailure,
    native_management::{NativeManagement, SudoMode, facade::UnavailableNativeManagement},
    startup::{AccountBuildKey, DesiredServerState, InstallationId},
};

use super::{
    arguments::{ConfirmationMode, PublicFingerprint, PublicRequestId, StableVersion},
    output::CliOutput,
    runner::CliIo,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationAvailability {
    Registered,
    Missing,
    Locked,
    Unavailable,
    Unsafe,
    Unsupported,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessObservation {
    Unknown,
    Absent,
    Observed,
}
/// Presentation facts from the full discovery provider, not a decoded 48-byte
/// registration, process-name scan, caller path or environment selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeObservation {
    pub availability: RegistrationAvailability,
    pub desired: Option<DesiredServerState>,
    pub process: ProcessObservation,
    pub verified: bool,
    pub approved: Option<InstallationId>,
    pub staged: Option<InstallationId>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeCliCommand {
    Resolve {
        request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    },
    Install {
        version: Option<StableVersion>,
        source: Option<crate::native::artifacts::LocalArtifactSource>,
    },
    Start,
    Stop,
    Restart,
    Uninstall,
    RemoteAccess {
        endpoints: Vec<std::net::SocketAddr>,
    },
    UpdateCheck,
    UpdateStage {
        version: Option<StableVersion>,
        source: Option<crate::native::artifacts::LocalArtifactSource>,
    },
    UpdateActivate {
        installation: InstallationId,
    },
}
/// The bridge is supplied by the native provider. It must preserve that owner's
/// original NativeRequestId, exact assessment and expiry. Only missing Linux
/// linger invokes the supplied authorization adapter. Remote policy is paired
/// protocol work, not a native or administrator-authorized lifecycle operation.
/// This port cannot mutate SQL or construct listener authority.
pub trait NativeCommandBindings: Send + Sync {
    fn inspect(&self, key: AccountBuildKey) -> ManagementFuture<'_, NativeObservation>;
    fn run<'a>(
        &'a self,
        key: AccountBuildKey,
        command: NativeCliCommand,
        confirmation: ConfirmationMode,
        mode: SudoMode,
        io: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicPairedDevice {
    pub server: ServerId,
    pub device: DeviceId,
    pub pairing: PairingId,
}
#[derive(Clone, Eq, PartialEq)]
pub struct InventoryCursor(String);
impl InventoryCursor {
    pub fn new(value: String) -> Result<Self, ManagementFailure> {
        if value.is_empty()
            || value.len() > 1024
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(ManagementFailure::LimitExceeded);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for InventoryCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InventoryCursor(..)")
    }
}
#[derive(Clone)]
pub struct PairingInventoryPage {
    pub devices: Vec<PublicPairedDevice>,
    pub next: Option<InventoryCursor>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationState {
    Confirmed,
    Pending,
    Indeterminate,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnpairReport {
    pub server: ServerId,
    pub device: DeviceId,
    pub pairing: PairingId,
    pub local_connection_removed: bool,
    pub revocation: RevocationState,
}
/// Pairing owns fingerprint verification, additional Client binding, native
/// credentials, owner enrollment, marker-first self-removal and restricted retries.
/// CLI, desktop and companion never borrow each other's ClientId. Owner recovery
/// uses native request authority, not an invented pairing-bound OperationId.
pub trait PairingCommandBindings: Send + Sync {
    /// With no supplied expected pin, ask CliIo for exact public fingerprint
    /// verification. Neither status nor ordinary reconnect calls this method.
    fn enroll<'a>(
        &'a self,
        expected: Option<PublicFingerprint>,
        recover_account: bool,
        mode: SudoMode,
        io: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput>;
    fn authorize(
        &self,
        request: PublicRequestId,
        expected: PublicFingerprint,
    ) -> ManagementFuture<'_, ManagementProgress>;
    fn list_page(
        &self,
        cursor: Option<InventoryCursor>,
    ) -> ManagementFuture<'_, PairingInventoryPage>;
    fn assess_unpair(
        &self,
        device: DeviceId,
        token: Option<AssessmentToken>,
    ) -> ManagementFuture<'_, ManagementAssessment>;
    fn unpair(&self, confirmed: ConfirmedManagementAction) -> ManagementFuture<'_, UnpairReport>;
    /// Drive ONLY the existing client journal scheduler's restricted tasks.
    fn retry_pending(&self) -> ManagementFuture<'_, Vec<UnpairReport>>;
}

/// Fail-closed composition until the provider's opaque native facade has real
/// status/assessment/accessors. No credentials, authority or helper are synthesized.
pub struct UnavailableNativeBindings {
    native: Arc<dyn NativeManagement>,
}
impl Default for UnavailableNativeBindings {
    fn default() -> Self {
        Self {
            native: Arc::new(UnavailableNativeManagement),
        }
    }
}
impl NativeCommandBindings for UnavailableNativeBindings {
    fn inspect(&self, key: AccountBuildKey) -> ManagementFuture<'_, NativeObservation> {
        Box::pin(async move {
            let _ = self.native.inspect(key).await.map_err(classify_native)?;
            Err(ManagementFailure::NativeUnavailable)
        })
    }
    fn run<'a>(
        &'a self,
        _key: AccountBuildKey,
        _command: NativeCliCommand,
        _confirmation: ConfirmationMode,
        _mode: SudoMode,
        _io: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        Box::pin(async { Err(ManagementFailure::AuthorizationUnavailable) })
    }
}
#[derive(Default)]
pub struct UnavailablePairingBindings;
impl PairingCommandBindings for UnavailablePairingBindings {
    fn enroll<'a>(
        &'a self,
        _expected: Option<PublicFingerprint>,
        _recover_account: bool,
        _mode: SudoMode,
        _io: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        Box::pin(async { Err(ManagementFailure::CredentialUnavailable) })
    }
    fn authorize(
        &self,
        _request: PublicRequestId,
        _expected: PublicFingerprint,
    ) -> ManagementFuture<'_, ManagementProgress> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn list_page(
        &self,
        _cursor: Option<InventoryCursor>,
    ) -> ManagementFuture<'_, PairingInventoryPage> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn assess_unpair(
        &self,
        _device: DeviceId,
        _token: Option<AssessmentToken>,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn unpair(&self, _confirmed: ConfirmedManagementAction) -> ManagementFuture<'_, UnpairReport> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn retry_pending(&self) -> ManagementFuture<'_, Vec<UnpairReport>> {
        Box::pin(async { Err(ManagementFailure::CredentialUnavailable) })
    }
}
#[derive(Default)]
pub struct UnavailableProtocolManagement;
impl ProtocolManagement for UnavailableProtocolManagement {
    fn negotiated_major(&self) -> Result<u32, ManagementFailure> {
        Err(ManagementFailure::PairingRequired)
    }
    fn status(&self) -> ManagementFuture<'_, spaceterm_core::management::ServerStatus> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn assess(
        &self,
        _action: spaceterm_client::management_cli::ManagementAction,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn restore_assessment(
        &self,
        _action: spaceterm_client::management_cli::ManagementAction,
        _token: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn execute(
        &self,
        _confirmed: ConfirmedManagementAction,
    ) -> ManagementFuture<'_, ManagementProgress> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn resolve(
        &self,
        _operation: spaceterm_core::operations::OperationId,
    ) -> ManagementFuture<'_, ManagementProgress> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
    fn subscribe_status(
        &self,
    ) -> ManagementFuture<'_, spaceterm_client::management_cli::StatusSubscription> {
        Box::pin(async { Err(ManagementFailure::PairingRequired) })
    }
}

pub fn classify_native(failure: NativeFailure) -> ManagementFailure {
    match failure {
        NativeFailure::StorageLocked => ManagementFailure::StorageLocked,
        NativeFailure::StorageUnavailable | NativeFailure::IoUnavailable => {
            ManagementFailure::StorageUnavailable
        }
        NativeFailure::UnsupportedHost => ManagementFailure::UnsupportedHost,
        NativeFailure::AuthorizationDenied | NativeFailure::AuthorizationCancelled => {
            ManagementFailure::AuthorizationDenied
        }
        NativeFailure::AuthorizationUnavailable | NativeFailure::InteractionRequired => {
            ManagementFailure::AuthorizationUnavailable
        }
        NativeFailure::CredentialUnavailable | NativeFailure::CustodyUnavailable => {
            ManagementFailure::CredentialUnavailable
        }
        NativeFailure::StaleAssessment | NativeFailure::GenerationMismatch => {
            ManagementFailure::StaleAssessment
        }
        NativeFailure::ConfirmationRequired => ManagementFailure::ConfirmationRequired,
        NativeFailure::OutcomePending
        | NativeFailure::CleanupUnresolved
        | NativeFailure::TimedOut => ManagementFailure::OutcomePending,
        NativeFailure::Indeterminate => ManagementFailure::Indeterminate,
        NativeFailure::LimitExceeded => ManagementFailure::LimitExceeded,
        NativeFailure::NativeUnavailable
        | NativeFailure::RegistrationMissing
        | NativeFailure::InstallationUnavailable
        | NativeFailure::SupervisorUnavailable
        | NativeFailure::BackgroundItemsSuppressed
        | NativeFailure::BootstrapTrustUnavailable => ManagementFailure::NativeUnavailable,
        _ => ManagementFailure::UnsafeNativeState,
    }
}

pub const CLI_VERSION: &str = concat!("spaceterm-cli ", env!("SPACETERM_CLI_VERSION"));

/// Uses the host's compiled identity and independently resolved account.
/// Environment values and command-line flags cannot choose another registration.
pub fn invoking_account_key() -> Result<AccountBuildKey, ManagementFailure> {
    spaceterm_host::account_service::ServiceSupervisor::current(None)
        .map(|supervisor| supervisor.key())
        .map_err(crate::native::classify_service)
}
