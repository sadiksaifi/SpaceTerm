//! Portable microphone authorization seam for terminal-hosted voice tools.

/// The application's current Operating-System microphone authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MicrophoneAuthorization {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
}

/// Content-free failures from native microphone authorization and recovery operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum MicrophoneAccessError {
    #[error("microphone access is unavailable off the main thread")]
    OffMainThread,
    #[error("microphone access is unavailable on this platform")]
    PlatformUnavailable,
    #[error("the platform rejected microphone permission recovery")]
    PlatformRejected,
}

pub(crate) type MicrophoneAuthorizationCompletion = Box<dyn FnOnce(MicrophoneAuthorization) + Send>;

/// Native microphone authorization and its explicit recovery operations.
///
/// Callers request authorization only from `NotDetermined`. A denied decision is persistent, so
/// recovery opens the Operating System's microphone privacy pane instead of prompting again.
pub(crate) trait MicrophoneAccess {
    fn authorization(&self) -> Result<MicrophoneAuthorization, MicrophoneAccessError>;

    fn request_authorization(
        &self,
        completion: MicrophoneAuthorizationCompletion,
    ) -> Result<(), MicrophoneAccessError>;

    fn open_settings(&self) -> Result<(), MicrophoneAccessError>;
}
