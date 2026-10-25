//! Network Access for the selected server: whether other devices may connect, at which of that
//! server's addresses, and the devices paired with it.
//!
//! Network Access appears for this computer's server and every paired server. Every step runs
//! over SpaceTerm's existing connection to the selected server, with the authority that
//! connection already has, and the addresses offered are the ones that server reads from its own
//! machine. A paired server's steps run over this computer's own network connection, so turning
//! access off, moving its address or unpairing this computer says that this computer disconnects
//! too. The rows read the
//! server only while Servers shows it, and choosing another server starts over, so nothing read
//! or chosen for one server reaches another.
//!
//! Losing the connection to a server changes nothing on it. While SpaceTerm can't reach a server,
//! the rows show what that server last reported, dimmed and dated, and offer no step that would
//! change it. A server never read shows its access as unknown rather than off. What each server
//! reported is retained with its other last-known facts, so it shows again after relaunch.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, ElementId, SharedString, Task, Window, accesskit, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, ComboBoxItem, Icon,
    IconName, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId,
    ProgressRing, ProgressSize, ProgressState, Switch, ToggleSize,
};

use super::SettingsWindow;
use super::catalog::SettingsSectionId;
use super::server_list::ServerChoice;
use super::server_selection;
use super::servers::LocalServerStatus;
use crate::direct_connections::DirectRouteId;
use crate::server_connection::direct::{DEFAULT_PORT, NetworkPolicy, PairingId};
use crate::server_connection::{ServerConnection, ServerError};
use crate::server_context::{
    LastKnownFacts, NetworkAccessFacts, NetworkListenerFacts, PairedDeviceFacts, PairedDevicesFacts,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{AccessFailure, AccessState, Device, DirectAccessControl, Listener};
use crate::ui::sidebar_window::form::FormRow;

pub(super) const ACCESS_SELECTOR: &str = "settings-network-access";
pub(super) const ADDRESS_SELECTOR: &str = "settings-network-address";
pub(super) const TURN_OFF_CONFIRM_SELECTOR: &str = "settings-network-access-turn-off-confirm";
pub(super) const TURN_OFF_CANCEL_SELECTOR: &str = "settings-network-access-turn-off-cancel";
pub(super) const MOVE_CONFIRM_SELECTOR: &str = "settings-network-access-move-confirm";
pub(super) const MOVE_CANCEL_SELECTOR: &str = "settings-network-access-move-cancel";
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

/// What the presented server last reported about its network access and its devices, each with
/// when it reported it in Unix seconds, as SpaceTerm retained it. It is display only: nothing
/// here says what the server does now, and no step acts on it. `None` is a part the server never
/// reported.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LastKnown {
    access: Option<(u64, AccessState)>,
    devices: Option<(u64, Vec<Device>)>,
}

impl LastKnown {
    fn from_facts(facts: &LastKnownFacts) -> Self {
        Self {
            access: facts.network_access.as_ref().map(|access| {
                let listener = match access.listener {
                    NetworkListenerFacts::Off => Listener::Off,
                    NetworkListenerFacts::Listening(address) => Listener::Listening(address),
                    NetworkListenerFacts::AddressGone => Listener::AddressGone,
                    NetworkListenerFacts::Failed => Listener::Failed,
                };
                let state = AccessState {
                    server_name: access.server_name.clone().into(),
                    policy: access.policy,
                    listener,
                    addresses: access.addresses.clone(),
                };
                (access.observed_at, state)
            }),
            devices: facts.paired_devices.as_ref().map(|paired| {
                let devices = paired
                    .devices
                    .iter()
                    .map(|device| Device {
                        pairing: device.pairing,
                        name: device.name.clone().into(),
                        fingerprint: device.fingerprint.clone().into(),
                        last_connected_at: device.last_connected_at,
                    })
                    .collect();
                (paired.observed_at, devices)
            }),
        }
    }
}

/// `state` as retained facts, reported at `observed_at`.
fn access_facts(observed_at: u64, state: &AccessState) -> NetworkAccessFacts {
    NetworkAccessFacts {
        observed_at,
        server_name: state.server_name.to_string(),
        policy: state.policy,
        listener: match state.listener {
            Listener::Off => NetworkListenerFacts::Off,
            Listener::Listening(address) => NetworkListenerFacts::Listening(address),
            Listener::AddressGone => NetworkListenerFacts::AddressGone,
            Listener::Failed => NetworkListenerFacts::Failed,
        },
        addresses: state.addresses.clone(),
    }
}

