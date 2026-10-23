//! The selected SSH server's maintenance rows, driven through a scripted operations owner whose
//! every call the test counts.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::A11yTree;

use super::super::servers::testing::{ScriptedServer, connected};
use super::super::servers::{LocalServerControl, OfferedLocalServer, OfferedServer};
use crate::appearance::Appearance;
use crate::domain::SshDestination;
use crate::local_server::service::control::Action;
use crate::local_server::service::maintenance::{MaintenanceFacts, PreparedChange, RemoteBuild};
use crate::local_server::service::{ServiceStartup, ServiceStatus};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ssh::cancellation::SshCancellationToken;
use crate::ssh::server_connection::{
    ConnectMode, ConnectProgress, ConnectedSshServer, ContinuityChange, InteractionReason,
    RemoteConnectionStatus, RetainedSshConnection, ServerAvailability, ServerCandidate,
    SshConnectBackend, SshConnectFailure, SshConnectionIntentStore, SshIntentStoreError,
    SshServerConnections, SshServerContinuity, SshServerFacts, SshServerObservation,
};
use crate::ssh::server_maintenance::{MaintenancePhase, SshMaintenanceFailure, SubmittedKind};
use crate::ssh::server_operations::{
    RemoteMaintenanceRequest, RemoteOperationEntry, RemoteOperationHandle, RemoteOperationState,
    RemoteServerOperations, RemoteUpdateOffer, RetainedOutcome,
};
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
/// The rows of the card that carry the selected server's service and its update.
const SERVICE_ROW: &str = "settings-row-server-status";
const UPDATE_ROW: &str = "settings-row-server-version";
/// The notice that this client couldn't read or write its record of these operations.
const OPERATIONS_PERSISTENCE: &str = "settings-server-notice-operations-record";
/// The notice for the removed connection's unresolved operation.
const REMOVED_NOTICE: &str = "settings-server-notice-removed-penguin";

/// One call the maintenance rows made on the owner.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Refresh,
    Describe(RemoteMaintenanceRequest),
    Confirm(RemoteOperationHandle),
    Start,
    Cancel,
    Resume,
    Acknowledge(RemoteOperationHandle),
    Discard([u8; 32]),
    Dismiss([u8; 32]),
    ResumeRetained,
    AutomaticStart(bool),
}

fn build(version: &str, digit: char) -> RemoteBuild {
    RemoteBuild {
        version: version.to_owned(),
        commit: std::iter::repeat_n(digit, 40).collect(),
        digest: std::iter::repeat_n(digit, 64).collect(),
    }
}

const fn status(installed: bool, running: bool, stopped: bool) -> ServiceStatus {
    ServiceStatus {
        installed,
        enabled: running,
        running,
        stopped,
    }
}

/// A validated result the service reported, as it survives relaunch.
const fn completed(after: Option<ServiceStatus>) -> RetainedOutcome {
    RetainedOutcome::Completed {
        status: after,
        build: None,
    }
}

fn facts(status: ServiceStatus) -> MaintenanceFacts {
    MaintenanceFacts {
        server: [2; 32],
        account: 501,
        build: "spaceterm-development".to_owned(),
        status,
        startup: ServiceStartup::AtLogin,
        approved: Some(build("1.4.0", 'a')),
        activation: None,
    }
}

/// One entry the scripted owner holds: the destination it binds, whether its saved connection
/// was removed, and the state it reports.
struct Held {
    destination: String,
    removed: bool,
    /// The original this entry holds, which names it. Two entries for one destination in one
    /// listing differ only by this, so a step that retires a record must carry it.
    operation: [u8; 32],
    state: RemoteOperationState,
    /// What the last verified read found about updating the server, reported with the state.
    offer: RemoteUpdateOffer,
}

/// A verified server archive newer than the approved 1.4.0, as a verified read finds it.
fn available() -> RemoteUpdateOffer {
    RemoteUpdateOffer::Available(ServerCandidate {
        version: "1.5.0".to_owned(),
        digest: build("1.5.0", 'b').digest,
    })
}

/// An owner whose states the test sets and whose calls it counts. Describing a change answers
/// with the exact change it describes, and only confirming it resolves an operation.
struct ScriptedOperations {
    /// Every entry the owner holds, in the order it reports them. One destination can hold more
    /// than one, so the order within a destination is the order the surface must read.
    states: Mutex<Vec<Held>>,
    calls: Mutex<Vec<Call>>,
    /// What a confirmed change resolves to.
    /// What resolving an unresolved original answers.
    resumed: Mutex<Option<RemoteOperationState>>,
    changes: (async_channel::Sender<()>, async_channel::Receiver<()>),
    handles: Mutex<u64>,
    /// The failure this client's private record of these operations last reported, which leaves
    /// every retirement unapplied.
    unsaved: Mutex<Option<SshIntentStoreError>>,
}

impl ScriptedOperations {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            states: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            resumed: Mutex::new(None),
            changes: async_channel::unbounded(),
            handles: Mutex::new(0),
            unsaved: Mutex::new(None),
        })
    }

    /// Sets one server's state, keeping whether its saved connection was removed: removal
    /// outlives every later state of the operation it holds.
    fn set(&self, destination: &str, state: RemoteOperationState) {
        let mut states = self.states.lock().expect("states");
        match states
            .iter_mut()
            .find(|held| held.destination == destination)
        {
            Some(held) => held.state = state,
            None => {
                let operation = [states.len() as u8 + 1; 32];
                states.push(Held {
                    destination: destination.to_owned(),
                    removed: false,
                    operation,
                    state,
                    offer: RemoteUpdateOffer::Unchecked,
                })
            }
        }
        drop(states);
        self.publish();
    }

    /// The original one entry holds, which a step that retires a record carries. A test reads it
    /// before the step, because clearing an entry ends its listing.
    fn operation_of(&self, destination: &str, removed: bool) -> [u8; 32] {
        self.states
            .lock()
            .expect("states")
            .iter()
            .find(|held| held.destination == destination && held.removed == removed)
            .map(|held| held.operation)
            .expect("an entry for that destination")
    }

    /// Holds more than one entry for one server, in the order the owner reports them. A read of
    /// the retained records that failed and later landed leaves the owner following the original
    /// it recovered beside the one the person submitted while it could not read, so the first
    /// here is the one the person's last step produced.
    fn hold(&self, destination: &str, held: impl IntoIterator<Item = RemoteOperationState>) {
        let mut states = self.states.lock().expect("states");
        states.retain(|held| held.destination != destination);
        let base = states.len() as u8;
        states.extend(held.into_iter().enumerate().map(|(offset, state)| Held {
            destination: destination.to_owned(),
            removed: false,
            operation: [base + offset as u8 + 1; 32],
            state,
            offer: RemoteUpdateOffer::Unchecked,
        }));
        drop(states);
        self.publish();
    }

    /// Keeps the state of a server whose saved connection the person removed.
    fn remove(&self, destination: &str) {
        for held in self
            .states
            .lock()
            .expect("states")
            .iter_mut()
            .filter(|held| held.destination == destination)
        {
            held.removed = true;
        }
        self.publish();
    }

    /// Reports what a verified read found about updating one server, beside its state.
    fn offer(&self, destination: &str, offer: RemoteUpdateOffer) {
        for held in self
            .states
            .lock()
            .expect("states")
            .iter_mut()
            .filter(|held| held.destination == destination)
        {
            held.offer = offer.clone();
        }
        self.publish();
    }

    fn publish(&self) {
        let _ = self.changes.0.try_send(());
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls").clone()
    }

    fn record(&self, call: Call) {
        self.calls.lock().expect("calls").push(call);
    }

    /// Makes every retirement fail as storage SpaceTerm can neither read nor write does, so the
    /// record stays and the state the person was looking at is unchanged.
    fn cannot_save(&self, error: SshIntentStoreError) {
        *self.unsaved.lock().expect("unsaved") = Some(error);
        self.publish();
    }

    fn retires(&self) -> bool {
        self.unsaved.lock().expect("unsaved").is_none()
    }

    fn resume_with(&self, state: RemoteOperationState) {
        *self.resumed.lock().expect("resumed") = Some(state);
    }

    fn next_handle(&self) -> RemoteOperationHandle {
        let mut handles = self.handles.lock().expect("handles");
        *handles += 1;
        RemoteOperationHandle(*handles)
    }

    fn described(&self, destination: &str) -> Option<RemoteMaintenanceRequest> {
        self.states
            .lock()
            .expect("states")
            .iter()
            .filter(|held| held.destination == destination)
            .find_map(|held| match held.state {
                RemoteOperationState::Described { request, .. } => Some(request),
                _ => None,
            })
    }
}

