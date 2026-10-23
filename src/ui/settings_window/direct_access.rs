//! Network Access for the selected server: whether other devices may connect, at which of that
//! server's addresses, and the devices paired with it.
//!
//! Network Access appears for this computer's server and SSH servers only. A paired connection
//! carries no authority over its server's network. Every step runs over SpaceTerm's existing
//! connection to the selected server, with the authority that connection already has, and the
//! addresses offered are the ones that server reads from its own machine. The rows read the
//! server only while Servers shows it, and choosing another server starts over, so nothing read
//! or chosen for one server reaches another.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, ElementId, SharedString, Task, Window, accesskit, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, ComboBoxItem, Icon,
    IconName, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId,
    ProgressRing, ProgressSize, ProgressState, Switch, ToggleSize,
};

use super::SettingsWindow;
use super::catalog::SettingsSectionId;
use super::server_list::ServerChoice;
use crate::local_server::LocalServer;
use crate::local_server::direct::{DEFAULT_PORT, NetworkPolicy, PairingId};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{AccessFailure, AccessState, Device, DirectAccessControl, Listener};
use crate::ui::sidebar_window::form::FormRow;

pub(super) const ACCESS_SELECTOR: &str = "settings-network-access";
pub(super) const ADDRESS_SELECTOR: &str = "settings-network-address";
pub(super) const TURN_OFF_CONFIRM_SELECTOR: &str = "settings-network-access-turn-off-confirm";
pub(super) const TURN_OFF_CANCEL_SELECTOR: &str = "settings-network-access-turn-off-cancel";
pub(super) const DEVICES_SELECTOR: &str = "settings-paired-devices";
pub(super) const INVITE_SELECTOR: &str = "settings-invite-device";
/// Prefixes each paired device's row and Remove button, followed by its position.
pub(super) const DEVICE_SELECTOR: &str = "settings-paired-device";
pub(super) const REMOVE_CONFIRM_SELECTOR: &str = "settings-paired-device-remove-confirm";
pub(super) const REMOVE_CANCEL_SELECTOR: &str = "settings-paired-device-remove-cancel";
/// How often the rows read the selected server while Servers shows it.
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

/// What the rows know about the selected server's network access.
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
    /// Devices the person asked to remove that the server still lists.
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

/// The address the switch turns access on at: the one the person chose, otherwise the one the
/// server's routing table suggests, otherwise its first address. `None` when it has none.
pub(super) fn address_to_use(state: &AccessState, chosen: Option<IpAddr>) -> Option<IpAddr> {
    let addresses = state.addresses.as_ref()?;
    chosen
        .filter(|chosen| addresses.available.contains(chosen))
        .or(addresses.suggested)
        .or_else(|| addresses.available.first().copied())
}

/// What the Allow row says under its control, if anything is wrong.
pub(super) fn access_problem(state: &AccessState, server: &str) -> Option<String> {
    let address = state.policy.map(|policy| policy.ip());
    match (state.listener, &state.addresses) {
        (Listener::AddressGone, _) => Some(match address {
            Some(address) => {
                format!("{address} is no longer on {server}. Choose another address.")
            }
            None => format!("The chosen address is no longer on {server}. Choose another."),
        }),
        (Listener::Failed, _) => Some(match address {
            Some(address) => format!("{server} couldn\u{2019}t listen at {address}."),
            None => format!("{server} couldn\u{2019}t listen on the network."),
        }),
        (Listener::Off, Some(addresses)) if addresses.available.is_empty() => Some(format!(
            "{server} has no network address other devices can reach."
        )),
        (_, None) => Some(format!(
            "{server} couldn\u{2019}t read its network addresses."
        )),
        _ => None,
    }
}

