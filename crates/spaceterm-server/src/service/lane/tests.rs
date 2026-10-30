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
    fn check(&self, _: &Path, _: bool) -> Result<(), ServerError> {
        Ok(())
    }
    fn install(&self, _: &Path) -> Result<(), ServerError> {
        panic!("the lane never installs")
    }
    fn prepare_start(&self) -> Result<(), ServerError> {
        panic!("the lane never starts the server")
    }
    fn start(&self) -> Result<(), ServerError> {
        panic!("the lane never starts the server")
    }
    fn stop(&self) -> Result<(), ServerError> {
        panic!("only the helper stops the server")
    }
    fn stop_timeout(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn set_automatic_start(&self, _: bool) -> Result<(), ServerError> {
        panic!("the lane never changes automatic start")
    }
    fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
        panic!("the lane never uninstalls")
    }
    fn running(&self) -> Result<bool, ServerError> {
        Ok(true)
    }
}

#[derive(Default)]
struct Host {
    sessions: Mutex<Vec<Id>>,
    fenced: AtomicBool,
    stopping: AtomicBool,
    /// Runs once inside the next fence release, before the fence clears.
    releasing: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Told each time a fence is set.
    fencing: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}
impl LaneHost for Host {
    fn fence(
        &self,
        admit: &mut dyn FnMut(&[Id]) -> Result<(), ServerError>,
    ) -> Result<(), ServerError> {
        let sessions = self.sessions.lock().unwrap();
        admit(&sessions)?;
        self.fenced.store(true, Ordering::SeqCst);
        if let Some(fencing) = self.fencing.lock().unwrap().as_ref() {
            let _ = fencing.send(());
        }
        Ok(())
    }
    fn release_fence(&self) {
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
    fn devices(&self) -> Result<Vec<LaneDevice>, ServerError> {
        Ok(Vec::new())
    }
    fn network(&self) -> Result<LaneNetwork, ServerError> {
        Ok(LaneNetwork {
            enabled: false,
            bind: None,
            listening: false,
            addresses: Vec::new(),
        })
    }
}

struct NoFeed;
impl Source for NoFeed {
    fn latest(&self) -> Result<Option<feed::Signed>, ServerError> {
        Ok(None)
    }
    fn download(&self, _: &feed::Feed, _: &mut dyn Write) -> Result<(), ServerError> {
        panic!("no feed to download from")
    }
}

/// A lane over the server state at `paths`, whose service manager reports a running server and
/// whose helper launches do nothing.
pub(crate) fn serving(paths: &crate::host::Paths) -> Arc<LaneServer> {
    serving_with(paths, Arc::new(Host::default()))
}

/// [`serving`] with its own view of the running server.
pub(crate) fn serving_with(paths: &crate::host::Paths, host: Arc<dyn LaneHost>) -> Arc<LaneServer> {
    serving_with_feed(paths, host, Arc::new(NoFeed))
}

/// [`serving_with`] whose release feed is `feed`.
pub(crate) fn serving_with_feed(
    paths: &crate::host::Paths,
    host: Arc<dyn LaneHost>,
    feed: Arc<dyn Source>,
) -> Arc<LaneServer> {
    LaneServer::new(
        Service::new(paths.clone(), paths.identity.clone(), Arc::new(Running)),
        host,
        feed,
        Box::new(|_| Ok(())),
        Arc::new(crate::permissions::Unsupported),
    )
    .unwrap()
}

/// A release feed that takes `0` to answer and then has no release, as a slow download holds
/// the lane connection that asked for it.
pub(crate) struct SlowFeed(pub Duration);
impl Source for SlowFeed {
    fn latest(&self) -> Result<Option<feed::Signed>, ServerError> {
        std::thread::sleep(self.0);
        Ok(None)
    }
    fn download(&self, _: &feed::Feed, _: &mut dyn Write) -> Result<(), ServerError> {
        panic!("no feed to download from")
    }
}

const ACTOR: Actor = Actor {
    pairing: Some([7; 32]),
    client: [8; 32],
};

type Hook = Arc<Mutex<Option<Box<dyn Fn(&OperationId) + Send>>>>;

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "the client flow tests use every field")
)]
struct Lane {
    lane: Arc<LaneServer>,
    service: Service,
    host: Arc<Host>,
    launches: Arc<AtomicUsize>,
    /// Plays the helper when an operation launches.
    hook: Hook,
    archive: Vec<u8>,
    signature: Vec<u8>,
}

