//! Bounded, connection-owned RGBA transfers. Cancellation drops only resource demand.
use super::*;
use tokio::sync::oneshot;

const MAX_TRANSFERS: usize = spaceterm_protocol::limits::MAX_CONCURRENT_TRANSFERS;
struct Transfer {
    request: GraphicsRequest,
    task: AbortHandle,
}
pub(in crate::protocol_dispatch) struct GraphicsEvent {
    request: GraphicsRequest,
    chunk: Option<GraphicsChunk>,
    consumed: oneshot::Sender<()>,
    admitted: bool,
}
pub(super) struct GraphicsConnection {
    limits: spaceterm_protocol::limits::NegotiatedLimits,
    transfers: BTreeMap<u64, Transfer>,
    last_transfer: u64,
    sender: mpsc::Sender<GraphicsEvent>,
    receiver: mpsc::Receiver<GraphicsEvent>,
}
impl Drop for GraphicsConnection {
    fn drop(&mut self) {
        self.reset();
    }
}
impl GraphicsConnection {
    pub(super) fn new(limits: spaceterm_protocol::limits::NegotiatedLimits) -> Self {
        let (sender, receiver) = mpsc::channel(2 * MAX_TRANSFERS);
        Self {
            limits,
            transfers: BTreeMap::new(),
            last_transfer: 0,
            sender,
            receiver,
        }
    }
    pub(super) fn reset(&mut self) {
        for (_, transfer) in std::mem::take(&mut self.transfers) {
            transfer.task.abort();
        }
    }
    pub(super) fn remove_pane(&mut self, pane: PaneId) {
        self.transfers.retain(|_, transfer| {
            if transfer.request.stamp().base().session.pane == pane {
                transfer.task.abort();
                false
            } else {
                true
            }
        });
    }
    pub(super) async fn next(&mut self) -> Option<GraphicsEvent> {
        self.receiver.recv().await
    }
    pub(super) async fn admit(
        &mut self,
        payload: &Payload,
        principal: VerifiedPrincipal,
        handle: &ServerHandle,
    ) -> Result<bool, ServerFailure> {
        macro_rules! cancel {
            ($value:expr) => {{
                let value = $value;
                let fence = identity::decode_fence(
                    value
                        .fence
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
                    None,
                )
                .map_err(protocol_failure)?;
                if fence.connection() != principal.binding()
                    || fence.session().epoch != principal.epoch()
                    || value.transfer == 0
                {
                    return Err(failure(ServerFailureClass::ScopeMismatch));
                }
                if self
                    .transfers
                    .get(&value.transfer)
                    .is_some_and(|transfer| fence.is_current(&transfer.request.stamp().fence()))
                    && let Some(transfer) = self.transfers.remove(&value.transfer)
                {
                    transfer.task.abort();
                }
                return Ok(true);
            }};
        }
        let request = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ResourceRequest(value)),
            }) => content::v1::decode_graphics_request(value).map_err(protocol_failure)?,
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ResourceRequest(value)),
            }) => content::v2::decode_graphics_request(value).map_err(protocol_failure)?,
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ResourceCancel(value)),
            }) => cancel!(value),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ResourceCancel(value)),
            }) => cancel!(value),
            _ => return Ok(false),
        };
        let fence = request.stamp().fence();
        if fence.connection() != principal.binding() || fence.session().epoch != principal.epoch() {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        if request.transfer().get() <= self.last_transfer {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        if !current(handle, principal, fence).await? {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        if self.transfers.len() >= self.limits.concurrent_transfers.min(MAX_TRANSFERS) {
            return self.reject(request);
        }
        let access = handle.runtime_access(principal, fence.session()).await?;
        let mut stream = match access.handle.read_graphics(request) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.class,
                    ServerFailureClass::Capacity
                        | ServerFailureClass::RuntimeUnavailable
                        | ServerFailureClass::StaleAuthority
                ) =>
            {
                return self.reject(request);
            }
            Err(error) => return Err(error),
        };
        self.last_transfer = request.transfer().get();
        let sender = self.sender.clone();
        let task = tokio::spawn(async move {
            loop {
                // At most one unconsumed chunk per transfer. The runtime owns and
                // charges its source lease; egress charges serialization at send.
                let chunk = match stream.next().await {
                    Some(Ok(chunk)) => Some(chunk),
                    _ => None,
                };
                let finished = chunk.as_ref().is_none_or(GraphicsChunk::complete);
                let (consumed, receipt) = oneshot::channel();
                if sender
                    .send(GraphicsEvent {
                        request,
                        chunk,
                        consumed,
                        admitted: true,
                    })
                    .await
                    .is_err()
                    || receipt.await.is_err()
                    || finished
                {
                    break;
                }
            }
        })
        .abort_handle();
        self.transfers
            .insert(request.transfer().get(), Transfer { request, task });
        Ok(true)
    }
    fn reject(&mut self, request: GraphicsRequest) -> Result<bool, ServerFailure> {
        let (consumed, _) = oneshot::channel();
        self.sender
            .try_send(GraphicsEvent {
                request,
                chunk: None,
                consumed,
                admitted: false,
            })
            .map_err(|_| failure(ServerFailureClass::Capacity))?;
        self.last_transfer = request.transfer().get();
        Ok(true)
    }
    pub(super) async fn publish(
        &mut self,
        mut event: GraphicsEvent,
        dispatcher: &ProtocolDispatcher,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        handle: &ServerHandle,
        egress: &mut ConnectionEgress,
    ) -> Result<(), ServerFailure> {
        let id = event.request.transfer().get();
        if event.admitted
            && self
                .transfers
                .get(&id)
                .is_none_or(|transfer| transfer.request != event.request)
        {
            return Ok(());
        }
        let fence = event.request.stamp().fence();
        if !current(handle, session.principal, fence).await? {
            if let Some(transfer) = self.transfers.remove(&id) {
                transfer.task.abort();
            }
            return Ok(());
        }
        let maximum = self
            .limits
            .resource_chunk_bytes
            .min(MAX_RESOURCE_CHUNK_BYTES)
            .min(self.limits.frame_bytes.saturating_sub(4096))
            .max(1);
        let mut offset = event.chunk.as_ref().map_or(0, GraphicsChunk::offset);
        let data = event.chunk.take();
        let mut terminated = data.is_none();
        let fragments = data.as_ref().map(|chunk| chunk.bytes().chunks(maximum));
        let mut fragments = fragments.into_iter().flatten().peekable();
        loop {
            let bytes = fragments.next();
            let complete =
                data.as_ref().is_some_and(|chunk| chunk.complete()) && fragments.peek().is_none();
            let reservation = match if bytes.is_some() {
                egress.reserve_content_reply((maximum + 4096) as u32)
            } else {
                egress.reserve_reliable(4096)
            } {
                Ok(reservation) => reservation,
                Err(EgressFailure::Capacity) if bytes.is_some() => {
                    terminated = true;
                    egress.reserve_reliable(4096).map_err(egress_failure)?
                }
                Err(error) => return Err(egress_failure(error)),
            };
            let chunk = if !terminated {
                let bytes = bytes.ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?;
                Some(
                    GraphicsChunk::new(event.request, offset, bytes.to_vec(), complete)
                        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
                )
            } else {
                None
            };
            macro_rules! payload {
                ($p:ident, $major:ident) => {
                    Payload::$major($p::Message {
                        kind: Some(match &chunk {
                            Some(chunk) => $p::message::Kind::ResourceChunk(
                                content::$p::encode_graphics_chunk(chunk),
                            ),
                            None => $p::message::Kind::ResourceCancel($p::ResourceCancel {
                                transfer: id,
                                fence: Some(identity::encode_fence(fence)),
                            }),
                        }),
                    })
                };
            }
            let payload = match session.codec.major() {
                1 => payload!(v1, Major1),
                2 => payload!(v2, Major2),
                _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
            };
            if !current(handle, session.principal, fence).await? {
                if let Some(transfer) = self.transfers.remove(&id) {
                    transfer.task.abort();
                }
                return Ok(());
            }
            dispatcher
                .send(session, transport, &payload, egress, Some(reservation))
                .await?;
            if terminated || fragments.peek().is_none() {
                break;
            }
            offset = offset
                .checked_add(
                    chunk
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?
                        .bytes()
                        .len() as u32,
                )
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
        }
        let finished = terminated || data.as_ref().is_none_or(GraphicsChunk::complete);
        let _ = event.consumed.send(());
        if finished && let Some(transfer) = self.transfers.remove(&id) {
            transfer.task.abort();
        }
        Ok(())
    }
}