fn change(running: bool, stopped: bool) -> PreparedChange {
    PreparedChange {
        confirmation: "an exact-observation confirmation".to_owned(),
        running,
        stopped,
        startup: ServiceStartup::AtLogin,
        previous: Some(build("1.4.0", 'a')),
        candidate: Some(build("1.5.0", 'b')),
    }
}

impl RemoteServerOperations for ScriptedOperations {
    fn subscribe(&self) -> async_channel::Receiver<()> {
        self.changes.1.clone()
    }

    fn entries(&self) -> Vec<RemoteOperationEntry> {
        let mut entries: Vec<_> = self
            .states
            .lock()
            .expect("states")
            .iter()
            .map(|held| RemoteOperationEntry {
                destination: SshDestination::new(held.destination.clone()).expect("destination"),
                removed: held.removed,
                operation: Some(held.operation),
                state: held.state.clone(),
                observed_at: None,
                update_offer: held.offer.clone(),
            })
            .collect();
        // The owner orders by destination and keeps the order it holds one destination's entries
        // in, so the surface reads them as the owner reports them.
        entries.sort_by(|left, right| left.destination.cmp(&right.destination));
        entries
    }

    /// The owner answers with one entry's state, preferring a connection still saved. It is the
    /// one reader that cannot see a second entry for the same destination.
    #[cfg(test)]
    fn state(&self, destination: &SshDestination) -> RemoteOperationState {
        let states = self.states.lock().expect("states");
        states
            .iter()
            .find(|held| held.destination == destination.as_str() && !held.removed)
            .or_else(|| {
                states
                    .iter()
                    .find(|held| held.destination == destination.as_str())
            })
            .map_or(RemoteOperationState::Unread, |held| held.state.clone())
    }

    /// A verified read reports the service and the verified build SpaceTerm could install.
    fn refresh(&self, destination: &SshDestination) {
        self.record(Call::Refresh);
        self.set(
            destination.as_str(),
            RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
        );
        self.offer(destination.as_str(), available());
    }

    /// Changes only whether the service starts on its own. Whether it runs, and a deliberate
    /// Stop, stay as the last read found them.
    fn set_automatic_start(&self, destination: &SshDestination, enabled: bool) {
        self.record(Call::AutomaticStart(enabled));
        let mut read = match self.state(destination) {
            RemoteOperationState::Read(facts) => *facts,
            _ => facts(status(true, true, false)),
        };
        read.status.enabled = enabled;
        self.set(
            destination.as_str(),
            RemoteOperationState::Read(Box::new(read)),
        );
    }

    fn describe(&self, destination: &SshDestination, request: RemoteMaintenanceRequest) {
        self.record(Call::Describe(request));
        let running = !matches!(request, RemoteMaintenanceRequest::Return);
        self.set(
            destination.as_str(),
            RemoteOperationState::Described {
                handle: self.next_handle(),
                request,
                change: Box::new(change(running, !running)),
            },
        );
    }

    fn confirm(&self, handle: RemoteOperationHandle) {
        self.record(Call::Confirm(handle));
        let described: Vec<_> = self
            .states
            .lock()
            .expect("states")
            .iter()
            .filter_map(|held| match held.state {
                RemoteOperationState::Described { request, .. } => {
                    Some((held.destination.clone(), request))
                }
                _ => None,
            })
            .collect();
        for (destination, request) in described {
            let kind = match request {
                RemoteMaintenanceRequest::Service(action) => SubmittedKind::Service(action),
                RemoteMaintenanceRequest::Update => SubmittedKind::Update {
                    candidate: build("1.5.0", 'b').digest,
                },
                RemoteMaintenanceRequest::Return => SubmittedKind::Return,
            };
            let outcome = RetainedOutcome::Completed {
                status: Some(status(true, false, true)),
                build: match request {
                    RemoteMaintenanceRequest::Service(_) => None,
                    RemoteMaintenanceRequest::Update => Some(build("1.5.0", 'b')),
                    RemoteMaintenanceRequest::Return => Some(build("1.4.0", 'a')),
                },
            };
            self.set(
                &destination,
                RemoteOperationState::Resolved {
                    handle,
                    kind,
                    outcome,
                },
            );
        }
    }

    fn start(&self, destination: &SshDestination) {
        self.record(Call::Start);
        self.set(
            destination.as_str(),
            RemoteOperationState::Resolved {
                handle: self.next_handle(),
                kind: SubmittedKind::Start,
                outcome: completed(Some(status(true, true, false))),
            },
        );
    }

    fn cancel(&self, destination: &SshDestination) {
        self.record(Call::Cancel);
        self.set(
            destination.as_str(),
            RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
        );
    }

    fn resume(&self, destination: &SshDestination) {
        self.record(Call::Resume);
        if let Some(state) = self.resumed.lock().expect("resumed").clone() {
            self.set(destination.as_str(), state);
        }
    }

