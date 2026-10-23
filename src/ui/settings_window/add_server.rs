//! Add Server: the one sheet that connects to an SSH host or joins with an invitation link.
//!
//! Every entry point, such as the server picker, the All Servers list, Settings Search, the
//! application menu or an invitation link the operating system delivered, starts here. Filling in
//! a value connects and joins nothing; the person chooses Connect or Join first.
//!
//! The sheet owns its own state and lives while it has work to finish, including while an SSH
//! sign-in prompt has taken its place. Each stage presents its own title and actions, and a stage
//! change replaces the sheet on screen with the next one. Connecting follows the SSH registry,
//! which owns installation offers, prompts and cancellation. Joining follows the direct
//! connections registry, which owns the enrollment and its exact cleanup. The invitation link
//! stays in the obscured field until Join hands it to the registry once; the sheet keeps only the
//! server name and address it names.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Context, ElementId, Entity, Global, SharedString,
    Subscription, Task, WeakEntity, Window, WindowId, accesskit, div,
};
use spaceterm_ui::{
    Button, ButtonShape, ButtonSize, ButtonVariant, Dialog, DialogActionRequest,
    DialogCloseDecision, DialogCompletion, DialogInitialFocus, DialogOutcome,
    DialogPendingCompletion, DialogSize, FieldState, Menu, MenuEntry, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLifecycleEvent,
    ProgressRing, ProgressSize, ProgressState, SegmentedControl, SegmentedOption, TextInput,
    TextInputContentMode, TextInputEscapeBehavior, TextInputEvent, TextInputReturnBehavior,
    TextInputVariant,
};
use zeroize::Zeroizing;

use super::direct_join::{WITHDRAW_UNSAVED, join_failure, join_paused};
use super::server_list::ServerChoice;
use super::{AddServerRequest, SettingsWindow};
use crate::direct_connections::{DirectConnections, JoinId, JoinStatus};
use crate::domain::SshDestination;
use crate::local_server::direct::{DirectError, Invitation};
use crate::local_server::pane::control::ClientName;
use crate::ssh::server_connection::{
    RemoteConnectionStatus, SshFlowConnectionIntent, SshServerConnections,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{code_speech, code_text};
use crate::ui::direct_servers::DirectChanges;
use crate::ui::ssh_servers::{
    SshPromptActivity, SshPromptEvent, SshServerAction, installation_consequence, platform_name,
    present,
};

pub(super) const MODAL_ID: &str = "add-server";
pub(super) const METHOD_SELECTOR: &str = "add-server-method";
pub(super) const HOST_SELECTOR: &str = "add-server-host";
pub(super) const HOSTS_MENU_SELECTOR: &str = "add-server-ssh-hosts";
/// Prefixes each configured host in the SSH Hosts menu, followed by its position.
pub(super) const HOST_CHOICE_SELECTOR: &str = "add-server-ssh-host";
pub(super) const LINK_SELECTOR: &str = "add-server-link";
pub(super) const PASTE_SELECTOR: &str = "add-server-paste";
pub(super) const PREVIEW_SELECTOR: &str = "add-server-preview";
pub(super) const PROBLEM_SELECTOR: &str = "add-server-problem";
pub(super) const STATUS_SELECTOR: &str = "add-server-status";
pub(super) const CODE_SELECTOR: &str = "add-server-code";

/// What the host field's description says. It is also the field's accessibility hint.
const HOST_DESCRIPTION: &str = "SpaceTerm signs in the way ssh does, then uses that \
     account\u{2019}s server or offers to install one.";
const HOST_INVALID: &str = "Enter one SSH host or alias, such as penguin or me@build.local, \
     without spaces or options.";
const LINK_DESCRIPTION: &str = "Paste the link from Invite Device on the other computer.";
/// How long the host and link fields are, so a long value scrolls inside the field.
const FIELD_WIDTH: f32 = 260.0;

/// The two ways to add a server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Method {
    Ssh,
    Invitation,
}

/// What the sheet offers as a button. Each stage presents some of these.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SheetAction {
    Connect,
    Join,
    Install,
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
            Self::Connect => "Connect",
            Self::Join => "Join",
            Self::Install => "Install",
            Self::TryAgain => "Try Again",
            Self::Back => "Back",
            Self::StartAgain => "Start Again",
            Self::Withdraw => "Withdraw Pairing",
            Self::Cancel => "Cancel",
            Self::Close => "Close",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Connect => "add-server-connect",
            Self::Join => "add-server-join",
            Self::Install => "add-server-install",
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
            Self::Connect | Self::Join | Self::Install | Self::TryAgain | Self::StartAgain => {
                action
                    .with_emphasis(ModalActionEmphasis::Prominent)
                    .default_action(true)
            }
            Self::Withdraw => action.with_intent(ModalActionIntent::Destructive),
            _ => action,
        }
    }
}

