use std::ffi::c_void;
use std::marker::PhantomData;
use std::process::{Command, ExitStatus, Stdio};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSBundle, NSDictionary, NSDistributedNotificationCenter, NSNotification,
    NSNotificationSuspensionBehavior, NSNumber, NSObject, NSObjectProtocol, NSString,
};

use super::computer_use_access::{
    ComputerUseAccess, ComputerUseAccessError, ComputerUseAccessObservation,
    ComputerUseAccessSubscription, ComputerUseAuthorization, ComputerUsePermission,
    ComputerUseResetCompletion,
};
use super::permission_recovery::{
    PermissionRecovery, PermissionRecoveryError, PermissionRecoveryOpener,
};
use crate::application_identity::ApplicationIdentity;

const SCREEN_RECORDING_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture";
const LEGACY_SCREEN_RECORDING_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
const ACCESSIBILITY_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility";
const LEGACY_ACCESSIBILITY_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// The distributed notification the system posts when an application's Accessibility grant
/// changes. HIServices observes it to clear the trust value `AXIsProcessTrusted` caches.
const ACCESSIBILITY_CHANGED_NOTIFICATION: &str = "com.apple.accessibility.api";

/// The system's own privacy reset tool, addressed by absolute path so no search path can replace it.
const TCCUTIL: &str = "/usr/bin/tccutil";

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: *const NSString;
}

pub(crate) struct MacosComputerUseAccess {
    screen_recording_settings: PermissionRecovery,
    accessibility_settings: PermissionRecovery,
    /// The bundle identifier a reset may name, present only when the running bundle is this build's
    /// own identity, so a reset can never reach another application's grant.
    reset_bundle_identifier: Option<&'static str>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl MacosComputerUseAccess {
    pub(crate) fn new(identity: ApplicationIdentity) -> Self {
        let running = NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|identifier| identifier.to_string());
        Self {
            screen_recording_settings: PermissionRecovery::new(
                Box::new(super::macos_system_settings::NsWorkspaceUrlLauncher::default()),
                SCREEN_RECORDING_SETTINGS_URI,
                LEGACY_SCREEN_RECORDING_SETTINGS_URI,
            ),
            accessibility_settings: PermissionRecovery::new(
                Box::new(super::macos_system_settings::NsWorkspaceUrlLauncher::default()),
                ACCESSIBILITY_SETTINGS_URI,
                LEGACY_ACCESSIBILITY_SETTINGS_URI,
            ),
            reset_bundle_identifier: reset_bundle_identifier(
                running.as_deref(),
                identity.bundle_identifier(),
            ),
            _not_send_or_sync: PhantomData,
        }
    }
}

