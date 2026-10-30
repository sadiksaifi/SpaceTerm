//! Network Access changes over the service lane, planned against real direct-network state and
//! served over QUIC, including changes that end the caller's own lane.
use super::*;
use crate::connection::io::Waker;
use crate::direct::tests::{Fixture, Refusing, service};
use crate::direct::{DeviceCredential, DirectConnection};
use crate::protocol::Preamble;
use crate::service::lane::network::{LaneCallerEffect, NetworkAccess, NetworkCaller, NetworkPlan};
use crate::service::lane::{
    LaneAction, LaneDevice, LaneHost, LaneNetwork, LaneOperationState, LaneReply, LaneRequest,
    LaneServer, MAX_FRAME,
};
use crate::service::operations::{Actor, OperationId};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The lane's view of a host that has only direct-network state.
struct Direct(Shared);
impl LaneHost for Direct {
    fn fence(
        &self,
        admit: &mut dyn FnMut(&[[u8; 32]]) -> Result<(), ServerError>,
    ) -> Result<(), ServerError> {
        admit(&[])
    }
    fn release_fence(&self) {}
    fn sessions(&self) -> Vec<[u8; 32]> {
        Vec::new()
    }
    fn devices(&self) -> Result<Vec<LaneDevice>, ServerError> {
        Ok(Vec::new())
    }
    fn network(&self) -> Result<LaneNetwork, ServerError> {
        let ManagementReply::Status(status) = self
            .0
            .lock()
            .unwrap()
            .manage(None, Management::Status)
            .map_err(ServerError::from)?
            .0
        else {
            return Err(ServerError::Protocol);
        };
        Ok(LaneNetwork {
            enabled: status.enabled,
            bind: status.policy.map(|policy| policy.bind.to_string()),
            listening: matches!(status.listener, ListenerState::Listening { .. }),
            addresses: Vec::new(),
        })
    }
    fn change_network(
        &self,
        access: NetworkAccess,
        caller: Option<&NetworkCaller>,
        decide: &mut dyn FnMut(&NetworkPlan) -> Result<bool, ServerError>,
    ) -> Result<Option<crate::service::lane::network::ListenerView>, ServerError> {
        change_network(&self.0, access, caller, decide)
    }
    fn listener(&self) -> Result<crate::service::lane::network::ListenerView, ServerError> {
        Ok(listener(&self.0))
    }
}

const LOCAL: Actor = Actor {
    pairing: None,
    client: [8; 32],
};

