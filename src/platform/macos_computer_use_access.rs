use std::marker::PhantomData;
use std::process::{Command, ExitStatus, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSBundle, NSDistributedNotificationCenter, NSNotification, NSNotificationSuspensionBehavior,
    NSObject, NSObjectProtocol, NSProcessInfo, NSString,
};

use super::computer_use_access::{
    AccessibilityNaming, ComputerUseAccess, ComputerUseAccessError, ComputerUseAccessObservation,
    ComputerUseAccessSubscription, ComputerUseAuthorization, ComputerUsePermission,
    ComputerUseResetCompletion, ComputerUseSetupCompletion, ComputerUseSetupReadiness,
};
use super::macos_computer_use_probe::{ProbeReport, run_probe};
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
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

/// Reads the calling process's own authorization.
///
/// The Screen Recording answer keeps its launch value for the life of the process, so only a
/// process started after a change reads that change.
pub(super) fn in_process_granted(permission: ComputerUsePermission) -> bool {
    // SAFETY: Both functions read the calling process's own authorization and take no input.
    match permission {
        ComputerUsePermission::ScreenRecording => unsafe { CGPreflightScreenCaptureAccess() },
        ComputerUsePermission::Accessibility => unsafe { AXIsProcessTrusted() != 0 },
    }
}

pub(crate) struct MacosComputerUseAccess {
    screen_recording_settings: PermissionRecovery,
    accessibility_settings: PermissionRecovery,
    /// The bundle identifier a reset may name, present only when the running bundle is this build's
    /// own identity, so a reset can never reach another application's grant.
    reset_bundle_identifier: Option<&'static str>,
    verification: Arc<Verification>,
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
            verification: Arc::default(),
            _not_send_or_sync: PhantomData,
        }
    }
}

