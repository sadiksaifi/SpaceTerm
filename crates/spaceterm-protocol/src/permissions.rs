//! System permission facts carried on the service lane.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One grant programs in a Terminal Session inherit from their server.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPermission {
    /// Recording audio, which voice tools need.
    Microphone,
    /// Recording video, which programs need to take photos or join calls.
    Camera,
    /// Capturing the screen, which a program needs to take screenshots.
    ScreenRecording,
    /// Controlling other applications, which a program needs to click and type. System Settings
    /// names it as [`AccessibilityNaming`] describes.
    Accessibility,
}

impl SystemPermission {
    /// Every System Permission, in the order the server reports them.
    pub const ALL: [Self; 4] = [
        Self::Microphone,
        Self::Camera,
        Self::ScreenRecording,
        Self::Accessibility,
    ];

    /// Its name in the contract, which is also its command-line spelling with `-` for `_`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Camera => "camera",
            Self::ScreenRecording => "screen_recording",
            Self::Accessibility => "accessibility",
        }
    }

    /// Whether the system asks the person itself, in a prompt, before a first grant. Otherwise
    /// only System Settings grants it.
    pub const fn prompts(self) -> bool {
        matches!(self, Self::Microphone | Self::Camera)
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
    /// `bundle` is where that bundle lies, which a person drags into a System Settings list to
    /// grant a permission. It is absent until the server reports it.
    Core {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bundle: Option<PathBuf>,
    },
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
    /// System Settings is ready for the person to grant the permission: the server's own bundle
    /// is dragged into the list, and an application that holds the grants for its server is listed
    /// to turn on. `cleared` says whether the setup removed the holder's earlier entry, which an
    /// earlier build may have left.
    Ready { cleared: bool },
    /// The person cancelled the setup before its next effect, so it made no request. `cleared`
    /// says whether it had already removed the holder's earlier entry.
    Cancelled { cleared: bool },
}
