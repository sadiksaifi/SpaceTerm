//! The Servers section: the SpaceTerm Server for this account, this device's pairing with it, and
//! its authenticated status. [`crate::local_server`] owns setup, pairing, and authentication.
//!
//! Every build offers the section for its own identity's server. Opening Settings asks the server
//! nothing. Presenting the section checks the saved pairing, and only the person's explicit Set Up
//! or Start sets up or starts a server.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Task, div};
use spaceterm_ui::{ProgressRing, ProgressSize, ProgressState};

use super::SettingsWindow;
use crate::application_identity::ApplicationIdentity;
use crate::local_server::{AuthenticatedStatus, LocalServer, ServerError};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::sidebar_window::form::{CaptionTone, action_button, badge};

pub(super) const CONTROL_SELECTOR: &str = "settings-local-server-control";
pub(super) const PROGRESS_SELECTOR: &str = "settings-local-server-progress";
pub(super) const CHECK_SELECTOR: &str = "settings-local-server-check";
pub(super) const SET_UP_SELECTOR: &str = "settings-local-server-set-up";
pub(super) const START_SELECTOR: &str = "settings-local-server-start";
pub(super) const OPEN_DEVELOPMENT_PANE_SELECTOR: &str =
    "settings-local-server-open-development-pane";
pub(super) const SHARED_WORKSPACES_SELECTOR: &str = "settings-local-server-shared-workspaces";

/// The blocking local server operations the section runs on the background executor.
pub(super) trait LocalServerControl: Send + Sync {
    /// Authenticates with the saved pairing. Never creates an identity or starts a server.
    fn refresh(&self) -> Result<ServerConnection, ServerError>;
    /// Sets up and pairs when needed, starts the server when it is not running, and authenticates.
    fn setup_and_start(&self) -> Result<ServerConnection, ServerError>;
}

impl LocalServerControl for LocalServer {
    fn refresh(&self) -> Result<ServerConnection, ServerError> {
        LocalServer::refresh(self).map(ServerConnection::from)
    }

    fn setup_and_start(&self) -> Result<ServerConnection, ServerError> {
        LocalServer::setup_and_start(self).map(ServerConnection::from)
    }
}

/// What the section presents about an authenticated connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ServerConnection {
    pub(super) protocol_major: u16,
    /// Whether the server said it runs the Development Pane's Terminal Session. A server that
    /// predates terminal operations authenticates without saying so.
    pub(super) development_pane: bool,
}

impl From<AuthenticatedStatus> for ServerConnection {
    fn from(status: AuthenticatedStatus) -> Self {
        Self {
            protocol_major: status.protocol_major,
            development_pane: status.development_pane,
        }
    }
}

/// The local server a Settings window presents. Every build offers one. `None` hides the Servers
/// section.
pub(super) type OfferedLocalServer = Option<Result<Arc<dyn LocalServerControl>, ServerError>>;

/// Resolves the running build's local server. Resolution reads paths only: it sets up, starts,
/// and reads credentials for nothing.
pub(super) fn offered_local_server() -> OfferedLocalServer {
    Some(
        LocalServer::for_identity(ApplicationIdentity::current().directory_name())
            .map(|server| Arc::new(server) as Arc<dyn LocalServerControl>),
    )
}

/// The step a person can take from the row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LocalServerAction {
    /// Authenticates with the saved pairing.
    Check,
    /// Sets up the server's identity and this device's pairing, then starts the server.
    SetUp,
    /// Starts the paired server.
    Start,
}

impl LocalServerAction {
    /// The button label. Checking reads as the Updates section's check does.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Check => "Check Now",
            Self::SetUp => "Set Up Server",
            Self::Start => "Start Server",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Check => CHECK_SELECTOR,
            Self::SetUp => SET_UP_SELECTOR,
            Self::Start => START_SELECTOR,
        }
    }
}

/// What SpaceTerm last learned about the local server.
#[derive(Clone, Copy, Debug)]
pub(super) enum LocalServerStatus {
    /// Nothing has been asked yet.
    NotChecked,
    /// One action awaits its result.
    Working(LocalServerAction),
    Connected(ServerConnection),
    Failed(ServerError),
}

/// The row's complete presentation, derived from status alone so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LocalServerPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) tone: CaptionTone,
    /// The step the row offers, if any.
    pub(super) action: Option<LocalServerAction>,
    /// Whether an action is running. The row shows progress and withholds its action meanwhile.
    pub(super) working: bool,
    /// Whether the row offers Open Development Pane, which needs an authenticated server that
    /// runs it.
    pub(super) development_pane: bool,
    /// Whether the row offers Use Shared Workspaces, which presents this server's Workspaces in
    /// the Workspace Window beside the Workspaces the desktop owns.
    pub(super) shared_workspaces: bool,
}

