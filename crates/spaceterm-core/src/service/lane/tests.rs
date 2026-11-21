use super::*;
use crate::service::runtime::test_support::{COMMIT, VERSION, target, with_artifact};
use crate::service::{ServiceStartup, Supervisor};

use std::path::Path;
use std::sync::atomic::AtomicUsize;

/// A running server whose service manager only answers observations.
struct Running;
impl Supervisor for Running {
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtLogin
    }
    fn check(&self, _: &Path, _: bool) -> Result<(), CoreError> {
        Ok(())
    }
    fn install(&self, _: &Path) -> Result<(), CoreError> {
        panic!("the lane never installs")
    }
    fn prepare_start(&self) -> Result<(), CoreError> {
        panic!("the lane never starts the server")
    }
    fn start(&self) -> Result<(), CoreError> {
        panic!("the lane never starts the server")
    }
    fn stop(&self) -> Result<(), CoreError> {
        panic!("only the helper stops the server")
    }
    fn stop_timeout(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn set_automatic_start(&self, _: bool) -> Result<(), CoreError> {
        panic!("the lane never changes automatic start")
    }
    fn uninstall(&self, _: &Path) -> Result<(), CoreError> {
        panic!("the lane never uninstalls")
    }
    fn running(&self) -> Result<bool, CoreError> {
        Ok(true)
    }
}

#[derive(Default)]
struct Host {
    sessions: Mutex<Vec<Id>>,
    fenced: AtomicBool,
    idle_granted: AtomicBool,
    native_lease: Mutex<Option<crate::idle_restart::Lease>>,
    grant_target: Mutex<Option<(OperationId, Id, Id)>>,
    stopping: AtomicBool,
    /// Runs once inside the next fence release, before the fence clears.
    releasing: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Told each time a fence is set.
    fencing: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    counters: crate::diagnostics::TerminalCounters,
}
impl LaneHost for Host {
    fn fence(
        &self,
        admit: &mut dyn FnMut(&[Id]) -> Result<(), CoreError>,
    ) -> Result<(), CoreError> {
        let sessions = self.sessions.lock().unwrap();
        admit(&sessions)?;
        self.fenced.store(true, Ordering::SeqCst);
        if let Some(fencing) = self.fencing.lock().unwrap().as_ref() {
            let _ = fencing.send(());
        }
        Ok(())
    }
    fn grant_stop(
        &self,
        id: OperationId,
        candidate: Id,
        boot: Id,
        check: &mut dyn FnMut(bool) -> Result<(), CoreError>,
    ) -> Result<(), CoreError> {
        if *self.grant_target.lock().unwrap() != Some((id, candidate, boot)) {
            return Err(CoreError::Changed);
        }
        check(false)?;
        self.idle_granted.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn idle_granted(&self) -> bool {
        self.idle_granted.load(Ordering::SeqCst)
            || self
                .native_lease
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|lease| lease.granted())
    }
    fn thaw_idle_grant(&self) {
        self.native_lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.idle_granted.store(false, Ordering::SeqCst);
    }
    fn release_fence(&self) {
        self.thaw_idle_grant();
        let releasing = self.releasing.lock().unwrap().take();
        if let Some(releasing) = releasing {
            releasing();
        }
        self.fenced.store(false, Ordering::SeqCst);
    }
    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
    fn sessions(&self) -> Vec<Id> {
        self.sessions.lock().unwrap().clone()
    }
    fn terminal_counters(&self) -> crate::diagnostics::TerminalCounterSnapshot {
        self.counters.snapshot()
    }
    fn devices(&self) -> Result<Vec<LaneDevice>, CoreError> {
        Ok(Vec::new())
    }
    fn network(&self) -> Result<LaneNetwork, CoreError> {
        Ok(LaneNetwork {
            enabled: false,
            bind: None,
            listening: false,
            addresses: Vec::new(),
        })
    }
}

/// A lane over the server state at `paths`, whose service manager reports a running server and
/// whose helper launches do nothing.
pub(crate) fn serving(paths: &crate::host::Paths) -> Arc<LaneCore> {
    serving_with(paths, Arc::new(Host::default()))
}

/// [`serving`] with its own view of the running server. It fetches no feed, as Preflight and
/// Development do.
pub(crate) fn serving_with(paths: &crate::host::Paths, host: Arc<dyn LaneHost>) -> Arc<LaneCore> {
    let service = Service::new(paths.clone(), paths.identity.clone(), Arc::new(Running));
    LaneCore::new(
        service.clone(),
        Updates::open(service),
        host,
        None,
        Box::new(|_| Ok(())),
        Arc::new(crate::permissions::Unsupported),
    )
    .unwrap()
}

const ACTOR: Actor = Actor {
    pairing: Some([7; 32]),
    client: [8; 32],
};

/// A caller on the Machine itself, the only one that may Stop SpaceTerm Core.
const LOCAL: Actor = Actor {
    pairing: None,
    client: [8; 32],
};

type Hook = Arc<Mutex<Option<Box<dyn Fn(&OperationId) + Send>>>>;

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "the client flow tests use every field")
)]
struct Lane {
    lane: Arc<LaneCore>,
    service: Service,
    host: Arc<Host>,
    launches: Arc<AtomicUsize>,
    /// Plays the helper when an operation launches.
    hook: Hook,
    /// Makes every later helper launch fail, as a failed process or unit start does.
    unlaunchable: Arc<AtomicBool>,
}

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "the client flow tests restart the server")
)]
impl Lane {
    /// A new run of the same server, as a service manager start creates it.
    fn restarted(&self) -> Arc<LaneCore> {
        LaneCore::new(
            self.service.clone(),
            Updates::open(self.service.clone()),
            self.host.clone(),
            None,
            Box::new(|_| Ok(())),
            Arc::new(crate::permissions::Unsupported),
        )
        .unwrap()
    }
}

/// A running fixture server with one approved signed installation.
fn with_lane(run: impl FnOnce(Lane)) {
    with_lane_running(VERSION, run);
}

/// A running fixture server whose one approved signed installation is `version`.
fn with_lane_running(version: &str, run: impl FnOnce(Lane)) {
    let fixture = super::super::tests::Fixture::new();
    let protocols = crate::update::facts::Protocols::CURRENT;
    crate::service::runtime::test_support::with_build(version, protocols, || {
        with_artifact(
            &fixture.root.join("source"),
            &fixture.paths.identity,
            |archive, signature| {
                serve(&fixture, version, archive, signature, run);
            },
        );
    });
}

/// Runs `run` with fixture signing trust after it approves a signed installation at `paths`,
/// so a server started there opens its update engine and answers its service lane.
pub(crate) fn with_approved(paths: &crate::host::Paths, run: impl FnOnce()) {
    let source = paths.data.with_extension("source");
    with_artifact(&source, &paths.identity, |archive, signature| {
        approve(paths, VERSION, archive, signature);
        run();
    });
}

/// Provisions the signed `version` at `archive` as the approved installation at `paths`.
fn approve(
    paths: &crate::host::Paths,
    version: &str,
    archive: &std::path::Path,
    signature: &std::path::Path,
) -> Service {
    let service = Service::new(paths.clone(), paths.identity.clone(), Arc::new(Running));
    let approved = service
        .provision(archive, signature, target(), COMMIT, version)
        .unwrap();
    let installation = approved
        .directory
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .into();
    let state = super::super::State {
        installation,
        stopped: false,
        automatic_start: true,
        installed: true,
        removing: false,
        run: None,
        activation: None,
    };
    let data = Directory::open(&paths.data, false).unwrap();
    data.write_json(c"service.json", &state).unwrap();
    service
}

/// Serves the signed `version` at `archive` as the approved installation of `fixture`.
fn serve(
    fixture: &super::super::tests::Fixture,
    version: &str,
    archive: &std::path::Path,
    signature: &std::path::Path,
    run: impl FnOnce(Lane),
) {
    let service = approve(&fixture.paths, version, archive, signature);
    let host = Arc::new(Host::default());
    let launches = Arc::new(AtomicUsize::new(0));
    let counted = launches.clone();
    let hook: Hook = Arc::default();
    let helper = hook.clone();
    let unlaunchable = Arc::new(AtomicBool::new(false));
    let refused = unlaunchable.clone();
    let lane = LaneCore::new(
        service.clone(),
        Updates::open(service.clone()),
        host.clone(),
        None,
        Box::new(move |op| {
            counted.fetch_add(1, Ordering::SeqCst);
            if refused.load(Ordering::SeqCst) {
                return Err(CoreError::Protocol);
            }
            if let Some(hook) = helper.lock().unwrap().as_ref() {
                hook(op);
            }
            Ok(())
        }),
        Arc::new(crate::permissions::Unsupported),
    )
    .unwrap();
    run(Lane {
        lane,
        service,
        host,
        launches,
        hook,
        unlaunchable,
    });
}

