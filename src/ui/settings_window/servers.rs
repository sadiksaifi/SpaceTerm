//! The Servers section: the Development server for this account, this device's pairing with it, and
//! its authenticated status. [`crate::local_server`] owns setup, pairing, and authentication.
//!
//! Only SpaceTerm Development offers the section while the server is unshipped, so no other build
//! can set up or start one. Opening Settings asks the server nothing. Presenting the section checks
//! the saved pairing, and only the person's explicit Set Up or Start sets up or starts a server.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Task, div};
use spaceterm_ui::{ProgressRing, ProgressSize, ProgressState};

use super::SettingsWindow;
use crate::application_identity::ApplicationIdentity;
use crate::local_server::{LocalServer, ServerError};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::sidebar_window::form::{CaptionTone, action_button, badge};

pub(super) const CONTROL_SELECTOR: &str = "settings-local-server-control";
pub(super) const PROGRESS_SELECTOR: &str = "settings-local-server-progress";
pub(super) const CHECK_SELECTOR: &str = "settings-local-server-check";
pub(super) const SET_UP_SELECTOR: &str = "settings-local-server-set-up";
pub(super) const START_SELECTOR: &str = "settings-local-server-start";

/// The directory name of the only identity that offers the local server.
const DEVELOPMENT_DIRECTORY_NAME: &str = "spaceterm-development";

/// The blocking local server operations the section runs on the background executor.
pub(super) trait LocalServerControl: Send + Sync {
    /// Authenticates with the saved pairing. Never creates an identity or starts a server.
    fn refresh(&self) -> Result<ServerConnection, ServerError>;
    /// Sets up and pairs when needed, starts the server when it is not running, and authenticates.
    fn setup_and_start(&self) -> Result<ServerConnection, ServerError>;
}

impl LocalServerControl for LocalServer {
    fn refresh(&self) -> Result<ServerConnection, ServerError> {
        LocalServer::refresh(self).map(|status| ServerConnection {
            protocol_major: status.protocol_major,
        })
    }

    fn setup_and_start(&self) -> Result<ServerConnection, ServerError> {
        LocalServer::setup_and_start(self).map(|status| ServerConnection {
            protocol_major: status.protocol_major,
        })
    }
}

/// What the section presents about an authenticated connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ServerConnection {
    pub(super) protocol_major: u16,
}

/// The local server a build offers. `None` hides the Servers section.
pub(super) type OfferedLocalServer = Option<Result<Arc<dyn LocalServerControl>, ServerError>>;

/// Whether `identity` offers the local server. Only Development does while it is unshipped.
pub(super) fn offers_local_server(identity: ApplicationIdentity) -> bool {
    identity.directory_name() == DEVELOPMENT_DIRECTORY_NAME
}

/// Resolves the running build's local server. Resolution reads paths only: it sets up, starts,
/// and reads credentials for nothing.
pub(super) fn offered_local_server() -> OfferedLocalServer {
    let identity = ApplicationIdentity::current();
    offers_local_server(identity).then(|| {
        LocalServer::for_identity(identity.directory_name())
            .map(|server| Arc::new(server) as Arc<dyn LocalServerControl>)
    })
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
        div()
            .debug_selector(|| CONTROL_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(state)
            .children(action)
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

    pub(in crate::ui::settings_window) const fn connected(protocol_major: u16) -> ServerConnection {
        ServerConnection { protocol_major }
    }
}

#[cfg(test)]
#[path = "servers_tests.rs"]
mod tests;
