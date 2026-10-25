//! Add Server: the one journey that adds a remote server, by SSH setup or by an invitation link.
//!
//! Every entry point, such as the server picker, the All Servers list, Settings Search, the
//! application menu or an invitation link the operating system delivered, starts here. Filling in
//! a value sets up and joins nothing; the person chooses Set Up or Join first.
//!
//! Both methods end with one saved paired connection. SSH setup signs in once with the host
//! alone, asks the person before it installs or starts the server or changes its network access,
//! enrolls this device and closes its SSH resources. Setup waits while the sheet shows a step, and
//! goes on or stops with the person's answer. The network address defaults to the one SSH reaches;
//! the person enters another only when this computer reaches the server differently. Setup
//! succeeds only once this device is admitted over the network, and every later operation uses
//! that connection. Joining follows the direct connections registry, which owns the enrollment
//! and its exact cleanup. The invitation link stays in the obscured field until Join hands it to
//! the registry once; the sheet keeps only the server name and address it names.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Context, ElementId, Entity, Global, SharedString,
    Subscription, Task, WeakEntity, Window, WindowId, accesskit, div,
};
use spaceterm_ui::{
    Button, ButtonShape, ButtonSize, ButtonVariant, Dialog, DialogActionRequest,
    DialogCloseDecision, DialogCompletion, DialogInitialFocus, DialogOutcome,
    DialogPendingCompletion, DialogSize, FieldState, Icon, IconName, Menu, MenuEntry, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLifecycleEvent,
    ProgressRing, ProgressSize, ProgressState, SegmentedControl, SegmentedOption, TextInput,
    TextInputContentMode, TextInputEscapeBehavior, TextInputEvent, TextInputReturnBehavior,
    TextInputVariant,
};
use zeroize::Zeroizing;

use super::direct_join::{WITHDRAW_UNSAVED, join_failure, join_paused};
use super::server_list::ServerChoice;
use super::{AddServerRequest, SettingsWindow};
use crate::cancellation::CancellationToken;
use crate::direct_connections::{DirectConnections, DirectRouteId, JoinId, JoinStatus};
use crate::domain::SshDestination;
use crate::server_connection::direct::{DEFAULT_PORT, DirectError, Invitation};
use crate::server_connection::pane::control::ClientName;
use crate::ssh::provisioning::{
    SetupAddress, SetupFailure, SetupOffer, SetupPhase, SetupProgress, SetupRequest,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{code_speech, code_text};
use crate::ui::direct_servers::DirectChanges;
use crate::ui::ssh_setup::{
    PendingApproval, SetupProblem, SshHostSource, SshPromptActivity, SshPromptEvent,
    SshSetupFactory,
};

pub(super) const MODAL_ID: &str = "add-server";
pub(super) const METHOD_SELECTOR: &str = "add-server-method";
pub(super) const HOST_SELECTOR: &str = "add-server-host";
pub(super) const ADDRESS_SELECTOR: &str = "add-server-address";
/// The disclosure that shows or hides the optional network address.
pub(super) const ADDRESS_DISCLOSURE_SELECTOR: &str = "add-server-address-disclosure";
pub(super) const HOSTS_MENU_SELECTOR: &str = "add-server-ssh-hosts";
/// Prefixes each configured host in the SSH Hosts menu, followed by its position.
pub(super) const HOST_CHOICE_SELECTOR: &str = "add-server-ssh-host";
pub(super) const LINK_SELECTOR: &str = "add-server-link";
pub(super) const PASTE_SELECTOR: &str = "add-server-paste";
pub(super) const PREVIEW_SELECTOR: &str = "add-server-preview";
pub(super) const PROBLEM_SELECTOR: &str = "add-server-problem";
pub(super) const STATUS_SELECTOR: &str = "add-server-status";
pub(super) const CODE_SELECTOR: &str = "add-server-code";
pub(super) const DEFERRED_SELECTOR: &str = "add-server-deferred";

/// What the host field's description says. It is also the field's accessibility hint.
const HOST_DESCRIPTION: &str =
    "Sign in with your SSH account to install, start and pair a SpaceTerm Server.";
/// What the disclosure says while the address is hidden. It is also its accessibility hint.
const ADDRESS_DISCLOSURE_DESCRIPTION: &str =
    "Optional. Use when this computer reaches the server at a different name or address than SSH.";
const HOST_INVALID: &str = "Enter an SSH host or alias without spaces or command options.";
const ADDRESS_INVALID: &str =
    "Enter a host name or IP address, optionally followed by a port, such as studio.local:9231.";
const LINK_DESCRIPTION: &str = "Paste the invitation link from the other computer.";
/// How long the host, address and link fields are, so a long value scrolls inside the field.
const FIELD_WIDTH: f32 = 260.0;

/// What the address field's description says, naming the port SpaceTerm uses unless one is given.
fn address_description() -> SharedString {
    format!(
        "The name or IP address this computer uses to reach the server. Leave it empty to use the one SSH reaches. Port {DEFAULT_PORT} unless you add one."
    )
    .into()
}

/// The two ways to add a server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Method {
    Ssh,
    Invitation,
}

/// What the sheet offers as a button. Each stage presents some of these.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SheetAction {
    SetUp,
    Join,
    Install,
    Start,
    AllowNetwork,
    TryAgain,
    Back,
    StartAgain,
    Withdraw,
    Cancel,
    Close,
}

impl SheetAction {
    const fn label(self) -> &'static str {
        match self {
            Self::SetUp => "Set Up",
            Self::Join => "Join",
            Self::Install => "Install",
            Self::Start => "Start Server",
            Self::AllowNetwork => "Allow",
            Self::TryAgain => "Try Again",
            Self::Back => "Back",
            Self::StartAgain => "Start Again",
            Self::Withdraw => "Cancel Pairing",
            Self::Cancel => "Cancel",
            Self::Close => "Close",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::SetUp => "add-server-set-up",
            Self::Join => "add-server-join",
            Self::Install => "add-server-install",
            Self::Start => "add-server-start",
            Self::AllowNetwork => "add-server-allow-network",
            Self::TryAgain => "add-server-try-again",
            Self::Back => "add-server-back",
            Self::StartAgain => "add-server-start-again",
            Self::Withdraw => "add-server-withdraw",
            Self::Cancel => "add-server-cancel",
            Self::Close => "add-server-close",
        }
    }

    fn modal(self, enabled: bool) -> ModalAction<Self> {
        let role = match self {
            Self::Cancel | Self::Close => ModalActionRole::Cancel,
            Self::Back => ModalActionRole::Auxiliary,
            _ => ModalActionRole::Affirmative,
        };
        let action = ModalAction::new(self, self.label(), role, self.selector()).enabled(enabled);
        match self {
            Self::SetUp
            | Self::Join
            | Self::Install
            | Self::Start
            | Self::AllowNetwork
            | Self::TryAgain
            | Self::StartAgain => action
                .with_emphasis(ModalActionEmphasis::Prominent)
                .default_action(true),
            Self::Withdraw => action.with_intent(ModalActionIntent::Destructive),
            _ => action,
        }
    }
}

/// Where the sheet's first keyboard focus goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusTarget {
    Host,
    Address,
    Link,
    Action(SheetAction),
}

/// The title and actions one stage presents. A different shape replaces the sheet on screen.
/// Which actions are available is not part of the shape: it changes in place as the person types.
#[derive(Clone, Debug, PartialEq)]
struct Shape {
    title: SharedString,
    actions: Vec<SheetAction>,
    focus: FocusTarget,
}

/// What an invitation link names, read without connecting. It holds no part of the secret.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Preview {
    server: String,
    address: String,
}

impl Preview {
    /// Reads the server and address `text` names. Parsing checks the whole link, its pinned
    /// identity and its expiry, and opens nothing.
    fn read(text: &str) -> Result<Self, DirectError> {
        let invitation = Invitation::parse(text)?;
        Ok(Self {
            server: invitation.server_name().to_owned(),
            address: address(invitation.endpoint()).to_owned(),
        })
    }
}

/// The address an endpoint such as `wss://192.168.1.20:9231/spaceterm` reaches, as a person reads
/// it: `192.168.1.20:9231`.
fn address(endpoint: &str) -> &str {
    let rest = endpoint.strip_prefix("wss://").unwrap_or(endpoint);
    rest.split('/').next().unwrap_or(rest)
}

/// Why a pasted link can't be used. It says only what the link itself shows.
fn link_problem(error: DirectError) -> &'static str {
    match error {
        DirectError::Expired => "Invitation expired. Ask for a new one.",
        _ => "Invalid SpaceTerm invitation link.",
    }
}

