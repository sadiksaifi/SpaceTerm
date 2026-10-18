//! Normal launch of the account-local SpaceTerm Server.
//!
//! The Workspace Window opens at once and owns no local Workspace of its own. A worker prepares
//! or reuses the server for this account off the event thread, and the window's empty stage says
//! what is happening until the server's Workspaces arrive. Launch never reverses a deliberate
//! Stop and never asks for authorization by itself: Start Server and startup authorization
//! run only when the person chooses them here, and each then runs launch again.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::prelude::*;
use gpui::{App, Context, SharedString, Task, Window, div};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, ProgressRing, ProgressSize, ProgressState,
};

use super::WorkspaceManager;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::domain::ValidatedLocalDirectory;
use crate::local_server::{LocalServer, ServerError};
use crate::platform::local_filesystem::LocalFilesystemAuthority;
use crate::ui::pane_lifecycle::PaneConstruction;
use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};

pub(super) const LAUNCH_SELECTOR: &str = "server-launch";
pub(super) const LAUNCH_START_SELECTOR: &str = "server-launch-start";
pub(super) const LAUNCH_AUTHORIZE_SELECTOR: &str = "server-launch-authorize";
pub(super) const LAUNCH_RETRY_SELECTOR: &str = "server-launch-retry";
pub(super) const LAUNCH_SETTINGS_SELECTOR: &str = "server-launch-settings";

/// What one launch of the account's server found.
pub(crate) enum LaunchOutcome {
    /// The server for this account is running and admitted this client.
    Ready,
    /// The person stopped the server, and it stays stopped until they start it.
    Stopped,
    /// The host needs one administrator authorization before the service can run at boot.
    AuthorizationRequired,
    /// The window closed or SpaceTerm quit before launch finished.
    Cancelled,
}

/// The blocking steps of local launch. Each runs on a background thread.
pub(crate) trait LocalLaunch: Send + Sync {
    /// Prepares or reuses the account's service and connects. Never reverses Stop and never
    /// asks for authorization.
    fn run(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<LaunchOutcome, ServerError>;
    /// The person's explicit Start Server, which reverses their Stop.
    fn start(&self) -> Result<(), ServerError>;
    /// The person's explicit startup authorization. Cancelling the system prompt changes nothing.
    fn authorize(&self) -> Result<(), ServerError>;
}

/// The Workspaces a ready server presents, and the connection that retains this client's views.
pub(crate) struct SharedServerConnection {
    pub(crate) edits: SharedEditSource,
    pub(crate) sessions: SharedPaneSessions,
    /// Retains attachments and arrangements across relaunch.
    pub(crate) retained: Option<LocalServer>,
}

/// Connects the Workspace Window to a ready server on the event thread.
pub(crate) trait SharedServerConnector {
    fn connect(
        &self,
        client_home: ValidatedLocalDirectory,
        local_files: LocalFilesystemAuthority,
        panes: &PaneConstruction,
        cx: &mut App,
    ) -> Option<SharedServerConnection>;
}

/// The account-local server a Workspace Window launches and presents.
#[derive(Clone)]
pub(crate) struct LocalServerAdapters {
    pub(crate) launch: Arc<dyn LocalLaunch>,
    pub(crate) connector: Rc<dyn SharedServerConnector>,
}

impl LocalServerAdapters {
    /// This build's own account server. Construction has no effects.
    pub(crate) fn account() -> Self {
        Self {
            launch: Arc::new(AccountLaunch),
            connector: Rc::new(AccountConnector),
        }
    }
}

struct AccountLaunch;

impl AccountLaunch {
    fn service() -> Result<crate::local_server::service::Service, ServerError> {
        crate::local_server::service::Service::for_identity(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
        )
    }
}

impl LocalLaunch for AccountLaunch {
    fn run(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<LaunchOutcome, ServerError> {
        use spaceterm_server::StartupOutcome;
        Ok(match crate::local_startup::run(cancelled)? {
            StartupOutcome::Ready { .. } => LaunchOutcome::Ready,
            StartupOutcome::Stopped => LaunchOutcome::Stopped,
            StartupOutcome::AuthorizationRequired => LaunchOutcome::AuthorizationRequired,
            StartupOutcome::Cancelled => LaunchOutcome::Cancelled,
        })
    }

    fn start(&self) -> Result<(), ServerError> {
        Self::service()?.start().map(drop)
    }

    fn authorize(&self) -> Result<(), ServerError> {
        // The service asks the operating system for its one fixed authorization, for this
        // account only. No SpaceTerm code runs as root.
        Self::service()?
            .enable_linger(crate::local_server::service::LingerAuthorization::Desktop)
    }
}

struct AccountConnector;

impl SharedServerConnector for AccountConnector {
    fn connect(
        &self,
        client_home: ValidatedLocalDirectory,
        local_files: LocalFilesystemAuthority,
        panes: &PaneConstruction,
        cx: &mut App,
    ) -> Option<SharedServerConnection> {
        // Server Settings and the Workspace Window share one edit writer for this client.
        let client = crate::ui::server_worktrees::Client::get(cx)?;
        let server = client.server.clone();
        Some(SharedServerConnection {
            edits: client.source,
            sessions: SharedPaneSessions::new(server.clone(), client_home, local_files, panes),
            retained: Some(server),
        })
    }
}

/// An explicit step the person chose from the launch screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LaunchStep {
    Start,
    Authorize,
}

/// Where launch stands. Only `Ready` hands the stage to the server's Workspaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LaunchState {
    Launching,
    Working(LaunchStep),
    Stopped,
    /// `declined` is whether the person's last authorization did not complete.
    AuthorizationRequired {
        declined: bool,
    },
    Failed(ServerError),
    Ready,
}

