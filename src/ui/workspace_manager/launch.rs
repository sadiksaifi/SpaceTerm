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
use spaceterm_ui::{Button, ProgressRing, ProgressSize, ProgressState};

use super::WorkspaceManager;
use crate::machine_connection::{CoreError, MachineConnection};
use crate::platform::local_filesystem::LocalFilesystemAuthority;
use crate::ui::pane_lifecycle::PaneConstruction;
use crate::ui::settings_window::{AuthorizationPrompt, AuthorizedRequest};
use crate::ui::workspaces::{WorkspaceEditSource, WorkspacePaneSessions};

pub(super) const LAUNCH_SELECTOR: &str = "server-launch";
pub(super) const LAUNCH_START_SELECTOR: &str = "server-launch-start";
pub(super) const LAUNCH_AUTHORIZE_SELECTOR: &str = "server-launch-authorize";
pub(super) const LAUNCH_RETRY_SELECTOR: &str = "server-launch-retry";
pub(super) const LAUNCH_SETTINGS_SELECTOR: &str = "server-launch-settings";
pub(super) const LAUNCH_UPDATE_SELECTOR: &str = "server-launch-update";
pub(super) const LAUNCH_LOGIN_ITEMS_SELECTOR: &str = "server-launch-login-items";
const LAUNCH_QUIET_SELECTOR: &str = "server-launch-quiet";
/// The launch alert's first action, the launch screen's default one.
pub(super) const LAUNCH_ALERT_ACTION_SELECTOR: &str = "server-launch-alert-action";
const LAUNCH_ALERT_SECOND_ACTION_SELECTOR: &str = "server-launch-alert-second-action";
/// The launch alert's OK, or its Cancel beside an action.
const LAUNCH_ALERT_CANCEL_SELECTOR: &str = "server-launch-alert-ok";

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
    /// Another service operation, such as Update, Restart or Stop, is running. Launch waits for
    /// it, then runs again.
    ServiceBusy,
    /// The window closed or SpaceTerm quit before launch finished.
    Cancelled,
}