/// One SSH setup the person asked for. `address` is the network address they entered, or `None`
/// to use the one SSH reaches.
#[derive(Clone, Debug)]
struct Setup {
    destination: SshDestination,
    address: Option<SetupAddress>,
}

/// Where adding a server stands.
enum Stage {
    /// Choosing SSH or Invitation and filling in its value.
    Form,
    /// SSH setup underway. `phase` is the last step it reported. `offer` is a step setup waits on
    /// until the person answers it. `cancelling` holds the sheet until setup has closed every SSH
    /// resource it opened.
    SettingUp {
        setup: Setup,
        phase: Option<SetupPhase>,
        offer: Option<PendingApproval>,
        cancel: CancellationToken,
        cancelling: bool,
    },
    /// Setup stopped with a state the person can read and maybe retry. `unreachable` is the
    /// address this computer couldn't reach the server at, which Back offers for editing.
    SetupProblem {
        setup: Setup,
        problem: SetupProblem,
        unreachable: Option<SetupAddress>,
    },
    /// Joining the server an invitation names. `join` is `None` until the registry accepts it.
    Joining {
        join: Option<JoinId>,
        server: String,
    },
    /// The registry refused to start the join.
    JoinRefused { server: String, error: DirectError },
    /// The person stopped the join, and SpaceTerm couldn't save that on this computer yet.
    StopUnsaved { server: String },
    /// A stop SpaceTerm couldn't save at first, saved since.
    Stopped { server: String },
    /// The sheet finished; nothing more is presented.
    Done,
}

/// The Add Server sheets open in each window, kept while they have work to finish.
#[derive(Default)]
struct OpenSheets(HashMap<WindowId, Entity<AddServerSheet>>);

impl Global for OpenSheets {}

/// The window's sheet, unless it already finished and is only waiting to be released.
fn open_sheet(window: &Window, cx: &App) -> Option<Entity<AddServerSheet>> {
    cx.try_global::<OpenSheets>()
        .and_then(|sheets| sheets.0.get(&window.window_handle().window_id()))
        .filter(|sheet| !sheet.read(cx).closed)
        .cloned()
}

fn retain_sheet(window: &Window, sheet: Entity<AddServerSheet>, cx: &mut App) {
    if cx.try_global::<OpenSheets>().is_none() {
        cx.set_global(OpenSheets::default());
        // A closed window ends its sheet, which cleans up what it started.
        cx.on_window_closed(|cx, id| {
            let closed = cx.global_mut::<OpenSheets>().0.remove(&id);
            drop(closed);
        })
        .detach();
    }
    cx.global_mut::<OpenSheets>()
        .0
        .insert(window.window_handle().window_id(), sheet);
}

impl SettingsWindow {
    /// Opens Add Server with `request`'s value filled in and keyboard focus on it. A sheet that is
    /// already open takes the value when it is waiting for one, and keeps its work otherwise.
    pub(super) fn begin_add_server(
        &mut self,
        request: AddServerRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(sheet) = open_sheet(window, cx) {
            sheet.update(cx, |sheet, cx| sheet.fill(request, window, cx));
            return;
        }
        let settings = cx.weak_entity();
        let sheet = cx.new(|cx| AddServerSheet::new(settings, window, cx));
        retain_sheet(window, sheet.clone(), cx);
        sheet.update(cx, |sheet, cx| sheet.fill(request, window, cx));
    }
}

/// The Add Server sheet's content and state. It is the body of each stage it presents.
pub(super) struct AddServerSheet {
    settings: WeakEntity<SettingsWindow>,
    window: AnyWindowHandle,
    ssh: Option<std::rc::Rc<dyn SshSetupFactory>>,
    direct: Option<DirectConnections>,
    method: Method,
    host: Entity<TextInput>,
    address: Entity<TextInput>,
    link: Entity<TextInput>,
    /// The hosts this computer's SSH configuration names. Empty until discovery, which starts
    /// when the sheet opens, answers.
    hosts: Vec<SshDestination>,
    /// Whether the typed host was refused when the person last chose Set Up.
    host_invalid: bool,
    /// Whether the typed address was refused when the person last chose Set Up.
    address_invalid: bool,
    /// Whether the optional network address is shown.
    address_shown: bool,
    /// What the link in the field names, or why it can't be used. `None` while it is empty.
    preview: Option<Result<Preview, DirectError>>,
    stage: Stage,
    /// Counts setup attempts, so the result of one this sheet replaced lands nowhere.
    attempt: u64,
    /// Whether the person dismissed an SSH prompt in the attempt underway, which returns to the
    /// form once setup has stopped.
    declined: bool,
    prompts: Entity<SshPromptActivity>,
    /// The shape on screen, if any.
    shown: Option<Shape>,
    presentation: Option<DialogCompletion>,
    /// Whether this sheet's presentation is the window's active modal rather than queued.
    active: bool,
    /// Counts presented shapes, so the result of one this sheet replaced lands nowhere.
    surface: u64,
    /// A close the sheet holds open while it saves the person's stop.
    pending: Option<DialogPendingCompletion>,
    closed: bool,
    /// The availability of each presented action, as last applied to the presentation.
    applied: Vec<(SheetAction, bool)>,
    /// A host or link that arrived while the sheet was busy. It waits here, in memory only, and
    /// Add Server shows it next, without touching the work underway.
    deferred: Option<AddServerRequest>,
    _subscriptions: Vec<Subscription>,
    _direct_changes: Option<DirectChanges>,
    _work: Option<Task<()>>,
    _setup: Option<Task<()>>,
    _offers: Option<Task<()>>,
    _discovery: Option<Task<()>>,
}