/// Serves every QUIC connection on its own thread, as the host does, until dropped.
struct Serving {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Serving {
    fn start(fixture: &Fixture, lane: &Arc<LaneServer>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (state, lane, stopped) = (Arc::clone(&fixture.state), Arc::clone(lane), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let accepted = {
                    let mut state = state.lock().unwrap();
                    // A narrowed listener binds once the previous one's connections ended.
                    state.retry_listen();
                    state.accept()
                };
                let Some((stream, generation)) = accepted else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let (state, lane) = (Arc::clone(&state), Arc::clone(&lane));
                std::thread::spawn(move || {
                    let (sync, _) = crate::connection::sync::SyncWorker::spawn().unwrap();
                    let mut environment = crate::host::Environment::for_tests(
                        service(Refusing),
                        sync,
                        Some(Arc::clone(&state)),
                    );
                    environment.lane = Some(lane);
                    let (waker, wake) = Waker::new().unwrap();
                    let _ = serve(
                        stream,
                        generation,
                        state,
                        &environment,
                        waker,
                        wake,
                        &LaneStop::default(),
                    );
                });
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Serving {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Stream = Box<crate::connection::quic::QuicStream>;

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

fn network(access: NetworkAccess) -> LaneAction {
    LaneAction::Network { access }
}

fn preview(reply: LaneReply) -> (String, crate::service::lane::network::LaneNetworkEffects) {
    match reply {
        LaneReply::NetworkPreview { token, effects, .. } => (token, effects),
        other => panic!("expected a Network Access preview, got {other:?}"),
    }
}

fn execute(token: &str, access: NetworkAccess, operation: u8) -> LaneRequest {
    LaneRequest::Execute {
        action: network(access),
        token: token.to_owned(),
        operation: OperationId([operation; 16]).to_string(),
    }
}

fn policy(fixture: &Fixture) -> (Option<SocketAddr>, bool) {
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    (status.policy.map(|policy| policy.bind), status.enabled)
}

fn lane(fixture: &Fixture) -> Arc<LaneServer> {
    crate::service::lane::tests::serving_with(
        &fixture.paths,
        Arc::new(Direct(Arc::clone(&fixture.state))),
    )
}

#[test]
fn a_previewed_change_applies_once_when_confirmed_and_resolves_done() {
    let fixture = Fixture::new();
    let at = fixture.listen_at(Ipv4Addr::LOCALHOST.into());
    let lane = lane(&fixture);
    let all = NetworkAccess::All;
    let (token, effects) = preview(lane.answer(
        LaneRequest::Preview {
            action: network(all),
        },
        &LOCAL,
    ));
    assert_eq!(effects.from.bind, Some(at.to_string()));
    assert_eq!(effects.to.bind, Some(format!("0.0.0.0:{}", at.port())));
    // Widening keeps the listener, and the server's own machine has no paired connection.
    assert!(!effects.ends_connections);
    assert_eq!(effects.caller, None);
    assert!(!effects.unchanged());
    // Previewing changes nothing.
    assert_eq!(policy(&fixture), (Some(at), true));
    let accepted = lane.answer(execute(&token, all, 1), &LOCAL);
    assert!(
        matches!(accepted, LaneReply::Accepted { .. }),
        "{accepted:?}"
    );
    let every = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), at.port());
    assert_eq!(policy(&fixture), (Some(every), true));
    let LaneReply::Receipt(receipt) = lane.answer(
        LaneRequest::Resolve {
            operation: OperationId([1; 16]).to_string(),
        },
        &LOCAL,
    ) else {
        panic!("expected a receipt");
    };
    assert_eq!(receipt.state, LaneOperationState::Done);
    assert_eq!(receipt.action, network(all));
    // Repeating the operation finds its record and changes nothing again.
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    assert!(matches!(
        lane.answer(execute(&token, all, 1), &LOCAL),
        LaneReply::Accepted { .. }
    ));
    assert_eq!(policy(&fixture), (Some(every), false));
    // The same setting again previews as unchanged.
    let (_, effects) = preview(lane.answer(
        LaneRequest::Preview {
            action: network(NetworkAccess::Off),
        },
        &LOCAL,
    ));
    assert!(effects.unchanged());
}

#[test]
fn a_token_for_another_change_or_an_earlier_listener_is_refused_as_changed() {
    let fixture = Fixture::new();
    let at = fixture.listen_at(Ipv4Addr::LOCALHOST.into());
    let lane = lane(&fixture);
    let (token, _) = preview(lane.answer(
        LaneRequest::Preview {
            action: network(NetworkAccess::All),
        },
        &LOCAL,
    ));
    // A token binds the change it previewed.
    let reply = lane.answer(execute(&token, NetworkAccess::Off, 1), &LOCAL);
    assert_eq!(
        reply,
        LaneReply::Error {
            code: "changed".into()
        }
    );
    // A change made since the preview starts a new listener, so the plan no longer matches.
    let (token, _) = preview(lane.answer(
        LaneRequest::Preview {
            action: network(NetworkAccess::All),
        },
        &LOCAL,
    ));
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    let reply = lane.answer(execute(&token, NetworkAccess::All, 2), &LOCAL);
    assert_eq!(
        reply,
        LaneReply::Error {
            code: "changed".into()
        }
    );
    assert_eq!(policy(&fixture), (Some(at), false));
    // A refused Execute admitted nothing.
    assert!(matches!(
        lane.answer(
            LaneRequest::Resolve {
                operation: OperationId([2; 16]).to_string(),
            },
            &LOCAL,
        ),
        LaneReply::Error { code } if code == "not_found"
    ));
}

#[test]
fn an_address_that_is_not_this_machines_is_refused_before_any_token() {
    let fixture = Fixture::new();
    fixture.listen_at(Ipv4Addr::LOCALHOST.into());
    let lane = lane(&fixture);
    let reply = lane.answer(
        LaneRequest::Preview {
            action: network(NetworkAccess::Address("192.0.2.1:9231".parse().unwrap())),
        },
        &LOCAL,
    );
    assert_eq!(
        reply,
        LaneReply::Error {
            code: "invalid_address".into()
        }
    );
}

#[test]
fn a_paired_caller_cannot_leave_every_connected_device_without_a_route() {
    let fixture = Fixture::new();
    let at = fixture.listen_at(Ipv4Addr::LOCALHOST.into());
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let lane = lane(&fixture);
    let _serving = Serving::start(&fixture, &lane);
    let mut stream = open_lane(&fixture.endpoint(), &paired, &credential);
    let other_port = SocketAddr::new(at.ip(), at.port().wrapping_add(1).max(1));
    for access in [NetworkAccess::Off, NetworkAccess::Address(other_port)] {
        let reply = call(
            &mut stream,
            &LaneRequest::Preview {
                action: network(access),
            },
        )
        .unwrap();
        assert_eq!(
            reply,
            LaneReply::Error {
                code: "disconnects_every_device".into()
            }
        );
    }
    // A change that keeps the caller's route is offered.
    let (_, effects) = preview(
        call(
            &mut stream,
            &LaneRequest::Preview {
                action: network(NetworkAccess::All),
            },
        )
        .unwrap(),
    );
    assert_eq!(effects.caller, Some(LaneCallerEffect::StaysConnected));
    // The server's own machine may still turn Network Access off.
    let (token, effects) = preview(lane.answer(
        LaneRequest::Preview {
            action: network(NetworkAccess::Off),
        },
        &LOCAL,
    ));
    assert_eq!(effects.caller, None);
    assert_eq!(effects.disconnects.len(), 1);
    assert_eq!(effects.disconnects[0].id, hex(&paired.pairing));
    assert!(matches!(
        lane.answer(execute(&token, NetworkAccess::Off, 3), &LOCAL),
        LaneReply::Accepted { .. }
    ));
    assert_eq!(policy(&fixture), (Some(at), false));
    // That change ended the paired lane.
    assert!(call(&mut stream, &LaneRequest::Status).is_err());
}

#[cfg(feature = "client")]
mod flow {
    use super::*;
    use crate::service::lane::client::{Lane, Outcome};

    struct Connector {
        endpoint: String,
        paired: PairedDevice,
        credential: DeviceCredential,
    }
    impl crate::direct::DirectConnector for Connector {
        fn connect(&self) -> Result<DirectConnection, DirectError> {
            DirectConnection::connect(&self.endpoint, &self.paired, &self.credential)
        }
    }

    fn paired_lane(endpoint: String, paired: &PairedDevice, credential: DeviceCredential) -> Lane {
        Lane::paired(
            Arc::new(Connector {
                endpoint,
                paired: paired.clone(),
                credential,
            }),
            paired.clone(),
            "Laptop CLI".into(),
            None,
        )
    }

    /// Listens on every interface and pairs a device that reaches the server at `ip`.
    fn device_at(
        fixture: &Fixture,
        port: u16,
        ip: std::net::IpAddr,
    ) -> (String, PairedDevice, DeviceCredential) {
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let endpoint = format!("quic://{}", SocketAddr::new(ip, port));
        (endpoint, paired, credential)
    }

    fn perform(
        lane: &Lane,
        access: NetworkAccess,
        wait: Duration,
    ) -> (crate::service::lane::network::LaneNetworkEffects, Outcome) {
        let mut session = lane.session().unwrap();
        let (token, effects) = preview(
            session
                .request(
                    &LaneRequest::Preview {
                        action: network(access),
                    },
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap(),
        );
        let outcome = lane
            .perform(
                session,
                network(access),
                token,
                wait,
                &mut |_| Ok(()),
                &|| None,
            )
            .unwrap();
        (effects, outcome)
    }

    #[test]
    fn a_change_that_ends_the_callers_own_lane_settles_after_it_reconnects() {
        let fixture = Fixture::new();
        let every = fixture.listen_at(Ipv4Addr::UNSPECIFIED.into());
        let lane = super::lane(&fixture);
        let _serving = Serving::start(&fixture, &lane);
        let (endpoint, paired, credential) =
            device_at(&fixture, every.port(), Ipv4Addr::LOCALHOST.into());
        let client = paired_lane(endpoint, &paired, credential);
        let narrowed = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), every.port());
        let (effects, outcome) = perform(
            &client,
            NetworkAccess::Address(narrowed),
            Duration::from_secs(20),
        );
        // Narrowing starts a new listener, which still serves the address this device uses.
        assert!(effects.ends_connections);
        assert_eq!(effects.caller, Some(LaneCallerEffect::Reconnects));
        assert!(effects.disconnects.is_empty());
        let Outcome::Settled { receipt, .. } = outcome else {
            panic!("expected a settled change, got {outcome:?}");
        };
        assert_eq!(receipt.state, LaneOperationState::Done);
        assert_eq!(policy(&fixture), (Some(narrowed), true));
        // The flow acknowledged the receipt on its new connection.
        assert!(
            crate::service::operations::Ledger::in_directory(
                crate::storage::Directory::open(&fixture.paths.data, false).unwrap(),
            )
            .records()
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn a_narrowing_that_slow_lane_work_delays_still_listens_before_it_settles_done() {
        let fixture = Fixture::new();
        let every = fixture.listen_at(Ipv4Addr::UNSPECIFIED.into());
        // Another device's lane is inside a release fetch that outlasts the change, so its
        // thread holds a connection of the every-interface listener the narrowing retires.
        let lane = crate::service::lane::tests::serving_with_feed(
            &fixture.paths,
            Arc::new(Direct(Arc::clone(&fixture.state))),
            Arc::new(crate::service::lane::tests::SlowFeed(
                crate::protocol::limits::STALL_AFTER + Duration::from_secs(5),
            )),
        );
        let _serving = Serving::start(&fixture, &lane);
        let (endpoint, other, credential) =
            device_at(&fixture, every.port(), Ipv4Addr::LOCALHOST.into());
        let mut busy = open_lane(&endpoint, &other, &credential);
        let fetching = std::thread::spawn(move || {
            let _ = call(&mut busy, &LaneRequest::StageFetch);
            busy
        });
        std::thread::sleep(Duration::from_millis(300));
        let (endpoint, paired, credential) =
            device_at(&fixture, every.port(), Ipv4Addr::LOCALHOST.into());
        let client = paired_lane(endpoint, &paired, credential);
        let narrowed = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), every.port());
        let started = Instant::now();
        let (effects, outcome) = perform(
            &client,
            NetworkAccess::Address(narrowed),
            crate::service::lane::admin::NETWORK_WAIT,
        );
        assert_eq!(effects.caller, Some(LaneCallerEffect::Reconnects));
        let Outcome::Settled { receipt, .. } = outcome else {
            panic!("expected a settled change, got {outcome:?}");
        };
        assert_eq!(receipt.state, LaneOperationState::Done);
        assert!(started.elapsed() < crate::service::lane::admin::NETWORK_WAIT);
        // Done means the narrowed listener accepts, although the fetch still runs.
        let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap()
        else {
            panic!()
        };
        assert_eq!(status.listener, ListenerState::Listening { addr: narrowed });
        assert!(!fetching.is_finished() || started.elapsed() > Duration::from_secs(5));
        drop(fetching.join());
    }

    #[test]
    fn a_caller_whose_route_the_change_removes_reports_an_unconfirmed_outcome() {
        let fixture = Fixture::new();
        let every = fixture.listen_at(Ipv4Addr::UNSPECIFIED.into());
        let lane = super::lane(&fixture);
        let _serving = Serving::start(&fixture, &lane);
        // Another device stays reachable over IPv6 loopback, so the change keeps a route.
        let (endpoint, other, credential) =
            device_at(&fixture, every.port(), Ipv6Addr::LOCALHOST.into());
        let Ok(connection) = DirectConnection::connect(&endpoint, &other, &credential) else {
            eprintln!("skipped: this host has no IPv6 loopback");
            return;
        };
        let mut kept = connection
            .admit_service_lane(crate::protocol::Caller {
                client: &other.client,
                name: &"Studio CLI".into(),
                agent: None,
            })
            .unwrap();
        assert!(call(&mut kept, &LaneRequest::Status).is_ok());
        let (endpoint, paired, credential) =
            device_at(&fixture, every.port(), Ipv4Addr::LOCALHOST.into());
        let client = paired_lane(endpoint, &paired, credential);
        let v6 = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), every.port());
        let started = Instant::now();
        let (effects, outcome) =
            perform(&client, NetworkAccess::Address(v6), Duration::from_secs(4));
        assert_eq!(effects.caller, Some(LaneCallerEffect::Disconnected));
        assert_eq!(
            effects
                .disconnects
                .iter()
                .map(|v| v.id.clone())
                .collect::<Vec<_>>(),
            vec![hex(&paired.pairing)]
        );
        // The change ended this device's lane, and its saved address no longer reaches the
        // server, so the flow ends unconfirmed at its wait.
        let Outcome::Unconfirmed { operation, .. } = outcome else {
            panic!("expected an unconfirmed change, got {outcome:?}");
        };
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(policy(&fixture), (Some(v6), true));
        // The record settled as done on the server, for the device's next connection.
        let record = crate::service::operations::Ledger::in_directory(
            crate::storage::Directory::open(&fixture.paths.data, false).unwrap(),
        )
        .resolve(&OperationId::parse(&operation).unwrap())
        .unwrap()
        .unwrap();
        assert_eq!(record.state, crate::service::operations::State::Done);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}

/// A lane transport that answers a fixed list of requests and runs `accepted` while it writes
/// an `Accepted` reply, the moment between admission and the change.
struct Scripted {
    requests: std::collections::VecDeque<LaneRequest>,
    replies: Vec<LaneReply>,
    accepted: Option<Box<dyn FnOnce()>>,
}
impl Scripted {
    fn new(requests: Vec<LaneRequest>) -> Self {
        Self {
            requests: requests.into(),
            replies: Vec::new(),
            accepted: None,
        }
    }
}
impl Preamble for Scripted {
    fn send(&mut self, frame: &[u8], _: Instant) -> Result<(), ServerError> {
        let reply: LaneReply = serde_json::from_slice(frame).unwrap();
        if matches!(reply, LaneReply::Accepted { .. })
            && let Some(accepted) = self.accepted.take()
        {
            accepted();
        }
        self.replies.push(reply);
        Ok(())
    }
    fn receive(&mut self, _: usize, _: Instant) -> Result<Vec<u8>, ServerError> {
        let request = self.requests.pop_front().ok_or(ServerError::Protocol)?;
        Ok(serde_json::to_vec(&request).unwrap())
    }
}

/// Holds `[::1]` at `port` like another app, or `None` without IPv6 loopback.
fn hold_ipv6(port: u16) -> Option<std::net::UdpSocket> {
    std::net::UdpSocket::bind(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)).ok()
}

