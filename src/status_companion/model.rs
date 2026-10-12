//! What the status area shows. The model holds only bounded, displayable facts: a state,
//! filtered version strings and typed reasons. It never holds paths, server-supplied
//! free text beyond a version, credentials or terminal content.

use spaceterm_client::management_cli::{ManagementFailure, ServerStatus};
use spaceterm_client_native::cli::bindings::NativeObservation;
use spaceterm_host::startup::DesiredServerState;

use super::CompanionIdentity;
use super::controller::CompanionAction;

/// The one fact the status area names. Each state has its own text and symbol shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum ServerState {
    Connecting,
    Running,
    Updating,
    Stopped,
    Offline,
    Incompatible,
    NotPaired,
    Unavailable,
}

impl ServerState {
    #[cfg_attr(not(test), allow(dead_code, reason = "tests enumerate every state"))]
    pub(crate) const ALL: [Self; 8] = [
        Self::Connecting,
        Self::Running,
        Self::Updating,
        Self::Stopped,
        Self::Offline,
        Self::Incompatible,
        Self::NotPaired,
        Self::Unavailable,
    ];

    pub(crate) const fn text(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting to Server…",
            Self::Running => "Server Running",
            Self::Updating => "Server Updating",
            Self::Stopped => "Server Stopped",
            Self::Offline => "Server Offline",
            Self::Incompatible => "Server Incompatible",
            Self::NotPaired => "Not Paired",
            Self::Unavailable => "Status Unavailable",
        }
    }
}

/// Why the server is not running normally, with the next step the user can take. The
/// text names only places a desktop package installs: its Settings and its menu commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Reason {
    PairingRequired,
    /// The device is paired, but no binding authorizes this companion's own Client (C23).
    NotAllowed,
    Revoked,
    Stopped,
    Incompatible,
    Offline,
    /// The server's own storage is locked. The server resumes on its own after unlock.
    StorageLocked,
    /// Native discovery found no usable server: its registration or installation is
    /// missing, its supervisor is unavailable or its background item is not allowed.
    NotSetUp,
    /// A failure a later read can clear, such as a credential that is not available yet.
    Unavailable,
    /// This installation cannot read status at all, so neither a timer nor Retry can help.
    Unsupported,
}

impl Reason {
    fn text(self, identity: CompanionIdentity) -> String {
        let desktop = identity.desktop_name();
        match self {
            Self::PairingRequired => format!("Pair this device in {desktop} Settings."),
            Self::NotAllowed => format!(
                "Allow {} to read status in {desktop} Settings.",
                identity.display_name()
            ),
            Self::Revoked => format!("Pair this device again in {desktop} Settings."),
            Self::Stopped => format!("Start it from {desktop} Settings."),
            Self::Incompatible => format!("Update {desktop} or the server to reconnect."),
            Self::Offline => "Retrying automatically.".to_owned(),
            Self::StorageLocked => {
                "The server is waiting for its storage to unlock. Status updates automatically."
                    .to_owned()
            }
            Self::NotSetUp => format!("Set up the server in {desktop} Settings."),
            Self::Unavailable => {
                "Server status could not be read. Retrying automatically.".to_owned()
            }
            Self::Unsupported => {
                format!("This installation cannot report server status. Open {desktop} to see it.")
            }
        }
    }
}

/// A local result the user should see once, attached to the current state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Notice {
    DesktopUnavailable,
}

/// A server version that is safe to show: 1 to 64 bytes of `[0-9A-Za-z.+-]`. Anything
/// else, including path-shaped or multi-line values, is omitted rather than escaped.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DisplayVersion(String);

impl DisplayVersion {
    const MAX: usize = 64;

    fn new(value: &str) -> Option<Self> {
        let safe = !value.is_empty()
            && value.len() <= Self::MAX
            && !value.contains("..")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'));
        safe.then(|| Self(value.to_owned()))
    }
}

/// The symbol for a state on each platform. macOS uses template SF Symbols and Linux uses
/// freedesktop symbolic icon names, so the system tints both and no state relies on color.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StatusSymbol {
    macos: &'static str,
    freedesktop: &'static str,
}

impl StatusSymbol {
    #[cfg_attr(
        not(any(test, target_os = "macos")),
        allow(dead_code, reason = "macOS only")
    )]
    pub(crate) const fn macos(self) -> &'static str {
        self.macos
    }

    #[cfg_attr(
        not(any(test, target_os = "linux")),
        allow(dead_code, reason = "Linux only")
    )]
    pub(crate) const fn freedesktop(self) -> &'static str {
        self.freedesktop
    }
}