/// Where the sheet's first keyboard focus goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusTarget {
    Host,
    Link,
    Action(SheetAction),
}

/// The title and actions one stage presents. A different shape replaces the sheet on screen.
#[derive(Clone, Debug, PartialEq)]
struct Shape {
    title: SharedString,
    actions: Vec<(SheetAction, bool)>,
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
        DirectError::Expired => "This invitation expired. Ask for a new one.",
        _ => "This isn\u{2019}t a valid SpaceTerm invitation link.",
    }
}

/// Where adding a server stands.
enum Stage {
    /// Choosing SSH or Invitation and filling in its value.
    Form,
    /// Connecting to an SSH host, including an installation the person approved.
    Connecting(SshDestination),
    /// The host's account has no server; the person reviews this exact offer first.
    Install {
        destination: SshDestination,
        offer_id: u64,
        detail: SharedString,
    },
    /// Connecting stopped with a state the person can read and maybe retry.
    SshProblem {
        destination: SshDestination,
        state: &'static str,
        explanation: SharedString,
        retry: bool,
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
    /// The sheet finished; nothing more is presented.
    Done,
}

/// The Add Server sheets open in each window, kept while they have work to finish.
#[derive(Default)]
struct OpenSheets(HashMap<WindowId, Entity<AddServerSheet>>);

impl Global for OpenSheets {}

fn open_sheet(window: &Window, cx: &App) -> Option<Entity<AddServerSheet>> {
    cx.try_global::<OpenSheets>()
        .and_then(|sheets| sheets.0.get(&window.window_handle().window_id()))
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
    ssh: Option<SshServerConnections>,
    direct: Option<DirectConnections>,
    method: Method,
    host: Entity<TextInput>,
    link: Entity<TextInput>,
    /// The hosts this computer's SSH configuration names, read when the sheet opens.
    hosts: Vec<SshDestination>,
    /// Whether the typed host was refused when the person last chose Connect.
    host_invalid: bool,
    /// What the link in the field names, or why it can't be used. `None` while it is empty.
    preview: Option<Result<Preview, DirectError>>,
    stage: Stage,
    /// The connection attempt this sheet started, which only this sheet's Cancel ends.
    intent: Option<SshFlowConnectionIntent>,
    /// The host this sheet's attempt saved for the first time. Ending the attempt before it
    /// connects forgets it again, so a cancelled Add Server adds nothing.
    added: Option<SshDestination>,
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
    _subscriptions: Vec<Subscription>,
    _ssh_changes: Option<Task<()>>,
    _direct_changes: Option<DirectChanges>,
    _reading: Option<Task<()>>,
    _work: Option<Task<()>>,
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
        let link = cx.new(|cx| {
            TextInput::new(LINK_SELECTOR, "Invitation link", String::new(), window, cx)
                .variant(TextInputVariant::Bare)
                .content_mode(TextInputContentMode::Obscured)
                .placeholder("spaceterm://pair/\u{2026}")
                .return_behavior(TextInputReturnBehavior::Propagate)
                .escape_behavior(TextInputEscapeBehavior::Propagate)
                .debug_selector(LINK_SELECTOR)
        });
        let prompts = crate::ui::ssh_servers::prompt_activity(window, cx);
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
                    if !matches!(sheet.stage, Stage::Connecting(_)) {
                        return;
                    }
                    if matches!(event, SshPromptEvent::Settled { cancelled: true }) {
                        // The person declined to sign in, so the host stays for another try.
                        sheet.back_to_form(window, cx);
                    } else {
                        sheet.observe(window, cx);
                    }
                },
            ),
        ];
        // The sheet ends with its window too, and ends what it started with it.
        subscriptions.push(cx.on_release(|sheet, cx| sheet.abandon(cx)));
        let ssh = crate::ui::ssh_servers::installed(cx);
        let ssh_changes = ssh.as_ref().map(|ssh| {
            let changes = ssh.subscribe();
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
        });
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
        // Reading SSH configuration reaches no host.
        let reading = crate::ui::ssh_servers::host_source(cx).map(|source| {
            let reading = cx.background_spawn(async move { source.hosts() });
            cx.spawn(async move |sheet, cx| {
                let hosts = reading.await;
                let _ = sheet.update(cx, |sheet, cx| {
                    sheet.hosts = hosts;
                    cx.notify();
                });
            })
        });
        Self {
            settings,
            window: window.window_handle(),
            ssh,
            direct,
            method: Method::Ssh,
            host,
            link,
            hosts: Vec::new(),
            host_invalid: false,
            preview: None,
            stage: Stage::Form,
            intent: None,
            added: None,
            prompts,
            shown: None,
            presentation: None,
            active: false,
            surface: 0,
            pending: None,
            closed: false,
            _subscriptions: subscriptions,
            _ssh_changes: ssh_changes,
            _direct_changes: direct_changes,
            _reading: reading,
            _work: None,
        }
    }

    /// Fills in `request`'s value when the sheet is waiting for one, then presents the sheet. Work
    /// underway, such as a connection or a join, keeps its place.
    fn fill(&mut self, request: AddServerRequest, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            self.show(window, cx);
            return;
        }
        self.end_attempt();
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
        match &self.stage {
            Stage::Connecting(_) | Stage::Install { .. } => true,
            Stage::Joining { join: None, .. } => true,
            Stage::Joining {
                join: Some(join), ..
            } => !matches!(
                self.join_status(*join),
                Some(JoinStatus::Rejected | JoinStatus::Expired | JoinStatus::Failed(_)) | None
            ),
            Stage::Form
            | Stage::SshProblem { .. }
            | Stage::JoinRefused { .. }
            | Stage::StopUnsaved { .. }
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
        let shape = match &self.stage {
            Stage::Form => {
                let (primary, enabled, focus) = match self.method {
                    Method::Ssh => (
                        SheetAction::Connect,
                        self.ssh.is_some() && !self.host.read(cx).value().trim().is_empty(),
                        FocusTarget::Host,
                    ),
                    Method::Invitation => (
                        SheetAction::Join,
                        self.direct.is_some() && matches!(self.preview, Some(Ok(_))),
                        FocusTarget::Link,
                    ),
                };
                Shape {
                    title: add(),
                    actions: vec![(primary, enabled), (SheetAction::Cancel, true)],
                    focus,
                }
            }
            Stage::Connecting(_) => {
                if self.prompts.read(cx).active {
                    return None;
                }
                Shape {
                    title: add(),
                    actions: vec![(SheetAction::Cancel, true)],
                    focus: FocusTarget::Action(SheetAction::Cancel),
                }
            }
            Stage::Install { destination, .. } => Shape {
                title: format!(
                    "Install SpaceTerm on \u{201C}{}\u{201D}?",
                    destination.as_str()
                )
                .into(),
                actions: vec![(SheetAction::Install, true), (SheetAction::Cancel, true)],
                focus: FocusTarget::Action(SheetAction::Install),
            },
            Stage::SshProblem {
                destination, retry, ..
            } => {
                let mut actions = Vec::with_capacity(3);
                if *retry {
                    actions.push((SheetAction::TryAgain, true));
                }
                actions.push((SheetAction::Back, true));
                actions.push((SheetAction::Cancel, true));
                Shape {
                    title: format!(
                        "Couldn\u{2019}t Connect to \u{201C}{}\u{201D}",
                        destination.as_str()
                    )
                    .into(),
                    actions,
                    focus: FocusTarget::Action(if *retry {
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
                        vec![(SheetAction::TryAgain, true), (SheetAction::Cancel, true)],
                        SheetAction::TryAgain,
                    ),
                    Some(JoinStatus::PublicationUnconfirmed) => (
                        vec![(SheetAction::Withdraw, true), (SheetAction::Close, true)],
                        SheetAction::Close,
                    ),
                    Some(JoinStatus::Rejected | JoinStatus::Expired | JoinStatus::Failed(_)) => (
                        vec![(SheetAction::StartAgain, true), (SheetAction::Close, true)],
                        SheetAction::StartAgain,
                    ),
                    _ => (vec![(SheetAction::Cancel, true)], SheetAction::Cancel),
                };
                Shape {
                    title: join_title(server),
                    actions,
                    focus: FocusTarget::Action(focus),
                }
            }
            Stage::JoinRefused { server, .. } => Shape {
                title: join_title(server),
                actions: vec![(SheetAction::StartAgain, true), (SheetAction::Close, true)],
                focus: FocusTarget::Action(SheetAction::StartAgain),
            },
            Stage::StopUnsaved { server } => Shape {
                title: join_title(server),
                actions: vec![(SheetAction::Close, true)],
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
        let presented = self.present(&shape, window, cx);
        match presented {
            Some(presentation) => {
                self.presentation = Some(presentation);
                self.shown = Some(shape);
            }
            None => {
                self.shown = None;
                self.abandon(cx);
                self.release(cx);
            }
        }
        cx.notify();
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
                .map(|(action, enabled)| action.modal(*enabled))
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
    /// saves; every other action moves to its next stage once this answer unwinds.
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
            SheetAction::Connect => self.connect_typed(window, cx),
            SheetAction::Join => self.join(window, cx),
            SheetAction::Install => {
                let Stage::Install {
                    destination,
                    offer_id,
                    ..
                } = &self.stage
                else {
                    return;
                };
                let (destination, offer_id) = (destination.clone(), *offer_id);
                if let Some(ssh) = &self.ssh {
                    // A Linux installation may ask for sudo, which appears here.
                    crate::ui::ssh_servers::present_prompts_in(window, cx);
                    ssh.install_for_flow(&destination, offer_id);
                }
                self.stage = Stage::Connecting(destination);
                self.observe(window, cx);
            }
            SheetAction::TryAgain => match &self.stage {
                Stage::SshProblem { destination, .. } => {
                    let destination = destination.clone();
                    self.connect(destination, window, cx);
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
            SheetAction::Back => self.back_to_form(window, cx),
            SheetAction::StartAgain => {
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

    /// Returns to the form with the host still typed, ending the attempt this sheet started.
    fn back_to_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_attempt();
        self.stage = Stage::Form;
        self.method = Method::Ssh;
        self.show(window, cx);
        self.focus_field(window, cx);
    }

    /// Connects to the host the person typed. The text passes to SSH as one destination, so SSH
    /// configuration still decides the host, user and keys.
    fn connect_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.host.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        match SshDestination::new(typed) {
            Ok(destination) => self.connect(destination, window, cx),
            Err(_) => {
                self.host_invalid = true;
                self.focus_field(window, cx);
                cx.notify();
            }
        }
    }

    /// Connects to `destination`, or reuses its connection when one is already up or underway.
    fn connect(
        &mut self,
        destination: SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ssh) = self.ssh.clone() else {
            return;
        };
        // The person asked here, so a sign-in or host approval appears here.
        crate::ui::ssh_servers::present_prompts_in(window, cx);
        if !matches!(
            ssh.status(&destination),
            RemoteConnectionStatus::Connected { .. }
        ) {
            self.end_attempt();
            let saved = ssh
                .connections()
                .iter()
                .any(|(listed, _)| *listed == destination);
            self.intent = ssh.connect_for_flow(destination.clone());
            self.added = (self.intent.is_some() && !saved).then(|| destination.clone());
        }
        self.stage = Stage::Connecting(destination);
        self.observe(window, cx);
    }

    /// Ends the connection attempt this sheet started. An attempt another consumer shares stays,
    /// and so does the host it saved.
    fn end_attempt(&mut self) {
        let Some(ssh) = &self.ssh else {
            return;
        };
        if let Some(intent) = self.intent.take() {
            ssh.cancel_flow_connection(intent);
        }
        if let Some(added) = self.added.take()
            && !ssh.intends(&added)
        {
            ssh.remove(&added);
        }
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

    /// Follows the registries: the connection or join this sheet started, and what it needs next.
    fn observe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.stage {
            Stage::Connecting(destination) => {
                let Some(ssh) = self.ssh.clone() else {
                    return;
                };
                let destination = destination.clone();
                match ssh.status(&destination) {
                    RemoteConnectionStatus::Connected { .. } => {
                        // The registry presents the server once its route is admitted.
                        if ssh.server(&destination).is_some() {
                            self.succeed(ServerChoice::Ssh(destination), window, cx);
                            return;
                        }
                    }
                    RemoteConnectionStatus::Working { .. }
                    | RemoteConnectionStatus::Reconnecting { .. }
                    | RemoteConnectionStatus::Cancelling => {}
                    RemoteConnectionStatus::InstallationOffered {
                        offer_id,
                        version,
                        platform,
                    } => {
                        let detail = format!(
                            "This account has no SpaceTerm server. SpaceTerm can install \
                             verified version {version} for {}. {}",
                            platform_name(&platform.0, &platform.1),
                            installation_consequence(&platform.0),
                        );
                        self.stage = Stage::Install {
                            destination,
                            offer_id,
                            detail: detail.into(),
                        };
                    }
                    // The attempt ended without an outcome, as when it was cancelled elsewhere.
                    RemoteConnectionStatus::Inactive => {
                        self.intent = None;
                        self.stage = Stage::Form;
                    }
                    status => {
                        let presentation = present(&status);
                        self.stage = Stage::SshProblem {
                            destination,
                            state: presentation.state,
                            explanation: presentation.explanation,
                            retry: presentation.actions.contains(&SshServerAction::Connect),
                        };
                    }
                }
            }
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
            _ => {}
        }
        self.show(window, cx);
    }

    /// Selects the server the person just added and closes the sheet.
    fn succeed(&mut self, server: ServerChoice, window: &mut Window, cx: &mut Context<Self>) {
        // The connection now belongs to the saved server, not to this sheet.
        self.intent = None;
        self.added = None;
        self.stage = Stage::Done;
        self.closed = true;
        let _ = self
            .settings
            .update(cx, |settings, cx| settings.select_server(server, cx));
        match self.presentation.take() {
            Some(presentation) => {
                let _ = presentation.complete(window, None, cx);
            }
            None => self.release(cx),
        }
        cx.notify();
    }

    /// Ends what this sheet started and still owns. Runs once, when the sheet closes or its
    /// window does.
    fn abandon(&mut self, cx: &mut App) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.end_attempt();
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
    fn release(&self, cx: &mut Context<Self>) {
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
        div()
            .debug_selector(|| MODAL_ID.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(12.0))
            .child(body)
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
            Method::Ssh => self.render_host(appearance, cx),
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
        description: &'static str,
        field: AnyElement,
        trailing: Option<AnyElement>,
        problem: Option<&'static str>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        div()
            .id(SharedString::from(format!("{id}-row")))
            .role(accesskit::Role::Group)
            .aria_label(label)
            .aria_description(description)
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
                                .id(PROBLEM_SELECTOR)
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

    fn render_host(&self, appearance: &ChromeAppearance, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        // The SSH Hosts picker sits inside the field and fills it in; Connect still connects.
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
        let field = Self::field_frame(
            appearance,
            HOST_SELECTOR,
            &self.host,
            self.host_invalid,
            menu,
            cx,
        );
        Self::field_row(
            appearance,
            HOST_SELECTOR,
            "Host",
            HOST_DESCRIPTION,
            field,
            None,
            self.host_invalid.then_some(HOST_INVALID),
        )
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
            LINK_DESCRIPTION,
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

    /// Everything after the form: progress, the installation offer, a problem, or the join.
    fn render_status(&self, appearance: &ChromeAppearance) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let text = StatusText(appearance);
        match &self.stage {
            Stage::Connecting(destination) => {
                let explanation = self
                    .ssh
                    .as_ref()
                    .map(|ssh| present(&ssh.status(destination)).explanation)
                    .unwrap_or_default();
                text.container(accesskit::Role::Status)
                    .child(
                        text.progress(
                            "connecting",
                            format!(
                                "Connecting to \u{201C}{}\u{201D}\u{2026}",
                                destination.as_str()
                            )
                            .into(),
                        ),
                    )
                    .child(text.line("detail", explanation, colors.text_secondary))
                    .into_any_element()
            }
            Stage::Install { detail, .. } => text
                .container(accesskit::Role::Group)
                .child(text.line("install", detail.clone(), colors.text))
                .into_any_element(),
            Stage::SshProblem {
                state, explanation, ..
            } => text
                .container(accesskit::Role::Alert)
                .child(text.line("state", SharedString::new_static(*state), colors.text))
                .child(text.line("detail", explanation.clone(), colors.text_secondary))
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
                    format!("Check that {server} shows the same code").into(),
                    colors.text,
                ))
                .child(text.progress("waiting", format!("Waiting for {server} to allow").into()))
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
                            "{server} allowed this device, but SpaceTerm couldn\u{2019}t confirm \
                         it saved the connection on this computer, so it isn\u{2019}t using it. \
                         If it was saved, it appears when SpaceTerm reopens."
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
                    format!("{server} didn\u{2019}t allow this device.").into(),
                    colors.text,
                ))
                .into_any_element(),
            Some(JoinStatus::Expired) => text
                .container(accesskit::Role::Alert)
                .child(
                    text.line(
                        "expired",
                        format!(
                            "The invitation expired before {server} allowed this device. Ask for a \
                         new one."
                        )
                        .into(),
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