impl ComputerUseAccess for MacosComputerUseAccess {
    fn authorization(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<ComputerUseAuthorization, ComputerUseAccessError> {
        let verified = self.verification.latest();
        Verification::start(&self.verification);
        Ok(verified.map_or_else(
            || {
                if in_process_granted(permission) {
                    ComputerUseAuthorization::Granted
                } else {
                    ComputerUseAuthorization::NotGranted
                }
            },
            |report| report.authorization(permission),
        ))
    }

    fn observe(&self) -> Option<ComputerUseAccessObservation> {
        let mtm = MainThreadMarker::new()?;
        let (sender, changed) = async_channel::bounded(1);
        self.verification.subscribe(sender.clone());
        let observer = AccessChangeObserver::new(mtm, sender, Arc::clone(&self.verification));
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSString::from_str(ACCESSIBILITY_CHANGED_NOTIFICATION);
        // A Permission Setup runs while System Settings is the active application, so the report
        // must arrive while SpaceTerm is inactive. Each report also starts a verification, which
        // reads from a fresh process instead of from this process's cached trust value.
        // SAFETY: The selector belongs to this retained observer, and the subscription removes the
        // registration before releasing it.
        unsafe {
            center.addObserver_selector_name_object_suspensionBehavior(
                &observer,
                sel!(accessChanged:),
                Some(&name),
                None,
                NSNotificationSuspensionBehavior::DeliverImmediately,
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

    fn prepare_setup(
        &self,
        permission: ComputerUsePermission,
        completion: ComputerUseSetupCompletion,
    ) -> Result<(), ComputerUseAccessError> {
        let reset_bundle_identifier = self.reset_bundle_identifier;
        let verification = Arc::clone(&self.verification);
        std::thread::Builder::new()
            .name("spaceterm-permission-setup".to_owned())
            .spawn(move || {
                let readiness = prepare(
                    permission,
                    reset_bundle_identifier,
                    run_probe,
                    run_reset,
                    |report| verification.record(report),
                );
                completion(Ok(readiness));
            })
            .map(drop)
            .map_err(|_| ComputerUseAccessError::PlatformUnavailable)
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

    fn accessibility_naming(&self) -> AccessibilityNaming {
        accessibility_naming(
            NSProcessInfo::processInfo()
                .operatingSystemVersion()
                .majorVersion,
        )
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
        let verification = Arc::clone(&self.verification);
        std::thread::Builder::new()
            .name("spaceterm-permission-reset".to_owned())
            .spawn(move || {
                let result = run_reset(permission, bundle_identifier);
                Verification::start(&verification);
                completion(result);
            })
            .map(drop)
            .map_err(|_| ComputerUseAccessError::PlatformUnavailable)
    }
}

/// Reads `permission` from a fresh process and clears its entry only on a verified NotGranted, so
/// a grant made since the last read is never reset and a failed read resets nothing.
fn prepare(
    permission: ComputerUsePermission,
    reset_bundle_identifier: Option<&'static str>,
    probe: impl FnOnce() -> Result<ProbeReport, ComputerUseAccessError>,
    reset: impl FnOnce(ComputerUsePermission, &'static str) -> Result<(), ComputerUseAccessError>,
    record: impl FnOnce(ProbeReport),
) -> ComputerUseSetupReadiness {
    let Ok(report) = probe() else {
        return ComputerUseSetupReadiness::Ready { cleared: false };
    };
    record(report);
    if report.authorization(permission) == ComputerUseAuthorization::Granted {
        return ComputerUseSetupReadiness::AlreadyGranted;
    }
    // A failed reset leaves an entry the person turns on instead.
    let cleared = reset_bundle_identifier
        .is_some_and(|bundle_identifier| reset(permission, bundle_identifier).is_ok());
    ComputerUseSetupReadiness::Ready { cleared }
}

fn run_reset(
    permission: ComputerUsePermission,
    bundle_identifier: &'static str,
) -> Result<(), ComputerUseAccessError> {
    let status = Command::new(TCCUTIL)
        .args(reset_arguments(permission, bundle_identifier))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    reset_result(status)
}

/// The latest probe report and the probe that refreshes it.
///
/// At most one probe runs at a time. A request made while one runs starts one more afterward, so
/// the last report always follows the last request.
#[derive(Default)]
struct Verification {
    state: Mutex<VerificationState>,
}

#[derive(Default)]
struct VerificationState {
    latest: Option<ProbeReport>,
    running: bool,
    requested_again: bool,
    subscribers: Vec<async_channel::Sender<()>>,
}

impl Verification {
    fn lock(&self) -> std::sync::MutexGuard<'_, VerificationState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn latest(&self) -> Option<ProbeReport> {
        self.lock().latest
    }

    fn subscribe(&self, sender: async_channel::Sender<()>) {
        let mut state = self.lock();
        state
            .subscribers
            .retain(|subscriber| !subscriber.is_closed());
        state.subscribers.push(sender);
    }

    /// Keeps a report and signals every subscriber when it differs from the last one.
    fn record(&self, report: ProbeReport) {
        let mut state = self.lock();
        if state.latest.replace(report) != Some(report) {
            state
                .subscribers
                .retain(|subscriber| !subscriber.is_closed());
            for subscriber in &state.subscribers {
                let _ = subscriber.try_send(());
            }
        }
    }

    fn start(this: &Arc<Self>) {
        {
            let mut state = this.lock();
            if state.running {
                state.requested_again = true;
                return;
            }
            state.running = true;
        }
        let verification = Arc::clone(this);
        let spawned = std::thread::Builder::new()
            .name("spaceterm-permission-probe".to_owned())
            .spawn(move || {
                loop {
                    // A failed probe keeps the last report rather than inventing one.
                    if let Ok(report) = run_probe() {
                        verification.record(report);
                    }
                    let mut state = verification.lock();
                    if !std::mem::take(&mut state.requested_again) {
                        state.running = false;
                        break;
                    }
                }
            });
        if spawned.is_err() {
            this.lock().running = false;
        }
    }
}

struct AccessChangeObserverIvars {
    sender: async_channel::Sender<()>,
    verification: Arc<Verification>,
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
            Verification::start(&self.ivars().verification);
        }
    }

    unsafe impl NSObjectProtocol for AccessChangeObserver {}
);

impl AccessChangeObserver {
    fn new(
        mtm: MainThreadMarker,
        sender: async_channel::Sender<()>,
        verification: Arc<Verification>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AccessChangeObserverIvars {
            sender,
            verification,
        });
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

/// macOS 27 renamed System Settings' Accessibility list to Device Control and Data Access.
fn accessibility_naming(major_version: isize) -> AccessibilityNaming {
    if major_version >= 27 {
        AccessibilityNaming::DeviceControl
    } else {
        AccessibilityNaming::Accessibility
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;

    use super::*;

    #[test]
    fn macos_27_names_the_accessibility_list_device_control() {
        assert_eq!(accessibility_naming(26), AccessibilityNaming::Accessibility);
        assert_eq!(accessibility_naming(27), AccessibilityNaming::DeviceControl);
        assert_eq!(accessibility_naming(28), AccessibilityNaming::DeviceControl);
    }

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

    const BUNDLE: &str = "io.github.sadiksaifi.spaceterm";

    /// Prepares a Screen Recording setup against a probe result and returns the readiness with the
    /// resets it made.
    fn prepare_with(
        probe: Result<ProbeReport, ComputerUseAccessError>,
        reset_bundle_identifier: Option<&'static str>,
    ) -> (
        ComputerUseSetupReadiness,
        Vec<(ComputerUsePermission, &'static str)>,
    ) {
        let mut resets = Vec::new();
        let readiness = prepare(
            ComputerUsePermission::ScreenRecording,
            reset_bundle_identifier,
            || probe,
            |permission, bundle| {
                resets.push((permission, bundle));
                Ok(())
            },
            |_| {},
        );
        (readiness, resets)
    }

    fn report(screen_recording: bool) -> ProbeReport {
        ProbeReport::read(|permission| {
            permission == ComputerUsePermission::ScreenRecording && screen_recording
        })
    }

    #[test]
    fn a_verified_missing_grant_clears_its_entry() {
        assert_eq!(
            prepare_with(Ok(report(false)), Some(BUNDLE)),
            (
                ComputerUseSetupReadiness::Ready { cleared: true },
                vec![(ComputerUsePermission::ScreenRecording, BUNDLE)]
            )
        );
    }

    #[test]
    fn a_failed_reset_clears_nothing() {
        let readiness = prepare(
            ComputerUsePermission::ScreenRecording,
            Some(BUNDLE),
            || Ok(report(false)),
            |_, _| Err(ComputerUseAccessError::PlatformRejected),
            |_| {},
        );

        assert_eq!(
            readiness,
            ComputerUseSetupReadiness::Ready { cleared: false }
        );
    }

    #[test]
    fn a_verified_grant_is_never_reset() {
        assert_eq!(
            prepare_with(Ok(report(true)), Some(BUNDLE)),
            (ComputerUseSetupReadiness::AlreadyGranted, Vec::new())
        );
    }

    #[test]
    fn a_failed_read_resets_nothing() {
        assert_eq!(
            prepare_with(
                Err(ComputerUseAccessError::PlatformUnavailable),
                Some(BUNDLE)
            ),
            (
                ComputerUseSetupReadiness::Ready { cleared: false },
                Vec::new()
            )
        );
    }

    #[test]
    fn another_identity_resets_nothing() {
        assert_eq!(
            prepare_with(Ok(report(false)), None),
            (
                ComputerUseSetupReadiness::Ready { cleared: false },
                Vec::new()
            )
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