/// The blocking steps of local launch. Each runs on a background thread.
pub(crate) trait LocalLaunch: Send + Sync {
    /// Prepares or reuses the account's service and connects. Never reverses Stop and never asks
    /// for authorization. It may install, enable and start the service, except one off in Login
    /// Items, which it refuses with [`CoreError::LoginItemOff`].
    fn run(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<LaunchOutcome, CoreError>;
    /// Whether macOS keeps SpaceTerm Core's background item off, because the person turned it
    /// off in Login Items or has not approved it yet. Reads only: it changes nothing. Launch reads
    /// it when an open connection fails, since only [`Self::run`] and [`Self::start`] refuse.
    fn background_item_off(&self) -> bool;
    /// Opens System Settings at Login Items, where the person turns SpaceTerm Core on. It must
    /// run on the event thread, so macOS brings System Settings forward.
    fn open_login_items(&self);
    /// The person's explicit Start Server, which reverses their Stop. It refuses with
    /// [`CoreError::LoginItemOff`] and changes nothing while the item is off in Login Items.
    fn start(&self) -> Result<(), CoreError>;
    /// The person's explicit startup authorization. Cancelling the system prompt changes nothing.
    fn authorize(&self) -> Result<(), CoreError>;
    /// Waits for the service operation a run answered `ServiceBusy` for. It fails with
    /// `ServiceBusy` only when the operation outlasts the server's bounded wait.
    fn wait_for_service(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<(), CoreError>;
    /// Whether this SpaceTerm includes a verified server build other than the approved one.
    /// Reads only: it stages, installs, and starts nothing.
    fn update_offered(&self) -> bool;
}

/// The Workspaces a ready server presents, and the connection that retains this client's views.
pub(crate) struct WorkspaceMachineConnection {
    pub(crate) edits: WorkspaceEditSource,
    pub(crate) sessions: WorkspacePaneSessions,
    /// Retains attachments and arrangements across relaunch.
    pub(crate) retained: Option<MachineConnection>,
}

/// Connects the Workspace Window to a ready server on the event thread.
pub(crate) trait WorkspaceCoreConnector {
    fn connect(
        &self,
        local_files: LocalFilesystemAuthority,
        panes: &PaneConstruction,
        cx: &mut App,
    ) -> Option<WorkspaceMachineConnection>;

    /// The account-local connection whose client directory retains this desktop's Workspace
    /// order, or `None` to keep the order for the window's life. Opening it needs no running
    /// server.
    fn workspace_order(&self) -> Option<MachineConnection> {
        None
    }
}

/// The account-local server a Workspace Window launches and presents.
#[derive(Clone)]
pub(crate) struct LocalCoreAdapters {
    pub(crate) launch: Arc<dyn LocalLaunch>,
    pub(crate) connector: Rc<dyn WorkspaceCoreConnector>,
}

impl LocalCoreAdapters {
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
    fn service() -> Result<crate::machine_connection::service::Service, CoreError> {
        crate::machine_connection::service::Service::for_identity(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
        )
    }
}

impl LocalLaunch for AccountLaunch {
    fn run(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<LaunchOutcome, CoreError> {
        use spaceterm_core::StartupOutcome;
        Ok(match crate::local_startup::run(cancelled)? {
            StartupOutcome::Ready { .. } => LaunchOutcome::Ready,
            StartupOutcome::Stopped => LaunchOutcome::Stopped,
            StartupOutcome::AuthorizationRequired => LaunchOutcome::AuthorizationRequired,
            StartupOutcome::ServiceBusy => LaunchOutcome::ServiceBusy,
            StartupOutcome::Cancelled => LaunchOutcome::Cancelled,
        })
    }

    fn background_item_off(&self) -> bool {
        crate::local_startup::background_item_off()
    }

    fn open_login_items(&self) {
        crate::local_startup::open_login_items();
    }

    fn wait_for_service(&self, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<(), CoreError> {
        spaceterm_core::LocalStartup::for_identity(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
        )?
        .wait_for_service(cancelled)
    }

    fn start(&self) -> Result<(), CoreError> {
        Self::service()?.start().map(drop)
    }

    fn authorize(&self) -> Result<(), CoreError> {
        // The service asks the operating system for its one fixed authorization, for this
        // account only. No SpaceTerm code runs as root.
        Self::service()?
            .enable_linger(crate::machine_connection::service::LingerAuthorization::Desktop)
    }

    fn update_offered(&self) -> bool {
        // The same offer the Server version row makes, so both surfaces agree.
        Self::service().is_ok_and(|service| crate::ui::settings_window::update_offered(&service))
    }
}

struct AccountConnector;

impl WorkspaceCoreConnector for AccountConnector {
    fn connect(
        &self,
        local_files: LocalFilesystemAuthority,
        panes: &PaneConstruction,
        cx: &mut App,
    ) -> Option<WorkspaceMachineConnection> {
        // Server Settings and the Workspace Window share one edit writer for this client.
        let client = crate::ui::core_worktrees::Client::get(cx)?;
        let core = client.core.clone();
        Some(WorkspaceMachineConnection {
            edits: client.source,
            sessions: WorkspacePaneSessions::new(core.clone(), client.controls, local_files, panes),
            retained: Some(core),
        })
    }

    fn workspace_order(&self) -> Option<MachineConnection> {
        MachineConnection::for_identity(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
        )
        .ok()
    }
}

/// A step that runs before launch: one the person chose from the launch screen, or the wait
/// for a service operation that another process runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LaunchStep {
    Start,
    Authorize,
    /// Says another server operation is running, and opens the Workspaces once it ends, without
    /// asking the person to act.
    WaitForService,
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
    Failed(CoreError),
    /// SpaceTerm Core's background item is off in Login Items: the person turned it off, or
    /// macOS waits for them to approve it. System Settings shows both the same way, and the
    /// person turns it on there in both.
    LoginItemOff,
    /// This client refused the server, which keeps running with its Terminal Sessions.
    /// `update` is whether this SpaceTerm includes a verified server build to update it to.
    Incompatible {
        reason: CoreError,
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
    /// Opens System Settings at Login Items & Extensions.
    OpenLoginItems,
}

impl LaunchAction {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start SpaceTerm Core",
            Self::Authorize => "Authorize\u{2026}",
            Self::Retry => "Try Again",
            Self::Settings => "Machine Settings\u{2026}",
            Self::Update => "Update SpaceTerm Core\u{2026}",
            Self::OpenLoginItems => "Open Login Items",
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Start => LAUNCH_START_SELECTOR,
            Self::Authorize => LAUNCH_AUTHORIZE_SELECTOR,
            Self::Retry => LAUNCH_RETRY_SELECTOR,
            Self::Settings => LAUNCH_SETTINGS_SELECTOR,
            Self::Update => LAUNCH_UPDATE_SELECTOR,
            Self::OpenLoginItems => LAUNCH_LOGIN_ITEMS_SELECTOR,
        }
    }
}

/// What the launch screen's icon says about the state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LaunchIcon {
    /// The person stopped the server.
    Stopped,
    /// Launch waits for the person's authorization.
    Authorization,
    /// Launch could not finish.
    Failure,
}

impl LaunchIcon {
    fn presentation(self) -> (spaceterm_ui::IconName, spaceterm_ui::EmptyStateTone) {
        use spaceterm_ui::{EmptyStateTone, IconName};
        match self {
            Self::Stopped => (IconName::Pause, EmptyStateTone::Quiet),
            Self::Authorization => (IconName::Lock, EmptyStateTone::Warning),
            Self::Failure => (IconName::CircleAlert, EmptyStateTone::Error),
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
    /// The icon above the title and whether it asks the person to act. A step that runs shows
    /// its progress instead.
    pub(super) icon: Option<LaunchIcon>,
    /// The window is only waiting for the server: nothing failed and nothing asks the person to
    /// act, so a short wait need not be presented at all.
    pub(super) waiting: bool,
}

pub(super) const OPENING_TITLE: &str = "Opening Workspaces";
// Launch reuses a running server or starts it, so the detail claims neither.
const OPENING_DETAIL: &str = "SpaceTerm is waiting for SpaceTerm Core.";
pub(super) const STOPPED_TITLE: &str = "SpaceTerm Core is stopped";

/// Why this computer's Workspaces are closed while its server is stopped. `place` is the host's
/// name for this computer, such as "This Mac".
pub(super) fn stopped_detail(place: &str) -> SharedString {
    format!("Workspaces on {place} open when it starts.").into()
}
pub(super) const AUTHORIZATION_TITLE: &str = "Run SpaceTerm Core at Startup";
const AUTHORIZATION_DECLINED: &str = "Authorization didn\u{2019}t finish.";
const AUTHORIZING_DETAIL: &str =
    "Enter an administrator password in the system prompt to continue.";
pub(super) const BUSY_TITLE: &str = "Waiting for SpaceTerm Core";
const BUSY_DETAIL: &str =
    "Another SpaceTerm Core operation is running. Your Workspaces open when it finishes.";

/// The title while SpaceTerm Core's background item is off in Login Items.
pub(super) const LOGIN_ITEM_OFF_TITLE: &str = "SpaceTerm Core is off in Login Items";

/// The title for a launch that could not finish. The detail names the exact reason.
const fn failure_title(error: CoreError) -> &'static str {
    match error {
        CoreError::IncompatibleState | CoreError::IncompatibleProtocol(_) => {
            "Incompatible SpaceTerm Core"
        }
        CoreError::ActivationUnresolved => "SpaceTerm Core Update Unfinished",
        CoreError::CandidateChanged => "Restart SpaceTerm",
        CoreError::CandidateMissing => "SpaceTerm Core Not Included",
        CoreError::RuntimeUnavailable => "Couldn\u{2019}t Verify SpaceTerm Core",
        CoreError::UnsafeStorage => "Unsafe SpaceTerm Core Storage",
        CoreError::StorageUnavailable => "SpaceTerm Core Storage Unavailable",
        CoreError::NotInstalled => "SpaceTerm Core Not Installed",
        CoreError::AuthenticationRefused => "Connection Refused",
        CoreError::ServiceBusy => "SpaceTerm Core Busy",
        _ => "Couldn\u{2019}t Open Workspaces",
    }
}

/// The detail for a launch that could not finish: the exact reason, and for a server package,
/// whether this build lacks one or holds one that failed verification.
fn failure_detail(error: CoreError) -> SharedString {
    match error {
        CoreError::CandidateMissing => {
            "This SpaceTerm build doesn\u{2019}t include SpaceTerm Core. Reinstall SpaceTerm, then try \
             again."
                .into()
        }
        CoreError::RuntimeUnavailable => "Reinstall SpaceTerm, then try again.".into(),
        error => error.to_string().into(),
    }
}

/// Whether a connection failure before the first inventory needs the person, because
/// reconnecting alone cannot open the Workspaces.
const fn ends_opening(error: CoreError) -> bool {
    matches!(
        error,
        CoreError::IncompatibleProtocol(_)
            | CoreError::ActivationUnresolved
            | CoreError::IncompatibleState
            | CoreError::DataResetRequired
            | CoreError::StorageUnavailable
            | CoreError::UnsafeStorage
            | CoreError::AuthenticationRefused
    )
}

/// Whether this client refused the server itself, which keeps running for its other clients.
const fn incompatible(error: CoreError) -> bool {
    matches!(error, CoreError::IncompatibleProtocol(_))
}

/// What launch presents for a failure. A refused server offers the update journey only when a
/// read found a verified build to update it to.
const fn failed(error: CoreError, update: bool) -> LaunchState {
    if incompatible(error) {
        LaunchState::Incompatible {
            reason: error,
            update,
        }
    } else {
        LaunchState::Failed(error)
    }
}

/// `place` is the host's name for this computer, such as "This Mac".
pub(super) fn present(state: LaunchState, place: &str) -> LaunchPresentation {
    use LaunchAction::{Authorize, OpenLoginItems, Retry, Settings, Start, Update};
    let failure = Some(LaunchIcon::Failure);
    let fixed = SharedString::new_static;
    match state {
        LaunchState::Launching | LaunchState::Ready => LaunchPresentation {
            title: OPENING_TITLE,
            detail: fixed(OPENING_DETAIL),
            progress: Some("Waiting for SpaceTerm Core\u{2026}"),
            actions: &[],
            icon: None,
            waiting: true,
        },
        LaunchState::Working(LaunchStep::Start) => LaunchPresentation {
            title: STOPPED_TITLE,
            detail: stopped_detail(place),
            progress: Some("Starting SpaceTerm Core\u{2026}"),
            actions: &[],
            icon: None,
            waiting: false,
        },
        LaunchState::Working(LaunchStep::Authorize) => LaunchPresentation {
            title: AUTHORIZATION_TITLE,
            detail: fixed(AUTHORIZING_DETAIL),
            progress: Some("Waiting for authorization\u{2026}"),
            actions: &[],
            icon: None,
            waiting: false,
        },
        // Nothing failed and nothing asks the person to act, so the screen offers no action.
        LaunchState::Working(LaunchStep::WaitForService) => LaunchPresentation {
            title: BUSY_TITLE,
            detail: fixed(BUSY_DETAIL),
            progress: Some("Waiting for SpaceTerm Core\u{2026}"),
            actions: &[],
            icon: None,
            waiting: false,
        },
        LaunchState::Stopped => LaunchPresentation {
            title: STOPPED_TITLE,
            detail: stopped_detail(place),
            progress: None,
            actions: &[Start],
            icon: Some(LaunchIcon::Stopped),
            waiting: false,
        },
        // The wording claims no one's choice: the person may have turned it off, or never
        // approved it. Either way it is a state to change, not a fault, so nothing warns.
        LaunchState::LoginItemOff => LaunchPresentation {
            title: LOGIN_ITEM_OFF_TITLE,
            detail: format!("Workspaces on {place} open when you turn it on.").into(),
            progress: None,
            actions: &[OpenLoginItems],
            icon: Some(LaunchIcon::Stopped),
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
                // The same explanation every other authorization surface gives.
                let prompt = AuthorizationPrompt::for_request(&AuthorizedRequest::Start, None);
                format!("{} {}", prompt.message, prompt.detail).into()
            },
            progress: None,
            actions: &[Authorize, Settings],
            icon: Some(LaunchIcon::Authorization),
            waiting: false,
        },
        LaunchState::Incompatible { reason, update } => {
            let detail = if update {
                "Updating SpaceTerm Core ends its Terminal Sessions for every client."
            } else {
                "SpaceTerm and this SpaceTerm Core are incompatible."
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
                icon: failure,
                waiting: false,
            }
        }
        LaunchState::Failed(error) => LaunchPresentation {
            title: failure_title(error),
            detail: failure_detail(error),
            progress: None,
            // Only another SpaceTerm build includes a server package, so trying again from this
            // one changes nothing, as Settings also treats it.
            actions: if matches!(error, CoreError::CandidateMissing) {
                &[Settings]
            } else {
                &[Retry, Settings]
            },
            icon: failure,
            waiting: false,
        },
    }
}

/// One Workspace Window's launch: its adapters, where it stands, and the running step.
pub(super) struct LocalLaunchState {
    adapters: LocalCoreAdapters,
    pub(super) state: LaunchState,
    /// The host's name for this computer, such as "This Mac".
    place: &'static str,
    /// The window went inactive while it said SpaceTerm Core is off in Login Items.
    away: bool,
    /// The last launch or connection loss found this computer's SpaceTerm Core turned off in
    /// Login Items. It holds while a launch rechecks, so the presented Workspaces keep saying so
    /// instead of reconnecting.
    turned_off: bool,
    /// The failed connection attempts counted when this computer's connection loss last read
    /// the background item, so each failed attempt reads it once.
    background_item_read: Option<u64>,
    _background_item: Option<Task<()>>,
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
    refusal_offer: Option<(CoreError, bool)>,
    _refusal_offer: Option<Task<()>>,
    /// New Workspace was chosen with no Workspace Window open. The window opened for it creates
    /// the Workspace once this computer's server presents its Workspaces.
    create_when_presented: bool,
    /// The failed connection attempts counted when the person last chose Try Again on the empty
    /// view, while that view still waits for this computer's server.
    pub(super) reconnect_from: std::cell::Cell<Option<u64>>,
}

impl LocalLaunchState {
    /// `place` is the host's name for this computer, such as "This Mac".
    pub(super) fn new(adapters: LocalCoreAdapters, place: &'static str) -> Self {
        Self {
            adapters,
            state: LaunchState::Launching,
            place,
            away: false,
            turned_off: false,
            background_item_read: None,
            _background_item: None,
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
            reconnect_from: std::cell::Cell::new(None),
        }
    }