    fn acknowledge(&self, handle: RemoteOperationHandle) {
        self.record(Call::Acknowledge(handle));
        if !self.retires() {
            // An unretired record still awaits acknowledgement, so the result stays.
            self.publish();
            return;
        }
        let mut states = self.states.lock().expect("states");
        if let Some(held) = states.iter_mut().find(|held| {
            matches!(held.state, RemoteOperationState::Resolved { handle: named, .. } if named == handle)
        }) {
            let observed = match &held.state {
                RemoteOperationState::Resolved {
                    outcome: RetainedOutcome::Completed { status: Some(status), .. },
                    ..
                } => *status,
                _ => status(true, true, false),
            };
            held.state = RemoteOperationState::Read(Box::new(facts(observed)));
        }
        drop(states);
        self.publish();
    }

    /// Retires the first entry in the named listing that holds an original a Discard settles, as
    /// the owner does, so a second entry for the same destination keeps its own original.
    fn discard(&self, operation: [u8; 32]) {
        self.record(Call::Discard(operation));
        if !self.retires() {
            // An unretired record is still followed, so the original stays.
            self.publish();
            return;
        }
        let mut states = self.states.lock().expect("states");
        if let Some(held) = states.iter_mut().find(|held| {
            held.operation == operation
                && matches!(
                    held.state,
                    RemoteOperationState::Paused { .. } | RemoteOperationState::Unconfirmed { .. }
                )
        }) {
            held.state = RemoteOperationState::Unread;
        }
        drop(states);
        self.publish();
    }

    fn dismiss(&self, operation: [u8; 32]) {
        self.record(Call::Dismiss(operation));
        // Nothing was retained for an original the service has no record of, so the entry goes
        // whether or not this client can write.
        let mut states = self.states.lock().expect("states");
        if let Some(index) = states.iter().position(|held| {
            held.operation == operation
                && matches!(
                    held.state,
                    RemoteOperationState::Failed {
                        failure: SshMaintenanceFailure::Forgotten,
                        ..
                    }
                )
        }) {
            // A removed connection's entry exists only to carry this last word, so clearing it
            // ends the listing. A saved connection keeps its row with nothing read.
            if states[index].removed {
                states.remove(index);
            } else {
                states[index].state = RemoteOperationState::Unread;
            }
        }
        drop(states);
        self.publish();
    }

    fn persistence_error(&self) -> Option<SshIntentStoreError> {
        *self.unsaved.lock().expect("unsaved")
    }

    fn resume_retained(&self) {
        self.record(Call::ResumeRetained);
    }
}

/// A computer whose state Check Status observes and whose connection waits for the person, so no
/// test connects to anything.
struct Observed;

impl SshConnectBackend for Observed {
    fn connect(
        &self,
        _: &SshDestination,
        _: ConnectMode,
        _: Option<&SshServerContinuity>,
        _: &SshCancellationToken,
        _: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure> {
        Err(SshConnectFailure::NeedsPerson(
            InteractionReason::Authentication,
        ))
    }

    fn observe(
        &self,
        _: &SshDestination,
        _: Option<&SshServerContinuity>,
        _: &SshCancellationToken,
        _: &ConnectProgress<'_>,
    ) -> Result<SshServerObservation, SshConnectFailure> {
        Ok(SshServerObservation {
            continuity: None,
            availability: ServerAvailability::Running,
            facts: SshServerFacts {
                platform: ("Example OS".to_owned(), "x86_64".to_owned()),
                version: Some("1.4.0".to_owned()),
                starts_automatically: true,
            },
        })
    }
}

/// The saved connections a relaunch restores: saved, inactive and never reached.
struct Saved(Mutex<Vec<RetainedSshConnection>>);

impl SshConnectionIntentStore for Saved {
    fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
        Ok(self.0.lock().expect("saved").clone())
    }
    fn retain(&self, record: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
        let mut saved = self.0.lock().expect("saved");
        saved.retain(|other| other.destination != record.destination);
        saved.push(record);
        Ok(())
    }
    fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError> {
        self.0
            .lock()
            .expect("saved")
            .retain(|record| record.destination != *destination);
        Ok(())
    }
}

struct Servers<'a> {
    window: Entity<SettingsWindow>,
    operations: Arc<ScriptedOperations>,
    connections: SshServerConnections,
    cx: &'a mut VisualTestContext,
}

/// Opens Settings on the Servers section with one saved SSH connection selected and the scripted
/// owner installed.
fn open_maintenance<'a>(cx: &'a mut TestAppContext) -> Servers<'a> {
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    let saved = Arc::new(Saved(Mutex::new(vec![RetainedSshConnection {
        destination,
        continuity: None,
        active: false,
    }])));
    let connections = SshServerConnections::new(Arc::new(Observed), saved);
    connections.resume_retained();
    let deadline = Instant::now() + Duration::from_secs(5);
    while connections.connections().is_empty() {
        assert!(Instant::now() < deadline, "the saved connection loads");
        std::thread::sleep(Duration::from_millis(2));
    }
    let operations = ScriptedOperations::new();
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    let registry = connections.clone();
    let owner = Arc::clone(&operations) as Arc<dyn RemoteServerOperations>;
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::ssh_servers::install(registry, None, cx);
        crate::ssh::server_operations::install(owner, cx);
    });
    let offered: OfferedLocalServer = Some(OfferedServer {
        connection: Ok(ScriptedServer::new(Ok(connected(1))) as Arc<dyn LocalServerControl>),
        service: None,
        update: None,
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
            offered,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click(NAVIGATION_SELECTOR, cx);
    // The SSH server is the one every maintenance row acts on.
    click("settings-all-servers-1", cx);
    Servers {
        window,
        operations,
        connections,
        cx,
    }
}

fn click(selector: &str, cx: &mut VisualTestContext) {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn rendered(selector: &str, cx: &mut VisualTestContext) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    cx.debug_bounds(selector).is_some()
}

fn modal_action(selector: &str) -> &'static str {
    Box::leak(format!("modal-action-{selector}").into_boxed_str())
}

fn alert_open(selector: &str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(modal_action(selector)).is_some()
}

fn click_modal(selector: &str, cx: &mut VisualTestContext) {
    click(modal_action(selector), cx);
}

/// Whether the window tells the person `text` as assistive technology reads it: a description, a
/// message under Status or Version, a notice, or an announced state. Text that exists only in
/// SpaceTerm's model and isn't on screen doesn't count.
fn explains(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).in_order().iter().any(|node| {
        ["label", "value"].into_iter().any(|field| {
            node["aria"][field]
                .as_str()
                .is_some_and(|published| published.contains(text))
        })
    })
}