impl AddServerSheet {
    fn new(
        settings: WeakEntity<SettingsWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = cx.new(|cx| {
            TextInput::new(HOST_SELECTOR, "Host", String::new(), window, cx)
                .variant(TextInputVariant::Bare)
                .placeholder("Host name or SSH alias")
                .input_length_limit(Some(1024))
                .return_behavior(TextInputReturnBehavior::Propagate)
                .escape_behavior(TextInputEscapeBehavior::Propagate)
                .debug_selector(HOST_SELECTOR)
        });
        let address = cx.new(|cx| {
            TextInput::new(
                ADDRESS_SELECTOR,
                "Network address",
                String::new(),
                window,
                cx,
            )
            .variant(TextInputVariant::Bare)
            .placeholder("studio.local")
            .input_length_limit(Some(270))
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .debug_selector(ADDRESS_SELECTOR)
        });
        let link = cx.new(|cx| {
            TextInput::new(LINK_SELECTOR, "Invitation link", String::new(), window, cx)
                .variant(TextInputVariant::Bare)
                .content_mode(TextInputContentMode::Obscured)
                .placeholder("spaceterm://pair/\u{2026}")
                .return_behavior(TextInputReturnBehavior::Propagate)
                .escape_behavior(TextInputEscapeBehavior::Propagate)
                .debug_selector(LINK_SELECTOR)
        });
        let prompts = crate::ui::ssh_setup::prompt_activity(window, cx);
        let mut subscriptions = vec![
            cx.subscribe_in(
                &host,
                window,
                |sheet, _, event: &TextInputEvent, window, cx| {
                    if matches!(event, TextInputEvent::ValueChanged(_)) {
                        sheet.host_invalid = false;
                        sheet.show(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &address,
                window,
                |sheet, _, event: &TextInputEvent, window, cx| {
                    if matches!(event, TextInputEvent::ValueChanged(_)) {
                        sheet.address_invalid = false;
                        sheet.show(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &link,
                window,
                |sheet, _, event: &TextInputEvent, window, cx| {
                    if matches!(event, TextInputEvent::ValueChanged(_)) {
                        sheet.read_link(cx);
                        sheet.show(window, cx);
                    }
                },
            ),
            // A sign-in prompt takes the sheet's place only while it is actually open.
            cx.subscribe_in(
                &prompts,
                window,
                |sheet, _, event: &SshPromptEvent, window, cx| {
                    let Stage::SettingUp { cancel, .. } = &sheet.stage else {
                        return;
                    };
                    if matches!(event, SshPromptEvent::Settled { cancelled: true }) {
                        // The person declined to sign in, so setup stops and the values stay
                        // for another try.
                        sheet.declined = true;
                        cancel.cancel();
                    }
                    sheet.show(window, cx);
                },
            ),
        ];
        // The sheet ends with its window too, and ends what it started with it.
        subscriptions.push(cx.on_release(|sheet, cx| sheet.abandon(cx)));
        let ssh = crate::ui::ssh_setup::installed(cx);
        let direct = crate::ui::direct_servers::installed(cx);
        let direct_changes = direct.as_ref().map(|direct| {
            DirectChanges::follow(direct, |changes| {
                cx.spawn_in(window, async move |sheet, cx| {
                    while changes.recv().await.is_ok() {
                        let followed = sheet.update_in(cx, |sheet, window, cx| {
                            sheet.observe(window, cx);
                        });
                        if followed.is_err() {
                            break;
                        }
                    }
                })
            })
        });
        let discovery = ssh
            .as_ref()
            .map(|factory| Self::discover_hosts(factory.hosts(), cx));
        Self {
            settings,
            window: window.window_handle(),
            ssh,
            direct,
            method: Method::Ssh,
            host,
            address,
            link,
            hosts: Vec::new(),
            host_invalid: false,
            address_invalid: false,
            address_shown: false,
            preview: None,
            stage: Stage::Form,
            attempt: 0,
            declined: false,
            prompts,
            shown: None,
            presentation: None,
            active: false,
            surface: 0,
            pending: None,
            closed: false,
            applied: Vec::new(),
            deferred: None,
            _subscriptions: subscriptions,
            _direct_changes: direct_changes,
            _work: None,
            _setup: None,
            _offers: None,
            _discovery: discovery,
        }
    }

    /// Reads the configured SSH hosts on a thread of their own, since a slow or unavailable
    /// volume can hold a read indefinitely, and fills the SSH Hosts menu with them once they
    /// arrive. Reading reaches no host. The hosts change only the menu, never what the person
    /// typed. A sheet that closes first takes nothing, and a read that can't start or ends
    /// without an answer leaves the menu empty, as a configuration naming no hosts does.
    fn discover_hosts(source: Arc<dyn SshHostSource>, cx: &mut Context<Self>) -> Task<()> {
        let (found, hosts) = async_channel::bounded(1);
        let started = std::thread::Builder::new()
            .name("spaceterm-ssh-hosts".to_owned())
            .spawn(move || {
                let _ = found.try_send(source.hosts());
            });
        cx.spawn(async move |sheet, cx| {
            if started.is_err() {
                return;
            }
            let Ok(hosts) = hosts.recv().await else {
                return;
            };
            let _ = sheet.update(cx, |sheet, cx| {
                if !sheet.closed {
                    sheet.hosts = hosts;
                    cx.notify();
                }
            });
        })
    }

    /// Fills in `request`'s value when the sheet is waiting for one, then presents the sheet. Work
    /// underway, such as a setup or a join, keeps its place.
    fn fill(&mut self, request: AddServerRequest, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            // The latest value waits; an empty request only brings the sheet forward.
            if matches!(
                request,
                AddServerRequest::Ssh(Some(_)) | AddServerRequest::Invitation(Some(_))
            ) {
                self.deferred = Some(request);
            }
            self.show(window, cx);
            return;
        }
        self.stage = Stage::Form;
        match request {
            AddServerRequest::Ssh(host) => {
                self.method = Method::Ssh;
                if let Some(host) = host {
                    self.host.update(cx, |input, cx| input.set_value(host, cx));
                    self.host_invalid = false;
                }
            }
            AddServerRequest::Invitation(link) => {
                self.method = Method::Invitation;
                if let Some(link) = link {
                    self.link
                        .update(cx, |input, cx| input.set_value(link.trim(), cx));
                    self.read_link(cx);
                }
            }
        }
        self.show(window, cx);
        self.focus_field(window, cx);
    }

    /// Whether the sheet has work underway that a new value must not replace.
    fn busy(&self) -> bool {
        // A stop being saved holds the sheet until it settles.
        if self.pending.is_some() {
            return true;
        }
        match &self.stage {
            Stage::SettingUp { .. } => true,
            Stage::Joining { join: None, .. } => true,
            Stage::Joining {
                join: Some(join), ..
            } => !matches!(
                self.join_status(*join),
                Some(JoinStatus::Rejected | JoinStatus::Expired | JoinStatus::Failed(_)) | None
            ),
            Stage::Form
            | Stage::SetupProblem { .. }
            | Stage::JoinRefused { .. }
            | Stage::StopUnsaved { .. }
            | Stage::Stopped { .. }
            | Stage::Done => false,
        }
    }

    fn join_status(&self, join: JoinId) -> Option<JoinStatus> {
        self.direct.as_ref()?.join_status(join)
    }

    /// The join this sheet started that is still asking, which Cancel ends exactly.
    fn live_join(&self) -> Option<JoinId> {
        self.joining(|status| {
            matches!(
                status,
                JoinStatus::Validating
                    | JoinStatus::Retryable { .. }
                    | JoinStatus::AwaitingApproval { .. }
            )
        })
    }

    /// The join this sheet started whose status satisfies `wanted`.
    fn joining(&self, wanted: impl Fn(&JoinStatus) -> bool) -> Option<JoinId> {
        let Stage::Joining {
            join: Some(join), ..
        } = &self.stage
        else {
            return None;
        };
        self.join_status(*join)
            .is_some_and(|status| wanted(&status))
            .then_some(*join)
    }

    fn read_link(&mut self, cx: &App) {
        let value = self.link.read(cx).value().trim();
        self.preview = (!value.is_empty()).then(|| Preview::read(value));
    }

    fn focus_field(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Form) {
            return;
        }
        let field = match self.method {
            Method::Ssh if self.address_invalid && !self.host_invalid => &self.address,
            Method::Ssh => &self.host,
            Method::Invitation => &self.link,
        };
        field.read(cx).focus_handle().focus(window, cx);
    }

    /// The title and actions the current stage presents, or `None` while nothing should be on
    /// screen, such as while an SSH sign-in prompt is open.
    fn shape(&self, cx: &App) -> Option<Shape> {
        let add = || SharedString::new_static("Add Server");
        let join_title = |server: &str| SharedString::from(format!("Join {server}"));
        let failure_title = |server: &str, error: DirectError| {
            if matches!(
                error,
                DirectError::WrongServer
                    | DirectError::IncompatibleProtocol
                    | DirectError::AuthenticationRefused
                    | DirectError::PinMismatch
                    | DirectError::ChangedClaim
                    | DirectError::Substituted
                    | DirectError::CredentialUnavailable
                    | DirectError::InteractionRequired
                    | DirectError::StorageUnavailable
                    | DirectError::Capacity
                    | DirectError::Transport
            ) {
                format!("Couldn't Pair With {server}").into()
            } else {
                join_title(server)
            }
        };
        let quoted =
            |destination: &SshDestination| format!("\u{201C}{}\u{201D}", destination.as_str());
        let shape = match &self.stage {
            Stage::Form => {
                let (primary, focus) = match self.method {
                    Method::Ssh => (SheetAction::SetUp, FocusTarget::Host),
                    Method::Invitation => (SheetAction::Join, FocusTarget::Link),
                };
                let focus = match focus {
                    FocusTarget::Host if self.address_invalid && !self.host_invalid => {
                        FocusTarget::Address
                    }
                    focus => focus,
                };
                Shape {
                    title: add(),
                    actions: vec![primary, SheetAction::Cancel],
                    focus,
                }
            }
            Stage::SettingUp {
                setup,
                offer: Some(pending),
                cancelling: false,
                ..
            } => {
                if self.prompts.read(cx).active {
                    return None;
                }
                let (title, action) = match &pending.offer {
                    SetupOffer::Installation { .. } => (
                        format!(
                            "Install SpaceTerm Server on {}?",
                            quoted(&setup.destination)
                        ),
                        SheetAction::Install,
                    ),
                    SetupOffer::Startup => (
                        format!("Start SpaceTerm Server on {}?", quoted(&setup.destination)),
                        SheetAction::Start,
                    ),
                    SetupOffer::Network { .. } => (
                        format!("Allow Network Access on {}?", quoted(&setup.destination)),
                        SheetAction::AllowNetwork,
                    ),
                };
                Shape {
                    title: title.into(),
                    actions: vec![action, SheetAction::Cancel],
                    focus: FocusTarget::Action(action),
                }
            }
            Stage::SettingUp { cancelling, .. } => {
                if self.prompts.read(cx).active {
                    return None;
                }
                Shape {
                    title: if *cancelling {
                        "Cancelling Setup".into()
                    } else {
                        add()
                    },
                    actions: vec![SheetAction::Cancel],
                    focus: FocusTarget::Action(SheetAction::Cancel),
                }
            }
            Stage::SetupProblem { setup, problem, .. } => {
                let mut actions = Vec::with_capacity(3);
                if problem.retry {
                    actions.push(SheetAction::TryAgain);
                }
                actions.push(SheetAction::Back);
                actions.push(SheetAction::Cancel);
                Shape {
                    title: format!("Couldn\u{2019}t Add {}", quoted(&setup.destination)).into(),
                    actions,
                    focus: FocusTarget::Action(if problem.retry {
                        SheetAction::TryAgain
                    } else {
                        SheetAction::Back
                    }),
                }
            }
            Stage::Joining { join, server } => {
                let status = join.and_then(|join| self.join_status(join));
                let (actions, focus) = match status {
                    Some(JoinStatus::Retryable { .. }) => (
                        vec![SheetAction::TryAgain, SheetAction::Cancel],
                        SheetAction::TryAgain,
                    ),
                    Some(JoinStatus::PublicationUnconfirmed) => (
                        vec![SheetAction::Withdraw, SheetAction::Close],
                        SheetAction::Close,
                    ),
                    Some(JoinStatus::Rejected | JoinStatus::Expired | JoinStatus::Failed(_)) => (
                        vec![SheetAction::StartAgain, SheetAction::Close],
                        SheetAction::StartAgain,
                    ),
                    _ => (vec![SheetAction::Cancel], SheetAction::Cancel),
                };
                Shape {
                    title: match status {
                        Some(JoinStatus::Retryable { .. }) => "Pairing Paused".into(),
                        Some(JoinStatus::Failed(error)) => failure_title(server, error),
                        _ => join_title(server),
                    },
                    actions,
                    focus: FocusTarget::Action(focus),
                }
            }
            Stage::JoinRefused { server, error } => Shape {
                title: failure_title(server, *error),
                actions: vec![SheetAction::StartAgain, SheetAction::Close],
                focus: FocusTarget::Action(SheetAction::StartAgain),
            },
            Stage::StopUnsaved { .. } => Shape {
                title: "Cancellation Pending".into(),
                actions: vec![SheetAction::Close],
                focus: FocusTarget::Action(SheetAction::Close),
            },
            Stage::Stopped { server } => Shape {
                title: format!("Cancelling Pairing With {server}").into(),
                actions: vec![SheetAction::Close],
                focus: FocusTarget::Action(SheetAction::Close),
            },
            Stage::Done => return None,
        };
        Some(shape)
    }

    /// Presents the shape the stage needs, replacing the one on screen when it differs.
    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A held close settles first; the stage it shows next follows its answer.
        if self.closed || self.pending.is_some() {
            cx.notify();
            return;
        }
        let wanted = self.shape(cx);
        if wanted == self.shown {
            self.sync_availability(window, cx);
            cx.notify();
            return;
        }
        self.surface = self.surface.wrapping_add(1);
        let Some(shape) = wanted else {
            self.shown = None;
            self.active = false;
            if let Some(presentation) = self.presentation.take() {
                let _ = presentation.dismiss(window, cx);
            }
            cx.notify();
            return;
        };
        // A queued presentation can't be replaced in place, so it gives up its place.
        if !self.active
            && let Some(presentation) = self.presentation.take()
        {
            let _ = presentation.dismiss(window, cx);
        }
        let applied = shape
            .actions
            .iter()
            .map(|action| (*action, self.available(*action, cx)))
            .collect();
        let presented = self.present(&shape, window, cx);
        match presented {
            Some(presentation) => {
                self.presentation = Some(presentation);
                self.shown = Some(shape);
                self.applied = applied;
            }
            None => {
                // A sheet that can't present its next stage ends, and takes the stage still on
                // screen with it, so no presentation outlives the sheet that owns it.
                self.shown = None;
                self.active = false;
                if let Some(presentation) = self.presentation.take() {
                    let _ = presentation.dismiss(window, cx);
                }
                self.abandon(cx);
                self.release(cx);
            }
        }
        cx.notify();
    }

    /// Whether `action` can be chosen now. Set Up waits for a host and Join for a link. Cancel stays
    /// available while setup is cancelling, so it keeps keyboard focus; choosing it again changes
    /// nothing.
    fn available(&self, action: SheetAction, cx: &App) -> bool {
        match action {
            // The host is enough: the network address is optional.
            SheetAction::SetUp => {
                self.ssh.is_some() && !self.host.read(cx).value().trim().is_empty()
            }
            SheetAction::Join => self.direct.is_some() && matches!(self.preview, Some(Ok(_))),
            _ => true,
        }
    }

    /// Brings the presented actions' availability up to date in place. The sheet stays on screen,
    /// so the field keeps its focus and caret.
    fn sync_availability(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(shape), Some(presentation)) = (&self.shown, self.presentation.clone()) else {
            return;
        };
        let changes: Vec<(SheetAction, bool)> = shape
            .actions
            .iter()
            .map(|action| (*action, self.available(*action, cx)))
            .filter(|change| !self.applied.contains(change))
            .collect();
        for (action, available) in changes {
            if presentation
                .set_action_enabled(&action, available, window, cx)
                .is_ok()
            {
                self.applied.retain(|(applied, _)| *applied != action);
                self.applied.push((action, available));
            }
        }
    }

    fn present(
        &mut self,
        shape: &Shape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<DialogCompletion> {
        let surface = self.surface;
        let focus = match shape.focus {
            FocusTarget::Host => DialogInitialFocus::Body(self.host.read(cx).focus_handle()),
            FocusTarget::Address => DialogInitialFocus::Body(self.address.read(cx).focus_handle()),
            FocusTarget::Link => DialogInitialFocus::Body(self.link.read(cx).focus_handle()),
            FocusTarget::Action(action) => DialogInitialFocus::Action(action),
        };
        let dialog = Dialog::new(
            ModalId::new(MODAL_ID),
            shape.title.clone(),
            shape.title.clone(),
            shape
                .actions
                .iter()
                .map(|action| action.modal(self.available(*action, cx)))
                .collect(),
            focus,
        )
        .size(DialogSize::Wide)
        .body(cx.entity());
        let acting = cx.entity();
        let on_action = move |request: DialogActionRequest<SheetAction>,
                              pending: DialogPendingCompletion,
                              cx: &mut App| {
            let action = *request.action_id();
            acting.update(cx, |sheet, cx| sheet.decide(surface, action, pending, cx))
        };
        let closing = cx.entity();
        let on_result = move |_: DialogOutcome<SheetAction>, cx: &mut App| {
            closing.update(cx, |sheet, cx| {
                if sheet.surface == surface {
                    sheet.shown = None;
                    sheet.presentation = None;
                    sheet.active = false;
                    sheet.abandon(cx);
                    sheet.release(cx);
                }
            });
        };
        let observing = cx.weak_entity();
        let on_lifecycle = move |event: &ModalLifecycleEvent, cx: &mut App| {
            let _ = observing.update(cx, |sheet, _| {
                let current = sheet
                    .presentation
                    .as_ref()
                    .map(DialogCompletion::presentation_id);
                match event {
                    ModalLifecycleEvent::Opened(id) if Some(*id) == current => sheet.active = true,
                    ModalLifecycleEvent::Closed(id, _) if Some(*id) == current => {
                        sheet.active = false;
                    }
                    _ => {}
                }
            });
        };
        let presented = if self.active {
            dialog.replace_active(window, cx, on_action, on_result, on_lifecycle)
        } else {
            dialog.present_with_lifecycle(window, cx, on_action, on_result, on_lifecycle)
        };
        if presented.is_err() {
            eprintln!("failed to present Add Server");
        }
        presented.ok()
    }

    /// Answers one action. Cancel and Close end the sheet, holding it open only while a stop
    /// saves or setup closes what it opened; every other action moves to its next stage once
    /// this answer unwinds.
    fn decide(
        &mut self,
        surface: u64,
        action: SheetAction,
        pending: DialogPendingCompletion,
        cx: &mut Context<Self>,
    ) -> DialogCloseDecision {
        let deny = DialogCloseDecision::Deny {
            first_invalid: None,
        };
        if surface != self.surface {
            return deny;
        }
        match action {
            // Setup ends truthfully: the sheet says it is cancelling until setup has stopped.
            SheetAction::Cancel if matches!(self.stage, Stage::SettingUp { .. }) => {
                if let Stage::SettingUp {
                    cancel,
                    cancelling,
                    offer,
                    ..
                } = &mut self.stage
                {
                    cancel.cancel();
                    *cancelling = true;
                    // Cancel declines a step setup waits on, so it changes nothing more.
                    if let Some(pending) = offer.take() {
                        pending.answer(false);
                    }
                }
                let sheet = cx.entity();
                let window = self.window;
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        sheet.update(cx, |sheet, cx| sheet.show(window, cx));
                    });
                });
                deny
            }
            SheetAction::Cancel | SheetAction::Close | SheetAction::Withdraw => {
                // Only the person's Withdraw asks the server to remove a pairing whose save
                // SpaceTerm couldn't confirm; closing withdraws nothing.
                let stopping = if action == SheetAction::Withdraw {
                    self.joining(|status| matches!(status, JoinStatus::PublicationUnconfirmed))
                } else {
                    self.live_join()
                };
                if let Some(join) = stopping {
                    self.pending = Some(pending);
                    self.stop_join(join, cx);
                    return DialogCloseDecision::Pending;
                }
                if action == SheetAction::Withdraw {
                    return deny;
                }
                self.abandon(cx);
                DialogCloseDecision::Allow
            }
            action => {
                let sheet = cx.entity();
                let window = self.window;
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        sheet.update(cx, |sheet, cx| sheet.perform(action, window, cx));
                    });
                });
                deny
            }
        }
    }

    fn perform(&mut self, action: SheetAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            SheetAction::SetUp => self.set_up_typed(window, cx),
            SheetAction::Join => self.join(window, cx),
            // The person approves exactly the step setup waits on, and setup goes on.
            SheetAction::Install | SheetAction::Start | SheetAction::AllowNetwork => {
                if let Stage::SettingUp {
                    offer: offer @ Some(_),
                    cancelling: false,
                    ..
                } = &mut self.stage
                    && let Some(pending) = offer.take()
                {
                    pending.answer(true);
                }
                self.show(window, cx);
            }
            SheetAction::TryAgain => match &self.stage {
                Stage::SetupProblem { setup, .. } => {
                    let setup = setup.clone();
                    self.set_up(setup, window, cx);
                }
                Stage::Joining {
                    join: Some(join), ..
                } => {
                    // Only the paused join's own request asks again.
                    if let Some(direct) = &self.direct {
                        let _ = direct.retry_join(*join);
                    }
                    self.show(window, cx);
                }
                _ => {}
            },
            SheetAction::Back => {
                // An address this computer couldn't reach comes back for editing.
                if let Stage::SetupProblem {
                    unreachable: Some(address),
                    ..
                } = &self.stage
                {
                    let authority = address.authority();
                    self.address_shown = true;
                    if self.address.read(cx).value().trim().is_empty() {
                        self.address
                            .update(cx, |input, cx| input.set_value(authority, cx));
                    }
                }
                self.back_to_form(window, cx);
            }
            SheetAction::StartAgain => {
                if let Some(request) = self.deferred.take() {
                    self.stage = Stage::Form;
                    self.fill(request, window, cx);
                    return;
                }
                self.stage = Stage::Form;
                self.method = Method::Invitation;
                self.link.update(cx, |input, cx| {
                    input.clear(cx);
                });
                self.preview = None;
                self.show(window, cx);
                self.focus_field(window, cx);
            }
            SheetAction::Withdraw | SheetAction::Cancel | SheetAction::Close => {}
        }
    }

    fn set_method(&mut self, method: Method, window: &mut Window, cx: &mut Context<Self>) {
        if self.method == method || !matches!(self.stage, Stage::Form) {
            return;
        }
        self.method = method;
        self.show(window, cx);
        self.focus_field(window, cx);
    }

    /// Returns to the form with the host and address still typed.
    fn back_to_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage = Stage::Form;
        if let Some(request) = self.deferred.take() {
            self.fill(request, window, cx);
            return;
        }
        self.method = Method::Ssh;
        self.show(window, cx);
        self.focus_field(window, cx);
    }

    /// Sets up the host the person typed, and the network address when they entered one. The
    /// host passes to SSH as one destination, so SSH configuration still decides the host, user
    /// and keys.
    fn set_up_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.host.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        let destination = SshDestination::new(typed).ok();
        let entered = self.address.read(cx).value().trim().to_owned();
        let address = if entered.is_empty() {
            Ok(None)
        } else {
            SetupAddress::parse(&entered).map(Some)
        };
        self.host_invalid = destination.is_none();
        self.address_invalid = address.is_err();
        if self.address_invalid {
            self.address_shown = true;
        }
        let (Some(destination), Ok(address)) = (destination, address) else {
            self.show(window, cx);
            self.focus_field(window, cx);
            cx.notify();
            return;
        };
        self.set_up(
            Setup {
                destination,
                address,
            },
            window,
            cx,
        );
    }

    /// Runs one setup attempt off the window's thread. Its OpenSSH prompts appear in this
    /// window, and its phases and outcome return here.
    fn set_up(&mut self, setup: Setup, window: &mut Window, cx: &mut Context<Self>) {
        let Some(factory) = self.ssh.clone() else {
            return;
        };
        let (Some(backend), Some(prompts)) = (factory.backend(), factory.prompts(window, cx))
        else {
            self.stage = Stage::SetupProblem {
                problem: crate::ui::ssh_setup::problem(&SetupFailure::Unavailable(
                    crate::ssh::provisioning::UnavailableKind::SshFailed,
                )),
                setup,
                unreachable: None,
            };
            self.show(window, cx);
            return;
        };
        self.attempt = self.attempt.wrapping_add(1);
        self.declined = false;
        let attempt = self.attempt;
        let cancel = CancellationToken::default();
        let (approvals, offers) = crate::ui::ssh_setup::window_approvals();
        let request = SetupRequest {
            destination: setup.destination.clone(),
            address: setup.address.clone(),
            name: ClientName::this_computer(),
            prompts,
            approvals,
        };
        self.stage = Stage::SettingUp {
            setup,
            phase: None,
            offer: None,
            cancel: cancel.clone(),
            cancelling: false,
        };
        let (phases, reported) = async_channel::unbounded();
        let progress = SetupProgress::new(move |phase| {
            let _ = phases.try_send(phase);
        });
        let following = cx.spawn(async move |sheet, cx| {
            while let Ok(phase) = reported.recv().await {
                let followed = sheet.update(cx, |sheet, cx| {
                    if sheet.attempt == attempt
                        && let Stage::SettingUp { phase: shown, .. } = &mut sheet.stage
                    {
                        *shown = Some(phase);
                        cx.notify();
                    }
                });
                if followed.is_err() {
                    break;
                }
            }
        });
        let window_handle = self.window;
        // Each step setup asks about waits in the sheet until the person answers it.
        self._offers = Some(cx.spawn(async move |sheet, cx| {
            while let Ok(pending) = offers.recv().await {
                let shown = window_handle.update(cx, |_, window, cx| {
                    sheet.update(cx, |sheet, cx| sheet.offered(attempt, pending, window, cx))
                });
                if !matches!(shown, Ok(Ok(()))) {
                    break;
                }
            }
        }));
        // Setup holds SSH for as long as the person takes to answer its prompts and offers, so it
        // runs on a thread of its own rather than occupying a shared worker.
        let (finished, outcome) = async_channel::bounded(1);
        let started = std::thread::Builder::new()
            .name("spaceterm-ssh-setup".to_owned())
            .spawn(move || {
                let _ = finished.try_send(backend.setup(request, &cancel, &progress));
            });
        if started.is_err() {
            // Nothing reached the other computer, so the person can try again as it was.
            if let Stage::SettingUp { setup, .. } = std::mem::replace(&mut self.stage, Stage::Form)
            {
                self.stage = Stage::SetupProblem {
                    setup,
                    problem: crate::ui::ssh_setup::start_failure(),
                    unreachable: None,
                };
            }
            self.show(window, cx);
            return;
        }
        self._setup = Some(cx.spawn(async move |sheet, cx| {
            // A setup that ended without an answer may have changed the server, so it is
            // reported as unconfirmed rather than as never started.
            let outcome = outcome
                .recv()
                .await
                .unwrap_or(Err(SetupFailure::Unconfirmed(
                    crate::ssh::provisioning::SetupOperation::Enrollment,
                )));
            drop(following);
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = sheet.update(cx, |sheet, cx| {
                    sheet.set_up_ended(attempt, outcome, window, cx)
                });
            });
        }));
        self.show(window, cx);
    }

    /// Shows the step setup attempt `attempt` waits on, until the person answers it. An offer for
    /// an attempt the sheet no longer follows, or one cancelling, is declined.
    fn offered(
        &mut self,
        attempt: u64,
        pending: PendingApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &mut self.stage {
            Stage::SettingUp {
                offer,
                cancelling: false,
                ..
            } if !self.closed && attempt == self.attempt => {
                *offer = Some(pending);
                self.show(window, cx);
            }
            _ => pending.answer(false),
        }
    }

    /// Follows the outcome of setup attempt `attempt`.
    fn set_up_ended(
        &mut self,
        attempt: u64,
        outcome: Result<DirectRouteId, SetupFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closed || attempt != self.attempt || !matches!(self.stage, Stage::SettingUp { .. })
        {
            return;
        }
        let Stage::SettingUp {
            setup, cancelling, ..
        } = std::mem::replace(&mut self.stage, Stage::Form)
        else {
            return;
        };
        // A setup that finished admits the server whether or not the person asked to cancel.
        let outcome = match outcome {
            Ok(route) => {
                self.succeed(ServerChoice::Direct(route), window, cx);
                return;
            }
            Err(failure) => failure,
        };
        // Setup reports Cancelled only when it stopped before changing the other computer, so
        // only then does the sheet close or return to the form. A step that had already run, such
        // as an installation or a start, reports its own outcome even when the person declined a
        // later prompt or chose Cancel.
        if matches!(outcome, SetupFailure::Cancelled) {
            if cancelling {
                self.finish(window, cx);
            } else {
                // The person declined an SSH prompt, or the attempt ended elsewhere.
                self.back_to_form(window, cx);
            }
            return;
        }
        // A declined sign-in can end SSH itself before setup sees the cancellation. That failure
        // changed nothing, so the person returns to the form as for any other decline.
        if self.declined
            && !cancelling
            && outcome
                == SetupFailure::Unavailable(crate::ssh::provisioning::UnavailableKind::SshFailed)
        {
            self.back_to_form(window, cx);
            return;
        }
        let unreachable = match &outcome {
            SetupFailure::Unreachable { address } => Some(address.clone()),
            _ => None,
        };
        self.stage = Stage::SetupProblem {
            problem: crate::ui::ssh_setup::problem(&outcome),
            setup,
            unreachable,
        };
        // A cancellation that arrived too late to stop the step still reports what happened.
        self.show(window, cx);
    }

    /// Hands the link to the registry once. The field empties as it does, so the link lives on
    /// only in the registry's own join.
    fn join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(direct) = self.direct.clone() else {
            return;
        };
        let Some(Ok(preview)) = self.preview.take() else {
            return;
        };
        let taken = Zeroizing::new(self.link.update(cx, |input, cx| input.take_value(cx)));
        let text = Zeroizing::new(taken.trim().to_owned());
        drop(taken);
        let server = preview.server;
        self.stage = Stage::Joining {
            join: None,
            server: server.clone(),
        };
        // The name this device gives the server, as every other client connection gives it. It
        // labels the device and proves nothing.
        let name = ClientName::this_computer();
        let cleanup = direct.clone();
        cx.spawn_in(window, async move |sheet, cx| {
            let joined = cx
                .background_executor()
                .spawn(async move { direct.join(text, name) })
                .await;
            let started = joined.as_ref().ok().copied();
            let kept = sheet
                .update_in(cx, |sheet, window, cx| sheet.joined(joined, window, cx))
                .unwrap_or(false);
            // A sheet that closed meanwhile asked for nothing, so its join ends exactly.
            if !kept && let Some(join) = started {
                cx.background_executor()
                    .spawn(async move {
                        let _ = cleanup.cancel_join(join);
                    })
                    .await;
            }
        })
        .detach();
        self.show(window, cx);
    }

    /// Follows the join the registry started. Answers whether the sheet still wants it.
    fn joined(
        &mut self,
        joined: Result<JoinId, DirectError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.closed {
            return false;
        }
        let Stage::Joining { join: None, server } = &self.stage else {
            return false;
        };
        let server = server.clone();
        match joined {
            Ok(join) => {
                self.stage = Stage::Joining {
                    join: Some(join),
                    server,
                };
                self.observe(window, cx);
            }
            Err(error) => {
                self.stage = Stage::JoinRefused { server, error };
                self.show(window, cx);
            }
        }
        true
    }

    /// Records the person's stop of `join` off the window's thread, holding the sheet open until
    /// the record is saved.
    fn stop_join(&mut self, join: JoinId, cx: &mut Context<Self>) {
        let Some(direct) = self.direct.clone() else {
            return;
        };
        let window = self.window;
        self._work = Some(cx.spawn(async move |sheet, cx| {
            let stopped = cx
                .background_executor()
                .spawn(async move { direct.cancel_join(join) })
                .await;
            let _ = window.update(cx, |_, window, cx| {
                let _ = sheet.update(cx, |sheet, cx| sheet.stopped(stopped, window, cx));
            });
        }));
    }

    fn stopped(
        &mut self,
        stopped: Result<(), DirectError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let server = match &self.stage {
            Stage::Joining { server, .. } => server.clone(),
            _ => String::new(),
        };
        match stopped {
            Ok(()) => {
                self.stage = Stage::Done;
                self.closed = true;
                let _ = pending.allow(window, None, cx);
            }
            Err(_) => {
                // The stop stays in memory and authorizes nothing until a save succeeds.
                self.stage = Stage::StopUnsaved { server };
                self.show(window, cx);
            }
        }
    }

    /// Follows the direct registry: the join this sheet started, and what it needs next.
    fn observe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.stage {
            Stage::Joining {
                join: Some(join), ..
            } => {
                // A route SpaceTerm published is in use whether or not its save was confirmed;
                // the route's own detail keeps saying when it wasn't.
                if let Some(JoinStatus::Approved(route) | JoinStatus::ApprovedUncertain(route)) =
                    self.join_status(*join)
                {
                    self.succeed(ServerChoice::Direct(route), window, cx);
                    return;
                }
            }
            // The stop is saved once the registry saves again, so the sheet follows that recovery
            // rather than outliving it.
            Stage::StopUnsaved { server }
                if self
                    .direct
                    .as_ref()
                    .is_some_and(|direct| !direct.persistence_error()) =>
            {
                let server = server.clone();
                self.stage = Stage::Stopped { server };
            }
            _ => {}
        }
        self.show(window, cx);
    }

    /// Selects the server the person just added and closes the sheet. A route whose save
    /// SpaceTerm couldn't confirm is selected too; its own detail says so.
    fn succeed(&mut self, server: ServerChoice, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self
            .settings
            .update(cx, |settings, cx| settings.select_server(server, cx));
        self.finish(window, cx);
    }

    /// Closes the sheet with nothing left to end.
    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage = Stage::Done;
        self.closed = true;
        match self.presentation.take() {
            Some(presentation) => {
                let _ = presentation.complete(window, None, cx);
            }
            None => self.release(cx),
        }
        cx.notify();
    }

    /// Ends what this sheet started and still owns. Runs once, when the sheet closes or its
    /// window does. A setup underway is asked to stop; it closes its own SSH resources.
    fn abandon(&mut self, cx: &mut App) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Stage::SettingUp { cancel, .. } = &self.stage {
            cancel.cancel();
        }
        if let (Some(direct), Some(join)) = (self.direct.clone(), self.live_join()) {
            cx.background_executor()
                .spawn(async move {
                    let _ = direct.cancel_join(join);
                })
                .detach();
        }
        self.stage = Stage::Done;
    }

    /// Lets go of this sheet once nothing it presents remains.
    fn release(&mut self, cx: &mut Context<Self>) {
        let window = self.window.window_id();
        let this = cx.entity_id();
        let owned = cx
            .try_global::<OpenSheets>()
            .and_then(|sheets| sheets.0.get(&window))
            .is_some_and(|sheet| sheet.entity_id() == this);
        if owned {
            let released = cx.global_mut::<OpenSheets>().0.remove(&window);
            // The sheet may hold its own last reference here, so it drops after this update.
            cx.defer(move |_| drop(released));
        }
        // A value that arrived while this sheet was busy opens in a fresh sheet.
        if let Some(request) = self.deferred.take() {
            let settings = self.settings.clone();
            let window = self.window;
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| {
                    let _ = settings.update(cx, |settings, cx| {
                        settings.begin_add_server(request, window, cx);
                    });
                });
            });
        }
    }
}