    /// The account-local connection that retains this desktop's Workspace order, if any.
    pub(super) fn workspace_order_connection(&self) -> Option<MachineConnection> {
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
    Failed(CoreError),
    /// The client refused the server. `update` is whether a verified build can update it.
    Incompatible(CoreError, bool),
    /// The person's authorization did not complete.
    Declined,
}

/// The person's explicit Start Server from a Pane whose server they stopped. The window runs its
/// Start step, then launch, and its Panes reconnect once the server answers.
pub(crate) fn start_local_core(window: &mut Window, cx: &mut App) {
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
                    | LaunchState::LoginItemOff
                    | LaunchState::Incompatible { .. }
            ) && manager.launch._task.is_none()
            {
                manager.run_local_launch(None, window, cx);
            } else if manager.launch._task.is_some() {
                manager.launch.resume = true;
            } else if manager.launch.state == LaunchState::Ready {
                manager.core_started(cx);
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
        self.create_pending_workspace(super::MachineKey::LOCAL, window, cx);
    }

    /// Creates the Workspace New Workspace asked for before this computer's server presented
    /// its Workspaces, once it has.
    pub(super) fn create_pending_workspace(
        &mut self,
        key: super::MachineKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key != super::MachineKey::LOCAL
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
            // The service refuses to start what is off in Login Items, so neither the person's
            // Start nor launch turns it back on.
            let refused = match step {
                Some(LaunchStep::Start) => adapter.start().err().map(StepResult::Failed),
                // Only a cancelled or refused prompt leaves startup unchanged. Any other failure
                // says what went wrong and claims nothing about what the system changed.
                Some(LaunchStep::Authorize) => adapter.authorize().err().map(|error| match error {
                    CoreError::AuthorizationRequired => StepResult::Declined,
                    error => StepResult::Failed(error),
                }),
                Some(LaunchStep::WaitForService) => adapter
                    .wait_for_service(&|| cancelled.load(Ordering::Acquire))
                    .err()
                    .map(StepResult::Failed),
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
            StepResult::Outcome(LaunchOutcome::Ready) => self.connect_local_core(window, cx),
            StepResult::Outcome(LaunchOutcome::Stopped) => LaunchState::Stopped,
            StepResult::Failed(CoreError::LoginItemOff) => LaunchState::LoginItemOff,
            // The operation decides what launch finds next, so launch runs again once it ends.
            StepResult::Outcome(LaunchOutcome::ServiceBusy) => {
                self.run_local_launch(Some(LaunchStep::WaitForService), window, cx);
                return;
            }
            StepResult::Outcome(LaunchOutcome::AuthorizationRequired)
            | StepResult::Failed(CoreError::AuthorizationRequired) => {
                LaunchState::AuthorizationRequired { declined: false }
            }
            StepResult::Declined => LaunchState::AuthorizationRequired { declined: true },
            StepResult::Failed(CoreError::Stopped) => LaunchState::Stopped,
            StepResult::Failed(error) => LaunchState::Failed(error),
            StepResult::Incompatible(error, update) => failed(error, update),
            // Only a closing window cancels, so there is nothing left to present.
            StepResult::Outcome(LaunchOutcome::Cancelled) => return,
        };
        self.launch.state = state;
        self.launch._task = None;
        let was_turned_off = std::mem::replace(
            &mut self.launch.turned_off,
            state == LaunchState::LoginItemOff,
        );
        if std::mem::take(&mut self.launch.resume)
            && matches!(
                state,
                LaunchState::Stopped
                    | LaunchState::AuthorizationRequired { .. }
                    | LaunchState::Failed(_)
                    | LaunchState::LoginItemOff
                    | LaunchState::Incompatible { .. }
            )
        {
            self.run_local_launch(None, window, cx);
            return;
        }
        // A running server answers the Panes a stop interrupted, so they connect again, and the
        // window's own connection reconnects without waiting for its delay.
        if state == LaunchState::Ready {
            self.core_started(cx);
            if let Some(binding) = self.binding.local() {
                binding.retry_connection();
            }
        }
        // While Workspaces are presented the launch screen is hidden, so a step that could not
        // finish explains itself in an alert instead. A Login Items state the window already
        // said, or says in place of the Active Workspace's Tabs, needs no alert.
        let said = state == LaunchState::LoginItemOff
            && (was_turned_off || self.local_core_turned_off_presented());
        if self.launch_presentation().is_none()
            && !said
            && let Some(presentation) =
                (state != LaunchState::Ready).then(|| present(state, self.launch.place))
        {
            Self::present_launch_alert(presentation, window, cx);
        }
        // The screen's own focus moves to its default action when one appears. The keyboard the
        // person or a reopened window left in the sidebar stays there.
        if self.workspaces.active_workspace_id().is_none()
            && !self.sidebar_holds_keyboard(window, cx)
        {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// Reconnects each Pane a stop or Login Items interrupted, once this computer's server runs
    /// again. Another computer's Panes follow their own server.
    fn core_started(&self, cx: &mut App) {
        for workspace in self.workspaces.iter() {
            if self
                .binding
                .owner(workspace.id())
                .is_some_and(|key| key != super::MachineKey::LOCAL)
            {
                continue;
            }
            workspace
                .payload()
                .update(cx, |manager, cx| manager.core_started(cx));
        }
    }

    /// Presents a ready server's Workspaces in this window.
    fn connect_local_core(&mut self, window: &mut Window, cx: &mut Context<Self>) -> LaunchState {
        if self.binding.local().is_some() {
            return LaunchState::Ready;
        }
        // One device presents its servers in one Workspace Window. Another window comes forward
        // instead; this one may already present a paired server and adds the local one beside it.
        if let Some(existing) = crate::ui::workspaces::presenting_window(cx)
            && existing.window_id() != window.window_handle().window_id()
        {
            cx.defer(move |cx| {
                let _ = existing.update(cx, |_, window, _| window.activate_window());
            });
            return LaunchState::Ready;
        }
        let connector = Rc::clone(&self.launch.adapters.connector);
        let Some(connection) =
            connector.connect(self.local_filesystem.clone(), &self.pane_construction, cx)
        else {
            return LaunchState::Failed(CoreError::Protocol);
        };
        self.present_workspaces(connection.edits, connection.sessions, window, cx);
        if let (Some(binding), Some(core)) = (self.binding.local_mut(), connection.retained) {
            binding.identify(Rc::new(core.clone()));
            binding.retain_client_views(core);
            let read = binding.client_views_read();
            self.launch._views_read = Some(cx.spawn_in(window, async move |manager, cx| {
                read.await;
                let _ = manager.update_in(cx, |manager, window, cx| {
                    manager.sync_workspaces(super::MachineKey::LOCAL, window, cx);
                });
            }));
        }
        LaunchState::Ready
    }

    /// Explains a step that could not finish while the launch screen is hidden. The alert offers
    /// the launch screen's own actions, such as Open Login Items, so nothing is left only to OK.
    fn present_launch_alert(
        presentation: LaunchPresentation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use spaceterm_ui::{
            Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionRole,
            ModalId,
        };
        let mut buttons: Vec<ModalAction<Option<LaunchAction>>> = presentation
            .actions
            .iter()
            .enumerate()
            .map(|(index, &action)| {
                if index == 0 {
                    ModalAction::new(
                        Some(action),
                        action.label(),
                        ModalActionRole::Affirmative,
                        LAUNCH_ALERT_ACTION_SELECTOR,
                    )
                    .with_emphasis(ModalActionEmphasis::Prominent)
                    .default_action(true)
                } else {
                    ModalAction::new(
                        Some(action),
                        action.label(),
                        ModalActionRole::Auxiliary,
                        LAUNCH_ALERT_SECOND_ACTION_SELECTOR,
                    )
                }
            })
            .collect();
        buttons.push(ModalAction::new(
            None,
            if buttons.is_empty() { "OK" } else { "Cancel" },
            ModalActionRole::Cancel,
            LAUNCH_ALERT_CANCEL_SELECTOR,
        ));
        let window_handle = window.window_handle();
        let manager = cx.entity().downgrade();
        let _ = Alert::new(
            ModalId::new("server-launch-alert"),
            presentation.title,
            presentation.title,
            presentation.detail.to_string(),
            buttons,
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, move |outcome, cx| {
            let AlertOutcome::Activated {
                action_id: Some(action),
                ..
            } = outcome
            else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |manager, cx| {
                    manager.run_launch_action(action, window, cx);
                });
            });
        });
    }