/// Which of the server's two terminal surfaces a device may open now.
///
/// One device holds one authenticated connection per Pane. Two surfaces presenting the same Pane
/// would rebind its lease and fence each other into reconnecting, so only one is offered at a
/// time. Shared Workspaces are the ordinary route; the Development Pane window stays as a single
/// Pane for diagnosis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ServerSurfaces {
    pub(super) shared_workspaces: bool,
    pub(super) development_pane: bool,
}

impl ServerSurfaces {
    const SHARED_HOLDS: &'static str = "A Workspace Window is presenting this server's Workspaces. One device holds one \
         connection per Pane.";
    const DEVELOPMENT_PANE_HOLDS: &'static str = "The Development Pane window is presenting a Pane this server owns. Close it to present \
         this server's Workspaces instead.";

    /// What each surface offers, given which one is open.
    pub(super) fn resolve(shared_adopted: bool, development_pane_open: bool) -> Self {
        Self {
            shared_workspaces: !development_pane_open,
            development_pane: !shared_adopted,
        }
    }

    pub(super) const fn shared_workspaces_reason(self) -> Option<&'static str> {
        if self.shared_workspaces {
            None
        } else {
            Some(Self::DEVELOPMENT_PANE_HOLDS)
        }
    }

    pub(super) const fn development_pane_reason(self) -> Option<&'static str> {
        if self.development_pane {
            None
        } else {
            Some(Self::SHARED_HOLDS)
        }
    }
}

/// Owns the offered server, the last status, and the pending operation.
pub(super) struct LocalServerRow {
    control: Result<Arc<dyn LocalServerControl>, ServerError>,
    status: LocalServerStatus,
    /// Returns the background result to GPUI. Dropping the window drops it, and a late result then
    /// has nowhere to land.
    _operation: Option<Task<()>>,
}

impl LocalServerRow {
    pub(super) fn new(control: Result<Arc<dyn LocalServerControl>, ServerError>) -> Self {
        Self {
            control,
            status: LocalServerStatus::NotChecked,
            _operation: None,
        }
    }

    #[cfg(test)]
    pub(super) fn status(&self) -> LocalServerStatus {
        self.status
    }

    pub(super) fn presentation(&self) -> LocalServerPresentation {
        let status = match &self.control {
            Err(error) => LocalServerStatus::Failed(*error),
            Ok(_) => self.status,
        };
        let mut presentation = present(status);
        // A server that could not be resolved offers no step: checking again resolves nothing.
        if self.control.is_err() {
            presentation.action = None;
        }
        // Opening never sets up or starts the server, so it waits for a successful check. A
        // server that does not run the Development Pane is offered no terminal operation.
        presentation.development_pane = self.control.is_ok()
            && matches!(
                status,
                LocalServerStatus::Connected(ServerConnection {
                    development_pane: true,
                    ..
                })
            );
        // Presenting the server's Workspaces reads its inventory, so it waits for the same
        // successful check. The Workspace Window keeps the Workspaces it already owns.
        presentation.shared_workspaces = presentation.development_pane;
        presentation
    }
}

/// The presentation of one status.
pub(super) fn present(status: LocalServerStatus) -> LocalServerPresentation {
    use LocalServerAction as Action;

    let guidance =
        |explanation: &'static str| (SharedString::new_static(explanation), CaptionTone::Guidance);
    let (state, state_selector, (explanation, tone), action, working) = match status {
        LocalServerStatus::NotChecked => (
            "Not Checked",
            "settings-local-server-state-not-checked",
            guidance("SpaceTerm checks the server when you open this section."),
            Some(Action::Check),
            false,
        ),
        LocalServerStatus::Working(action) => (
            match action {
                Action::Check => "Checking",
                Action::SetUp => "Setting Up",
                Action::Start => "Starting",
            },
            "settings-local-server-state-working",
            guidance(match action {
                Action::Check => "Connecting to the server…",
                Action::SetUp => {
                    "Setting up the server and pairing this device. The system may ask you to \
                     allow access to its secure storage."
                }
                Action::Start => "Starting the server…",
            }),
            Some(action),
            true,
        ),
        LocalServerStatus::Connected(connection) => (
            "Connected",
            "settings-local-server-state-connected",
            (
                SharedString::from(format!(
                    "This device is paired and authenticated. Protocol version {}.",
                    connection.protocol_major
                )),
                CaptionTone::Guidance,
            ),
            Some(Action::Check),
            false,
        ),
        LocalServerStatus::Failed(error) => {
            let (state, state_selector, action) = failure(error);
            let explanation = match error {
                ServerError::NotPaired => guidance(
                    "Set Up creates a server for your account, pairs this device with it, saves \
                     the device's credential in the system's secure storage, and starts the \
                     server.",
                ),
                ServerError::NotRunning => {
                    guidance("This device is paired. Start the server to connect.")
                }
                _ => (SharedString::from(error.to_string()), CaptionTone::Error),
            };
            (state, state_selector, explanation, action, false)
        }
    };
    LocalServerPresentation {
        state,
        state_selector,
        explanation,
        tone,
        action,
        working,
        development_pane: false,
        shared_workspaces: false,
    }
}

