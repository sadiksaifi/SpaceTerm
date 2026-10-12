use super::*;
use spaceterm_protocol::{
    authentication::{
        Established, HandshakeConfig, PairingLookup, PairingRecord, Principal, ServerHandshake,
    },
    negotiation::{CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_NORMALIZED_CREATION_TOPOLOGY},
    wire::{common, v1, v2},
};
use std::collections::BTreeMap;
use tokio::sync::{Semaphore, mpsc};

pub struct PeerPlan {
    pub majors: Vec<u32>,
    pub capabilities: Vec<u32>,
    pub snapshot_gate: Arc<Semaphore>,
    pub client_send_gate: Arc<Semaphore>,
    pub reconciliation_gate: Arc<Semaphore>,
    pub opens: AtomicUsize,
    pub queries: AtomicUsize,
    pub reconcile_requests: AtomicUsize,
    pub reconciliation_revisions: Mutex<Vec<u64>>,
    pub suffix_before_reconciliation: Mutex<Option<(u64, u64)>>,
    pub received: Mutex<Vec<u32>>,
    pub dispatch_and_ack: Mutex<Vec<&'static str>>,
    pub sends_started: Arc<AtomicUsize>,
    pub controls: Mutex<Vec<mpsc::Sender<PeerControl>>>,
    pub revision: std::sync::atomic::AtomicU64,
}
impl PeerPlan {
    pub fn new(majors: Vec<u32>) -> Self {
        Self {
            majors,
            capabilities: vec![
                CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
                CAPABILITY_CANONICAL_DOMAIN,
            ],
            snapshot_gate: Arc::new(Semaphore::new(100)),
            client_send_gate: Arc::new(Semaphore::new(4096)),
            reconciliation_gate: Arc::new(Semaphore::new(100)),
            opens: AtomicUsize::new(0),
            queries: AtomicUsize::new(0),
            reconcile_requests: AtomicUsize::new(0),
            reconciliation_revisions: Mutex::new(vec![]),
            suffix_before_reconciliation: Mutex::new(None),
            received: Mutex::new(vec![]),
            dispatch_and_ack: Mutex::new(vec![]),
            sends_started: Arc::new(AtomicUsize::new(0)),
            controls: Mutex::new(vec![]),
            revision: std::sync::atomic::AtomicU64::new(1),
        }
    }
    pub async fn control(&self, value: PeerControl) {
        let control = self.controls.lock().unwrap().last().unwrap().clone();
        control.send(value).await.unwrap();
    }
}
pub enum PeerControl {
    Disconnect,
    Stopped,
    Gap { base: u64, revision: u64 },
    InvalidEnvelope,
    Callback,
}
pub struct Peers(pub BTreeMap<ServerId, Arc<PeerPlan>>);
impl Peers {
    pub fn one(binding: &PairedServerBinding, plan: Arc<PeerPlan>) -> Self {
        Self(BTreeMap::from([(binding.target().server(), plan)]))
    }
}