#[test]
fn a_widening_another_app_blocks_keeps_the_listener_and_every_connection() {
    let fixture = Fixture::new();
    let at = fixture.listen_at(Ipv4Addr::LOCALHOST.into());
    let Some(_other_app) = hold_ipv6(at.port()) else {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let lane = lane(&fixture);
    let _serving = Serving::start(&fixture, &lane);
    let mut stream = open_lane(&fixture.endpoint(), &paired, &credential);
    // The preview expects the listener to widen in place.
    let (token, effects) = preview(
        call(
            &mut stream,
            &LaneRequest::Preview {
                action: network(NetworkAccess::All),
            },
        )
        .unwrap(),
    );
    assert_eq!(effects.caller, Some(LaneCallerEffect::StaysConnected));
    assert!(matches!(
        call(&mut stream, &execute(&token, NetworkAccess::All, 4)).unwrap(),
        LaneReply::Accepted { .. }
    ));
    // Widening failed before anything was saved or retired, so the device keeps its lane and
    // the record reports the typed failure instead of done.
    let LaneReply::Receipt(receipt) = call(
        &mut stream,
        &LaneRequest::Resolve {
            operation: OperationId([4; 16]).to_string(),
        },
    )
    .unwrap() else {
        panic!("expected a receipt");
    };
    assert_eq!(receipt.state, LaneOperationState::Failed);
    assert_eq!(receipt.code.as_deref(), Some("address_unavailable"));
    assert_eq!(policy(&fixture), (Some(at), true));
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    assert_eq!(status.listener, ListenerState::Listening { addr: at });
    // The same refusal reaches `server setup --network` on the server's own machine.
    assert!(matches!(
        fixture.management(Management::SetPolicy {
            enabled: true,
            policy: Some(NetworkPolicy::every_interface(at.port()).unwrap()),
        }),
        Err(DirectError::AddressUnavailable)
    ));
    assert_eq!(policy(&fixture), (Some(at), true));
    assert!(call(&mut stream, &LaneRequest::Status).is_ok());
}

#[test]
fn an_address_another_app_holds_is_refused_before_the_current_listener_retires() {
    let fixture = Fixture::new();
    let at = fixture.listen_at(Ipv4Addr::LOCALHOST.into());
    let taken = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let elsewhere = taken.local_addr().unwrap();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let lane = lane(&fixture);
    let _serving = Serving::start(&fixture, &lane);
    let mut stream = open_lane(&fixture.endpoint(), &paired, &credential);
    assert!(matches!(
        fixture.management(Management::SetPolicy {
            enabled: true,
            policy: Some(NetworkPolicy::new(elsewhere).unwrap()),
        }),
        Err(DirectError::AddressUnavailable)
    ));
    assert_eq!(policy(&fixture), (Some(at), true));
    assert!(call(&mut stream, &LaneRequest::Status).is_ok());
}

#[test]
fn a_device_that_leaves_while_accepted_is_written_stops_a_change_that_would_strand_the_caller() {
    let fixture = Fixture::new();
    let every = fixture.listen_at(Ipv4Addr::UNSPECIFIED.into());
    let lane = lane(&fixture);
    let _serving = Serving::start(&fixture, &lane);
    // The only device that keeps a route reaches the server over IPv6.
    let credential = DeviceCredential::generate().unwrap();
    let other = fixture.enroll(&credential);
    let v6 = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), every.port());
    let Ok(connection) = DirectConnection::connect(&format!("quic://{v6}"), &other, &credential)
    else {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    let mut kept = connection
        .admit_service_lane(crate::protocol::Caller {
            client: &other.client,
            name: &"Studio CLI".into(),
            agent: None,
        })
        .unwrap();
    assert!(call(&mut kept, &LaneRequest::Status).is_ok());
    // The caller reaches the server over IPv4 and asks for IPv6 only.
    let caller = fixture.enroll(&DeviceCredential::generate().unwrap());
    let actor = Actor {
        pairing: Some(caller.pairing),
        client: caller.client,
    };
    let route = Some(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), every.port()));
    let access = NetworkAccess::Address(v6);
    let mut previewing = Scripted::new(vec![LaneRequest::Preview {
        action: network(access),
    }]);
    lane.serve(&mut previewing, actor, route, &|| Ok(()))
        .unwrap();
    let (token, effects) = preview(previewing.replies.remove(0));
    assert_eq!(effects.caller, Some(LaneCallerEffect::Disconnected));
    // The other device is revoked while `Accepted` is written, before the change applies.
    let mut executing = Scripted::new(vec![execute(&token, access, 5)]);
    let (state, pairing) = (Arc::clone(&fixture.state), other.pairing);
    executing.accepted = Some(Box::new(move || {
        state
            .lock()
            .unwrap()
            .manage(
                None,
                Management::Revoke {
                    operation: crate::direct::crypto::random().unwrap(),
                    target: pairing,
                },
            )
            .unwrap();
    }));
    lane.serve(&mut executing, actor, route, &|| Ok(()))
        .unwrap();
    assert!(matches!(executing.replies[0], LaneReply::Accepted { .. }));
    // No connected device keeps a route now, so the change was refused, not applied.
    assert_eq!(policy(&fixture), (Some(every), true));
    let record = crate::service::operations::Ledger::in_directory(
        crate::storage::Directory::open(&fixture.paths.data, false).unwrap(),
    )
    .resolve(&OperationId([5; 16]))
    .unwrap()
    .unwrap();
    assert_eq!(
        record.state,
        crate::service::operations::State::Failed {
            code: "disconnects_every_device".into()
        }
    );
}