/// The badge, its selector, and the step that can recover from one failure.
fn failure(error: ServerError) -> (&'static str, &'static str, Option<LocalServerAction>) {
    use LocalServerAction as Action;

    match error {
        ServerError::NotPaired => (
            "Not Set Up",
            "settings-local-server-state-not-paired",
            Some(Action::SetUp),
        ),
        ServerError::NotRunning => (
            "Not Running",
            "settings-local-server-state-not-running",
            Some(Action::Start),
        ),
        ServerError::StartupFailed => (
            "Not Started",
            "settings-local-server-state-startup-failed",
            Some(Action::Start),
        ),
        // Unreadable or unsafe storage and a refused or incompatible server are never repaired by
        // setting up again, so the row offers only another check.
        ServerError::CredentialUnavailable | ServerError::StorageUnavailable => (
            "Unavailable",
            "settings-local-server-state-unavailable",
            Some(Action::Check),
        ),
        ServerError::UnsafeStorage => (
            "Unsafe Storage",
            "settings-local-server-state-unsafe-storage",
            Some(Action::Check),
        ),
        ServerError::AuthenticationRefused => (
            "Refused",
            "settings-local-server-state-refused",
            Some(Action::Check),
        ),
        ServerError::IncompatibleProtocol => (
            "Incompatible",
            "settings-local-server-state-incompatible",
            Some(Action::Check),
        ),
        ServerError::Protocol => (
            "Protocol Error",
            "settings-local-server-state-protocol-error",
            Some(Action::Check),
        ),
        // Terminal Session refusals reach this row only through a check, which another check
        // answers afresh.
        ServerError::Controlled => (
            "In Use",
            "settings-local-server-state-controlled",
            Some(Action::Check),
        ),
        ServerError::SessionUnavailable | ServerError::StaleControl => (
            "Unavailable",
            "settings-local-server-state-session-unavailable",
            Some(Action::Check),
        ),
        ServerError::DirectoryUnavailable => (
            "Directory Unavailable",
            "settings-local-server-state-directory-unavailable",
            Some(Action::Check),
        ),
        ServerError::Unsupported => (
            "Unsupported",
            "settings-local-server-state-unsupported",
            Some(Action::Check),
        ),
        ServerError::CleanupUnresolved => (
            "Cleanup Unresolved",
            "settings-local-server-state-cleanup-unresolved",
            Some(Action::Check),
        ),
        // The server is holding shared edit outcomes for clients that have not acknowledged
        // them. Another check reports whether they have since.
        ServerError::Capacity => (
            "At Capacity",
            "settings-local-server-state-capacity",
            Some(Action::Check),
        ),
    }
}

impl SettingsWindow {
    /// Checks the saved pairing as the section comes into view. A running action keeps its place.
    pub(super) fn check_local_server(&mut self, cx: &mut Context<Self>) {
        self.run_local_server(LocalServerAction::Check, cx);
    }