/// One status-area menu entry. Identifiers are stable per role so a host that tracks
/// focus or expansion by identifier keeps it across updates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MenuEntry {
    Header {
        id: i32,
        text: String,
    },
    Information {
        id: i32,
        text: String,
    },
    Separator {
        id: i32,
    },
    Action {
        id: i32,
        action: CompanionAction,
        title: String,
        /// The macOS key equivalent, used with Command.
        key: char,
        /// The Linux access key, marked in the label by [`mnemonic_label`].
        mnemonic: char,
    },
}

impl MenuEntry {
    #[cfg_attr(
        not(any(test, target_os = "linux")),
        allow(dead_code, reason = "DBusMenu addresses entries by identifier")
    )]
    pub(crate) const fn id(&self) -> i32 {
        match self {
            Self::Header { id, .. }
            | Self::Information { id, .. }
            | Self::Separator { id }
            | Self::Action { id, .. } => *id,
        }
    }
}

mod ids {
    pub(super) const HEADER: i32 = 1;
    pub(super) const REFRESHING: i32 = 2;
    pub(super) const VERSION: i32 = 3;
    pub(super) const TARGET: i32 = 4;
    pub(super) const STAGED: i32 = 5;
    pub(super) const REASON: i32 = 6;
    pub(super) const UNVERIFIED: i32 = 7;
    pub(super) const NOTICE: i32 = 8;
    pub(super) const STATUS_SEPARATOR: i32 = 10;
    pub(super) const OPEN: i32 = 11;
    pub(super) const RETRY: i32 = 12;
    pub(super) const QUIT_SEPARATOR: i32 = 20;
    pub(super) const QUIT: i32 = 21;
}

/// The single replaceable value the status area renders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompanionModel {
    state: ServerState,
    version: Option<DisplayVersion>,
    target: Option<DisplayVersion>,
    staged: Option<DisplayVersion>,
    reason: Option<Reason>,
    unverified: bool,
    refreshing: bool,
    notice: Option<Notice>,
}

impl CompanionModel {
    const fn new(state: ServerState, reason: Option<Reason>) -> Self {
        Self {
            state,
            version: None,
            target: None,
            staged: None,
            reason,
            unverified: false,
            refreshing: false,
            notice: None,
        }
    }

    pub(crate) const fn connecting() -> Self {
        Self::new(ServerState::Connecting, None)
    }

    /// A companion that cannot read status in this installation.
    pub(crate) const fn unsupported() -> Self {
        Self::new(ServerState::Unavailable, Some(Reason::Unsupported))
    }

    /// Updating means the server runs one version and has approved another. A staged
    /// version is listed only when it is neither of those.
    pub(crate) fn from_status(status: &ServerStatus) -> Self {
        let parts = status.parts();
        let running = parts.running.as_ref().map(|version| version.as_str());
        let approved = parts.approved.as_ref().map(|version| version.as_str());
        let staged = parts.staged.as_ref().map(|version| version.as_str());
        let updating =
            matches!((running, approved), (Some(running), Some(approved)) if running != approved);
        let mut model = Self::new(
            if updating {
                ServerState::Updating
            } else {
                ServerState::Running
            },
            None,
        );
        model.version = running.and_then(DisplayVersion::new);
        if updating {
            model.target = approved.and_then(DisplayVersion::new);
        }
        if staged.is_some_and(|staged| Some(staged) != running && Some(staged) != approved) {
            model.staged = staged.and_then(DisplayVersion::new);
        }
        model
    }