/// `devices` as retained facts, listed at `observed_at`.
fn devices_facts(observed_at: u64, devices: &[Device]) -> PairedDevicesFacts {
    PairedDevicesFacts {
        observed_at,
        devices: devices
            .iter()
            .map(|device| PairedDeviceFacts {
                pairing: device.pairing,
                name: device.name.to_string(),
                fingerprint: device.fingerprint.to_string(),
                last_connected_at: device.last_connected_at,
            })
            .collect(),
    }
}

/// How long an unchanged live reading goes before it is retained again with its new time, so a
/// server's shown time stays close to when SpaceTerm last reached it without a write every poll.
const UNCHANGED_RETENTION: u64 = 60;

/// The access the rows show: the live reading, or what the server last reported while SpaceTerm
/// can't reach it.
struct Shown<'a> {
    policy: Option<SocketAddr>,
    /// `None` when the server never listed its devices.
    devices: Option<&'a [Device]>,
    /// When the server reported a last-known reading. `None` for a live one.
    observed_at: Option<u64>,
}

/// What the rows know about the selected server's network access.
pub(super) struct DirectAccess {
    /// The server the rows present.
    target: Option<ServerChoice>,
    /// Counts the times the rows started over, so an answer for an earlier server lands nowhere.
    generation: u64,
    /// The latest reading. `None` until the server answers, or while SpaceTerm can't ask it.
    reading: Option<Result<AccessState, AccessFailure>>,
    /// The devices the live server listed. `None` while it isn't live or didn't list them.
    devices: Option<Vec<Device>>,
    /// What the presented server last reported, as SpaceTerm retained it.
    known: LastKnown,
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
            known: LastKnown::default(),
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
        self.known = LastKnown::default();
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
        self.reading = Some(Ok(state));
        if let Some(devices) = devices {
            self.receive_devices(devices);
        }
    }

    /// Takes the devices the live server listed.
    fn receive_devices(&mut self, devices: Vec<Device>) {
        self.unpairing
            .retain(|pairing| devices.iter().any(|device| device.pairing == *pairing));
        self.devices = Some(devices);
    }

    /// SpaceTerm can't reach the server now: `failure` says why, or `None` when it has no
    /// connection to ask over. What the server last reported stays.
    fn lose(&mut self, failure: Option<AccessFailure>) {
        self.reading = failure.map(Err);
        self.devices = None;
    }

    /// The access the rows show for the presented server, or `None` when nothing is known.
    fn shown(&self) -> Option<Shown<'_>> {
        if let Some(state) = self.state() {
            return Some(Shown {
                policy: state.policy,
                devices: self.devices.as_deref(),
                observed_at: None,
            });
        }
        self.target.as_ref()?;
        let (observed_at, access) = self.known.access.as_ref()?;
        Some(Shown {
            policy: access.policy,
            devices: self
                .known
                .devices
                .as_ref()
                .map(|(_, devices)| devices.as_slice()),
            observed_at: Some(*observed_at),
        })
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
                format!("{address} is no longer on {server}. Choose another.")
            }
            None => format!("Address no longer on {server}. Choose another."),
        }),
        (Listener::Failed, _) => Some(match address {
            Some(address) => format!("Couldn't listen at {address}."),
            None => "Couldn't listen on the network.".to_owned(),
        }),
        (Listener::Off, Some(addresses)) if addresses.available.is_empty() => {
            Some(format!("No reachable network address on {server}."))
        }
        (_, None) => Some(format!("Couldn't read {server}'s network addresses.")),
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
            ServerChoice::Direct(_) => self
                .direct_access_server(target)
                .map(|server| Arc::new(server) as Arc<dyn DirectAccessControl>),
        }
    }

    /// The connection a removal is recorded against, so its retries keep that exact route and
    /// authority.
    fn direct_access_server(&self, target: &ServerChoice) -> Option<ServerConnection> {
        match target {
            ServerChoice::Local => self.local_server.as_ref()?.worktree_server(),
            ServerChoice::Direct(route) => self.direct_routes.as_ref()?.connections.server(route),
        }
    }

    /// The paired route this computer reaches the presented server over, when it is one. A
    /// change to that server's network reaches this computer's own connection.
    fn own_route(&self) -> Option<DirectRouteId> {
        match self.direct_access.target.as_ref()? {
            ServerChoice::Direct(route) => Some(*route),
            ServerChoice::Local => None,
        }
    }

    /// The pairing this computer holds with the presented paired server.
    fn own_pairing(&self) -> Option<PairingId> {
        let route = self.own_route()?;
        Some(
            self.direct_routes
                .as_ref()?
                .connections
                .route(&route)?
                .paired
                .pairing,
        )
    }

    /// The selected server's name as the rows say it: the name the server gave, once it has.
    fn direct_access_server_name(&self) -> SharedString {
        self.direct_access
            .state()
            .or_else(|| {
                let (_, access) = self.direct_access.known.access.as_ref()?;
                Some(access)
            })
            .map(|state| state.server_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.selected_server_name())
    }

    /// Shows what SpaceTerm retained about the presented server, including a record that loaded
    /// after Settings opened.
    fn reload_known_access(&mut self, cx: &App) {
        self.direct_access.known = self
            .direct_access
            .target
            .as_ref()
            .and_then(|target| server_selection::facts(&target.key(), cx))
            .map(|facts| LastKnown::from_facts(&facts))
            .unwrap_or_default();
    }

    /// Retains what the presented server returned in this reading: `state` and `devices` are only
    /// the parts it returned now, each timed now. A part it didn't return keeps what was retained,
    /// and the server's other last-known facts stay as they are.
    fn retain_direct_access(
        &self,
        state: Option<&AccessState>,
        devices: Option<&[Device]>,
        cx: &mut App,
    ) {
        let Some(key) = self.direct_access.target.as_ref().map(ServerChoice::key) else {
            return;
        };
        let now = crate::ui::direct_access::unix_now();
        // A reading that says something new is retained at once; an unchanged one only once its
        // retained time is old enough to renew.
        let renew = |at: u64| now.saturating_sub(at) >= UNCHANGED_RETENTION;
        // With no record yet the general facts stay unobserved: status Unknown, time zero.
        let mut facts = server_selection::facts(&key, cx).unwrap_or_default();
        let mut changed = false;
        if let Some(state) = state
            && facts.network_access.as_ref().is_none_or(|known| {
                renew(known.observed_at) || *known != access_facts(known.observed_at, state)
            })
        {
            facts.network_access = Some(access_facts(now, state));
            changed = true;
        }
        if let Some(devices) = devices
            && facts.paired_devices.as_ref().is_none_or(|known| {
                renew(known.observed_at) || *known != devices_facts(known.observed_at, devices)
            })
        {
            facts.paired_devices = Some(devices_facts(now, devices));
            changed = true;
        }
        if changed {
            server_selection::remember(key, facts, cx);
        }
    }

    /// Whether the selected server lets other devices connect, as it last said. While SpaceTerm
    /// can't reach the server this is what it last reported, not what it does now.
    pub(super) fn network_access_on(&self) -> bool {
        self.direct_access
            .shown()
            .is_some_and(|shown| shown.policy.is_some())
    }

    /// Whether SpaceTerm can change the selected server's network access now.
    pub(super) fn network_access_live(&self) -> bool {
        self.direct_access.state().is_some()
    }

    /// The Network Access card's footnote while its rows can't change.
    pub(super) fn network_access_footnote(&self, cx: &App) -> Option<SharedString> {
        if self.network_access_live() {
            return None;
        }
        let stopped = match self.direct_access.target.as_ref()? {
            ServerChoice::Local => {
                self.server_service
                    .as_ref()
                    .and_then(|row| row.state.status)
                    .is_some_and(|status| status.stopped)
                    || self.local_server.as_ref().is_some_and(|row| {
                        matches!(
                            row.current_status(),
                            LocalServerStatus::Failed(ServerError::Stopped)
                        )
                    })
            }
            ServerChoice::Direct(_) => false,
        };
        if !stopped
            && matches!(
                self.direct_access.reading,
                Some(Err(AccessFailure::Unreachable))
            )
        {
            let name = self.selected_server_name();
            let observed = self
                .direct_access
                .shown()
                .and_then(|shown| shown.observed_at)
                .map(|at| {
                    format!(
                        "SpaceTerm can't reach {name}. Last known Network Access settings: {}.",
                        super::server_card::observed_time(at, cx)
                    )
                });
            return Some(
                observed
                    .unwrap_or_else(|| "Can't reach the server.".to_owned())
                    .into(),
            );
        }
        let name = self.selected_server_name();
        Some(match &self.direct_access.target {
            Some(ServerChoice::Local) => {
                format!("Start the server on {name} to change its network access.").into()
            }
            _ => format!("Connect to {name} to change its network access.").into(),
        })
    }

    /// Follows the selected server's network access while Servers shows it. Selection, section
    /// and connection changes call this; rendering never does.
    pub(super) fn sync_direct_access(&mut self, cx: &mut Context<Self>) {
        let target = self.selected_server();
        if self.direct_access.target != target {
            self.direct_access.start_over(target.clone());
            self.reload_known_access(cx);
        }
        if self.active_section != SettingsSectionId::Servers {
            self.direct_access.poll = None;
            return;
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
                    if settings.direct_access.generation != generation {
                        return;
                    }
                    // A change in flight answers for itself; a reading taken meanwhile may
                    // describe the server before it.
                    if settings.direct_access.change.is_none() {
                        match read {
                            Some((Ok(status), devices)) => {
                                settings.retain_direct_access(
                                    Some(&status),
                                    devices.as_deref(),
                                    cx,
                                );
                                settings.direct_access.receive(status, devices);
                            }
                            Some((Err(failure), _)) => settings.direct_access.lose(Some(failure)),
                            None => settings.direct_access.lose(None),
                        }
                    }
                    // A record that loads after Settings opens shows from the next reading on.
                    settings.reload_known_access(cx);
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
        if let Some(AccessState {
            listener: Listener::Listening(address),
            ..
        }) = self.direct_access.state()
        {
            return SharedString::from(format!("Listening on {address}."));
        }
        // A server SpaceTerm can't reach may have stopped listening since, so its address reads
        // as what it last reported.
        match self.direct_access.shown() {
            Some(Shown {
                policy: Some(address),
                observed_at: Some(_),
                ..
            }) => SharedString::from(format!("Last known address: {address}.")),
            _ => SharedString::new_static("Invited devices can connect over the network."),
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
        let name = SharedString::from(format!("Allow other devices to connect to {server}"));
        if self.direct_access.shown().is_none() {
            // Nothing says whether the server lets devices connect, so no switch claims it's off.
            let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
            return div()
                .id(SharedString::from(format!("{ACCESS_SELECTOR}-unknown")))
                .debug_selector(|| format!("{ACCESS_SELECTOR}-unknown"))
                .role(accesskit::Role::Label)
                .aria_label(name)
                .aria_value("Unknown")
                .chrome_text(appearance.typography.style(TextRole::Body))
                .text_color(gpui_color(colors.text_secondary))
                .child("Unknown")
                .into_any_element();
        }
        let state = self.direct_access.state().cloned();
        let change = self.direct_access.change;
        // What the server last reported while SpaceTerm can't reach it, dimmed by its row.
        let on = self.network_access_on();
        let live_on = state.as_ref().is_some_and(|state| state.policy.is_some());
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
        let switch = Switch::new(ACCESS_SELECTOR, name, on)
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
            .children(chooser.filter(|_| live_on || gone))
            .child(switch)
            .into_any_element()
    }

    /// Moves access to `address`, or turns it on there, keeping the chosen port.
    fn choose_address(&mut self, address: IpAddr, window: &mut Window, cx: &mut Context<Self>) {
        let on = self
            .direct_access
            .state()
            .is_some_and(|state| state.policy.is_some());
        if on && self.own_route().is_some() {
            self.confirm_move(address, window, cx);
        } else if on {
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

    /// Asks before a paired server moves its listener away from the address this computer uses,
    /// which disconnects this computer and every other device using that address.
    fn confirm_move(&mut self, address: IpAddr, window: &mut Window, cx: &mut Context<Self>) {
        if !self.network_access_live() {
            return;
        }
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let server = self.direct_access_server_name();
        let title = "Change Network Address?";
        let message = format!(
            "This computer and other paired devices reach {server} at its current address, so they disconnect. Each needs a new invitation to reconnect at {address}."
        );
        let result = Alert::new(
            ModalId::new("settings-network-access-move"),
            title,
            title,
            message,
            vec![
                ModalAction::new(
                    true,
                    "Change Address",
                    ModalActionRole::Affirmative,
                    MOVE_CONFIRM_SELECTOR,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    MOVE_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Critical)
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
                if settings.direct_access.generation == generation {
                    settings.apply_address(address, Change::Moving, cx);
                }
            });
        });
        if result.is_err() {
            eprintln!("failed to present the network address change confirmation");
        }
    }

    /// Asks before the selected server stops listening, naming the server and what ends. On a
    /// paired server this computer's own connection ends too, and only that server's own
    /// computer can turn access on again.
    fn confirm_turn_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.network_access_live() {
            return;
        }
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let Some(control) = self.direct_access_control(&target) else {
            return;
        };
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let title = "Turn Off Network Access?";
        let own = self.own_route().is_some();
        let message: SharedString = if own {
            format!(
                "Paired devices disconnect, including this computer. To turn access on again, use SpaceTerm on {}.",
                self.direct_access_server_name()
            )
            .into()
        } else {
            "Paired devices connected over the network disconnect.".into()
        };
        let confirm = ModalAction::new(
            true,
            "Turn Off",
            ModalActionRole::Affirmative,
            TURN_OFF_CONFIRM_SELECTOR,
        )
        .with_emphasis(ModalActionEmphasis::Prominent);
        let result = Alert::new(
            ModalId::new("settings-network-access-turn-off"),
            title,
            title,
            message,
            vec![
                if own {
                    confirm.with_intent(ModalActionIntent::Destructive)
                } else {
                    confirm
                },
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    TURN_OFF_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(if own {
            AlertIntent::Critical
        } else {
            AlertIntent::Informational
        })
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
        // A server SpaceTerm can't reach now gets no change, even one confirmed while it could.
        if !self.network_access_live() {
            self.direct_access.problem = Some(AccessFailure::Unreachable);
            cx.notify();
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
                if settings.direct_access.generation != generation {
                    return;
                }
                settings.direct_access.change = None;
                match applied {
                    Ok(status) => {
                        settings.retain_direct_access(Some(&status), None, cx);
                        settings.direct_access.receive(status, None);
                        settings.reload_known_access(cx);
                    }
                    Err(failure) => settings.direct_access.problem = Some(failure),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The Devices row's value: how many devices are paired, or Unknown when the server never
    /// listed them.
    fn paired_count(&self) -> SharedString {
        match self
            .direct_access
            .shown()
            .and_then(|shown| shown.devices)
            .map(<[Device]>::len)
        {
            None => SharedString::new_static("Unknown"),
            Some(0) => SharedString::new_static("None"),
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
                        "Invite another device to connect to {server}."
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
        // While SpaceTerm can't reach the server these are the devices it last listed, dimmed.
        let Some(devices) = self
            .direct_access
            .shown()
            .and_then(|shown| shown.devices)
            .map(<[Device]>::to_vec)
        else {
            return Vec::new();
        };
        let now = crate::ui::direct_access::unix_now();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let dimmed = !self.network_access_live();
        let own_pairing = self.own_pairing();
        let this_computer = self.this_computer;
        devices
            .into_iter()
            .enumerate()
            .map(|(index, device)| {
                let removing = self.direct_access.unpairing.contains(&device.pairing);
                let selector = format!("{DEVICE_SELECTOR}-{index}");
                let owner = cx.weak_entity();
                let name = if own_pairing == Some(device.pairing) {
                    SharedString::from(format!("{} ({this_computer})", device.name))
                } else {
                    device.name.clone()
                };
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
                    .accessibility_description(SharedString::from(format!("Remove {name}.")))
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
        if !self.network_access_live() {
            return;
        }
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
                    settings.retain_direct_access(None, Some(&devices), cx);
                    settings.direct_access.receive_devices(devices);
                    settings.reload_known_access(cx);
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
        if !self.network_access_live() {
            return;
        }
        let Some(target) = self.direct_access.target.clone() else {
            return;
        };
        let generation = self.direct_access.generation;
        let owner = cx.weak_entity();
        let title = format!("Unpair \"{}\"?", device.name);
        let message: SharedString = if self.own_pairing() == Some(device.pairing) {
            format!(
                "This is this computer. It disconnects from {} and needs a new invitation to reconnect.",
                self.direct_access_server_name()
            )
            .into()
        } else {
            "It needs a new invitation to reconnect.".into()
        };
        let result = Alert::new(
            ModalId::new("settings-paired-device-remove"),
            title.clone(),
            title,
            message,
            vec![
                ModalAction::new(
                    true,
                    "Unpair",
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
        // A removal confirmed just before SpaceTerm lost the server is refused, not recorded.
        if !self.network_access_live() {
            self.direct_access.problem = Some(AccessFailure::Unreachable);
            cx.notify();
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
        // A paired server's removal is recorded against this computer's own route: its own
        // pairing as Unpair, which removes the saved connection at once, another as a revocation.
        let own = self.own_route();
        let own_pairing = self.own_pairing();
        self.direct_access.unpairing.push(pairing);
        self.direct_access.problem = None;
        self.direct_access.work = Some(cx.spawn(async move |settings, cx| {
            let recorded = cx
                .background_executor()
                .spawn(async move {
                    match own {
                        Some(route) if own_pairing == Some(pairing) => registry.unpair_self(&route),
                        Some(route) => registry.unpair(&route, pairing),
                        None => registry.revoke_other(server, pairing, label),
                    }
                })
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