    /// The launch screen this window presents in place of the empty view, if any.
    ///
    /// It stays until the server's first inventory arrives, so a person never sees "No
    /// Workspaces" while their Workspaces are on the way.
    pub(super) fn launch_presentation(&self) -> Option<LaunchPresentation> {
        match self.binding.local() {
            Some(binding) if binding.latest_inventory().is_some() => None,
            // A step the person chose, or its refusal, says so until launch runs again.
            Some(_)
                if !matches!(
                    self.launch.state,
                    LaunchState::Launching | LaunchState::Ready
                ) =>
            {
                Some(present(self.launch.state, self.launch.place))
            }
            Some(binding) => Some(present(
                match binding.connection_error() {
                    Some(CoreError::Stopped) => LaunchState::Stopped,
                    // The connection waits for the operation again by itself.
                    Some(CoreError::ServiceBusy) => {
                        LaunchState::Working(LaunchStep::WaitForService)
                    }
                    // An open connection's refusal offers what the Server version row offers.
                    Some(error) if ends_opening(error) => failed(
                        error,
                        self.launch
                            .refusal_offer
                            .is_some_and(|(refused, update)| refused == error && update),
                    ),
                    // The connection retries an interruption by itself, so opening goes on.
                    _ => LaunchState::Launching,
                },
                self.launch.place,
            )),
            None => Some(present(self.launch.state, self.launch.place)),
        }
    }

