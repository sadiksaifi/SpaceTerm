use super::*;
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry};
use std::sync::Arc;

struct TestPackage;
impl PackagedExecutable for TestPackage {
    fn publish(
        &self,
        transport: &mut dyn ObservationTransport,
        proof: LaunchProof,
    ) -> Result<(), AcceptanceObservationError> {
        write_frame(
            transport,
            format!("{}{}", proof.prefix(), proof.suffix()).as_bytes(),
        )
        .map_err(Into::into)
    }
}

struct MemoryTransport {
    sender: Option<mpsc::Sender<Vec<u8>>>,
    receiver: mpsc::Receiver<Vec<u8>>,
    buffered: std::collections::VecDeque<u8>,
    timeout: Mutex<Duration>,
}
impl MemoryTransport {
    fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = mpsc::channel();
        let (c, d) = mpsc::channel();
        Ok((
            Self {
                sender: Some(a),
                receiver: d,
                buffered: Default::default(),
                timeout: Mutex::new(Duration::from_secs(2)),
            },
            Self {
                sender: Some(c),
                receiver: b,
                buffered: Default::default(),
                timeout: Mutex::new(Duration::from_secs(2)),
            },
        ))
    }
}
impl Read for MemoryTransport {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.buffered.is_empty() {
            match self.receiver.recv_timeout(*self.timeout.lock().unwrap()) {
                Ok(bytes) => self.buffered.extend(bytes),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let count = output.len().min(self.buffered.len());
        for byte in &mut output[..count] {
            *byte = self.buffered.pop_front().unwrap();
        }
        Ok(count)
    }
}
impl Write for MemoryTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.sender
            .as_ref()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .send(bytes.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl ObservationTransport for MemoryTransport {
    fn set_read_timeout(&self, value: Option<Duration>) -> Result<(), AcceptanceObservationError> {
        *self.timeout.lock().unwrap() = value.unwrap_or(Duration::from_secs(2));
        Ok(())
    }
    fn set_write_timeout(&self, _: Option<Duration>) -> Result<(), AcceptanceObservationError> {
        Ok(())
    }
    fn close(&self) -> Result<(), AcceptanceObservationError> {
        Ok(())
    }
}
fn finish_runtime_writer_slot(
    slot: &Mutex<Option<RuntimeWriter>>,
) -> Result<(), AcceptanceObservationError> {
    let Some(writer) = slot.lock().unwrap().take() else {
        return Ok(());
    };
    finish_runtime_writer(writer)
}
fn request(stream: MemoryTransport) -> ObservationRequest {
    let (failure_action_sender, failure_action_receiver) = async_channel::bounded(1);
    let (failure_result_sender, failure_result_receiver) = mpsc::sync_channel(8);
    ObservationRequest {
        stream: Box::new(stream),
        nonce: "a".repeat(64),
        run_id: "i43-proof".to_owned(),
        app_sha256: "b".repeat(64),
        initial: None,
        runtime: RuntimeObservation::new(),
        package: Box::new(TestPackage),
        failure_actions_enabled: true,
        failure_action_sender: Some(failure_action_sender),
        failure_action_receiver: Some(failure_action_receiver),
        failure_result_sender: Some(failure_result_sender),
        failure_result_receiver: Some(failure_result_receiver),
    }
}

fn running_observation() -> RuntimeObservation {
    let observation = RuntimeObservation::new();
    observation.worker_started(TerminalGeometry::from_grid(
        CellGridSize::new(80, 24),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::new(2.0).unwrap(),
    ));
    observation
}

fn runtime_writer_fixture(
    observation: RuntimeObservation,
) -> (Arc<Mutex<Option<RuntimeWriter>>>, MemoryTransport) {
    let (writer_stream, peer) = MemoryTransport::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let writer = spawn_runtime_writer(
        Box::new(writer_stream),
        observation,
        FailureTransport {
            nonce: "a".repeat(64),
            run_id: "i43-proof".to_owned(),
            app_sha256: "b".repeat(64),
            requests: None,
            results: None,
        },
    )
    .unwrap();
    (Arc::new(Mutex::new(Some(writer))), peer)
}

fn read_text_frame(peer: &mut MemoryTransport) -> String {
    String::from_utf8(read_frame(peer).unwrap()).unwrap()
}

fn accept_runtime_closure(peer: &mut MemoryTransport) -> Vec<String> {
    let mut frames = Vec::new();
    loop {
        let frame = read_text_frame(peer);
        let complete = frame.starts_with(&format!("schema\t{RUNTIME_COMPLETE_SCHEMA}\n"));
        frames.push(frame);
        if complete {
            break;
        }
    }
    write_frame(
        peer,
        format!("schema\t{RUNTIME_ACK_SCHEMA}\nstatus\taccepted\n").as_bytes(),
    )
    .unwrap();
    assert_eq!(
        read_text_frame(peer),
        format!("schema\t{RUNTIME_CLOSED_SCHEMA}\nstatus\tconfirmed\n")
    );
    frames
}

#[test]
fn normal_app_quit_should_close_the_runtime_stream_before_process_teardown() {
    let (slot, mut peer) = runtime_writer_fixture(running_observation());
    let initial = read_text_frame(&mut peer);
    assert!(initial.contains("\trunning\t0\n"));

    let finalizer_slot = Arc::clone(&slot);
    let finalizer = thread::spawn(move || finish_runtime_writer_slot(&finalizer_slot));
    let frames = accept_runtime_closure(&mut peer);
    let final_tick = frames
        .iter()
        .find(|frame| frame.starts_with(&format!("schema\t{RUNTIME_TICK_SCHEMA}\n")))
        .expect("normal quit must emit a final tick");
    assert!(final_tick.contains("\texited\t0\n"));
    assert!(final_tick.contains("\tsession-exited\t"));
    let complete = frames.last().unwrap();
    assert_eq!(complete.lines().count(), 6);
    assert!(complete.contains("observer.status\tcomplete\n"));
    assert!(finalizer.join().unwrap().is_ok());

    let mut trailing = [0_u8; 1];
    assert_eq!(peer.read(&mut trailing).unwrap(), 0);
}

#[test]
fn duplicate_runtime_finalization_should_be_inert() {
    let (slot, mut peer) = runtime_writer_fixture(running_observation());
    let _ = read_text_frame(&mut peer);
    let finalizer_slot = Arc::clone(&slot);
    let finalizer = thread::spawn(move || finish_runtime_writer_slot(&finalizer_slot));
    let _ = accept_runtime_closure(&mut peer);
    assert!(finalizer.join().unwrap().is_ok());
    assert!(finish_runtime_writer_slot(&slot).is_ok());
}

#[test]
fn forced_terminal_exit_should_not_be_reclassified_by_app_quit() {
    let observation = running_observation();
    let (slot, mut peer) = runtime_writer_fixture(observation.clone());
    let _ = read_text_frame(&mut peer);
    observation.session_exited(5);

    let finalizer_slot = Arc::clone(&slot);
    let finalizer = thread::spawn(move || finish_runtime_writer_slot(&finalizer_slot));
    let frames = accept_runtime_closure(&mut peer);
    let final_tick = frames
        .iter()
        .find(|frame| frame.starts_with(&format!("schema\t{RUNTIME_TICK_SCHEMA}\n")))
        .expect("forced exit must emit a final tick");
    assert!(final_tick.contains("\texited\t0\n"));
    assert!(final_tick.contains("\tsession-exited\t"));
    assert!(final_tick.lines().any(|line| line.ends_with("\t5\t0")));
    assert!(finalizer.join().unwrap().is_ok());
}

#[test]
fn disconnected_verifier_should_fail_finalization_without_a_duplicate_attempt() {
    let (slot, mut peer) = runtime_writer_fixture(running_observation());
    let _ = read_text_frame(&mut peer);
    drop(peer);

    assert!(finish_runtime_writer_slot(&slot).is_err());
    assert!(finish_runtime_writer_slot(&slot).is_ok());
}

#[test]
fn challenge_should_be_exact_and_bounded() {
    let challenge = format!(
        "schema\t{CHALLENGE_SCHEMA}\nlaunch.nonce\t{}\nrun.id\ti43-proof\npackage.app.sha256\t{}\nruntime.schema\t{RUNTIME_SCHEMA}\nruntime.sample_interval_ms\t1000\nruntime.transition_capacity\t64\nfailure.action.schema\t{FAILURE_ACTION_SCHEMA}\nfailure.action.enabled\ttrue\n",
        "a".repeat(64),
        "b".repeat(64),
    );
    let LaunchAuthentication {
        run_id,
        failure_actions_enabled: enabled,
        ..
    } = parse_challenge(challenge.as_bytes()).unwrap();
    assert_eq!(run_id, "i43-proof");
    assert!(enabled);
    let disabled = challenge.replace(
        "failure.action.enabled\ttrue",
        "failure.action.enabled\tfalse",
    );
    assert!(
        !parse_challenge(disabled.as_bytes())
            .unwrap()
            .failure_actions_enabled
    );
    let disabled_channels = failure_channels(false);
    assert!(disabled_channels.0.is_none());
    assert!(disabled_channels.1.is_none());
    assert!(disabled_channels.2.is_none());
    assert!(disabled_channels.3.is_none());

    assert!(parse_challenge(format!("{challenge}extra\ttrue\n").as_bytes()).is_err());
    assert!(parse_challenge(challenge.trim_end().as_bytes()).is_err());
    assert!(parse_challenge(challenge.replace("run.id", "invalid").as_bytes()).is_err());
}

#[test]
fn observation_should_bind_runtime_facts_without_terminal_content() {
    let (stream, _peer) = MemoryTransport::pair().unwrap();
    let record = format_observation(
        &request(stream),
        "JetBrainsMono Nerd Font",
        ObservationGeometry {
            rows: 24,
            columns: 80,
            logical_width: 800.5,
            logical_height: 480.0,
            backing_pixel_width: 1601,
            backing_pixel_height: 960,
        },
    );

    let record = format!("{}{}", record.prefix(), record.suffix());
    assert!(record.contains("observation.source\tproduction-app\n"));
    assert!(record.contains("initial_grid.logical_width\t800.5\n"));
    assert!(record.contains("failure.action.schema\tspaceterm.acceptance.failure-action/v1\n"));
    assert!(record.contains("observation.complete\ttrue\n"));
    assert!(!record.contains("terminal.content"));
}

#[test]
fn runtime_tick_should_have_the_exact_content_free_schema() {
    let sample = RuntimeSample {
        continuous_ns: 1,
        worker_generation: 2,
        screens_published: 3,
        screens_enqueued: 4,
        screens_superseded: 5,
        event_queue_length: 1,
        event_queue_high_water: 2,
        ui_dispatches: 6,
        ui_screen_events: 7,
        ui_drain_high_water: 2,
        ui_latest_generation: 8,
        render_latest_generation: 9,
        next_frame_generation: 10,
        next_frame_count: 11,
        presentable: true,
        minimized: false,
        occluded: false,
        workspace_visible: true,
        pane_visible: true,
        live_resize: false,
        viewport_total_rows: 12,
        viewport_visible_rows: 13,
        viewport_offset_rows: 14,
        selection_present: true,
        resize_requests: 15,
        resize_notifications: 16,
        resize_applied: 17,
        resize_coalesced: 18,
        pty_rows: 19,
        pty_columns: 20,
        pty_pixel_width: 21,
        pty_pixel_height: 22,
        terminal_inputs_accepted: 23,
        lifecycle: RuntimeLifecycle::Running,
        observer_drops: 0,
    };
    let transition = RuntimeTransition {
        sequence: 0,
        continuous_ns: 24,
        kind: crate::terminal::RuntimeEventKind::VisibilityRestored,
        generation: 25,
        aux0: 0,
        aux1: 0,
    };

    assert_eq!(
        format_runtime_tick(0, sample, &[transition]),
        "schema\tspaceterm.acceptance.runtime-tick/v1\nsequence\t0\nevent_count\t1\nsample\t1\t2\t3\t4\t5\t1\t2\t6\t7\t2\t8\t9\t10\t11\t1\t0\t0\t1\t1\t0\t12\t13\t14\t1\t15\t16\t17\t18\t19\t20\t21\t22\t23\trunning\t0\nevent\t0\t24\tvisibility-restored\t25\t0\t0\n"
    );
}

#[test]
fn runtime_tick_types_cannot_carry_terminal_strings() {
    assert!(std::mem::size_of::<RuntimeSample>() < 512);
    assert!(std::mem::size_of::<RuntimeTransition>() < 128);
    let observation = RuntimeObservation::new();
    let sample = observation.sample();
    let canaries = [
        "terminal canary",
        "title canary",
        "/private/path/canary",
        "clipboard canary",
        "key canary",
        "https://canary.invalid",
    ];
    let tick = format_runtime_tick(0, sample, &[]);
    for canary in canaries {
        assert!(!tick.contains(canary));
    }
}

#[test]
fn value_encoding_should_reject_noncanonical_escapes() {
    assert_eq!(decode_value("100%25%09ok").unwrap(), "100%\tok");
    assert!(decode_value("%2f").is_err());
    assert!(decode_value("%").is_err());
}

#[test]
fn failure_action_should_require_exact_authentication_order_and_one_shot_sequence() {
    let nonce = "a".repeat(64);
    let app_sha256 = "b".repeat(64);
    let request_id = "c".repeat(64);
    let frame = format!(
        "schema\t{FAILURE_ACTION_SCHEMA}\nlaunch.nonce\t{nonce}\nrun.id\ti43-proof\npackage.app.sha256\t{app_sha256}\nrequest.id\t{request_id}\nsequence\t0\ncase.id\tpresentation-glyph\nrequest.once\ttrue\n"
    );
    let request =
        parse_failure_action(frame.as_bytes(), &nonce, "i43-proof", &app_sha256, 0).unwrap();
    assert_eq!(request.case, FailureActionCase::PresentationGlyph);
    assert!(parse_failure_action(frame.as_bytes(), &nonce, "i43-proof", &app_sha256, 1).is_err());
    assert!(
        parse_failure_action(
            frame
                .replace("request.once\ttrue", "request.once\tfalse")
                .as_bytes(),
            &nonce,
            "i43-proof",
            &app_sha256,
            0,
        )
        .is_err()
    );
    assert!(
        parse_failure_action(
            frame
                .replace("presentation-glyph", "arbitrary-failure")
                .as_bytes(),
            &nonce,
            "i43-proof",
            &app_sha256,
            0,
        )
        .is_err()
    );
}

#[test]
fn failure_result_schema_should_be_content_free_and_closed() {
    let event = FailureActionEvent {
        request: FailureActionRequest {
            id: "c".repeat(64),
            sequence: 0,
            case: FailureActionCase::PasteboardWrite,
        },
        phase: FailureActionPhase::Injected,
        result: FailureActionResult::FailedState,
        pane_identity: 7,
        pane_state: FailurePaneState::Failed,
        failure_class: Some(FailureClass::Platform),
        recoverability: Some(Recoverability::Recoverable),
        failure_operation: Some("write-selection-pasteboard"),
        state_revision: 2,
        latest_generation: 9,
        last_valid_generation: 8,
        visible_generation: Some(8),
        pending_recovery: FailurePendingRecovery::CopySelection,
        terminal_input_usable: true,
        session_attached: true,
        resource_staged_count: 0,
        resource_staged_bytes: 0,
        resource_rolled_back_count: 0,
        resource_rolled_back_bytes: 0,
    };
    let result = format_failure_action_result(&event);
    assert!(result.starts_with("schema\tspaceterm.acceptance.failure-action-result/v2\n"));
    assert!(result.contains("failure.class\tplatform\n"));
    for canary in [
        "terminal canary",
        "clipboard canary",
        "/private/path/canary",
        "environment canary",
    ] {
        assert!(!result.contains(canary));
    }
}

fn challenge() -> String {
    format!(
        "schema\t{CHALLENGE_SCHEMA}\nlaunch.nonce\t{}\nrun.id\ti43-proof\npackage.app.sha256\t{}\nruntime.schema\t{RUNTIME_SCHEMA}\nruntime.sample_interval_ms\t1000\nruntime.transition_capacity\t64\nfailure.action.schema\t{FAILURE_ACTION_SCHEMA}\nfailure.action.enabled\ttrue\n",
        "a".repeat(64),
        "b".repeat(64)
    )
}
fn configured() -> (AuthenticatedObservation, MemoryTransport) {
    let (stream, peer) = MemoryTransport::pair().unwrap();
    let authentication = parse_challenge(challenge().as_bytes()).unwrap();
    (
        AuthenticatedObservation::configure(
            Box::new(stream),
            authentication,
            Box::new(TestPackage),
            Arc::new(TestClock::default()),
        )
        .unwrap(),
        peer,
    )
}
fn geometry() -> ObservationGeometry {
    ObservationGeometry {
        rows: 24,
        columns: 80,
        logical_width: 800.0,
        logical_height: 480.0,
        backing_pixel_width: 800,
        backing_pixel_height: 480,
    }
}
#[test]
fn owner_should_bind_one_pane_and_one_consumed_session_lease() {
    let (owner, _peer) = configured();
    let claim = owner.claim_session("test-font", geometry()).unwrap();
    assert!(owner.claim_session("test-font", geometry()).is_none());
    let runtime = claim.session.consume().unwrap();
    runtime.screen_published(4);
    assert_eq!(claim.runtime.sample().worker_generation, 4);
    assert!(claim.lease.prepare_once(23, 80).is_none());
    drop(claim.lease);
    assert!(owner.claim_session("test-font", geometry()).is_none());
}
#[test]
fn dropped_and_finished_owners_should_revoke_retained_claims_without_affecting_successors() {
    let (owner, _peer) = configured();
    let claim = owner.claim_session("test-font", geometry()).unwrap();
    drop(owner);
    assert!(claim.session.consume().is_none());
    assert!(claim.lease.prepare_once(24, 80).is_none());
    let (successor, _peer) = configured();
    assert!(successor.claim_session("test-font", geometry()).is_some());
}
#[test]
fn unconsumed_session_and_missing_first_frame_should_fail_closed() {
    let (owner, _peer) = configured();
    let claim = owner.claim_session("test-font", geometry()).unwrap();
    drop(claim.session);
    assert!(claim.runtime.is_failed());
    assert!(owner.finish().is_err());
    assert!(owner.finish().is_err());
    assert!(claim.lease.prepare_once(24, 80).is_none());
}
#[test]
fn scoped_writer_should_publish_once_and_preserve_completion_on_fallback() {
    let (owner, mut peer) = configured();
    let claim = owner.claim_session("test-font", geometry()).unwrap();
    let runtime = claim.session.consume().unwrap();
    runtime.worker_started(TerminalGeometry::from_grid(
        CellGridSize::new(80, 24),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::new(1.0).unwrap(),
    ));
    claim.lease.prepare_once(24, 80).unwrap().emit().unwrap();
    assert!(claim.lease.prepare_once(24, 80).is_none());
    assert!(read_text_frame(&mut peer).starts_with(&format!("schema\t{OBSERVATION_SCHEMA}\n")));
    let finalizer_owner = owner.clone();
    let finalizer = thread::spawn(move || finalizer_owner.finish());
    let frames = accept_runtime_closure(&mut peer);
    assert!(
        frames
            .last()
            .unwrap()
            .contains("observer.status\tcomplete\n")
    );
    assert!(finalizer.join().unwrap().is_ok());
    assert!(owner.finish().is_ok());
    assert!(!runtime.is_failed());
    runtime.screen_published(999);
    assert_ne!(runtime.sample().worker_generation, 999);
}
#[test]
fn protocol_should_reject_oversized_invalid_utf8_duplicate_and_unknown_fields() {
    for bytes in [
        vec![b'x'; MAX_FRAME_BYTES + 1],
        vec![0xff],
        challenge().replace("run.id", "launch.nonce").into_bytes(),
        challenge()
            .replace("runtime.schema", "unknown")
            .into_bytes(),
        challenge().replace("/v5", "/v99").into_bytes(),
    ] {
        assert!(parse_challenge(&bytes).is_err());
    }
}
#[test]
fn action_debug_should_not_disclose_request_authentication() {
    let request = FailureActionRequest {
        id: "SECRET-REQUEST-CANARY".to_owned(),
        sequence: 0,
        case: FailureActionCase::PtyFatal,
    };
    assert!(!format!("{request:?}").contains("SECRET"));
}
#[test]
fn frame_reader_should_reassemble_partial_operations_and_reject_oversized_prefix() {
    let (mut reader, mut peer) = MemoryTransport::pair().unwrap();
    for byte in [0, 0, 0, 3, b'a', b'b', b'c'] {
        peer.write_all(&[byte]).unwrap();
    }
    assert_eq!(read_frame(&mut reader).unwrap(), b"abc");
    peer.write_all(&(MAX_FRAME_BYTES as u32 + 1).to_be_bytes())
        .unwrap();
    assert!(read_frame(&mut reader).is_err());
}
#[test]
fn failure_authority_should_reject_overlap_unknown_receipts_and_identity_changes() {
    let request = FailureActionRequest {
        id: "a".repeat(64),
        sequence: 0,
        case: FailureActionCase::PtyFatal,
    };
    let mut authority = FailureAuthority::default();
    authority.request(request.clone()).unwrap();
    assert!(authority.request(request).is_err());
}
#[test]
fn portable_production_observation_should_not_reintroduce_native_mechanics_or_global_handoffs() {
    for source in [
        include_str!("../observation.rs"),
        include_str!("../app.rs"),
        include_str!("../ui/terminal_pane.rs"),
        include_str!("../terminal/session.rs"),
        include_str!("../terminal/runtime_observation.rs"),
    ] {
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in [
            "UnixStream",
            "std::os::unix",
            "AsRawFd",
            "mach_continuous_time",
            "mach_timebase_info",
            "fstatfs",
            "MNT_RDONLY",
            "fcntl",
            "SpaceTerm.app/",
            "platform::acceptance_observation",
            "static REQUEST",
            "static RUNTIME_WRITER",
            "take_runtime_session_observation",
        ] {
            assert!(
                !production.contains(forbidden),
                "shared observation contains {forbidden}"
            );
        }
    }
}

#[test]
fn writer_cadence_should_use_absolute_deadlines_and_reject_backward_time() {
    let start = Instant::now();
    let mut cadence = WriterCadence::new(start);
    assert_eq!(cadence.poll(start).unwrap(), Some(false));
    assert_eq!(cadence.poll(start).unwrap(), None);
    assert_eq!(
        cadence.poll(start + Duration::from_millis(999)).unwrap(),
        None
    );
    assert_eq!(
        cadence.poll(start + Duration::from_millis(1100)).unwrap(),
        Some(false)
    );
    assert_eq!(cadence.deadline, start + Duration::from_secs(2));
    assert_eq!(
        cadence.poll(start + Duration::from_millis(2300)).unwrap(),
        Some(true)
    );
    assert!(cadence.poll(start).is_err());
}
#[derive(Debug)]
struct ControlledClock(std::sync::atomic::AtomicU64);
impl ContinuousClock for ControlledClock {
    fn now_ns(&self) -> Option<u64> {
        let value = self.0.load(Ordering::Acquire);
        (value != u64::MAX).then_some(value)
    }
}
#[test]
fn injected_clock_should_accept_equal_time_and_fail_on_backward_or_unavailable_time() {
    for failed_value in [9, u64::MAX] {
        let clock = Arc::new(ControlledClock(std::sync::atomic::AtomicU64::new(10)));
        let observation = RuntimeObservation::with_clock(clock.clone());
        assert_eq!(observation.sample().continuous_ns, 10);
        assert_eq!(observation.sample().continuous_ns, 10);
        assert!(!observation.is_failed());
        clock.0.store(failed_value, Ordering::Release);
        observation.sample();
        assert!(observation.is_failed());
    }
}
#[test]
fn expired_frame_deadline_should_refuse_even_an_available_frame() {
    let (mut reader, mut writer) = MemoryTransport::pair().unwrap();
    write_frame(&mut writer, b"available").unwrap();
    assert!(read_frame_before(&mut reader, Instant::now() - Duration::from_secs(1)).is_err());
}
