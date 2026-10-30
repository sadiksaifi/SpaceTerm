//! The server's macOS System Permissions, which TCC keys to the server bundle's identifier and
//! signature.
//!
//! Reads run in a probe child. Requests run in the server itself, the responsible process whose
//! prompt and System Settings entry name the server.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::probe::{self, Probe};
use super::{
    AccessibilityNaming, PermissionError, PermissionHolder, PermissionReading, PermissionState,
    PermissionsReport, RequestOutcome, SetupReadiness, SystemPermission, SystemPermissions,
};
use crate::service::{Layout, bundle_identifier, bundle_name};

/// The system's own privacy reset tool, addressed by absolute path so no search path replaces it.
const TCCUTIL: &str = "/usr/bin/tccutil";
/// Both tools answer within a second. A slower one is stuck and is stopped.
const TOOL_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// Registers the bundle at `bundle` with Launch Services, so System Settings can name the
    /// entry a request lists. launchd starts the server without Launch Services.
    fn register(&self, bundle: &std::path::Path) -> Result<(), PermissionError>;
    fn reset(&self, service: &'static str, bundle: &str) -> Result<(), PermissionError>;
    fn os_major(&self) -> Option<u32>;
}

pub(crate) struct MacosSystemPermissions {
    holder: PermissionHolder,
    /// The server's own bundle, present only while launchd runs the server from this identity's
    /// bundle as its own responsible process, so a reset reaches no other grant.
    own: Option<OwnBundle>,
    native: Box<dyn Native>,
    /// Held for each probe, setup and reset, so a reset never lands between a probe and the
    /// setup that acts on its reading.
    probing: Mutex<()>,
}

struct OwnBundle {
    identifier: String,
    path: PathBuf,
}

impl MacosSystemPermissions {
    pub(crate) fn new(identity: &str) -> Self {
        // SAFETY: getppid reads this process's parent and cannot fail.
        let launched = unsafe { libc::getppid() } == 1;
        let bundle = Layout::host(identity)
            .zip(
                std::env::current_exe()
                    .and_then(|executable| executable.canonicalize())
                    .ok(),
            )
            .and_then(|(layout, executable)| own_bundle(&executable, layout))
            .filter(|_| launched);
        Self::with(identity, bundle, Box::new(System))
    }

    /// `bundle` is the server's own bundle when launchd started the server from it, which makes
    /// the server its own responsible process.
    fn with(identity: &str, bundle: Option<PathBuf>, native: Box<dyn Native>) -> Self {
        let name = bundle_name(identity).and_then(|name| name.strip_suffix(".app"));
        let (holder, own) = match (name, bundle) {
            (Some(name), Some(path)) => (
                PermissionHolder::Server { name: name.into() },
                Some(OwnBundle {
                    identifier: bundle_identifier(identity),
                    path,
                }),
            ),
            _ => (PermissionHolder::Launcher, None),
        };
        Self {
            holder,
            own,
            native,
            probing: Mutex::new(()),
        }
    }