fn code(reply: LaneReply) -> String {
    match reply {
        LaneReply::Error { code } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn preview(lane: &Arc<LaneCore>, action: LaneAction) -> (String, LaneEffects) {
    let actor = caller(&action);
    match lane.answer(LaneRequest::Preview { action }, actor) {
        LaneReply::Preview { token, effects, .. } => (token, effects),
        other => panic!("expected a preview, got {other:?}"),
    }
}

fn execute(lane: &Arc<LaneCore>, action: LaneAction, token: &str, op: u8) -> LaneReply {
    let actor = caller(&action);
    lane.answer(
        LaneRequest::Execute {
            action,
            token: token.to_owned(),
            operation: OperationId([op; 16]).to_string(),
        },
        actor,
    )
}

/// The caller these tests use for `action`: a paired device, except for a local-only Stop.
fn caller(action: &LaneAction) -> &'static Actor {
    if *action == LaneAction::Stop {
        &LOCAL
    } else {
        &ACTOR
    }
}

#[test]
fn a_paired_caller_cannot_stop_the_core() {
    with_lane(|lane| {
        let local_only = || LaneReply::Error {
            code: "local_only".into(),
        };
        let request = LaneRequest::Preview {
            action: LaneAction::Stop,
        };
        assert_eq!(lane.lane.answer(request, &ACTOR), local_only());
        let (token, _) = preview(&lane.lane, LaneAction::Stop);
        let request = LaneRequest::Execute {
            action: LaneAction::Stop,
            token,
            operation: OperationId([1; 16]).to_string(),
        };
        assert_eq!(lane.lane.answer(request, &ACTOR), local_only());
        assert!(!lane.host.fenced.load(Ordering::SeqCst));
        assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn lane_messages_keep_their_frozen_json_shape() {
    let request = LaneRequest::Execute {
        action: LaneAction::Restart,
        token: "c1-00".into(),
        operation: "op".into(),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "request": "execute",
            "action": { "action": "restart" },
            "token": "c1-00",
            "operation": "op",
        })
    );
    assert_eq!(
        serde_json::from_str::<LaneRequest>(r#"{"request":"status"}"#).unwrap(),
        LaneRequest::Status
    );
    assert!(
        serde_json::from_str::<LaneRequest>(r#"{"request":"resolve","operation":"op","x":1}"#)
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(LaneReply::Error {
            code: "unsupported".into()
        })
        .unwrap(),
        serde_json::json!({ "reply": "error", "code": "unsupported" })
    );
}

/// A lane whose System Permissions are `permissions`.
fn serving_permissions(
    paths: &crate::host::Paths,
    permissions: Arc<dyn crate::permissions::SystemPermissions>,
) -> Arc<LaneCore> {
    let service = Service::new(paths.clone(), paths.identity.clone(), Arc::new(Running));
    LaneCore::new(
        service.clone(),
        Updates::open(service),
        Arc::new(Host::default()),
        None,
        Box::new(|_| Ok(())),
        permissions,
    )
    .unwrap()
}

const LOCAL_ACTOR: Actor = Actor {
    pairing: None,
    client: [8; 32],
};

const SETUP: &str = "0123456789abcdef0123456789abcdef";

fn permission_requests() -> [LaneRequest; 5] {
    use crate::permissions::SystemPermission;
    [
        LaneRequest::Permissions,
        LaneRequest::RequestPermission {
            permission: SystemPermission::Microphone,
        },
        LaneRequest::PreparePermissionSetup {
            permission: SystemPermission::Accessibility,
            setup: SETUP.into(),
        },
        LaneRequest::ResetPermission {
            permission: SystemPermission::Accessibility,
        },
        LaneRequest::CancelPermissionSetup {
            setup: SETUP.into(),
        },
    ]
}

#[test]
fn a_paired_caller_reaches_no_system_permission() {
    use crate::permissions::{PermissionState, testing::ScriptedPermissions};
    let fixture = super::super::tests::Fixture::new();
    let permissions = ScriptedPermissions::granting(PermissionState::Granted);
    let lane = serving_permissions(&fixture.paths, permissions.clone());
    for request in permission_requests() {
        assert_eq!(
            lane.answer(request, &ACTOR),
            LaneReply::Error {
                code: "local_only".into()
            }
        );
    }
    assert!(
        permissions.calls.lock().unwrap().is_empty(),
        "the system is asked nothing for a paired caller"
    );
}

#[test]
fn a_local_caller_reads_and_changes_the_machines_own_permissions() {
    use crate::permissions::{
        PermissionState, RequestOutcome, SetupReadiness, SystemPermission,
        testing::ScriptedPermissions,
    };
    let fixture = super::super::tests::Fixture::new();
    let permissions = ScriptedPermissions::granting(PermissionState::NotGranted);
    let lane = serving_permissions(&fixture.paths, permissions.clone());
    let [read, request, prepare, reset, _] = permission_requests();
    let LaneReply::Permissions(report) = lane.answer(read, &LOCAL_ACTOR) else {
        panic!("expected a report");
    };
    assert_eq!(
        report.state(SystemPermission::Microphone),
        Some(PermissionState::NotGranted)
    );
    assert_eq!(
        lane.answer(request, &LOCAL_ACTOR),
        LaneReply::PermissionRequested(RequestOutcome::Requested)
    );
    assert_eq!(
        lane.answer(prepare, &LOCAL_ACTOR),
        LaneReply::PermissionSetup(SetupReadiness::Ready { cleared: true })
    );
    assert_eq!(lane.answer(reset, &LOCAL_ACTOR), LaneReply::PermissionReset);
    assert_eq!(
        *permissions.calls.lock().unwrap(),
        [
            ("request", SystemPermission::Microphone),
            ("prepare_setup", SystemPermission::Accessibility),
            ("reset", SystemPermission::Accessibility),
        ]
    );
}

#[test]
fn a_platform_without_system_permissions_answers_unsupported() {
    let fixture = super::super::tests::Fixture::new();
    let lane = serving_permissions(&fixture.paths, Arc::new(crate::permissions::Unsupported));
    let [read, request, prepare, reset, _] = permission_requests();
    for request in [read, request, prepare, reset] {
        assert_eq!(
            lane.answer(request, &LOCAL_ACTOR),
            LaneReply::Error {
                code: "unsupported".into()
            }
        );
    }
}

/// A preparation that waits for its test to release it before it reads the cancellation, as one
/// waiting for another setup's probe does.
struct HeldPreparation {
    started: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl crate::permissions::SystemPermissions for HeldPreparation {
    fn read(
        &self,
    ) -> Result<crate::permissions::PermissionsReport, crate::permissions::PermissionError> {
        Err(crate::permissions::PermissionError::Unsupported)
    }
    fn request(
        &self,
        _: crate::permissions::SystemPermission,
    ) -> Result<crate::permissions::RequestOutcome, crate::permissions::PermissionError> {
        Err(crate::permissions::PermissionError::Unsupported)
    }
    fn prepare_setup(
        &self,
        _: crate::permissions::SystemPermission,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<crate::permissions::SetupReadiness, crate::permissions::PermissionError> {
        self.started.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        Ok(if cancelled() {
            crate::permissions::SetupReadiness::Cancelled { cleared: false }
        } else {
            crate::permissions::SetupReadiness::Ready { cleared: false }
        })
    }
    fn reset(
        &self,
        _: crate::permissions::SystemPermission,
    ) -> Result<(), crate::permissions::PermissionError> {
        Err(crate::permissions::PermissionError::Unsupported)
    }
}

/// A cancellation sent over another connection while a preparation waits reaches that
/// preparation, and the server then forgets it.
#[test]
fn a_cancellation_reaches_a_waiting_preparation() {
    use crate::permissions::SetupReadiness;
    let fixture = super::super::tests::Fixture::new();
    let (started, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let lane = serving_permissions(
        &fixture.paths,
        Arc::new(HeldPreparation {
            started,
            release: Mutex::new(released),
        }),
    );
    let [_, _, prepare, _, cancel] = permission_requests();
    let preparing = std::thread::spawn({
        let lane = Arc::clone(&lane);
        let prepare = prepare.clone();
        move || lane.answer(prepare, &LOCAL_ACTOR)
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        lane.answer(cancel, &LOCAL_ACTOR),
        LaneReply::PermissionSetupCancelled
    );
    release.send(()).unwrap();
    assert_eq!(
        preparing.join().unwrap(),
        LaneReply::PermissionSetup(SetupReadiness::Cancelled { cleared: false })
    );

    // The same name prepares again once its cancellation was spent.
    let preparing = std::thread::spawn({
        let lane = Arc::clone(&lane);
        move || lane.answer(prepare, &LOCAL_ACTOR)
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    assert_eq!(
        preparing.join().unwrap(),
        LaneReply::PermissionSetup(SetupReadiness::Ready { cleared: false })
    );
}

/// Setup A is cancelled while it waits for the probing lock, then more cancellations arrive than
/// the lane once remembered. A still takes no effect once it runs.
#[test]
fn later_cancellations_never_revive_a_waiting_setup() {
    use crate::permissions::SetupReadiness;
    let fixture = super::super::tests::Fixture::new();
    let (started, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let lane = serving_permissions(
        &fixture.paths,
        Arc::new(HeldPreparation {
            started,
            release: Mutex::new(released),
        }),
    );
    let [_, _, prepare, _, cancel] = permission_requests();
    let preparing = std::thread::spawn({
        let lane = Arc::clone(&lane);
        move || lane.answer(prepare, &LOCAL_ACTOR)
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        lane.answer(cancel, &LOCAL_ACTOR),
        LaneReply::PermissionSetupCancelled
    );
    let bound = super::setups::MAX_EARLY_CANCELLATIONS;
    for index in 0..(bound + 16) {
        assert_eq!(
            lane.answer(
                LaneRequest::CancelPermissionSetup {
                    setup: format!("{index:032x}"),
                },
                &LOCAL_ACTOR,
            ),
            if index < bound {
                LaneReply::PermissionSetupCancelled
            } else {
                LaneReply::Error {
                    code: "service_busy".into(),
                }
            }
        );
    }
    release.send(()).unwrap();
    assert_eq!(
        preparing.join().unwrap(),
        LaneReply::PermissionSetup(SetupReadiness::Cancelled { cleared: false })
    );
}

#[test]
fn a_malformed_setup_name_is_refused() {
    use crate::permissions::{PermissionState, SystemPermission, testing::ScriptedPermissions};
    let fixture = super::super::tests::Fixture::new();
    let permissions = ScriptedPermissions::granting(PermissionState::NotGranted);
    let lane = serving_permissions(&fixture.paths, permissions.clone());
    for request in [
        LaneRequest::PreparePermissionSetup {
            permission: SystemPermission::Accessibility,
            setup: "../setup".into(),
        },
        LaneRequest::CancelPermissionSetup {
            setup: "A".repeat(32),
        },
    ] {
        assert_eq!(
            lane.answer(request, &LOCAL_ACTOR),
            LaneReply::Error {
                code: "protocol_error".into()
            }
        );
    }
    assert!(permissions.calls.lock().unwrap().is_empty());
}

#[test]
fn permission_messages_keep_their_json_shape() {
    use crate::permissions::{RequestOutcome, SystemPermission};
    assert_eq!(
        serde_json::to_value(LaneRequest::RequestPermission {
            permission: SystemPermission::ScreenRecording
        })
        .unwrap(),
        serde_json::json!({ "request": "request_permission", "permission": "screen_recording" })
    );
    assert_eq!(
        serde_json::to_value(LaneReply::PermissionRequested(RequestOutcome::Decided {
            state: crate::permissions::PermissionState::NotRequested
        }))
        .unwrap(),
        serde_json::json!({
            "reply": "permission_requested",
            "outcome": "decided",
            "state": "not_requested",
        })
    );
}

#[test]
fn status_reports_the_approved_runtime_and_live_sessions() {
    with_lane(|lane| {
        lane.host.sessions.lock().unwrap().push([3; 32]);
        lane.host.counters.record_frame_decoded();
        lane.host.counters.record_line_mode_read();
        lane.host.counters.record_line_mode_read();
        let LaneReply::Status(status) = lane.lane.answer(LaneRequest::Status, &ACTOR) else {
            panic!("expected a status");
        };
        assert!(status.running && status.installed && !status.stopped);
        assert_eq!(status.runtime.version, VERSION);
        assert_eq!(status.runtime.commit, COMMIT);
        assert_eq!(status.sessions, 1);
        assert_eq!(status.activation, LaneActivation::Settled);
        assert_eq!(status.lane_major, crate::protocol::version::SERVICE.major);
        assert_eq!(
            status.terminal_counters,
            Some(crate::diagnostics::TerminalCounterSnapshot {
                frames_decoded: 1,
                line_mode_reads: 2,
                ..Default::default()
            })
        );
    });
}

#[test]
fn a_confirmed_stop_is_admitted_once_behind_the_fence() {
    with_lane(|lane| {
        lane.host.sessions.lock().unwrap().push([3; 32]);
        let (token, effects) = preview(&lane.lane, LaneAction::Stop);
        assert_eq!(effects.ends_sessions, 1);
        assert_eq!(effects.sessions, vec!["03".repeat(32)]);
        assert!(effects.start_requires_machine);
        let accepted = execute(&lane.lane, LaneAction::Stop, &token, 1);
        let LaneReply::Accepted { boot, .. } = &accepted else {
            panic!("expected acceptance, got {accepted:?}");
        };
        assert_eq!(boot, &hex(&lane.lane.boot));
        assert!(lane.host.fenced.load(Ordering::SeqCst));
        assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        // A lost reply: the same operation returns its admission and launches nothing.
        assert_eq!(execute(&lane.lane, LaneAction::Stop, &token, 1), accepted);
        assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        let LaneReply::Receipt(receipt) = lane.lane.answer(
            LaneRequest::Resolve {
                operation: OperationId([1; 16]).to_string(),
            },
            &ACTOR,
        ) else {
            panic!("expected a receipt");
        };
        assert_eq!(receipt.state, LaneOperationState::Launched);
        assert_eq!(receipt.action, LaneAction::Stop);
        // One unsettled operation admits no other.
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        assert_eq!(
            code(execute(&lane.lane, LaneAction::Restart, &token, 2)),
            "service_busy"
        );
    });
}

#[test]
fn a_paired_device_removes_the_machine_with_one_admitted_uninstall() {
    with_lane(|lane| {
        lane.host.sessions.lock().unwrap().push([3; 32]);
        let (token, effects) = preview(&lane.lane, LaneAction::Uninstall);
        assert_eq!(effects.ends_sessions, 1);
        assert!(!effects.start_requires_machine);
        assert!(matches!(
            execute(&lane.lane, LaneAction::Uninstall, &token, 1),
            LaneReply::Accepted { .. }
        ));
        assert!(lane.host.fenced.load(Ordering::SeqCst));
        assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        let LaneReply::Receipt(receipt) = lane.lane.answer(
            LaneRequest::Resolve {
                operation: OperationId([1; 16]).to_string(),
            },
            &ACTOR,
        ) else {
            panic!("expected a receipt");
        };
        assert_eq!(receipt.action, LaneAction::Uninstall);
        assert_eq!(
            serde_json::to_value(LaneRequest::Preview {
                action: LaneAction::Uninstall
            })
            .unwrap(),
            serde_json::json!({"request": "preview", "action": {"action": "uninstall"}})
        );
    });
}

#[test]
fn a_core_without_the_uninstall_capability_refuses_its_preview_and_execution() {
    with_lane(|mut lane| {
        let (token, _) = preview(&lane.lane, LaneAction::Uninstall);
        Arc::get_mut(&mut lane.lane).unwrap().capabilities =
            crate::protocol::capability::Capabilities::fake(&[
                crate::protocol::capability::Capability::Restart,
            ]);
        let refused = lane.lane.answer(
            LaneRequest::Preview {
                action: LaneAction::Uninstall,
            },
            &ACTOR,
        );
        assert_eq!(code(refused), "unsupported");
        assert_eq!(
            code(execute(&lane.lane, LaneAction::Uninstall, &token, 1)),
            "unsupported"
        );
        assert!(!lane.host.fenced.load(Ordering::SeqCst));
        assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
        // Another capability the Core still has is unaffected.
        preview(&lane.lane, LaneAction::Restart);
    });
}

#[test]
fn a_spent_foreign_or_changed_observation_token_is_refused_as_changed() {
    with_lane(|lane| {
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        assert_eq!(
            code(execute(&lane.lane, LaneAction::Stop, &token, 1)),
            "changed"
        );
        assert_eq!(
            code(execute(&lane.lane, LaneAction::Restart, "c1-00", 1)),
            "changed"
        );
        // A Session that started after the preview changes what the operation ends.
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        lane.host.sessions.lock().unwrap().push([4; 32]);
        assert_eq!(
            code(execute(&lane.lane, LaneAction::Restart, &token, 1)),
            "changed"
        );
        assert!(!lane.host.fenced.load(Ordering::SeqCst));
        assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
        // Another client cannot spend this client's token.
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        let stranger = Actor {
            pairing: None,
            client: [9; 32],
        };
        let reply = lane.lane.answer(
            LaneRequest::Execute {
                action: LaneAction::Restart,
                token,
                operation: OperationId([1; 16]).to_string(),
            },
            &stranger,
        );
        assert_eq!(code(reply), "changed");
    });
}

#[test]
fn a_settled_outcome_is_resolved_then_acknowledged_by_its_own_actor() {
    with_lane(|lane| {
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        execute(&lane.lane, LaneAction::Restart, &token, 1);
        let id = OperationId([1; 16]);
        lane.lane.ledger().unwrap().claim(&id).unwrap();
        lane.lane.ledger().unwrap().mark(&id, State::Done).unwrap();
        let resolve = || {
            lane.lane.answer(
                LaneRequest::Resolve {
                    operation: id.to_string(),
                },
                &ACTOR,
            )
        };
        let LaneReply::Receipt(receipt) = resolve() else {
            panic!("expected a receipt");
        };
        assert_eq!(receipt.state, LaneOperationState::Done);
        let stranger = Actor {
            pairing: None,
            client: [9; 32],
        };
        let acknowledge = |actor: &Actor| {
            lane.lane.answer(
                LaneRequest::Acknowledge {
                    operation: id.to_string(),
                },
                actor,
            )
        };
        assert_eq!(code(acknowledge(&stranger)), "authentication_refused");
        assert_eq!(acknowledge(&ACTOR), LaneReply::Acknowledged);
        assert_eq!(code(resolve()), "not_found");
        // The fence watcher releases the fence once nothing is unsettled.
        let deadline = Instant::now() + Duration::from_secs(5);
        while lane.host.fenced.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the fence was never released");
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

#[test]
fn a_settling_fence_watcher_never_clears_the_fence_of_an_operation_admitted_meanwhile() {
    use std::sync::mpsc;
    with_lane(|lane| {
        // An earlier operation settled, and its watcher still owns the fence.
        lane.host.fenced.store(true, Ordering::SeqCst);
        lane.lane.watching.store(true, Ordering::SeqCst);
        let (entered, releasing) = mpsc::channel();
        let (resume, paused) = mpsc::channel::<()>();
        *lane.host.releasing.lock().unwrap() = Some(Box::new(move || {
            entered.send(()).unwrap();
            paused.recv().unwrap();
        }));
        let (fencing, fenced) = mpsc::channel();
        *lane.host.fencing.lock().unwrap() = Some(fencing);
        let (token, _) = preview(&lane.lane, LaneAction::Stop);
        let watcher = {
            let lane = Arc::clone(&lane.lane);
            std::thread::spawn(move || lane.release_if_settled())
        };
        // The watcher found every record settled and is clearing the fence.
        releasing.recv().unwrap();
        let executing = {
            let lane = Arc::clone(&lane.lane);
            std::thread::spawn(move || execute(&lane, LaneAction::Stop, &token, 1))
        };
        // The new Execute admits nothing and sets no fence until the release ends.
        assert_eq!(
            fenced.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        resume.send(()).unwrap();
        assert!(watcher.join().unwrap());
        let accepted = executing.join().unwrap();
        assert!(
            matches!(accepted, LaneReply::Accepted { .. }),
            "{accepted:?}"
        );
        fenced.recv().unwrap();
        // The new operation keeps its fence and has a watcher of its own.
        assert!(lane.host.fenced.load(Ordering::SeqCst));
        assert!(lane.lane.watching.load(Ordering::SeqCst));
        assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn revoking_the_actor_mid_operation_keeps_the_fence_and_refuses_another_operation() {
    with_lane(|lane| {
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        execute(&lane.lane, LaneAction::Restart, &token, 1);
        let id = OperationId([1; 16]);
        // The helper holds its lock and runs the Restart.
        let helper = Directory::open(&lane.service.paths.data, false)
            .unwrap()
            .try_lock(HELPER_LOCK)
            .unwrap()
            .unwrap();
        lane.lane.ledger().unwrap().claim(&id).unwrap();
        lane.lane
            .ledger()
            .unwrap()
            .forget_pairing(&ACTOR.pairing.unwrap())
            .unwrap();
        // The fence watcher sees an unsettled orphan, not an empty ledger.
        std::thread::sleep(FENCE_INTERVAL * 3);
        assert!(lane.host.fenced.load(Ordering::SeqCst));
        let other = Actor {
            pairing: Some([9; 32]),
            client: [9; 32],
        };
        let LaneReply::Preview { token, .. } = lane.lane.answer(
            LaneRequest::Preview {
                action: LaneAction::Restart,
            },
            &other,
        ) else {
            panic!("expected a preview");
        };
        let reply = lane.lane.answer(
            LaneRequest::Execute {
                action: LaneAction::Restart,
                token,
                operation: OperationId([2; 16]).to_string(),
            },
            &other,
        );
        assert_eq!(code(reply), "service_busy");
        assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        // The helper's outcome deletes the orphan, and only then does the fence clear.
        lane.lane.ledger().unwrap().mark(&id, State::Done).unwrap();
        drop(helper);
        assert_eq!(lane.lane.ledger().unwrap().resolve(&id).unwrap(), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while lane.host.fenced.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the fence was never released");
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

#[test]
fn a_full_ledger_refuses_execute_with_capacity() {
    with_lane(|lane| {
        let mut ledger = lane.lane.ledger().unwrap();
        for id in 0..crate::service::operations::MAX_RECORDS as u8 {
            let record = Record {
                id: OperationId([id + 100; 16]),
                actor: ACTOR,
                core: lane.lane.core,
                boot: lane.lane.boot,
                action: Action::Stop,
                observation_digest: [0; 32],
                candidate: None,
                state: State::Admitted,
                updated_at: 0,
                orphaned: false,
            };
            ledger.admit(record).unwrap();
            ledger
                .mark(&OperationId([id + 100; 16]), State::Unresolved)
                .unwrap();
        }
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        assert_eq!(
            code(execute(&lane.lane, LaneAction::Restart, &token, 1)),
            "capacity"
        );
    });
}

#[test]
fn a_core_without_a_feed_refuses_to_check_or_install_from_one() {
    with_lane(|lane| {
        assert_eq!(
            code(lane.lane.answer(LaneRequest::Check, &ACTOR)),
            "no_update_feed"
        );
        assert_eq!(
            code(lane.lane.answer(LaneRequest::StageFetch, &ACTOR)),
            "no_update_feed"
        );
    });
}

/// A release host that cannot be reached.
struct Unreachable;
impl crate::update::fetch::FeedSource for Unreachable {
    fn feed(
        &self,
        _: crate::update::feed::FeedTarget,
        _: Option<&crate::update::facts::Version>,
    ) -> Result<crate::update::fetch::Signed, crate::update::fetch::FetchError> {
        Err(crate::update::fetch::FetchError::Network)
    }
    fn archive(
        &self,
        _: crate::update::feed::FeedTarget,
        _: &crate::update::facts::Version,
        _: u64,
        _: &mut dyn std::io::Write,
    ) -> Result<(), crate::update::fetch::FetchError> {
        panic!("no feed to download from")
    }
}

#[test]
fn a_feed_check_that_fails_answers_its_code_and_ends_its_operation() {
    use crate::update::facts::UpdateCheck;
    with_lane(|lane| {
        let served = LaneCore::new(
            lane.service.clone(),
            Updates::open(lane.service.clone()),
            lane.host.clone(),
            Some(FeedCheck::fixture(
                lane.service.paths.data.clone(),
                None,
                Arc::new(Unreachable),
            )),
            Box::new(|_| Ok(())),
            Arc::new(crate::permissions::Unsupported),
        )
        .unwrap();
        assert_eq!(
            code(served.answer(LaneRequest::Check, &ACTOR)),
            "feed_unreachable"
        );
        assert_eq!(
            code(served.answer(LaneRequest::StageFetch, &ACTOR)),
            "feed_unreachable"
        );
        let LaneReply::Facts { boot, facts } = served.answer(LaneRequest::Facts {}, &ACTOR) else {
            panic!("expected facts");
        };
        assert_eq!(boot, hex(&served.boot));
        // The fixture is a Development Core, whose facts always report its check as off.
        assert_eq!(facts.check, UpdateCheck::Disabled);
        assert_eq!(facts.operation, None, "the check ended");
        let LaneReply::Status(status) = served.answer(LaneRequest::Status, &ACTOR) else {
            panic!("expected a status");
        };
        assert_eq!(
            status.update.map(|facts| facts.running),
            Some(facts.running)
        );
    });
}

#[test]
fn a_push_of_the_running_version_is_refused_before_any_byte() {
    with_lane(|lane| {
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        assert_eq!(
            code(lane.lane.answer(
                LaneRequest::StageUpload {
                    version: VERSION.into(),
                    floor: None,
                    length: 10,
                    sha256: "ab".repeat(32),
                    signature: encode(&[3; 64]),
                },
                &ACTOR,
            )),
            "not_newer"
        );
    });
}

#[test]
fn a_push_whose_floor_is_above_the_running_version_is_refused_with_its_floor() {
    with_lane_running("1.0.0", |lane| {
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let push = |floor: &str| LaneRequest::StageUpload {
            version: "1.2.2".into(),
            floor: Some(floor.into()),
            length: 10,
            sha256: "ab".repeat(32),
            signature: encode(&[3; 64]),
        };
        let reply = lane.lane.answer(push("1.1.0"), &ACTOR);
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            serde_json::json!({ "reply": "below_floor", "floor": "1.1.0" })
        );
        assert!(matches!(
            lane.lane.answer(push("1.0.0"), &ACTOR),
            LaneReply::UploadProgress { .. }
        ));
        assert_eq!(
            code(lane.lane.answer(push("1.2.3"), &ACTOR)),
            "protocol_error",
            "a floor newer than its own release is malformed"
        );
    });
}

#[test]
fn a_malformed_push_is_refused_before_anything_is_received() {
    with_lane(|lane| {
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let sha256 = "ab".repeat(32);
        for request in [
            LaneRequest::StageUpload {
                version: "not a version".into(),
                floor: None,
                length: 10,
                sha256: sha256.clone(),
                signature: encode(&[3; 64]),
            },
            LaneRequest::StageUpload {
                version: VERSION.into(),
                floor: None,
                length: 10,
                sha256: sha256.clone(),
                signature: "not base64".into(),
            },
            LaneRequest::StageUpload {
                version: VERSION.into(),
                floor: None,
                length: 0,
                sha256: sha256.clone(),
                signature: encode(&[3; 64]),
            },
            LaneRequest::StageUpload {
                version: VERSION.into(),
                floor: None,
                length: 10,
                sha256: sha256.to_uppercase(),
                signature: encode(&[3; 64]),
            },
            LaneRequest::StageUpload {
                version: VERSION.into(),
                floor: None,
                length: 10,
                sha256,
                signature: encode(&[3; 63]),
            },
        ] {
            assert_eq!(code(lane.lane.answer(request, &ACTOR)), "protocol_error");
        }
        assert_eq!(
            code(lane.lane.answer(
                LaneRequest::UploadChunk {
                    upload: "00".repeat(16),
                    offset: 0,
                    data: encode(b"abc"),
                },
                &ACTOR,
            )),
            "changed",
            "a chunk of no push is refused"
        );
        assert_eq!(
            code(lane.lane.answer(
                LaneRequest::UploadChunk {
                    upload: "00".repeat(16),
                    offset: 0,
                    data: encode(&vec![0; MAX_CHUNK + 1]),
                },
                &ACTOR,
            )),
            "protocol_error"
        );
    });
}

/// Installs a signed Release `version` as the Pending Version of `lane`'s Core.
fn install_pending(lane: &Lane, version: &str) -> String {
    let source = lane.service.paths.data.join("../pending-source");
    let (archive, signature) = crate::service::runtime::test_support::with_build(
        version,
        crate::update::facts::Protocols::CURRENT,
        || {
            with_artifact(&source, &lane.service.identity, |archive, signature| {
                (archive.to_owned(), signature.to_owned())
            })
        },
    );
    let sha256 =
        hex(
            ring::digest::digest(&ring::digest::SHA256, &std::fs::read(&archive).unwrap()).as_ref(),
        );
    runtime::updates::Updates::open(lane.service.clone())
        .unwrap()
        .install(runtime::updates::Archive {
            archive: &archive,
            signature: &signature,
            sha256: &sha256,
            source: runtime::updates::Source::Local,
        })
        .unwrap();
    sha256
}

/// An activation takes the service lock before it stops the running Core, then waits for that
/// Core to stop. A preview that waited for the lock would hold the connection worker that the
/// Core's shutdown joins, so it refuses instead.
#[test]
fn a_running_core_refuses_a_restart_preview_while_an_activation_holds_the_service_lock() {
    with_lane(|lane| {
        install_pending(&lane, "1.0.0");
        let data = Directory::open(&lane.service.paths.data, false).unwrap();
        let (held, locked) = std::sync::mpsc::channel();
        let activation = std::thread::spawn(move || {
            let _operation = data.lock("service.lock", false).unwrap();
            held.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(3));
        });
        locked.recv().unwrap();
        let started = Instant::now();
        let reply = lane.lane.answer(
            LaneRequest::Preview {
                action: LaneAction::Restart,
            },
            &ACTOR,
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the preview waited for the service lock"
        );
        assert_eq!(
            reply,
            LaneReply::Error {
                code: "service_busy".into()
            }
        );
        activation.join().unwrap();
        let (_, effects) = preview(&lane.lane, LaneAction::Restart);
        assert_eq!(
            effects.to.as_ref().map(|to| to.version.as_str()),
            Some("1.0.0")
        );
    });
}

/// A Restart with no Pending Version names the version it restarts, so a confirmation never
/// names a running version from facts another client's update has made stale.
#[test]
fn a_same_version_restart_preview_names_the_running_version() {
    with_lane_running("0.9.0", |lane| {
        let (_, effects) = preview(&lane.lane, LaneAction::Restart);
        let from = effects.from.expect("the preview names the running version");
        assert_eq!(from.version, "0.9.0");
        assert_eq!(from.commit, COMMIT);
        assert_eq!(effects.to, None);
        assert_eq!(effects.to_major, None);
    });
}

#[test]
fn a_restart_moves_into_the_pending_version_when_one_waits() {
    with_lane(|lane| {
        let (_, effects) = preview(&lane.lane, LaneAction::Restart);
        assert_eq!(effects.to, None);
        let sha256 = install_pending(&lane, "1.0.0");
        lane.host.sessions.lock().unwrap().push([3; 32]);
        let (token, effects) = preview(&lane.lane, LaneAction::Restart);
        assert_eq!(
            effects.to.as_ref().map(|to| to.version.as_str()),
            Some("1.0.0")
        );
        assert_eq!(effects.ends_sessions, 1);
        let reply = execute(&lane.lane, LaneAction::Restart, &token, 1);
        assert!(matches!(reply, LaneReply::Accepted { .. }), "{reply:?}");
        let record = lane
            .lane
            .ledger()
            .unwrap()
            .resolve(&OperationId([1; 16]))
            .unwrap()
            .unwrap();
        assert_eq!(record.action, Action::Update);
        assert_eq!(record.candidate.map(|v| hex(&v)), Some(sha256));
    });
}

#[test]
fn a_restart_the_core_decides_on_waits_for_zero_sessions_and_leaves_no_record() {
    use runtime::updates::{Fence, RestartRefusal, Restarter};
    with_lane(|lane| {
        lane.host.sessions.lock().unwrap().push([3; 32]);
        assert_eq!(
            Arc::clone(&lane.lane)
                .restart(None, Fence::NoSessions)
                .err(),
            Some(RestartRefusal::SessionsOpen)
        );
        assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
        assert!(!lane.host.fenced.load(Ordering::SeqCst));
        lane.host.sessions.lock().unwrap().clear();
        let launch = Arc::clone(&lane.lane)
            .restart(None, Fence::NoSessions)
            .ok()
            .unwrap();
        // Admission launches nothing: the engine publishes the restart first.
        assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
        launch();
        assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        assert!(lane.host.fenced.load(Ordering::SeqCst));
        let records = lane.lane.ledger().unwrap().records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            (records[0].actor, records[0].action),
            (Actor::CORE, Action::Restart)
        );
        lane.lane
            .ledger()
            .unwrap()
            .mark(&records[0].id, State::Done)
            .unwrap();
        assert!(lane.lane.settled().unwrap());
        assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
    });
}

#[test]
fn a_service_lane_hello_reaches_the_lane_as_its_agent() {
    with_lane(|lane| {
        let (mut client, mut core) = lane_pair().unwrap();
        let identity = lane.lane.service.identity.clone();
        let machine_id = lane.lane.core;
        let agent: crate::protocol::AgentName = "agent-1".parse().unwrap();
        let derived = agent.client(&[5; 32]);
        let peer = std::thread::spawn(move || {
            let name: crate::pane::control::ClientName = "Build Mac CLI agent-1".into();
            let major = crate::protocol::admit_service_lane(
                &mut client,
                crate::protocol::Caller {
                    client: &[5; 32],
                    name: &name,
                    agent: Some(&agent),
                },
                &machine_id,
                &identity,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            client
                .send(
                    br#"{"request":"resolve","operation":"00000000000000000000000000000000"}"#,
                    deadline,
                )
                .unwrap();
            let reply: LaneReply =
                serde_json::from_slice(&client.receive(MAX_FRAME, deadline).unwrap()).unwrap();
            (major, reply)
        });
        // Admission hands the lane connection to the caller with its actor.
        let admitted = crate::protocol::admit_core(
            &mut core.0,
            &lane.lane.core,
            &lane.lane.service.identity,
            &crate::protocol::versions_for_tests(),
        )
        .unwrap();
        let crate::protocol::Admission::Service(actor) = admitted else {
            panic!("expected a service-lane connection");
        };
        assert_eq!(actor.actor.client, derived);
        lane.lane.serve(&mut core, actor, None, &|| Ok(())).unwrap();
        let (major, reply) = peer.join().unwrap();
        assert_eq!(major.selected, crate::protocol::version::SERVICE);
        assert_eq!(code(reply), "not_found");
        // The admitted actor is the agent's own identity, so its tokens are its own.
        let (token, _) = preview(&lane.lane, LaneAction::Stop);
        let agent_actor = Actor {
            pairing: None,
            client: derived,
        };
        let reply = lane.lane.answer(
            LaneRequest::Execute {
                action: LaneAction::Stop,
                token,
                operation: OperationId([1; 16]).to_string(),
            },
            &agent_actor,
        );
        assert_eq!(code(reply), "changed");
    });
}

#[test]
fn an_unsettled_operation_never_keeps_a_stopping_core_alive() {
    with_lane(|lane| {
        let (token, _) = preview(&lane.lane, LaneAction::Stop);
        // The helper never settles this record, as while it waits for the server to exit.
        assert!(matches!(
            execute(&lane.lane, LaneAction::Stop, &token, 1),
            LaneReply::Accepted { .. }
        ));
        let Lane {
            lane: core, host, ..
        } = lane;
        assert!(host.fenced.load(Ordering::SeqCst));
        // The server's shutdown drops its lane. The fence watcher must not hold the host.
        drop(core);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&host) > 1 {
            assert!(
                Instant::now() < deadline,
                "the fence watcher kept the lane's host alive"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

#[test]
fn a_stopping_machine_closes_a_polling_lane_connection() {
    with_lane(|lane| {
        let (mut client, mut core) = lane_pair().unwrap();
        let host = lane.host.clone();
        let peer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let resolve =
                br#"{"request":"resolve","operation":"00000000000000000000000000000000"}"#;
            client.send(resolve, deadline).unwrap();
            client.receive(MAX_FRAME, deadline).unwrap();
            // The server starts stopping while the client keeps polling.
            host.stopping.store(true, Ordering::SeqCst);
            let _ = client.send(resolve, deadline);
            client.receive(MAX_FRAME, deadline).is_err()
        });
        let started = Instant::now();
        lane.lane
            .serve(&mut core, ACTOR.into(), None, &|| Ok(()))
            .unwrap();
        drop(core);
        assert!(peer.join().unwrap(), "the client saw the connection close");
        assert!(started.elapsed() < Duration::from_secs(5));
    });
}

#[test]
fn the_lane_answers_until_the_peer_closes_and_refuses_malformed_requests() {
    with_lane(|lane| {
        let (mut client, mut core) = lane_pair().unwrap();
        // Fixture trust is per thread, so the lane serves on this one.
        let peer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut replies = Vec::new();
            for request in [
                &br#"{"request":"nonsense"}"#[..],
                br#"{"request":"status"}"#,
            ] {
                client.send(request, deadline).unwrap();
                replies.push(
                    serde_json::from_slice::<LaneReply>(
                        &client.receive(MAX_FRAME, deadline).unwrap(),
                    )
                    .unwrap(),
                );
            }
            replies
        });
        lane.lane
            .serve(&mut core, ACTOR.into(), None, &|| Ok(()))
            .unwrap();
        let mut replies = peer.join().unwrap().into_iter();
        assert_eq!(code(replies.next().unwrap()), "protocol_error");
        assert!(matches!(replies.next().unwrap(), LaneReply::Status(_)));
    });
}

#[cfg(feature = "client")]
mod flow {
    //! The client flow against a served lane whose server goes away and comes back.
    use super::*;
    use crate::service::lane::admin;
    use crate::service::lane::client::{self, Connector, LaneError, Leftover, Outcome, Session};

    /// Connects to whichever run of the server is up. `None` means no server runs.
    #[derive(Default)]
    struct Switch {
        lane: Mutex<Option<Arc<LaneCore>>>,
        open: Mutex<Vec<LaneStream>>,
    }
    impl Switch {
        fn up(&self, lane: Arc<LaneCore>) {
            *self.lane.lock().unwrap() = Some(lane);
        }
        /// The server exits: its lane connections close and no new one opens.
        fn down(&self) {
            *self.lane.lock().unwrap() = None;
            for stream in self.open.lock().unwrap().drain(..) {
                let _ = stream.0.shutdown(std::net::Shutdown::Both);
            }
        }
    }
    impl Connector for Switch {
        fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
            let lane = self
                .lane
                .lock()
                .unwrap()
                .clone()
                .ok_or(CoreError::NotRunning)?;
            let (client, mut core) = lane_pair().map_err(|_| CoreError::Protocol)?;
            self.open
                .lock()
                .unwrap()
                .push(crate::protocol::LocalPreamble(
                    core.0.try_clone().map_err(|_| CoreError::Protocol)?,
                ));
            // The fixture's signing trust is per thread, so the serving thread holds it too.
            std::thread::spawn(move || {
                crate::service::runtime::test_support::with_fixture_trust(|| {
                    lane.serve(&mut core, LOCAL.into(), None, &|| Ok(()))
                })
            });
            Ok((
                Box::new(client),
                crate::protocol::version::SERVICE,
                crate::protocol::version::SERVICE,
            ))
        }
    }

    /// The helper's lock, held from before its claim until it records an outcome.
    fn helper_lock(lane: &LaneCore) -> crate::storage::Lock {
        Directory::open(&lane.service.paths.data, false)
            .unwrap()
            .try_lock(HELPER_LOCK)
            .unwrap()
            .unwrap()
    }

    fn previewed(switch: &Switch, action: LaneAction) -> (Session, String) {
        let mut session = Session::open(switch, Instant::now() + Duration::from_secs(5)).unwrap();
        let LaneReply::Preview { token, .. } = session
            .request(
                &LaneRequest::Preview { action },
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap()
        else {
            panic!("expected a preview");
        };
        (session, token)
    }

    #[test]
    fn within_waits_only_until_its_deadline_once_one_is_set() {
        use crate::service::lane::client::within;
        let status = || Err::<(), _>(CoreError::Changed);
        // Quick work reports its own result, with a deadline or without.
        assert_eq!(
            within("test", &|| None, status),
            Some(Err(CoreError::Changed))
        );
        let until = Instant::now() + Duration::from_secs(5);
        assert!(within("test", &|| Some(until), status).is_some());
        // Stalled work is abandoned at the deadline.
        let started = Instant::now();
        let until = started + Duration::from_millis(300);
        let stalled = within("test", &|| Some(until), move || {
            std::thread::sleep(Duration::from_secs(5));
            status()
        });
        assert!(stalled.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_restart_settles_after_the_core_returns_with_a_new_boot() {
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let (helper_switch, ledger_lane) = (switch.clone(), lane.lane.clone());
            let returned = Arc::new(Mutex::new(None));
            let next = returned.clone();
            let service = lane.service.clone();
            let host = lane.host.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                let (switch, old, next, service, host) = (
                    helper_switch.clone(),
                    ledger_lane.clone(),
                    next.clone(),
                    service.clone(),
                    host.clone(),
                );
                let op = *op;
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    let helper = helper_lock(&old);
                    old.ledger().unwrap().claim(&op).unwrap();
                    switch.down();
                    std::thread::sleep(Duration::from_millis(1500));
                    drop(helper);
                    // The helper ends before the new server starts, so start-up settles it.
                    let new = LaneCore::new(
                        service.clone(),
                        Updates::open(service.clone()),
                        host,
                        None,
                        Box::new(|_| Ok(())),
                        Arc::new(crate::permissions::Unsupported),
                    )
                    .unwrap();
                    *next.lock().unwrap() = Some(hex(&new.boot));
                    switch.up(new);
                });
            }));
            let (session, token) = previewed(&switch, LaneAction::Restart);
            let mut admitted = Vec::new();
            let outcome = client::perform(
                &(switch.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Restart,
                token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::from_secs(20),
                &mut |op| {
                    admitted.push(*op);
                    Ok(())
                },
                &|| None,
            )
            .unwrap();
            let Outcome::Settled { receipt, boot } = outcome else {
                panic!("expected a settled restart, got {outcome:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::Done);
            assert_eq!(boot, hex(&lane.lane.boot));
            assert_eq!(Some(receipt.boot), returned.lock().unwrap().clone());
            assert_eq!(receipt.operation, admitted[0].to_string());
            // The flow acknowledged the receipt, so the ledger is empty again.
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
        });
    }

    fn uninstall(switch: &Arc<Switch>) -> Outcome {
        let (session, token) = previewed(switch, LaneAction::Uninstall);
        client::perform(
            &(switch.clone() as Arc<dyn Connector>),
            session,
            LaneAction::Uninstall,
            token,
            crate::service::operations::random_operation_id().unwrap(),
            Duration::from_secs(20),
            &mut |_| Ok(()),
            &|| None,
        )
        .unwrap()
    }

    /// Once its helper launched, an uninstalling Core stops answering, so Remove Machine's
    /// uninstall ends when the lane closes rather than at the flow's deadline.
    #[test]
    fn an_uninstall_ends_when_the_core_closes_the_lane_after_its_helper_launched() {
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let stopping = switch.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |_| {
                let stopping = stopping.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    stopping.down();
                });
            }));
            let started = Instant::now();
            let outcome = uninstall(&switch);
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "took {:?}",
                started.elapsed()
            );
            let Outcome::Shutdown { boot, .. } = outcome else {
                panic!("expected a started uninstall, got {outcome:?}");
            };
            assert_eq!(boot, hex(&lane.lane.boot));
        });
    }

    /// A Core whose uninstall helper fails to launch stays installed and paired, so the flow
    /// returns that failure instead of ending at admission.
    #[test]
    fn an_uninstall_whose_helper_fails_to_launch_returns_its_failure() {
        with_lane(|lane| {
            lane.unlaunchable.store(true, Ordering::SeqCst);
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let outcome = uninstall(&switch);
            let Outcome::Settled { receipt, boot } = outcome else {
                panic!("expected the failed launch, got {outcome:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::NotPerformed);
            assert_eq!(boot, hex(&lane.lane.boot));
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        });
    }

    /// A lane that closes before any receipt showed the helper launched proves nothing, so the
    /// uninstall stays unconfirmed and its operation stays to settle later.
    #[test]
    fn an_uninstall_whose_lane_closes_at_admission_stays_unconfirmed() {
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let closing = switch.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |_| closing.down()));
            let outcome = uninstall(&switch);
            let Outcome::Unconfirmed { boot, last, .. } = outcome else {
                panic!("expected an unconfirmed uninstall, got {outcome:?}");
            };
            assert_eq!(boot, hex(&lane.lane.boot));
            assert_eq!(last, None);
        });
    }

    /// Another client's admitted Restart, which holds SpaceTerm Core until `settle` runs.
    fn another_clients_restart(lane: &Lane) -> impl FnOnce() + Send + 'static {
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        assert!(matches!(
            execute(&lane.lane, LaneAction::Restart, &token, 1),
            LaneReply::Accepted { .. }
        ));
        let core = lane.lane.clone();
        move || {
            let id = OperationId([1; 16]);
            let _helper = helper_lock(&core);
            let mut ledger = core.ledger().unwrap();
            ledger.claim(&id).unwrap();
            ledger.mark(&id, State::Done).unwrap();
        }
    }

    /// The person's approved Stop, previewed before another client's Restart admitted.
    fn approved_stop(lane: &Lane) -> (client::Lane, Session, admin::Preview) {
        let switch = Arc::new(Switch::default());
        switch.up(lane.lane.clone());
        let client = client::Lane::over(switch);
        let mut session = client.session().unwrap();
        let approved = admin::preview(&mut session, LaneAction::Stop).unwrap();
        (client, session, approved)
    }

    fn pending(lane: &Lane) -> admin::PendingOperations {
        admin::pending_operations(
            Directory::open(&lane.service.paths.data.with_extension("pending"), true).unwrap(),
        )
    }

    #[test]
    fn an_approved_stop_waits_for_another_operation_then_runs_once() {
        with_lane(|lane| {
            lane.host.sessions.lock().unwrap().push([3; 32]);
            let (client, session, approved) = approved_stop(&lane);
            let settle = another_clients_restart(&lane);
            let settled = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                settle();
            });
            let mut said = Vec::new();
            let outcome = admin::execute_approved(
                &client,
                session,
                LaneAction::Stop,
                approved.token,
                Instant::now() + admin::BUSY_WAIT,
                Duration::from_secs(2),
                &pending(&lane),
                &mut admin::Unshared,
                &mut || Ok(()),
                &|| None,
                &mut |began| said.push((began, Instant::now())),
            )
            .unwrap();
            let returned = Instant::now();
            settled.join().unwrap();
            // No helper runs here, so the admitted Stop is followed until the wait ends.
            assert!(
                matches!(outcome, Outcome::Unconfirmed { .. }),
                "{outcome:?}"
            );
            let said_began: Vec<_> = said.iter().map(|(began, _)| *began).collect();
            assert_eq!(
                said_began,
                [true, false],
                "the wait is said once and ends once"
            );
            // The wait ends when the Core admits the Stop, not when its follow ends.
            assert!(returned - said[1].1 >= Duration::from_millis(900));
            assert_eq!(
                lane.launches.load(Ordering::SeqCst),
                2,
                "the Restart and then the Stop each launched once"
            );
        });
    }

    #[test]
    fn an_approved_stop_is_refused_as_changed_when_its_sessions_change_while_it_waits() {
        with_lane(|lane| {
            lane.host.sessions.lock().unwrap().push([3; 32]);
            let (client, session, approved) = approved_stop(&lane);
            let settle = another_clients_restart(&lane);
            let host = lane.host.clone();
            let settled = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                // A new shell starts once the other operation ends.
                host.sessions.lock().unwrap().push([4; 32]);
                settle();
            });
            let outcome = admin::execute_approved(
                &client,
                session,
                LaneAction::Stop,
                approved.token,
                Instant::now() + admin::BUSY_WAIT,
                Duration::from_secs(2),
                &pending(&lane),
                &mut admin::Unshared,
                &mut || Ok(()),
                &|| None,
                &mut |_| {},
            );
            settled.join().unwrap();
            assert_eq!(outcome, Err(LaneError::Code("changed".into())));
            assert_eq!(
                lane.launches.load(Ordering::SeqCst),
                1,
                "the changed Stop dispatched nothing"
            );
        });
    }

    /// The caller's deadline bounds the whole wait, as `--timeout` does for the CLI, and a
    /// refusal that outlasts it admits nothing.
    #[test]
    fn an_approval_waits_no_longer_than_its_deadline() {
        with_lane(|lane| {
            let (client, session, approved) = approved_stop(&lane);
            let _settle = another_clients_restart(&lane);
            let began = Instant::now();
            let outcome = admin::execute_approved(
                &client,
                session,
                LaneAction::Stop,
                approved.token,
                began + Duration::from_millis(2500),
                Duration::from_secs(90),
                &pending(&lane),
                &mut admin::Unshared,
                &mut || Ok(()),
                &|| None,
                &mut |_| {},
            );
            assert_eq!(outcome, Err(LaneError::Code("service_busy".into())));
            assert!(
                began.elapsed() < Duration::from_secs(4),
                "{:?}",
                began.elapsed()
            );
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn an_interrupted_approval_stops_waiting_and_reports_busy() {
        with_lane(|lane| {
            let (client, session, approved) = approved_stop(&lane);
            let _settle = another_clients_restart(&lane);
            // The person interrupts once the wait began.
            let interrupted = std::cell::Cell::new(None);
            let outcome = admin::execute_approved(
                &client,
                session,
                LaneAction::Stop,
                approved.token,
                Instant::now() + admin::BUSY_WAIT,
                Duration::from_secs(2),
                &pending(&lane),
                &mut admin::Unshared,
                &mut || Ok(()),
                &|| interrupted.get(),
                &mut |began| {
                    if began {
                        interrupted.set(Some(Instant::now()));
                    }
                },
            );
            assert_eq!(outcome, Err(LaneError::Code("service_busy".into())));
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn a_stop_reports_shutdown_and_a_later_connection_settles_it() {
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let (helper_switch, ledger_lane) = (switch.clone(), lane.lane.clone());
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                let (switch, old, op) = (helper_switch.clone(), ledger_lane.clone(), *op);
                std::thread::spawn(move || {
                    let _helper = helper_lock(&old);
                    let mut ledger = old.ledger().unwrap();
                    ledger.claim(&op).unwrap();
                    ledger.mark(&op, State::Done).unwrap();
                    switch.down();
                });
            }));
            let (session, token) = previewed(&switch, LaneAction::Stop);
            let mut admitted = None;
            let outcome = client::perform(
                &(switch.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Stop,
                token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::from_secs(20),
                &mut |op| {
                    admitted = Some(*op);
                    Ok(())
                },
                &|| None,
            )
            .unwrap();
            assert!(matches!(outcome, Outcome::Shutdown { .. }), "{outcome:?}");
            // A local Start brings the server back, and Check Status settles it.
            switch.up(lane.restarted());
            let route = client::Lane::over(switch.clone());
            let pending = pending(&lane);
            let id = admitted.unwrap();
            let check = || admin::check(&route, &pending, &id, Instant::now() + REQUEST_LIMIT);
            let leftover = check().unwrap();
            let Leftover::Settled(receipt) = leftover.clone() else {
                panic!("expected a settled stop, got {leftover:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::Done);
            assert_eq!(lane.lane.ledger().unwrap().resolve(&id).unwrap(), None);
            // SpaceTerm Core forgot the acknowledged receipt; the kept outcome still answers.
            assert_eq!(check().unwrap(), leftover);
        });
    }

    #[test]
    fn a_helper_failure_settles_without_waiting_for_a_disconnect() {
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let ledger_lane = lane.lane.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                let (old, op) = (ledger_lane.clone(), *op);
                std::thread::spawn(move || {
                    let _helper = helper_lock(&old);
                    let mut ledger = old.ledger().unwrap();
                    ledger.claim(&op).unwrap();
                    ledger
                        .mark(
                            &op,
                            State::Failed {
                                code: "startup_failed".into(),
                            },
                        )
                        .unwrap();
                });
            }));
            let (session, token) = previewed(&switch, LaneAction::Restart);
            let started = Instant::now();
            let outcome = client::perform(
                &(switch.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Restart,
                token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::from_secs(20),
                &mut |_| Ok(()),
                &|| None,
            )
            .unwrap();
            let Outcome::Settled { receipt, .. } = outcome else {
                panic!("expected a settled failure, got {outcome:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::Failed);
            assert_eq!(receipt.code.as_deref(), Some("startup_failed"));
            assert!(started.elapsed() < Duration::from_secs(10));
        });
    }

    /// A server that, once `lose` is set, admits the next `Execute` and closes its connection
    /// before the reply leaves.
    #[derive(Default)]
    struct Lossy {
        lane: Mutex<Option<Arc<LaneCore>>>,
        lose: Arc<AtomicBool>,
        /// Drops the next `Execute` unread instead, as a request lost on its way.
        drop: Arc<AtomicBool>,
    }
    impl Connector for Lossy {
        fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
            let lane = self
                .lane
                .lock()
                .unwrap()
                .clone()
                .ok_or(CoreError::NotRunning)?;
            let (client, mut core) = lane_pair().map_err(|_| CoreError::Protocol)?;
            let lose = self.lose.clone();
            let drop = self.drop.clone();
            std::thread::spawn(move || {
                loop {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let Ok(bytes) = core.receive(MAX_FRAME, deadline) else {
                        return;
                    };
                    let request: LaneRequest = serde_json::from_slice(&bytes).unwrap();
                    let execute = matches!(request, LaneRequest::Execute { .. });
                    if execute && drop.swap(false, Ordering::SeqCst) {
                        let _ = core.0.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    let reply = lane.answer(request, &LOCAL);
                    if execute && lose.swap(false, Ordering::SeqCst) {
                        let _ = core.0.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    let _ = core.send(&serde_json::to_vec(&reply).unwrap(), deadline);
                }
            });
            Ok((
                Box::new(client),
                crate::protocol::version::SERVICE,
                crate::protocol::version::SERVICE,
            ))
        }
    }

    impl Lossy {
        fn up(lane: &Lane) -> Arc<Self> {
            let lossy = Arc::new(Self::default());
            *lossy.lane.lock().unwrap() = Some(lane.lane.clone());
            let ledger_lane = lane.lane.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                let (old, op) = (ledger_lane.clone(), *op);
                std::thread::spawn(move || {
                    let _helper = helper_lock(&old);
                    let mut ledger = old.ledger().unwrap();
                    ledger.claim(&op).unwrap();
                    ledger.mark(&op, State::Done).unwrap();
                });
            }));
            lossy
        }

        fn previewed(&self, action: LaneAction) -> (Session, String) {
            let mut session = Session::open(self, Instant::now() + Duration::from_secs(5)).unwrap();
            let LaneReply::Preview { token, .. } = session
                .request(
                    &LaneRequest::Preview { action },
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap()
            else {
                panic!("expected a preview");
            };
            self.lose.store(true, Ordering::SeqCst);
            (session, token)
        }
    }

    #[test]
    fn a_lost_accepted_reply_follows_the_same_operation_without_launching_it_twice() {
        with_lane(|lane| {
            let lossy = Lossy::up(&lane);
            let (session, token) = lossy.previewed(LaneAction::Restart);
            let mut admitted = None;
            let outcome = client::perform(
                &(lossy.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Restart,
                token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::from_secs(20),
                &mut |op| {
                    admitted = Some(*op);
                    Ok(())
                },
                &|| None,
            )
            .unwrap();
            let Outcome::Settled { receipt, .. } = outcome else {
                panic!("expected a settled restart, got {outcome:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::Done);
            assert_eq!(receipt.operation, admitted.unwrap().to_string());
            // The repeated Execute found the admitted record and dispatched nothing.
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        });
    }

    /// The first `Execute` never reached the Core, and another operation holds it when the flow
    /// asks again. Busy then proves nothing about the first request, so the flow keeps the same
    /// operation and admits it, once, after the other operation settles.
    #[test]
    fn a_busy_replay_after_a_lost_execute_keeps_the_same_operation() {
        with_lane(|lane| {
            lane.host.sessions.lock().unwrap().push([3; 32]);
            let lossy = Lossy::up(&lane);
            let (session, token) = lossy.previewed(LaneAction::Stop);
            lossy.lose.store(false, Ordering::SeqCst);
            lossy.drop.store(true, Ordering::SeqCst);
            // The other client's Restart holds the Core until `settle`; only this Stop's helper
            // settles at once.
            let hook = lane.hook.lock().unwrap().take();
            let settle = another_clients_restart(&lane);
            *lane.hook.lock().unwrap() = hook;
            let settled = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(2500));
                settle();
            });
            let id = crate::service::operations::random_operation_id().unwrap();
            let mut admitted = Vec::new();
            let outcome = client::perform(
                &(lossy.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Stop,
                token,
                id,
                Duration::from_secs(20),
                &mut |op| {
                    admitted.push(*op);
                    Ok(())
                },
                &|| None,
            );
            settled.join().unwrap();
            let operation = match outcome {
                Ok(
                    Outcome::Shutdown { operation, .. } | Outcome::Unconfirmed { operation, .. },
                ) => operation,
                Ok(Outcome::Settled { receipt, .. }) => receipt.operation,
                Err(error) => panic!("a lost Execute is never refused as busy: {error:?}"),
            };
            assert_eq!(operation, id.to_string());
            assert_eq!(admitted, [id]);
            assert_eq!(
                lane.launches.load(Ordering::SeqCst),
                2,
                "the other Restart and this Stop each launched once"
            );
        });
    }

    #[test]
    fn an_interrupted_flow_resolves_a_lost_accepted_reply_without_sending_execute_again() {
        with_lane(|lane| {
            let lossy = Lossy::up(&lane);
            let (session, token) = lossy.previewed(LaneAction::Restart);
            let started = Instant::now();
            let interrupted = started + Duration::from_secs(3);
            let outcome = client::perform(
                &(lossy.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Restart,
                token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::from_secs(60),
                &mut |_| Ok(()),
                &|| Some(interrupted),
            )
            .unwrap();
            let Outcome::Settled { receipt, .. } = outcome else {
                panic!("expected a settled restart, got {outcome:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::Done);
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
            assert!(started.elapsed() < Duration::from_secs(5));
        });
    }

    /// A server that admits `Execute` and then answers nothing more on any connection, while
    /// it keeps each connection open.
    #[derive(Default)]
    struct Stalling {
        lane: Mutex<Option<Arc<LaneCore>>>,
        stalled: Arc<AtomicBool>,
    }
    impl Connector for Stalling {
        fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
            let lane = self.lane.lock().unwrap().clone().unwrap();
            let (client, mut core) = lane_pair().map_err(|_| CoreError::Protocol)?;
            let stalled = self.stalled.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(60);
                while let Ok(bytes) = core.receive(MAX_FRAME, deadline) {
                    if stalled.load(Ordering::SeqCst) {
                        continue;
                    }
                    let request: LaneRequest = serde_json::from_slice(&bytes).unwrap();
                    let execute = matches!(request, LaneRequest::Execute { .. });
                    let reply = lane.answer(request, &LOCAL);
                    if execute {
                        stalled.store(true, Ordering::SeqCst);
                        continue;
                    }
                    let _ = core.send(&serde_json::to_vec(&reply).unwrap(), deadline);
                }
            });
            Ok((
                Box::new(client),
                crate::protocol::version::SERVICE,
                crate::protocol::version::SERVICE,
            ))
        }
    }

    /// A preview that a stalled Core never answers ends at the caller's deadline, not at the
    /// lane's own request limit.
    #[test]
    fn a_stalled_preview_ends_at_the_callers_deadline() {
        with_lane(|lane| {
            let stalling = Arc::new(Stalling::default());
            *stalling.lane.lock().unwrap() = Some(lane.lane.clone());
            stalling.stalled.store(true, Ordering::SeqCst);
            let client = client::Lane::over(stalling.clone());
            let session = client.session().unwrap();
            let began = Instant::now();
            let previewed = admin::preview_when_ready(
                &client,
                session,
                LaneAction::Restart,
                began + Duration::from_millis(1500),
                &|| None,
                &mut |_| {},
            );
            assert!(previewed.is_err());
            assert!(
                began.elapsed() < Duration::from_secs(4),
                "{:?}",
                began.elapsed()
            );
        });
    }

    #[test]
    fn an_interrupt_ends_a_request_a_stalled_core_holds_within_the_grace() {
        with_lane(|lane| {
            let stalling = Arc::new(Stalling::default());
            *stalling.lane.lock().unwrap() = Some(lane.lane.clone());
            let mut session =
                Session::open(stalling.as_ref(), Instant::now() + Duration::from_secs(5)).unwrap();
            let LaneReply::Preview { token, .. } = session
                .request(
                    &LaneRequest::Preview {
                        action: LaneAction::Restart,
                    },
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap()
            else {
                panic!("expected a preview");
            };
            let started = Instant::now();
            // SIGINT arrives 1 s into the stalled Execute, and the caller settles within 5 s.
            let at = started + Duration::from_secs(1);
            let outcome = client::perform(
                &(stalling.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Restart,
                token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::from_secs(60),
                &mut |_| Ok(()),
                &|| (Instant::now() >= at).then_some(at + Duration::from_secs(5)),
            )
            .unwrap();
            assert!(
                matches!(outcome, Outcome::Unconfirmed { .. }),
                "{outcome:?}"
            );
            let elapsed = started.elapsed();
            assert!(
                elapsed >= Duration::from_secs(6) && elapsed < Duration::from_secs(7),
                "{elapsed:?}"
            );
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn a_stop_whose_accepted_reply_was_lost_reports_shutdown_when_the_core_is_gone() {
        with_lane(|lane| {
            let lossy = Lossy::up(&lane);
            let (session, token) = lossy.previewed(LaneAction::Stop);
            // The server stops right after it admitted the Stop.
            let gone = lossy.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |_: &OperationId| {
                *gone.lane.lock().unwrap() = None;
            }));
            let outcome = client::perform(
                &(lossy.clone() as Arc<dyn Connector>),
                session,
                LaneAction::Stop,
                token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::from_secs(20),
                &mut |_| Ok(()),
                &|| None,
            )
            .unwrap();
            assert!(matches!(outcome, Outcome::Shutdown { .. }), "{outcome:?}");
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        });
    }

    /// One route's pending record, beside the fixture server's own state.
    fn pending_record(lane: &Lane) -> crate::service::lane::admin::PendingOperations {
        crate::service::lane::admin::pending_operations(
            Directory::open(&lane.service.paths.data.join("client-pending"), true).unwrap(),
        )
    }

    /// A client's shared lock on its state root, as the CLI holds it for each command.
    struct Shared {
        directory: Directory,
        lock: Option<crate::storage::Lock>,
    }
    impl crate::service::lane::admin::Share for Shared {
        fn release(&mut self) {
            self.lock = None;
        }
        fn resume(&mut self, deadline: Instant) -> bool {
            let limit = deadline.saturating_duration_since(Instant::now());
            match self
                .directory
                .wait_lock_shared("state.lock", limit)
                .unwrap()
            {
                crate::storage::LockWait::Locked(lock) => {
                    self.lock = Some(lock);
                    true
                }
                _ => false,
            }
        }
    }

    /// The Pending Version a Restart starts may migrate the client's state before it is ready,
    /// so the client lets its share go while it waits.
    #[test]
    fn a_client_waiting_for_a_restart_lets_its_state_migrate() {
        use crate::service::lane::admin;
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let state = lane.service.paths.data.join("client-state");
            let migrated = Arc::new(Mutex::new(None));
            let (old, seen, root) = (lane.lane.clone(), migrated.clone(), state.clone());
            let helper_switch = switch.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                // The new runtime takes the state alone, as its CLI state migration does.
                let directory = Directory::open(&root, false).unwrap();
                *seen.lock().unwrap() = Some(directory.try_lock("state.lock").unwrap().is_some());
                let (switch, old, op) = (helper_switch.clone(), old.clone(), *op);
                std::thread::spawn(move || {
                    let helper = helper_lock(&old);
                    old.ledger().unwrap().claim(&op).unwrap();
                    switch.down();
                    drop(helper);
                });
            }));
            let directory = Directory::open(&state, true).unwrap();
            let mut share = Shared {
                lock: Some(directory.lock_shared("state.lock").unwrap()),
                directory,
            };
            let pending = pending_record(&lane);
            let route = client::Lane::over(switch.clone());
            let (mut session, _) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            let preview = admin::preview(&mut session, LaneAction::Restart).unwrap();
            admin::execute(
                &route,
                session,
                LaneAction::Restart,
                preview.token,
                Duration::from_secs(1),
                &pending,
                &mut share,
                &mut || Ok(()),
                &|| None,
            )
            .unwrap();
            assert_eq!(*migrated.lock().unwrap(), Some(true));
            assert!(share.lock.is_some(), "the client shares its state again");
        });
    }

    /// A Restart whose receipt never arrives stays in the route's pending record, and the next
    /// connection after the server returns settles and acknowledges it.
    #[test]
    fn an_unconfirmed_restart_stays_pending_until_a_later_connection_settles_it() {
        use crate::service::lane::admin::{self, Verdict};
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let (helper_switch, old) = (switch.clone(), lane.lane.clone());
            // The helper takes the Restart and the server goes away. It returns only after the
            // flow gave up, as when a server stalls past the client's wait.
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                let (switch, old, op) = (helper_switch.clone(), old.clone(), *op);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    let helper = helper_lock(&old);
                    old.ledger().unwrap().claim(&op).unwrap();
                    switch.down();
                    drop(helper);
                });
            }));
            let pending = pending_record(&lane);
            let route = client::Lane::over(switch.clone());
            let (mut session, _) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            let preview = admin::preview(&mut session, LaneAction::Restart).unwrap();
            let outcome = admin::execute(
                &route,
                session,
                LaneAction::Restart,
                preview.token,
                Duration::from_secs(3),
                &pending,
                &mut admin::Unshared,
                &mut || Ok(()),
                &|| None,
            )
            .unwrap();
            assert!(
                matches!(outcome, Outcome::Unconfirmed { .. }),
                "{outcome:?}"
            );
            assert_eq!(admin::verdict(&outcome), Verdict::Uncertain);
            let recorded = pending.list().unwrap();
            assert_eq!(recorded.len(), 1, "the operation stays pending");

            switch.up(lane.restarted());
            let (_, settled) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            assert_eq!(settled.len(), 1);
            assert_eq!(settled[0].operation, recorded[0]);
            // Start-up can't prove a Restart whose helper ended without recording an outcome,
            // so it settles as unresolved, which the client reports as uncertain.
            assert_eq!(settled[0].state, LaneOperationState::Unresolved);
            assert_eq!(admin::receipt_verdict(&settled[0]), Verdict::Uncertain);
            assert!(pending.list().unwrap().is_empty(), "settling removes it");
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
        });
    }

    /// A server whose admission stalls for `stall`, as a stopped Core holds an accepted
    /// connection, and that answers every request once it resumes.
    struct Paused {
        lane: Arc<LaneCore>,
        stall: Duration,
    }
    impl Connector for Paused {
        fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
            std::thread::sleep(self.stall);
            let switch = Switch::default();
            switch.up(self.lane.clone());
            let connected = switch.connect(Instant::now() + Duration::from_secs(5));
            // The serving thread owns its own end, so the switch may go.
            std::mem::forget(switch);
            connected
        }
    }

    /// A confirmed Stop whose lane stalls past its deadline while it opens never sends
    /// `Execute`, even once the Core resumes and would admit it.
    #[test]
    fn a_lane_that_stalls_past_the_deadline_at_admission_dispatches_nothing() {
        with_lane(|lane| {
            lane.host.sessions.lock().unwrap().push([3; 32]);
            let pending = pending(&lane);
            let paused = client::Lane::over(Arc::new(Paused {
                lane: lane.lane.clone(),
                stall: Duration::from_millis(1500),
            }));
            let began = Instant::now();
            let opened = admin::open(&paused, &pending, began + Duration::from_millis(500));
            assert_eq!(
                opened.err().map(|unopened| unopened.error),
                Some(LaneError::Deadline)
            );
            assert!(
                began.elapsed() < Duration::from_secs(1),
                "{:?}",
                began.elapsed()
            );
            // The Core resumes; nothing it receives admits the Stop.
            std::thread::sleep(Duration::from_millis(1500));
            assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
            assert!(pending.list().unwrap().is_empty());
        });
    }

    /// An approval whose deadline already passed records and sends nothing, and a flow with no
    /// time left never runs its dispatch step.
    #[test]
    fn an_approval_with_no_time_left_records_and_sends_nothing() {
        with_lane(|lane| {
            lane.host.sessions.lock().unwrap().push([3; 32]);
            let (client, session, approved) = approved_stop(&lane);
            let pending = pending(&lane);
            let mut dispatched = 0;
            let outcome = admin::execute_approved(
                &client,
                session,
                LaneAction::Stop,
                approved.token.clone(),
                Instant::now(),
                Duration::from_secs(90),
                &pending,
                &mut admin::Unshared,
                &mut || {
                    dispatched += 1;
                    Ok(())
                },
                &|| None,
                &mut |_| {},
            );
            assert_eq!(outcome, Err(LaneError::Deadline));
            assert_eq!(dispatched, 0);
            let session = client.session().unwrap();
            let mut admitted = 0;
            let outcome = client::perform(
                &(Arc::new({
                    let switch = Switch::default();
                    switch.up(lane.lane.clone());
                    switch
                }) as Arc<dyn Connector>),
                session,
                LaneAction::Stop,
                approved.token,
                crate::service::operations::random_operation_id().unwrap(),
                Duration::ZERO,
                &mut |_| {
                    admitted += 1;
                    Ok(())
                },
                &|| None,
            );
            assert_eq!(outcome, Err(LaneError::Deadline));
            assert_eq!(admitted, 0);
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
            assert!(pending.list().unwrap().is_empty());
        });
    }

    /// Another caller of the same route and identity that holds the operation record past the
    /// deadline ends the recovery and the approval by it. The approval sends no `Execute`.
    #[test]
    fn a_record_another_caller_holds_past_the_deadline_dispatches_nothing() {
        with_lane(|lane| {
            let (client, session, approved) = approved_stop(&lane);
            let pending = pending(&lane);
            let record =
                Directory::open(&lane.service.paths.data.with_extension("pending"), false).unwrap();
            let held = record.lock("service-operations.lock", false).unwrap();

            let began = Instant::now();
            let opened = admin::open(&client, &pending, began + Duration::from_millis(500));
            assert_eq!(
                opened.err().map(|unopened| unopened.error),
                Some(LaneError::Deadline)
            );
            assert!(
                began.elapsed() < Duration::from_millis(1500),
                "{:?}",
                began.elapsed()
            );

            let began = Instant::now();
            let outcome = admin::execute_approved(
                &client,
                session,
                LaneAction::Stop,
                approved.token,
                began + Duration::from_secs(1),
                Duration::from_secs(90),
                &pending,
                &mut admin::Unshared,
                &mut || Ok(()),
                &|| None,
                &mut |_| {},
            );
            assert_eq!(outcome, Err(LaneError::Deadline));
            assert!(
                began.elapsed() < Duration::from_secs(2),
                "{:?}",
                began.elapsed()
            );
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
            drop(held);
            assert!(pending.list().unwrap().is_empty());
        });
    }

    /// A [`Shared`] whose first release hands the state to a migration and returns only once
    /// the migration holds it alone, so the approval's next resume always meets that migration.
    struct HandedOff {
        shared: Shared,
        /// Starts the migration, then reports that it holds the state.
        handoff: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
        migrated: bool,
    }
    impl crate::service::lane::admin::Share for HandedOff {
        fn release(&mut self) {
            crate::service::lane::admin::Share::release(&mut self.shared);
            if let Some((start, taken)) = self.handoff.take() {
                start.send(()).unwrap();
                taken.recv_timeout(Duration::from_secs(10)).unwrap();
                self.migrated = true;
            }
        }
        fn resume(&mut self, deadline: Instant) -> bool {
            crate::service::lane::admin::Share::resume(&mut self.shared, deadline)
        }
    }

    /// A starting SpaceTerm Core's migration that takes the client's state alone while an
    /// approval waits for another operation, and keeps it past the deadline, never extends the
    /// approval: it ends by its deadline with the refusal that admitted nothing.
    #[test]
    fn a_migration_that_holds_the_state_past_the_deadline_ends_the_approval_by_it() {
        with_lane(|lane| {
            let (client, session, approved) = approved_stop(&lane);
            let _settle = another_clients_restart(&lane);
            let state = lane.service.paths.data.join("client-state");
            let directory = Directory::open(&state, true).unwrap();
            let (start, started) = std::sync::mpsc::channel();
            let (taken, took) = std::sync::mpsc::channel();
            let mut share = HandedOff {
                shared: Shared {
                    lock: Some(directory.lock_shared("state.lock").unwrap()),
                    directory,
                },
                handoff: Some((start, took)),
                migrated: false,
            };
            let migrating = Directory::open(&state, false).unwrap();
            let (end, ended) = std::sync::mpsc::channel::<()>();
            let migration = std::thread::spawn(move || {
                // The migration starts once the approval let its share go, and takes the state
                // alone before the approval asks for its share again.
                if started.recv_timeout(Duration::from_secs(10)).is_err() {
                    return;
                }
                let lock = match migrating
                    .wait_lock("state.lock", Duration::from_secs(5), None)
                    .unwrap()
                {
                    crate::storage::LockWait::Locked(lock) => lock,
                    _ => return,
                };
                taken.send(()).unwrap();
                let _ = ended.recv_timeout(Duration::from_secs(10));
                drop(lock);
            });
            let began = Instant::now();
            let outcome = admin::execute_approved(
                &client,
                session,
                LaneAction::Stop,
                approved.token,
                began + Duration::from_secs(2),
                Duration::from_secs(90),
                &pending(&lane),
                &mut share,
                &mut || Ok(()),
                &|| None,
                &mut |_| {},
            );
            assert_eq!(outcome, Err(LaneError::Code("service_busy".into())));
            // The approval waited for its share until the deadline, and not past it.
            let elapsed = began.elapsed();
            assert!(
                elapsed >= Duration::from_secs(2) && elapsed < Duration::from_millis(3500),
                "{elapsed:?}"
            );
            assert!(share.migrated, "the migration took the state");
            assert!(share.shared.lock.is_none());
            // Only the other client's Restart launched.
            assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
            end.send(()).unwrap();
            migration.join().unwrap();
        });
    }

    /// A transport whose write of every frame stops at its deadline, after a byte of the frame
    /// left when `written`.
    struct Expiring {
        written: bool,
    }
    impl crate::protocol::Preamble for Expiring {
        fn send_frame(
            &mut self,
            _: &[u8],
            deadline: Instant,
        ) -> Result<(), crate::protocol::Unsent> {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            Err(crate::protocol::Unsent {
                error: CoreError::Protocol,
                written: self.written,
            })
        }
        fn receive(&mut self, _: usize, _: Instant) -> Result<Vec<u8>, CoreError> {
            Err(CoreError::Protocol)
        }
    }

    /// One [`Expiring`] connection; every later dial finds no server.
    struct OneDial {
        written: bool,
        dials: AtomicUsize,
    }
    impl Connector for OneDial {
        fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
            if self.dials.fetch_add(1, Ordering::SeqCst) > 0 {
                return Err(CoreError::NotRunning);
            }
            Ok((
                Box::new(Expiring {
                    written: self.written,
                }),
                crate::protocol::version::SERVICE,
                crate::protocol::version::SERVICE,
            ))
        }
    }

    /// An `Execute` whose write stops at the deadline dispatched nothing when no byte of it left,
    /// and stays an unconfirmed outcome of its operation when one did.
    #[test]
    fn an_execute_whose_write_expires_is_unsent_only_when_no_byte_left() {
        for written in [false, true] {
            let connector: Arc<dyn Connector> = Arc::new(OneDial {
                written,
                dials: AtomicUsize::new(0),
            });
            let session =
                Session::open(connector.as_ref(), Instant::now() + Duration::from_secs(5)).unwrap();
            let id = crate::service::operations::random_operation_id().unwrap();
            let outcome = client::perform(
                &connector,
                session,
                LaneAction::Restart,
                "c1-00".into(),
                id,
                Duration::from_millis(1500),
                &mut |_| Ok(()),
                &|| None,
            );
            if written {
                assert_eq!(
                    outcome,
                    Ok(Outcome::Unconfirmed {
                        operation: id.to_string(),
                        boot: String::new(),
                        last: None,
                    })
                );
            } else {
                assert_eq!(outcome, Err(LaneError::Deadline));
            }
        }
    }

    /// Which requests [`Cutting`] counts, and at which of them it cuts the connection.
    type Cut = Arc<Mutex<Option<(fn(&LaneRequest) -> bool, usize)>>>;

    /// A server that answers like the lane and cuts its connection at the `nth` request that
    /// `cut` matches: before the lane handles it, or once the lane handled it and before its
    /// reply leaves when `answered`.
    struct Cutting {
        lane: Arc<LaneCore>,
        cut: Cut,
        answered: bool,
    }
    impl Connector for Cutting {
        fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
            let lane = self.lane.clone();
            let cut = self.cut.clone();
            let answered = self.answered;
            let (client, mut core) = lane_pair().map_err(|_| CoreError::Protocol)?;
            std::thread::spawn(move || {
                loop {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let Ok(bytes) = core.receive(MAX_FRAME, deadline) else {
                        return;
                    };
                    let request: LaneRequest = serde_json::from_slice(&bytes).unwrap();
                    let cutting = {
                        let mut cut = cut.lock().unwrap();
                        let mut cutting = false;
                        if let Some((matches, nth)) = cut.as_mut()
                            && matches(&request)
                        {
                            *nth -= 1;
                            if *nth == 0 {
                                *cut = None;
                                cutting = true;
                            }
                        }
                        cutting
                    };
                    if cutting {
                        if answered {
                            let _ = lane.answer(request, &LOCAL);
                        }
                        let _ = core.0.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    let reply = lane.answer(request, &LOCAL);
                    let _ = core.send(&serde_json::to_vec(&reply).unwrap(), deadline);
                }
            });
            Ok((
                Box::new(client),
                crate::protocol::version::SERVICE,
                crate::protocol::version::SERVICE,
            ))
        }
    }

    /// Records a Restart this client admitted earlier and the server settled as done, as a
    /// later lane connection finds it.
    fn settled_leftover(lane: &Lane, pending: &admin::PendingOperations) -> OperationId {
        settled_leftover_as(lane, pending, State::Done)
    }

    /// [`settled_leftover`] that the server settled as `state`.
    fn settled_leftover_as(
        lane: &Lane,
        pending: &admin::PendingOperations,
        state: State,
    ) -> OperationId {
        let id = crate::service::operations::random_operation_id().unwrap();
        let mut ledger = lane.lane.ledger().unwrap();
        ledger
            .admit(Record {
                id,
                actor: LOCAL,
                core: lane.lane.core,
                boot: lane.lane.boot,
                action: Action::Restart,
                observation_digest: [0; 32],
                candidate: None,
                state: State::Admitted,
                updated_at: 0,
                orphaned: false,
            })
            .unwrap();
        ledger.mark(&id, state).unwrap();
        pending.add(&id).unwrap();
        id
    }

    fn resolve(request: &LaneRequest) -> bool {
        matches!(request, LaneRequest::Resolve { .. })
    }

    fn acknowledge(request: &LaneRequest) -> bool {
        matches!(request, LaneRequest::Acknowledge { .. })
    }

    /// A connection that fails while it reads leftovers acknowledges none, and one that fails
    /// while it acknowledges them returns every outcome it already acknowledged.
    #[test]
    fn a_partly_failed_recovery_keeps_every_outcome_it_read() {
        with_lane(|lane| {
            let pending = pending(&lane);
            let first = settled_leftover(&lane, &pending);
            let second = settled_leftover(&lane, &pending);
            let cut = Arc::new(Mutex::new(None));
            let route = client::Lane::over(Arc::new(Cutting {
                lane: lane.lane.clone(),
                cut: cut.clone(),
                answered: false,
            }));
            let open = || admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT);

            *cut.lock().unwrap() = Some((resolve as fn(&LaneRequest) -> bool, 2));
            let unopened = open().err().unwrap();
            assert!(unopened.settled.is_empty());
            assert_eq!(lane.lane.ledger().unwrap().records().unwrap().len(), 2);
            assert_eq!(pending.list().unwrap().len(), 2);

            *cut.lock().unwrap() = Some((acknowledge as fn(&LaneRequest) -> bool, 2));
            let unopened = open().err().unwrap();
            let operations: Vec<_> = unopened
                .settled
                .iter()
                .map(|v| v.operation.clone())
                .collect();
            assert_eq!(operations, vec![first.to_string()]);
            assert_eq!(unopened.settled[0].state, LaneOperationState::Done);
            assert_eq!(pending.list().unwrap(), vec![second.to_string()]);

            let (_, settled) = open().unwrap();
            let operations: Vec<_> = settled.iter().map(|v| v.operation.clone()).collect();
            assert_eq!(operations, vec![second.to_string()]);
            assert!(pending.list().unwrap().is_empty());
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
        });
    }

    fn cutting(lane: &Lane, answered: bool) -> (client::Lane, Cut) {
        let cut = Arc::new(Mutex::new(None));
        let route = client::Lane::over(Arc::new(Cutting {
            lane: lane.lane.clone(),
            cut: cut.clone(),
            answered,
        }));
        (route, cut)
    }

    fn settled_receipt(leftover: Leftover) -> LaneReceipt {
        match leftover {
            Leftover::Settled(receipt) => receipt,
            other => panic!("expected a settled receipt, got {other:?}"),
        }
    }

    /// SpaceTerm Core forgets a receipt before its `Acknowledged` reply leaves. A reply lost after
    /// that, on a recovering connection or on Check Status, still leaves the exact outcome for
    /// the next Check Status and the next recovering connection.
    #[test]
    fn an_acknowledgement_lost_after_the_core_forgot_the_receipt_keeps_its_outcome() {
        with_lane(|lane| {
            let pending = pending(&lane);
            let failed = State::Failed {
                code: "startup_failed".into(),
            };
            let recovered = settled_leftover_as(&lane, &pending, failed.clone());
            let (route, cut) = cutting(&lane, true);
            let check = |id: &OperationId| {
                admin::check(&route, &pending, id, Instant::now() + REQUEST_LIMIT)
                    .map(settled_receipt)
                    .unwrap()
            };
            let expected = |id: &OperationId| LaneReceipt {
                operation: id.to_string(),
                action: LaneAction::Restart,
                state: LaneOperationState::Failed,
                code: Some("startup_failed".into()),
                boot: hex(&lane.lane.boot),
            };

            // A recovering connection's first acknowledgement reaches the Core, which forgets
            // the receipt, and its reply is lost.
            *cut.lock().unwrap() = Some((acknowledge as fn(&LaneRequest) -> bool, 1));
            let unopened = admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT)
                .err()
                .unwrap();
            assert!(unopened.settled.is_empty());
            assert_eq!(
                lane.lane.ledger().unwrap().resolve(&recovered).unwrap(),
                None
            );
            assert!(pending.list().unwrap().contains(&recovered.to_string()));
            assert_eq!(check(&recovered), expected(&recovered));

            // Check Status's own acknowledgement reaches the Core, and its reply is lost.
            let checked = settled_leftover_as(&lane, &pending, failed);
            *cut.lock().unwrap() = Some((acknowledge as fn(&LaneRequest) -> bool, 1));
            assert_eq!(check(&checked), expected(&checked));
            assert_eq!(lane.lane.ledger().unwrap().resolve(&checked).unwrap(), None);
            assert_eq!(check(&checked), expected(&checked));

            // The next recovering connection reports both from what this identity kept.
            let (_, settled) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            let mut operations: Vec<_> = settled.iter().map(|v| v.operation.clone()).collect();
            operations.sort();
            let mut both = vec![recovered.to_string(), checked.to_string()];
            both.sort();
            assert_eq!(operations, both);
            assert!(
                settled
                    .iter()
                    .all(|receipt| receipt.code.as_deref() == Some("startup_failed"))
            );
            assert!(pending.list().unwrap().is_empty());
            assert_eq!(check(&recovered), expected(&recovered));
        });
    }

    /// A flow that follows its own Restart keeps the receipt's outcome before it acknowledges the
    /// receipt, and forgets the operation only once SpaceTerm Core confirmed the
    /// acknowledgement. An acknowledgement cut before the Core handled it leaves the receipt with
    /// the Core and the operation recorded, so the next connection acknowledges it. One cut after
    /// the Core forgot the receipt leaves the operation recorded too, and the kept outcome
    /// answers the next connection.
    #[test]
    fn a_followed_receipt_is_kept_before_its_acknowledgement_and_forgotten_once_confirmed() {
        for answered in [false, true] {
            with_lane(|lane| {
                let pending = pending(&lane);
                let ledger_lane = lane.lane.clone();
                *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                    let (old, op) = (ledger_lane.clone(), *op);
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(300));
                        let _helper = helper_lock(&old);
                        let mut ledger = old.ledger().unwrap();
                        ledger.claim(&op).unwrap();
                        ledger.mark(&op, State::Done).unwrap();
                    });
                }));
                let (route, cut) = cutting(&lane, answered);
                let (mut session, _) =
                    admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
                let preview = admin::preview(&mut session, LaneAction::Restart).unwrap();
                *cut.lock().unwrap() = Some((acknowledge as fn(&LaneRequest) -> bool, 1));
                let outcome = admin::execute_approved(
                    &route,
                    session,
                    LaneAction::Restart,
                    preview.token,
                    Instant::now() + Duration::from_secs(20),
                    Duration::from_secs(20),
                    &pending,
                    &mut admin::Unshared,
                    &mut || Ok(()),
                    &|| None,
                    &mut |_| {},
                )
                .unwrap();
                let Outcome::Settled { receipt, .. } = outcome else {
                    panic!("answered={answered}: expected a settled receipt, got {outcome:?}");
                };
                assert_eq!(receipt.state, LaneOperationState::Done);
                let id = OperationId::parse(&receipt.operation).unwrap();
                assert_eq!(
                    pending.list().unwrap(),
                    vec![receipt.operation.clone()],
                    "answered={answered}: an unconfirmed acknowledgement forgets nothing"
                );
                assert_eq!(
                    pending
                        .kept_by(&receipt.operation, Instant::now() + REQUEST_LIMIT)
                        .unwrap()
                        .as_ref(),
                    Some(&receipt),
                    "answered={answered}: the outcome was kept before the acknowledgement"
                );
                assert_eq!(
                    lane.lane.ledger().unwrap().resolve(&id).unwrap().is_some(),
                    !answered,
                    "answered={answered}"
                );

                let (_, settled) =
                    admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
                assert_eq!(settled, vec![receipt], "answered={answered}");
                assert!(pending.list().unwrap().is_empty(), "answered={answered}");
                assert_eq!(lane.lane.ledger().unwrap().resolve(&id).unwrap(), None);
            });
        }
    }

    fn executes(request: &LaneRequest) -> bool {
        matches!(request, LaneRequest::Execute { .. })
    }

    /// An approval's one absolute deadline holds across every handoff before `Execute`. A step
    /// that runs past it, as a slow journal or a process suspended between steps would, sends
    /// no byte of `Execute`, admits nothing and leaves no record.
    #[test]
    fn no_execute_byte_leaves_after_the_approvals_deadline() {
        with_lane(|lane| {
            let pending = pending(&lane);
            let (route, cut) = cutting(&lane, false);
            let (mut session, _) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            let preview = admin::preview(&mut session, LaneAction::Restart).unwrap();
            *cut.lock().unwrap() = Some((executes as fn(&LaneRequest) -> bool, 1));
            let deadline = Instant::now() + Duration::from_millis(300);
            let outcome = admin::execute_approved(
                &route,
                session,
                LaneAction::Restart,
                preview.token,
                deadline,
                Duration::from_secs(20),
                &pending,
                &mut admin::Unshared,
                &mut || {
                    std::thread::sleep(
                        deadline.saturating_duration_since(Instant::now())
                            + Duration::from_millis(50),
                    );
                    Ok(())
                },
                &|| None,
                &mut |_| {},
            );
            assert_eq!(outcome, Err(LaneError::Deadline));
            assert!(cut.lock().unwrap().is_some(), "no Execute reached the Core");
            assert!(pending.list().unwrap().is_empty());
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
        });
    }

    /// Settings' Check Now on a paired Machine opens the lane as the original surface's identity
    /// and acknowledges its leftovers, whole or in part. The original surface's Check Status for
    /// each operation then reads the same outcome.
    #[test]
    fn check_now_recovery_leaves_the_outcome_for_the_original_check_status() {
        with_lane(|lane| {
            let pending = pending(&lane);
            let (route, cut) = cutting(&lane, false);
            let check = |id: &OperationId| {
                admin::check(&route, &pending, id, Instant::now() + REQUEST_LIMIT)
                    .map(settled_receipt)
                    .unwrap()
            };

            let done = settled_leftover(&lane, &pending);
            let refused = settled_leftover_as(
                &lane,
                &pending,
                State::Failed {
                    code: "address_unavailable".into(),
                },
            );
            let (_, settled) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            assert_eq!(settled.len(), 2);
            assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
            for receipt in &settled {
                let id = OperationId::parse(&receipt.operation).unwrap();
                assert_eq!(&check(&id), receipt);
            }
            assert_eq!(check(&done).state, LaneOperationState::Done);
            assert_eq!(check(&refused).code.as_deref(), Some("address_unavailable"));

            // A recovery that fails before its second acknowledgement reaches the Core.
            let first = settled_leftover(&lane, &pending);
            let second = settled_leftover(&lane, &pending);
            *cut.lock().unwrap() = Some((acknowledge as fn(&LaneRequest) -> bool, 2));
            let unopened = admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT)
                .err()
                .unwrap();
            assert_eq!(unopened.settled.len(), 1);
            let acknowledged = &unopened.settled[0];
            let id = OperationId::parse(&acknowledged.operation).unwrap();
            assert!([first, second].contains(&id));
            assert_eq!(&check(&id), acknowledged);
            let other = if id == first { second } else { first };
            assert_eq!(check(&other).state, LaneOperationState::Done);
        });
    }

    /// Check Status for an operation a flow of the same identity still follows reads its
    /// outcome and leaves the receipt for that flow, which still settles on its own receipt.
    #[test]
    fn check_status_never_acknowledges_a_receipt_a_running_flow_of_its_identity_follows() {
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let route = client::Lane::over(switch.clone());
            let pending = Arc::new(pending(&lane));
            let ledger_lane = lane.lane.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                let (old, op) = (ledger_lane.clone(), *op);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    let _helper = helper_lock(&old);
                    let mut ledger = old.ledger().unwrap();
                    ledger.claim(&op).unwrap();
                    ledger.mark(&op, State::Done).unwrap();
                });
            }));
            let (mut session, _) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            let preview = admin::preview(&mut session, LaneAction::Restart).unwrap();
            let (follow_route, follow_pending) = (route.clone(), pending.clone());
            let follower = std::thread::spawn(move || {
                admin::execute_approved(
                    &follow_route,
                    session,
                    LaneAction::Restart,
                    preview.token,
                    Instant::now() + Duration::from_secs(20),
                    Duration::from_secs(20),
                    &follow_pending,
                    &mut admin::Unshared,
                    &mut || Ok(()),
                    &|| None,
                    &mut |_| {},
                )
            });
            let began = Instant::now();
            let followed = loop {
                if let Some(operation) = pending.list().unwrap().first() {
                    break OperationId::parse(operation).unwrap();
                }
                assert!(began.elapsed() < Duration::from_secs(5), "never journaled");
                std::thread::sleep(Duration::from_millis(20));
            };
            // The Restart settles at 1.5 s, and the follower reads it at its next poll.
            std::thread::sleep(Duration::from_millis(1700));
            let checked = settled_receipt(
                admin::check(&route, &pending, &followed, Instant::now() + REQUEST_LIMIT).unwrap(),
            );
            assert_eq!(checked.state, LaneOperationState::Done);
            let outcome = follower.join().unwrap().unwrap();
            let Outcome::Settled { receipt, .. } = outcome else {
                panic!("expected the follower's own settled receipt, got {outcome:?}");
            };
            assert_eq!(receipt, checked);
            assert!(pending.list().unwrap().is_empty());
        });
    }

    /// Another command of the same identity that opens the lane while a Restart is followed
    /// leaves every leftover for later, so the Restart reads its own receipt.
    #[test]
    fn a_concurrent_connection_never_settles_a_receipt_a_running_flow_follows() {
        with_lane(|lane| {
            let switch = Arc::new(Switch::default());
            switch.up(lane.lane.clone());
            let route = client::Lane::over(switch.clone());
            let pending = Arc::new(pending(&lane));
            // An earlier command's settled Restart, recorded after the follower opened.
            let earlier = settled_leftover(&lane, &pending);
            pending.remove(&earlier.to_string()).unwrap();
            let ledger_lane = lane.lane.clone();
            *lane.hook.lock().unwrap() = Some(Box::new(move |op: &OperationId| {
                let (old, op) = (ledger_lane.clone(), *op);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    let _helper = helper_lock(&old);
                    let mut ledger = old.ledger().unwrap();
                    ledger.claim(&op).unwrap();
                    ledger.mark(&op, State::Done).unwrap();
                });
            }));
            let (mut session, _) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            let preview = admin::preview(&mut session, LaneAction::Restart).unwrap();
            let (follow_route, follow_pending) = (route.clone(), pending.clone());
            let follower = std::thread::spawn(move || {
                admin::execute_approved(
                    &follow_route,
                    session,
                    LaneAction::Restart,
                    preview.token,
                    Instant::now() + Duration::from_secs(20),
                    Duration::from_secs(20),
                    &follow_pending,
                    &mut admin::Unshared,
                    &mut || Ok(()),
                    &|| None,
                    &mut |_| {},
                )
            });
            pending.add(&earlier).unwrap();
            let began = Instant::now();
            while pending.list().unwrap().len() < 2 {
                assert!(began.elapsed() < Duration::from_secs(5), "never journaled");
                std::thread::sleep(Duration::from_millis(20));
            }
            // The Restart settles while this connection is open, as `status` would find it.
            std::thread::sleep(Duration::from_millis(1700));
            let (_, settled) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            assert!(settled.is_empty(), "{settled:?}");
            let outcome = follower.join().unwrap().unwrap();
            let Outcome::Settled { receipt, .. } = outcome else {
                panic!("expected the follower's own settled receipt, got {outcome:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::Done);
            let (_, settled) =
                admin::open(&route, &pending, Instant::now() + REQUEST_LIMIT).unwrap();
            let operations: Vec<_> = settled.iter().map(|v| v.operation.clone()).collect();
            assert_eq!(operations, vec![earlier.to_string()]);
            assert!(pending.list().unwrap().is_empty());
        });
    }

    /// A service version below every request: each request is 1.0 today, so only a selected
    /// version below 1.0 lacks one.
    const OLDER: crate::protocol::version::ProtocolVersion =
        crate::protocol::version::ProtocolVersion { major: 0, minor: 9 };

    #[test]
    fn the_lane_answers_a_request_its_selected_version_lacks_as_unsupported() {
        with_lane(|lane| {
            let (mut client, mut core) = lane_pair().unwrap();
            let peer = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut replies = Vec::new();
                for request in [LaneRequest::Status, LaneRequest::Facts {}] {
                    client
                        .send(&serde_json::to_vec(&request).unwrap(), deadline)
                        .unwrap();
                    replies.push(
                        serde_json::from_slice::<LaneReply>(
                            &client.receive(MAX_FRAME, deadline).unwrap(),
                        )
                        .unwrap(),
                    );
                }
                replies
            });
            let admitted = crate::protocol::AdmittedLane {
                actor: LOCAL,
                selected: OLDER,
            };
            let _ = lane.lane.serve(&mut core, admitted, None, &|| Ok(()));
            let unsupported = LaneReply::Error {
                code: "unsupported".into(),
            };
            assert_eq!(peer.join().unwrap(), [unsupported.clone(), unsupported]);
        });
    }

    #[test]
    fn a_reply_only_a_newer_core_sends_names_this_client_as_the_older_side() {
        use crate::protocol::version::{Incompatible, ProtocolVersion, SERVICE};
        /// Admits at this client's version, offers `core`, and answers every request with a
        /// reply this client doesn't know.
        struct Newer(ProtocolVersion);
        impl Connector for Newer {
            fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
                let (client, mut core) = lane_pair().map_err(|_| CoreError::Protocol)?;
                std::thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while core.receive(MAX_FRAME, deadline).is_ok() {
                        let _ = core.send(br#"{"reply":"later"}"#, deadline);
                    }
                });
                Ok((Box::new(client), SERVICE, self.0))
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let newer = ProtocolVersion {
            minor: SERVICE.minor + 1,
            ..SERVICE
        };
        for (core, expected) in [
            (
                newer,
                CoreError::IncompatibleProtocol(Incompatible::CallerTooOld),
            ),
            (SERVICE, CoreError::Protocol),
        ] {
            let mut session = Session::open(&Newer(core), deadline).unwrap();
            assert_eq!(
                session.request(&LaneRequest::Facts {}, deadline),
                Err(expected)
            );
        }
    }

    #[test]
    fn a_session_sends_nothing_its_selected_version_lacks() {
        /// Admits at [`OLDER`] and keeps the Core's end of the connection.
        #[derive(Default)]
        struct Older(Mutex<Option<LaneStream>>);
        impl Connector for Older {
            fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
                let (client, core) = lane_pair().map_err(|_| CoreError::Protocol)?;
                *self.0.lock().unwrap() = Some(core);
                Ok((Box::new(client), OLDER, OLDER))
            }
        }
        let older = Older::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut session = Session::open(&older, deadline).unwrap();
        assert_eq!(session.selected(), OLDER);
        assert_eq!(
            session.request(&LaneRequest::Status, deadline),
            Err(CoreError::Unsupported)
        );
        drop(session);
        let mut core = older.0.lock().unwrap().take().unwrap();
        let mut sent = Vec::new();
        std::io::Read::read_to_end(&mut core.0, &mut sent).unwrap();
        assert!(sent.is_empty(), "the Core received {} bytes", sent.len());
    }

    /// The service minor before the detailed update facts.
    const BEFORE_DETAILED: crate::protocol::version::ProtocolVersion =
        crate::protocol::version::ProtocolVersion { major: 1, minor: 1 };

    /// Serves `requests` on a connection whose admission selected `selected`, and returns the
    /// replies.
    fn served(
        lane: &Lane,
        selected: crate::protocol::version::ProtocolVersion,
        requests: Vec<LaneRequest>,
    ) -> Vec<LaneReply> {
        let (mut client, mut core) = lane_pair().unwrap();
        let peer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut replies = Vec::new();
            for request in requests {
                client
                    .send(&serde_json::to_vec(&request).unwrap(), deadline)
                    .unwrap();
                replies.push(
                    serde_json::from_slice::<LaneReply>(
                        &client.receive(MAX_FRAME, deadline).unwrap(),
                    )
                    .unwrap(),
                );
            }
            replies
        });
        let admitted = crate::protocol::AdmittedLane {
            actor: LOCAL,
            selected,
        };
        let _ = lane.lane.serve(&mut core, admitted, None, &|| Ok(()));
        peer.join().unwrap()
    }

    #[test]
    fn a_service_1_2_peer_cannot_request_the_final_stop_grant() {
        with_lane(|lane| {
            assert_eq!(
                served(
                    &lane,
                    crate::protocol::version::ProtocolVersion { major: 1, minor: 2 },
                    vec![LaneRequest::GrantStop {
                        operation: crate::service::operations::random_operation_id()
                            .unwrap()
                            .to_string(),
                        boot: hex(&lane.lane.boot),
                        candidate: hex(&[7; 32]),
                    }],
                ),
                vec![LaneReply::Error {
                    code: "unsupported".into()
                }]
            );
        });
    }

    #[test]
    fn a_service_1_2_peer_reads_the_detailed_update_facts_as_one_snapshot() {
        with_lane(|lane| {
            let replies = served(
                &lane,
                crate::protocol::version::ProtocolVersion { major: 1, minor: 2 },
                vec![
                    LaneRequest::Facts {},
                    LaneRequest::DetailedFacts {},
                    // Another boot answers at once.
                    LaneRequest::WaitDetailedFacts {
                        boot: "another".into(),
                        revision: 0,
                    },
                ],
            );
            let LaneReply::Facts { boot, facts } = &replies[0] else {
                panic!("{:?}", replies[0]);
            };
            for reply in &replies[1..] {
                let LaneReply::DetailedFacts {
                    boot: detailed_boot,
                    detailed,
                } = reply
                else {
                    panic!("{reply:?}");
                };
                assert_eq!(detailed_boot, boot);
                assert_eq!(detailed.facts, **facts);
                assert_eq!(detailed.validate(), Ok(()));
                assert_eq!(detailed.continuation, None, "nothing waits");
            }
        });
    }

    #[test]
    fn a_service_1_1_peer_never_receives_the_detailed_update_facts() {
        with_lane(|lane| {
            let replies = served(
                &lane,
                BEFORE_DETAILED,
                vec![
                    LaneRequest::DetailedFacts {},
                    LaneRequest::WaitDetailedFacts {
                        boot: "another".into(),
                        revision: 0,
                    },
                    LaneRequest::Facts {},
                ],
            );
            let unsupported = LaneReply::Error {
                code: "unsupported".into(),
            };
            assert_eq!(replies[..2], [unsupported.clone(), unsupported]);
            assert!(matches!(replies[2], LaneReply::Facts { .. }));
        });
    }

    #[test]
    fn a_client_asks_a_service_1_1_core_for_no_detailed_update_facts() {
        /// Admits at service 1.1 and keeps the Core's end of the connection.
        #[derive(Default)]
        struct Older(Mutex<Option<LaneStream>>);
        impl Connector for Older {
            fn connect(&self, _: Instant) -> Result<client::Connected, CoreError> {
                let (client, core) = lane_pair().map_err(|_| CoreError::Protocol)?;
                *self.0.lock().unwrap() = Some(core);
                Ok((Box::new(client), BEFORE_DETAILED, BEFORE_DETAILED))
            }
        }
        let older = Older::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut session = Session::open(&older, deadline).unwrap();
        assert_eq!(admin::detailed_facts(&mut session), Ok(None));
        drop(session);
        let mut core = older.0.lock().unwrap().take().unwrap();
        let mut sent = Vec::new();
        std::io::Read::read_to_end(&mut core.0, &mut sent).unwrap();
        assert!(sent.is_empty(), "the Core received {} bytes", sent.len());
    }

    #[test]
    fn detailed_facts_messages_keep_their_json_shape() {
        assert_eq!(
            serde_json::to_value(LaneRequest::DetailedFacts {}).unwrap(),
            serde_json::json!({ "request": "detailed_facts" })
        );
        assert_eq!(
            serde_json::from_str::<LaneRequest>(
                r#"{"request":"wait_detailed_facts","boot":"b1","revision":4}"#
            )
            .unwrap(),
            LaneRequest::WaitDetailedFacts {
                boot: "b1".into(),
                revision: 4
            }
        );
    }
}