/// A host whose listener reports `Starting` as a change applies it, then stops, as when another
/// socket takes a narrowed address before the server binds it.
struct Stalling {
    direct: Direct,
    reads: std::sync::atomic::AtomicUsize,
}
impl LaneHost for Stalling {
    fn fence(
        &self,
        admit: &mut dyn FnMut(&[[u8; 32]]) -> Result<(), ServerError>,
    ) -> Result<(), ServerError> {
        self.direct.fence(admit)
    }
    fn release_fence(&self) {}
    fn sessions(&self) -> Vec<[u8; 32]> {
        Vec::new()
    }
    fn devices(&self) -> Result<Vec<LaneDevice>, ServerError> {
        Ok(Vec::new())
    }
    fn network(&self) -> Result<LaneNetwork, ServerError> {
        self.direct.network()
    }
    fn change_network(
        &self,
        access: NetworkAccess,
        caller: Option<&NetworkCaller>,
        decide: &mut dyn FnMut(&NetworkPlan) -> Result<bool, ServerError>,
    ) -> Result<Option<crate::service::lane::network::ListenerView>, ServerError> {
        use crate::service::lane::network::ListenerProgress;
        let applied = self.direct.change_network(access, caller, decide)?;
        Ok(applied.map(|mut applied| {
            applied.progress = ListenerProgress::Starting;
            applied
        }))
    }
    fn listener(&self) -> Result<crate::service::lane::network::ListenerView, ServerError> {
        use crate::service::lane::network::ListenerProgress;
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut now = self.direct.listener()?;
        now.progress = ListenerProgress::Stopped;
        Ok(now)
    }
}