/// Check Now on the Version row.
const CHECK_NOW: &str = super::super::server_card::VERSION_CHECK_SELECTOR;
/// The Version row's steps for an SSH server's update journey.
const VERSION_UPDATE: &str = "settings-server-version-update";
const VERSION_ACKNOWLEDGE: &str = "settings-server-version-acknowledge";
const VERSION_DISCARD: &str = "settings-server-version-discard";
/// The Status row's steps for an SSH server's service.
const STATUS_ACKNOWLEDGE: &str = "settings-ssh-server-maintenance-acknowledge";
const STATUS_DISCARD: &str = "settings-ssh-server-maintenance-discard";
/// The ⋯ menu's service steps, as assistive technology names them.
const STOP_SERVER: &str = "Stop Server";
const RESTART_SERVER: &str = "Restart Server";
const UNINSTALL_SERVER: &str = "Uninstall Server\u{2026}";

/// Whether the Status row's ⋯ menu offers `label` as a step the person can choose. It opens the
/// menu to read the entry and closes it again without choosing anything.
fn menu_offers(label: &str, cx: &mut VisualTestContext) -> bool {
    click(super::super::server_card::MENU_SELECTOR, cx);
    let offered = A11yTree::read(cx)
        .find(label)
        .is_some_and(|entry| entry["aria"]["disabled"] != true);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        A11yTree::read(cx).find(label).is_none(),
        "the menu closes without choosing {label}"
    );
    offered
}

/// What Status says about a server a read found running while SpaceTerm isn't connected to it.
const RUNNING_UNCONNECTED: &str = "Running \u{b7} not connected";

/// Whether the Status row announces penguin's state as `state`.
fn status_reads(state: &str, cx: &mut VisualTestContext) -> bool {
    announces(&format!("penguin: {state}"), cx)
}

/// Whether the Start automatically switch reads as on, as assistive technology hears it.
fn starts_automatically(cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx)
        .with_role("Switch")
        .iter()
        .find(|switch| switch["aria"]["label"] == "Start automatically")
        .expect("the Start automatically switch is rendered")["aria"]["toggled"]
        == "True"
}

/// Every step an SSH server's maintenance offers on its card, in either row.
const SSH_STEPS: [&str; 13] = [
    "settings-ssh-server-service-start",
    "settings-ssh-server-service-again",
    STATUS_ACKNOWLEDGE,
    STATUS_DISCARD,
    "settings-ssh-server-maintenance-dismiss",
    "settings-ssh-server-maintenance-cancel",
    VERSION_UPDATE,
    "settings-server-version-finish",
    "settings-server-version-return",
    "settings-server-version-check-again",
    "settings-server-version-cancel",
    VERSION_ACKNOWLEDGE,
    VERSION_DISCARD,
];

/// Chooses `selector` from the Status row's ⋯ menu.
fn choose(selector: &str, cx: &mut VisualTestContext) {
    click(super::super::server_card::MENU_SELECTOR, cx);
    click(selector, cx);
}

/// Whether any status the page publishes carries `text`, as assistive technology announces it.
fn announces(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).with_role("Status").iter().any(|node| {
        node["aria"]["label"]
            .as_str()
            .is_some_and(|published| published.contains(text))
    })
}

#[gpui::test]
fn dropdown_ssh_update_describes_only_the_selected_servers_existing_journey(
    cx: &mut TestAppContext,
) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).unwrap();
    servers.operations.refresh(&destination);
    servers.cx.run_until_parked();
    servers.cx.update(|window, cx| {
        cx.set_global(super::super::OpenSettingsWindow(
            window.window_handle().downcast::<SettingsWindow>().unwrap(),
        ));
        crate::ui::updates::server_rows::ServerUpdateAction::Ssh(destination.clone()).activate(cx);
    });
    servers.cx.run_until_parked();
    assert_eq!(
        servers.operations.described("penguin"),
        Some(RemoteMaintenanceRequest::Update)
    );
    assert_eq!(
        servers.operations.calls(),
        [
            Call::Refresh,
            Call::Describe(RemoteMaintenanceRequest::Update)
        ]
    );
}

/// Opening the section reads nothing from the other computer, and the rows say so.
#[gpui::test]
fn opening_the_section_reaches_no_computer(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);

    assert!(rendered(SERVICE_ROW, servers.cx));
    assert!(rendered(UPDATE_ROW, servers.cx));
    assert!(
        servers.operations.calls().is_empty(),
        "rendering must ask no computer anything, got {:?}",
        servers.operations.calls()
    );
    assert!(rendered(CHECK_NOW, servers.cx));
    assert!(!menu_offers(STOP_SERVER, servers.cx));
    assert!(servers.operations.calls().is_empty());
}

/// The local server keeps its own rows, and the remote maintenance rows belong to the selected
/// SSH server alone.
#[gpui::test]
fn the_local_server_keeps_its_own_rows(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);

    click("settings-all-servers-0", servers.cx);

    for step in SSH_STEPS {
        assert!(!rendered(step, servers.cx), "{step} belongs to penguin");
    }
    assert!(!status_reads("", servers.cx));
}

/// One explicit read reports the service and its approved version without connecting.
#[gpui::test]
fn check_now_shows_the_approved_version_without_connecting(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);

    click(CHECK_NOW, servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Refresh]);
    assert!(
        status_reads(RUNNING_UNCONNECTED, servers.cx),
        "Check Now says what it found, not only what it offers"
    );
    assert!(
        explains("Version 1.4.0", servers.cx),
        "the Version row shows the approved build"
    );
    assert!(menu_offers(STOP_SERVER, servers.cx));
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    assert!(matches!(
        servers.connections.status(&destination),
        RemoteConnectionStatus::Inactive
    ));
    assert!(servers.connections.last_facts(&destination).is_none());
}

/// Stop describes its change, names the machine and the Sessions that end, and sends nothing
/// until the person confirms that exact change.
#[gpui::test]
fn stopping_asks_once_before_it_sends_anything(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    servers.cx.run_until_parked();

    choose("settings-ssh-server-service-stop", servers.cx);

    assert!(alert_open("settings-ssh-server-stop-confirm", servers.cx));
    assert_eq!(
        servers.operations.calls(),
        vec![Call::Describe(RemoteMaintenanceRequest::Service(
            Action::Stop
        ))],
        "describing a change sends no operation"
    );
    assert!(explains(
        "Stop the penguin server and end all of its Terminal Sessions?",
        servers.cx
    ));

    click_modal("settings-ssh-server-stop-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![
            Call::Describe(RemoteMaintenanceRequest::Service(Action::Stop)),
            Call::Confirm(RemoteOperationHandle(1)),
        ],
        "only the described change is performed, once"
    );
    assert!(explains("The server on penguin stopped", servers.cx));
    assert!(rendered(STATUS_ACKNOWLEDGE, servers.cx));
}

/// Cancelling the confirmation abandons the described change, and nothing was sent.
#[gpui::test]
fn cancelling_the_confirmation_sends_nothing(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    servers.cx.run_until_parked();

    choose("settings-ssh-server-service-stop", servers.cx);
    click_modal("settings-ssh-server-stop-cancel", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![
            Call::Describe(RemoteMaintenanceRequest::Service(Action::Stop)),
            Call::Cancel,
        ]
    );
    assert!(servers.operations.described("penguin").is_none());
    assert!(
        menu_offers(STOP_SERVER, servers.cx),
        "the running server can still be stopped"
    );
}