/// An action the launch screen offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LaunchAction {
    Start,
    Authorize,
    Retry,
    Settings,
}

impl LaunchAction {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start Server",
            Self::Authorize => "Authorize\u{2026}",
            Self::Retry => "Try Again",
            Self::Settings => "Server Settings\u{2026}",
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Start => LAUNCH_START_SELECTOR,
            Self::Authorize => LAUNCH_AUTHORIZE_SELECTOR,
            Self::Retry => LAUNCH_RETRY_SELECTOR,
            Self::Settings => LAUNCH_SETTINGS_SELECTOR,
        }
    }
}

/// The launch screen's complete presentation, derived from the state alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LaunchPresentation {
    pub(super) title: &'static str,
    pub(super) detail: SharedString,
    /// The progress label while a step runs. Actions are withheld meanwhile.
    pub(super) progress: Option<&'static str>,
    /// The first action is the default one.
    pub(super) actions: &'static [LaunchAction],
}

pub(super) const OPENING_TITLE: &str = "Opening Workspaces\u{2026}";
const OPENING_DETAIL: &str = "SpaceTerm is starting the server for your account. Your Workspaces \
                              open as soon as it\u{2019}s ready.";
pub(super) const STOPPED_TITLE: &str = "Server Stopped";
const STOPPED_DETAIL: &str = "You stopped the server, so no Terminal Sessions are running. It \
                              stays stopped, even after you restart the computer, until you start \
                              it. Starting it opens your Workspaces.";
pub(super) const AUTHORIZATION_TITLE: &str = "Run the Server at Startup";
const AUTHORIZATION_DETAIL: &str = "The server keeps your Terminal Sessions running after you log \
                                    out. Your computer asks for an administrator password once to \
                                    let your account\u{2019}s services start at boot and keep \
                                    running after you log out. This applies to every service your \
                                    account runs.";
const AUTHORIZATION_DECLINED: &str = "Authorization did not complete, so nothing changed.";
const AUTHORIZING_DETAIL: &str = "Enter an administrator password in the system prompt to \
                                  continue. If you cancel, nothing changes.";

/// The title for a launch that could not finish. The detail names the exact reason.
const fn failure_title(error: ServerError) -> &'static str {
    match error {
        ServerError::IncompatibleState | ServerError::IncompatibleProtocol => {
            "Incompatible Server"
        }
        ServerError::RuntimeUnavailable => "Server Unavailable",
        ServerError::UnsafeStorage => "Unsafe Server Storage",
        ServerError::StorageUnavailable => "Server Storage Unavailable",
        ServerError::NotInstalled => "Server Not Installed",
        ServerError::AuthenticationRefused => "Connection Refused",
        _ => "Couldn\u{2019}t Open Workspaces",
    }
}