#[test]
fn an_applied_change_whose_listener_never_accepts_settles_unresolved_not_done() {
    let fixture = Fixture::new();
    let at = fixture.listen_at(Ipv4Addr::LOCALHOST.into());
    let lane = crate::service::lane::tests::serving_with(
        &fixture.paths,
        Arc::new(Stalling {
            direct: Direct(Arc::clone(&fixture.state)),
            reads: Default::default(),
        }),
    );
    let all = NetworkAccess::All;
    let (token, _) = preview(lane.answer(
        LaneRequest::Preview {
            action: network(all),
        },
        &LOCAL,
    ));
    assert!(matches!(
        lane.answer(execute(&token, all, 6), &LOCAL),
        LaneReply::Accepted { .. }
    ));
    let resolve = || {
        let LaneReply::Receipt(receipt) = lane.answer(
            LaneRequest::Resolve {
                operation: OperationId([6; 16]).to_string(),
            },
            &LOCAL,
        ) else {
            panic!("expected a receipt");
        };
        receipt.state
    };
    // The change applied and waits for its listener.
    assert_eq!(resolve(), LaneOperationState::Running);
    assert_eq!(
        policy(&fixture),
        (
            Some(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), at.port())),
            true
        )
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while resolve() == LaneOperationState::Running {
        assert!(Instant::now() < deadline, "the watcher never settled");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(resolve(), LaneOperationState::Unresolved);
}

