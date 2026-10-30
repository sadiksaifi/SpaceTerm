//! The server's macOS System Permissions, which TCC keys to the server bundle's identifier and
//! signature.
//!
//! Reads run in a probe child. Requests run in the server itself, the responsible process whose
//! prompt and System Settings entry name the server. Opening System Settings goes through Launch
//! Services by `/usr/bin/open`, because the server links no AppKit.
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::probe::{self, Probe};
use super::{
    AccessibilityNaming, PermissionError, PermissionHolder, PermissionReading, PermissionState,
    PermissionsReport, RequestOutcome, SetupReadiness, SystemPermission, SystemPermissions,
};
use crate::service::{Layout, bundle_identifier, bundle_name};

const OPEN: &str = "/usr/bin/open";
/// The system's own privacy reset tool, addressed by absolute path so no search path replaces it.
const TCCUTIL: &str = "/usr/bin/tccutil";
/// Both tools answer within a second. A slower one is stuck and is stopped.
const TOOL_TIMEOUT: Duration = Duration::from_secs(10);

/// The System Settings list that holds each permission.
const fn settings_uri(permission: SystemPermission) -> &'static str {
    match permission {
        SystemPermission::Microphone => {
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone"
        }
        SystemPermission::ScreenRecording => {
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture"
        }
        SystemPermission::Accessibility => {
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility"
        }
    }
}

/// The privacy service `tccutil` names for each permission.
const fn tcc_service(permission: SystemPermission) -> &'static str {
    match permission {
        SystemPermission::Microphone => "Microphone",
        SystemPermission::ScreenRecording => "ScreenCapture",
        SystemPermission::Accessibility => "Accessibility",
    }
}