pub(super) fn present(state: LaunchState) -> LaunchPresentation {
    use LaunchAction::{Authorize, Retry, Settings, Start};
    let fixed = SharedString::new_static;
    match state {
        LaunchState::Launching | LaunchState::Ready => LaunchPresentation {
            title: OPENING_TITLE,
            detail: fixed(OPENING_DETAIL),
            progress: Some("Starting\u{2026}"),
            actions: &[],
        },
        LaunchState::Working(LaunchStep::Start) => LaunchPresentation {
            title: STOPPED_TITLE,
            detail: fixed(STOPPED_DETAIL),
            progress: Some("Starting the server\u{2026}"),
            actions: &[],
        },
        LaunchState::Working(LaunchStep::Authorize) => LaunchPresentation {
            title: AUTHORIZATION_TITLE,
            detail: fixed(AUTHORIZING_DETAIL),
            progress: Some("Waiting for authorization\u{2026}"),
            actions: &[],
        },
        LaunchState::Stopped => LaunchPresentation {
            title: STOPPED_TITLE,
            detail: fixed(STOPPED_DETAIL),
            progress: None,
            actions: &[Start, Settings],
        },
        LaunchState::AuthorizationRequired { declined } => LaunchPresentation {
            title: AUTHORIZATION_TITLE,
            detail: if declined {
                format!("{AUTHORIZATION_DECLINED} {AUTHORIZATION_DETAIL}").into()
            } else {
                fixed(AUTHORIZATION_DETAIL)
            },
            progress: None,
            actions: &[Authorize, Settings],
        },
        LaunchState::Failed(error) => LaunchPresentation {
            title: failure_title(error),
            detail: error.to_string().into(),
            progress: None,
            actions: &[Retry, Settings],
        },
    }
}

/// One Workspace Window's launch: its adapters, where it stands, and the running step.
pub(super) struct LocalLaunchState {
    adapters: LocalServerAdapters,
    pub(super) state: LaunchState,
    cancelled: Arc<AtomicBool>,
    _task: Option<Task<()>>,
    /// The message assistive technology last announced, so each change is spoken once.
    announced: std::cell::RefCell<Option<SharedString>>,
}

impl LocalLaunchState {
    pub(super) fn new(adapters: LocalServerAdapters) -> Self {
        Self {
            adapters,
            state: LaunchState::Launching,
            cancelled: Arc::new(AtomicBool::new(false)),
            _task: None,
            announced: std::cell::RefCell::default(),
        }
    }

    /// The live status node for `message`, polite only when it changed since the last frame.
    fn announcement(&self, message: &SharedString) -> gpui::accesskit::Node {
        let mut announced = self.announced.borrow_mut();
        let changed = announced.as_ref() != Some(message);
        *announced = Some(message.clone());
        let mut node = gpui::accesskit::Node::new(gpui::accesskit::Role::Status);
        node.set_label(message.as_ref());
        node.set_value(message.as_ref());
        node.set_live(if changed {
            gpui::accesskit::Live::Polite
        } else {
            gpui::accesskit::Live::Off
        });
        node
    }
}

impl Drop for LocalLaunchState {
    /// Closing the window abandons launch. A step already admitted reports nothing here.
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// What one background run reports back to the window.
enum StepResult {
    Outcome(LaunchOutcome),
    Failed(ServerError),
    /// The person's authorization did not complete.
    Declined,
}

impl WorkspaceManager {
    /// Runs launch, after the person's explicit step when there is one.
    pub(super) fn run_local_launch(
        &mut self,
        step: Option<LaunchStep>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(launch) = self.launch.as_mut() else {
            return;
        };
        launch.state = step.map_or(LaunchState::Launching, LaunchState::Working);
        let adapter = Arc::clone(&launch.adapters.launch);
        let cancelled = Arc::clone(&launch.cancelled);
        let work = cx.background_spawn(async move {
            let refused = match step {
                Some(LaunchStep::Start) => adapter.start().err().map(StepResult::Failed),
                Some(LaunchStep::Authorize) => adapter.authorize().err().map(|_| StepResult::Declined),
                None => None,
            };
            if let Some(refused) = refused {
                return refused;
            }
            let observed = move || cancelled.load(Ordering::Acquire);
            match adapter.run(&observed) {
                Ok(outcome) => StepResult::Outcome(outcome),
                Err(error) => StepResult::Failed(error),
            }
        });
        launch._task = Some(cx.spawn_in(window, async move |manager, cx| {
            let result = work.await;
            let _ = manager.update_in(cx, |manager, window, cx| {
                manager.finish_local_launch(result, window, cx);
            });
        }));
        cx.notify();
    }

