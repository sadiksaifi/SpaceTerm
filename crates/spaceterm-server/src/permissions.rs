//! System Permissions: the operating-system privacy grants that programs in this server's
//! Terminal Sessions inherit.
//!
//! macOS attributes a program's privacy checks to its responsible process. A LaunchAgent is its
//! own responsible process and its children inherit that, so the server, not a client, holds the
//! grants its Terminal Sessions use. The server reads, requests and resets its own grants and opens
//! System Settings for them, only for a caller on its own machine: the service lane refuses a
//! paired caller before it reaches [`SystemPermissions`]. Nothing here records audio, captures the
//! screen or sends input to test a grant. [ADR 0061](../../../docs/adr/0061-own-system-permissions-in-the-server-package.md)
//! records the decisions.
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod probe;

/// One grant programs in a Terminal Session inherit from their server.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPermission {
    /// Recording audio, which voice tools need.
    Microphone,
    /// Capturing the screen, which a program needs to take screenshots.
    ScreenRecording,
    /// Controlling other applications, which a program needs to click and type. System Settings
    /// names it as [`AccessibilityNaming`] describes.
    Accessibility,
}

impl SystemPermission {
    /// Every System Permission, in the order the server reports them.
    pub const ALL: [Self; 3] = [Self::Microphone, Self::ScreenRecording, Self::Accessibility];

    /// Its name in the contract, which is also its command-line spelling with `-` for `_`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::ScreenRecording => "screen_recording",
            Self::Accessibility => "accessibility",
        }
    }

    /// Whether the system asks the person itself, in a prompt, before a first grant. Otherwise
    /// only System Settings grants it.
    pub const fn prompts(self) -> bool {
        matches!(self, Self::Microphone)
    }
}

/// The authorization a program started now in a Terminal Session receives. A program that was
/// already running keeps the authorization it started with.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    /// Nobody decided yet, so a request shows the system prompt. Only a permission that
    /// [`SystemPermission::prompts`] reads this way.
    NotRequested,
    Granted,
    /// Denied, or never turned on. The system reports a denial, a turned-off entry and a request
    /// nobody answered the same way for a permission without a prompt.
    NotGranted,
    /// A device administrator decides.
    Restricted,
}

/// What System Settings calls the Accessibility permission on this machine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityNaming {
    /// The Accessibility list, before macOS 27.
    Accessibility,
    /// The Device Control and Data Access list, from macOS 27.
    #[default]
    DeviceControl,
}

/// Whose entry in System Settings holds the grants.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "holder", rename_all = "snake_case")]
pub enum PermissionHolder {
    /// The server's own bundle, which launchd started. `name` is how System Settings lists it.
    Server { name: String },
    /// The application that started this server holds them, as a terminal holds the grants of a
    /// Development server it runs.
    Launcher,
}

/// One permission's authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionReading {
    pub permission: SystemPermission,
    pub state: PermissionState,
}

/// Every System Permission as a program started now in a Terminal Session receives it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionsReport {
    pub permissions: Vec<PermissionReading>,
    pub accessibility_naming: AccessibilityNaming,
    pub holder: PermissionHolder,
    /// Whether a reset names exactly this server's own entry.
    pub resettable: bool,
}

impl PermissionsReport {
    pub fn state(&self, permission: SystemPermission) -> Option<PermissionState> {
        self.permissions
            .iter()
            .find(|reading| reading.permission == permission)
            .map(|reading| reading.state)
    }
}

/// What a request did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RequestOutcome {
    /// The system shows its prompt, or lists the holder in System Settings to turn on.
    Requested,
    /// There was nothing to ask: the permission already stands at `state`.
    Decided { state: PermissionState },
}

/// What preparing a Permission Setup found.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "readiness", rename_all = "snake_case")]
pub enum SetupReadiness {
    /// Programs started now already receive the permission, so nothing changed.
    AlreadyGranted,
    /// System Settings lists the holder for the person to turn on. `cleared` says whether the
    /// setup first removed the holder's earlier entry, which an earlier build may have left.
    Ready { cleared: bool },
}

/// Content-free failures of a System Permission operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PermissionError {
    /// This platform has no System Permissions.
    #[error("this platform has no System Permissions")]
    Unsupported,
    /// The operation could not start or did not answer.
    #[error("the permission operation is unavailable")]
    Unavailable,
    /// The system refused the operation.
    #[error("the system refused the permission operation")]
    Rejected,
}

