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

/// Capture authorization and its explicit recovery operations.
///
/// Callers request authorization only from `NotDetermined`. A denied decision is persistent, so
/// recovery opens the Operating System's privacy list for the device instead of prompting again.
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
}
