//! A paired device's service lane over QUIC: it stays registered for its whole life, so
//! revocation, Network Access changes and host shutdown end it.
use super::*;
use crate::connection::io::Waker;
use crate::direct::tests::{Fixture, Refusing, service};
use crate::direct::{DeviceCredential, DirectConnection};
use crate::protocol::Preamble;
use crate::service::lane::{LaneReply, LaneRequest, MAX_FRAME};
use crate::service::operations::{Action, Actor, Ledger, OperationId, Record, State};
use std::sync::Arc;
use std::time::{Duration, Instant};

type Stream = Box<crate::connection::quic::QuicStream>;

/// Serves the next QUIC connection with a lane, as the host serves one.
fn serve_next(fixture: &Fixture) -> (std::thread::JoinHandle<Result<(), ServerError>>, LaneStop) {
    let state = Arc::clone(&fixture.state);
    let mut environment = fixture.environment(service(Refusing));
    environment.lane = Some(crate::service::lane::tests::serving(&fixture.paths));
    let stopping = LaneStop::default();
    let lane = stopping.clone();
    let server = std::thread::spawn(move || {
        let (stream, generation) = accept(&state);
        let (waker, wake) = Waker::new().unwrap();
        serve(stream, generation, state, &environment, waker, wake, &lane)
    });
    (server, stopping)
}

fn accept(state: &Shared) -> (crate::connection::quic::QuicStream, [u8; 32]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(accepted) = state.lock().unwrap().accept() {
            return accepted;
        }
        assert!(Instant::now() < deadline, "QUIC accept deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn open_lane(endpoint: &str, paired: &PairedDevice, credential: &DeviceCredential) -> Stream {
    DirectConnection::connect(endpoint, paired, credential)
        .unwrap()
        .admit_service_lane(crate::protocol::Caller {
            client: &paired.client,
            name: &"Laptop CLI".into(),
            agent: None,
        })
        .unwrap()
}

/// One lane request. An error means the lane is gone.
fn call(stream: &mut Stream, request: &LaneRequest) -> Result<LaneReply, ServerError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    Preamble::send(
        stream.as_mut(),
        &serde_json::to_vec(request).unwrap(),
        deadline,
    )?;
    let reply = Preamble::receive(stream.as_mut(), MAX_FRAME, deadline)?;
    Ok(serde_json::from_slice(&reply).unwrap())
}

fn resolve() -> LaneRequest {
    LaneRequest::Resolve {
        operation: OperationId([1; 16]).to_string(),
    }
}

/// Joins `server`, which must end within a few seconds rather than at the lane's 30 s request
/// limit.
fn ended(server: std::thread::JoinHandle<Result<(), ServerError>>) -> Result<(), ServerError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server.is_finished() {
        assert!(Instant::now() < deadline, "the lane outlived its authority");
        std::thread::sleep(Duration::from_millis(5));
    }
    server.join().unwrap()
}

#[test]
fn revoking_a_pairing_closes_its_open_service_lane_and_refuses_later_requests() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    let (server, _stopping) = serve_next(&fixture);
    let mut lane = open_lane(&endpoint, &paired, &credential);
    assert!(matches!(
        call(&mut lane, &resolve()),
        Ok(LaneReply::Error { code }) if code == "not_found"
    ));
    fixture
        .management(Management::Revoke {
            operation: crypto::random().unwrap(),
            target: paired.pairing,
        })
        .unwrap();
    // Revocation severed the registered lane while it waited for the next request.
    assert_eq!(ended(server), Ok(()));
    assert!(call(&mut lane, &resolve()).is_err());
    let server = fixture.serve_next();
    assert!(matches!(
        DirectConnection::connect(&endpoint, &paired, &credential),
        Err(DirectError::Revoked)
    ));
    let _ = server.join().unwrap();
}

#[test]
fn changing_network_access_closes_an_open_service_lane() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    let (server, _stopping) = serve_next(&fixture);
    let mut lane = open_lane(&endpoint, &paired, &credential);
    assert!(call(&mut lane, &resolve()).is_ok());
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    assert_eq!(ended(server), Ok(()));
    assert!(call(&mut lane, &resolve()).is_err());
}

#[test]
fn a_revoked_pairing_refuses_the_request_that_was_already_on_its_way() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    let (server, _stopping) = serve_next(&fixture);
    let mut lane = open_lane(&endpoint, &paired, &credential);
    assert!(call(&mut lane, &resolve()).is_ok());
    // A revocation recorded without reaching the registry, as after a crash between the two,
    // still refuses the next request before the lane answers it.
    {
        let mut state = fixture.state.lock().unwrap();
        let mut next = state.record.clone();
        next.devices
            .iter_mut()
            .find(|device| device.info.pairing == paired.pairing)
            .unwrap()
            .revoked = true;
        state.commit(next).unwrap();
    }
    assert!(call(&mut lane, &resolve()).is_err());
    assert_eq!(
        ended(server),
        Err(ServerError::Direct(DirectError::Revoked))
    );
}

#[test]
fn stopping_the_host_ends_a_service_lane_whose_client_keeps_polling() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    let mut environment = fixture.environment(service(Refusing));
    environment.lane = Some(crate::service::lane::tests::serving(&fixture.paths));
    let client = std::thread::spawn(move || {
        let mut lane = open_lane(&endpoint, &paired, &credential);
        // The client polls every 200 ms, so the lane's 30 s request limit never ends the lane.
        let started = Instant::now();
        while call(&mut lane, &resolve()).is_ok() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the lane stayed open"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    let (stream, generation) = accept(&fixture.state);
    let running =
        crate::host::spawn_network(stream, generation, &fixture.state, &environment).unwrap();
    // Let the client poll a few times first.
    std::thread::sleep(Duration::from_millis(700));
    let stopped = Instant::now();
    running.stop().join().unwrap();
    assert!(stopped.elapsed() < Duration::from_secs(5));
    client.join().unwrap();
}

#[test]
fn a_device_that_removes_its_own_connection_forgets_its_service_operations() {
    let fixture = Fixture::new();
    fixture
        .state
        .lock()
        .unwrap()
        .on_revoke(crate::host::forget_service_operations(
            fixture.paths.data.clone(),
        ));
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let data = || crate::storage::Directory::open(&fixture.paths.data, false).unwrap();
    for (id, pairing) in [(1, paired.pairing), (2, [3; 32])] {
        let mut ledger = Ledger::in_directory(data());
        ledger
            .admit(Record {
                id: OperationId([id; 16]),
                actor: Actor {
                    pairing: Some(pairing),
                    client: paired.client,
                },
                server: paired.pin.server,
                boot: [0; 32],
                action: Action::Stop,
                observation_digest: [0; 32],
                candidate: None,
                state: State::Admitted,
                updated_at: 0,
            })
            .unwrap();
        ledger.mark(&OperationId([id; 16]), State::Done).unwrap();
    }
    // `connection remove` and Unpair This Computer revoke over the pre-admission wire action.
    let answer = fixture
        .authenticate(
            &credential,
            wire::Action::Revoke {
                actor: paired.clone(),
                operation: crypto::random().unwrap(),
                target: paired.pairing,
            },
        )
        .unwrap();
    assert!(matches!(answer, wire::Answer::Revoked(_)));
    let ledger = Ledger::in_directory(data());
    assert_eq!(ledger.resolve(&OperationId([1; 16])).unwrap(), None);
    assert!(ledger.resolve(&OperationId([2; 16])).unwrap().is_some());
}