    /// Classifies a failed connection or read. Only offline-like failures consult the local
    /// native observation, and only to tell a deliberately stopped server from a lost one.
    pub(crate) fn disconnected(
        failure: ManagementFailure,
        native: Option<&NativeObservation>,
    ) -> Self {
        use ManagementFailure as Failure;
        match failure {
            Failure::Offline
            | Failure::IoUnavailable
            | Failure::OutcomePending
            | Failure::Indeterminate => match native {
                Some(observation) if observation.desired == Some(DesiredServerState::Stopped) => {
                    let mut model = Self::new(ServerState::Stopped, Some(Reason::Stopped));
                    model.unverified = !observation.verified;
                    model
                }
                _ => Self::new(ServerState::Offline, Some(Reason::Offline)),
            },
            Failure::Incompatible => {
                Self::new(ServerState::Incompatible, Some(Reason::Incompatible))
            }
            Failure::PairingRequired => {
                Self::new(ServerState::NotPaired, Some(Reason::PairingRequired))
            }
            Failure::AuthorizationDenied => {
                Self::new(ServerState::NotPaired, Some(Reason::NotAllowed))
            }
            Failure::Revoked => Self::new(ServerState::NotPaired, Some(Reason::Revoked)),
            Failure::StorageLocked => {
                Self::new(ServerState::Unavailable, Some(Reason::StorageLocked))
            }
            Failure::NativeUnavailable => {
                Self::new(ServerState::Unavailable, Some(Reason::NotSetUp))
            }
            Failure::UnsupportedHost => Self::unsupported(),
            _ => Self::new(ServerState::Unavailable, Some(Reason::Unavailable)),
        }
    }

    /// Whether a failure is one the local native observation can explain.
    pub(crate) const fn consults_native(failure: ManagementFailure) -> bool {
        matches!(
            failure,
            ManagementFailure::Offline
                | ManagementFailure::IoUnavailable
                | ManagementFailure::OutcomePending
                | ManagementFailure::Indeterminate
        )
    }

    pub(crate) fn with_refreshing(mut self, refreshing: bool) -> Self {
        self.refreshing = refreshing;
        self
    }

    pub(crate) fn with_notice(mut self, notice: Option<Notice>) -> Self {
        self.notice = notice;
        self
    }

    pub(crate) const fn state(&self) -> ServerState {
        self.state
    }

