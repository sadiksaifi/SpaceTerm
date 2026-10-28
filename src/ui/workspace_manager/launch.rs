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
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, ProgressRing, ProgressSize, ProgressState};

use super::WorkspaceManager;
use crate::domain::ValidatedLocalDirectory;
use crate::platform::local_filesystem::LocalFilesystemAuthority;
use crate::server_connection::{ServerConnection, ServerError};
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::pane_lifecycle::PaneConstruction;
use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};

pub(super) const LAUNCH_SELECTOR: &str = "server-launch";
pub(super) const LAUNCH_START_SELECTOR: &str = "server-launch-start";
pub(super) const LAUNCH_AUTHORIZE_SELECTOR: &str = "server-launch-authorize";
pub(super) const LAUNCH_RETRY_SELECTOR: &str = "server-launch-retry";
pub(super) const LAUNCH_SETTINGS_SELECTOR: &str = "server-launch-settings";
pub(super) const LAUNCH_UPDATE_SELECTOR: &str = "server-launch-update";
const LAUNCH_QUIET_SELECTOR: &str = "server-launch-quiet";

/// How long a window's first launch may wait for the server before it says so. Opening an
/// already running server finishes well inside this, so a person sees the window and then their
/// Workspaces, never a message that vanishes before it can be read.
pub(super) const LAUNCH_QUIET_PERIOD: std::time::Duration = std::time::Duration::from_secs(1);

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
    /// Whether this SpaceTerm includes a verified server build other than the approved one.
    /// Reads only: it stages, installs, and starts nothing.
    fn update_offered(&self) -> bool;
}

/// The Workspaces a ready server presents, and the connection that retains this client's views.
pub(crate) struct SharedServerConnection {
    pub(crate) edits: SharedEditSource,
    pub(crate) sessions: SharedPaneSessions,
    /// Retains attachments and arrangements across relaunch.
    pub(crate) retained: Option<ServerConnection>,
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

    /// The account-local connection whose client directory retains this desktop's Workspace
    /// order, or `None` to keep the order for the window's life. Opening it needs no running
    /// server.
    fn workspace_order(&self) -> Option<ServerConnection> {
        None
    }
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
    fn service() -> Result<crate::server_connection::service::Service, ServerError> {
        crate::server_connection::service::Service::for_identity(
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
            .enable_linger(crate::server_connection::service::LingerAuthorization::Desktop)
    }

    fn update_offered(&self) -> bool {
        // The same offer the Server version row makes, so both surfaces agree.
        Self::service().is_ok_and(|service| crate::ui::settings_window::update_offered(&service))
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
            sessions: SharedPaneSessions::new(
                server.clone(),
                client.controls,
                client_home,
                local_files,
                panes,
            ),
            retained: Some(server),
        })
    }

    fn workspace_order(&self) -> Option<ServerConnection> {
        ServerConnection::for_identity(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
        )
        .ok()
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
    /// This client refused the server, which keeps running with its Terminal Sessions.
    /// `update` is whether this SpaceTerm includes a verified server build to update it to.
    Incompatible {
        reason: ServerError,
        update: bool,
    },
    Ready,
}

/// An action the launch screen offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LaunchAction {
    Start,
    Authorize,
    Retry,
    Settings,
    /// Opens Settings on Servers and starts the Update Server journey there.
    Update,
}

impl LaunchAction {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start Server",
            Self::Authorize => "Authorize\u{2026}",
            Self::Retry => "Try Again",
            Self::Settings => "Server Settings\u{2026}",
            Self::Update => "Update Server\u{2026}",
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Start => LAUNCH_START_SELECTOR,
            Self::Authorize => LAUNCH_AUTHORIZE_SELECTOR,
            Self::Retry => LAUNCH_RETRY_SELECTOR,
            Self::Settings => LAUNCH_SETTINGS_SELECTOR,
            Self::Update => LAUNCH_UPDATE_SELECTOR,
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
    /// The window is only waiting for the server: nothing failed and nothing asks the person to
    /// act, so a short wait need not be presented at all.
    pub(super) waiting: bool,
}