/// Start is the only step that reverses a deliberate Stop, and it asks nothing.
#[gpui::test]
fn only_start_reverses_a_deliberate_stop(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, false, true)))),
    );
    servers.cx.run_until_parked();

    assert!(rendered("settings-ssh-server-service-start", servers.cx));
    assert!(!menu_offers(RESTART_SERVER, servers.cx));
    assert!(!menu_offers(STOP_SERVER, servers.cx));

    click("settings-ssh-server-service-start", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Start]);
    assert!(
        !alert_open("settings-ssh-server-stop-confirm", servers.cx),
        "starting ends no work, so it asks nothing"
    );
}

/// Update Server is offered only when a verified read found a newer verified build than the one
/// the server approved, and its badge names that build. Otherwise Check Now reads again.
#[gpui::test]
fn only_a_verified_newer_build_offers_update_server(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    for offer in [
        RemoteUpdateOffer::Unchecked,
        RemoteUpdateOffer::Unavailable,
        RemoteUpdateOffer::UpToDate,
    ] {
        servers.operations.offer("penguin", offer.clone());
        servers.cx.run_until_parked();

        assert!(!rendered(VERSION_UPDATE, servers.cx), "{offer:?}");
        assert!(rendered(CHECK_NOW, servers.cx), "{offer:?}");
    }

    servers.operations.offer("penguin", available());
    servers.cx.run_until_parked();

    assert!(rendered(VERSION_UPDATE, servers.cx));
    assert!(
        explains("1.5.0 available", servers.cx),
        "the badge names the verified build, not this SpaceTerm's version"
    );
    assert!(servers.operations.calls().is_empty());
}

/// An update names both exact versions, ends the Sessions it warns about, and is confirmed once.
#[gpui::test]
fn updating_names_both_versions_and_asks_once(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    servers.operations.offer("penguin", available());
    servers.cx.run_until_parked();

    click(VERSION_UPDATE, servers.cx);

    assert!(alert_open("settings-ssh-server-update-confirm", servers.cx));
    assert!(explains(
        "Update the penguin server to version 1.5.0 (commit bbbbbbb) and end all of its Terminal \
         Sessions?",
        servers.cx
    ));

    click_modal("settings-ssh-server-update-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![
            Call::Describe(RemoteMaintenanceRequest::Update),
            Call::Confirm(RemoteOperationHandle(1)),
        ]
    );
    assert!(explains(
        "now runs version 1.5.0 (commit bbbbbbb)",
        servers.cx
    ));
}

/// A validated result stays until the person acknowledges it, which is an ordinary dismissal.
#[gpui::test]
fn a_validated_result_waits_for_an_ordinary_acknowledgement(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Resolved {
            handle: RemoteOperationHandle(4),
            kind: SubmittedKind::Service(Action::Stop),
            outcome: completed(Some(status(true, false, true))),
        },
    );
    servers.cx.run_until_parked();

    assert!(explains("The server on penguin stopped", servers.cx));
    assert!(!rendered(CHECK_NOW, servers.cx));

    click(STATUS_ACKNOWLEDGE, servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Acknowledge(RemoteOperationHandle(4))]
    );
    assert!(status_reads("Stopped", servers.cx));
}

/// An answer that never arrived stays unconfirmed. Asking again resolves the same original and
/// never repeats it.
#[gpui::test]
fn an_unconfirmed_operation_is_resolved_and_never_repeated(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
    );
    servers
        .operations
        .resume_with(RemoteOperationState::Resolved {
            handle: RemoteOperationHandle(5),
            kind: SubmittedKind::Service(Action::Stop),
            outcome: completed(Some(status(true, false, true))),
        });
    servers.cx.run_until_parked();

    assert!(status_reads("Unconfirmed", servers.cx));
    assert!(explains(
        "didn\u{2019}t hear whether penguin stopped its server",
        servers.cx
    ));

    click("settings-ssh-server-service-again", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Resume]);
    assert!(explains("The server on penguin stopped", servers.cx));
}

/// While an operation is underway, its row reports the phase and the other row offers nothing.
#[gpui::test]
fn progress_names_the_phase_and_only_one_row_reports_it(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Working {
            phase: MaintenancePhase::Dispatching,
            kind: Some(SubmittedKind::Service(Action::Stop)),
            cancellable: true,
        },
    );
    servers.cx.run_until_parked();

    assert!(rendered(
        "settings-server-status-progress-track",
        servers.cx
    ));
    assert!(!rendered(
        "settings-server-version-progress-track",
        servers.cx
    ));
    assert!(explains("Stopping the server on penguin", servers.cx));
    assert!(rendered(
        "settings-ssh-server-maintenance-cancel",
        servers.cx
    ));
    assert!(!rendered(VERSION_UPDATE, servers.cx));

    click("settings-ssh-server-maintenance-cancel", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Cancel]);
}

/// Assistive technology hears the machine each state and step names.
#[gpui::test]
fn assistive_technology_hears_the_machine_each_step_acts_on(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    servers.operations.offer("penguin", available());
    servers.cx.run_until_parked();

    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.with_role("Status")
            .iter()
            .any(|status| status["aria"]["label"]
                .as_str()
                .is_some_and(|label| label.starts_with("penguin: "))),
        "the state is announced with the machine it reports"
    );
    assert_eq!(
        tree.node("Update Server\u{2026}")["aria"]["description"],
        "Transfers the verified server version SpaceTerm includes to penguin, then asks before it \
         replaces the approved one."
    );
}

/// The saved connection's removal leaves no maintenance row for it, and the registry keeps the
/// removal.
#[gpui::test]
fn removing_the_connection_removes_its_maintenance_rows(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    servers.operations.set(
        "penguin",
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
    );
    servers.cx.run_until_parked();

    servers.connections.remove(&destination);
    servers.operations.remove("penguin");
    servers.cx.run_until_parked();

    for step in SSH_STEPS {
        assert!(!rendered(step, servers.cx), "{step} would act on penguin");
    }
    assert!(
        !status_reads("", servers.cx),
        "the card presents another server once penguin is removed"
    );
    assert!(
        servers.operations.calls().is_empty(),
        "removing a connection asks the machine nothing, got {:?}",
        servers.operations.calls()
    );
}