    /// Whether the window has no usable server state: it presents no Workspace, this computer's
    /// server has not delivered its first inventory, and no other server has delivered one. The
    /// launch screen then fills the window in place of the sidebar and Workspace creation. Another
    /// server's inventory, even with no attached Workspace, keeps the normal layout.
    pub(super) fn startup_recovery(&self) -> bool {
        self.workspaces.active_workspace_id().is_none()
            && self.launch_presentation().is_some()
            && !self.binding.iter().any(|(key, binding)| {
                key != super::MachineKey::LOCAL && binding.latest_inventory().is_some()
            })
    }

    /// Reads whether a verified build can update a server the open connection refused before
    /// its first inventory. Admission can succeed before Pane negotiation refuses a required
    /// capability, so this refusal reads the same offer launch reads, once per refusal.
    pub(super) fn read_refusal_offer(&mut self, cx: &mut Context<Self>) {
        let refused = self
            .binding
            .local()
            .filter(|binding| binding.latest_inventory().is_none())
            .and_then(|binding| binding.connection_error())
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
            icon,
            waiting: _,
        } = presentation;
        self.launch.spoke.set(true);
        let message: SharedString = match progress {
            Some(progress) => progress.into(),
            None if detail.is_empty() => title.into(),
            None => format!("{title}. {detail}").into(),
        };
        let announcement = self.launch.announcement(&message);
        let mut state = crate::ui::empty_state::empty_state(title, cx)
            .title_debug_selector(format!("{LAUNCH_SELECTOR}-title"));
        if let Some((icon, tone)) = icon.map(LaunchIcon::presentation) {
            state = state.icon(icon).tone(tone);
        }
        if !detail.is_empty() {
            state = state.detail_element(
                div()
                    .id("server-launch-detail")
                    .debug_selector(|| format!("{LAUNCH_SELECTOR}-detail"))
                    .role(gpui::accesskit::Role::Group)
                    .a11y_synthetic_children(move |builder| {
                        let mut node = announcement.clone();
                        if let Some(bounds) = builder.parent_node().bounds() {
                            node.set_bounds(bounds);
                        }
                        builder.push_child(builder.synthetic_node_id("status"), node);
                    })
                    .child(detail.clone()),
            );
        }
        for (index, &action) in actions.iter().enumerate() {
            let owner = cx.weak_entity();
            let button = Button::new(action.selector(), action.label())
                .tab_stop(true)
                .debug_selector(action.selector())
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |manager, cx| {
                        manager.run_launch_action(action, window, cx);
                    });
                });
            state = if index == 0 {
                state.primary(button)
            } else {
                state.link(button)
            };
        }
        // The detail already says what launch waits for, so the progress label is spoken only.
        if let Some(progress) = progress {
            state = state.footer(
                div()
                    .debug_selector(|| format!("{LAUNCH_SELECTOR}-progress"))
                    .mt(appearance.spacing(12.0))
                    .child(
                        ProgressRing::new(
                            "server-launch-progress",
                            progress,
                            ProgressState::Indeterminate,
                        )
                        .size(ProgressSize::Compact),
                    ),
            );
        }
        let default = actions.first().copied();
        let owner = cx.weak_entity();
        div()
            .id(LAUNCH_SELECTOR)
            .debug_selector(|| LAUNCH_SELECTOR.to_owned())
            .role(gpui::accesskit::Role::Group)
            .aria_label(title)
            .aria_description(detail)
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
            .px(appearance.spacing(24.0))
            .child(state)
    }

    /// Follows the window's activation while it says SpaceTerm Core is off in Login Items. The
    /// person may turn it on there while the window is inactive, so launch runs again when they
    /// come back.
    pub(super) fn launch_window_activation(
        &mut self,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let waiting =
            self.launch.turned_off || matches!(self.launch.state, LaunchState::LoginItemOff);
        if !active {
            self.launch.away = waiting;
        } else if std::mem::take(&mut self.launch.away) && waiting {
            if self.launch._task.is_none() {
                self.run_local_launch(None, window, cx);
            } else {
                // The running launch may have read the item before the person turned it on.
                self.launch.resume = true;
            }
        }
    }

    /// Whether the stage says this computer's SpaceTerm Core is turned off in Login Items in
    /// place of the Active Workspace's Tabs.
    pub(super) fn local_core_turned_off_presented(&self) -> bool {
        self.launch.turned_off
            && self
                .workspaces
                .active_workspace_id()
                .and_then(|workspace_id| self.binding.owner(workspace_id))
                == Some(super::MachineKey::LOCAL)
    }

    /// Whether this computer's SpaceTerm Core is turned off in Login Items, as the last launch
    /// or connection loss read.
    pub(super) const fn local_core_turned_off(&self) -> bool {
        self.launch.turned_off
    }

    /// Reads, off the event thread, the background item launch reads, once for each failed
    /// attempt of this computer's open connection. macOS stops SpaceTerm Core when the person
    /// turns it off in Login Items, and reconnecting can't reach it until they turn it on, so
    /// the window then says what launch says. Once the item is on again, launch runs again and
    /// the Panes reconnect. The read changes nothing.
    pub(super) fn read_background_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(binding) = self.binding.local() else {
            return;
        };
        let attempts = binding.failed_attempts();
        if binding.connection_error().is_none() {
            self.launch.background_item_read = None;
            self.launch._background_item = None;
            // The server answered again, so its Panes reconnect now rather than on their delay.
            if binding.connected() && std::mem::take(&mut self.launch.turned_off) {
                if self.launch.state == LaunchState::LoginItemOff {
                    self.launch.state = LaunchState::Ready;
                }
                self.core_started(cx);
                cx.notify();
            }
            return;
        }
        // A launch reads the item itself, and a launch screen's own state stays until it reruns.
        if self.launch._task.is_some()
            || !matches!(
                self.launch.state,
                LaunchState::Ready | LaunchState::LoginItemOff
            )
            || self.launch.background_item_read == Some(attempts)
        {
            return;
        }
        self.launch.background_item_read = Some(attempts);
        let adapter = Arc::clone(&self.launch.adapters.launch);
        let read = cx.background_spawn(async move { adapter.background_item_off() });
        self.launch._background_item = Some(cx.spawn_in(window, async move |manager, cx| {
            let off = read.await;
            let _ = manager.update_in(cx, |manager, window, cx| {
                manager.finish_background_item_read(off, window, cx);
            });
        }));
    }

    fn finish_background_item_read(
        &mut self,
        off: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let launch = &mut self.launch;
        // A launch that began meanwhile answers for itself.
        if launch._task.is_some()
            || !matches!(launch.state, LaunchState::Ready | LaunchState::LoginItemOff)
        {
            return;
        }
        if off {
            if !launch.turned_off {
                launch.state = LaunchState::LoginItemOff;
                launch.turned_off = true;
                // The person turns it on in System Settings, so coming back runs launch again.
                launch.away = true;
                cx.notify();
            }
        } else if launch.turned_off {
            // The person turned it on again. Launch reuses or starts the service, and its Ready
            // reconnects the Panes.
            self.run_local_launch(None, window, cx);
        }
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
                Some(crate::ui::settings_window::SettingsSectionId::Machines),
                cx,
            ),
            // The journey reads the server again and asks before it changes anything. A
            // finished update runs launch again in every waiting window.
            LaunchAction::Update => crate::ui::settings_window::open_core_update(cx),
            // Launch runs again when the person comes back to this window.
            LaunchAction::OpenLoginItems => self.launch.adapters.launch.open_login_items(),
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

    use super::{LaunchOutcome, LocalCoreAdapters, LocalLaunch, WorkspaceCoreConnector};
    use crate::machine_connection::CoreError;

    /// A ready server presenting one Workspace, "Default", whose Panes run in `session_factory`.
    pub(crate) fn presenting(
        session_factory: Rc<dyn crate::terminal::TerminalSessionFactory>,
    ) -> LocalCoreAdapters {
        use crate::ui::workspaces::testing::{ScriptedEdits, ScriptedPanes};
        use crate::ui::workspaces::{WorkspaceEditSource, WorkspacePaneSessions};
        use spaceterm_core::hierarchy::{Inventory, Pane, Side, Tab, Workspace};

        struct Presenting(Rc<dyn crate::terminal::TerminalSessionFactory>);
        impl WorkspaceCoreConnector for Presenting {
            fn connect(
                &self,
                _: crate::platform::local_filesystem::LocalFilesystemAuthority,
                panes: &crate::ui::pane_lifecycle::PaneConstruction,
                _: &mut gpui::App,
            ) -> Option<super::WorkspaceMachineConnection> {
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
                            worktree_membership:
                                spaceterm_core::hierarchy::WorktreeMembership::Following,
                            worktree_repository: None,
                            id: [52; 32],
                            root_pane: [53; 32],
                            panes: vec![Pane {
                                id: [53; 32],
                                source: None,
                                side: Side::Right,
                                starting_directory: std::env::temp_dir(),
                                current_directory: None,
                                repository_directory: None,
                                presentation: None,
                                observers: Default::default(),
                                confirmed_directory: None,
                                creator: None,
                            }],
                        }],
                    }],
                };
                let opener = ScriptedPanes::new(
                    crate::terminal::WorkspaceTerminalSessionFactory::new_core(Rc::clone(&self.0)),
                );
                Some(super::WorkspaceMachineConnection {
                    edits: WorkspaceEditSource::new(Rc::new(ScriptedEdits::new(Some(inventory)))),
                    sessions: WorkspacePaneSessions::with_opener(opener, panes),
                    retained: None,
                })
            }
        }
        LocalCoreAdapters {
            launch: Arc::new(ScriptedLaunch {
                ready: true,
                ..ScriptedLaunch::default()
            }),
            connector: Rc::new(Presenting(session_factory)),
        }
    }

    /// A server whose launch never answers, for windows a test does not launch.
    pub(crate) fn unanswered() -> LocalCoreAdapters {
        struct Unreachable;
        impl WorkspaceCoreConnector for Unreachable {
            fn connect(
                &self,
                _: crate::platform::local_filesystem::LocalFilesystemAuthority,
                _: &crate::ui::pane_lifecycle::PaneConstruction,
                _: &mut gpui::App,
            ) -> Option<super::WorkspaceMachineConnection> {
                None
            }
        }
        LocalCoreAdapters {
            launch: Arc::new(ScriptedLaunch::default()),
            connector: Rc::new(Unreachable),
        }
    }

    /// A launch whose answers a test queues and whose explicit steps it records.
    #[derive(Default)]
    pub(crate) struct ScriptedLaunch {
        /// Answers every run with Ready once the queue is empty, instead of never answering.
        ready: bool,
        runs: Mutex<VecDeque<Result<LaunchOutcome, CoreError>>>,
        steps: Mutex<Vec<&'static str>>,
        start: Mutex<Option<CoreError>>,
        authorize: Mutex<Option<CoreError>>,
        /// Whether a verified build can update a refused server.
        update: Mutex<bool>,
        /// Each background item read in order. It is on once the queue is empty.
        background_item_off: Mutex<VecDeque<bool>>,
    }

    impl ScriptedLaunch {
        /// Offers a verified server build when a run refuses the server.
        pub(crate) fn offer_update(&self) {
            *self.update.lock().unwrap() = true;
        }
        pub(crate) fn answer(&self, result: Result<LaunchOutcome, CoreError>) {
            self.runs.lock().unwrap().push_back(result);
        }
        /// Answers the next background item reads with `off`, in order.
        pub(crate) fn background_item(&self, off: impl IntoIterator<Item = bool>) {
            self.background_item_off.lock().unwrap().extend(off);
        }
        pub(crate) fn refuse_start(&self, error: CoreError) {
            *self.start.lock().unwrap() = Some(error);
        }
        /// Answers the next authorization with `error`. `AuthorizationRequired` is a prompt
        /// that was cancelled or refused.
        pub(crate) fn refuse_authorization(&self, error: CoreError) {
            *self.authorize.lock().unwrap() = Some(error);
        }
        /// Every step in order: `run`, `start`, `authorize`, `wait_for_service`,
        /// `update_offered`, and `open_login_items`.
        pub(crate) fn steps(&self) -> Vec<&'static str> {
            self.steps.lock().unwrap().clone()
        }
    }

    impl LocalLaunch for ScriptedLaunch {
        fn run(&self, _: &(dyn Fn() -> bool + Sync)) -> Result<LaunchOutcome, CoreError> {
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
        fn start(&self) -> Result<(), CoreError> {
            self.steps.lock().unwrap().push("start");
            self.start.lock().unwrap().take().map_or(Ok(()), Err)
        }
        fn authorize(&self) -> Result<(), CoreError> {
            self.steps.lock().unwrap().push("authorize");
            self.authorize.lock().unwrap().take().map_or(Ok(()), Err)
        }
        fn wait_for_service(&self, _: &(dyn Fn() -> bool + Sync)) -> Result<(), CoreError> {
            self.steps.lock().unwrap().push("wait_for_service");
            Ok(())
        }
        fn update_offered(&self) -> bool {
            self.steps.lock().unwrap().push("update_offered");
            *self.update.lock().unwrap()
        }
        fn background_item_off(&self) -> bool {
            self.background_item_off
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(false)
        }
        fn open_login_items(&self) {
            self.steps.lock().unwrap().push("open_login_items");
        }
    }
}
