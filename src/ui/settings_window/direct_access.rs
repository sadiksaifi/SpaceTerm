//! Direct network access and paired devices for the server selected in the Servers list.
//!
//! Two rows carry it. Direct network access reports whether the selected server listens for
//! direct network connections and where, and turns that on, moves it or turns it off after the
//! person confirms a step that names the server. Paired devices lists the devices that server
//! admits over the network, pairs another through an invitation, and unpairs one after the person
//! confirms that exact device.
//!
//! Every step runs over SpaceTerm's existing connection to the selected server, with the
//! authority that connection already has. The rows read that server only while the Servers
//! section shows it, and choosing another server starts over, so nothing read or typed for one
//! server reaches another.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, Context, ElementId, Entity, FocusHandle, SharedString, Task,
    Window, accesskit, div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, Dialog,
    DialogCloseDecision, DialogInitialFocus, DialogOutcome, DialogPendingCompletion, DialogSize,
    FieldState, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId,
    ProgressRing, ProgressSize, ProgressState, TextInput, TextInputVariant,
};

use super::SettingsWindow;
use super::catalog::{SettingsRowId, SettingsSectionId};
use super::server_list::ServerChoice;
use crate::local_server::LocalServer;
use crate::local_server::direct::{NetworkPolicy, PairingId};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{
    AccessFailure, AccessState, Device, DirectAccessControl, Listener, this_computers_address,
};
use crate::ui::sidebar_window::form::{CaptionTone, badge};

pub(super) const ACCESS_SELECTOR: &str = "settings-direct-network-access";
pub(super) const TURN_ON_SELECTOR: &str = "settings-direct-network-turn-on";
pub(super) const CHANGE_SELECTOR: &str = "settings-direct-network-change";
pub(super) const TURN_OFF_SELECTOR: &str = "settings-direct-network-turn-off";
pub(super) const ADDRESS_SELECTOR: &str = "settings-direct-network-address";
pub(super) const ADDRESS_CONFIRM_SELECTOR: &str = "settings-direct-network-address-confirm";
pub(super) const ADDRESS_CANCEL_SELECTOR: &str = "settings-direct-network-address-cancel";
pub(super) const TURN_OFF_CONFIRM_SELECTOR: &str = "settings-direct-network-turn-off-confirm";
pub(super) const TURN_OFF_CANCEL_SELECTOR: &str = "settings-direct-network-turn-off-cancel";
pub(super) const DEVICES_SELECTOR: &str = "settings-paired-devices";
pub(super) const PAIR_SELECTOR: &str = "settings-pair-device";
/// Prefixes each paired device's Unpair button, followed by its position.
pub(super) const UNPAIR_SELECTOR: &str = "settings-unpair-device";
pub(super) const UNPAIR_CONFIRM_SELECTOR: &str = "settings-unpair-device-confirm";
pub(super) const UNPAIR_CANCEL_SELECTOR: &str = "settings-unpair-device-cancel";
/// How often the rows read the selected server while the Servers section shows it.
const POLL: Duration = Duration::from_secs(2);

/// A change to the selected server's access that awaits the server's answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Change {
    TurningOn,
    Moving,
    TurningOff,
}

impl Change {
    const fn progress(self) -> &'static str {
        match self {
            Self::TurningOn => "Turning On\u{2026}",
            Self::Moving => "Changing Address\u{2026}",
            Self::TurningOff => "Turning Off\u{2026}",
        }
    }
}

/// What the rows know about the selected server's direct network access.
pub(super) struct DirectAccess {
    /// The server the rows present.
    target: Option<ServerChoice>,
    /// Counts the times the rows started over, so an answer for an earlier server lands nowhere.
    generation: u64,
    /// The latest reading. `None` until the server answers, or while SpaceTerm can't ask it.
    reading: Option<Result<AccessState, AccessFailure>>,
    devices: Option<Vec<Device>>,
    change: Option<Change>,
    /// Why the person's latest step didn't take effect.
    problem: Option<AccessFailure>,
    /// Devices the person asked to unpair that the server still lists.
    unpairing: Vec<PairingId>,
    poll: Option<Task<()>>,
    work: Option<Task<()>>,
}

impl DirectAccess {
    pub(super) fn new() -> Self {
        Self {
            target: None,
            generation: 0,
            reading: None,
            devices: None,
            change: None,
            problem: None,
            unpairing: Vec::new(),
            poll: None,
            work: None,
        }
    }