    pub(crate) const fn state_text(&self) -> &'static str {
        self.state.text()
    }

    #[cfg(test)]
    pub(crate) fn running_version(&self) -> Option<&str> {
        self.version.as_ref().map(|version| version.0.as_str())
    }

    #[cfg(test)]
    pub(crate) const fn reason(&self) -> Option<Reason> {
        self.reason
    }

    #[cfg(test)]
    pub(crate) const fn notice(&self) -> Option<Notice> {
        self.notice
    }

    #[cfg(test)]
    pub(crate) const fn is_refreshing(&self) -> bool {
        self.refreshing
    }

    /// A Retry Connection command appears only where a new read could change the result.
    /// A stopped server is refreshed passively, because retrying cannot start it. An
    /// installation that cannot report status offers no Retry, because none can succeed.
    pub(crate) fn offers_retry(&self) -> bool {
        matches!(
            self.state,
            ServerState::Offline
                | ServerState::Incompatible
                | ServerState::NotPaired
                | ServerState::Unavailable
        ) && self.reason != Some(Reason::Unsupported)
    }

    /// A revoked binding is not retried on a timer. Only the user's explicit retry reads
    /// again. An installation that cannot report status is not read again at all.
    pub(crate) fn retries_automatically(&self) -> bool {
        !matches!(self.reason, Some(Reason::Revoked | Reason::Unsupported))
    }

    pub(crate) const fn symbol(&self) -> StatusSymbol {
        let (macos, freedesktop) = match self.state {
            ServerState::Running => ("terminal", "utilities-terminal-symbolic"),
            ServerState::Updating => (
                "arrow.triangle.2.circlepath",
                "software-update-available-symbolic",
            ),
            ServerState::Connecting => ("ellipsis.circle", "network-transmit-receive-symbolic"),
            ServerState::Stopped => ("stop.circle", "media-playback-stop-symbolic"),
            ServerState::Offline => ("bolt.horizontal.circle", "network-offline-symbolic"),
            ServerState::Incompatible => ("exclamationmark.triangle", "dialog-warning-symbolic"),
            ServerState::NotPaired => ("lock", "changes-prevent-symbolic"),
            ServerState::Unavailable => ("questionmark.circle", "dialog-question-symbolic"),
        };
        StatusSymbol { macos, freedesktop }
    }

    /// What the status item says about the server: the state, and a resync in progress.
    pub(crate) fn state_description(&self) -> String {
        let mut description = self.state_text().to_owned();
        if self.refreshing {
            description.push_str(", Refreshing Status");
        }
        description
    }

    /// The status item's accessible name: the companion's name, then the state.
    #[cfg_attr(
        not(any(test, target_os = "macos")),
        allow(
            dead_code,
            reason = "a StatusNotifierItem host composes its own name and description"
        )
    )]
    pub(crate) fn accessibility_label(&self, identity: CompanionIdentity) -> String {
        format!("{}, {}", identity.display_name(), self.state_description())
    }

    #[cfg_attr(
        not(any(test, target_os = "macos")),
        allow(
            dead_code,
            reason = "a StatusNotifierItem host composes its own tool tip"
        )
    )]
    pub(crate) fn tooltip(&self, identity: CompanionIdentity) -> String {
        format!("{}: {}", identity.display_name(), self.state_description())
    }

    /// The state with any resync, then every other status line, one per line. A
    /// StatusNotifierItem host shows it as the tool tip text and exposes it as the item's
    /// accessible description. GTK and Qt menus skip disabled items during keyboard
    /// navigation, so this is how a screen reader reaches the reason and any notice.
    #[cfg_attr(
        not(any(test, target_os = "linux")),
        allow(
            dead_code,
            reason = "VoiceOver reads the disabled status lines in the macOS menu"
        )
    )]
    pub(crate) fn description(&self, identity: CompanionIdentity) -> String {
        std::iter::once(self.state_description())
            .chain(
                self.status_lines(identity)
                    .into_iter()
                    .filter(|(id, _)| *id != ids::REFRESHING)
                    .map(|(_, text)| text),
            )
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The status lines after the state, each with its stable menu identifier.
    fn status_lines(&self, identity: CompanionIdentity) -> Vec<(i32, String)> {
        let desktop = identity.desktop_name();
        let mut lines = Vec::new();
        let mut information = |id, text: String| lines.push((id, text));
        if self.refreshing {
            information(ids::REFRESHING, "Refreshing Status".to_owned());
        }
        if let Some(version) = &self.version {
            information(ids::VERSION, format!("Version {}", version.0));
        }
        if let Some(target) = &self.target {
            information(ids::TARGET, format!("Updating to {}", target.0));
        }
        if let Some(staged) = &self.staged {
            information(ids::STAGED, format!("Update {} Staged", staged.0));
        }
        if let Some(reason) = self.reason {
            information(ids::REASON, reason.text(identity));
        }
        if self.unverified {
            information(
                ids::UNVERIFIED,
                "This state is from an unverified local check.".to_owned(),
            );
        }
        if self.notice == Some(Notice::DesktopUnavailable) {
            information(ids::NOTICE, format!("Could not open {desktop}."));
        }
        lines
    }

    /// Status lines first, then commands. Status lines are disabled items, so keyboard focus
    /// moves only between commands. VoiceOver still reads every line in the macOS menu; on
    /// Linux, [`CompanionModel::description`] carries them.
    pub(crate) fn menu(&self, identity: CompanionIdentity) -> Vec<MenuEntry> {
        let desktop = identity.desktop_name();
        let mut entries = vec![MenuEntry::Header {
            id: ids::HEADER,
            text: self.state_text().to_owned(),
        }];
        entries.extend(
            self.status_lines(identity)
                .into_iter()
                .map(|(id, text)| MenuEntry::Information { id, text }),
        );
        entries.push(MenuEntry::Separator {
            id: ids::STATUS_SEPARATOR,
        });
        entries.push(MenuEntry::Action {
            id: ids::OPEN,
            action: CompanionAction::OpenDesktop,
            title: format!("Open {desktop}"),
            key: 'o',
            mnemonic: 'O',
        });
        if self.offers_retry() {
            entries.push(MenuEntry::Action {
                id: ids::RETRY,
                action: CompanionAction::RetryConnection,
                title: "Retry Connection".to_owned(),
                key: 'r',
                mnemonic: 'R',
            });
        }
        entries.push(MenuEntry::Separator {
            id: ids::QUIT_SEPARATOR,
        });
        entries.push(MenuEntry::Action {
            id: ids::QUIT,
            action: CompanionAction::Quit,
            title: format!("Quit {}", identity.display_name()),
            key: 'q',
            mnemonic: 'Q',
        });
        entries
    }
}

/// A DBusMenu label: literal underscores are doubled and one underscore marks the access key.
#[cfg_attr(
    not(any(test, target_os = "linux")),
    allow(dead_code, reason = "Linux only")
)]
pub(crate) fn mnemonic_label(title: &str, mnemonic: char) -> String {
    let mut label = String::with_capacity(title.len() + 2);
    let mut marked = false;
    for character in title.chars() {
        if character == '_' {
            label.push_str("__");
            continue;
        }
        if !marked && character == mnemonic {
            label.push('_');
            marked = true;
        }
        label.push(character);
    }
    label
}
