//! The selected SSH server's maintenance rows, driven through a scripted operations owner whose
//! every call the test counts.

use std::collections::BTreeMap;
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
    RemoteMaintenanceRequest, RemoteOperationEntry, RemoteOperationHandle, RemoteOperationScope,
    RemoteOperationState, RemoteServerOperations, RetainedOutcome,
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
    Discard(RemoteOperationScope),
    Dismiss(RemoteOperationScope),
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

/// An owner whose states the test sets and whose calls it counts. Describing a change answers
/// with the exact change it describes, and only confirming it resolves an operation.
struct ScriptedOperations {
    states: Mutex<BTreeMap<String, (bool, RemoteOperationState)>>,
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
            states: Mutex::new(BTreeMap::new()),
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
        let removed = states.get(destination).is_some_and(|(removed, _)| *removed);
        states.insert(destination.to_owned(), (removed, state));
        drop(states);
        self.publish();
    }

    /// Keeps the state of a server whose saved connection the person removed.
    fn remove(&self, destination: &str) {
        if let Some(entry) = self.states.lock().expect("states").get_mut(destination) {
            entry.0 = true;
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
        match self.state(&SshDestination::new(destination.to_owned()).expect("destination")) {
            RemoteOperationState::Described { request, .. } => Some(request),
            _ => None,
        }
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
        self.states
            .lock()
            .expect("states")
            .iter()
            .map(|(destination, (removed, state))| RemoteOperationEntry {
                destination: SshDestination::new(destination.clone()).expect("destination"),
                removed: *removed,
                state: state.clone(),
            })
            .collect()
    }

    fn state(&self, destination: &SshDestination) -> RemoteOperationState {
        self.states
            .lock()
            .expect("states")
            .get(destination.as_str())
            .map_or(RemoteOperationState::Unread, |(_, state)| state.clone())
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
            .filter_map(|(destination, (_, state))| match state {
                RemoteOperationState::Described { request, .. } => {
                    Some((destination.clone(), *request))
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
        let acknowledged: Vec<_> = self
            .states
            .lock()
            .expect("states")
            .iter()
            .filter_map(|(destination, (_, state))| match state {
                RemoteOperationState::Resolved {
                    handle: held,
                    outcome,
                    ..
                } if *held == handle => Some((destination.clone(), outcome.clone())),
                _ => None,
            })
            .collect();
        for (destination, outcome) in acknowledged {
            // The owner returns to the facts the operation produced, so this reads them from the
            // outcome rather than inventing a state the row is then asserted to show.
            let observed = match outcome {
                RetainedOutcome::Completed {
                    status: Some(status),
                    ..
                } => status,
                _ => status(true, true, false),
            };
            self.set(
                &destination,
                RemoteOperationState::Read(Box::new(facts(observed))),
            );
        }
    }

    fn discard(&self, destination: &SshDestination, scope: RemoteOperationScope) {
        self.record(Call::Discard(scope));
        if !self.retires() {
            // An unretired record is still followed, so the original stays.
            self.publish();
            return;
        }
        self.set(destination.as_str(), RemoteOperationState::Unread);
    }

    fn dismiss(&self, destination: &SshDestination, scope: RemoteOperationScope) {
        self.record(Call::Dismiss(scope));
        // Nothing was retained for an original the service has no record of, so the entry goes
        // whether or not this client can write.
        self.states
            .lock()
            .expect("states")
            .remove(destination.as_str());
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
    assert!(rendered("settings-ssh-server-service-check", servers.cx));
    assert!(!rendered("settings-ssh-server-service-stop", servers.cx));
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

    click("settings-ssh-server-service-check", servers.cx);

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

    click("settings-ssh-server-service-stop", servers.cx);

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
        "settings-ssh-server-maintenance-acknowledge",
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

    click("settings-ssh-server-service-stop", servers.cx);
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

    assert!(rendered("settings-ssh-server-service-start", servers.cx));
    assert!(!rendered("settings-ssh-server-service-restart", servers.cx));
    assert!(!rendered("settings-ssh-server-service-stop", servers.cx));

    click("settings-ssh-server-service-start", servers.cx);

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

    click("settings-ssh-server-update", servers.cx);

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
    assert!(!rendered("settings-ssh-server-service-check", servers.cx));

    click("settings-ssh-server-maintenance-acknowledge", servers.cx);

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

    click("settings-ssh-server-service-again", servers.cx);

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
        "settings-ssh-server-maintenance-cancel",
        servers.cx
    ));
    assert!(!rendered("settings-ssh-server-update", servers.cx));

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

    click("settings-ssh-server-maintenance-discard", servers.cx);

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
        "settings-ssh-server-maintenance-discard",
        servers.cx
    ));

    click("settings-ssh-server-maintenance-discard", servers.cx);
    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(RemoteOperationScope::Saved)],
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
        vec![Call::Discard(RemoteOperationScope::Removed)],
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
        "settings-ssh-server-service-stop",
        "settings-ssh-server-service-restart",
        "settings-ssh-server-service-uninstall",
        "settings-ssh-server-service-check",
        "settings-ssh-server-update",
    ] {
        assert!(rendered(offered, servers.cx), "{offered} should be offered");
    }
    assert!(!rendered("settings-ssh-server-service-start", servers.cx));
    assert!(
        servers.operations.calls().is_empty(),
        "the observation needed no maintenance read, got {:?}",
        servers.operations.calls()
    );

    click("settings-ssh-server-service-stop", servers.cx);

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

    click("settings-ssh-server-maintenance-acknowledge", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Acknowledge(RemoteOperationHandle(4))]
    );
    assert!(
        rendered("settings-ssh-server-maintenance-acknowledge", servers.cx),
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

    click("settings-ssh-server-maintenance-discard", servers.cx);
    click_modal("settings-ssh-server-discard-confirm", servers.cx);

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Discard(RemoteOperationScope::Saved)]
    );
    assert!(
        rendered("settings-ssh-server-maintenance-discard", servers.cx),
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

    click(
        "settings-ssh-server-maintenance-dismiss-penguin",
        servers.cx,
    );

    assert_eq!(
        servers.operations.calls(),
        vec![Call::Dismiss(RemoteOperationScope::Removed)],
        "clearing the entry sends nothing and asks nothing"
    );
    assert!(
        !rendered("settings-row-removed-server-operations", servers.cx),
        "the cleared entry leaves nothing to say about a removed connection"
    );
}