/// Removing the saved connection keeps the operation's outcome understandable in the same
/// Servers surface. The entry offers only the step that settles it, and nothing that would bring
/// the connection back.
#[gpui::test]
fn a_removed_connection_keeps_what_spaceterm_still_has_to_say(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    servers.operations.set(
        "penguin",
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
    );
    servers
        .operations
        .resume_with(RemoteOperationState::Resolved {
            handle: RemoteOperationHandle(7),
            kind: SubmittedKind::Service(Action::Stop),
            outcome: completed(Some(status(true, false, true))),
        });
    servers.cx.run_until_parked();

    servers.connections.remove(&destination);
    servers.operations.remove("penguin");
    servers.cx.run_until_parked();

    assert!(rendered(REMOVED_NOTICE, servers.cx));
    assert!(explains(
        "didn\u{2019}t hear whether penguin stopped its server",
        servers.cx
    ));
    assert!(explains("Nothing here reconnects", servers.cx));
    for absent in [
        "settings-ssh-server-connect",
        "settings-ssh-server-check",
        "settings-ssh-server-service-check-penguin",
        "settings-ssh-server-service-start-penguin",
    ] {
        assert!(
            !rendered(absent, servers.cx),
            "{absent} would reach a connection the person removed"
        );
    }

    click("settings-ssh-server-service-again-penguin", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Resume]);
    assert!(explains("The server on penguin stopped", servers.cx));

    click(
        "settings-ssh-server-maintenance-acknowledge-penguin",
        servers.cx,
    );

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Resume, Call::Acknowledge(RemoteOperationHandle(7))]
    );
    assert!(
        !rendered("settings-server-notice-removed-penguin", servers.cx),
        "an acknowledged result leaves nothing to say about a removed connection"
    );
}

/// The removed-connections section also lists operations that have no result: one still
/// unconfirmed, and one still working. Its caption says what is true of every entry it lists.
#[gpui::test]
fn the_removed_section_claims_no_result_it_does_not_have(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    servers.operations.set(
        "penguin",
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
    );
    servers.cx.run_until_parked();
    servers.connections.remove(&destination);
    servers.operations.remove("penguin");
    servers.cx.run_until_parked();

    let listed = [REMOVED_NOTICE];
    let unresolved = [
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
        RemoteOperationState::Working {
            phase: MaintenancePhase::Resolving,
            kind: Some(SubmittedKind::Service(Action::Stop)),
            cancellable: true,
        },
    ];
    for state in unresolved {
        servers.operations.set("penguin", state.clone());
        servers.cx.run_until_parked();

        for selector in listed {
            assert!(
                rendered(selector, servers.cx),
                "{state:?} is still something to say about a removed connection"
            );
        }
        assert!(
            !explains("has the result of work you asked for", servers.cx),
            "{state:?} has no result, so the section must not claim one"
        );
        assert!(explains("Nothing here reconnects", servers.cx));
    }
}

/// One paused operation the destination can no longer answer, as Discard finds it.
fn paused(kind: SubmittedKind) -> RemoteOperationState {
    RemoteOperationState::Paused {
        reason: ContinuityChange::HostChanged,
        kind: Some(kind),
    }
}

/// Discard retires the only record that could still say what the operation did, so the selected
/// server's detail asks first and sends nothing to the machine. Cancelling keeps the record.
#[gpui::test]
fn discarding_an_operation_asks_before_it_retires_the_record(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers
        .operations
        .set("penguin", paused(SubmittedKind::Service(Action::Stop)));
    servers.cx.run_until_parked();

    click(STATUS_DISCARD, servers.cx);

    assert!(alert_open(
        "settings-ssh-server-discard-confirm",
        servers.cx
    ));
    assert!(
        servers.operations.calls().is_empty(),
        "nothing is retired until the person confirms, got {:?}",
        servers.operations.calls()
    );
    assert!(explains(
        "Discard the stop SpaceTerm sent to penguin?",
        servers.cx
    ));
    assert!(explains("what the stop did on penguin", servers.cx));

    click_modal("settings-ssh-server-discard-cancel", servers.cx);

    assert!(
        servers.operations.calls().is_empty(),
        "cancelling keeps the record, got {:?}",
        servers.operations.calls()
    );
    assert!(rendered(STATUS_DISCARD, servers.cx));

    click(STATUS_DISCARD, servers.cx);
    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(
            servers.operations.operation_of("penguin", false)
        )],
        "the selected server's own row settles its own original"
    );
}

/// A removed connection's entry offers the same Discard, and it asks there too.
#[gpui::test]
fn discarding_a_removed_connections_operation_asks_first(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    servers
        .operations
        .set("penguin", paused(SubmittedKind::Service(Action::Uninstall)));
    servers.cx.run_until_parked();
    servers.connections.remove(&destination);
    servers.operations.remove("penguin");
    servers.cx.run_until_parked();

    click(
        "settings-ssh-server-maintenance-discard-penguin",
        servers.cx,
    );

    assert!(alert_open(
        "settings-ssh-server-discard-confirm",
        servers.cx
    ));
    assert!(
        servers.operations.calls().is_empty(),
        "nothing is retired until the person confirms, got {:?}",
        servers.operations.calls()
    );
    assert!(explains(
        "Discard the removal SpaceTerm sent to penguin?",
        servers.cx
    ));

    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(
            servers.operations.operation_of("penguin", true)
        )],
        "the removed connections row settles the removed connection's original"
    );
}

/// A removed connection with nothing left to tell is not listed, so the surface never invites a
/// reconnection it has no reason to offer.
#[gpui::test]
fn a_removed_connection_with_nothing_to_say_is_not_listed(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    servers.cx.run_until_parked();

    servers.connections.remove(&destination);
    servers.operations.remove("penguin");
    servers.cx.run_until_parked();

    assert!(!rendered(
        "settings-server-notice-removed-penguin",
        servers.cx
    ));
}

fn search(window: &Entity<SettingsWindow>, query: &str, cx: &mut VisualTestContext) {
    cx.update(|_, cx| {
        let search = window.read(cx).search.clone();
        search.update(cx, |search, cx| search.set_value(query.to_owned(), cx));
    });
    cx.run_until_parked();
}

/// Settings Search reaches the remote maintenance rows by the words a person types for these
/// operations, and searching asks the other computer nothing.
#[gpui::test]
fn settings_search_reaches_the_maintenance_rows_without_reaching_the_machine(
    cx: &mut TestAppContext,
) {
    let servers = open_maintenance(cx);

    for (query, row) in [
        ("stop server", SERVICE_ROW),
        ("restart server", SERVICE_ROW),
        ("uninstall", SERVICE_ROW),
        ("update server", UPDATE_ROW),
        ("finish update", UPDATE_ROW),
    ] {
        search(&servers.window, query, servers.cx);

        assert!(
            rendered(row, servers.cx),
            "{query:?} should reach {row}, and the row's machine is the one selected"
        );
        assert!(
            servers.operations.calls().is_empty(),
            "{query:?} must ask the machine nothing, got {:?}",
            servers.operations.calls()
        );
    }
}