/// How long ago a device connected, as the person reads it.
pub(super) fn last_connected(at: Option<u64>, now: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let Some(at) = at else {
        return "Never connected".to_owned();
    };
    let plural = |count: u64, unit: &str| {
        if count == 1 {
            format!("1 {unit}")
        } else {
            format!("{count} {unit}s")
        }
    };
    match now.saturating_sub(at) {
        age if age < MINUTE => "Last connected just now".to_owned(),
        age if age < HOUR => format!("Last connected {} ago", plural(age / MINUTE, "minute")),
        age if age < DAY => format!("Last connected {} ago", plural(age / HOUR, "hour")),
        age if age < 2 * DAY => "Last connected yesterday".to_owned(),
        age => format!("Last connected {} ago", plural(age / DAY, "day")),
    }
}

impl SettingsWindow {
    /// The server's direct network operations over SpaceTerm's existing connection to it.
    fn direct_access_control(&self, target: &ServerChoice) -> Option<Arc<dyn DirectAccessControl>> {
        match target {
            ServerChoice::Local => self.local_server.as_ref()?.direct_access(),
            ServerChoice::Ssh(_) => self
                .direct_access_server(target)
                .map(|server| Arc::new(server) as Arc<dyn DirectAccessControl>),
            // A paired connection carries no authority over the server's network.
            ServerChoice::Direct(_) => None,
        }
    }

    /// The account connection a removal is recorded against, so its retries keep that exact
    /// route and authority.
    fn direct_access_server(&self, target: &ServerChoice) -> Option<LocalServer> {
        match target {
            ServerChoice::Local => self.local_server.as_ref()?.worktree_server(),
            ServerChoice::Ssh(destination) => {
                self.ssh_servers.as_ref()?.connections.server(destination)
            }
            ServerChoice::Direct(_) => None,
        }
    }

    /// The selected server's name as the rows say it: the name the server gave, once it has.
    fn direct_access_server_name(&self) -> SharedString {
        self.direct_access
            .state()
            .map(|state| state.server_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.selected_server_name())
    }

    /// Whether the selected server lets other devices connect, as it last said.
    pub(super) fn network_access_on(&self) -> bool {
        self.direct_access
            .state()
            .is_some_and(|state| state.policy.is_some())
    }

    /// Whether SpaceTerm can change the selected server's network access now.
    pub(super) fn network_access_live(&self) -> bool {
        self.direct_access.state().is_some()
    }

    /// The Network Access card's footnote while its rows can't change.
    pub(super) fn network_access_footnote(&self) -> Option<SharedString> {
        if self.network_access_live() {
            return None;
        }
        let name = self.selected_server_name();
        Some(SharedString::from(match &self.direct_access.target {
            Some(ServerChoice::Local) => {
                format!("Start the server on {name} to change its network access.")
            }
            _ => format!("Connect to {name} to change its network access."),
        }))
    }

