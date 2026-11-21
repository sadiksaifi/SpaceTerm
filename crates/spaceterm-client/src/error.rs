use crate::protocol;

/// An admitted work connection's negotiated protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedStatus {
    pub work: protocol::version::ProtocolVersion,
}

/// Bounded classifications. Native errors and peer-supplied values never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Direct(crate::direct::DirectError),
    #[error("SpaceTerm Core is not set up.")]
    NotSetUp,
    #[error("SpaceTerm Core service unavailable. Sign in and try again.")]
    ServiceUnavailable,
    /// Another service operation, such as Update, Restart, Stop or Start, holds the service.
    /// Launch reports this while it runs. Launch and admission also report it when the
    /// operation outlasts their wait.
    #[error("Another SpaceTerm Core operation is running. Try again when it finishes.")]
    ServiceBusy,
    #[error("Install the SpaceTerm Core service first.")]
    NotInstalled,
    #[error("The local SpaceTerm Core was stopped. Use Start SpaceTerm Core to start it again.")]
    Stopped,
    /// The service's background item is off in Login Items & Extensions, because the person
    /// turned it off or has not approved it yet. The service manager won't run it, and only the
    /// person turns it on there.
    #[error(
        "SpaceTerm Core is off in Login Items. Turn it on in System Settings > General > Login Items & Extensions."
    )]
    LoginItemOff,
    #[error("SpaceTerm Core changed. Review and confirm again.")]
    Changed,
    #[error(
        "SpaceTerm couldn\u{2019}t confirm whether the SpaceTerm Core update finished. Open Settings > Machines to check the update."
    )]
    ActivationUnresolved,
    #[error("Couldn\u{2019}t verify this SpaceTerm Core package.")]
    RuntimeUnavailable,
    /// This build includes no server package to verify. Distinct from a package that failed
    /// verification.
    #[error("This SpaceTerm build doesn\u{2019}t include a SpaceTerm Core package.")]
    CandidateMissing,
    #[error("Administrator authorization required.")]
    AuthorizationRequired,
    #[error("SpaceTerm Core is not running.")]
    NotRunning,
    #[error("SpaceTerm Core storage is unavailable.")]
    StorageUnavailable,
    #[error("This SpaceTerm Core\u{2019}s saved state is from an incompatible SpaceTerm.")]
    IncompatibleState,
    /// The Core's data is older than any generation it migrates, or its migration failed. The
    /// runtime refuses to read it until the person resets it or installs a Stepping Stone.
    #[error("SpaceTerm Core\u{2019}s saved data needs a reset before it can start.")]
    DataResetRequired,
    #[error("SpaceTerm Core storage has unsafe permissions.")]
    UnsafeStorage,
    #[error("SpaceTerm couldn\u{2019}t verify the machine or account for this connection.")]
    AuthenticationRefused,
    /// The two sides share no protocol major. It names the older side.
    #[error("SpaceTerm and this SpaceTerm Core are incompatible.")]
    IncompatibleProtocol(protocol::version::Incompatible),
    #[error(
        "SpaceTerm was updated or moved after it opened. Quit and reopen SpaceTerm to continue."
    )]
    CandidateChanged,
    #[error("The connection to SpaceTerm Core did not complete.")]
    Protocol,
    #[error("Reinstall SpaceTerm and try again.")]
    StartupFailed,
    #[error("Another client controls this Pane.")]
    Controlled,
    #[error("This Terminal Session is unavailable.")]
    SessionUnavailable,
    #[error("This connection no longer controls the Terminal Session.")]
    StaleControl,
    #[error("This operation isn\u{2019}t supported.")]
    Unsupported,
    #[error("Directory unavailable.")]
    DirectoryUnavailable,
    #[error("Couldn\u{2019}t confirm the Terminal Sessions ended.")]
    CleanupUnresolved,
    #[error("Waiting for other clients to reconnect.")]
    Capacity,
}

use crate::pane::Refusal;

impl From<Refusal> for CoreError {
    fn from(value: Refusal) -> Self {
        match value {
            Refusal::Capacity => Self::Capacity,
            Refusal::Controlled => Self::Controlled,
            Refusal::Missing => Self::SessionUnavailable,
            Refusal::Stale => Self::StaleControl,
            Refusal::Unsupported => Self::Unsupported,
            Refusal::Failed => Self::StartupFailed,
            Refusal::DirectoryUnavailable => Self::DirectoryUnavailable,
            Refusal::StorageUnavailable => Self::StorageUnavailable,
            Refusal::CleanupUnresolved => Self::CleanupUnresolved,
            Refusal::ServiceBusy => Self::ServiceBusy,
        }
    }
}