    /// Asks the system for `permission`, after Launch Services learns the server's own bundle.
    /// A failed registration only leaves System Settings without the bundle's name. Registration
    /// can wait, so `cancelled` is read after it, immediately before the request. Returns whether
    /// the request was made.
    fn ask(
        &self,
        permission: SystemPermission,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<bool, PermissionError> {
        if let Some(own) = &self.own {
            let _ = self.native.register(&own.path);
        }
        if cancelled() {
            return Ok(false);
        }
        self.native.request(permission)?;
        Ok(true)
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
            resettable: self.own.is_some(),
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
        self.ask(permission, &|| false)?;
        Ok(RequestOutcome::Requested)
    }

    fn prepare_setup(
        &self,
        permission: SystemPermission,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SetupReadiness, PermissionError> {
        let _probing = self.probing();
        // A failed read resets nothing, so a grant it could not see is never removed.
        let state = self.native.probe()?.state(permission);
        if state == PermissionState::Granted {
            return Ok(SetupReadiness::AlreadyGranted);
        }
        // A setup may wait here for another one's probe, so it reads the cancellation again
        // right before each effect.
        if cancelled() {
            return Ok(SetupReadiness::Cancelled { cleared: false });
        }
        // A failed reset leaves an entry the person turns on instead.
        let cleared = self.own.as_ref().is_some_and(|own| {
            self.native
                .reset(tcc_service(permission), &own.identifier)
                .is_ok()
        });
        if self.ask(permission, cancelled)? {
            Ok(SetupReadiness::Ready { cleared })
        } else {
            Ok(SetupReadiness::Cancelled { cleared })
        }
    }

    fn reset(&self, permission: SystemPermission) -> Result<(), PermissionError> {
        let own = self.own.as_ref().ok_or(PermissionError::Unavailable)?;
        let _probing = self.probing();
        self.native.reset(tcc_service(permission), &own.identifier)
    }
}

/// The bundle that holds `executable` in `layout`, when it does.
fn own_bundle(executable: &Path, layout: Layout) -> Option<PathBuf> {
    let installation = layout
        .executable()
        .components()
        .try_fold(executable, |path, _| path.parent())?;
    (installation.join(layout.executable()) == executable)
        .then(|| installation.join(layout.signed()))
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

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSRegisterURL(url: CFTypeRef, update: u8) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFTypeRef,
        buffer: *const u8,
        length: isize,
        directory: u8,
    ) -> CFTypeRef;
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

    fn register(&self, bundle: &std::path::Path) -> Result<(), PermissionError> {
        register_bundle(bundle)
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

fn register_bundle(bundle: &Path) -> Result<(), PermissionError> {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = bundle.as_os_str().as_bytes();
    let length = isize::try_from(bytes.len()).map_err(|_| PermissionError::Unavailable)?;
    // SAFETY: The buffer and length describe the path's bytes for the call. The URL is released
    // after Launch Services, which retains what it keeps, reads it.
    unsafe {
        let url =
            CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), length, 1);
        if url.is_null() {
            return Err(PermissionError::Unavailable);
        }
        let status = LSRegisterURL(url, 0);
        CFRelease(url);
        if status == 0 {
            Ok(())
        } else {
            Err(PermissionError::Rejected)
        }
    }
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
        fn register(&self, bundle: &Path) -> Result<(), PermissionError> {
            self.effects
                .lock()
                .unwrap()
                .push(format!("register {}", bundle.display()));
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
                own.then(|| PathBuf::from(OWN_BUNDLE)),
                Box::new(Arc::clone(&scripted)),
            ),
            scripted,
        )
    }

    fn effects(scripted: &Scripted) -> Vec<String> {
        std::mem::take(&mut *scripted.effects.lock().unwrap())
    }

    const BUNDLE: &str = "io.github.sadiksaifi.spaceterm-preflight.server";
    const OWN_BUNDLE: &str = "/installations/aa/SpaceTerm Preflight Server.app";
    const REGISTER: &str = "register /installations/aa/SpaceTerm Preflight Server.app";

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
                expected_effects.extend([REGISTER.to_owned(), "request microphone".into()]);
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
        assert_eq!(
            effects(&scripted),
            ["probe", REGISTER, "request screen_recording"]
        );

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
            permissions.prepare_setup(SystemPermission::Accessibility, &|| false),
            Ok(SetupReadiness::Ready { cleared: true })
        );
        assert_eq!(
            effects(&scripted),
            [
                "probe".to_owned(),
                format!("reset Accessibility {BUNDLE}"),
                REGISTER.to_owned(),
                "request accessibility".to_owned(),
            ]
        );

        scripted
            .reset_fails
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            permissions.prepare_setup(SystemPermission::Accessibility, &|| false),
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
            permissions.prepare_setup(SystemPermission::ScreenRecording, &|| false),
            Ok(SetupReadiness::AlreadyGranted)
        );
        assert_eq!(effects(&scripted), ["probe"]);

        scripted
            .probe_fails
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            permissions.prepare_setup(SystemPermission::ScreenRecording, &|| false),
            Err(PermissionError::Unavailable)
        );
        assert_eq!(effects(&scripted), ["probe"]);
    }

    /// A setup cancelled while it waited resets nothing, and one cancelled after its reset makes
    /// no request, so no prompt appears after the person cancelled.
    #[test]
    fn a_cancelled_setup_takes_no_further_effect() {
        let (permissions, scripted) = permissions(
            true,
            &[(
                SystemPermission::ScreenRecording,
                PermissionState::NotGranted,
            )],
        );
        assert_eq!(
            permissions.prepare_setup(SystemPermission::ScreenRecording, &|| true),
            Ok(SetupReadiness::Cancelled { cleared: false })
        );
        assert_eq!(effects(&scripted), ["probe"]);

        let checks = std::sync::atomic::AtomicUsize::new(0);
        let cancelled_after_reset =
            || checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0;
        assert_eq!(
            permissions.prepare_setup(SystemPermission::ScreenRecording, &cancelled_after_reset),
            Ok(SetupReadiness::Cancelled { cleared: true })
        );
        assert_eq!(
            effects(&scripted),
            [
                "probe".to_owned(),
                format!("reset ScreenCapture {BUNDLE}"),
                REGISTER.to_owned(),
            ]
        );
    }

    /// A Cancel the server acknowledges while Launch Services registers the bundle still stops
    /// the request, because the last check follows the registration.
    #[test]
    fn a_setup_cancelled_during_registration_makes_no_request() {
        let (permissions, scripted) = permissions(
            true,
            &[(SystemPermission::Accessibility, PermissionState::NotGranted)],
        );
        let cancelled_by_registration = || {
            scripted
                .effects
                .lock()
                .unwrap()
                .iter()
                .any(|effect| effect.starts_with("register "))
        };
        assert_eq!(
            permissions.prepare_setup(SystemPermission::Accessibility, &cancelled_by_registration),
            Ok(SetupReadiness::Cancelled { cleared: true })
        );
        assert_eq!(
            effects(&scripted),
            [
                "probe".to_owned(),
                format!("reset Accessibility {BUNDLE}"),
                REGISTER.to_owned(),
            ],
            "the request never follows a registration the person cancelled during"
        );
    }

    /// An inherited holder is the launching app, which Launch Services already knows.
    #[test]
    fn a_setup_for_an_inherited_holder_requests_without_resetting_or_registering() {
        let (permissions, scripted) = permissions(
            false,
            &[(
                SystemPermission::ScreenRecording,
                PermissionState::NotGranted,
            )],
        );
        assert_eq!(
            permissions.prepare_setup(SystemPermission::ScreenRecording, &|| false),
            Ok(SetupReadiness::Ready { cleared: false })
        );
        assert_eq!(effects(&scripted), ["probe", "request screen_recording"]);
    }

    #[test]
    fn settings_routes_and_reset_services_are_exact() {
        let (permissions, scripted) = permissions(true, &[]);
        for permission in SystemPermission::ALL {
            permissions.reset(permission).unwrap();
        }
        assert_eq!(
            effects(&scripted),
            [
                format!("reset Microphone {BUNDLE}"),
                format!("reset ScreenCapture {BUNDLE}"),
                format!("reset Accessibility {BUNDLE}"),
            ]
        );
        assert_eq!(
            SystemPermission::ALL.map(super::super::settings_uri),
            [
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone",
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture",
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
            ]
        );
    }

    #[test]
    fn the_own_bundle_is_the_one_that_holds_the_executable() {
        let layout = Layout::for_target("aarch64-apple-darwin", "spaceterm-preflight").unwrap();
        let installation = Path::new("/data/installations/aa");
        assert_eq!(
            own_bundle(&installation.join(layout.executable()), layout),
            Some(installation.join("SpaceTerm Preflight Server.app"))
        );
        assert_eq!(
            own_bundle(
                Path::new("/Applications/SpaceTerm Development.app/Contents/MacOS/spaceterm"),
                layout
            ),
            None,
            "the Development server inside the desktop bundle is not the server's own bundle"
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