#[cfg_attr(
    not(feature = "client"),
    allow(dead_code, reason = "the client flow tests restart the server")
)]
impl Lane {
    /// A new run of the same server, as a service manager start creates it.
    fn restarted(&self) -> Arc<LaneServer> {
        LaneServer::new(
            self.service.clone(),
            self.host.clone(),
            Arc::new(NoFeed),
            Box::new(|_| Ok(())),
            Arc::new(crate::permissions::Unsupported),
        )
        .unwrap()
    }
}

/// A running fixture server with one approved signed installation.
fn with_lane(run: impl FnOnce(Lane)) {
    let fixture = super::super::tests::Fixture::new();
    with_artifact(
        &fixture.root.join("source"),
        &fixture.paths.identity,
        |archive, signature| {
            let service = Service::new(
                fixture.paths.clone(),
                fixture.paths.identity.clone(),
                Arc::new(Running),
            );
            let approved = service
                .provision(archive, signature, target(), COMMIT, VERSION)
                .unwrap();
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            let mut state = Service::state(&data).unwrap().unwrap();
            state.installation = approved
                .directory
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .into();
            data.write_json(c"service.json", &state).unwrap();
            let host = Arc::new(Host::default());
            let launches = Arc::new(AtomicUsize::new(0));
            let counted = launches.clone();
            let hook: Hook = Arc::default();
            let helper = hook.clone();
            let lane = LaneServer::new(
                service.clone(),
                host.clone(),
                Arc::new(NoFeed),
                Box::new(move |op| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    if let Some(hook) = helper.lock().unwrap().as_ref() {
                        hook(op);
                    }
                    Ok(())
                }),
                Arc::new(crate::permissions::Unsupported),
            )
            .unwrap();
            // A second package of the same build: a different gzip timestamp, re-signed.
            let mut bytes = std::fs::read(archive).unwrap();
            bytes[4] = 1;
            let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
            let signature = key.sign(&bytes).as_ref().to_vec();
            run(Lane {
                lane,
                service,
                host,
                launches,
                hook,
                archive: bytes,
                signature,
            });
        },
    );
}