impl PermissionError {
    /// The CLI output contract's code for this failure.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Unavailable | Self::Rejected => PERMISSION_FAILED,
        }
    }
}

/// The contract code of a System Permission operation the system did not complete.
pub const PERMISSION_FAILED: &str = "permission_failed";

/// The server's own System Permissions. Each operation acts on this server's grant alone.
pub trait SystemPermissions: Send + Sync {
    /// Reads every permission as a program started now receives it.
    fn read(&self) -> Result<PermissionsReport, PermissionError>;

    /// Asks the system for `permission`: a prompt for one that [`SystemPermission::prompts`],
    /// otherwise an entry in System Settings that the person turns on.
    fn request(&self, permission: SystemPermission) -> Result<RequestOutcome, PermissionError>;

    /// Opens System Settings at the list that holds `permission`.
    fn open_settings(&self, permission: SystemPermission) -> Result<(), PermissionError>;

    /// Prepares System Settings for a Permission Setup: when programs do not receive the
    /// permission, removes the holder's earlier entry if it can, then requests it again so
    /// System Settings lists the holder.
    fn prepare_setup(
        &self,
        permission: SystemPermission,
    ) -> Result<SetupReadiness, PermissionError>;

    /// Forgets the system's decision for `permission` and this server alone.
    fn reset(&self, permission: SystemPermission) -> Result<(), PermissionError>;
}

/// A platform without System Permissions.
pub struct Unsupported;

impl SystemPermissions for Unsupported {
    fn read(&self) -> Result<PermissionsReport, PermissionError> {
        Err(PermissionError::Unsupported)
    }
    fn request(&self, _: SystemPermission) -> Result<RequestOutcome, PermissionError> {
        Err(PermissionError::Unsupported)
    }
    fn open_settings(&self, _: SystemPermission) -> Result<(), PermissionError> {
        Err(PermissionError::Unsupported)
    }
    fn prepare_setup(&self, _: SystemPermission) -> Result<SetupReadiness, PermissionError> {
        Err(PermissionError::Unsupported)
    }
    fn reset(&self, _: SystemPermission) -> Result<(), PermissionError> {
        Err(PermissionError::Unsupported)
    }
}

/// This platform's System Permissions for the server of `identity`.
pub fn native(identity: &str) -> Arc<dyn SystemPermissions> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacosSystemPermissions::new(identity))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identity;
        Arc::new(Unsupported)
    }
}

/// The probe role: one line reporting this process's own authorization, which a child of the
/// server reads the way a program started now in a Terminal Session does. `None` on a platform
/// without System Permissions.
pub fn probe_report() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some(probe::Probe::read_in_process().encode())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;

    /// System Permissions whose report and failures a test controls, recording each call.
    pub(crate) struct ScriptedPermissions {
        pub(crate) report: Mutex<Result<PermissionsReport, PermissionError>>,
        pub(crate) calls: Mutex<Vec<(&'static str, SystemPermission)>>,
    }

    impl ScriptedPermissions {
        pub(crate) fn granting(state: PermissionState) -> Arc<Self> {
            Arc::new(Self {
                report: Mutex::new(Ok(PermissionsReport {
                    permissions: SystemPermission::ALL
                        .map(|permission| PermissionReading { permission, state })
                        .to_vec(),
                    accessibility_naming: AccessibilityNaming::DeviceControl,
                    holder: PermissionHolder::Server {
                        name: "SpaceTerm Development Server".into(),
                    },
                    resettable: true,
                })),
                calls: Mutex::default(),
            })
        }

        fn record(&self, call: &'static str, permission: SystemPermission) {
            self.calls.lock().unwrap().push((call, permission));
        }
    }

    impl SystemPermissions for ScriptedPermissions {
        fn read(&self) -> Result<PermissionsReport, PermissionError> {
            self.report.lock().unwrap().clone()
        }
        fn request(&self, permission: SystemPermission) -> Result<RequestOutcome, PermissionError> {
            self.record("request", permission);
            Ok(RequestOutcome::Requested)
        }
        fn open_settings(&self, permission: SystemPermission) -> Result<(), PermissionError> {
            self.record("open_settings", permission);
            Ok(())
        }
        fn prepare_setup(
            &self,
            permission: SystemPermission,
        ) -> Result<SetupReadiness, PermissionError> {
            self.record("prepare_setup", permission);
            Ok(SetupReadiness::Ready { cleared: true })
        }
        fn reset(&self, permission: SystemPermission) -> Result<(), PermissionError> {
            self.record("reset", permission);
            Ok(())
        }
    }
}
