//! Portable authorization seam for the system permissions terminal-hosted computer-use tools need.
//!
//! A computer-use tool running in a Terminal Session takes screenshots and sends input through
//! SpaceTerm's grants, so SpaceTerm reads, requests, and recovers them for the tool.

/// One system permission that computer-use tools in a Terminal Session inherit from SpaceTerm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ComputerUsePermission {
    /// Capturing the screen, which a tool needs to take screenshots.
    ScreenRecording,
    /// Controlling other applications, which a tool needs to click and type. macOS presents it as
    /// Device Control and Data Access.
    Accessibility,
}

/// The current application's authorization for one permission.
///
/// The Operating System reports only whether the grant is usable now. A denial, a restriction,
/// and a request nobody answered read the same.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ComputerUseAuthorization {
    NotGranted,
    Granted,
}

/// Content-free failures from native computer-use authorization and recovery operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ComputerUseAccessError {
    #[error("computer-use access is unavailable off the main thread")]
    OffMainThread,
    #[error("computer-use access is unavailable on this platform")]
    PlatformUnavailable,
    #[error("the platform rejected the computer-use permission operation")]
    PlatformRejected,
}

pub(crate) type ComputerUseResetCompletion =
    Box<dyn FnOnce(Result<(), ComputerUseAccessError>) + Send>;

/// Native computer-use authorization and its explicit recovery operations.
///
/// Every operation acts on the running application's own grant for one permission. None of them
/// captures the screen or sends input to test access.
pub(crate) trait ComputerUseAccess {
    fn authorization(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<ComputerUseAuthorization, ComputerUseAccessError>;

    /// Asks the Operating System to prompt for the permission.
    ///
    /// The system prompts at most once per application identity and reports no decision here, so
    /// callers read authorization again afterward.
    fn request_authorization(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<(), ComputerUseAccessError>;

    fn open_settings(&self, permission: ComputerUsePermission)
    -> Result<(), ComputerUseAccessError>;

    /// Whether [`Self::reset`] can act on exactly the running application's identity.
    fn can_reset(&self) -> bool;

    /// Forgets the Operating System's decision for one permission and the running application
    /// only, so the next request prompts again.
    ///
    /// The completion may run on any thread.
    fn reset(
        &self,
        permission: ComputerUsePermission,
        completion: ComputerUseResetCompletion,
    ) -> Result<(), ComputerUseAccessError>;
}
