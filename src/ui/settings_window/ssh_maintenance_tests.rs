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
    RemoteConnectionStatus, RetainedSshConnection, ServerAvailability, SshConnectBackend,
    SshConnectFailure, SshConnectionIntentStore, SshIntentStoreError, SshServerConnections,
    SshServerContinuity, SshServerFacts, SshServerObservation,
};
use crate::ssh::server_maintenance::{MaintenancePhase, SshMaintenanceFailure, SubmittedKind};
use crate::ssh::server_operations::{
    RemoteMaintenanceRequest, RemoteOperationEntry, RemoteOperationHandle, RemoteOperationState,
    RemoteServerOperations, RetainedOutcome,
};
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;

use super::REMOVED_SELECTOR;

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
const SERVICE_ROW: &str = "settings-row-ssh-server-service";
/// The notice that this client couldn't read or write its record of these operations.
const OPERATIONS_PERSISTENCE: &str = super::super::ssh_servers::OPERATIONS_PERSISTENCE_SELECTOR;
const UPDATE_ROW: &str = "settings-row-ssh-server-update";

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

    fn refresh(&self, destination: &SshDestination) {
        self.record(Call::Refresh);
        self.set(
            destination.as_str(),
            RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
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
    click("settings-server-list-1", cx);
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

/// Whether any text the page publishes carries `text`, as assistive technology reads it.
fn says(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).with_role("Label").iter().any(|node| {
        node["aria"]["value"]
            .as_str()
            .is_some_and(|published| published.contains(text))
    })
}

/// Whether any status the page publishes carries `text`, as assistive technology announces it.
fn announces(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).with_role("Status").iter().any(|node| {
        node["aria"]["label"]
            .as_str()
            .is_some_and(|published| published.contains(text))
    })
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
    assert!(rendered(
        "settings-ssh-server-service-state-not-checked",
        servers.cx
    ));
    assert!(rendered(
        "settings-ssh-server-service-control-check",
        servers.cx
    ));
    assert!(!rendered(
        "settings-ssh-server-service-control-stop",
        servers.cx
    ));
}

/// The local server keeps its own rows, and the remote maintenance rows belong to the selected
/// SSH server alone.
#[gpui::test]
fn the_local_server_keeps_its_own_rows(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);

    click("settings-server-list-0", servers.cx);

    assert!(!rendered(SERVICE_ROW, servers.cx));
    assert!(!rendered(UPDATE_ROW, servers.cx));
}

/// One explicit read reads the service once and starts nothing.
#[gpui::test]
fn check_now_reads_the_service_once(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);

    click("settings-ssh-server-service-control-check", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Refresh]);
    assert!(rendered(
        "settings-ssh-server-service-state-running",
        servers.cx
    ));
    assert!(says("The server on penguin is running", servers.cx));
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

    click("settings-ssh-server-service-control-stop", servers.cx);

    assert!(alert_open("settings-ssh-server-stop-confirm", servers.cx));
    assert_eq!(
        servers.operations.calls(),
        vec![Call::Describe(RemoteMaintenanceRequest::Service(
            Action::Stop
        ))],
        "describing a change sends no operation"
    );
    assert!(says(
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
    assert!(says("The server on penguin stopped", servers.cx));
    assert!(rendered(
        "settings-ssh-server-service-control-acknowledge",
        servers.cx
    ));
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

    click("settings-ssh-server-service-control-stop", servers.cx);
    click_modal("settings-ssh-server-stop-cancel", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![
            Call::Describe(RemoteMaintenanceRequest::Service(Action::Stop)),
            Call::Cancel,
        ]
    );
    assert!(servers.operations.described("penguin").is_none());
    assert!(rendered(
        "settings-ssh-server-service-state-running",
        servers.cx
    ));
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

    assert!(rendered(
        "settings-ssh-server-service-control-start",
        servers.cx
    ));
    assert!(!rendered(
        "settings-ssh-server-service-control-restart",
        servers.cx
    ));
    assert!(!rendered(
        "settings-ssh-server-service-control-stop",
        servers.cx
    ));

    click("settings-ssh-server-service-control-start", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Start]);
    assert!(
        !alert_open("settings-ssh-server-stop-confirm", servers.cx),
        "starting ends no work, so it asks nothing"
    );
}

