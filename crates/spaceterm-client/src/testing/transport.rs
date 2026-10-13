use super::*;
use spaceterm_protocol::{
    authentication::{
        Established, HandshakeConfig, PairingLookup, PairingRecord, Principal, ServerHandshake,
    },
    convert::{control, identity::*, startup},
    wire::{v1, v2},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

struct Message {
    bytes: Vec<u8>,
    _charge: OwnedSemaphorePermit,
}
#[derive(Clone)]
struct Lane {
    sender: mpsc::Sender<Message>,
    bytes: Arc<Semaphore>,
}
impl Lane {
    fn send(&self, bytes: Vec<u8>) -> Result<(), ClientFailure> {
        if bytes.len() > spaceterm_protocol::limits::MAX_FRAME_BYTES {
            return Err(resource_limit());
        }
        let charge = Arc::clone(&self.bytes)
            .try_acquire_many_owned(u32::try_from(bytes.len()).map_err(|_| resource_limit())?)
            .map_err(|_| resource_limit())?;
        self.sender
            .try_send(Message {
                bytes,
                _charge: charge,
            })
            .map_err(|_| resource_limit())
    }
}
fn lane() -> (Lane, mpsc::Receiver<Message>) {
    let (sender, receiver) = mpsc::channel(spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES);
    (
        Lane {
            sender,
            bytes: Arc::new(Semaphore::new(spaceterm_protocol::limits::MAX_EGRESS_BYTES)),
        },
        receiver,
    )
}
struct Sender(Mutex<Option<Lane>>);
impl MessageSender for Sender {
    fn send<'a>(&'a self, bytes: &'a [u8]) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.0
                .lock()
                .map_err(|_| invalid_peer())?
                .as_ref()
                .ok_or_else(stale)?
                .send(bytes.to_vec())
        })
    }
    fn close(&self) -> ClientFuture<'_, ()> {
        self.abort();
        Box::pin(async { Ok(()) })
    }
    fn abort(&self) {
        if let Ok(mut sender) = self.0.lock() {
            sender.take();
        }
    }
}
struct Receiver(mpsc::Receiver<Message>);
impl MessageReceiver for Receiver {
    fn receive(&mut self) -> ClientFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.0.recv().await.map(|message| message.bytes)) })
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
        if (server, pairing, client)
            != (
                self.principal.server,
                self.principal.pairing,
                self.principal.client,
            )
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(PairingRecord {
            principal: self.principal,
            device_public_key: self.key,
            active: true,
        })
    }
}
impl RouteConnector for FakeServer {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async move {
            if purpose != ConnectionPurpose::Ordinary
                || target != self.binding(target.client().local())?.target()
                || !self
                    .clients
                    .lock()
                    .map_err(|_| invalid_peer())?
                    .contains_key(&target.client())
            {
                return Err(stale());
            }
            let slot = Arc::clone(&self.slots)
                .try_acquire_owned()
                .map_err(|_| resource_limit())?;
            let (incoming, receive) = lane();
            let (outgoing, responses) = lane();
            let (faults, fault_receiver) = mpsc::channel(8);
            {
                let mut connections = self.connections.lock().map_err(|_| invalid_peer())?;
                connections.retain(|sender| !sender.is_closed());
                connections.push(faults);
            }
            let generation = self
                .next_connection
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
                .map_err(|_| exhausted())?
                + 1;
            let server = self.clone();
            let target = target.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let _ = drive(
                    server,
                    target,
                    generation,
                    receive,
                    outgoing,
                    fault_receiver,
                )
                .await;
            });
            Ok(ConnectedRoute {
                sender: Arc::new(Sender(Mutex::new(Some(incoming)))),
                receiver: Box::new(Receiver(responses)),
                peer: PeerFacts {
                    route: RouteKind::PrivateIpc,
                    local_account_verified: true,
                    pinned_server_key: None,
                },
            })
        })
    }
}
struct ConnectionGuard {
    server: FakeServer,
    binding: ConnectionBinding,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.server.deactivate_connection(self.binding);
    }
}
async fn drive(
    server: FakeServer,
    target: ConnectionTarget,
    generation: u64,
    mut incoming: mpsc::Receiver<Message>,
    outgoing: Lane,
    mut faults: mpsc::Receiver<FakeFault>,
) -> Result<(), ClientFailure> {
    let principal = Principal {
        server: target.server().bytes(),
        device: target.device().bytes(),
        pairing: encode_local(target.pairing().local())
            .value
            .try_into()
            .map_err(|_| invalid_peer())?,
        client: encode_local(target.client().local())
            .value
            .try_into()
            .map_err(|_| invalid_peer())?,
    };
    let lookup = Lookup {
        principal,
        key: server.device_signer.public_key(),
    };
    let hello = incoming.recv().await.ok_or_else(stale)?;
    let config = HandshakeConfig {
        majors: server.majors.clone(),
        capabilities: vec![
            CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            CAPABILITY_CANONICAL_DOMAIN,
        ],
        ..HandshakeConfig::default()
    };
    let (handshake, reply) = ServerHandshake::accept(
        server.server.bytes(),
        1,
        config,
        server.signer.clone(),
        &lookup,
        &hello.bytes,
    )?;
    outgoing.send(reply)?;
    let proof = incoming.recv().await.ok_or_else(stale)?;
    let (established, ready) = handshake.receive_device(&lookup, &proof.bytes)?;
    let Established::Ordinary(mut codec) = established else {
        return Err(stale());
    };
    let binding = ConnectionBinding::new(target.client(), ConnectionGeneration::new(generation));
    server.activate_connection(binding)?;
    let _connection = ConnectionGuard {
        server: server.clone(),
        binding,
    };
    outgoing.send(ready)?;
    {
        let mut majors = server.wire_majors.lock().map_err(|_| invalid_peer())?;
        if majors.len() == spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES {
            majors.remove(0);
        }
        majors.push(codec.major());
    }
    loop {
        let payload = tokio::select! {
            message = incoming.recv() => {
                let Some(message) = message else { return Ok(()); }; codec.decode(&message.bytes)?
            }
            fault = faults.recv() => match fault {
                Some(FakeFault::Disconnect) | None => return Ok(()),
                Some(FakeFault::MaliciousEnvelope(bytes)) => { outgoing.send(bytes)?; continue; },
                Some(FakeFault::OrganizationGap { revision }) => {
                    let payload = match codec.major() {
                        1 => Payload::Major1(v1::Message { kind: Some(v1::message::Kind::OrganizationResync(v1::OrganizationResync { request: 1, request_generation: 1, epoch: 1, revision })) }),
                        2 => Payload::Major2(v2::Message { kind: Some(v2::message::Kind::OrganizationResync(v2::OrganizationResync { request: 1, request_generation: 1, epoch: 1, revision })) }),
                        _ => return Err(invalid_peer()),
                    }; outgoing.send(codec.encode(&payload)?)?; continue;
                }
            }
        };
        // The generic organization/content/operation conversions are not installed
        // on this base. Use installed control/startup conversions, never a JSON or
        // empty-organization success fallback. Other messages fail closed.
        macro_rules! dispatch {
            ($message:expr, $p:ident, $variant:ident) => {{
                match $message.kind {
                    Some($p::message::Kind::Reconciliation(mut request)) => {
                        let snapshot = server.snapshot()?;
                        if request.acknowledged
                            || decode_server(request.server.as_ref().ok_or_else(invalid_peer)?)?
                                != target.server()
                            || decode_client(request.client.as_ref().ok_or_else(invalid_peer)?)?
                                != target.client()
                            || request.epoch != snapshot.epoch().get()
                            || request.organization_revision != snapshot.state().revision.get()
                        {
                            return Err(stale());
                        }
                        request.acknowledged = true;
                        request.connection_generation = generation;
                        request.grid_limits = Some($p::Geometry {
                            rows: spaceterm_core::geometry::SUPPORTED_GRID_LIMITS.max_rows(),
                            columns: spaceterm_core::geometry::SUPPORTED_GRID_LIMITS.max_columns(),
                            cell_width_px: 1,
                            cell_height_px: 1,
                        });
                        Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::Reconciliation(request)),
                        })
                    }
                    Some($p::message::Kind::Operation(operation)) => {
                        let id = decode_operation(operation.id.as_ref().ok_or_else(invalid_peer)?)?;
                        if id.stream().client() != target.client()
                            || id.stream().pairing() != target.pairing()
                        {
                            return Err(stale());
                        }
                        let command = match operation.action {
                            Some($p::operation::Action::IdentifiedControl(request)) => {
                                control::$p::decode_request(&request, target.client())?
                            }
                            Some($p::operation::Action::IdentifiedStartup(request)) => {
                                startup::$p::decode_request(&request, id)?
                            }
                            Some($p::operation::Action::ReleaseControl(request)) => {
                                control::$p::decode_release(&request)?
                            }
                            _ => {
                                return Err(ClientFailure::new(
                                    ClientFailureClass::MissingCapability,
                                ));
                            }
                        };
                        let OperationLookup::Completed {
                            result,
                            publication_revision,
                        } = server.command_at(id, command, Some(binding))?
                        else {
                            return Err(stale());
                        };
                        if server
                            .faults
                            .drop_next_outcome
                            .swap(false, Ordering::AcqRel)
                        {
                            continue;
                        }
                        let result = match result {
                            Outcome::Applied => $p::outcome::Result::Committed(true),
                            Outcome::ControlGranted(grant) => $p::outcome::Result::ControlGranted(
                                control::$p::encode_grant(grant),
                            ),
                            Outcome::StartupAwaitingMeasurement { intent } => {
                                $p::outcome::Result::StartupAwaitingMeasurement(
                                    $p::StartupAwaitingMeasurementResult {
                                        intent: Some(encode_operation(intent)),
                                    },
                                )
                            }
                            Outcome::SessionStarted(session) => {
                                $p::outcome::Result::SessionStarted(encode_session(session))
                            }
                            Outcome::Failed(
                                OperationFailure::StaleAuthority | OperationFailure::BusyRetiring,
                            ) => {
                                $p::outcome::Result::OperationFailure($p::OperationFailureResult {
                                    kind: match result {
                                        Outcome::Failed(OperationFailure::StaleAuthority) => {
                                            $p::OperationFailureKind::OpStaleAuthority
                                        }
                                        _ => $p::OperationFailureKind::OpBusyRetiring,
                                    } as i32,
                                    runtime_class: None,
                                })
                            }
                            _ => {
                                return Err(ClientFailure::new(
                                    ClientFailureClass::MissingCapability,
                                ));
                            }
                        };
                        Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::Outcome($p::Outcome {
                                operation: Some(encode_operation(id)),
                                publication_revision: publication_revision.get(),
                                result: Some(result),
                            })),
                        })
                    }
                    _ => return Err(ClientFailure::new(ClientFailureClass::MissingCapability)),
                }
            }};
        }
        let response = match payload {
            Payload::Major1(message) => dispatch!(message, v1, Major1),
            Payload::Major2(message) => dispatch!(message, v2, Major2),
        };
        outgoing.send(codec.encode(&response)?)?;
    }
}
