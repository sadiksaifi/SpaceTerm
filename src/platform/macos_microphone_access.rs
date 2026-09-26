use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Mutex;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::NSInteger;

use super::microphone_access::{
    MicrophoneAccess, MicrophoneAccessError, MicrophoneAuthorization,
    MicrophoneAuthorizationCompletion,
};
use super::permission_recovery::{PermissionRecovery, PermissionRecoveryError};

const MICROPHONE_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone";
const LEGACY_MICROPHONE_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

pub(crate) struct MacosMicrophoneAccess {
    settings: PermissionRecovery,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl MacosMicrophoneAccess {
    pub(crate) fn new() -> Self {
        Self {
            settings: PermissionRecovery::new(
                Box::new(super::macos_system_settings::NsWorkspaceUrlLauncher::default()),
                MICROPHONE_SETTINGS_URI,
                LEGACY_MICROPHONE_SETTINGS_URI,
                "Open System Settings",
            ),
            _not_send_or_sync: PhantomData,
        }
    }
}

impl MicrophoneAccess for MacosMicrophoneAccess {
    fn authorization(&self) -> Result<MicrophoneAuthorization, MicrophoneAccessError> {
        if MainThreadMarker::new().is_none() {
            return Err(MicrophoneAccessError::OffMainThread);
        }
        // SAFETY: AVFoundation exports this immutable audio media type when available.
        let media_type =
            unsafe { AVMediaTypeAudio }.ok_or(MicrophoneAccessError::PlatformUnavailable)?;
        // SAFETY: AVMediaTypeAudio is a valid input for this AVCaptureDevice class method.
        let raw: NSInteger =
            unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) }.0;
        authorization_from_raw(raw).ok_or(MicrophoneAccessError::PlatformUnavailable)
    }

    fn request_authorization(
        &self,
        completion: MicrophoneAuthorizationCompletion,
    ) -> Result<(), MicrophoneAccessError> {
        if MainThreadMarker::new().is_none() {
            return Err(MicrophoneAccessError::OffMainThread);
        }
        // SAFETY: AVFoundation exports this immutable audio media type when available.
        let media_type =
            unsafe { AVMediaTypeAudio }.ok_or(MicrophoneAccessError::PlatformUnavailable)?;
        let completion = Mutex::new(Some(completion));
        let completion = RcBlock::new(move |granted: Bool| {
            let Some(completion) = completion
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            else {
                return;
            };
            completion(if !granted.as_bool() {
                MicrophoneAuthorization::Denied
            } else {
                MicrophoneAuthorization::Authorized
            });
        });
        // SAFETY: AVMediaTypeAudio is valid here. AVCaptureDevice copies the escaping block,
        // which owns its Send callback and runs on an arbitrary completion queue.
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &completion);
        }
        Ok(())
    }

    fn open_settings(&self) -> Result<(), MicrophoneAccessError> {
        use super::permission_recovery::PermissionRecoveryOpener;

        self.settings.open().map_err(MicrophoneAccessError::from)
    }
}

fn authorization_from_raw(raw: NSInteger) -> Option<MicrophoneAuthorization> {
    match raw {
        0 => Some(MicrophoneAuthorization::NotDetermined),
        1 => Some(MicrophoneAuthorization::Restricted),
        2 => Some(MicrophoneAuthorization::Denied),
        3 => Some(MicrophoneAuthorization::Authorized),
        _ => None,
    }
}

impl From<PermissionRecoveryError> for MicrophoneAccessError {
    fn from(error: PermissionRecoveryError) -> Self {
        match error {
            PermissionRecoveryError::OffMainThread => Self::OffMainThread,
            PermissionRecoveryError::PlatformUnavailable => Self::PlatformUnavailable,
            PermissionRecoveryError::PlatformRejected => Self::PlatformRejected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_authorization_values_map_without_guessing_unknown_values() {
        assert_eq!(
            [
                authorization_from_raw(0),
                authorization_from_raw(1),
                authorization_from_raw(2),
                authorization_from_raw(3),
                authorization_from_raw(4),
            ],
            [
                Some(MicrophoneAuthorization::NotDetermined),
                Some(MicrophoneAuthorization::Restricted),
                Some(MicrophoneAuthorization::Denied),
                Some(MicrophoneAuthorization::Authorized),
                None,
            ]
        );
    }

    #[test]
    fn microphone_settings_routes_are_exact() {
        assert_eq!(
            (MICROPHONE_SETTINGS_URI, LEGACY_MICROPHONE_SETTINGS_URI,),
            (
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone",
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
            )
        );
    }
}