/// Text that assistive technology reads, with an identity of its own.
fn readable(id: impl Into<SharedString>, text: impl Into<SharedString>) -> gpui::Text {
    gpui::Text::new(ElementId::from(id.into()), text.into())
}

impl Render for AddServerSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = crate::ui::appearance::shared_chrome(cx);
        let body = match &self.stage {
            Stage::Form => self.render_form(&appearance, cx),
            _ => self.render_status(&appearance),
        };
        let deferred = self.deferred.as_ref().map(|request| {
            let note: SharedString = match request {
                AddServerRequest::Ssh(Some(host)) => format!("\"{host}\" is next.").into(),
                _ => "Another invitation is next.".into(),
            };
            let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
            div()
                .id(DEFERRED_SELECTOR)
                .debug_selector(|| DEFERRED_SELECTOR.to_owned())
                .role(accesskit::Role::Status)
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.text_secondary))
                .whitespace_normal()
                .child(readable(format!("{DEFERRED_SELECTOR}-text"), note))
        });
        div()
            .debug_selector(|| MODAL_ID.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(12.0))
            .child(body)
            .children(deferred)
    }
}

impl AddServerSheet {
    fn render_form(&self, appearance: &ChromeAppearance, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        let method = SegmentedControl::new(
            METHOD_SELECTOR,
            "Connection method",
            &self.method,
            vec![
                SegmentedOption::new(Method::Ssh, "SSH")
                    .disabled(self.ssh.is_none())
                    .debug_selector(format!("{METHOD_SELECTOR}-ssh")),
                SegmentedOption::new(Method::Invitation, "Invitation")
                    .disabled(self.direct.is_none())
                    .debug_selector(format!("{METHOD_SELECTOR}-invitation")),
            ],
        )
        .map(|control| {
            control
                .full_width(true)
                .debug_selector(METHOD_SELECTOR)
                .on_change(move |change, window, cx| {
                    let method = *change.requested();
                    let _ = owner.update(cx, |sheet, cx| sheet.set_method(method, window, cx));
                })
                .into_any_element()
        })
        .unwrap_or_else(|_| div().into_any_element());
        let content = match self.method {
            Method::Ssh => self.render_ssh(appearance, cx),
            Method::Invitation => self.render_link(appearance, cx),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(16.0))
            .child(method)
            .child(content)
            .into_any_element()
    }