/// What Check Status already observed decides what the rows offer before any maintenance read,
/// so a server the person just checked can be stopped without a reading step of its own.
#[gpui::test]
fn an_observed_server_offers_its_steps_before_any_maintenance_read(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");

    servers.connections.check(&destination);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(
        servers.connections.status(&destination),
        RemoteConnectionStatus::Checked { .. }
    ) {
        assert!(Instant::now() < deadline, "the check settles");
        std::thread::sleep(Duration::from_millis(2));
    }
    servers.cx.run_until_parked();

    assert!(
        status_reads(RUNNING_UNCONNECTED, servers.cx),
        "Check Status says what it found"
    );
    for offered in [STOP_SERVER, RESTART_SERVER, UNINSTALL_SERVER] {
        assert!(
            menu_offers(offered, servers.cx),
            "{offered} should be offered"
        );
    }
    assert!(rendered(CHECK_NOW, servers.cx));
    assert!(
        !rendered(VERSION_UPDATE, servers.cx),
        "an observed connection has no verified read, so nothing proves an update is newer"
    );
    assert!(!rendered("settings-ssh-server-service-start", servers.cx));
    assert!(
        servers.operations.calls().is_empty(),
        "the observation needed no maintenance read, got {:?}",
        servers.operations.calls()
    );

    choose("settings-ssh-server-service-stop", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Describe(RemoteMaintenanceRequest::Service(
            Action::Stop
        ))],
        "the step still describes its change against fresh facts"
    );
    assert!(alert_open("settings-ssh-server-stop-confirm", servers.cx));
}

/// The selected server's detail reads from its state down to its details: what it is doing, the
/// steps that change it, then the facts SpaceTerm observed.
#[gpui::test]
fn the_detail_presents_the_controls_above_the_facts(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);

    let top = |cx: &mut VisualTestContext, selector: &str| {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"))
            .top()
    };
    let order = [
        SERVICE_ROW,
        UPDATE_ROW,
        "settings-row-server-auto-start",
        "settings-row-end-all-sessions",
        "settings-row-network-access",
    ];
    let tops: Vec<_> = order
        .iter()
        .map(|selector| top(servers.cx, selector))
        .collect();
    assert!(
        tops.windows(2).all(|pair| pair[0] < pair[1]),
        "the detail should read in order, got {order:?} at {tops:?}"
    );
}

/// The record of these operations is this client's own, and the Servers section says when it
/// cannot be written. The result stays on the row, OK leaves it there, and the section says that
/// it is gone after SpaceTerm quits. The saved connections are a different record, so their
/// notice stays silent.
#[gpui::test]
fn a_result_this_client_cannot_save_says_what_ok_leaves_behind(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers
        .operations
        .cannot_save(SshIntentStoreError::Unavailable);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Resolved {
            handle: RemoteOperationHandle(4),
            kind: SubmittedKind::Service(Action::Stop),
            outcome: completed(Some(status(true, false, true))),
        },
    );
    servers.cx.run_until_parked();

    assert!(
        rendered(OPERATIONS_PERSISTENCE, servers.cx),
        "the section says what it couldn\u{2019}t save before the person clicks anything"
    );
    assert!(
        announces(
            "couldn\u{2019}t save what it knows about the operations",
            servers.cx
        ),
        "the notice names what SpaceTerm couldn\u{2019}t save"
    );
    assert!(
        announces(
            "Choosing OK or Discard leaves the operation listed.",
            servers.cx
        ),
        "the notice says the step the person is about to take doesn\u{2019}t clear it"
    );
    assert!(
        announces("gone after SpaceTerm quits", servers.cx),
        "the notice says the result on screen doesn\u{2019}t survive quitting"
    );
    assert!(
        !rendered("settings-server-notice-ssh-record", servers.cx),
        "the saved connections saved, so their own notice claims nothing"
    );

    click(STATUS_ACKNOWLEDGE, servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Acknowledge(RemoteOperationHandle(4))]
    );
    assert!(
        rendered(STATUS_ACKNOWLEDGE, servers.cx),
        "the result SpaceTerm couldn\u{2019}t retire stays on the row"
    );
    assert!(explains("The server on penguin stopped", servers.cx));
    assert!(rendered(OPERATIONS_PERSISTENCE, servers.cx));
}

/// A damaged record reads differently from storage SpaceTerm couldn't reach, and discarding an
/// original it cannot retire leaves that original followed.
#[gpui::test]
fn a_discard_this_client_cannot_save_says_the_original_stays(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers
        .operations
        .cannot_save(SshIntentStoreError::Malformed);
    servers
        .operations
        .set("penguin", paused(SubmittedKind::Service(Action::Stop)));
    servers.cx.run_until_parked();

    assert!(
        announces("is damaged, so it left the record unchanged", servers.cx),
        "a damaged record says that SpaceTerm left it as it was"
    );

    click(STATUS_DISCARD, servers.cx);
    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(
            servers.operations.operation_of("penguin", false)
        )]
    );
    assert!(
        rendered(STATUS_DISCARD, servers.cx),
        "the original SpaceTerm couldn\u{2019}t stop following stays"
    );
    assert!(announces(
        "Choosing OK or Discard leaves the operation listed.",
        servers.cx
    ));
}

/// A removed connection's server that has no record of its original leaves nothing to resolve, so
/// the entry says where it ends and one step clears it. Nothing is sent, so it asks nothing.
#[gpui::test]
fn a_removed_connections_unknown_original_ends_with_a_word(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    servers.operations.set(
        "penguin",
        RemoteOperationState::Unconfirmed {
            kind: SubmittedKind::Service(Action::Stop),
        },
    );
    servers.cx.run_until_parked();
    servers.connections.remove(&destination);
    servers.operations.remove("penguin");
    servers.operations.set(
        "penguin",
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Forgotten,
            kind: None,
        },
    );
    servers.cx.run_until_parked();

    assert!(
        rendered(REMOVED_NOTICE, servers.cx),
        "the entry says where it ends instead of disappearing without a word"
    );
    assert!(explains(
        "penguin has no record of the operation SpaceTerm sent.",
        servers.cx
    ));
    assert!(explains(
        "may never have arrived, or that server may have dropped it",
        servers.cx
    ));
    assert!(explains("has stopped following it", servers.cx));
    for absent in [
        "settings-ssh-server-service-again-penguin",
        "settings-ssh-server-maintenance-discard-penguin",
        "settings-ssh-server-service-check-penguin",
    ] {
        assert!(
            !rendered(absent, servers.cx),
            "{absent} offers a step that cannot settle an original the service has no record of"
        );
    }

    // The entry's listing ends when it is cleared, so its original is read before the step.
    let cleared = servers.operations.operation_of("penguin", true);
    click(
        "settings-ssh-server-maintenance-dismiss-penguin",
        servers.cx,
    );

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Dismiss(cleared)],
        "clearing the entry sends nothing and asks nothing"
    );
    assert!(
        !rendered("settings-server-notice-removed-penguin", servers.cx),
        "the cleared entry leaves nothing to say about a removed connection"
    );
}