    /// Runs one action on the background executor. Only one runs at a time.
    pub(super) fn run_local_server(&mut self, action: LocalServerAction, cx: &mut Context<Self>) {
        let Some(row) = &mut self.local_server else {
            return;
        };
        if matches!(row.status, LocalServerStatus::Working(_)) {
            return;
        }
        let Ok(control) = &row.control else {
            return;
        };
        let control = Arc::clone(control);
        row.status = LocalServerStatus::Working(action);
        let result = cx.background_spawn(async move {
            match action {
                LocalServerAction::Check => control.refresh(),
                LocalServerAction::SetUp | LocalServerAction::Start => control.setup_and_start(),
            }
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let result = result.await;
            let _ = settings.update(cx, |settings, cx| {
                if let Some(row) = &mut settings.local_server {
                    row.status = match result {
                        Ok(connection) => LocalServerStatus::Connected(connection),
                        Err(error) => LocalServerStatus::Failed(error),
                    };
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn local_server_presentation(&self) -> Option<LocalServerPresentation> {
        self.local_server.as_ref().map(LocalServerRow::presentation)
    }

    /// The state badge, or progress while an action runs, and the step beside it.
    pub(super) fn render_local_server(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(presentation) = self.local_server_presentation() else {
            return div().into_any_element();
        };
        let state_selector = presentation.state_selector;
        let state = if presentation.working {
            ProgressRing::new(
                PROGRESS_SELECTOR,
                presentation.state,
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(PROGRESS_SELECTOR)
            .into_any_element()
        } else {
            div()
                .debug_selector(move || state_selector.to_owned())
                .child(badge(state_selector, presentation.state, appearance))
                .into_any_element()
        };
        let owner = cx.weak_entity();
        let action = presentation.action.map(|action| {
            action_button(
                action.selector(),
                action.label(),
                !presentation.working,
                move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| settings.run_local_server(action, cx));
                },
            )
        });
        let surfaces = ServerSurfaces::resolve(
            crate::ui::shared_workspaces::adopted(cx),
            crate::ui::development_pane::is_open(cx),
        );
        let development_pane = presentation.development_pane.then(|| {
            action_button(
                OPEN_DEVELOPMENT_PANE_SELECTOR,
                "Open Development Pane",
                surfaces.development_pane,
                |_, cx| crate::ui::development_pane::open_or_activate(cx),
            )
            .when_some(surfaces.development_pane_reason(), |button, reason| {
                button.accessibility_description(reason)
            })
        });
        // Presenting the server's Workspaces adds them to the Workspace Window's sidebar. The
        // Workspaces, Tabs, Panes, and Terminal Sessions the desktop owns are left untouched.
        let shared_workspaces = presentation.shared_workspaces.then(|| {
            action_button(
                SHARED_WORKSPACES_SELECTOR,
                "Use Shared Workspaces",
                surfaces.shared_workspaces,
                |_, cx| crate::ui::shared_workspaces::adopt(cx),
            )
            .when_some(surfaces.shared_workspaces_reason(), |button, reason| {
                button.accessibility_description(reason)
            })
        });
        div()
            .debug_selector(|| CONTROL_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(state)
            .children(action)
            .children(development_pane)
            .children(shared_workspaces)
            .into_any_element()
    }
}

/// A local server whose answers a test sets and whose operations it counts.
#[cfg(test)]
pub(super) mod testing {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{LocalServerControl, ServerConnection};
    use crate::local_server::ServerError;

    /// A server whose answers the test sets and whose operations it counts.
    pub(in crate::ui::settings_window) struct ScriptedServer {
        refresh: Mutex<Result<ServerConnection, ServerError>>,
        setup: Mutex<Result<ServerConnection, ServerError>>,
        refreshes: AtomicUsize,
        setups: AtomicUsize,
    }

    impl ScriptedServer {
        pub(in crate::ui::settings_window) fn new(
            refresh: Result<ServerConnection, ServerError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                refresh: Mutex::new(refresh),
                setup: Mutex::new(Ok(connected(2))),
                refreshes: AtomicUsize::new(0),
                setups: AtomicUsize::new(0),
            })
        }

        pub(in crate::ui::settings_window) fn answer_setup(
            &self,
            result: Result<ServerConnection, ServerError>,
        ) {
            *self.setup.lock().expect("setup answer") = result;
        }

        pub(in crate::ui::settings_window) fn refreshes(&self) -> usize {
            self.refreshes.load(Ordering::SeqCst)
        }

        pub(in crate::ui::settings_window) fn setups(&self) -> usize {
            self.setups.load(Ordering::SeqCst)
        }
    }

    impl LocalServerControl for ScriptedServer {
        fn refresh(&self) -> Result<ServerConnection, ServerError> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            *self.refresh.lock().expect("refresh answer")
        }

        fn setup_and_start(&self) -> Result<ServerConnection, ServerError> {
            self.setups.fetch_add(1, Ordering::SeqCst);
            *self.setup.lock().expect("setup answer")
        }
    }

    /// A server that runs the Development Pane.
    pub(in crate::ui::settings_window) const fn connected(protocol_major: u16) -> ServerConnection {
        ServerConnection {
            protocol_major,
            development_pane: true,
        }
    }

    /// A server from before terminal operations, which authenticates and offers status only.
    pub(in crate::ui::settings_window) const fn status_only(
        protocol_major: u16,
    ) -> ServerConnection {
        ServerConnection {
            protocol_major,
            development_pane: false,
        }
    }
}

#[cfg(test)]
#[path = "servers_tests.rs"]
mod tests;