    fn start_over(&mut self, target: Option<ServerChoice>) {
        self.target = target;
        self.generation = self.generation.wrapping_add(1);
        self.reading = None;
        self.devices = None;
        self.change = None;
        self.problem = None;
        self.unpairing.clear();
        self.poll = None;
        self.work = None;
    }

    fn state(&self) -> Option<&AccessState> {
        self.reading.as_ref()?.as_ref().ok()
    }

    fn listening(&self) -> bool {
        self.state()
            .is_some_and(|state| matches!(state.listener, Listener::Listening(_)))
    }

    fn receive(&mut self, state: AccessState, devices: Option<Vec<Device>>) {
        if let Some(devices) = &devices {
            self.unpairing
                .retain(|pairing| devices.iter().any(|device| device.pairing == *pairing));
        }
        self.reading = Some(Ok(state));
        if devices.is_some() {
            self.devices = devices;
        }
    }
}

/// The steps the access row offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessAction {
    TurnOn,
    Change,
    TurnOff,
}

impl AccessAction {
    const fn label(self) -> &'static str {
        match self {
            Self::TurnOn => "Turn On\u{2026}",
            Self::Change => "Change Address\u{2026}",
            Self::TurnOff => "Turn Off\u{2026}",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::TurnOn => TURN_ON_SELECTOR,
            Self::Change => CHANGE_SELECTOR,
            Self::TurnOff => TURN_OFF_SELECTOR,
        }
    }
}

/// How the access row reads, derived from what the rows know.
struct AccessPresentation {
    /// The badge and its selector, or a plain value when the state isn't known.
    state: Result<(&'static str, &'static str), &'static str>,
    caption: String,
    tone: CaptionTone,
    actions: &'static [AccessAction],
}

impl SettingsWindow {
    /// The server's direct network operations over SpaceTerm's existing connection to it.
    fn direct_access_control(&self, target: &ServerChoice) -> Option<Arc<dyn DirectAccessControl>> {
        match target {
            ServerChoice::Local => self.local_server.as_ref()?.direct_access(),
            ServerChoice::Ssh(_) => self
                .direct_access_server(target)
                .map(|server| Arc::new(server) as Arc<dyn DirectAccessControl>),
            // A paired device manages the server's pairings equally, over its own route.
            ServerChoice::Direct(route) => self
                .direct_routes
                .as_ref()?
                .connections
                .server(route)
                .map(|server| Arc::new(server) as Arc<dyn DirectAccessControl>),
        }
    }

    /// The account connection an unpair request is recorded against, so its retries keep that
    /// exact route and authority. A direct route records its own pairing instead.
    fn direct_access_server(&self, target: &ServerChoice) -> Option<LocalServer> {
        match target {
            ServerChoice::Local => self.local_server.as_ref()?.worktree_server(),
            ServerChoice::Ssh(destination) => {
                self.ssh_servers.as_ref()?.connections.server(destination)
            }
            ServerChoice::Direct(_) => None,
        }
    }

    /// This device's own pairing with the selected server, when a direct route reaches it.
    fn this_device_pairing(&self) -> Option<PairingId> {
        let Some(ServerChoice::Direct(route)) = &self.direct_access.target else {
            return None;
        };
        Some(
            self.direct_routes
                .as_ref()?
                .connections
                .route(route)?
                .paired
                .pairing,
        )
    }

    /// The selected server's name as the rows say it: the name the server gave, once it has.
    fn direct_access_server_name(&self) -> SharedString {
        self.direct_access
            .state()
            .map(|state| state.server_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.selected_server_name())
    }