impl ComputerUseAccess for MacosComputerUseAccess {
    fn authorization(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<ComputerUseAuthorization, ComputerUseAccessError> {
        // SAFETY: Both functions read the calling process's own authorization and take no input.
        let granted = match permission {
            ComputerUsePermission::ScreenRecording => unsafe { CGPreflightScreenCaptureAccess() },
            ComputerUsePermission::Accessibility => unsafe { AXIsProcessTrusted() != 0 },
        };
        Ok(if granted {
            ComputerUseAuthorization::Granted
        } else {
            ComputerUseAuthorization::NotGranted
        })
    }

    fn observe(&self) -> Option<ComputerUseAccessObservation> {
        let mtm = MainThreadMarker::new()?;
        let (sender, changed) = async_channel::bounded(1);
        let observer = AccessChangeObserver::new(mtm, sender);
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSString::from_str(ACCESSIBILITY_CHANGED_NOTIFICATION);
        // HIServices clears its cached trust value from this notification with coalesced delivery,
        // which the system holds while SpaceTerm is inactive. Matching that delivery signals the
        // owner in the same pass, so its read follows the cache clear instead of preceding it.
        // SAFETY: The selector belongs to this retained observer, and the subscription removes the
        // registration before releasing it.
        unsafe {
            center.addObserver_selector_name_object_suspensionBehavior(
                &observer,
                sel!(accessChanged:),
                Some(&name),
                None,
                NSNotificationSuspensionBehavior::Coalesce,
            );
        }
        Some(ComputerUseAccessObservation {
            changed,
            subscription: Box::new(MacosComputerUseAccessSubscription {
                center,
                observer,
                name,
            }),
        })
    }

    fn request_authorization(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<(), ComputerUseAccessError> {
        if MainThreadMarker::new().is_none() {
            return Err(ComputerUseAccessError::OffMainThread);
        }
        match permission {
            ComputerUsePermission::ScreenRecording => {
                // SAFETY: Called on the main thread; the system presents its own prompt at most
                // once and the immediate result is read again by the caller.
                unsafe { CGRequestScreenCaptureAccess() };
            }
            ComputerUsePermission::Accessibility => {
                // SAFETY: HIServices exports this immutable option key when available.
                let key = unsafe { kAXTrustedCheckOptionPrompt.as_ref() }
                    .ok_or(ComputerUseAccessError::PlatformUnavailable)?;
                let prompt = NSNumber::new_bool(true);
                let options = NSDictionary::from_slices(&[key], &[&*prompt]);
                // SAFETY: NSDictionary is toll-free bridged to the CFDictionary this function
                // reads during the call, and `options` outlives the call.
                unsafe {
                    AXIsProcessTrustedWithOptions(Retained::as_ptr(&options).cast::<c_void>())
                };
            }
        }
        Ok(())
    }

    fn open_settings(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<(), ComputerUseAccessError> {
        match permission {
            ComputerUsePermission::ScreenRecording => self.screen_recording_settings.open(),
            ComputerUsePermission::Accessibility => self.accessibility_settings.open(),
        }
        .map_err(ComputerUseAccessError::from)
    }

    fn can_reset(&self) -> bool {
        self.reset_bundle_identifier.is_some()
    }

    fn reset(
        &self,
        permission: ComputerUsePermission,
        completion: ComputerUseResetCompletion,
    ) -> Result<(), ComputerUseAccessError> {
        let bundle_identifier = self
            .reset_bundle_identifier
            .ok_or(ComputerUseAccessError::PlatformUnavailable)?;
        let arguments = reset_arguments(permission, bundle_identifier);
        std::thread::Builder::new()
            .name("spaceterm-permission-reset".to_owned())
            .spawn(move || {
                let status = Command::new(TCCUTIL)
                    .args(arguments)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                completion(reset_result(status));
            })
            .map(drop)
            .map_err(|_| ComputerUseAccessError::PlatformUnavailable)
    }
}

struct AccessChangeObserverIvars {
    sender: async_channel::Sender<()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and define_class! drops the sender ivar.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermComputerUseAccessObserver"]
    #[thread_kind = MainThreadOnly]
    #[ivars = AccessChangeObserverIvars]
    struct AccessChangeObserver;

    impl AccessChangeObserver {
        #[unsafe(method(accessChanged:))]
        fn access_changed(&self, _notification: &NSNotification) {
            let _ = self.ivars().sender.try_send(());
        }
    }

    unsafe impl NSObjectProtocol for AccessChangeObserver {}
);

impl AccessChangeObserver {
    fn new(mtm: MainThreadMarker, sender: async_channel::Sender<()>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AccessChangeObserverIvars { sender });
        // SAFETY: NSObject's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

struct MacosComputerUseAccessSubscription {
    center: Retained<NSDistributedNotificationCenter>,
    observer: Retained<AccessChangeObserver>,
    name: Retained<NSString>,
}

impl ComputerUseAccessSubscription for MacosComputerUseAccessSubscription {}

impl Drop for MacosComputerUseAccessSubscription {
    fn drop(&mut self) {
        // SAFETY: The observer and name remain alive until the registration is removed.
        unsafe {
            self.center
                .removeObserver_name_object(&self.observer, Some(&self.name), None);
        }
    }
}

/// A reset names the running bundle only when it is this build's identity.
fn reset_bundle_identifier(running: Option<&str>, expected: &'static str) -> Option<&'static str> {
    (running == Some(expected)).then_some(expected)
}

/// The privacy service names `tccutil` uses for each permission.
fn reset_arguments(
    permission: ComputerUsePermission,
    bundle_identifier: &'static str,
) -> [&'static str; 3] {
    let service = match permission {
        ComputerUsePermission::ScreenRecording => "ScreenCapture",
        ComputerUsePermission::Accessibility => "Accessibility",
    };
    ["reset", service, bundle_identifier]
}

fn reset_result(status: std::io::Result<ExitStatus>) -> Result<(), ComputerUseAccessError> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(ComputerUseAccessError::PlatformRejected),
        Err(_) => Err(ComputerUseAccessError::PlatformUnavailable),
    }
}

impl From<PermissionRecoveryError> for ComputerUseAccessError {
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
    use std::os::unix::process::ExitStatusExt as _;

    use super::*;

    #[test]
    fn a_reset_names_only_the_running_bundle_of_this_identity() {
        let expected = "io.github.sadiksaifi.spaceterm-development";

        assert_eq!(
            [
                reset_bundle_identifier(Some(expected), expected),
                reset_bundle_identifier(Some("io.github.sadiksaifi.spaceterm"), expected),
                reset_bundle_identifier(Some("com.apple.Terminal"), expected),
                reset_bundle_identifier(None, expected),
            ],
            [Some(expected), None, None, None]
        );
    }

    #[test]
    fn a_reset_clears_one_service_for_one_bundle() {
        let bundle = "io.github.sadiksaifi.spaceterm";

        assert_eq!(
            [
                reset_arguments(ComputerUsePermission::ScreenRecording, bundle),
                reset_arguments(ComputerUsePermission::Accessibility, bundle),
            ],
            [
                ["reset", "ScreenCapture", bundle],
                ["reset", "Accessibility", bundle],
            ]
        );
    }

    #[test]
    fn a_reset_reports_success_only_for_a_successful_exit() {
        assert_eq!(
            [
                reset_result(Ok(ExitStatus::from_raw(0))),
                reset_result(Ok(ExitStatus::from_raw(64 << 8))),
                reset_result(Err(std::io::Error::from(std::io::ErrorKind::NotFound))),
            ],
            [
                Ok(()),
                Err(ComputerUseAccessError::PlatformRejected),
                Err(ComputerUseAccessError::PlatformUnavailable),
            ]
        );
    }

    #[test]
    fn computer_use_settings_routes_are_exact() {
        assert_eq!(
            [
                SCREEN_RECORDING_SETTINGS_URI,
                LEGACY_SCREEN_RECORDING_SETTINGS_URI,
                ACCESSIBILITY_SETTINGS_URI,
                LEGACY_ACCESSIBILITY_SETTINGS_URI,
            ],
            [
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture",
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            ]
        );
    }
}