/// The native effects, so the policy around them runs without the system in tests.
pub(super) trait Native: Send + Sync {
    fn probe(&self) -> Result<Probe, PermissionError>;
    /// Asks the system for `permission` from this process. It returns before anyone answers.
    fn request(&self, permission: SystemPermission) -> Result<(), PermissionError>;
    fn open(&self, uri: &'static str) -> Result<(), PermissionError>;
    fn reset(&self, service: &'static str, bundle: &str) -> Result<(), PermissionError>;
    fn os_major(&self) -> Option<u32>;
}

pub(crate) struct MacosSystemPermissions {
    holder: PermissionHolder,
    /// The bundle identifier a reset may name, present only while the server runs from this
    /// identity's bundle as its own responsible process, so a reset reaches no other grant.
    reset_bundle: Option<String>,
    native: Box<dyn Native>,
    /// Held for each probe, setup and reset, so a reset never lands between a probe and the
    /// setup that acts on its reading.
    probing: Mutex<()>,
}

impl MacosSystemPermissions {
    pub(crate) fn new(identity: &str) -> Self {
        let own_bundle = Layout::host(identity).is_some_and(|layout| {
            std::env::current_exe()
                .and_then(|executable| executable.canonicalize())
                .is_ok_and(|executable| executable.ends_with(layout.executable()))
        });
        // SAFETY: getppid reads this process's parent and cannot fail.
        let launched = unsafe { libc::getppid() } == 1;
        Self::with(identity, own_bundle && launched, Box::new(System))
    }

    /// `own` says whether launchd started this server from its identity's bundle, which makes
    /// the server its own responsible process.
    fn with(identity: &str, own: bool, native: Box<dyn Native>) -> Self {
        let name = bundle_name(identity).and_then(|name| name.strip_suffix(".app"));
        let (holder, reset_bundle) = match name {
            Some(name) if own => (
                PermissionHolder::Server { name: name.into() },
                Some(bundle_identifier(identity)),
            ),
            _ => (PermissionHolder::Launcher, None),
        };
        Self {
            holder,
            reset_bundle,
            native,
            probing: Mutex::new(()),
        }
    }

    fn probing(&self) -> std::sync::MutexGuard<'_, ()> {
        self.probing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn report(&self, probe: &Probe) -> PermissionsReport {
        PermissionsReport {
            permissions: probe
                .states()
                .iter()
                .map(|&(permission, state)| PermissionReading { permission, state })
                .collect(),
            accessibility_naming: accessibility_naming(self.native.os_major()),
            holder: self.holder.clone(),
            resettable: self.reset_bundle.is_some(),
        }
    }
}

impl SystemPermissions for MacosSystemPermissions {
    fn read(&self) -> Result<PermissionsReport, PermissionError> {
        let _probing = self.probing();
        self.native.probe().map(|probe| self.report(&probe))
    }

    fn request(&self, permission: SystemPermission) -> Result<RequestOutcome, PermissionError> {
        let state = {
            let _probing = self.probing();
            self.native.probe()?.state(permission)
        };
        // A prompt appears only before anyone decided. A permission without one is requested
        // again until it is granted, because each request lists the holder in System Settings.
        let ask = match state {
            PermissionState::NotRequested => true,
            PermissionState::NotGranted => !permission.prompts(),
            PermissionState::Granted | PermissionState::Restricted => false,
        };
        if !ask {
            return Ok(RequestOutcome::Decided { state });
        }
        self.native.request(permission)?;
        Ok(RequestOutcome::Requested)
    }

    fn open_settings(&self, permission: SystemPermission) -> Result<(), PermissionError> {
        self.native.open(settings_uri(permission))
    }

    fn prepare_setup(
        &self,
        permission: SystemPermission,
    ) -> Result<SetupReadiness, PermissionError> {
        let _probing = self.probing();
        // A failed read resets nothing, so a grant it could not see is never removed.
        let state = self.native.probe()?.state(permission);
        if state == PermissionState::Granted {
            return Ok(SetupReadiness::AlreadyGranted);
        }
        // A failed reset leaves an entry the person turns on instead.
        let cleared = self
            .reset_bundle
            .as_deref()
            .is_some_and(|bundle| self.native.reset(tcc_service(permission), bundle).is_ok());
        self.native.request(permission)?;
        Ok(SetupReadiness::Ready { cleared })
    }

    fn reset(&self, permission: SystemPermission) -> Result<(), PermissionError> {
        let bundle = self
            .reset_bundle
            .as_deref()
            .ok_or(PermissionError::Unavailable)?;
        let _probing = self.probing();
        self.native.reset(tcc_service(permission), bundle)
    }
}

/// macOS 27 renamed System Settings' Accessibility list to Device Control and Data Access.
fn accessibility_naming(major: Option<u32>) -> AccessibilityNaming {
    match major {
        Some(major) if major < 27 => AccessibilityNaming::Accessibility,
        _ => AccessibilityNaming::DeviceControl,
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

type CFTypeRef = *const std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> u8;
    static kAXTrustedCheckOptionPrompt: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const u8,
        value_callbacks: *const u8,
    ) -> CFTypeRef;
    fn CFRelease(value: CFTypeRef);
}

/// Reads this process's own authorization of `permission`.
///
/// The Screen Recording answer keeps its launch value for the life of the process, so only a
/// process started after a change reads that change.
pub(super) fn in_process_state(permission: SystemPermission) -> PermissionState {
    let granted = |granted: bool| {
        if granted {
            PermissionState::Granted
        } else {
            PermissionState::NotGranted
        }
    };
    match permission {
        SystemPermission::Microphone => capture_state(),
        // SAFETY: Both functions read the calling process's own authorization and take no input.
        SystemPermission::ScreenRecording => granted(unsafe { CGPreflightScreenCaptureAccess() }),
        SystemPermission::Accessibility => granted(unsafe { AXIsProcessTrusted() } != 0),
    }
}

fn capture_state() -> PermissionState {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
    // SAFETY: AVFoundation exports this immutable media type when available.
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return PermissionState::NotGranted;
    };
    // SAFETY: The media type is a valid input for this class method.
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) }.0 {
        0 => PermissionState::NotRequested,
        1 => PermissionState::Restricted,
        3 => PermissionState::Granted,
        _ => PermissionState::NotGranted,
    }
}

/// The system itself.
struct System;

impl Native for System {
    fn probe(&self) -> Result<Probe, PermissionError> {
        probe::run()
    }

    fn request(&self, permission: SystemPermission) -> Result<(), PermissionError> {
        match permission {
            SystemPermission::Microphone => request_capture(),
            SystemPermission::ScreenRecording => {
                // SAFETY: The function takes no input. The system lists this process in System
                // Settings and may show its prompt; the return value only repeats the grant.
                let _ = unsafe { CGRequestScreenCaptureAccess() };
                Ok(())
            }
            SystemPermission::Accessibility => request_accessibility(),
        }
    }

    fn open(&self, uri: &'static str) -> Result<(), PermissionError> {
        run_tool(Command::new(OPEN).arg(uri))
    }

    fn reset(&self, service: &'static str, bundle: &str) -> Result<(), PermissionError> {
        run_tool(Command::new(TCCUTIL).args(["reset", service, bundle]))
    }

    fn os_major(&self) -> Option<u32> {
        let mut buffer = [0u8; 32];
        let mut length = buffer.len();
        // SAFETY: The name is NUL-terminated, and the buffer and its length describe writable
        // memory the call fills with at most `length` bytes.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.osproductversion".as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return None;
        }
        std::str::from_utf8(&buffer[..length])
            .ok()?
            .trim_end_matches('\0')
            .split('.')
            .next()?
            .parse()
            .ok()
    }
}

