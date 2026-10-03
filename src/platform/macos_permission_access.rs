use std::marker::PhantomData;
use std::process::{Command, ExitStatus, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSBundle, NSDistributedNotificationCenter, NSNotification, NSNotificationSuspensionBehavior,
    NSObject, NSObjectProtocol, NSProcessInfo, NSString,
};

use super::macos_permission_probe::{ProbeReport, run_probe, wait_bounded};
use super::permission_access::{
    AccessibilityNaming, PermissionAccess, PermissionAccessError, PermissionAccessObservation,
    PermissionAccessSubscription, PermissionAuthorization, PermissionResetCompletion,
    PermissionSetupCancellation, PermissionSetupCompletion, PermissionSetupPreparation,
    PermissionSetupReadiness, SystemPermission,
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
/// A reset answers within a second. A slower one is stuck and is stopped, which reports the reset
/// as failed.
const RESET_TIMEOUT: Duration = Duration::from_secs(10);

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
pub(super) fn in_process_granted(permission: SystemPermission) -> bool {
    // SAFETY: Both functions read the calling process's own authorization and take no input.
    match permission {
        SystemPermission::ScreenRecording => unsafe { CGPreflightScreenCaptureAccess() },
        SystemPermission::Accessibility => unsafe { AXIsProcessTrusted() != 0 },
    }
}

pub(crate) struct MacosPermissionAccess {
    screen_recording_settings: PermissionRecovery,
    accessibility_settings: PermissionRecovery,
    /// The bundle identifier a reset may name, present only when the running bundle is this build's
    /// own identity, so a reset can never reach another application's grant.
    reset_bundle_identifier: Option<&'static str>,
    verification: Arc<Verification>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl MacosPermissionAccess {
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

impl PermissionAccess for MacosPermissionAccess {
    fn authorization(
        &self,
        permission: SystemPermission,
    ) -> Result<PermissionAuthorization, PermissionAccessError> {
        let verified = self.verification.latest();
        Verification::start(&self.verification);
        Ok(verified.map_or_else(
            || {
                if in_process_granted(permission) {
                    PermissionAuthorization::Granted
                } else {
                    PermissionAuthorization::NotGranted
                }
            },
            |report| report.authorization(permission),
        ))
    }

    fn observe(&self) -> Option<PermissionAccessObservation> {
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
        Some(PermissionAccessObservation {
            changed,
            subscription: Box::new(MacosPermissionAccessSubscription {
                center,
                observer,
                name,
            }),
        })
    }

    fn prepare_setup(
        &self,
        permission: SystemPermission,
        completion: PermissionSetupCompletion,
    ) -> Result<PermissionSetupPreparation, PermissionAccessError> {
        let reset_bundle_identifier = self.reset_bundle_identifier;
        let verification = Arc::clone(&self.verification);
        let (preparation, cancellation) = PermissionSetupPreparation::new();
        std::thread::Builder::new()
            .name("spaceterm-permission-setup".to_owned())
            .spawn(move || {
                if let Some(readiness) = prepare_exclusively(
                    &verification,
                    permission,
                    reset_bundle_identifier,
                    &cancellation,
                    run_probe,
                    run_reset,
                ) {
                    completion(Ok(readiness));
                }
            })
            .map(|_| preparation)
            .map_err(|_| PermissionAccessError::PlatformUnavailable)
    }

    fn open_settings(&self, permission: SystemPermission) -> Result<(), PermissionAccessError> {
        match permission {
            SystemPermission::ScreenRecording => self.screen_recording_settings.open(),
            SystemPermission::Accessibility => self.accessibility_settings.open(),
        }
        .map_err(PermissionAccessError::from)
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
        permission: SystemPermission,
        completion: PermissionResetCompletion,
    ) -> Result<(), PermissionAccessError> {
        let bundle_identifier = self
            .reset_bundle_identifier
            .ok_or(PermissionAccessError::PlatformUnavailable)?;
        let verification = Arc::clone(&self.verification);
        std::thread::Builder::new()
            .name("spaceterm-permission-reset".to_owned())
            .spawn(move || {
                completion(reset_and_verify(
                    &verification,
                    permission,
                    || run_reset(permission, bundle_identifier),
                    run_probe,
                ));
            })
            .map(drop)
            .map_err(|_| PermissionAccessError::PlatformUnavailable)
    }
}

/// Resets one grant, refreshes authorization, and reports the reset's own result.
///
/// The reset and its verification hold the probing lock together, so no earlier probe's report
/// lands after them. A removed entry grants nothing, so a verification that fails after a
/// successful reset records the permission as not granted instead of keeping the grant the reset
/// removed. Without an earlier report, the other permission then reads as not granted until a
/// probe succeeds; a Permission Setup of it verifies again before it changes anything.
fn reset_and_verify(
    verification: &Verification,
    permission: SystemPermission,
    reset: impl FnOnce() -> Result<(), PermissionAccessError>,
    probe: impl FnOnce() -> Result<ProbeReport, PermissionAccessError>,
) -> Result<(), PermissionAccessError> {
    let _probing = verification.probing();
    let result = reset();
    match probe() {
        Ok(report) => verification.record(report),
        Err(_) if result.is_ok() => verification.record(
            verification
                .latest()
                .unwrap_or_else(|| ProbeReport::read(|_| false))
                .revoking(permission),
        ),
        Err(_) => {}
    }
    result
}

/// Prepares once no probe or earlier preparation runs, so a setup cancelled and started again
/// never probes or resets beside its predecessor. Returns `None` when the setup was cancelled
/// while it waited.
fn prepare_exclusively(
    verification: &Verification,
    permission: SystemPermission,
    reset_bundle_identifier: Option<&'static str>,
    cancellation: &PermissionSetupCancellation,
    probe: impl FnOnce() -> Result<ProbeReport, PermissionAccessError>,
    reset: impl FnOnce(SystemPermission, &'static str) -> Result<(), PermissionAccessError>,
) -> Option<PermissionSetupReadiness> {
    let _probing = verification.probing();
    if cancellation.is_cancelled() {
        return None;
    }
    Some(prepare(
        permission,
        reset_bundle_identifier,
        cancellation,
        probe,
        reset,
        |report| verification.record(report),
    ))
}

/// Reads `permission` from a fresh process and clears its entry only on a verified NotGranted, so
/// a grant made since the last read is never reset and a failed read resets nothing. A setup
/// cancelled before the reset begins resets nothing either.
fn prepare(
    permission: SystemPermission,
    reset_bundle_identifier: Option<&'static str>,
    cancellation: &PermissionSetupCancellation,
    probe: impl FnOnce() -> Result<ProbeReport, PermissionAccessError>,
    reset: impl FnOnce(SystemPermission, &'static str) -> Result<(), PermissionAccessError>,
    record: impl FnOnce(ProbeReport),
) -> PermissionSetupReadiness {
    let Ok(report) = probe() else {
        return PermissionSetupReadiness::Ready { cleared: false };
    };
    record(report);
    if report.authorization(permission) == PermissionAuthorization::Granted {
        return PermissionSetupReadiness::AlreadyGranted;
    }
    // A failed reset leaves an entry the person turns on instead.
    let cleared = reset_bundle_identifier
        .filter(|_| !cancellation.is_cancelled())
        .is_some_and(|bundle_identifier| reset(permission, bundle_identifier).is_ok());
    PermissionSetupReadiness::Ready { cleared }
}

fn run_reset(
    permission: SystemPermission,
    bundle_identifier: &'static str,
) -> Result<(), PermissionAccessError> {
    let status = Command::new(TCCUTIL)
        .args(reset_arguments(permission, bundle_identifier))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| {
            // A reset holds the probing lock during a setup, so a stuck one must not hold it
            // for the rest of the session.
            wait_bounded(&mut child, RESET_TIMEOUT)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::TimedOut))
        });
    reset_result(status)
}

/// The latest probe report and the probe that refreshes it.
///
/// At most one probe runs at a time, counting the probe a setup preparation makes. A request made
/// while one runs starts one more afterward, so the last report always follows the last request.
#[derive(Default)]
struct Verification {
    state: Mutex<VerificationState>,
    /// Held for each probe and each whole setup preparation.
    probing: Mutex<()>,
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

    /// Waits until no probe or setup preparation runs, and keeps others waiting while held.
    fn probing(&self) -> std::sync::MutexGuard<'_, ()> {
        self.probing.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// Probes once no other probe, setup preparation, or reset runs, and records the report before
    /// releasing the probing lock, so reports land in the order their probes ran. A failed probe
    /// keeps the last report rather than inventing one.
    fn verify(&self, probe: impl FnOnce() -> Result<ProbeReport, PermissionAccessError>) {
        let _probing = self.probing();
        if let Ok(report) = probe() {
            self.record(report);
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
                    verification.verify(run_probe);
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
    #[name = "SpaceTermPermissionAccessObserver"]
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

struct MacosPermissionAccessSubscription {
    center: Retained<NSDistributedNotificationCenter>,
    observer: Retained<AccessChangeObserver>,
    name: Retained<NSString>,
}

impl PermissionAccessSubscription for MacosPermissionAccessSubscription {}

impl Drop for MacosPermissionAccessSubscription {
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
    permission: SystemPermission,
    bundle_identifier: &'static str,
) -> [&'static str; 3] {
    let service = match permission {
        SystemPermission::ScreenRecording => "ScreenCapture",
        SystemPermission::Accessibility => "Accessibility",
    };
    ["reset", service, bundle_identifier]
}

fn reset_result(status: std::io::Result<ExitStatus>) -> Result<(), PermissionAccessError> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(PermissionAccessError::PlatformRejected),
        Err(_) => Err(PermissionAccessError::PlatformUnavailable),
    }
}

impl From<PermissionRecoveryError> for PermissionAccessError {
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
                reset_arguments(SystemPermission::ScreenRecording, bundle),
                reset_arguments(SystemPermission::Accessibility, bundle),
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
        probe: Result<ProbeReport, PermissionAccessError>,
        reset_bundle_identifier: Option<&'static str>,
    ) -> (
        PermissionSetupReadiness,
        Vec<(SystemPermission, &'static str)>,
    ) {
        let mut resets = Vec::new();
        let (_preparation, cancellation) = PermissionSetupPreparation::new();
        let readiness = prepare(
            SystemPermission::ScreenRecording,
            reset_bundle_identifier,
            &cancellation,
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
            permission == SystemPermission::ScreenRecording && screen_recording
        })
    }

    #[test]
    fn a_verified_missing_grant_clears_its_entry() {
        assert_eq!(
            prepare_with(Ok(report(false)), Some(BUNDLE)),
            (
                PermissionSetupReadiness::Ready { cleared: true },
                vec![(SystemPermission::ScreenRecording, BUNDLE)]
            )
        );
    }

    #[test]
    fn a_failed_reset_clears_nothing() {
        let (_preparation, cancellation) = PermissionSetupPreparation::new();
        let readiness = prepare(
            SystemPermission::ScreenRecording,
            Some(BUNDLE),
            &cancellation,
            || Ok(report(false)),
            |_, _| Err(PermissionAccessError::PlatformRejected),
            |_| {},
        );

        assert_eq!(
            readiness,
            PermissionSetupReadiness::Ready { cleared: false }
        );
    }

    #[test]
    fn a_setup_cancelled_during_its_probe_resets_nothing() {
        let (preparation, cancellation) = PermissionSetupPreparation::new();
        let mut preparation = Some(preparation);
        let mut resets = Vec::new();
        let readiness = prepare(
            SystemPermission::ScreenRecording,
            Some(BUNDLE),
            &cancellation,
            || {
                preparation.take();
                Ok(report(false))
            },
            |permission, bundle| {
                resets.push((permission, bundle));
                Ok(())
            },
            |_| {},
        );

        assert_eq!(
            readiness,
            PermissionSetupReadiness::Ready { cleared: false }
        );
        assert!(resets.is_empty());
    }

    #[test]
    fn a_preparation_waits_for_a_running_probe() {
        let verification = Arc::new(Verification::default());
        let probing = verification.probing();
        let (preparation, cancellation) = PermissionSetupPreparation::new();
        let (sender, probed) = std::sync::mpsc::channel();
        let waiting = std::thread::spawn({
            let verification = Arc::clone(&verification);
            move || {
                prepare_exclusively(
                    &verification,
                    SystemPermission::ScreenRecording,
                    Some(BUNDLE),
                    &cancellation,
                    || {
                        let _ = sender.send(());
                        Ok(report(false))
                    },
                    |_, _| Ok(()),
                )
            }
        });

        assert!(
            probed.recv_timeout(Duration::from_millis(100)).is_err(),
            "a preparation does not probe beside a running probe"
        );
        drop(preparation);
        drop(probing);

        assert_eq!(waiting.join().expect("the preparation"), None);
        assert!(
            probed.try_recv().is_err(),
            "a preparation cancelled while it waits neither probes nor resets"
        );
    }

    #[test]
    fn a_probe_publishes_its_report_before_the_next_probe_runs() {
        let verification = Arc::new(Verification::default());
        // Hold publication while the older probe finishes, as a competing authorization read can.
        let publication = verification.lock();
        let (sender, older_probed) = std::sync::mpsc::channel();
        let older = std::thread::spawn({
            let verification = Arc::clone(&verification);
            move || {
                verification.verify(|| {
                    sender.send(()).expect("the older probe");
                    Ok(report(true))
                });
            }
        });
        older_probed
            .recv_timeout(Duration::from_secs(5))
            .expect("the older probe finished reading");

        let (sender, newer_probed) = std::sync::mpsc::channel();
        let newer = std::thread::spawn({
            let verification = Arc::clone(&verification);
            move || {
                verification.verify(|| {
                    sender.send(()).expect("the newer probe");
                    Ok(report(false))
                });
            }
        });
        let overtook_publication = newer_probed
            .recv_timeout(Duration::from_millis(100))
            .is_ok();
        drop(publication);
        older.join().expect("the older verification");
        newer.join().expect("the newer verification");

        assert!(
            !overtook_publication,
            "a newer probe must wait for the older report to be published"
        );
        assert_eq!(verification.latest(), Some(report(false)));
    }

    #[test]
    fn a_verified_grant_is_never_reset() {
        assert_eq!(
            prepare_with(Ok(report(true)), Some(BUNDLE)),
            (PermissionSetupReadiness::AlreadyGranted, Vec::new())
        );
    }

    #[test]
    fn a_failed_read_resets_nothing() {
        assert_eq!(
            prepare_with(
                Err(PermissionAccessError::PlatformUnavailable),
                Some(BUNDLE)
            ),
            (
                PermissionSetupReadiness::Ready { cleared: false },
                Vec::new()
            )
        );
    }

    #[test]
    fn another_identity_resets_nothing() {
        assert_eq!(
            prepare_with(Ok(report(false)), None),
            (
                PermissionSetupReadiness::Ready { cleared: false },
                Vec::new()
            )
        );
    }

    fn both_granted() -> ProbeReport {
        ProbeReport::read(|_| true)
    }

    #[test]
    fn a_successful_reset_publishes_authorization_before_reporting_success() {
        let verification = Verification::default();
        verification.record(both_granted());

        let result = reset_and_verify(
            &verification,
            SystemPermission::ScreenRecording,
            || Ok(()),
            || Ok(report(false)),
        );

        assert_eq!(result, Ok(()));
        assert_eq!(verification.latest(), Some(report(false)));
    }

    /// A failed verification does not undo a reset: the removed entry grants nothing, so the
    /// reset reports success and its permission reads as not granted.
    #[test]
    fn a_failed_verification_after_a_reset_records_the_removed_grant() {
        let verification = Verification::default();
        verification.record(both_granted());
        let reset = std::cell::Cell::new(false);

        let result = reset_and_verify(
            &verification,
            SystemPermission::ScreenRecording,
            || {
                reset.set(true);
                Ok(())
            },
            || {
                assert!(reset.get(), "verification follows the reset");
                Err(PermissionAccessError::PlatformUnavailable)
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            verification.latest(),
            Some(ProbeReport::read(
                |permission| permission == SystemPermission::Accessibility
            ))
        );
    }

    #[test]
    fn a_failed_reset_reports_its_error_after_refreshing_authorization() {
        let verification = Verification::default();
        verification.record(report(true));

        let result = reset_and_verify(
            &verification,
            SystemPermission::ScreenRecording,
            || Err(PermissionAccessError::PlatformRejected),
            || Ok(report(false)),
        );

        assert_eq!(result, Err(PermissionAccessError::PlatformRejected));
        assert_eq!(verification.latest(), Some(report(false)));
    }

    #[test]
    fn a_failed_reset_and_verification_keep_the_last_report() {
        let verification = Verification::default();
        verification.record(both_granted());

        let result = reset_and_verify(
            &verification,
            SystemPermission::ScreenRecording,
            || Err(PermissionAccessError::PlatformRejected),
            || Err(PermissionAccessError::PlatformUnavailable),
        );

        assert_eq!(result, Err(PermissionAccessError::PlatformRejected));
        assert_eq!(verification.latest(), Some(both_granted()));
    }

    /// A probe that starts during a reset waits for the reset's verification, so its report lands
    /// after the reset's instead of between the reset and its verification.
    #[test]
    fn no_probe_runs_between_a_reset_and_its_verification() {
        let verification = Arc::new(Verification::default());
        let (sender, probed) = std::sync::mpsc::channel();
        let mut competing = None;

        let result = reset_and_verify(
            &verification,
            SystemPermission::ScreenRecording,
            || {
                competing = Some(std::thread::spawn({
                    let verification = Arc::clone(&verification);
                    move || {
                        verification.verify(|| {
                            sender.send(()).expect("the competing probe");
                            Ok(both_granted())
                        });
                    }
                }));
                assert!(
                    probed.recv_timeout(Duration::from_millis(100)).is_err(),
                    "a probe must wait for the reset's verification"
                );
                Ok(())
            },
            || Ok(report(false)),
        );
        competing
            .expect("the competing probe started")
            .join()
            .expect("the competing verification");

        assert_eq!(result, Ok(()));
        assert!(
            probed.try_recv().is_ok(),
            "the competing probe ran afterward"
        );
        assert_eq!(verification.latest(), Some(both_granted()));
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
                Err(PermissionAccessError::PlatformRejected),
                Err(PermissionAccessError::PlatformUnavailable),
            ]
        );
    }

    #[test]
    fn permission_settings_routes_are_exact() {
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
