//! Portable authorization seam for the capture devices terminal programs use, such as voice and
//! video tools. Their grants belong to this machine's SpaceTerm Server.

/// A capture device whose grant programs in a Terminal Session inherit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureDevice {
    Microphone,
    Camera,
}

/// The server's current Operating-System authorization of one capture device.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "desktops without capture authorization construct only the denied states"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureAuthorization {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
}

/// Content-free failures from capture authorization and recovery operations.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "desktops without capture authorization construct only the denied states"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CaptureAccessError {
    #[error("capture access is unavailable on this platform")]
    PlatformUnavailable,
    #[error("the platform rejected capture permission recovery")]
    PlatformRejected,
}

pub(crate) type CaptureAuthorizationCompletion = Box<dyn FnOnce(CaptureAuthorization) + Send>;

pub(crate) type CaptureResetCompletion = Box<dyn FnOnce(Result<(), CaptureAccessError>) + Send>;

/// Capture authorization and its explicit recovery operations.
///
/// Callers request authorization only from `NotDetermined`. A denied decision is persistent, so
/// recovery opens the Operating System's privacy list for the device, or resets the decision so
/// the next request prompts again.
pub(crate) trait CaptureAccess {
    fn authorization(
        &self,
        device: CaptureDevice,
    ) -> Result<CaptureAuthorization, CaptureAccessError>;

    fn request_authorization(
        &self,
        device: CaptureDevice,
        completion: CaptureAuthorizationCompletion,
    ) -> Result<(), CaptureAccessError>;

    fn open_settings(&self, device: CaptureDevice) -> Result<(), CaptureAccessError>;

    /// Whether [`Self::reset`] can act on exactly the grant holder's own entry.
    fn can_reset(&self) -> bool;

    /// Forgets the Operating System's decision for one device and the grant holder only, so the
    /// device reads as not determined and the next request prompts again.
    ///
    /// The completion follows a verification, so a read made on completion reflects the reset. It
    /// may run on any thread.
    fn reset(
        &self,
        device: CaptureDevice,
        completion: CaptureResetCompletion,
    ) -> Result<(), CaptureAccessError>;
}