    /// One labeled field: the label and description on the left, the field on the right, and a
    /// problem directly under the field.
    fn field_row(
        appearance: &ChromeAppearance,
        id: &'static str,
        label: &'static str,
        description: SharedString,
        field: AnyElement,
        trailing: Option<AnyElement>,
        problem: Option<&'static str>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        div()
            .id(SharedString::from(format!("{id}-row")))
            .role(accesskit::Role::Group)
            .aria_label(label)
            .aria_description(description.clone())
            .flex()
            .flex_row()
            .items_start()
            .w_full()
            .gap(appearance.spacing(16.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(2.0))
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Body))
                            .text_color(gpui_color(colors.text))
                            .child(readable(format!("{id}-label"), label)),
                    )
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(colors.text_secondary))
                            .whitespace_normal()
                            .child(readable(format!("{id}-description"), description)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(appearance.spacing(4.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(appearance.spacing(8.0))
                            .child(field)
                            .children(trailing),
                    )
                    .when_some(problem, |column, problem| {
                        column.child(
                            div()
                                .id(SharedString::from(format!("{PROBLEM_SELECTOR}-{id}")))
                                .debug_selector(|| PROBLEM_SELECTOR.to_owned())
                                .role(accesskit::Role::Label)
                                .aria_value(problem)
                                .w(appearance.spacing(FIELD_WIDTH))
                                .text_right()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(colors.error))
                                .whitespace_normal()
                                .child(problem),
                        )
                    }),
            )
            .into_any_element()
    }

    fn field_frame(
        appearance: &ChromeAppearance,
        id: &'static str,
        input: &Entity<TextInput>,
        invalid: bool,
        trailing: Option<AnyElement>,
        cx: &App,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let focus = input.read(cx).focus_handle();
        spaceterm_ui::field_frame(
            SharedString::from(format!("{id}-frame")),
            &focus,
            FieldState::default().invalid(invalid),
            RadiusRole::Control.pixels(),
            cx,
        )
        .h(appearance.height(28.0, 13.0))
        .w(appearance.spacing(FIELD_WIDTH))
        .flex()
        .flex_row()
        .items_center()
        .gap(appearance.spacing(4.0))
        .pl(appearance.spacing(8.0))
        .pr(appearance.spacing(2.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(colors.text))
        .on_click(move |_, window, cx| {
            focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(div().flex_1().min_w_0().child(input.clone()))
        .children(trailing)
        .into_any_element()
    }

    /// The SSH method's host and network address.
    fn render_ssh(&self, appearance: &ChromeAppearance, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        // The SSH Hosts picker sits inside the field and fills it in; Set Up still sets up.
        let menu = (!self.hosts.is_empty()).then(|| {
            let entries = self
                .hosts
                .iter()
                .enumerate()
                .map(|(index, host)| {
                    MenuEntry::action(host.as_str().to_owned(), host.clone())
                        .debug_selector(format!("{HOST_CHOICE_SELECTOR}-{index}"))
                })
                .collect();
            Menu::new(ElementId::from(HOSTS_MENU_SELECTOR), "SSH Hosts", entries)
                .button_trigger(
                    ButtonVariant::Ghost,
                    ButtonSize::Small,
                    ButtonShape::Rounded,
                )
                .debug_selector(HOSTS_MENU_SELECTOR)
                .on_activate(move |activation, window, cx| {
                    let host = activation.action().as_str().to_owned();
                    let _ = owner.update(cx, |sheet, cx| {
                        sheet.host.update(cx, |input, cx| input.set_value(host, cx));
                        sheet.host_invalid = false;
                        sheet.show(window, cx);
                        sheet.focus_field(window, cx);
                    });
                })
                .into_any_element()
        });
        let host = Self::field_row(
            appearance,
            HOST_SELECTOR,
            "Host",
            HOST_DESCRIPTION.into(),
            Self::field_frame(
                appearance,
                HOST_SELECTOR,
                &self.host,
                self.host_invalid,
                menu,
                cx,
            ),
            None,
            self.host_invalid.then_some(HOST_INVALID),
        );
        // The network address is optional, so it stays folded away until the person asks for it
        // or it needs attention.
        let shown = self.address_shown || self.address_invalid;
        let disclosure_owner = cx.weak_entity();
        let icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        let disclosure = Button::new(ADDRESS_DISCLOSURE_SELECTOR, "Network Address")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Regular)
            .align_start()
            .tab_stop(true)
            .leading(move |foreground| {
                Icon::new(
                    if shown {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    icon_size,
                    foreground,
                )
                .into_any_element()
            })
            .debug_selector(ADDRESS_DISCLOSURE_SELECTOR)
            .accessibility_description(if shown {
                "Hide the network address"
            } else {
                ADDRESS_DISCLOSURE_DESCRIPTION
            })
            .on_activate(move |_, window, cx| {
                let _ = disclosure_owner.update(cx, |sheet, cx| {
                    sheet.address_shown = !sheet.address_shown;
                    if sheet.address_shown {
                        sheet.address.read(cx).focus_handle().focus(window, cx);
                    }
                    cx.notify();
                });
            })
            .into_any_element();
        let address = shown.then(|| {
            Self::field_row(
                appearance,
                ADDRESS_SELECTOR,
                "Network Address",
                address_description(),
                Self::field_frame(
                    appearance,
                    ADDRESS_SELECTOR,
                    &self.address,
                    self.address_invalid,
                    None,
                    cx,
                ),
                None,
                self.address_invalid.then_some(ADDRESS_INVALID),
            )
        });
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(12.0))
            .child(host)
            .child(disclosure)
            .children(address)
            .into_any_element()
    }

    fn render_link(&self, appearance: &ChromeAppearance, cx: &mut Context<Self>) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let owner = cx.weak_entity();
        let problem = match &self.preview {
            Some(Err(error)) => Some(link_problem(*error)),
            _ => None,
        };
        let field = Self::field_frame(
            appearance,
            LINK_SELECTOR,
            &self.link,
            problem.is_some(),
            None,
            cx,
        );
        let paste = Button::new(PASTE_SELECTOR, "Paste")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .debug_selector(PASTE_SELECTOR)
            .accessibility_description("Paste an invitation link from the clipboard")
            .on_activate(move |_, window, cx| {
                // The clipboard is read only because the person asked.
                let pasted = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .map(Zeroizing::new);
                let Some(pasted) = pasted else {
                    return;
                };
                let _ = owner.update(cx, |sheet, cx| {
                    sheet
                        .link
                        .update(cx, |input, cx| input.set_value(pasted.trim(), cx));
                    sheet.read_link(cx);
                    sheet.show(window, cx);
                    sheet.focus_field(window, cx);
                });
            })
            .into_any_element();
        let row = Self::field_row(
            appearance,
            LINK_SELECTOR,
            "Invitation Link",
            LINK_DESCRIPTION.into(),
            field,
            Some(paste),
            problem,
        );
        let preview = match &self.preview {
            Some(Ok(preview)) => {
                let fact = |id: &'static str, label: &'static str, value: &str| {
                    div()
                        .id(SharedString::from(format!("{PREVIEW_SELECTOR}-{id}")))
                        .role(accesskit::Role::Label)
                        .aria_label(label)
                        .aria_value(SharedString::from(value.to_owned()))
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .gap(appearance.spacing(16.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .chrome_text(appearance.typography.style(TextRole::Body))
                                .text_color(gpui_color(colors.text))
                                .child(label),
                        )
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Body))
                                .text_color(gpui_color(colors.text_secondary))
                                .child(SharedString::from(value.to_owned())),
                        )
                };
                Some(
                    div()
                        .debug_selector(|| PREVIEW_SELECTOR.to_owned())
                        .flex()
                        .flex_col()
                        .w_full()
                        .gap(appearance.spacing(8.0))
                        .child(fact("server", "Server", &preview.server))
                        .child(fact("address", "Address", &preview.address)),
                )
            }
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(12.0))
            .child(row)
            .children(preview)
            .into_any_element()
    }

    /// Everything after the form: setup progress, an offer, a problem, or the join.
    fn render_status(&self, appearance: &ChromeAppearance) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let text = StatusText(appearance);
        match &self.stage {
            Stage::SettingUp {
                offer: Some(pending),
                cancelling: false,
                ..
            } => {
                let mut container = text.container(accesskit::Role::Group);
                for (index, line) in offer_lines(&pending.offer).into_iter().enumerate() {
                    container =
                        container.child(text.line(&format!("offer-{index}"), line, colors.text));
                }
                container.into_any_element()
            }
            Stage::SettingUp {
                setup,
                phase,
                cancelling,
                ..
            } => {
                let (label, detail): (SharedString, SharedString) = if *cancelling {
                    (
                        "Cancelling\u{2026}".into(),
                        "SpaceTerm is closing the SSH connection. A step the server already accepted can still finish.".into(),
                    )
                } else {
                    (
                        format!(
                            "Setting up \u{201C}{}\u{201D}\u{2026}",
                            setup.destination.as_str()
                        )
                        .into(),
                        phase
                            .map(crate::ui::ssh_setup::phase_detail)
                            .unwrap_or("Connecting over SSH\u{2026}")
                            .into(),
                    )
                };
                text.container(accesskit::Role::Status)
                    .child(text.progress("setting-up", label))
                    .child(text.line("detail", detail, colors.text_secondary))
                    .into_any_element()
            }
            Stage::SetupProblem { problem, .. } => text
                .container(accesskit::Role::Alert)
                .child(text.line(
                    "state",
                    SharedString::new_static(problem.state),
                    colors.text,
                ))
                .child(text.line("detail", problem.explanation.clone(), colors.text_secondary))
                .into_any_element(),
            Stage::Joining { join, server } => {
                let joining = join.and_then(|join| self.join_status(join));
                Self::render_join(joining, server, appearance)
            }
            Stage::JoinRefused { error, .. } => text
                .container(accesskit::Role::Alert)
                .child(text.line("refused", join_failure(*error).into(), colors.text))
                .into_any_element(),
            Stage::StopUnsaved { .. } => text
                .container(accesskit::Role::Alert)
                .child(text.line("unsaved", WITHDRAW_UNSAVED.into(), colors.error))
                .into_any_element(),
            Stage::Stopped { server } => text
                .container(accesskit::Role::Status)
                .child(text.line(
                    "stopped",
                    format!("SpaceTerm is cancelling the pairing with {server}.").into(),
                    colors.text,
                ))
                .into_any_element(),
            Stage::Form | Stage::Done => div().into_any_element(),
        }
    }

    /// The join underway: the code to compare while the owner decides, or how it ended.
    fn render_join(
        joining: Option<JoinStatus>,
        server: &str,
        appearance: &ChromeAppearance,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let text = StatusText(appearance);
        match joining {
            None | Some(JoinStatus::Validating) => text
                .container(accesskit::Role::Status)
                .child(text.progress("joining", format!("Connecting to {server}\u{2026}").into()))
                .into_any_element(),
            Some(JoinStatus::AwaitingApproval {
                comparison_code, ..
            }) => text
                .container(accesskit::Role::Status)
                .items_center()
                .child(
                    div()
                        .id(CODE_SELECTOR)
                        .debug_selector(|| CODE_SELECTOR.to_owned())
                        .role(accesskit::Role::Label)
                        .aria_label("Code")
                        .aria_value(code_speech(comparison_code))
                        .chrome_text(appearance.typography.style(TextRole::Title))
                        .text_color(gpui_color(colors.text))
                        .child(code_text(comparison_code)),
                )
                .child(text.line(
                    "compare",
                    format!("Check that {server} shows this code").into(),
                    colors.text,
                ))
                .child(text.progress("waiting", "Waiting for approval…".into()))
                .into_any_element(),
            Some(JoinStatus::Retryable { stage }) => text
                .container(accesskit::Role::Alert)
                .child(text.line("paused", join_paused(stage).into(), colors.warning))
                .into_any_element(),
            Some(JoinStatus::Approved(_) | JoinStatus::ApprovedUncertain(_)) => text
                .container(accesskit::Role::Status)
                .child(text.progress("joined", format!("Joining {server}\u{2026}").into()))
                .into_any_element(),
            // SpaceTerm can't tell whether the connection was saved, so it uses none and claims
            // neither success nor withdrawal. Only the person's Withdraw asks the server to
            // remove the pairing.
            Some(JoinStatus::PublicationUnconfirmed) => text
                .container(accesskit::Role::Alert)
                .child(
                    text.line(
                        "unconfirmed",
                        format!(
                            "{server} approved this device, but SpaceTerm couldn't save the connection."
                        )
                        .into(),
                        colors.warning,
                    ),
                )
                .into_any_element(),
            Some(JoinStatus::Rejected) => text
                .container(accesskit::Role::Alert)
                .child(text.line(
                    "denied",
                    format!("{server} denied this device.").into(),
                    colors.text,
                ))
                .into_any_element(),
            Some(JoinStatus::Expired) => text
                .container(accesskit::Role::Alert)
                .child(
                    text.line(
                        "expired",
                        "Invitation expired. Ask for a new one.".into(),
                        colors.text,
                    ),
                )
                .into_any_element(),
            Some(JoinStatus::Failed(error)) => text
                .container(accesskit::Role::Alert)
                .child(text.line("failed", join_failure(error).into(), colors.text))
                .into_any_element(),
        }
    }
}