#[test]
fn facts_messages_keep_their_json_shape() {
    use crate::update::facts::{Platform, UpdateCheck, UpdateFacts, Version};
    assert_eq!(
        serde_json::from_str::<LaneRequest>(r#"{"request":"facts"}"#).unwrap(),
        LaneRequest::Facts {}
    );
    assert_eq!(
        serde_json::to_value(LaneRequest::Facts {}).unwrap(),
        serde_json::json!({ "request": "facts" })
    );
    assert_eq!(
        serde_json::from_str::<LaneRequest>(r#"{"request":"wait_facts","boot":"b1","revision":4}"#)
            .unwrap(),
        LaneRequest::WaitFacts {
            boot: "b1".into(),
            revision: 4
        }
    );
    let reply = LaneReply::Facts {
        boot: "b1".into(),
        facts: Box::new(UpdateFacts {
            revision: 4,
            platform: Platform::MacosArm64,
            running: Version::parse("0.9.0").unwrap(),
            pending: None,
            sessions: 2,
            operation: None,
            check: UpdateCheck::Succeeded { at: 10 },
            skipped: None,
            last_failure: None,
        }),
    };
    let json = serde_json::json!({
        "reply": "facts",
        "boot": "b1",
        "facts": {
            "revision": 4,
            "platform": "macos-arm64",
            "running": "0.9.0",
            "pending": null,
            "sessions": 2,
            "operation": null,
            "check": { "succeeded": { "at": 10 } },
            "skipped": null,
            "last_failure": null,
        },
    });
    assert_eq!(serde_json::to_value(&reply).unwrap(), json);
    assert_eq!(
        serde_json::from_value::<LaneReply>(json.clone()).unwrap(),
        reply
    );

    assert!(serde_json::from_str::<LaneRequest>(r#"{"request":"facts","since":1}"#).is_err());
    assert!(
        serde_json::from_str::<LaneRequest>(
            r#"{"request":"wait_facts","boot":"b1","revision":4,"since":1}"#
        )
        .is_err()
    );
    let mut envelope = json.clone();
    envelope["staged"] = serde_json::json!([]);
    assert!(serde_json::from_value::<LaneReply>(envelope).is_err());
    let mut nested = json;
    nested["facts"]["staged"] = serde_json::json!([]);
    assert!(serde_json::from_value::<LaneReply>(nested).is_err());
}

#[test]
fn no_client_can_activate_an_installation_it_names() {
    // A Restart reaches only the Pending Version, so a skipped or previous version cannot run.
    assert!(
        serde_json::from_str::<LaneRequest>(
            r#"{"request":"execute","action":{"action":"update","installation":"abc"},"token":"c1-00","operation":"op"}"#
        )
        .is_err()
    );
}

#[test]
fn every_client_sees_a_confirmed_restart_stop_or_uninstall_in_the_update_facts() {
    use crate::update::facts::Operation;
    use crate::update::source::FactsSource;
    for (action, op) in [
        (LaneAction::Restart, 1),
        (LaneAction::Stop, 2),
        (LaneAction::Uninstall, 3),
    ] {
        with_lane(|lane| {
            let updates = lane.lane.updates().unwrap().clone();
            let (token, _) = preview(&lane.lane, action.clone());
            let reply = execute(&lane.lane, action.clone(), &token, op);
            assert!(matches!(reply, LaneReply::Accepted { .. }), "{reply:?}");
            let expected = match action {
                LaneAction::Stop => Operation::Stopping,
                LaneAction::Uninstall => Operation::Uninstalling,
                // No Pending Version waits, so the version stays.
                _ => Operation::RestartingSameVersion,
            };
            // Another desktop follows the same engine, which it did not ask.
            assert_eq!(updates.facts().operation, Some(expected));
        });
    }
}

#[test]
fn a_lane_of_service_1_0_reads_only_the_facts_and_codes_that_minor_has() {
    use crate::protocol::version::ProtocolVersion;
    use crate::update::facts::{CheckFailure, Operation, UpdateCheck};
    use crate::update::source::FactsSource;
    with_lane(|lane| {
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        execute(&lane.lane, LaneAction::Restart, &token, 1);
        let reply = lane.lane.answer(LaneRequest::Facts {}, &ACTOR);
        let older = ProtocolVersion { major: 1, minor: 0 };
        let running = lane.lane.updates().unwrap().facts().running;
        let LaneReply::Facts { facts, .. } = reply.clone().for_selected(older) else {
            panic!("expected facts");
        };
        assert_eq!(facts.operation, Some(Operation::Restarting { to: running }));
        let LaneReply::Facts { facts, .. } = reply.for_selected(crate::protocol::version::SERVICE)
        else {
            panic!("expected facts");
        };
        assert_eq!(facts.operation, Some(Operation::RestartingSameVersion));
        let clock = LaneReply::Error {
            code: "clock_behind".into(),
        };
        assert_eq!(
            code(clock.clone().for_selected(older)),
            "candidate_unverified"
        );
        assert_eq!(
            code(clock.for_selected(crate::protocol::version::SERVICE)),
            "clock_behind"
        );
        let failing = UpdateCheck::Failing {
            since: 1,
            last_success: None,
            reason: CheckFailure::ClockBehind,
        };
        let facts = crate::update::facts::UpdateFacts {
            check: failing,
            ..*facts
        };
        assert!(matches!(
            facts.for_selected(older).check,
            UpdateCheck::Failing {
                reason: CheckFailure::Verification,
                ..
            }
        ));
    });
}

#[test]
fn removing_a_machine_with_a_pending_version_reads_as_uninstalling_everywhere() {
    use crate::update::facts::Operation;
    use crate::update::source::FactsSource;
    with_lane(|lane| {
        install_pending(&lane, "1.0.0");
        let (token, _) = preview(&lane.lane, LaneAction::Uninstall);
        assert!(matches!(
            execute(&lane.lane, LaneAction::Uninstall, &token, 1),
            LaneReply::Accepted { .. }
        ));
        assert_eq!(lane.launches.load(Ordering::SeqCst), 1);
        let updates = lane.lane.updates().unwrap();
        assert_eq!(updates.facts().operation, Some(Operation::Uninstalling));
    });
}

#[test]
fn a_confirmed_restart_into_the_pending_version_names_it() {
    use crate::update::facts::{Operation, Version};
    with_lane(|lane| {
        install_pending(&lane, "1.0.0");
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        assert!(matches!(
            execute(&lane.lane, LaneAction::Restart, &token, 1),
            LaneReply::Accepted { .. }
        ));
        let LaneReply::Facts { facts, .. } = lane.lane.answer(LaneRequest::Facts {}, &ACTOR) else {
            panic!("expected facts");
        };
        let updating = Some(Operation::Restarting {
            to: Version::parse("1.0.0").unwrap(),
        });
        assert_eq!(facts.operation, updating);
    });
}

#[test]
fn a_confirmed_restart_waits_for_an_install_in_progress() {
    with_lane(|lane| {
        let updates = lane.lane.updates().unwrap().clone();
        let installing = updates
            .begin_install(
                crate::update::ordering::Candidate::new(
                    crate::update::facts::Version::parse("2.0.0").unwrap(),
                    Some(crate::update::ordering::Floor::parse("0.0.0").unwrap()),
                )
                .unwrap(),
                10,
            )
            .unwrap();
        let (token, _) = preview(&lane.lane, LaneAction::Restart);
        assert_eq!(
            code(execute(&lane.lane, LaneAction::Restart, &token, 1)),
            "service_busy"
        );
        assert!(lane.lane.ledger().unwrap().records().unwrap().is_empty());
        assert_eq!(lane.launches.load(Ordering::SeqCst), 0);
        drop(installing);
    });
}

#[test]
fn a_restart_whose_helper_fails_before_stopping_the_server_ends_and_may_try_again() {
    use crate::update::source::FactsSource;
    with_lane(|lane| {
        let updates = lane.lane.updates().unwrap().clone();
        install_pending(&lane, "1.0.0");
        let restarter: Arc<dyn runtime::updates::Restarter> = lane.lane.clone();
        updates
            .run(Arc::downgrade(&restarter), Arc::new(|_| Vec::new()), || {
                None
            })
            .unwrap();
        crate::service::activation::tests::quiet(&updates);
        assert!(
            updates.facts().operation.is_some(),
            "the silent restart began"
        );
        let records = lane.lane.ledger().unwrap().records().unwrap();
        // The helper fails before it stops this server.
        lane.lane
            .ledger()
            .unwrap()
            .mark(
                &records[0].id,
                State::Failed {
                    code: "service_busy".into(),
                },
            )
            .unwrap();
        assert!(lane.lane.release_if_settled());
        assert_eq!(updates.facts().operation, None);
        let data = Directory::open(&lane.service.paths.data, false).unwrap();
        let record: serde_json::Value = data.read_json("updates.json").unwrap().unwrap();
        assert_eq!(
            record["silent"],
            serde_json::Value::Null,
            "the next attempt may run"
        );
    });
}

#[test]
fn a_granted_input_barrier_does_not_wait_for_the_helpers_service_lock() {
    with_lane(|lane| {
        lane.host.idle_granted.store(true, Ordering::SeqCst);
        lane.host.fenced.store(true, Ordering::SeqCst);
        let data = Directory::open(&lane.lane.service.paths.data, false).unwrap();
        let held = data.lock("service.lock", false).unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let watching = Arc::clone(&lane.lane);
        let watcher = std::thread::spawn(move || sent.send(watching.release_if_settled()).unwrap());
        let while_held = received.recv_timeout(Duration::from_secs(2));
        drop(held);
        watcher.join().unwrap();
        assert_eq!(while_held.ok(), Some(false));
        assert!(lane.host.fenced.load(Ordering::SeqCst));
    });
}

#[test]
fn a_service_1_3_helper_and_handler_complete_the_encoded_stop_grant() {
    with_lane(|lane| {
        let candidate = digest_bytes(&install_pending(&lane, "1.0.0")).unwrap();
        lane.lane
            .updates()
            .unwrap()
            .confirm(
                runtime::updates::Confirmed::Restart {
                    candidate: Some(candidate),
                },
                || Ok(()),
            )
            .unwrap();
        let record = Record {
            id: OperationId([17; 16]),
            actor: Actor::CORE,
            core: lane.lane.core,
            boot: lane.lane.boot,
            action: Action::Update,
            observation_digest: [0; 32],
            candidate: Some(candidate),
            state: State::Running,
            updated_at: 0,
            orphaned: false,
        };
        *lane.host.grant_target.lock().unwrap() = Some((record.id, candidate, record.boot));
        let (mut helper, mut core) = lane_pair().unwrap();
        let identity = lane.service.identity.clone();
        let caller = std::thread::spawn(move || {
            super::super::helper::grant_stop_over(&mut helper, &identity, &record)
        });
        let mut versions = crate::protocol::versions_for_tests();
        versions.service = crate::protocol::version::ProtocolVersion { major: 1, minor: 3 };
        let admitted = crate::protocol::admit_core(
            &mut core.0,
            &lane.lane.core,
            &lane.service.identity,
            &versions,
        )
        .unwrap();
        let crate::protocol::Admission::Service(admitted) = admitted else {
            panic!("not service");
        };
        assert_eq!(
            admitted.selected,
            crate::protocol::version::ProtocolVersion { major: 1, minor: 3 }
        );
        lane.lane
            .serve(&mut core, admitted, None, &|| Ok(()))
            .unwrap();
        assert_eq!(caller.join().unwrap(), Ok(()));
        assert!(lane.host.idle_granted.load(Ordering::SeqCst));
    });
}

#[test]
fn a_service_1_2_helper_sends_no_stop_grant_request_bytes() {
    with_lane(|lane| {
        let record = Record {
            id: OperationId([17; 16]),
            actor: Actor::CORE,
            core: lane.lane.core,
            boot: lane.lane.boot,
            action: Action::Update,
            observation_digest: [0; 32],
            candidate: Some([5; 32]),
            state: State::Running,
            updated_at: 0,
            orphaned: false,
        };
        let (mut helper, mut core) = lane_pair().unwrap();
        let identity = lane.service.identity.clone();
        let caller = std::thread::spawn(move || {
            super::super::helper::grant_stop_over(&mut helper, &identity, &record)
        });
        let mut versions = crate::protocol::versions_for_tests();
        versions.service = crate::protocol::version::ProtocolVersion { major: 1, minor: 2 };
        let admitted = crate::protocol::admit_core(
            &mut core.0,
            &lane.lane.core,
            &lane.service.identity,
            &versions,
        )
        .unwrap();
        assert!(matches!(admitted, crate::protocol::Admission::Service(_)));
        let request = core
            .receive(MAX_FRAME, Instant::now() + Duration::from_secs(3))
            .ok();
        if request.is_some() {
            core.send(
                br#"{"reply":"stop_granted"}"#,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        }
        assert_eq!(caller.join().unwrap(), Err(CoreError::Unsupported));
        assert!(
            request.is_none(),
            "an older service received grant request bytes"
        );
    });
}

#[test]
fn an_abandoned_helper_thaws_its_real_lease_at_every_dispatch_boundary() {
    use crate::idle_restart::{Activity, Frozen, Lease, Restarting, Session};
    struct Guard(Arc<AtomicUsize>);
    impl Frozen for Guard {
        fn verify(&self) -> Result<(), Restarting> {
            Ok(())
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    struct Idle(Activity, Arc<AtomicUsize>);
    impl Session for Idle {
        fn activity(&self) -> Activity {
            self.0.clone()
        }
        fn working(&self, _: Instant) -> bool {
            false
        }
        fn freeze(&self, _: Instant) -> Result<Box<dyn Frozen>, Restarting> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Guard(self.1.clone())))
        }
    }
    for crash in 0..4 {
        with_lane(|lane| {
            let digest = install_pending(&lane, "1.0.0");
            let directory = lane.service.paths.data.join("installations").join(&digest);
            let (version, commit) = runtime::describe(&directory, &lane.service.identity).unwrap();
            let candidate = RetainedInstallation {
                directory,
                version,
                commit,
            };
            let now = Instant::now();
            let activity = Activity::new(now);
            let stopped = Arc::new(AtomicUsize::new(0));
            let sessions: Vec<Arc<dyn Session>> =
                vec![Arc::new(Idle(activity.clone(), stopped.clone()))];
            *lane.host.native_lease.lock().unwrap() = Some(
                Lease::scan(
                    &sessions,
                    now + Duration::from_secs(7200),
                    Duration::from_secs(3600),
                )
                .unwrap(),
            );
            let mut record = Record {
                id: OperationId([41; 16]),
                actor: Actor::CORE,
                core: lane.lane.core,
                boot: lane.lane.boot,
                action: Action::Update,
                observation_digest: [0; 32],
                candidate: Some(digest_bytes(&digest).unwrap()),
                state: State::Admitted,
                updated_at: 0,
                orphaned: false,
            };
            let data = Directory::open(&lane.service.paths.data, false).unwrap();
            let helper = data.lock(HELPER_LOCK, false).unwrap();
            lane.lane.ledger().unwrap().admit(record.clone()).unwrap();
            record = lane
                .lane
                .ledger()
                .unwrap()
                .claim(&record.id)
                .unwrap()
                .unwrap();
            let calls = std::cell::Cell::new(0);
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::super::helper::activate_pending(&lane.service, &candidate, &record, |_| {
                    calls.set(calls.get() + 1);
                    if calls.get() == 2 && crash == 1 {
                        panic!("before final grant")
                    }
                    let held = lane
                        .host
                        .native_lease
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    held.as_ref().unwrap().freeze_and_grant().unwrap();
                    if (calls.get() == 1 && crash == 0) || (calls.get() == 2 && crash == 2) {
                        panic!("after grant")
                    }
                    Ok(())
                })
            }));
            assert!(crashed.is_err());
            assert_eq!(stopped.load(Ordering::SeqCst), 1);
            assert!(activity.input(Instant::now()).is_err());
            assert!(
                !lane.lane.release_if_settled(),
                "the live helper still owns Stop"
            );
            if crash == 2 {
                lane.lane
                    .ledger()
                    .unwrap()
                    .mark(&record.id, State::Unresolved)
                    .unwrap();
                assert!(
                    !lane.lane.release_if_settled(),
                    "a terminal ledger cannot override live helper ownership"
                );
                assert_eq!(stopped.load(Ordering::SeqCst), 1);
            }
            drop(helper);
            assert!(
                lane.lane.release_if_settled(),
                "crash {crash}: dead helper cannot retain the freeze"
            );
            assert_eq!(stopped.load(Ordering::SeqCst), 0, "crash {crash}");
            assert!(activity.input(Instant::now()).is_ok());
            // Running::stop panics if recovery ever tries to dispatch another Stop.
            if crash != 0 {
                assert!(lane.service.recover_activation().is_err());
            }
            assert_eq!(stopped.load(Ordering::SeqCst), 0);
        });
    }
}

#[test]
fn a_settlement_write_failure_thaws_abandoned_sessions_without_repairing_storage() {
    const NAME: &str = "service::lane::tests::a_settlement_write_failure_thaws_abandoned_sessions_without_repairing_storage";
    const CHILD: &str = "SPACETERM_SETTLEMENT_LIMIT_CHILD";
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("SETTLEMENT_LIMIT_ENTERED"));
        return;
    }
    eprintln!("SETTLEMENT_LIMIT_ENTERED");
    use crate::idle_restart::{Activity, Frozen, Lease, Restarting, Session};
    struct Guard(Arc<AtomicUsize>);
    impl Frozen for Guard {
        fn verify(&self) -> Result<(), Restarting> {
            Ok(())
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    struct Idle(Activity, Arc<AtomicUsize>);
    impl Session for Idle {
        fn activity(&self) -> Activity {
            self.0.clone()
        }
        fn working(&self, _: Instant) -> bool {
            false
        }
        fn freeze(&self, _: Instant) -> Result<Box<dyn Frozen>, Restarting> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Guard(self.1.clone())))
        }
    }
    struct WriteLimit(libc::rlimit, libc::sighandler_t);
    impl Drop for WriteLimit {
        fn drop(&mut self) {
            // SAFETY: this isolated process restores exactly its previous soft limit and handler.
            unsafe {
                assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &self.0), 0);
                libc::signal(libc::SIGXFSZ, self.1);
            }
        }
    }
    #[cfg(target_os = "macos")]
    struct ReadOnlyJournal(Vec<(std::ffi::CString, u32)>);
    #[cfg(target_os = "macos")]
    impl ReadOnlyJournal {
        fn protect(directory: &std::path::Path) -> Self {
            use std::os::unix::ffi::OsStrExt;
            let mut saved = Self(Vec::new());
            for name in ["service.json", HELPER_LOCK, "service.lock"] {
                let path =
                    std::ffi::CString::new(directory.join(name).as_os_str().as_bytes()).unwrap();
                let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
                // SAFETY: these are this fixture's owned records. Immutable flags preserve
                // mode and readability while rejecting write-open, as a read-only mount does.
                unsafe {
                    assert_eq!(libc::stat(path.as_ptr(), metadata.as_mut_ptr()), 0);
                    let flags = metadata.assume_init().st_flags;
                    saved.0.push((path, flags));
                    assert_eq!(
                        libc::chflags(
                            saved.0.last().unwrap().0.as_ptr(),
                            flags | libc::UF_IMMUTABLE
                        ),
                        0
                    );
                }
            }
            saved
        }
    }
    #[cfg(target_os = "macos")]
    impl Drop for ReadOnlyJournal {
        fn drop(&mut self) {
            for (path, flags) in &self.0 {
                // SAFETY: restore this isolated fixture's exact original flags on every exit.
                unsafe {
                    assert_eq!(libc::chflags(path.as_ptr(), *flags), 0);
                }
            }
        }
    }
    with_lane(|lane| {
        let digest = install_pending(&lane, "1.0.0");
        let directory = lane.service.paths.data.join("installations").join(&digest);
        let (version, commit) = runtime::describe(&directory, &lane.service.identity).unwrap();
        let candidate = RetainedInstallation {
            directory,
            version,
            commit,
        };
        let now = Instant::now();
        let activity = Activity::new(now);
        let stopped = Arc::new(AtomicUsize::new(0));
        let sessions: Vec<Arc<dyn Session>> =
            vec![Arc::new(Idle(activity.clone(), stopped.clone()))];
        *lane.host.native_lease.lock().unwrap() = Some(
            Lease::scan(
                &sessions,
                now + Duration::from_secs(7200),
                Duration::from_secs(3600),
            )
            .unwrap(),
        );
        lane.host.fenced.store(true, Ordering::SeqCst);
        lane.lane.watching.store(true, Ordering::SeqCst);
        let mut record = Record {
            id: OperationId([42; 16]),
            actor: Actor::CORE,
            core: lane.lane.core,
            boot: lane.lane.boot,
            action: Action::Update,
            observation_digest: [0; 32],
            candidate: Some(digest_bytes(&digest).unwrap()),
            state: State::Admitted,
            updated_at: 0,
            orphaned: false,
        };
        let data = Directory::open(&lane.service.paths.data, false).unwrap();
        let helper = data.lock(HELPER_LOCK, false).unwrap();
        lane.lane.ledger().unwrap().admit(record.clone()).unwrap();
        record = lane
            .lane
            .ledger()
            .unwrap()
            .claim(&record.id)
            .unwrap()
            .unwrap();
        let calls = std::cell::Cell::new(0);
        let abandoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::super::helper::activate_pending(&lane.service, &candidate, &record, |_| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    panic!("helper abandoned its dispatch marker");
                }
                lane.host
                    .native_lease
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .freeze_and_grant()
                    .unwrap();
                Ok(())
            })
        }));
        assert!(abandoned.is_err());
        drop(helper);
        let retained =
            std::fs::read(lane.service.paths.data.join("service-operations.json")).unwrap();
        let mut previous = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: only this isolated child changes its file-size limit; existing records remain
        // readable, but an atomic settlement write exceeds one byte and fails with EFBIG.
        unsafe {
            assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut previous), 0);
            let handler = libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            assert_ne!(handler, libc::SIG_ERR);
            let _limit = WriteLimit(previous, handler);
            let limited = libc::rlimit {
                rlim_cur: 1,
                rlim_max: previous.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limited), 0);
            assert_eq!(lane.lane.settled(), Err(CoreError::StorageUnavailable));
            #[cfg(target_os = "macos")]
            let _read_only = ReadOnlyJournal::protect(&lane.service.paths.data);
            #[cfg(target_os = "macos")]
            assert!(
                data.read_json::<serde_json::Value>("service.json").is_err(),
                "the fixture must reject write-open"
            );

            assert!(
                !lane.lane.release_if_settled(),
                "the operation stays unresolved"
            );
            assert_eq!(
                stopped.load(Ordering::SeqCst),
                0,
                "storage failure cannot retain the native freeze"
            );
            assert!(
                activity.input(Instant::now()).is_ok(),
                "input resumes while writes still fail"
            );
            assert!(
                lane.host.fenced.load(Ordering::SeqCst),
                "membership stays fenced for the unresolved operation"
            );
            assert!(lane.lane.watching.load(Ordering::SeqCst));
            assert_eq!(
                lane.lane.settled(),
                Err(CoreError::StorageUnavailable),
                "storage has not been repaired"
            );
            assert_eq!(
                std::fs::read(lane.service.paths.data.join("service-operations.json")).unwrap(),
                retained
            );
            assert!(
                data.observe_json::<serde_json::Value>("service.json")
                    .unwrap()
                    .is_some()
            );
        }
    });
}

type LaneStream = crate::protocol::LocalPreamble<std::os::unix::net::UnixStream>;

fn lane_pair() -> std::io::Result<(LaneStream, LaneStream)> {
    let (client, core) = std::os::unix::net::UnixStream::pair()?;
    Ok((
        crate::protocol::LocalPreamble(client),
        crate::protocol::LocalPreamble(core),
    ))
}