    /// Follows the selected server's network access while Servers shows it. Selection, section
    /// and connection changes call this; rendering never does.
    pub(super) fn sync_direct_access(&mut self, cx: &mut Context<Self>) {
        let target = if self.active_section == SettingsSectionId::Servers {
            self.selected_server()
                .filter(|target| !matches!(target, ServerChoice::Direct(_)))
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

    /// The Allow row's description: where the server listens, or what turning it on does.
    pub(super) fn network_access_description(&self) -> SharedString {
        match self.direct_access.state() {
            Some(AccessState {
                listener: Listener::Listening(address),
                ..
            }) => SharedString::from(format!("Listening on {address}.")),
            _ => SharedString::new_static("Devices you invite can connect over the network."),
        }
    }

    /// Why the Allow row's control can't do what it shows, under the control.
    pub(super) fn network_access_error(&self) -> Option<SharedString> {
        let server = self.direct_access_server_name();
        if let Some(problem) = self.direct_access.problem {
            return Some(problem.explanation(&server).into());
        }
        match &self.direct_access.reading {
            Some(Ok(state)) => access_problem(state, &server).map(SharedString::from),
            Some(Err(AccessFailure::Unreachable)) | None => None,
            Some(Err(failure)) => Some(failure.explanation(&server).into()),
        }
    }

    /// The Allow switch, and the address chooser beside it when the server has more than one
    /// address or its chosen address is gone.
    pub(super) fn render_network_access(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let server = self.direct_access_server_name();
        let state = self.direct_access.state().cloned();
        let change = self.direct_access.change;
        let on = state.as_ref().is_some_and(|state| state.policy.is_some());
        let gone = state
            .as_ref()
            .is_some_and(|state| state.listener == Listener::AddressGone);
        let addresses: Vec<IpAddr> = state
            .as_ref()
            .and_then(|state| state.addresses.as_ref())
            .map(|addresses| addresses.available.clone())
            .unwrap_or_default();
        let suggested = state
            .as_ref()
            .and_then(|state| state.addresses.as_ref())
            .and_then(|addresses| addresses.suggested);
        let chosen = state
            .as_ref()
            .and_then(|state| state.policy)
            .map(|policy| policy.ip())
            .or_else(|| state.as_ref().and_then(|state| address_to_use(state, None)));
        let can_turn_on = state
            .as_ref()
            .and_then(|state| address_to_use(state, None))
            .is_some();
        let owner = cx.weak_entity();
        let chooser = (addresses.len() > 1 || (gone && !addresses.is_empty())).then(|| {
            let owner = owner.clone();
            let items = addresses
                .iter()
                .map(|address| {
                    let item = ComboBoxItem::new(*address, address.to_string());
                    if Some(*address) == suggested {
                        item.description("Suggested")
                    } else {
                        item
                    }
                })
                .collect();
            super::settings_selector(
                ADDRESS_SELECTOR.to_owned(),
                SharedString::from(format!("Address on {server}")),
                chosen.filter(|address| addresses.contains(address)),
                "Choose an address",
                items,
                appearance,
            )
            .disabled(change.is_some())
            .on_accept(move |acceptance, window, cx| {
                let address = *acceptance.item_id();
                let _ = owner.update(cx, |settings, cx| {
                    settings.choose_address(address, window, cx);
                });
            })
            .into_any_element()
        });
        let progress = change.map(|change| {
            ProgressRing::new(
                SharedString::from(format!("{ACCESS_SELECTOR}-progress")),
                change.progress(),
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .into_any_element()
        });
        let switch = Switch::new(
            ACCESS_SELECTOR,
            SharedString::from(format!("Allow other devices to connect to {server}")),
            on,
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(state.is_none() || change.is_some() || (!on && !can_turn_on))
        .debug_selector(ACCESS_SELECTOR)
        .on_change(move |change, window, cx| {
            let enabled = change.requested();
            let _ = owner.update(cx, |settings, cx| {
                if enabled {
                    settings.turn_on_network_access(None, cx);
                } else {
                    settings.confirm_turn_off(window, cx);
                }
            });
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .children(progress)
            .children(chooser.filter(|_| on || gone))
            .child(switch)
            .into_any_element()
    }

    /// Moves access to `address`, or turns it on there, keeping the chosen port.
    fn choose_address(&mut self, address: IpAddr, _: &mut Window, cx: &mut Context<Self>) {
        let on = self.network_access_on();
        if on {
            self.apply_address(address, Change::Moving, cx);
        } else {
            self.turn_on_network_access(Some(address), cx);
        }
    }

    /// Turns access on at the address the person chose, or the one the server suggests.
    fn turn_on_network_access(&mut self, chosen: Option<IpAddr>, cx: &mut Context<Self>) {
        let Some(address) = self
            .direct_access
            .state()
            .and_then(|state| address_to_use(state, chosen))
        else {
            return;
        };
        self.apply_address(address, Change::TurningOn, cx);
    }

    fn apply_address(&mut self, address: IpAddr, change: Change, cx: &mut Context<Self>) {
        let port = self
            .direct_access
            .state()
            .and_then(|state| state.policy)
            .map_or(DEFAULT_PORT, |policy| policy.port());
        let Ok(policy) = NetworkPolicy::new(SocketAddr::new(address, port)) else {
            self.direct_access.problem = Some(AccessFailure::InvalidAddress);
            cx.notify();
            return;
        };
        let Some(control) = self
            .direct_access
            .target
            .clone()
            .and_then(|target| self.direct_access_control(&target))
        else {
            return;
        };
        let generation = self.direct_access.generation;
        self.apply_policy(generation, control, Some(policy), change, cx);
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
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let title = format!("Stop other devices from connecting to {server}?");
        let result = Alert::new(
            ModalId::new("settings-network-access-turn-off"),
            title.clone(),
            title,
            "Paired devices connected over the network disconnect.",
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
            "Devices stay paired and can connect again when you turn this back on. Terminal \
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
                settings.apply_policy(generation, control, None, Change::TurningOff, cx)
            });
        });
        if result.is_err() {
            eprintln!("failed to present the network access turn-off confirmation");
        }
    }

    /// Saves `policy` on the server the rows presented when the person chose it. The switch shows
    /// progress until the server answers, and a refusal under it.
    fn apply_policy(
        &mut self,
        generation: u64,
        control: Arc<dyn DirectAccessControl>,
        policy: Option<NetworkPolicy>,
        change: Change,
        cx: &mut Context<Self>,
    ) {
        if self.direct_access.generation != generation || self.direct_access.change.is_some() {
            return;
        }
        self.direct_access.change = Some(change);
        self.direct_access.problem = None;
        self.direct_access.work = Some(cx.spawn(async move |settings, cx| {
            let applied = cx
                .background_executor()
                .spawn(async move { control.set_policy(policy) })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                let state = &mut settings.direct_access;
                if state.generation != generation {
                    return;
                }
                state.change = None;
                match applied {
                    Ok(status) => state.receive(status, None),
                    Err(failure) => state.problem = Some(failure),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The Devices row's value: how many devices are paired.
    fn paired_count(&self) -> SharedString {
        match self
            .direct_access
            .devices
            .as_ref()
            .map(|devices| devices.len())
        {
            Some(0) | None => SharedString::new_static("None"),
            Some(count) => SharedString::from(format!("{count} paired")),
        }
    }

    /// The Devices row: how many devices are paired, and Invite Device.
    pub(super) fn render_paired_devices(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let server = self.direct_access_server_name();
        let listening = self
            .direct_access
            .state()
            .is_some_and(|state| matches!(state.listener, Listener::Listening(_)));
        let owner = cx.weak_entity();
        let count = self.paired_count();
        div()
            .debug_selector(|| DEVICES_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(
                div()
                    .id(SharedString::from(format!("{DEVICES_SELECTOR}-count")))
                    .role(accesskit::Role::Status)
                    .aria_label(SharedString::from(format!("Devices: {count}")))
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(colors.text_secondary))
                    .child(count.clone()),
            )
            .child(
                Button::new(INVITE_SELECTOR, "Invite Device\u{2026}")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .disabled(!listening)
                    .tab_stop(true)
                    .debug_selector(INVITE_SELECTOR)
                    .accessibility_description(SharedString::from(format!(
                        "Show an invitation a phone or computer uses to connect to {server}"
                    )))
                    .on_activate(move |_, window, cx| {
                        let _ = owner.update(cx, |settings, cx| settings.invite_device(window, cx));
                    }),
            )
            .into_any_element()
    }

    /// One row per paired device: its name, when it last connected, and Remove.
    pub(super) fn render_device_rows(
        &mut self,
        appearance: &ChromeAppearance,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(devices) = self.direct_access.devices.clone() else {
            return Vec::new();
        };
        let now = crate::ui::direct_access::unix_now();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let dimmed = !self.network_access_live();
        devices
            .into_iter()
            .enumerate()
            .map(|(index, device)| {
                let removing = self.direct_access.unpairing.contains(&device.pairing);
                let selector = format!("{DEVICE_SELECTOR}-{index}");
                let owner = cx.weak_entity();
                let name = device.name.clone();
                let control = if removing {
                    ProgressRing::new(
                        SharedString::from(format!("{selector}-progress")),
                        "Removing\u{2026}",
                        ProgressState::Indeterminate,
                    )
                    .size(ProgressSize::Compact)
                    .into_any_element()
                } else {
                    let removed = device.clone();
                    Button::new(
                        ElementId::from(SharedString::from(format!("{selector}-remove"))),
                        "Remove\u{2026}",
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .tab_stop(true)
                    .disabled(dimmed)
                    .debug_selector(format!("{selector}-remove"))
                    .accessibility_description(SharedString::from(format!(
                        "Remove {name}. SpaceTerm asks first."
                    )))
                    .on_activate(move |_, window, cx| {
                        let device = removed.clone();
                        let _ = owner.update(cx, |settings, cx| {
                            settings.confirm_remove_device(device, window, cx);
                        });
                    })
                    .into_any_element()
                };
                let icon =
                    div()
                        .flex_none()
                        .text_color(gpui_color(colors.icon))
                        .child(Icon::inherited(
                            IconName::AppWindow,
                            appearance.icons.metrics(IconRole::Row).glyph_size,
                        ));
                let row = FormRow::new(selector.clone(), name, control)
                    .description(last_connected(device.last_connected_at, now))
                    .dimmed(dimmed)
                    .render(appearance, window, cx);
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .pl(appearance.spacing(12.0))
                    .child(icon)
                    .child(div().flex_1().min_w_0().child(row))
                    .into_any_element()
            })
            .collect()
    }

    /// Opens Invite Device on the selected server.
    fn invite_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let Some(control) = self.direct_access_control(&target) else {
            return;
        };
        let generation = self.direct_access.generation;
        let presented = crate::ui::direct_pairing::InviteDevice::present(
            control,
            self.direct_access_server_name(),
            move |settings: &mut SettingsWindow, cx| settings.reread_devices(generation, cx),
            window,
            cx,
        );
        if !presented {
            eprintln!("failed to present Invite Device");
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

    /// Asks before removing exactly `device` from the selected server.
    fn confirm_remove_device(
        &mut self,
        device: Device,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let server = self.direct_access_server_name();
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let title = format!("Remove \u{201C}{}\u{201D}?", device.name);
        let result = Alert::new(
            ModalId::new("settings-paired-device-remove"),
            title.clone(),
            title,
            format!(
                "\u{201C}{}\u{201D} can no longer connect to {server}. If it\u{2019}s connected \
                 now, it disconnects and gives up control of its Panes.",
                device.name
            ),
            vec![
                ModalAction::new(
                    true,
                    "Remove",
                    ModalActionRole::Affirmative,
                    REMOVE_CONFIRM_SELECTOR,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    REMOVE_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .detail(
            "Terminal Sessions keep running. To connect again, the device needs a new invitation.",
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
                settings.remove_device(generation, &target, device.pairing, cx);
            });
        });
        if result.is_err() {
            eprintln!("failed to present the device removal confirmation");
        }
    }

    /// Records the removal against the connection the person confirmed it on. The record
    /// survives relaunch and finishes on its own; the device leaves the list once the server
    /// revokes it.
    fn remove_device(
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
        let Some(server) = self.direct_access_server(target) else {
            self.direct_access.problem = Some(AccessFailure::Unreachable);
            cx.notify();
            return;
        };
        let label = self.direct_access_server_name().to_string();
        self.direct_access.unpairing.push(pairing);
        self.direct_access.problem = None;
        self.direct_access.work = Some(cx.spawn(async move |settings, cx| {
            let recorded = cx
                .background_executor()
                .spawn(async move { registry.revoke_other(server, pairing, label) })
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

#[cfg(test)]
#[path = "direct_access_tests.rs"]
mod tests;