pub(super) const OPENING_TITLE: &str = "Opening Workspaces\u{2026}";
// Launch reuses a running server or starts it, so the detail claims neither.
const OPENING_DETAIL: &str = "Waiting for the server…";
pub(super) const STOPPED_TITLE: &str = "Server Stopped";
const STOPPED_DETAIL: &str = "Start the server to open your Workspaces.";
pub(super) const AUTHORIZATION_TITLE: &str = "Run the Server at Startup";
const AUTHORIZATION_DETAIL: &str =
    "Enter an administrator password so your Terminal Sessions keep running after you log out.";
const AUTHORIZATION_DECLINED: &str = "Authorization didn\u{2019}t finish.";
const AUTHORIZING_DETAIL: &str =
    "Enter an administrator password in the system prompt to continue.";

/// The title for a launch that could not finish. The detail names the exact reason.
const fn failure_title(error: ServerError) -> &'static str {
    match error {
        ServerError::IncompatibleState
        | ServerError::IncompatibleProtocol
        | ServerError::MissingRequiredCapability => "Incompatible Server",
        ServerError::ActivationUnresolved | ServerError::ReturnUnavailable => {
            "Server Update Unfinished"
        }
        ServerError::CandidateChanged => "Restart SpaceTerm",
        ServerError::CandidateMissing => "Server Not Included",
        ServerError::RuntimeUnavailable => "Couldn\u{2019}t Verify Server",
        ServerError::UnsafeStorage => "Unsafe Server Storage",
        ServerError::StorageUnavailable => "Server Storage Unavailable",
        ServerError::NotInstalled => "Server Not Installed",
        ServerError::AuthenticationRefused => "Connection Refused",
        _ => "Couldn\u{2019}t Open Workspaces",
    }
}

/// The detail for a launch that could not finish: the exact reason, and for a server package,
/// whether this build lacks one or holds one that failed verification.
fn failure_detail(error: ServerError) -> SharedString {
    match error {
        ServerError::CandidateMissing => "This SpaceTerm build has no server to set up, so \
                                          reinstall SpaceTerm and try again."
            .into(),
        ServerError::RuntimeUnavailable => "Reinstall SpaceTerm, then try again.".into(),
        error => error.to_string().into(),
    }
}

/// Whether a connection failure before the first inventory needs the person, because
/// reconnecting alone cannot open the Workspaces.
const fn ends_opening(error: ServerError) -> bool {
    matches!(
        error,
        ServerError::IncompatibleProtocol
            | ServerError::MissingRequiredCapability
            | ServerError::ActivationUnresolved
            | ServerError::IncompatibleState
            | ServerError::StorageUnavailable
            | ServerError::UnsafeStorage
            | ServerError::AuthenticationRefused
    )
}

/// Whether this client refused the server itself, which keeps running for its other clients.
const fn incompatible(error: ServerError) -> bool {
    matches!(
        error,
        ServerError::IncompatibleProtocol | ServerError::MissingRequiredCapability
    )
}

/// What launch presents for a failure. A refused server offers the update journey only when a
/// read found a verified build to update it to.
const fn failed(error: ServerError, update: bool) -> LaunchState {
    if incompatible(error) {
        LaunchState::Incompatible {
            reason: error,
            update,
        }
    } else {
        LaunchState::Failed(error)
    }
}

pub(super) fn present(state: LaunchState) -> LaunchPresentation {
    use LaunchAction::{Authorize, Retry, Settings, Start, Update};
    let fixed = SharedString::new_static;
    match state {
        LaunchState::Launching | LaunchState::Ready => LaunchPresentation {
            title: OPENING_TITLE,
            detail: fixed(OPENING_DETAIL),
            progress: Some("Waiting for the server\u{2026}"),
            actions: &[],
            waiting: true,
        },
        LaunchState::Working(LaunchStep::Start) => LaunchPresentation {
            title: STOPPED_TITLE,
            detail: fixed(STOPPED_DETAIL),
            progress: Some("Starting the server\u{2026}"),
            actions: &[],
            waiting: false,
        },
        LaunchState::Working(LaunchStep::Authorize) => LaunchPresentation {
            title: AUTHORIZATION_TITLE,
            detail: fixed(AUTHORIZING_DETAIL),
            progress: Some("Waiting for authorization\u{2026}"),
            actions: &[],
            waiting: false,
        },
        LaunchState::Stopped => LaunchPresentation {
            title: STOPPED_TITLE,
            detail: fixed(STOPPED_DETAIL),
            progress: None,
            actions: &[Start, Settings],
            waiting: false,
        },
        LaunchState::AuthorizationRequired { declined } => LaunchPresentation {
            title: if declined {
                "Startup Authorization"
            } else {
                AUTHORIZATION_TITLE
            },
            detail: if declined {
                fixed(AUTHORIZATION_DECLINED)
            } else {
                fixed(AUTHORIZATION_DETAIL)
            },
            progress: None,
            actions: &[Authorize, Settings],
            waiting: false,
        },
        LaunchState::Incompatible { reason, update } => {
            let detail = match (update, reason) {
                (true, _) => "Updating the server ends its Terminal Sessions for every client.",
                (false, ServerError::MissingRequiredCapability) => "This server needs an update.",
                (false, _) => "SpaceTerm and this server are incompatible.",
            };
            LaunchPresentation {
                title: failure_title(reason),
                detail: fixed(detail),
                progress: None,
                actions: if update {
                    &[Update, Retry]
                } else {
                    &[Retry, Settings]
                },
                waiting: false,
            }
        }
        LaunchState::Failed(error) => LaunchPresentation {
            title: failure_title(error),
            detail: failure_detail(error),
            progress: None,
            actions: &[Retry, Settings],
            waiting: false,
        },
    }
}

