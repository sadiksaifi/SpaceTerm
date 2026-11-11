use super::*;

fn fill_local_admission(fixture: &Fixture) -> (ChildGuard, Vec<UnixStream>) {
    let child = fixture.start();
    let mut held = Vec::new();
    for _ in 0..64 {
        let mut stream = connect_when_ready(fixture);
        protocol::admit_service_lane(
            &mut stream,
            (&fixture.device, &"Capacity test".into()).into(),
            &fixture.pin,
            "spaceterm-development",
        )
        .unwrap();
        held.push(stream);
    }
    (child, held)
}

#[test]
fn local_capacity_waits_with_cancellation_and_admits_when_a_slot_returns() {
    let fixture = Fixture::new();
    let (_child, mut held) = fill_local_admission(&fixture);
    let core = crate::MachineConnection::with_paths(fixture.paths.clone()).unwrap();
    let (answer, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        answer.send(core.refresh_cancellable(&|| false)).unwrap();
    });
    assert!(
        matches!(
            result.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "full local admission is temporary, not a protocol failure"
    );
    drop(held.pop());
    assert!(result.recv_timeout(Duration::from_secs(3)).unwrap().is_ok());
    worker.join().unwrap();
}

#[test]
fn local_capacity_wait_can_be_cancelled_without_connecting_later() {
    let fixture = Fixture::new();
    let (_child, held) = fill_local_admission(&fixture);
    let core = crate::MachineConnection::with_paths(fixture.paths.clone()).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = Arc::clone(&cancelled);
    let (answer, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        answer
            .send(core.refresh_cancellable(&|| cancel.load(Ordering::Acquire)))
            .unwrap();
    });
    assert!(matches!(
        result.recv_timeout(Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        result.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(CoreError::ServiceBusy)
    );
    worker.join().unwrap();
    drop(held);
}

#[test]
fn local_capacity_does_not_turn_other_protocol_failures_into_retries() {
    for kind in 0..8 {
        let fixture = Fixture::new();
        let listener = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let core = crate::MachineConnection::with_paths(fixture.paths.clone()).unwrap();
        let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
        let socket = fixture.paths.socket();
        let (done, finished) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut wire = protocol::Wire::new(&mut stream);
            let _: serde_json::Value = wire.read().unwrap();
            // Capacity can change after Hello. Partial and malformed replies still fail.
            let marker = (kind != 0).then(|| runtime.lock(capacity::MARKER, false).unwrap());
            // A replacement Core's signal cannot explain this older socket's empty reply.
            let _replacement = (kind == 7).then(|| {
                fs::remove_file(&socket).unwrap();
                let replacement = UnixListener::bind(&socket).unwrap();
                fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
                replacement
            });
            if kind == 1 {
                wire.write(&serde_json::json!({"invalid": true})).unwrap();
            }
            if kind == 2 {
                stream.write_all(&[0, 0]).unwrap();
            }
            if kind == 5 {
                let mut malformed = CAPACITY_REJECTION.to_vec();
                *malformed.last_mut().unwrap() = b'!';
                stream.write_all(&malformed).unwrap();
            }
            if kind == 6 {
                stream.write_all(&CAPACITY_REJECTION[..8]).unwrap();
            }
            let _held = if kind == 3 {
                drop(marker);
                None
            } else {
                marker
            };
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            finished.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        let result = core.refresh();
        done.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(result, Err(CoreError::Protocol), "case {kind}");
    }
}

// The native transport receipt is deliberately outside length-prefixed JSON. Normal
// admission frames, including malformed Offers, never constitute this receipt.
const CAPACITY_REJECTION: &[u8] = b"\xff\xff\xff\xffSpaceTerm local capacity\0";

fn capacity_rejection_after_hello(release_before_reply: bool) {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.paths.socket()).unwrap();
    fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
    let core = crate::MachineConnection::with_paths(fixture.paths.clone()).unwrap();
    let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _: serde_json::Value = protocol::Wire::new(&mut stream).read().unwrap();
        // Reading Hello proves that the caller passed its first capacity probe.
        let marker = runtime.lock(capacity::MARKER, false).unwrap();
        let last = CAPACITY_REJECTION.len() - 1;
        stream.write_all(&CAPACITY_REJECTION[..last]).unwrap();
        let held = if release_before_reply {
            // No complete receipt or EOF exists until after capacity has returned.
            drop(marker);
            assert!(!runtime.existing_lock_held(capacity::MARKER).unwrap());
            None
        } else {
            Some(marker)
        };
        // The old reader closes as soon as it sees the non-JSON prefix.
        let _ = stream.write_all(&CAPACITY_REJECTION[last..]);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(held);
    });
    let result = core.refresh();
    done.send(()).unwrap();
    server.join().unwrap();
    assert_eq!(result, Err(CoreError::Capacity));
}

#[test]
fn local_capacity_rejection_survives_capacity_release_before_reply_is_complete() {
    capacity_rejection_after_hello(true);
}

#[test]
fn local_capacity_rejection_after_the_first_probe_is_temporary() {
    capacity_rejection_after_hello(false);
}

#[test]
fn local_capacity_receipt_survives_the_core_closing_before_hello() {
    let (mut caller, mut server) = UnixStream::pair().unwrap();
    server.write_all(CAPACITY_REJECTION).unwrap();
    drop(server);
    assert_eq!(
        protocol::admit_client(
            &mut caller,
            (&[9; 32], &"Capacity test".into()),
            &[7; 32],
            "spaceterm-development",
        ),
        Err(protocol::Refused::Failed(CoreError::Capacity))
    );
}

#[test]
fn local_capacity_rejected_socket_reports_its_own_core() {
    let fixture = Fixture::new();
    let (_child, _held) = fill_local_admission(&fixture);
    // Bypass the advisory probe to exercise the actual full host's rejection path.
    let mut caller = connect_when_ready(&fixture);
    assert_eq!(
        protocol::admit_client(
            &mut caller,
            (&fixture.device, &"Capacity test".into()),
            &fixture.pin,
            "spaceterm-development",
        ),
        Err(protocol::Refused::Failed(CoreError::Capacity))
    );
}
