#![allow(deprecated)]
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Mutex;

use block::ConcreteBlock;
use cocoa::base::{BOOL, NO, nil};
use cocoa::foundation::{NSAutoreleasePool, NSInteger, NSString};
use objc::runtime::Class;
use objc::{msg_send, sel, sel_impl};

use super::microphone_access::{
    MicrophoneAccess, MicrophoneAccessError, MicrophoneAuthorization,
    MicrophoneAuthorizationCompletion,
};
use super::permission_recovery::{PermissionRecovery, PermissionRecoveryError};

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

const AV_MEDIA_TYPE_AUDIO: &str = "soun";
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
        if !main_thread() {
            return Err(MicrophoneAccessError::OffMainThread);
        }
        let device =
            Class::get("AVCaptureDevice").ok_or(MicrophoneAccessError::PlatformUnavailable)?;

        // SAFETY: AVFoundation is linked into the application. The media-type string lives until
        // the synchronous class query returns, and the autorelease pool drains on the AppKit thread.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let media_type = NSString::alloc(nil)
                .init_str(AV_MEDIA_TYPE_AUDIO)
                .autorelease();
            let raw: NSInteger = msg_send![device, authorizationStatusForMediaType: media_type];
            pool.drain();
            authorization_from_raw(raw).ok_or(MicrophoneAccessError::PlatformUnavailable)
        }
    }

    fn request_authorization(
        &self,
        completion: MicrophoneAuthorizationCompletion,
    ) -> Result<(), MicrophoneAccessError> {
        if !main_thread() {
            return Err(MicrophoneAccessError::OffMainThread);
        }
        let device =
            Class::get("AVCaptureDevice").ok_or(MicrophoneAccessError::PlatformUnavailable)?;
        let completion = Mutex::new(Some(completion));

        // SAFETY: AVFoundation copies the escaping block before this call returns. The block owns
        // its Send callback and publishes only the closed authorization result from any native
        // completion queue. The media-type string is consumed synchronously by the class method.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let media_type = NSString::alloc(nil)
                .init_str(AV_MEDIA_TYPE_AUDIO)
                .autorelease();
            let completion = ConcreteBlock::new(move |granted: BOOL| {
                let Some(completion) = completion
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                completion(if granted == NO {
                    MicrophoneAuthorization::Denied
                } else {
                    MicrophoneAuthorization::Authorized
                });
            })
            .copy();
            let _: () = msg_send![device,
                requestAccessForMediaType: media_type
                completionHandler: &*completion
            ];
            pool.drain();
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

fn main_thread() -> bool {
    let Some(thread) = Class::get("NSThread") else {
        return false;
    };
    // SAFETY: `NSThread.isMainThread` is a process query with no object lifetime transfer.
    unsafe {
        let is_main: BOOL = msg_send![thread, isMainThread];
        is_main != NO
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
