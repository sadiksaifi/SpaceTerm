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
    for kind in 0..4 {
        let fixture = Fixture::new();
        let listener = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let core = crate::MachineConnection::with_paths(fixture.paths.clone()).unwrap();
        let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut wire = protocol::Wire::new(&mut stream);
            let _: serde_json::Value = wire.read().unwrap();
            // Capacity can change after Hello. Partial and malformed replies still fail.
            let marker = (kind != 0).then(|| runtime.lock(capacity::MARKER, false).unwrap());
            if kind == 1 {
                wire.write(&serde_json::json!({"invalid": true})).unwrap();
            }
            if kind == 2 {
                stream.write_all(&[0, 0]).unwrap();
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
        assert_eq!(
            core.refresh_cancellable(&|| false),
            Err(CoreError::Protocol)
        );
        done.send(()).unwrap();
        server.join().unwrap();
    }
}