fn code(reply: LaneReply) -> String {
    match reply {
        LaneReply::Error { code } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn preview(lane: &Arc<LaneServer>, action: LaneAction) -> (String, LaneEffects) {
    match lane.answer(LaneRequest::Preview { action }, &ACTOR) {
        LaneReply::Preview { token, effects, .. } => (token, effects),
        other => panic!("expected a preview, got {other:?}"),
    }
}

fn execute(lane: &Arc<LaneServer>, action: LaneAction, token: &str, op: u8) -> LaneReply {
    lane.answer(
        LaneRequest::Execute {
            action,
            token: token.to_owned(),
            operation: OperationId([op; 16]).to_string(),
        },
        &ACTOR,
    )
}

#[test]
fn lane_messages_keep_their_frozen_json_shape() {
    let request = LaneRequest::Execute {
        action: LaneAction::Update {
            installation: "abc".into(),
        },
        token: "c1-00".into(),
        operation: "op".into(),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "request": "execute",
            "action": { "action": "update", "installation": "abc" },
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
) -> Arc<LaneServer> {
    LaneServer::new(
        Service::new(paths.clone(), paths.identity.clone(), Arc::new(Running)),
        Arc::new(Host::default()),
        Arc::new(NoFeed),
        Box::new(|_| Ok(())),
        permissions,
    )
    .unwrap()
}

const LOCAL_ACTOR: Actor = Actor {
    pairing: None,
    client: [8; 32],
};

fn permission_requests() -> [LaneRequest; 5] {
    use crate::permissions::SystemPermission;
    [
        LaneRequest::Permissions,
        LaneRequest::RequestPermission {
            permission: SystemPermission::Microphone,
        },
        LaneRequest::OpenPermissionSettings {
            permission: SystemPermission::ScreenRecording,
        },
        LaneRequest::PreparePermissionSetup {
            permission: SystemPermission::Accessibility,
        },
        LaneRequest::ResetPermission {
            permission: SystemPermission::Accessibility,
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
fn a_local_caller_reads_and_changes_the_servers_own_permissions() {
    use crate::permissions::{
        PermissionState, RequestOutcome, SetupReadiness, SystemPermission,
        testing::ScriptedPermissions,
    };
    let fixture = super::super::tests::Fixture::new();
    let permissions = ScriptedPermissions::granting(PermissionState::NotGranted);
    let lane = serving_permissions(&fixture.paths, permissions.clone());
    let [read, request, open, prepare, reset] = permission_requests();
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
        lane.answer(open, &LOCAL_ACTOR),
        LaneReply::PermissionSettingsOpened
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
            ("open_settings", SystemPermission::ScreenRecording),
            ("prepare_setup", SystemPermission::Accessibility),
            ("reset", SystemPermission::Accessibility),
        ]
    );
}

#[test]
fn a_platform_without_system_permissions_answers_unsupported() {
    let fixture = super::super::tests::Fixture::new();
    let lane = serving_permissions(&fixture.paths, Arc::new(crate::permissions::Unsupported));
    for request in permission_requests() {
        assert_eq!(
            lane.answer(request, &LOCAL_ACTOR),
            LaneReply::Error {
                code: "unsupported".into()
            }
        );
    }
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
        let LaneReply::Status(status) = lane.lane.answer(LaneRequest::Status, &ACTOR) else {
            panic!("expected a status");
        };
        assert!(status.running && status.installed && !status.stopped);
        assert_eq!(status.runtime.version, VERSION);
        assert_eq!(status.runtime.commit, COMMIT);
        assert_eq!(status.sessions, 1);
        assert_eq!(status.activation, LaneActivation::Settled);
        assert_eq!(status.lane_major, LANE_MAJOR);
        assert!(status.staged.is_empty());
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
fn a_running_server_without_a_failed_activation_offers_no_return() {
    with_lane(|lane| {
        let reply = lane.lane.answer(
            LaneRequest::Preview {
                action: LaneAction::Return,
            },
            &ACTOR,
        );
        assert_eq!(code(reply), "return_unavailable");
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
                action: LaneAction::Stop,
            },
            &other,
        ) else {
            panic!("expected a preview");
        };
        let reply = lane.lane.answer(
            LaneRequest::Execute {
                action: LaneAction::Stop,
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
                server: lane.lane.server,
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
fn an_uploaded_candidate_is_staged_and_previewed_as_an_update() {
    with_lane(|lane| {
        let sha256: String = ring::digest::digest(&ring::digest::SHA256, &lane.archive)
            .as_ref()
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect();
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let LaneReply::UploadProgress {
            upload,
            next_offset,
        } = lane.lane.answer(
            LaneRequest::StageUpload {
                length: lane.archive.len() as u64,
                sha256: sha256.clone(),
                signature: encode(&lane.signature),
            },
            &ACTOR,
        )
        else {
            panic!("expected an upload");
        };
        assert_eq!(next_offset, 0);
        let chunk = |offset: usize, end: usize| {
            lane.lane.answer(
                LaneRequest::UploadChunk {
                    upload: upload.clone(),
                    offset: offset as u64,
                    data: encode(&lane.archive[offset..end]),
                },
                &ACTOR,
            )
        };
        let half = lane.archive.len() / 2;
        assert_eq!(
            chunk(0, half),
            LaneReply::UploadProgress {
                upload: upload.clone(),
                next_offset: half as u64,
            }
        );
        // A chunk out of order is refused without changing the upload.
        assert_eq!(code(chunk(0, half)), "protocol_error");
        let LaneReply::Staged {
            installation,
            version,
            ..
        } = chunk(half, lane.archive.len())
        else {
            panic!("expected a staged candidate");
        };
        assert_eq!(installation, sha256);
        assert_eq!(version, VERSION);
        let LaneReply::Check(check) = lane.lane.answer(LaneRequest::Check, &ACTOR) else {
            panic!("expected a check");
        };
        assert_eq!(check.latest, None);
        assert_eq!(check.staged.len(), 1);
        lane.host.sessions.lock().unwrap().push([3; 32]);
        let (token, effects) = preview(
            &lane.lane,
            LaneAction::Update {
                installation: installation.clone(),
            },
        );
        assert_eq!(effects.kind, Some(LaneUpdateKind::Update));
        assert_eq!(effects.to.as_ref().unwrap().sha256, installation);
        assert_eq!(effects.to_major, Some(crate::protocol::MAJOR));
        assert_eq!(effects.ends_sessions, 1);
        let reply = execute(&lane.lane, LaneAction::Update { installation }, &token, 1);
        assert!(matches!(reply, LaneReply::Accepted { .. }), "{reply:?}");
        let record = lane
            .lane
            .ledger()
            .unwrap()
            .resolve(&OperationId([1; 16]))
            .unwrap()
            .unwrap();
        assert_eq!(hex(&record.candidate.unwrap()), sha256);
    });
}

#[test]
fn a_service_lane_hello_reaches_the_lane_as_its_agent() {
    with_lane(|lane| {
        let (mut client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
        let identity = lane.lane.service.identity.clone();
        let server_id = lane.lane.server;
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
                &server_id,
                &identity,
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
        let admitted = crate::protocol::admit_server(
            &mut server,
            &lane.lane.server,
            &lane.lane.service.identity,
        )
        .unwrap();
        let crate::protocol::Admission::Service(actor) = admitted else {
            panic!("expected a service-lane connection");
        };
        assert_eq!(actor.client, derived);
        lane.lane
            .serve(&mut server, actor, None, &|| Ok(()))
            .unwrap();
        let (major, reply) = peer.join().unwrap();
        assert_eq!(major, crate::protocol::MAJOR);
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
fn an_unsettled_operation_never_keeps_a_stopping_server_alive() {
    with_lane(|lane| {
        let (token, _) = preview(&lane.lane, LaneAction::Stop);
        // The helper never settles this record, as while it waits for the server to exit.
        assert!(matches!(
            execute(&lane.lane, LaneAction::Stop, &token, 1),
            LaneReply::Accepted { .. }
        ));
        let Lane {
            lane: server, host, ..
        } = lane;
        assert!(host.fenced.load(Ordering::SeqCst));
        // The server's shutdown drops its lane. The fence watcher must not hold the host.
        drop(server);
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
fn a_stopping_server_closes_a_polling_lane_connection() {
    with_lane(|lane| {
        let (mut client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
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
            .serve(&mut server, ACTOR, None, &|| Ok(()))
            .unwrap();
        drop(server);
        assert!(peer.join().unwrap(), "the client saw the connection close");
        assert!(started.elapsed() < Duration::from_secs(5));
    });
}

#[test]
fn the_lane_answers_until_the_peer_closes_and_refuses_malformed_requests() {
    with_lane(|lane| {
        let (mut client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
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
            .serve(&mut server, ACTOR, None, &|| Ok(()))
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
    use crate::service::lane::client::{self, Connector, Leftover, Outcome, Session};
    use std::os::unix::net::UnixStream;

    /// Connects to whichever run of the server is up. `None` means no server runs.
    #[derive(Default)]
    struct Switch {
        lane: Mutex<Option<Arc<LaneServer>>>,
        open: Mutex<Vec<UnixStream>>,
    }
    impl Switch {
        fn up(&self, lane: Arc<LaneServer>) {
            *self.lane.lock().unwrap() = Some(lane);
        }
        /// The server exits: its lane connections close and no new one opens.
        fn down(&self) {
            *self.lane.lock().unwrap() = None;
            for stream in self.open.lock().unwrap().drain(..) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
    impl Connector for Switch {
        fn connect(&self, _: Instant) -> Result<Box<dyn Preamble + Send>, ServerError> {
            let lane = self
                .lane
                .lock()
                .unwrap()
                .clone()
                .ok_or(ServerError::NotRunning)?;
            let (client, mut server) = UnixStream::pair().map_err(|_| ServerError::Protocol)?;
            self.open
                .lock()
                .unwrap()
                .push(server.try_clone().map_err(|_| ServerError::Protocol)?);
            // The fixture's signing trust is per thread, so the serving thread holds it too.
            std::thread::spawn(move || {
                crate::service::runtime::test_support::with_fixture_trust(|| {
                    lane.serve(&mut server, ACTOR, None, &|| Ok(()))
                })
            });
            Ok(Box::new(client))
        }
    }

    /// The helper's lock, held from before its claim until it records an outcome.
    fn helper_lock(lane: &LaneServer) -> crate::storage::Lock {
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
        let status = || Err::<(), _>(ServerError::Changed);
        // Quick work reports its own result, with a deadline or without.
        assert_eq!(
            within("test", &|| None, status),
            Some(Err(ServerError::Changed))
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
    fn a_restart_settles_after_the_server_returns_with_a_new_boot() {
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
                    let new = LaneServer::new(
                        service,
                        host,
                        Arc::new(NoFeed),
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
                Duration::from_secs(20),
                &mut |op| {
                    admitted = Some(*op);
                    Ok(())
                },
                &|| None,
            )
            .unwrap();
            assert!(matches!(outcome, Outcome::Shutdown { .. }), "{outcome:?}");
            // A local Start brings the server back, and the next lane connection settles it.
            switch.up(lane.restarted());
            let mut session =
                Session::open(switch.as_ref(), Instant::now() + Duration::from_secs(5)).unwrap();
            let leftover = client::settle_leftover(&mut session, &admitted.unwrap()).unwrap();
            let Leftover::Settled(receipt) = leftover else {
                panic!("expected a settled stop, got {leftover:?}");
            };
            assert_eq!(receipt.state, LaneOperationState::Done);
            assert_eq!(
                client::settle_leftover(&mut session, &admitted.unwrap()).unwrap(),
                Leftover::Gone
            );
        });
    }

    #[test]
    fn a_helperailure_settles_without_waiting_for_a_disconnect() {
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
        lane: Mutex<Option<Arc<LaneServer>>>,
        lose: Arc<AtomicBool>,
    }
    impl Connector for Lossy {
        fn connect(&self, _: Instant) -> Result<Box<dyn Preamble + Send>, ServerError> {
            let lane = self
                .lane
                .lock()
                .unwrap()
                .clone()
                .ok_or(ServerError::NotRunning)?;
            let (client, mut server) = UnixStream::pair().map_err(|_| ServerError::Protocol)?;
            let lose = self.lose.clone();
            std::thread::spawn(move || {
                loop {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let Ok(bytes) = server.receive(MAX_FRAME, deadline) else {
                        return;
                    };
                    let request: LaneRequest = serde_json::from_slice(&bytes).unwrap();
                    let execute = matches!(request, LaneRequest::Execute { .. });
                    let reply = lane.answer(request, &ACTOR);
                    if execute && lose.swap(false, Ordering::SeqCst) {
                        let _ = server.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    let _ = server.send(&serde_json::to_vec(&reply).unwrap(), deadline);
                }
            });
            Ok(Box::new(client))
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
        lane: Mutex<Option<Arc<LaneServer>>>,
        stalled: Arc<AtomicBool>,
    }
    impl Connector for Stalling {
        fn connect(&self, _: Instant) -> Result<Box<dyn Preamble + Send>, ServerError> {
            let lane = self.lane.lock().unwrap().clone().unwrap();
            let (client, mut server) = UnixStream::pair().map_err(|_| ServerError::Protocol)?;
            let stalled = self.stalled.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(60);
                while let Ok(bytes) = server.receive(MAX_FRAME, deadline) {
                    if stalled.load(Ordering::SeqCst) {
                        continue;
                    }
                    let request: LaneRequest = serde_json::from_slice(&bytes).unwrap();
                    let execute = matches!(request, LaneRequest::Execute { .. });
                    let reply = lane.answer(request, &ACTOR);
                    if execute {
                        stalled.store(true, Ordering::SeqCst);
                        continue;
                    }
                    let _ = server.send(&serde_json::to_vec(&reply).unwrap(), deadline);
                }
            });
            Ok(Box::new(client))
        }
    }

    #[test]
    fn an_interrupt_ends_a_request_a_stalled_server_holds_within_the_grace() {
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
    fn a_stop_whose_accepted_reply_was_lost_reports_shutdown_when_the_server_is_gone() {
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
        crate::service::lane::admin::PendingOperations::new(
            Directory::open(&lane.service.paths.data.join("client-pending"), true).unwrap(),
        )
    }

    /// A signed release feed naming the fixture's second package of the same build.
    struct Release {
        document: Vec<u8>,
        signature: Vec<u8>,
        archive: Vec<u8>,
    }

    impl Release {
        fn of(lane: &Lane) -> Self {
            #[cfg(target_os = "macos")]
            const PLATFORM: &str = "darwin-arm64";
            #[cfg(target_os = "linux")]
            const PLATFORM: &str = "linux-x86_64";
            let sha256 = hex(ring::digest::digest(&ring::digest::SHA256, &lane.archive).as_ref());
            let document = serde_json::json!({
                "schema": 1,
                "identity": lane.service.identity,
                "target": target(),
                "version": VERSION,
                "published_at": 1,
                "archive": {
                    "name": format!("spaceterm-{VERSION}-{PLATFORM}.tar.gz"),
                    "size": lane.archive.len(),
                    "sha256": sha256,
                    "signature": base64::engine::general_purpose::STANDARD.encode(&lane.signature),
                },
            })
            .to_string()
            .into_bytes();
            let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
            let signature = key.sign(&document).as_ref().to_vec();
            Self {
                document,
                signature,
                archive: lane.archive.clone(),
            }
        }
    }

    impl Source for Release {
        fn latest(&self) -> Result<Option<feed::Signed>, ServerError> {
            Ok(Some((self.document.clone(), self.signature.clone())))
        }
        fn download(&self, _: &feed::Feed, into: &mut dyn Write) -> Result<(), ServerError> {
            into.write_all(&self.archive)
                .map_err(|_| ServerError::StorageUnavailable)
        }
    }

    /// The desktop's and the CLI's Update: no return applies, so the server stages its newest
    /// signed release and previews exactly that candidate.
    #[test]
    fn planning_an_update_stages_the_servers_signed_release_and_previews_it() {
        use crate::service::lane::admin::{self, UpdatePlan};
        with_lane(|lane| {
            let served = LaneServer::new(
                lane.service.clone(),
                lane.host.clone(),
                Arc::new(Release::of(&lane)),
                Box::new(|_| Ok(())),
                Arc::new(crate::permissions::Unsupported),
            )
            .unwrap();
            let switch = Arc::new(Switch::default());
            switch.up(served);
            let pending = pending_record(&lane);
            let (mut session, settled) =
                admin::open(&client::Lane::over(switch.clone()), &pending).unwrap();
            assert!(settled.is_empty());
            let check = admin::check(&mut session).unwrap();
            let latest = check.latest.clone().expect("the feed names a release");
            assert_ne!(latest.sha256, check.approved.sha256);
            assert!(check.staged.is_empty(), "the check stages nothing");

            let plan = admin::plan_update(&mut session).unwrap();
            let UpdatePlan::Update { candidate, preview } = plan else {
                panic!("expected an update, got {plan:?}");
            };
            assert_eq!(candidate.sha256, latest.sha256);
            assert_eq!(candidate.version, VERSION);
            assert_eq!(
                preview.action,
                LaneAction::Update {
                    installation: latest.sha256.clone(),
                }
            );
            assert_eq!(preview.effects.kind, Some(LaneUpdateKind::Update));
            assert_eq!(preview.effects.to.as_ref().unwrap().sha256, latest.sha256);
            assert!(preview.token.starts_with("c1-"));
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
            let (mut session, _) = admin::open(&route, &pending).unwrap();
            let preview = admin::preview(&mut session, LaneAction::Restart).unwrap();
            let outcome = admin::execute(
                &route,
                session,
                LaneAction::Restart,
                preview.token,
                Duration::from_secs(3),
                &pending,
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
            let (_, settled) = admin::open(&route, &pending).unwrap();
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
}