    /// Follows the selected server's direct network access while the Servers section shows it.
    /// Selection, section and connection changes call this; rendering never does.
    pub(super) fn sync_direct_access(&mut self, cx: &mut Context<Self>) {
        let target = if self.active_section == SettingsSectionId::Servers {
            self.selected_server()
        } else {
            None
        };
        if self.direct_access.target != target {
            self.direct_access.start_over(target.clone());
        }
        if target.is_none() || self.direct_access.poll.is_some() {
            return;
        }
        let generation = self.direct_access.generation;
        self.direct_access.poll = Some(cx.spawn(async move |settings, cx| {
            loop {
                // A reconnected server answers through its new connection, so each reading looks
                // the connection up again.
                let Ok(control) = settings.update(cx, |settings, _| {
                    let target = settings.direct_access.target.clone()?;
                    settings.direct_access_control(&target)
                }) else {
                    return;
                };
                let read = match control {
                    Some(control) => Some(
                        cx.background_executor()
                            .spawn(async move {
                                let status = control.status();
                                let devices = status.is_ok().then(|| control.devices().ok());
                                (status, devices.flatten())
                            })
                            .await,
                    ),
                    None => None,
                };
                let Ok(()) = settings.update(cx, |settings, cx| {
                    let state = &mut settings.direct_access;
                    if state.generation != generation {
                        return;
                    }
                    // A change in flight answers for itself; a reading taken meanwhile may
                    // describe the server before it.
                    if state.change.is_none() {
                        match read {
                            Some((Ok(status), devices)) => state.receive(status, devices),
                            Some((Err(failure), _)) => {
                                state.reading = Some(Err(failure));
                                state.devices = None;
                            }
                            None => {
                                state.reading = None;
                                state.devices = None;
                            }
                        }
                    }
                    cx.notify();
                }) else {
                    return;
                };
                cx.background_executor().timer(POLL).await;
            }
        }));
    }

    fn direct_access_presentation(&self) -> AccessPresentation {
        let server = self.direct_access_server_name();
        let unreachable = || match self.direct_access.target {
            Some(ServerChoice::Ssh(_) | ServerChoice::Direct(_)) => format!(
                "Connect to \u{201C}{server}\u{201D} to see or change its direct network access."
            ),
            _ => "Start the server on this computer to see or change its direct network access."
                .to_owned(),
        };
        let mut presentation = match &self.direct_access.reading {
            None if self
                .direct_access
                .target
                .as_ref()
                .and_then(|target| self.direct_access_control(target))
                .is_none() =>
            {
                AccessPresentation {
                    state: Err("Unavailable"),
                    caption: unreachable(),
                    tone: CaptionTone::Guidance,
                    actions: &[],
                }
            }
            None => AccessPresentation {
                state: Err("Checking\u{2026}"),
                caption: String::new(),
                tone: CaptionTone::Guidance,
                actions: &[],
            },
            Some(Err(AccessFailure::Unreachable)) => AccessPresentation {
                state: Err("Unavailable"),
                caption: unreachable(),
                tone: CaptionTone::Guidance,
                actions: &[],
            },
            Some(Err(failure)) => AccessPresentation {
                state: Err("Unavailable"),
                caption: failure.explanation(&server),
                tone: CaptionTone::Error,
                actions: &[],
            },
            Some(Ok(state)) => match (state.policy, state.listener) {
                (_, Listener::Listening(address)) => AccessPresentation {
                    state: Ok(("On", "settings-direct-network-state-on")),
                    caption: format!(
                        "\u{201C}{server}\u{201D} listens at {address}. Only devices you pair \
                         can connect, and pairing gives them no SSH or administrator access."
                    ),
                    tone: CaptionTone::Guidance,
                    actions: &[AccessAction::Change, AccessAction::TurnOff],
                },
                (None, _) => AccessPresentation {
                    state: Ok(("Off", "settings-direct-network-state-off")),
                    caption: format!(
                        "Other computers can\u{2019}t reach \u{201C}{server}\u{201D} over the \
                         network. This computer and SSH connections don\u{2019}t need it."
                    ),
                    tone: CaptionTone::Guidance,
                    actions: &[AccessAction::TurnOn],
                },
                (Some(address), Listener::Failed) => AccessPresentation {
                    state: Ok(("Not Listening", "settings-direct-network-state-failed")),
                    caption: format!(
                        "\u{201C}{server}\u{201D} couldn\u{2019}t listen at {address}. Choose an \
                         address that belongs to that computer and a port no other program \
                         uses."
                    ),
                    tone: CaptionTone::Error,
                    actions: &[AccessAction::Change, AccessAction::TurnOff],
                },
                (Some(address), Listener::Off) => AccessPresentation {
                    state: Ok(("Not Listening", "settings-direct-network-state-idle")),
                    caption: format!(
                        "\u{201C}{server}\u{201D} keeps {address} for direct network \
                         connections but isn\u{2019}t listening now."
                    ),
                    tone: CaptionTone::Warning,
                    actions: &[AccessAction::Change, AccessAction::TurnOff],
                },
            },
        };
        if self.direct_access.change.is_some() {
            presentation.actions = &[];
        }
        if let Some(problem) = self.direct_access.problem {
            presentation.caption = problem.explanation(&server);
            presentation.tone = CaptionTone::Error;
        }
        presentation
    }