/// What the person reviews before approving `offer`: what the step does, in plain terms.
fn offer_lines(offer: &SetupOffer) -> Vec<SharedString> {
    match offer {
        SetupOffer::Installation { version, platform } => {
            let mut lines = vec![SharedString::from(format!(
                "SpaceTerm installs its signed SpaceTerm Server {version} for {} and starts it.",
                crate::ui::ssh_setup::platform_name(&platform.0, &platform.1)
            ))];
            if platform.0.eq_ignore_ascii_case("linux") {
                lines.push("SpaceTerm may ask for your sudo password on that computer.".into());
            }
            lines
        }
        SetupOffer::Startup => vec![
            "SpaceTerm Server is installed but stopped. Starting it lets paired devices reach its Workspaces."
                .into(),
        ],
        SetupOffer::Network {
            address,
            bind,
            current,
            enabled,
        } => {
            let mut lines = vec![SharedString::from(format!(
                "SpaceTerm Server will accept connections from paired devices at {bind}, and this computer will reach it at {}. Devices must still be approved before they can connect.",
                address.authority()
            ))];
            match current.filter(|current| current != bind) {
                Some(current) if *enabled => lines.push(
                    format!("It stops listening at {current}. Paired devices that use that address can\u{2019}t connect until they use the new one.")
                        .into(),
                ),
                _ if !*enabled => {
                    lines.push("Network access is off on that server. Allowing turns it on.".into());
                }
                _ => {}
            }
            lines
        }
    }
}