fn request_capture() -> Result<(), PermissionError> {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
    // SAFETY: AVFoundation exports this immutable media type when available.
    let media_type = unsafe { AVMediaTypeAudio }.ok_or(PermissionError::Unavailable)?;
    // The decision reaches the probe's next read, so the completion keeps nothing.
    let completion = RcBlock::new(|_: Bool| {});
    // SAFETY: The media type is valid here. AVCaptureDevice copies the escaping block, which
    // captures nothing and runs on an arbitrary queue.
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &completion);
    }
    Ok(())
}

fn request_accessibility() -> Result<(), PermissionError> {
    // SAFETY: The key and value are immortal Core Foundation constants. The dictionary retains
    // them with the standard callbacks and is released after the call that reads it.
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        );
        if options.is_null() {
            return Err(PermissionError::Unavailable);
        }
        // Prompting happens asynchronously and does not change the result.
        let _ = AXIsProcessTrustedWithOptions(options);
        CFRelease(options);
    }
    Ok(())
}

fn run_tool(command: &mut Command) -> Result<(), PermissionError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PermissionError::Unavailable)?;
    match probe::wait_bounded(&mut child, TOOL_TIMEOUT) {
        Some(status) if status.success() => Ok(()),
        Some(_) => Err(PermissionError::Rejected),
        None => Err(PermissionError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Native effects whose readings and failures the test scripts, recording each effect.
    #[derive(Default)]
    struct Scripted {
        states: Mutex<Vec<(SystemPermission, PermissionState)>>,
        probe_fails: std::sync::atomic::AtomicBool,
        reset_fails: std::sync::atomic::AtomicBool,
        effects: Arc<Mutex<Vec<String>>>,
    }

    impl Native for Arc<Scripted> {
        fn probe(&self) -> Result<Probe, PermissionError> {
            self.effects.lock().unwrap().push("probe".into());
            if self.probe_fails.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(PermissionError::Unavailable);
            }
            let states = self.states.lock().unwrap().clone();
            Ok(Probe::read(|permission| {
                states
                    .iter()
                    .find_map(|(read, state)| (*read == permission).then_some(*state))
                    .unwrap_or(PermissionState::NotGranted)
            }))
        }
        fn request(&self, permission: SystemPermission) -> Result<(), PermissionError> {
            self.effects
                .lock()
                .unwrap()
                .push(format!("request {}", permission.as_str()));
            Ok(())
        }
        fn open(&self, uri: &'static str) -> Result<(), PermissionError> {
            self.effects.lock().unwrap().push(format!("open {uri}"));
            Ok(())
        }
        fn reset(&self, service: &'static str, bundle: &str) -> Result<(), PermissionError> {
            self.effects
                .lock()
                .unwrap()
                .push(format!("reset {service} {bundle}"));
            if self.reset_fails.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(PermissionError::Rejected);
            }
            Ok(())
        }
        fn os_major(&self) -> Option<u32> {
            Some(27)
        }
    }

    fn permissions(
        own: bool,
        states: &[(SystemPermission, PermissionState)],
    ) -> (MacosSystemPermissions, Arc<Scripted>) {
        let scripted = Arc::new(Scripted::default());
        *scripted.states.lock().unwrap() = states.to_vec();
        (
            MacosSystemPermissions::with(
                "spaceterm-preflight",
                own,
                Box::new(Arc::clone(&scripted)),
            ),
            scripted,
        )
    }

    fn effects(scripted: &Scripted) -> Vec<String> {
        std::mem::take(&mut *scripted.effects.lock().unwrap())
    }

    const BUNDLE: &str = "io.github.sadiksaifi.spaceterm-preflight.server";

    #[test]
    fn the_server_names_its_own_bundle_only_when_launchd_started_it_from_there() {
        let (own, _) = permissions(true, &[]);
        let report = own.read().unwrap();
        assert_eq!(
            report.holder,
            PermissionHolder::Server {
                name: "SpaceTerm Preflight Server".into()
            }
        );
        assert!(report.resettable);
        assert_eq!(
            report.accessibility_naming,
            AccessibilityNaming::DeviceControl
        );

        let (inherited, scripted) = permissions(false, &[]);
        let report = inherited.read().unwrap();
        assert_eq!(report.holder, PermissionHolder::Launcher);
        assert!(!report.resettable);
        assert_eq!(
            inherited.reset(SystemPermission::ScreenRecording),
            Err(PermissionError::Unavailable)
        );
        assert_eq!(
            effects(&scripted),
            ["probe"],
            "nothing is reset for another app"
        );
    }

    #[test]
    fn a_microphone_prompt_is_requested_only_before_anyone_decided() {
        for (state, expected, asked) in [
            (
                PermissionState::NotRequested,
                RequestOutcome::Requested,
                true,
            ),
            (
                PermissionState::NotGranted,
                RequestOutcome::Decided {
                    state: PermissionState::NotGranted,
                },
                false,
            ),
            (
                PermissionState::Granted,
                RequestOutcome::Decided {
                    state: PermissionState::Granted,
                },
                false,
            ),
            (
                PermissionState::Restricted,
                RequestOutcome::Decided {
                    state: PermissionState::Restricted,
                },
                false,
            ),
        ] {
            let (permissions, scripted) =
                permissions(true, &[(SystemPermission::Microphone, state)]);
            assert_eq!(
                permissions.request(SystemPermission::Microphone),
                Ok(expected)
            );
            let mut expected_effects = vec!["probe".to_owned()];
            if asked {
                expected_effects.push("request microphone".into());
            }
            assert_eq!(effects(&scripted), expected_effects, "{state:?}");
        }
    }

    #[test]
    fn a_permission_without_a_prompt_is_requested_until_it_is_granted() {
        let (permissions, scripted) = permissions(
            true,
            &[(
                SystemPermission::ScreenRecording,
                PermissionState::NotGranted,
            )],
        );
        assert_eq!(
            permissions.request(SystemPermission::ScreenRecording),
            Ok(RequestOutcome::Requested)
        );
        assert_eq!(effects(&scripted), ["probe", "request screen_recording"]);

        *scripted.states.lock().unwrap() =
            vec![(SystemPermission::ScreenRecording, PermissionState::Granted)];
        assert_eq!(
            permissions.request(SystemPermission::ScreenRecording),
            Ok(RequestOutcome::Decided {
                state: PermissionState::Granted
            })
        );
        assert_eq!(effects(&scripted), ["probe"]);
    }

    #[test]
    fn a_setup_clears_the_servers_earlier_entry_before_requesting_again() {
        let (permissions, scripted) = permissions(
            true,
            &[(SystemPermission::Accessibility, PermissionState::NotGranted)],
        );
        assert_eq!(
            permissions.prepare_setup(SystemPermission::Accessibility),
            Ok(SetupReadiness::Ready { cleared: true })
        );
        assert_eq!(
            effects(&scripted),
            [
                "probe".to_owned(),
                format!("reset Accessibility {BUNDLE}"),
                "request accessibility".to_owned(),
            ]
        );

        scripted
            .reset_fails
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            permissions.prepare_setup(SystemPermission::Accessibility),
            Ok(SetupReadiness::Ready { cleared: false }),
            "a failed reset leaves an entry to turn on"
        );
    }

    #[test]
    fn a_setup_never_resets_a_grant_or_acts_on_a_failed_read() {
        let (permissions, scripted) = permissions(
            true,
            &[(SystemPermission::ScreenRecording, PermissionState::Granted)],
        );
        assert_eq!(
            permissions.prepare_setup(SystemPermission::ScreenRecording),
            Ok(SetupReadiness::AlreadyGranted)
        );
        assert_eq!(effects(&scripted), ["probe"]);

        scripted
            .probe_fails
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            permissions.prepare_setup(SystemPermission::ScreenRecording),
            Err(PermissionError::Unavailable)
        );
        assert_eq!(effects(&scripted), ["probe"]);
    }

    #[test]
    fn a_setup_for_an_inherited_holder_requests_without_resetting() {
        let (permissions, scripted) = permissions(
            false,
            &[(
                SystemPermission::ScreenRecording,
                PermissionState::NotGranted,
            )],
        );
        assert_eq!(
            permissions.prepare_setup(SystemPermission::ScreenRecording),
            Ok(SetupReadiness::Ready { cleared: false })
        );
        assert_eq!(effects(&scripted), ["probe", "request screen_recording"]);
    }

    #[test]
    fn settings_routes_and_reset_services_are_exact() {
        let (permissions, scripted) = permissions(true, &[]);
        for permission in SystemPermission::ALL {
            permissions.open_settings(permission).unwrap();
            permissions.reset(permission).unwrap();
        }
        assert_eq!(
            effects(&scripted),
            [
                "open x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone".to_owned(),
                format!("reset Microphone {BUNDLE}"),
                "open x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture".to_owned(),
                format!("reset ScreenCapture {BUNDLE}"),
                "open x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility".to_owned(),
                format!("reset Accessibility {BUNDLE}"),
            ]
        );
    }

    #[test]
    fn macos_27_names_the_accessibility_list_device_control() {
        assert_eq!(
            accessibility_naming(Some(26)),
            AccessibilityNaming::Accessibility
        );
        assert_eq!(
            accessibility_naming(Some(27)),
            AccessibilityNaming::DeviceControl
        );
        assert_eq!(
            accessibility_naming(None),
            AccessibilityNaming::DeviceControl
        );
    }

    #[test]
    fn this_macos_reports_its_major_version() {
        assert!(System.os_major().is_some_and(|major| major >= 26));
    }
}