/// An update names both exact versions, ends the Sessions it warns about, and is confirmed once.
#[gpui::test]
fn updating_names_both_versions_and_asks_once(cx: &mut TestAppContext) {
    let servers = open_maintenance(cx);
    servers.operations.set(
        "penguin",
        RemoteOperationState::Read(Box::new(facts(status(true, true, false)))),
    );
    servers.cx.run_until_parked();

    click("settings-ssh-server-update-control-update", servers.cx);

    assert!(alert_open("settings-ssh-server-update-confirm", servers.cx));
    assert!(says(
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
    assert!(says("now runs version 1.5.0 (commit bbbbbbb)", servers.cx));
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

    assert!(says("The server on penguin stopped", servers.cx));
    assert!(!rendered(
        "settings-ssh-server-service-control-check",
        servers.cx
    ));

    click(
        "settings-ssh-server-service-control-acknowledge",
        servers.cx,
    );

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Acknowledge(RemoteOperationHandle(4))]
    );
    assert!(rendered(
        "settings-ssh-server-service-state-stopped",
        servers.cx
    ));
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

    assert!(rendered(
        "settings-ssh-server-service-state-unconfirmed",
        servers.cx
    ));
    assert!(says(
        "didn\u{2019}t hear whether penguin stopped its server",
        servers.cx
    ));

    click(
        "settings-ssh-server-service-control-check-again",
        servers.cx,
    );

    assert_eq!(servers.operations.calls(), vec![Call::Resume]);
    assert!(says("The server on penguin stopped", servers.cx));
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
        "settings-ssh-server-service-progress-track",
        servers.cx
    ));
    assert!(!rendered(
        "settings-ssh-server-update-progress-track",
        servers.cx
    ));
    assert!(says("Stopping the server on penguin", servers.cx));
    assert!(rendered(
        "settings-ssh-server-service-control-cancel",
        servers.cx
    ));
    assert!(!rendered(
        "settings-ssh-server-update-control-update",
        servers.cx
    ));

    click("settings-ssh-server-service-control-cancel", servers.cx);

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
    servers.cx.run_until_parked();

    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.with_role("Status")
            .iter()
            .any(|status| status["aria"]["label"] == "penguin: Running"),
        "the state is announced with the machine it reports"
    );
    let stop = tree.node("Stop Server");
    assert_eq!(stop["aria"]["role"], "Button");
    assert_eq!(
        stop["aria"]["description"],
        "Ends the Terminal Sessions on penguin for every client. SpaceTerm asks first."
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

    assert!(!rendered(SERVICE_ROW, servers.cx));
    assert!(!rendered(UPDATE_ROW, servers.cx));
    assert!(
        servers.operations.calls().is_empty(),
        "removing a connection asks the machine nothing, got {:?}",
        servers.operations.calls()
    );
    let _ = &servers.window;
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

    assert!(rendered(
        "settings-row-removed-server-operations",
        servers.cx
    ));
    assert!(rendered(
        "settings-removed-server-operations-penguin",
        servers.cx
    ));
    assert!(says(
        "didn\u{2019}t hear whether penguin stopped its server",
        servers.cx
    ));
    assert!(says("Nothing here reconnects", servers.cx));
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
    assert!(says("The server on penguin stopped", servers.cx));

    click(
        "settings-ssh-server-maintenance-acknowledge-penguin",
        servers.cx,
    );

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Resume, Call::Acknowledge(RemoteOperationHandle(7))]
    );
    assert!(
        !rendered("settings-row-removed-server-operations", servers.cx),
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

    let listed = ["settings-row-removed-server-operations", REMOVED_SELECTOR];
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
            !says("has the result of work you asked for", servers.cx),
            "{state:?} has no result, so the section must not claim one"
        );
        assert!(
            says("whether or not it has a result yet", servers.cx),
            "the section says that an entry may have no result yet"
        );
        assert!(says("Nothing here reconnects", servers.cx));
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

    click("settings-ssh-server-service-control-discard", servers.cx);

    assert!(alert_open(
        "settings-ssh-server-discard-confirm",
        servers.cx
    ));
    assert!(
        servers.operations.calls().is_empty(),
        "nothing is retired until the person confirms, got {:?}",
        servers.operations.calls()
    );
    assert!(says(
        "Discard the stop SpaceTerm sent to penguin?",
        servers.cx
    ));
    assert!(says("what the stop did on penguin", servers.cx));

    click_modal("settings-ssh-server-discard-cancel", servers.cx);

    assert!(
        servers.operations.calls().is_empty(),
        "cancelling keeps the record, got {:?}",
        servers.operations.calls()
    );
    assert!(rendered(
        "settings-ssh-server-service-control-discard",
        servers.cx
    ));

    click("settings-ssh-server-service-control-discard", servers.cx);
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
    assert!(says(
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
        "settings-row-removed-server-operations",
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

    assert!(rendered(
        "settings-ssh-server-service-state-observed",
        servers.cx
    ));
    assert!(says(
        "was running when SpaceTerm last reached it",
        servers.cx
    ));
    for offered in [
        "settings-ssh-server-service-control-stop",
        "settings-ssh-server-service-control-restart",
        "settings-ssh-server-service-control-uninstall",
        "settings-ssh-server-service-control-check",
        "settings-ssh-server-update-control-update",
    ] {
        assert!(rendered(offered, servers.cx), "{offered} should be offered");
    }
    assert!(!rendered(
        "settings-ssh-server-service-control-start",
        servers.cx
    ));
    assert!(
        servers.operations.calls().is_empty(),
        "the observation needed no maintenance read, got {:?}",
        servers.operations.calls()
    );

    click("settings-ssh-server-service-control-stop", servers.cx);

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
        "settings-row-ssh-server-status",
        SERVICE_ROW,
        UPDATE_ROW,
        "settings-row-ssh-server-version",
        "settings-row-ssh-server-platform",
        "settings-row-ssh-server-compatibility",
        "settings-row-ssh-server-startup",
        "settings-row-end-all-sessions",
        "settings-row-remove-server",
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
        !rendered(super::super::ssh_servers::PERSISTENCE_SELECTOR, servers.cx),
        "the saved connections saved, so their own notice claims nothing"
    );

    click(
        "settings-ssh-server-service-control-acknowledge",
        servers.cx,
    );

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Acknowledge(RemoteOperationHandle(4))]
    );
    assert!(
        rendered(
            "settings-ssh-server-service-control-acknowledge",
            servers.cx
        ),
        "the result SpaceTerm couldn\u{2019}t retire stays on the row"
    );
    assert!(says("The server on penguin stopped", servers.cx));
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

    click("settings-ssh-server-service-control-discard", servers.cx);
    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(
            servers.operations.operation_of("penguin", false)
        )]
    );
    assert!(
        rendered("settings-ssh-server-service-control-discard", servers.cx),
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
        rendered("settings-removed-server-operations-penguin", servers.cx),
        "the entry says where it ends instead of disappearing without a word"
    );
    assert!(says(
        "penguin has no record of the operation SpaceTerm sent.",
        servers.cx
    ));
    assert!(says(
        "may never have arrived, or that server may have dropped it",
        servers.cx
    ));
    assert!(says("has stopped following it", servers.cx));
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
        !rendered("settings-row-removed-server-operations", servers.cx),
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
    assert!(says(
        "didn\u{2019}t hear whether penguin stopped its server",
        servers.cx
    ));
    assert!(
        says(
            "You asked for one more operation on penguin: the restart.",
            servers.cx
        ),
        "the row must name the original it has still to report"
    );
    assert!(
        says("reports that one here once this one is settled", servers.cx),
        "the row must say the named original gets its turn"
    );

    // Discard settles the original the row reports, and asks about that one alone.
    click("settings-ssh-server-service-control-discard", servers.cx);

    assert!(says(
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
        says(
            "can\u{2019}t ask it about the restart it already sent",
            servers.cx
        ),
        "the original that was waiting takes the settled one's place in the row"
    );
    assert!(
        !says("one more operation on penguin", servers.cx),
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

    assert!(rendered(
        "settings-removed-server-operations-penguin",
        servers.cx
    ));
    assert!(says(
        "can\u{2019}t ask it about the stop it already sent",
        servers.cx
    ));
    assert!(
        says(
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
        says("Discard the stop SpaceTerm sent to penguin?", servers.cx),
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
        says(
            "didn\u{2019}t hear whether penguin removed its service",
            servers.cx
        ),
        "the original that was waiting takes the settled one's place"
    );
    assert!(
        !says("one more operation on penguin", servers.cx),
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

    click("settings-ssh-server-update-control-discard", servers.cx);
    assert!(says(
        "Discard the update SpaceTerm sent to penguin?",
        servers.cx
    ));
    assert!(servers.operations.calls().is_empty());
    click_modal("settings-ssh-server-discard-cancel", servers.cx);
    assert!(servers.operations.calls().is_empty());
    click("settings-ssh-server-update-control-discard", servers.cx);
    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(servers.operations.calls(), vec![Call::Discard([2; 32])]);
    assert!(rendered(
        "settings-ssh-server-service-control-discard",
        servers.cx
    ));
    assert!(!rendered(
        "settings-ssh-server-update-control-discard",
        servers.cx
    ));
    assert!(says(
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

    click("settings-ssh-server-update-control-acknowledge", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Acknowledge(RemoteOperationHandle(5))]
    );
    assert!(rendered(
        "settings-ssh-server-service-control-acknowledge",
        servers.cx
    ));
    assert!(!rendered(
        "settings-ssh-server-update-control-acknowledge",
        servers.cx
    ));
    assert!(says("The server on penguin stopped", servers.cx));
    click(
        "settings-ssh-server-service-control-acknowledge",
        servers.cx,
    );
    assert_eq!(
        servers.operations.calls(),
        vec![
            Call::Acknowledge(RemoteOperationHandle(5)),
            Call::Acknowledge(RemoteOperationHandle(4)),
        ]
    );
    assert!(!rendered(
        "settings-ssh-server-service-control-acknowledge",
        servers.cx
    ));
}