#[test]
fn a_narrowing_that_a_later_change_supersedes_before_it_listens_settles_unresolved() {
    let fixture = Fixture::new();
    let every = fixture.listen_at(Ipv4Addr::UNSPECIFIED.into());
    // A device's lane sits in a release fetch, so the every-interface listener a narrowing
    // retires keeps routing to its connection and the narrowed bind waits.
    let lane = crate::service::lane::tests::serving_with_feed(
        &fixture.paths,
        Arc::new(Direct(Arc::clone(&fixture.state))),
        Arc::new(crate::service::lane::tests::SlowFeed(Duration::from_secs(
            8,
        ))),
    );
    let _serving = Serving::start(&fixture, &lane);
    let credential = DeviceCredential::generate().unwrap();
    let other = fixture.enroll(&credential);
    let endpoint = format!(
        "quic://{}",
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), every.port())
    );
    let mut busy = open_lane(&endpoint, &other, &credential);
    let fetching = std::thread::spawn(move || {
        let _ = call(&mut busy, &LaneRequest::StageFetch);
        busy
    });
    std::thread::sleep(Duration::from_millis(300));
    let narrowed =
        NetworkAccess::Address(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), every.port()));
    let (token, _) = preview(lane.answer(
        LaneRequest::Preview {
            action: network(narrowed),
        },
        &LOCAL,
    ));
    assert!(matches!(
        lane.answer(execute(&token, narrowed, 7), &LOCAL),
        LaneReply::Accepted { .. }
    ));
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    assert!(matches!(status.listener, ListenerState::Starting { .. }));
    // Settings restores every interface before the narrowed listener binds, which revives the
    // listener the narrowing retired.
    fixture
        .management(Management::SetPolicy {
            enabled: true,
            policy: Some(NetworkPolicy::every_interface(every.port()).unwrap()),
        })
        .unwrap();
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    assert_eq!(status.listener, ListenerState::Listening { addr: every });
    // That listener accepts, but the narrowing never did, so it is not done.
    let resolve = || {
        let LaneReply::Receipt(receipt) = lane.answer(
            LaneRequest::Resolve {
                operation: OperationId([7; 16]).to_string(),
            },
            &LOCAL,
        ) else {
            panic!("expected a receipt");
        };
        receipt.state
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while resolve() == LaneOperationState::Running {
        assert!(Instant::now() < deadline, "the watcher never settled");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(resolve(), LaneOperationState::Unresolved);
    drop(fetching.join());
}