    fn finish_local_launch(
        &mut self,
        result: StepResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = match result {
            StepResult::Outcome(LaunchOutcome::Ready) => self.connect_local_server(window, cx),
            StepResult::Outcome(LaunchOutcome::Stopped) => LaunchState::Stopped,
            StepResult::Outcome(LaunchOutcome::AuthorizationRequired)
            | StepResult::Failed(ServerError::AuthorizationRequired) => {
                LaunchState::AuthorizationRequired { declined: false }
            }
            StepResult::Declined => LaunchState::AuthorizationRequired { declined: true },
            StepResult::Failed(ServerError::Stopped) => LaunchState::Stopped,
            StepResult::Failed(error) => LaunchState::Failed(error),
            // Only a closing window cancels, so there is nothing left to present.
            StepResult::Outcome(LaunchOutcome::Cancelled) => return,
        };
        if let Some(launch) = self.launch.as_mut() {
            launch.state = state;
            launch._task = None;
        }
        // The screen's own focus moves to its default action when one appears.
        if self.workspaces.active_workspace_id().is_none() {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// Presents a ready server's Workspaces in this window.
    fn connect_local_server(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LaunchState {
        if self.shared.is_some() {
            return LaunchState::Ready;
        }
        // The client home only completes the launch plan a server-backed Terminal Session
        // ignores. It is never the directory the server starts a shell in.
        let client_home = match self.local_home_directory() {
            Ok(client_home) => client_home,
            Err(_) => return LaunchState::Failed(ServerError::StorageUnavailable),
        };
        let Some(connector) = self
            .launch
            .as_ref()
            .map(|launch| Rc::clone(&launch.adapters.connector))
        else {
            return LaunchState::Ready;
        };
        let Some(connection) = connector.connect(
            client_home,
            self.local_filesystem.clone(),
            &self.pane_construction,
            cx,
        ) else {
            return LaunchState::Failed(ServerError::RuntimeUnavailable);
        };
        self.present_shared_workspaces(connection.edits, connection.sessions, window, cx);
        if let (Some(shared), Some(server)) = (self.shared.as_mut(), connection.retained) {
            shared.retain_client_views(server);
        }
        LaunchState::Ready
    }

    /// The launch screen this window presents in place of the empty view, if any.
    ///
    /// It stays until the server's first inventory arrives, so a person never sees "No
    /// Workspaces" while their Workspaces are on the way.
    pub(super) fn launch_presentation(&self) -> Option<LaunchPresentation> {
        let state = self.launch.as_ref()?.state;
        if state == LaunchState::Ready
            && self
                .shared
                .as_ref()
                .is_none_or(|shared| shared.latest_inventory().is_some())
        {
            return None;
        }
        Some(present(state))
    }

    /// The launch screen's message and actions, centered in the empty stage. Return runs the
    /// default action, as it does for a dialog's default button.
    pub(super) fn render_launch_message(
        &self,
        presentation: LaunchPresentation,
        focus: &gpui::FocusHandle,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let appearance = crate::ui::appearance::chrome(cx);
        let LaunchPresentation {
            title,
            detail,
            progress,
            actions,
        } = presentation;
        let message: SharedString = match progress {
            Some(progress) => format!("{title} {progress}").into(),
            None => format!("{title}. {detail}").into(),
        };
        let announcement = self
            .launch
            .as_ref()
            .map(|launch| launch.announcement(&message));
        let buttons = actions.iter().enumerate().map(|(index, &action)| {
            let owner = cx.weak_entity();
            Button::new(action.selector(), action.label())
                .variant(if index == 0 {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Secondary
                })
                .size(ButtonSize::Regular)
                .tab_stop(true)
                .debug_selector(action.selector())
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |manager, cx| {
                        manager.run_launch_action(action, window, cx);
                    });
                })
        });
        let default = actions.first().copied();
        let owner = cx.weak_entity();
        div()
            .id(LAUNCH_SELECTOR)
            .debug_selector(|| LAUNCH_SELECTOR.to_owned())
            .role(gpui::accesskit::Role::Group)
            .aria_label(title)
            .aria_description(detail.clone())
            .size_full()
            .track_focus(focus)
            .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
                let modifiers = event.keystroke.modifiers;
                if event.keystroke.key != "enter" || modifiers.modified() {
                    return;
                }
                if let Some(action) = default {
                    let _ = owner.update(cx, |manager, cx| {
                        manager.run_launch_action(action, window, cx);
                    });
                    cx.stop_propagation();
                }
            })
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(appearance.spacing(12.0))
            .px(appearance.spacing(24.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(appearance.spacing(4.0))
                    .max_w(appearance.spacing(400.0))
                    .text_center()
                    .child(
                        div()
                            .debug_selector(|| format!("{LAUNCH_SELECTOR}-title"))
                            .chrome_text(appearance.typography.style(TextRole::Section))
                            .text_color(gpui_color(appearance.colors.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .id("server-launch-detail")
                            .debug_selector(|| format!("{LAUNCH_SELECTOR}-detail"))
                            .role(gpui::accesskit::Role::Group)
                            .when_some(announcement, |detail, node| {
                                detail.a11y_synthetic_children(move |builder| {
                                    let mut node = node.clone();
                                    if let Some(bounds) = builder.parent_node().bounds() {
                                        node.set_bounds(bounds);
                                    }
                                    builder.push_child(builder.synthetic_node_id("status"), node);
                                })
                            })
                            .whitespace_normal()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(appearance.colors.text_secondary))
                            .child(detail),
                    ),
            )
            .when_some(progress, |stage, progress| {
                stage.child(
                    div()
                        .debug_selector(|| format!("{LAUNCH_SELECTOR}-progress"))
                        .flex()
                        .items_center()
                        .gap(appearance.spacing(6.0))
                        .child(
                            ProgressRing::new(
                                "server-launch-progress",
                                progress,
                                ProgressState::Indeterminate,
                            )
                            .size(ProgressSize::Compact),
                        )
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(appearance.colors.text_secondary))
                                .child(progress),
                        ),
                )
            })
            .when(!actions.is_empty(), |stage| {
                stage.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap(appearance.spacing(8.0))
                        .children(buttons),
                )
            })
    }

    pub(super) fn run_launch_action(
        &mut self,
        action: LaunchAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ready = self
            .launch
            .as_ref()
            .is_some_and(|launch| launch._task.is_none());
        match action {
            LaunchAction::Settings => crate::ui::settings_window::open_or_activate(
                Some(crate::ui::settings_window::SettingsSectionId::Servers),
                cx,
            ),
            LaunchAction::Start if ready => {
                self.run_local_launch(Some(LaunchStep::Start), window, cx);
            }
            LaunchAction::Authorize if ready => {
                self.run_local_launch(Some(LaunchStep::Authorize), window, cx);
            }
            LaunchAction::Retry if ready => self.run_local_launch(None, window, cx),
            LaunchAction::Start | LaunchAction::Authorize | LaunchAction::Retry => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::{LaunchOutcome, LocalLaunch};
    use crate::local_server::ServerError;

    /// A launch whose answers a test queues and whose explicit steps it records.
    #[derive(Default)]
    pub(crate) struct ScriptedLaunch {
        runs: Mutex<VecDeque<Result<LaunchOutcome, ServerError>>>,
        steps: Mutex<Vec<&'static str>>,
        start: Mutex<Option<ServerError>>,
        authorize: Mutex<Option<ServerError>>,
    }

    impl ScriptedLaunch {
        pub(crate) fn answer(&self, result: Result<LaunchOutcome, ServerError>) {
            self.runs.lock().unwrap().push_back(result);
        }
        pub(crate) fn refuse_start(&self, error: ServerError) {
            *self.start.lock().unwrap() = Some(error);
        }
        pub(crate) fn refuse_authorization(&self) {
            *self.authorize.lock().unwrap() = Some(ServerError::AuthorizationRequired);
        }
        /// Every step in order: `run`, `start`, and `authorize`.
        pub(crate) fn steps(&self) -> Vec<&'static str> {
            self.steps.lock().unwrap().clone()
        }
    }

    impl LocalLaunch for ScriptedLaunch {
        fn run(&self, _: &(dyn Fn() -> bool + Sync)) -> Result<LaunchOutcome, ServerError> {
            self.steps.lock().unwrap().push("run");
            self.runs
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(LaunchOutcome::Cancelled))
        }
        fn start(&self) -> Result<(), ServerError> {
            self.steps.lock().unwrap().push("start");
            self.start.lock().unwrap().take().map_or(Ok(()), Err)
        }
        fn authorize(&self) -> Result<(), ServerError> {
            self.steps.lock().unwrap().push("authorize");
            self.authorize.lock().unwrap().take().map_or(Ok(()), Err)
        }
    }
}