struct Sender {
    sender: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    gate: Arc<Semaphore>,
    sends_started: Arc<AtomicUsize>,
}
impl MessageSender for Sender {
    fn send<'a>(&'a self, bytes: &'a [u8]) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let sender = self.sender.lock().unwrap().as_ref().unwrap().clone();
            self.sends_started.fetch_add(1, Ordering::SeqCst);
            Arc::clone(&self.gate)
                .acquire_owned()
                .await
                .unwrap()
                .forget();
            sender
                .send(bytes.to_vec())
                .await
                .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))
        })
    }
    fn close(&self) -> ClientFuture<'_, ()> {
        self.sender.lock().unwrap().take();
        Box::pin(async { Ok(()) })
    }
    fn abort(&self) {
        self.sender.lock().unwrap().take();
    }
}
struct Receiver(mpsc::Receiver<Vec<u8>>);
impl MessageReceiver for Receiver {
    fn receive(&mut self) -> ClientFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.0.recv().await) })
    }
}
impl RouteConnector for Peers {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async move {
            assert_eq!(purpose, ConnectionPurpose::Ordinary);
            let plan = Arc::clone(self.0.get(&target.server()).unwrap());
            plan.opens.fetch_add(1, Ordering::SeqCst);
            let (client_sender, client_receiver) = mpsc::channel(16);
            let (server_sender, server_receiver) = mpsc::channel(16);
            let (control_sender, controls) = mpsc::channel(16);
            plan.controls.lock().unwrap().push(control_sender);
            let target = target.clone();
            let send_gate = Arc::clone(&plan.client_send_gate);
            let sends_started = Arc::clone(&plan.sends_started);
            tokio::spawn(async move {
                peer(target, plan, client_receiver, server_sender, controls).await;
            });
            Ok(ConnectedRoute {
                sender: Arc::new(Sender {
                    sender: Mutex::new(Some(client_sender)),
                    gate: send_gate,
                    sends_started,
                }),
                receiver: Box::new(Receiver(server_receiver)),
                peer: PeerFacts {
                    route: RouteKind::PrivateIpc,
                    local_account_verified: true,
                    pinned_server_key: None,
                },
            })
        })
    }
}
struct Lookup {
    principal: Principal,
    key: [u8; 32],
}
impl PairingLookup for Lookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        assert_eq!(
            (server, pairing, client),
            (
                self.principal.server,
                self.principal.pairing,
                self.principal.client
            )
        );
        Ok(PairingRecord {
            principal: self.principal,
            device_public_key: self.key,
            active: true,
        })
    }
}
fn local(value: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&value.to_be_bytes());
    bytes
}
fn id(bytes: [u8; 16]) -> common::Identity {
    common::Identity {
        value: bytes.to_vec(),
    }
}
async fn gate(gate: &Arc<Semaphore>, sender: &mpsc::Sender<Vec<u8>>) -> bool {
    tokio::select! {
        permit = Arc::clone(gate).acquire_owned() => { permit.unwrap().forget(); true },
        _ = sender.closed() => false,
    }
}
async fn peer(
    target: ConnectionTarget,
    plan: Arc<PeerPlan>,
    mut incoming: mpsc::Receiver<Vec<u8>>,
    outgoing: mpsc::Sender<Vec<u8>>,
    mut controls: mpsc::Receiver<PeerControl>,
) {
    let signer = Arc::new(TestKey::new(target.server().bytes()[0]));
    let principal = Principal {
        server: target.server().bytes(),
        device: target.device().bytes(),
        pairing: local(target.pairing().local()),
        client: local(target.client().local()),
    };
    let lookup = Lookup {
        principal,
        key: TestKey::new(99).public_key(),
    };
    let Some(hello) = incoming.recv().await else {
        return;
    };
    let config = HandshakeConfig {
        majors: plan.majors.clone(),
        capabilities: plan.capabilities.clone(),
        ..HandshakeConfig::default()
    };
    let (handshake, reply) =
        ServerHandshake::accept(target.server().bytes(), 1, config, signer, &lookup, &hello)
            .unwrap();
    if outgoing.send(reply).await.is_err() {
        return;
    }
    let Some(proof) = incoming.recv().await else {
        return;
    };
    let (established, ready) = handshake.receive_device(&lookup, &proof).unwrap();
    if outgoing.send(ready).await.is_err() {
        return;
    }
    let Established::Ordinary(mut codec) = established else {
        panic!("unexpected restricted peer");
    };
    loop {
        enum Event {
            Incoming(Option<Vec<u8>>),
            Control(Option<PeerControl>),
        }
        let event = tokio::select! { bytes = incoming.recv() => Event::Incoming(bytes), control = controls.recv() => Event::Control(control) };
        match event {
            Event::Control(Some(PeerControl::Disconnect)) | Event::Incoming(None) => break,
            Event::Control(Some(PeerControl::Stopped)) => {
                macro_rules! stopped {
                    ($p:ident,$variant:ident) => {
                        Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::ManagementStatus($p::ManagementStatus {
                                request: 1,
                                server: Some(id(target.server().bytes())),
                                epoch: 1,
                                state: $p::ServerState::Stopped as i32,
                                status_revision: 1,
                                native_verified: true,
                                ..Default::default()
                            })),
                        })
                    };
                }
                let payload = if codec.major() == 1 {
                    stopped!(v1, Major1)
                } else {
                    stopped!(v2, Major2)
                };
                if outgoing
                    .send(codec.encode(&payload).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Event::Control(Some(PeerControl::Callback)) => {
                let payload = match codec.major() {
                    1 => Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ManagementStatus(v1::ManagementStatus {
                            request: 99,
                            server: Some(id(target.server().bytes())),
                            epoch: 1,
                            state: v1::ServerState::Online as i32,
                            status_revision: 1,
                            ..Default::default()
                        })),
                    }),
                    2 => Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ManagementStatus(v2::ManagementStatus {
                            request: 99,
                            server: Some(id(target.server().bytes())),
                            epoch: 1,
                            state: v2::ServerState::Online as i32,
                            status_revision: 1,
                            ..Default::default()
                        })),
                    }),
                    _ => unreachable!(),
                };
                if outgoing
                    .send(codec.encode(&payload).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Event::Control(Some(PeerControl::InvalidEnvelope)) => {
                outgoing.send(vec![1, 2, 3]).await.unwrap();
            }
            Event::Control(Some(PeerControl::Gap { base, revision })) => {
                plan.revision.store(revision, Ordering::SeqCst);
                let payload = delta(codec.major(), target.server(), base, revision);
                if outgoing
                    .send(codec.encode(&payload).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Event::Control(None) => break,
            Event::Incoming(Some(bytes)) => {
                let payload = match codec.decode(&bytes) {
                    Ok(payload) => payload,
                    Err(_) => break,
                };
                plan.received.lock().unwrap().push(payload.major());
                macro_rules! respond {
                    ($message:expr, $p:ident, $variant:ident) => {
                        match $message.kind {
                            Some($p::message::Kind::OrganizationQuery(query)) => {
                                plan.queries.fetch_add(1, Ordering::SeqCst);
                                if !gate(&plan.snapshot_gate, &outgoing).await {
                                    return;
                                }
                                let revision = plan.revision.load(Ordering::SeqCst);
                                Some(Payload::$variant($p::Message {
                                    kind: Some($p::message::Kind::OrganizationSnapshot(
                                        $p::OrganizationSnapshot {
                                            server: Some(id(target.server().bytes())),
                                            epoch: 1,
                                            revision,
                                            complete: true,
                                            part: Some($p::PublicationPart {
                                                request: query.request,
                                                request_generation: query.request_generation,
                                                transfer: query.request,
                                                index: 0,
                                                total: 1,
                                                epoch: 1,
                                                base_revision: 0,
                                                revision,
                                            }),
                                            ..Default::default()
                                        },
                                    )),
                                }))
                            }
                            Some($p::message::Kind::Reconciliation(mut query)) => {
                                plan.reconcile_requests.fetch_add(1, Ordering::SeqCst);
                                plan.reconciliation_revisions
                                    .lock()
                                    .unwrap()
                                    .push(query.organization_revision);
                                let suffix =
                                    plan.suffix_before_reconciliation.lock().unwrap().take();
                                if let Some((base, revision)) = suffix {
                                    plan.revision.store(revision, Ordering::SeqCst);
                                    let payload =
                                        delta(codec.major(), target.server(), base, revision);
                                    if outgoing
                                        .send(codec.encode(&payload).unwrap())
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                if !gate(&plan.reconciliation_gate, &outgoing).await {
                                    return;
                                }
                                query.acknowledged = true;
                                query.connection_generation =
                                    plan.opens.load(Ordering::SeqCst) as u64;
                                Some(Payload::$variant($p::Message {
                                    kind: Some($p::message::Kind::Reconciliation(query)),
                                }))
                            }
                            Some($p::message::Kind::StatusQuery(_)) => {
                                plan.dispatch_and_ack.lock().unwrap().push("dispatch");
                                None
                            }
                            Some($p::message::Kind::Acknowledgement(_)) => {
                                plan.dispatch_and_ack.lock().unwrap().push("ack");
                                None
                            }
                            _ => None,
                        }
                    };
                }
                let response = match payload {
                    Payload::Major1(message) => respond!(message, v1, Major1),
                    Payload::Major2(message) => respond!(message, v2, Major2),
                };
                if let Some(response) = response
                    && outgoing
                        .send(codec.encode(&response).unwrap())
                        .await
                        .is_err()
                {
                    break;
                }
            }
        }
    }
}
fn delta(major: u32, server: ServerId, base: u64, revision: u64) -> Payload {
    macro_rules! build {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some($p::message::Kind::OrganizationDelta(
                    $p::OrganizationDelta {
                        server: Some(id(server.bytes())),
                        epoch: 1,
                        base_revision: base,
                        revision,
                        complete: true,
                        ..Default::default()
                    },
                )),
            })
        };
    }
    match major {
        1 => build!(v1, Major1),
        2 => build!(v2, Major2),
        _ => panic!("unsupported test profile"),
    }
}