/// The status lines a stage after the form shows.
struct StatusText<'a>(&'a ChromeAppearance);

impl StatusText<'_> {
    fn container(&self, role: accesskit::Role) -> gpui::Stateful<gpui::Div> {
        div()
            .id(STATUS_SELECTOR)
            .debug_selector(|| STATUS_SELECTOR.to_owned())
            .role(role)
            .flex()
            .flex_col()
            .w_full()
            .gap(self.0.spacing(8.0))
    }

    fn line(&self, id: &str, text: SharedString, color: crate::appearance::Color) -> gpui::Div {
        div()
            .chrome_text(self.0.typography.style(TextRole::Body))
            .text_color(gpui_color(color))
            .whitespace_normal()
            .child(readable(format!("{STATUS_SELECTOR}-{id}"), text))
    }

    /// Work underway, named beside its spinner because the spinner alone says nothing.
    fn progress(&self, id: &str, label: SharedString) -> gpui::Div {
        let colors = self.0.host_colors(spaceterm_ui::ControlHost::Floating);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(self.0.spacing(8.0))
            .child(
                ProgressRing::new(
                    SharedString::from(format!("{STATUS_SELECTOR}-{id}-progress")),
                    label.clone(),
                    ProgressState::Indeterminate,
                )
                .size(ProgressSize::Compact),
            )
            .child(self.line(id, label, colors.text))
    }
}

#[cfg(test)]
#[path = "add_server_tests.rs"]
mod tests;