/// A read of this client's retained records that fails and later lands leaves the owner holding
/// two originals on one machine, neither a duplicate of the other. A row reports one state, so
/// the row reports one of the originals and names the one it has still to report. The step it
/// offers settles the original it reports, and the one that was waiting takes its place, so
/// nothing a person asked for on that machine is unreachable.
#[gpui::test]
fn a_second_original_on_one_machine_is_named_and_settled_in_turn(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    // The owner holds the original the person just submitted first, and the one its retry
    // recovered second. Both belong to the connection still saved.
    servers.operations.hold(
        "penguin",
        [
            RemoteOperationState::Unconfirmed {
                kind: SubmittedKind::Service(Action::Stop),
            },
            paused(SubmittedKind::Service(Action::Restart)),
        ],
    );
    servers.cx.run_until_parked();

    assert!(
        announces("penguin: Unconfirmed", servers.cx),
        "the row reports the original the person's last step produced"
    );
    assert!(explains(
        "didn\u{2019}t hear whether penguin stopped its server",
        servers.cx
    ));
    assert!(
        explains(
            "You asked for one more operation on penguin: the restart.",
            servers.cx
        ),
        "the row must name the original it has still to report"
    );
    assert!(
        explains("reports that one here once this one is settled", servers.cx),
        "the row must say the named original gets its turn"
    );

    // Discard settles the original the row reports, and asks about that one alone.
    click(STATUS_DISCARD, servers.cx);

    assert!(explains(
        "Discard the stop SpaceTerm sent to penguin?",
        servers.cx
    ));

    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(
            servers.operations.operation_of("penguin", false)
        )],
        "the row settles the one original it reports"
    );
    assert!(
        explains(
            "can\u{2019}t ask it about the restart it already sent",
            servers.cx
        ),
        "the original that was waiting takes the settled one's place in the row"
    );
    assert!(
        !explains("one more operation on penguin", servers.cx),
        "nothing is left waiting once the machine holds one original"
    );
}

/// A removed connection holding two originals is listed once. Two entries under one machine name
/// would give the person two sets of steps with nothing to tell them apart, so the entry reports
/// one original, names the other, and its step settles the one it reports.
#[gpui::test]
fn a_removed_connection_with_two_originals_is_listed_once(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    let destination = SshDestination::new("penguin".to_owned()).expect("destination");
    servers.operations.hold(
        "penguin",
        [
            paused(SubmittedKind::Service(Action::Stop)),
            RemoteOperationState::Unconfirmed {
                kind: SubmittedKind::Service(Action::Uninstall),
            },
        ],
    );
    servers.cx.run_until_parked();
    servers.connections.remove(&destination);
    servers.operations.remove("penguin");
    servers.cx.run_until_parked();

    assert!(rendered(REMOVED_NOTICE, servers.cx));
    assert!(explains(
        "can\u{2019}t ask it about the stop it already sent",
        servers.cx
    ));
    assert!(
        explains(
            "You asked for one more operation on penguin: the removal.",
            servers.cx
        ),
        "the entry must name the original it has still to report"
    );

    click(
        "settings-ssh-server-maintenance-discard-penguin",
        servers.cx,
    );

    assert!(
        explains("Discard the stop SpaceTerm sent to penguin?", servers.cx),
        "the step settles the original the entry reports"
    );

    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(
            servers.operations.operation_of("penguin", true)
        )]
    );
    assert!(
        explains(
            "didn\u{2019}t hear whether penguin removed its service",
            servers.cx
        ),
        "the original that was waiting takes the settled one's place"
    );
    assert!(
        !explains("one more operation on penguin", servers.cx),
        "nothing is left waiting once the entry holds one original"
    );
}

#[gpui::test]
fn discarding_the_update_row_retires_only_its_original(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.hold(
        "penguin",
        [
            paused(SubmittedKind::Service(Action::Stop)),
            paused(SubmittedKind::Update {
                candidate: build("1.5.0", 'b').digest,
            }),
        ],
    );
    servers.cx.run_until_parked();

    click(VERSION_DISCARD, servers.cx);
    assert!(explains(
        "Discard the update SpaceTerm sent to penguin?",
        servers.cx
    ));
    assert!(servers.operations.calls().is_empty());
    click_modal("settings-ssh-server-discard-cancel", servers.cx);
    assert!(servers.operations.calls().is_empty());
    click(VERSION_DISCARD, servers.cx);
    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Discard([2; 32])]);
    assert!(
        rendered(STATUS_DISCARD, servers.cx),
        "the stop's own original stays on Status"
    );
    assert!(!rendered(VERSION_DISCARD, servers.cx));
    assert!(explains(
        "can\u{2019}t ask it about the stop it already sent",
        servers.cx
    ));
}

#[gpui::test]
fn acknowledging_the_update_row_retires_its_second_resolved_original(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.hold(
        "penguin",
        [
            RemoteOperationState::Resolved {
                handle: RemoteOperationHandle(4),
                kind: SubmittedKind::Service(Action::Stop),
                outcome: completed(Some(status(true, false, true))),
            },
            RemoteOperationState::Resolved {
                handle: RemoteOperationHandle(5),
                kind: SubmittedKind::Return,
                outcome: completed(Some(status(true, false, true))),
            },
        ],
    );
    servers.cx.run_until_parked();

    click(VERSION_ACKNOWLEDGE, servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Acknowledge(RemoteOperationHandle(5))]
    );
    assert!(
        rendered(STATUS_ACKNOWLEDGE, servers.cx),
        "the stop's own result stays on Status"
    );
    assert!(!rendered(VERSION_ACKNOWLEDGE, servers.cx));
    assert!(explains("The server on penguin stopped", servers.cx));
    click(STATUS_ACKNOWLEDGE, servers.cx);
    assert_eq!(
        servers.operations.calls(),
        vec![
            Call::Acknowledge(RemoteOperationHandle(5)),
            Call::Acknowledge(RemoteOperationHandle(4)),
        ]
    );
    assert!(!rendered(STATUS_ACKNOWLEDGE, servers.cx));
}

/// Start automatically changes only the SSH server's start policy, over its account route. It
/// starts, stops and removes nothing, and asks nothing first.
#[gpui::test]
fn the_automatic_start_switch_changes_only_the_start_policy(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    servers.cx.run_until_parked();
    assert!(starts_automatically(servers.cx));

    click(super::super::server_card::AUTO_START_SELECTOR, servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::AutomaticStart(false)]
    );
    assert!(!starts_automatically(servers.cx));
    assert!(
        menu_offers(STOP_SERVER, servers.cx),
        "the server keeps running, so it can still be stopped"
    );
    assert!(!alert_open("settings-ssh-server-stop-confirm", servers.cx));

    click(super::super::server_card::AUTO_START_SELECTOR, servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::AutomaticStart(false), Call::AutomaticStart(true)],
        "the switch performs no Start, Stop or Uninstall"
    );
    assert!(starts_automatically(servers.cx));
}