/// One Workspace Window's launch: its adapters, where it stands, and the running step.
pub(super) struct LocalLaunchState {
    adapters: LocalServerAdapters,
    pub(super) state: LaunchState,
    cancelled: Arc<AtomicBool>,
    _task: Option<Task<()>>,
    /// The server was started elsewhere while this launch was answering. A launch that then
    /// finishes waiting runs once more, because its answer may predate that start.
    resume: bool,
    /// The message assistive technology last announced, so each change is spoken once.
    announced: std::cell::RefCell<Option<SharedString>>,
    /// When the window's first launch may start saying it is waiting, once that launch began.
    quiet_until: Option<std::time::Instant>,
    /// The launch screen has said something, so a later wait keeps saying what it is doing
    /// rather than going quiet, even within the first launch's quiet period.
    spoke: std::cell::Cell<bool>,
    _quiet: Option<Task<()>>,
    /// Presents the server's Workspaces as soon as this client's saved arrangement is read,
    /// rather than at the next periodic reading.
    _views_read: Option<Task<()>>,
    /// The open connection's refusal and whether a verified build can update its server. Read
    /// once per refusal, off the event thread; `false` until the read answers.
    refusal_offer: Option<(ServerError, bool)>,
    _refusal_offer: Option<Task<()>>,
    /// New Workspace was chosen with no Workspace Window open. The window opened for it creates
    /// the Workspace once this computer's server presents its Workspaces.
    create_when_presented: bool,
}

impl LocalLaunchState {
    pub(super) fn new(adapters: LocalServerAdapters) -> Self {
        Self {
            adapters,
            state: LaunchState::Launching,
            cancelled: Arc::new(AtomicBool::new(false)),
            _task: None,
            resume: false,
            announced: std::cell::RefCell::default(),
            quiet_until: None,
            spoke: std::cell::Cell::new(false),
            _quiet: None,
            _views_read: None,
            refusal_offer: None,
            _refusal_offer: None,
            create_when_presented: false,
        }
    }

    /// The account-local connection that retains this desktop's Workspace order, if any.
    pub(super) fn workspace_order_connection(&self) -> Option<ServerConnection> {
        self.adapters.connector.workspace_order()
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
    /// The client refused the server. `update` is whether a verified build can update it.
    Incompatible(ServerError, bool),
    /// The person's authorization did not complete.
    Declined,
}

/// The person's explicit Start Server from a Pane whose server they stopped. The window runs its
/// Start step, then launch, and its Panes reconnect once the server answers.
pub(crate) fn start_local_server(window: &mut Window, cx: &mut App) {
    window.defer(cx, |window, cx| {
        if let Some(Some(manager)) = window.root::<WorkspaceManager>() {
            manager.update(cx, |manager, cx| {
                manager.run_launch_action(LaunchAction::Start, window, cx);
            });
        }
    });
}

/// Runs launch again in every Workspace Window still waiting on the server, after the person
/// started or installed it from Settings. A window already presenting its Workspaces reconnects
/// the Panes the stop interrupted instead. A window still launching runs launch once more if it
/// then finishes waiting. Nothing here starts the server itself, so a retained Stop stays.
pub(crate) fn resume_waiting_launches(cx: &mut App) {
    for window in cx.windows() {
        let Some(window) = window.downcast::<WorkspaceManager>() else {
            continue;
        };
        let _ = window.update(cx, |manager, window, cx| {
            if matches!(
                manager.launch.state,
                LaunchState::Stopped
                    | LaunchState::AuthorizationRequired { .. }
                    | LaunchState::Failed(_)
                    | LaunchState::Incompatible { .. }
            ) && manager.launch._task.is_none()
            {
                manager.run_local_launch(None, window, cx);
            } else if manager.launch._task.is_some() {
                manager.launch.resume = true;
            } else if manager.launch.state == LaunchState::Ready {
                manager.server_started(cx);
            }
        });
    }
}

impl WorkspaceManager {
    /// The application's New Workspace, chosen while no Workspace Window was open. This window
    /// opened for it, so it creates a Workspace as soon as this computer's server presents its
    /// Workspaces, even when this client has none attached.
    pub(crate) fn create_workspace_when_presented(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.launch.create_when_presented = true;
        self.create_pending_workspace(super::ServerKey::LOCAL, window, cx);
    }