    pub(super) fn direct_access_caption(
        &self,
        row: SettingsRowId,
    ) -> Option<(SharedString, CaptionTone)> {
        match row {
            SettingsRowId::DirectNetworkAccess => {
                let presentation = self.direct_access_presentation();
                (!presentation.caption.is_empty())
                    .then(|| (presentation.caption.into(), presentation.tone))
            }
            SettingsRowId::DirectNetworkPairing => {
                let server = self.direct_access_server_name();
                let caption = if self.direct_access.state().is_none() {
                    format!(
                        "Devices paired with \u{201C}{server}\u{201D} appear here while \
                         SpaceTerm can reach it."
                    )
                } else if !self.direct_access.listening() {
                    "Turn on direct network access to pair a device.".to_owned()
                } else {
                    format!(
                        "A paired device can do everything this computer can on \
                         \u{201C}{server}\u{201D}: open, take over and close terminal work, \
                         change Server Settings and manage the server."
                    )
                };
                Some((caption.into(), CaptionTone::Guidance))
            }
            _ => None,
        }
    }

    pub(super) fn render_direct_access(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let presentation = self.direct_access_presentation();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let server = self.direct_access_server_name();
        let state: AnyElement = match (self.direct_access.change, presentation.state) {
            (Some(change), _) => ProgressRing::new(
                SharedString::from(format!("{ACCESS_SELECTOR}-progress")),
                change.progress(),
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(format!("{ACCESS_SELECTOR}-progress"))
            .into_any_element(),
            (None, Ok((label, selector))) => div()
                .debug_selector(move || selector.to_owned())
                .child(badge(selector, label, appearance))
                .into_any_element(),
            (None, Err(value)) => {
                self.render_server_value(SettingsRowId::DirectNetworkAccess, value, appearance)
            }
        };
        let label = match (self.direct_access.change, presentation.state) {
            (Some(change), _) => change.progress(),
            (None, Ok((label, _))) | (None, Err(label)) => label,
        };
        // Assistive technology announces the state when it changes, as a status.
        let state = div()
            .id(SharedString::from(format!("{ACCESS_SELECTOR}-state")))
            .role(accesskit::Role::Status)
            .aria_label(SharedString::from(format!(
                "Direct network access on {server}: {label}"
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .child(state)
            .when(self.direct_access.change.is_some(), |state| {
                state.child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.text_muted))
                        .child(label),
                )
            });
        let owner = cx.weak_entity();
        let buttons = presentation.actions.iter().map(|action| {
            let action = *action;
            let owner = owner.clone();
            Button::new(action.selector(), action.label())
                .variant(if action == AccessAction::TurnOn {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Outline
                })
                .size(ButtonSize::Regular)
                .tab_stop(true)
                .debug_selector(action.selector())
                .accessibility_description(SharedString::from(format!(
                    "{} direct network access on {server}",
                    action.label().trim_end_matches('\u{2026}')
                )))
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |settings, cx| match action {
                        AccessAction::TurnOn | AccessAction::Change => {
                            settings.ask_for_address(action, window, cx)
                        }
                        AccessAction::TurnOff => settings.confirm_turn_off(window, cx),
                    });
                })
        });
        div()
            .debug_selector(|| ACCESS_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(state)
            .children(buttons)
            .into_any_element()
    }

    /// Asks where the selected server should listen, and saves that address once the person
    /// confirms it and the server applies it.
    fn ask_for_address(
        &mut self,
        action: AccessAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let Some(control) = self.direct_access_control(&target) else {
            return;
        };
        let server = self.direct_access_server_name();
        // The current address, or this computer's own for its own server. A remote server's
        // addresses are its own, so SpaceTerm suggests none for it.
        let suggested = self
            .direct_access
            .state()
            .and_then(|state| state.policy)
            .map(|address| address.to_string())
            .or_else(|| {
                (target == ServerChoice::Local)
                    .then(this_computers_address)
                    .flatten()
                    .map(|address| address.to_string())
            })
            .unwrap_or_default();
        let form = cx.new(|cx| AddressForm::new(server.clone(), suggested, window, cx));
        let focus = form.read(cx).input.read(cx).focus_handle();
        let (title, confirm, change) = match action {
            AccessAction::Change => ("Change Address", "Change Address", Change::Moving),
            _ => (
                "Turn On Direct Network Access",
                "Turn On",
                Change::TurningOn,
            ),
        };
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let deciding = form.downgrade();
        let window_handle = window.window_handle();
        let dialog = Dialog::new(
            ModalId::new("settings-direct-network-address"),
            format!("{title} on {server}"),
            title,
            vec![
                ModalAction::new(
                    true,
                    confirm,
                    ModalActionRole::Affirmative,
                    ADDRESS_CONFIRM_SELECTOR,
                )
                .default_action(true),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    ADDRESS_CANCEL_SELECTOR,
                ),
            ],
            DialogInitialFocus::Body(focus),
        )
        .description(format!(
            "Choose where \u{201C}{server}\u{201D} listens for devices you pair. Devices that \
             can reach this address can ask to pair, and only devices you approve connect."
        ))
        .size(DialogSize::Regular)
        .body(form);
        let result = dialog.present(
            window,
            cx,
            move |request, completion, cx| {
                if !*request.action_id() {
                    return DialogCloseDecision::Allow;
                }
                let Some(form) = deciding.upgrade() else {
                    return DialogCloseDecision::Allow;
                };
                let policy = form.update(cx, |form, cx| form.policy(cx));
                let policy = match policy {
                    Ok(policy) => policy,
                    Err(focus) => {
                        return DialogCloseDecision::Deny {
                            first_invalid: Some(focus),
                        };
                    }
                };
                let started = owner
                    .update(cx, |settings, cx| {
                        settings.apply_policy(
                            generation,
                            Arc::clone(&control),
                            Some(policy),
                            change,
                            Some((form.downgrade(), completion, window_handle)),
                            cx,
                        )
                    })
                    .unwrap_or(false);
                if started {
                    DialogCloseDecision::Pending
                } else {
                    DialogCloseDecision::Deny {
                        first_invalid: None,
                    }
                }
            },
            |_: DialogOutcome<bool>, _| {},
        );
        if result.is_err() {
            eprintln!("failed to present the direct network address dialog");
        }
    }

    /// Asks before the selected server stops listening, naming the server and what ends.
    fn confirm_turn_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let Some(control) = self.direct_access_control(&target) else {
            return;
        };
        let server = self.direct_access_server_name();
        let address = self
            .direct_access
            .state()
            .and_then(|state| state.policy)
            .map_or_else(|| "its address".to_owned(), |address| address.to_string());
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let title = format!("Turn off direct network access on \u{201C}{server}\u{201D}?");
        let result = Alert::new(
            ModalId::new("settings-direct-network-turn-off"),
            title.clone(),
            title,
            format!(
                "\u{201C}{server}\u{201D} stops listening at {address}, and paired devices \
                 connected over the network disconnect."
            ),
            vec![
                ModalAction::new(
                    true,
                    "Turn Off",
                    ModalActionRole::Affirmative,
                    TURN_OFF_CONFIRM_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    TURN_OFF_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Informational)
        .detail(
            "Devices stay paired, so they can connect again after you turn it back on. Terminal \
             Sessions keep running.",
        )
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                settings.apply_policy(generation, control, None, Change::TurningOff, None, cx)
            });
        });
        if result.is_err() {
            eprintln!("failed to present the direct network turn-off confirmation");
        }
    }

    /// Saves `policy` on the server the rows presented when the person confirmed it, and
    /// answers whether it asked. A dialog that asked for it stays open until the server answers,
    /// and shows a refusal itself.
    fn apply_policy(
        &mut self,
        generation: u64,
        control: Arc<dyn DirectAccessControl>,
        policy: Option<NetworkPolicy>,
        change: Change,
        dialog: Option<(
            gpui::WeakEntity<AddressForm>,
            DialogPendingCompletion,
            AnyWindowHandle,
        )>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.direct_access.generation != generation || self.direct_access.change.is_some() {
            return false;
        }
        self.direct_access.change = Some(change);
        self.direct_access.problem = None;
        if let Some((form, _, _)) = &dialog {
            let _ = form.update(cx, |form, cx| form.set_pending(true, cx));
        }
        self.direct_access.work = Some(cx.spawn(async move |settings, cx| {
            let applied = cx
                .background_executor()
                .spawn(async move { control.set_policy(policy) })
                .await;
            let failure = applied.as_ref().err().copied();
            let _ = settings.update(cx, |settings, cx| {
                let state = &mut settings.direct_access;
                if state.generation != generation {
                    return;
                }
                state.change = None;
                match applied {
                    Ok(status) => state.receive(status, None),
                    // A dialog shows its own refusal; a confirmation that closed already leaves
                    // it on the row.
                    Err(failure) if dialog.is_none() => state.problem = Some(failure),
                    Err(_) => {}
                }
                cx.notify();
            });
            let Some((form, completion, window)) = dialog else {
                return;
            };
            let _ = window.update(cx, |_, window, cx| match failure {
                None => {
                    let _ = completion.allow(window, None, cx);
                }
                Some(failure) => {
                    let focus = form.update(cx, |form, cx| form.refuse(failure, cx)).ok();
                    let _ = completion.deny(window, focus, cx);
                }
            });
        }));
        cx.notify();
        true
    }

    pub(super) fn render_paired_devices(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(devices) = self.direct_access.devices.clone() else {
            return self.render_server_value(
                SettingsRowId::DirectNetworkPairing,
                if self.direct_access.state().is_some() {
                    "Unknown"
                } else {
                    "Unavailable"
                },
                appearance,
            );
        };
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let server = self.direct_access_server_name();
        let owner = cx.weak_entity();
        let count = devices.len();
        let this_device = self.this_device_pairing();
        let entries = devices.into_iter().enumerate().map(|(index, device)| {
            let unpairing = self.direct_access.unpairing.contains(&device.pairing);
            let own = this_device == Some(device.pairing);
            let selector = format!("{UNPAIR_SELECTOR}-{index}");
            let unpair = (!unpairing).then(|| {
                let owner = owner.clone();
                let device = device.clone();
                Button::new(
                    ElementId::from(SharedString::from(selector.clone())),
                    "Unpair\u{2026}",
                )
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .tab_stop(true)
                .debug_selector(selector.clone())
                .accessibility_description(SharedString::from(if own {
                    format!("Unpair this device from {server}")
                } else {
                    format!("Unpair {} from {server}", device.name)
                }))
                .on_activate(move |_, window, cx| {
                    let device = device.clone();
                    let _ = owner.update(cx, |settings, cx| {
                        // Unpairing this device removes its connection here as well.
                        if own {
                            settings.confirm_unpair_this_device(window, cx);
                        } else {
                            settings.confirm_unpair(device, window, cx);
                        }
                    });
                })
            });
            div()
                .id(ElementId::from(SharedString::from(format!(
                    "{DEVICES_SELECTOR}-{index}"
                ))))
                .debug_selector(move || format!("{DEVICES_SELECTOR}-{index}"))
                .role(accesskit::Role::ListItem)
                .aria_label(if own {
                    SharedString::from(format!("{}, this device", device.name))
                } else {
                    device.name.clone()
                })
                .aria_description(SharedString::from(format!(
                    "Device fingerprint {}",
                    device.fingerprint
                )))
                .aria_position_in_set(index + 1)
                .aria_size_of_set(count)
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .w_full()
                .py(appearance.spacing(4.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(appearance.spacing(2.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(appearance.spacing(6.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .chrome_text(appearance.typography.style(TextRole::Body))
                                        .text_color(gpui_color(colors.text))
                                        .child(device.name.clone()),
                                )
                                .when(own, |name| {
                                    name.child(badge(
                                        SharedString::from(format!(
                                            "{DEVICES_SELECTOR}-{index}-this-device"
                                        )),
                                        "This Device",
                                        appearance,
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(colors.text_secondary))
                                .child(SharedString::from(format!(
                                    "Device fingerprint {}",
                                    device.fingerprint
                                ))),
                        ),
                )
                .when(unpairing, |row| {
                    row.child(badge(
                        SharedString::from(format!("{selector}-pending")),
                        "Unpairing\u{2026}",
                        appearance,
                    ))
                })
                .children(unpair)
        });
        let empty = count == 0;
        let pair = {
            let owner = owner.clone();
            Button::new(PAIR_SELECTOR, "Pair Device\u{2026}")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .tab_stop(true)
                .disabled(!self.direct_access.listening())
                .debug_selector(PAIR_SELECTOR)
                .accessibility_description(SharedString::from(format!(
                    "Pair another device with {server}"
                )))
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |settings, cx| settings.pair_device(window, cx));
                })
        };
        div()
            .debug_selector(|| DEVICES_SELECTOR.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(6.0))
            .child(
                div()
                    .id(DEVICES_SELECTOR)
                    .role(accesskit::Role::List)
                    .aria_label(SharedString::from(format!("Devices paired with {server}")))
                    .flex()
                    .flex_col()
                    .w_full()
                    .when(empty, |list| {
                        list.child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Body))
                                .text_color(gpui_color(colors.text_secondary))
                                .child(gpui::Text::new(
                                    ElementId::from("settings-paired-devices-none"),
                                    SharedString::new_static("No devices are paired."),
                                )),
                        )
                    })
                    .children(entries),
            )
            .child(div().flex().flex_row().child(pair))
            .into_any_element()
    }

    /// Opens Pair Device for the selected server. A device the person pairs there appears in the
    /// list at once.
    fn pair_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let (Some(control), Some(state)) = (
            self.direct_access_control(&target),
            self.direct_access.state().cloned(),
        ) else {
            return;
        };
        let generation = self.direct_access.generation;
        let presented = crate::ui::direct_pairing::PairDevice::present(
            control,
            self.direct_access_server_name(),
            state.server_fingerprint,
            move |settings: &mut SettingsWindow, cx| settings.reread_devices(generation, cx),
            window,
            cx,
        );
        if !presented {
            eprintln!("failed to present Pair Device");
        }
    }

    /// Reads the selected server's devices now, rather than at the next reading.
    fn reread_devices(&mut self, generation: u64, cx: &mut Context<Self>) {
        let Some(control) = self
            .direct_access
            .target
            .clone()
            .filter(|_| self.direct_access.generation == generation)
            .and_then(|target| self.direct_access_control(&target))
        else {
            return;
        };
        self.direct_access.work = Some(cx.spawn(async move |settings, cx| {
            let devices = cx
                .background_executor()
                .spawn(async move { control.devices() })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                if settings.direct_access.generation == generation
                    && let Ok(devices) = devices
                {
                    settings.direct_access.devices = Some(devices);
                    cx.notify();
                }
            });
        }));
    }

    /// Asks before unpairing exactly `device` from the selected server.
    fn confirm_unpair(&mut self, device: Device, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let server = self.direct_access_server_name();
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let title = format!("Unpair \u{201C}{}\u{201D}?", device.name);
        let result = Alert::new(
            ModalId::new("settings-unpair-device"),
            title.clone(),
            title,
            format!(
                "\u{201C}{}\u{201D} can no longer connect to \u{201C}{server}\u{201D} over the \
                 network. Its connections end and it gives up control of its Panes.",
                device.name
            ),
            vec![
                ModalAction::new(
                    true,
                    "Unpair",
                    ModalActionRole::Affirmative,
                    UNPAIR_CONFIRM_SELECTOR,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    UNPAIR_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .detail(format!(
            "Device fingerprint {}. Terminal Sessions keep running, and access from this computer \
             and over SSH isn\u{2019}t affected.",
            device.fingerprint
        ))
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                settings.unpair_device(generation, &target, device.pairing, cx);
            });
        });
        if result.is_err() {
            eprintln!("failed to present the Unpair Device confirmation");
        }
    }

    /// Records the unpair against the connection the person confirmed it on. The record
    /// survives relaunch and finishes on its own; the device leaves the list once the server
    /// revokes it.
    fn unpair_device(
        &mut self,
        generation: u64,
        target: &ServerChoice,
        pairing: PairingId,
        cx: &mut Context<Self>,
    ) {
        if self.direct_access.generation != generation
            || self.direct_access.target.as_ref() != Some(target)
        {
            return;
        }
        let Some(registry) = crate::ui::direct_servers::installed(cx) else {
            self.direct_access.problem = Some(AccessFailure::Unreachable);
            cx.notify();
            return;
        };
        // A paired device unpairs another with its own pairing; an account route records the
        // account connection it was confirmed on.
        let request: Box<
            dyn FnOnce() -> Result<(), crate::local_server::direct::DirectError> + Send,
        > = match target {
            ServerChoice::Direct(route) => {
                let route = *route;
                Box::new(move || registry.unpair(&route, pairing))
            }
            _ => {
                let Some(server) = self.direct_access_server(target) else {
                    self.direct_access.problem = Some(AccessFailure::Unreachable);
                    cx.notify();
                    return;
                };
                let label = self.direct_access_server_name().to_string();
                Box::new(move || registry.revoke_other(server, pairing, label))
            }
        };
        self.direct_access.unpairing.push(pairing);
        self.direct_access.problem = None;
        self.direct_access.work = Some(cx.spawn(async move |settings, cx| {
            let recorded = cx
                .background_executor()
                .spawn(async move { request() })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                let state = &mut settings.direct_access;
                if state.generation != generation {
                    return;
                }
                if let Err(error) = recorded {
                    state.unpairing.retain(|pending| *pending != pairing);
                    state.problem = Some(AccessFailure::from(error));
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

/// The address field in Turn On and Change Address.
pub(super) struct AddressForm {
    server: SharedString,
    input: Entity<TextInput>,
    problem: Option<AccessFailure>,
    pending: bool,
}

impl AddressForm {
    fn new(
        server: SharedString,
        address: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(ADDRESS_SELECTOR, "Address", address, window, cx)
                .variant(TextInputVariant::Bare)
                .placeholder("192.168.1.20")
                .input_length_limit(Some(64))
                .debug_selector(ADDRESS_SELECTOR)
        });
        Self {
            server,
            input,
            problem: None,
            pending: false,
        }
    }

    /// The policy the typed address names, or the field to focus when it names none.
    fn policy(&mut self, cx: &mut Context<Self>) -> Result<NetworkPolicy, FocusHandle> {
        if self.pending {
            return Err(self.input.read(cx).focus_handle());
        }
        let typed = self.input.read(cx).value().trim().to_owned();
        match NetworkPolicy::parse(&typed) {
            Ok(policy) => {
                self.problem = None;
                cx.notify();
                Ok(policy)
            }
            Err(error) => {
                self.problem = Some(AccessFailure::from(error));
                cx.notify();
                Err(self.input.read(cx).focus_handle())
            }
        }
    }

    fn set_pending(&mut self, pending: bool, cx: &mut Context<Self>) {
        self.pending = pending;
        cx.notify();
    }

    fn refuse(&mut self, failure: AccessFailure, cx: &mut Context<Self>) -> FocusHandle {
        self.pending = false;
        self.problem = Some(failure);
        cx.notify();
        self.input.read(cx).focus_handle()
    }
}

impl Render for AddressForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = crate::ui::appearance::shared_chrome(cx);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let focus = self.input.read(cx).focus_handle();
        let field = spaceterm_ui::field_frame(
            "settings-direct-network-address-frame",
            &focus,
            FieldState::default()
                .disabled(self.pending)
                .invalid(self.problem.is_some()),
            RadiusRole::Control.pixels(),
            cx,
        )
        .h(appearance.height(28.0, 13.0))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .px(appearance.spacing(8.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(colors.text))
        .on_click(move |_, window, cx| {
            focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(self.input.clone());
        let (note, color) = match self.problem {
            Some(problem) => (problem.explanation(&self.server), colors.error),
            None => (
                format!(
                    "Enter one of \u{201C}{}\u{201D}\u{2019}s own IP addresses. The port is {} \
                     unless you add another, such as 192.168.1.20:9300.",
                    self.server,
                    crate::local_server::direct::DEFAULT_PORT
                ),
                colors.text_muted,
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap(appearance.spacing(6.0))
            .child(
                div()
                    .text_color(gpui_color(colors.text_muted))
                    .child("Address"),
            )
            .child(field)
            .child(
                div()
                    .id("settings-direct-network-address-note")
                    .when(self.problem.is_some(), |note| {
                        note.role(accesskit::Role::Alert)
                    })
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(color))
                    .whitespace_normal()
                    .child(gpui::Text::new(
                        ElementId::from("settings-direct-network-address-note-text"),
                        SharedString::from(note),
                    )),
            )
            .when(self.pending, |form| {
                form.child(
                    ProgressRing::new(
                        "settings-direct-network-address-progress",
                        "Saving\u{2026}",
                        ProgressState::Indeterminate,
                    )
                    .size(ProgressSize::Compact),
                )
            })
    }
}

#[cfg(test)]
#[path = "direct_access_tests.rs"]
mod tests;