    /// Creates the Workspace New Workspace asked for before this computer's server presented
    /// its Workspaces, once it has.
    pub(super) fn create_pending_workspace(
        &mut self,
        key: super::ServerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key != super::ServerKey::LOCAL
            || !self.launch.create_when_presented
            || self.launch_presentation().is_some()
        {
            return;
        }
        self.launch.create_when_presented = false;
        self.run_workspace_creation(
            crate::ui::workspace_creation::WorkspaceCreation::Local,
            window,
            cx,
        );
    }

    /// Runs launch, after the person's explicit step when there is one.
    pub(super) fn run_local_launch(
        &mut self,
        step: Option<LaunchStep>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let launch = &mut self.launch;
        launch.state = step.map_or(LaunchState::Launching, LaunchState::Working);
        // Only the window's first launch begins quietly. A later one follows a screen the person
        // already sees, so it keeps saying what it is doing.
        if launch.quiet_until.is_none() {
            launch.quiet_until = Some(cx.background_executor().now() + LAUNCH_QUIET_PERIOD);
            launch._quiet = Some(cx.spawn_in(window, async move |manager, cx| {
                cx.background_executor().timer(LAUNCH_QUIET_PERIOD).await;
                let _ = manager.update(cx, |_, cx| cx.notify());
            }));
        }
        let adapter = Arc::clone(&launch.adapters.launch);
        let cancelled = Arc::clone(&launch.cancelled);
        let work = cx.background_spawn(async move {
            let refused = match step {
                Some(LaunchStep::Start) => adapter.start().err().map(StepResult::Failed),
                // Only a cancelled or refused prompt leaves startup unchanged. Any other failure
                // says what went wrong and claims nothing about what the system changed.
                Some(LaunchStep::Authorize) => adapter.authorize().err().map(|error| match error {
                    ServerError::AuthorizationRequired => StepResult::Declined,
                    error => StepResult::Failed(error),
                }),
                None => None,
            };
            if let Some(refused) = refused {
                return refused;
            }
            let observed = move || cancelled.load(Ordering::Acquire);
            match adapter.run(&observed) {
                Ok(outcome) => StepResult::Outcome(outcome),
                Err(error) if incompatible(error) => {
                    StepResult::Incompatible(error, adapter.update_offered())
                }
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
            StepResult::Incompatible(error, update) => failed(error, update),
            // Only a closing window cancels, so there is nothing left to present.
            StepResult::Outcome(LaunchOutcome::Cancelled) => return,
        };
        self.launch.state = state;
        self.launch._task = None;
        if std::mem::take(&mut self.launch.resume)
            && matches!(
                state,
                LaunchState::Stopped
                    | LaunchState::AuthorizationRequired { .. }
                    | LaunchState::Failed(_)
                    | LaunchState::Incompatible { .. }
            )
        {
            self.run_local_launch(None, window, cx);
            return;
        }
        // A running server answers the Panes a stop interrupted, so they connect again.
        if state == LaunchState::Ready {
            self.server_started(cx);
        }
        // While Workspaces are presented the launch screen is hidden, so a step that could not
        // finish explains itself in an alert instead.
        if self.launch_presentation().is_none()
            && let Some(presentation) = (state != LaunchState::Ready).then(|| present(state))
        {
            Self::present_launch_alert(presentation, window, cx);
        }
        // The screen's own focus moves to its default action when one appears.
        if self.workspaces.active_workspace_id().is_none() {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// Reconnects each Pane the person's stop interrupted, once this computer's server runs
    /// again. Another computer's Panes follow their own server.
    fn server_started(&self, cx: &mut App) {
        for workspace in self.workspaces.iter() {
            if self
                .shared
                .owner(workspace.id())
                .is_some_and(|key| key != super::ServerKey::LOCAL)
            {
                continue;
            }
            workspace
                .payload()
                .update(cx, |manager, cx| manager.server_started(cx));
        }
    }

    /// Presents a ready server's Workspaces in this window.
    fn connect_local_server(&mut self, window: &mut Window, cx: &mut Context<Self>) -> LaunchState {
        if self.shared.local().is_some() {
            return LaunchState::Ready;
        }
        // One device presents its servers in one Workspace Window. Another window comes forward
        // instead; this one may already present a paired server and adds the local one beside it.
        if let Some(existing) = crate::ui::shared_workspaces::presenting_window(cx)
            && existing.window_id() != window.window_handle().window_id()
        {
            cx.defer(move |cx| {
                let _ = existing.update(cx, |_, window, _| window.activate_window());
            });
            return LaunchState::Ready;
        }
        // The client home only completes the launch plan a server-backed Terminal Session
        // ignores. It is never the directory the server starts a shell in.
        let client_home = match self.local_home_directory() {
            Ok(client_home) => client_home,
            Err(_) => return LaunchState::Failed(ServerError::StorageUnavailable),
        };
        let connector = Rc::clone(&self.launch.adapters.connector);
        let Some(connection) = connector.connect(
            client_home,
            self.local_filesystem.clone(),
            &self.pane_construction,
            cx,
        ) else {
            return LaunchState::Failed(ServerError::Protocol);
        };
        self.present_shared_workspaces(connection.edits, connection.sessions, window, cx);
        if let (Some(shared), Some(server)) = (self.shared.local_mut(), connection.retained) {
            shared.identify(Rc::new(server.clone()));
            shared.retain_client_views(server);
            let read = shared.client_views_read();
            self.launch._views_read = Some(cx.spawn_in(window, async move |manager, cx| {
                read.await;
                let _ = manager.update_in(cx, |manager, window, cx| {
                    manager.sync_shared_workspaces(super::ServerKey::LOCAL, window, cx);
                });
            }));
        }
        LaunchState::Ready
    }

    fn present_launch_alert(
        presentation: LaunchPresentation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use spaceterm_ui::{Alert, AlertIntent, ModalAction, ModalActionRole, ModalId};
        let _ = Alert::new(
            ModalId::new("server-launch-alert"),
            presentation.title,
            presentation.title,
            presentation.detail.to_string(),
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "server-launch-alert-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }

    /// The launch screen this window presents in place of the empty view, if any.
    ///
    /// It stays until the server's first inventory arrives, so a person never sees "No
    /// Workspaces" while their Workspaces are on the way.
    pub(super) fn launch_presentation(&self) -> Option<LaunchPresentation> {
        match self.shared.local() {
            Some(shared) if shared.latest_inventory().is_some() => None,
            // A step the person chose, or its refusal, says so until launch runs again.
            Some(_)
                if !matches!(
                    self.launch.state,
                    LaunchState::Launching | LaunchState::Ready
                ) =>
            {
                Some(present(self.launch.state))
            }
            Some(shared) => Some(present(match shared.connection_error() {
                Some(ServerError::Stopped) => LaunchState::Stopped,
                // An open connection's refusal offers what the Server version row offers.
                Some(error) if ends_opening(error) => failed(
                    error,
                    self.launch
                        .refusal_offer
                        .is_some_and(|(refused, update)| refused == error && update),
                ),
                // The connection retries an interruption by itself, so opening goes on.
                _ => LaunchState::Launching,
            })),
            None => Some(present(self.launch.state)),
        }
    }

    /// Whether the window has no usable server state: it presents no Workspace, this computer's
    /// server has not delivered its first inventory, and no other server has delivered one. The
    /// launch screen then fills the window in place of the sidebar and Workspace creation. Another
    /// server's inventory, even with no attached Workspace, keeps the normal layout.
    pub(super) fn startup_recovery(&self) -> bool {
        self.workspaces.active_workspace_id().is_none()
            && self.launch_presentation().is_some()
            && !self.shared.iter().any(|(key, shared)| {
                key != super::ServerKey::LOCAL && shared.latest_inventory().is_some()
            })
    }

    /// Reads whether a verified build can update a server the open connection refused before
    /// its first inventory. Admission can succeed before Pane negotiation refuses a required
    /// capability, so this refusal reads the same offer launch reads, once per refusal.
    pub(super) fn read_refusal_offer(&mut self, cx: &mut Context<Self>) {
        let refused = self
            .shared
            .local()
            .filter(|shared| shared.latest_inventory().is_none())
            .and_then(|shared| shared.connection_error())
            .filter(|error| incompatible(*error));
        let Some(error) = refused else {
            self.launch.refusal_offer = None;
            self.launch._refusal_offer = None;
            return;
        };
        if self
            .launch
            .refusal_offer
            .is_some_and(|(read, _)| read == error)
        {
            return;
        }
        self.launch.refusal_offer = Some((error, false));
        let adapter = Arc::clone(&self.launch.adapters.launch);
        let offered = cx.background_spawn(async move { adapter.update_offered() });
        self.launch._refusal_offer = Some(cx.spawn(async move |manager, cx| {
            let update = offered.await;
            let _ = manager.update(cx, |manager, cx| {
                // A later refusal reads its own offer.
                if manager
                    .launch
                    .refusal_offer
                    .is_some_and(|(read, _)| read == error)
                {
                    manager.launch.refusal_offer = Some((error, update));
                    cx.notify();
                }
            });
        }));
    }

    /// Whether the stage presents nothing for `presentation` now: the window's first launch is
    /// only waiting for the server, and has not waited long enough to say so.
    pub(super) fn launch_is_quiet(&self, presentation: &LaunchPresentation, cx: &App) -> bool {
        presentation.waiting
            && !self.launch.spoke.get()
            && self
                .launch
                .quiet_until
                .is_some_and(|until| cx.background_executor().now() < until)
    }

    /// The stage while launch is quiet. It holds the window's keyboard focus and says nothing.
    pub(super) fn render_launch_quiet(
        &self,
        focus: &gpui::FocusHandle,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(LAUNCH_QUIET_SELECTOR)
            .debug_selector(|| LAUNCH_QUIET_SELECTOR.to_owned())
            .size_full()
            .track_focus(focus)
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
            waiting: _,
        } = presentation;
        self.launch.spoke.set(true);
        let message: SharedString = match progress {
            Some(progress) => progress.into(),
            None if detail.is_empty() => title.into(),
            None => format!("{title}. {detail}").into(),
        };
        let announcement = Some(self.launch.announcement(&message));
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
                    .when(!detail.is_empty(), |column| {
                        column.child(
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
                                        builder
                                            .push_child(builder.synthetic_node_id("status"), node);
                                    })
                                })
                                .whitespace_normal()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(appearance.colors.text_secondary))
                                .child(detail),
                        )
                    }),
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
        let ready = self.launch._task.is_none();
        match action {
            LaunchAction::Settings => crate::ui::settings_window::open_or_activate(
                Some(crate::ui::settings_window::SettingsSectionId::Servers),
                cx,
            ),
            // The journey reads the server again and asks before it changes anything. A
            // finished update runs launch again in every waiting window.
            LaunchAction::Update => crate::ui::settings_window::open_server_update(cx),
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
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use super::{LaunchOutcome, LocalLaunch, LocalServerAdapters, SharedServerConnector};
    use crate::server_connection::ServerError;

    /// A ready server presenting one Workspace, "Default", whose Panes run in `session_factory`.
    pub(crate) fn presenting(
        session_factory: Rc<dyn crate::terminal::TerminalSessionFactory>,
    ) -> LocalServerAdapters {
        use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
        use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};
        use spaceterm_server::hierarchy::{Axis, Inventory, Pane, Tab, Workspace};

        struct Presenting(Rc<dyn crate::terminal::TerminalSessionFactory>);
        impl SharedServerConnector for Presenting {
            fn connect(
                &self,
                client_home: crate::domain::ValidatedLocalDirectory,
                _: crate::platform::local_filesystem::LocalFilesystemAuthority,
                panes: &crate::ui::pane_lifecycle::PaneConstruction,
                _: &mut gpui::App,
            ) -> Option<super::SharedServerConnection> {
                let inventory = Inventory {
                    repositories: None,
                    revision: 1,
                    close: None,
                    workspaces: vec![Workspace {
                        id: [51; 32],
                        name: "Default".to_owned(),
                        name_version: 0,
                        pin_version: 0,
                        pinned_directory: None,
                        root_tab: [52; 32],
                        tabs: vec![Tab {
                            worktree: None,
                            worktree_membership_fixed: false,
                            worktree_repository: None,
                            id: [52; 32],
                            root_pane: [53; 32],
                            panes: vec![Pane {
                                id: [53; 32],
                                source: None,
                                axis: Axis::Right,
                                starting_directory: client_home.path().to_owned(),
                                current_directory: None,
                                repository_directory: None,
                                presentation: None,
                                confirmed_directory: None,
                                creator: None,
                            }],
                        }],
                    }],
                };
                let opener = ScriptedPanes::new(
                    crate::terminal::WorkspaceTerminalSessionFactory::new_local(
                        Rc::clone(&self.0),
                        client_home,
                    ),
                );
                Some(super::SharedServerConnection {
                    edits: SharedEditSource::new(Rc::new(ScriptedEdits::new(Some(inventory)))),
                    sessions: SharedPaneSessions::with_opener(opener, panes),
                    retained: None,
                })
            }
        }
        LocalServerAdapters {
            launch: Arc::new(ScriptedLaunch {
                ready: true,
                ..ScriptedLaunch::default()
            }),
            connector: Rc::new(Presenting(session_factory)),
        }
    }

    /// A server whose launch never answers, for windows a test does not launch.
    pub(crate) fn unanswered() -> LocalServerAdapters {
        struct Unreachable;
        impl SharedServerConnector for Unreachable {
            fn connect(
                &self,
                _: crate::domain::ValidatedLocalDirectory,
                _: crate::platform::local_filesystem::LocalFilesystemAuthority,
                _: &crate::ui::pane_lifecycle::PaneConstruction,
                _: &mut gpui::App,
            ) -> Option<super::SharedServerConnection> {
                None
            }
        }
        LocalServerAdapters {
            launch: Arc::new(ScriptedLaunch::default()),
            connector: Rc::new(Unreachable),
        }
    }

    /// A launch whose answers a test queues and whose explicit steps it records.
    #[derive(Default)]
    pub(crate) struct ScriptedLaunch {
        /// Answers every run with Ready once the queue is empty, instead of never answering.
        ready: bool,
        runs: Mutex<VecDeque<Result<LaunchOutcome, ServerError>>>,
        steps: Mutex<Vec<&'static str>>,
        start: Mutex<Option<ServerError>>,
        authorize: Mutex<Option<ServerError>>,
        /// Whether a verified build can update a refused server.
        update: Mutex<bool>,
    }

    impl ScriptedLaunch {
        /// Offers a verified server build when a run refuses the server.
        pub(crate) fn offer_update(&self) {
            *self.update.lock().unwrap() = true;
        }
        pub(crate) fn answer(&self, result: Result<LaunchOutcome, ServerError>) {
            self.runs.lock().unwrap().push_back(result);
        }
        pub(crate) fn refuse_start(&self, error: ServerError) {
            *self.start.lock().unwrap() = Some(error);
        }
        /// Answers the next authorization with `error`. `AuthorizationRequired` is a prompt
        /// that was cancelled or refused.
        pub(crate) fn refuse_authorization(&self, error: ServerError) {
            *self.authorize.lock().unwrap() = Some(error);
        }
        /// Every step in order: `run`, `start`, `authorize`, and `update_offered`.
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
                .unwrap_or(Ok(if self.ready {
                    LaunchOutcome::Ready
                } else {
                    LaunchOutcome::Cancelled
                }))
        }
        fn start(&self) -> Result<(), ServerError> {
            self.steps.lock().unwrap().push("start");
            self.start.lock().unwrap().take().map_or(Ok(()), Err)
        }
        fn authorize(&self) -> Result<(), ServerError> {
            self.steps.lock().unwrap().push("authorize");
            self.authorize.lock().unwrap().take().map_or(Ok(()), Err)
        }
        fn update_offered(&self) -> bool {
            self.steps.lock().unwrap().push("update_offered");
            *self.update.lock().unwrap()
        }
    }
}
