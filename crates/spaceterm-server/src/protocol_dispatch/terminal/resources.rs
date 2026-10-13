//! Four connection-owned graphics streams. Runtime owns decoding and immutable
//! asset leases; this adapter reserves egress before pulling each bounded chunk.

use super::*;
use spaceterm_core::content::{GraphicsResourceId, GraphicsResourceKey, GraphicsResourceRef};

const MAX_TRANSFERS: usize = 4;
// QueryStamp, resource metadata, protobuf lengths and authenticated framing.
const CHUNK_METADATA_BYTES: u32 = 4096;
struct Transfer {
    request: GraphicsRequest,
    stream: Box<dyn RuntimeStream<GraphicsChunk>>,
}
pub(in crate::protocol_dispatch) struct ResourceEvent {
    request: GraphicsRequest,
    chunk: Option<Result<GraphicsChunk, ServerFailure>>,
    reservation: EgressReservation,
}
pub(in crate::protocol_dispatch) struct ResourceTransfers {
    active: [Option<Transfer>; MAX_TRANSFERS],
    last_transfer: u64,
}
impl ResourceTransfers {
    pub(in crate::protocol_dispatch) fn new() -> Self {
        Self {
            active: std::array::from_fn(|_| None),
            last_transfer: 0,
        }
    }
    pub(in crate::protocol_dispatch) fn reset(&mut self) {
        self.active = std::array::from_fn(|_| None);
    }
    pub(in crate::protocol_dispatch) async fn admit(
        &mut self,
        payload: &Payload,
        principal: VerifiedPrincipal,
        handle: &ServerHandle,
        limits: spaceterm_protocol::limits::NegotiatedLimits,
    ) -> Result<bool, ServerFailure> {
        macro_rules! request {
            ($value:expr, $p:ident) => {{
                let value = $value;
                let stamp = content::$p::decode_stamp(
                    value
                        .stamp
                        .as_deref()
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
                )
                .map_err(protocol_failure)?;
                let fence = identity::decode_fence(
                    value
                        .fence
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
                    Some(stamp.base().revision),
                )
                .map_err(protocol_failure)?;
                if fence != stamp.fence() {
                    return Err(failure(ServerFailureClass::ScopeMismatch));
                }
                let resource = value
                    .resource
                    .as_ref()
                    .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?;
                let resource = GraphicsResourceRef {
                    key: GraphicsResourceKey {
                        session: identity::decode_session(
                            resource
                                .session
                                .as_ref()
                                .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
                        )
                        .map_err(protocol_failure)?,
                        id: GraphicsResourceId::new(resource.local_id)
                            .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
                        generation: resource.generation,
                    },
                    width: value.width,
                    height: value.height,
                    rgba_bytes: value.rgba_bytes,
                    digest: value
                        .digest
                        .as_slice()
                        .try_into()
                        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
                };
                GraphicsRequest::new(
                    stamp,
                    ResourceTransferId::new(value.transfer)
                        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
                    resource,
                    u32::try_from(value.offset)
                        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
                )
                .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?
            }};
        }
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
                if value.transfer == 0
                    || fence.connection() != principal.binding()
                    || fence.session().epoch != principal.epoch()
                {
                    return Err(failure(ServerFailureClass::ScopeMismatch));
                }
                for slot in &mut self.active {
                    if slot.as_ref().is_some_and(|transfer| {
                        transfer.request.transfer().get() == value.transfer
                            && fence.is_current(&transfer.request.stamp().fence())
                    }) {
                        *slot = None;
                    }
                }
                return Ok(true);
            }};
        }
        let request = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ResourceRequest(value)),
            }) => request!(value, v1),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ResourceRequest(value)),
            }) => request!(value, v2),
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ResourceCancel(value)),
            }) => cancel!(value),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ResourceCancel(value)),
            }) => cancel!(value),
            _ => return Ok(false),
        };
        if request.transfer().get() <= self.last_transfer
            || request.stamp().fence().connection() != principal.binding()
            || request.stamp().base().session.epoch != principal.epoch()
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let slot = self
            .active
            .iter_mut()
            .take(limits.concurrent_transfers.min(MAX_TRANSFERS))
            .find(|slot| slot.is_none())
            .ok_or_else(|| failure(ServerFailureClass::Capacity))?;
        let access = handle
            .runtime_access(principal, request.stamp().base().session)
            .await?;
        let stream = access.handle.read_graphics(request)?;
        self.last_transfer = request.transfer().get();
        *slot = Some(Transfer { request, stream });
        Ok(true)
    }
    pub(in crate::protocol_dispatch) async fn next(
        &mut self,
        egress: &ConnectionEgress,
    ) -> Result<ResourceEvent, ServerFailure> {
        if self.active.iter().all(Option::is_none) {
            return std::future::pending().await;
        }
        // Only one chunk crosses into the connection at a time, irrespective of
        // the four runtime streams. Charges include encoding and native copies.
        let reservation = egress
            .reserve_content_reply(MAX_RESOURCE_CHUNK_BYTES as u32 + CHUNK_METADATA_BYTES)
            .map_err(egress_failure)?;
        let [a, b, c, d] = &mut self.active;
        let (request, chunk) = tokio::select! {
            event = next(a) => event,
            event = next(b) => event,
            event = next(c) => event,
            event = next(d) => event,
        };
        Ok(ResourceEvent {
            request,
            chunk,
            reservation,
        })
    }
    pub(in crate::protocol_dispatch) async fn publish(
        &mut self,
        event: ResourceEvent,
        dispatcher: &ProtocolDispatcher,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        handle: &ServerHandle,
        egress: &mut ConnectionEgress,
    ) -> Result<(), ServerFailure> {
        let Some(slot) = self.active.iter_mut().find(|slot| {
            slot.as_ref()
                .is_some_and(|transfer| transfer.request == event.request)
        }) else {
            return Ok(());
        };
        if !current(handle, session.principal, event.request.stamp().fence()).await? {
            *slot = None;
            return Ok(());
        }
        match event.chunk {
            Some(Ok(chunk)) => {
                if chunk.request() != event.request {
                    return Err(failure(ServerFailureClass::ScopeMismatch));
                }
                let resource = chunk.request().resource();
                let maximum = session
                    .codec
                    .limits()
                    .resource_chunk_bytes
                    .min(MAX_RESOURCE_CHUNK_BYTES);
                if maximum == 0 {
                    return Err(failure(ServerFailureClass::Capacity));
                }
                let mut offset = chunk.offset();
                for data in chunk.bytes().chunks(maximum) {
                    // A peer may negotiate a smaller chunk than the runtime's
                    // 256 KiB quantum. Revalidate before every wire fragment.
                    if !current(handle, session.principal, event.request.stamp().fence()).await? {
                        *slot = None;
                        return Ok(());
                    }
                    let end = offset
                        .checked_add(
                            u32::try_from(data.len())
                                .map_err(|_| failure(ServerFailureClass::Capacity))?,
                        )
                        .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
                    let complete = end == resource.rgba_bytes;
                    macro_rules! encode {
                        ($p:ident, $major:ident) => {
                            Payload::$major($p::Message {
                                kind: Some($p::message::Kind::ResourceChunk($p::ResourceChunk {
                                    fence: Some(identity::encode_fence(
                                        event.request.stamp().fence(),
                                    )),
                                    resource: Some($p::ResourceId {
                                        session: Some(identity::encode_session(
                                            resource.key.session,
                                        )),
                                        local_id: resource.key.id.get(),
                                        generation: resource.key.generation,
                                    }),
                                    transfer: event.request.transfer().get(),
                                    offset: u64::from(offset),
                                    total_bytes: u64::from(resource.rgba_bytes),
                                    data: data.to_vec(),
                                    digest: resource.digest.to_vec(),
                                    complete,
                                    format: $p::ResourceFormat::Rgba8 as i32,
                                    width: resource.width,
                                    height: resource.height,
                                    stamp: Some(Box::new(content::$p::encode_stamp(
                                        event.request.stamp(),
                                    ))),
                                })),
                            })
                        };
                    }
                    let payload = match session.codec.major() {
                        1 => encode!(v1, Major1),
                        2 => encode!(v2, Major2),
                        _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
                    };
                    dispatcher
                        .send(
                            session,
                            transport,
                            &payload,
                            egress,
                            Some(event.reservation.retain()),
                        )
                        .await?;
                    offset = end;
                }
                if chunk.complete() {
                    *slot = None;
                }
                Ok(())
            }
            None | Some(Err(_)) => {
                // Cancellation is request-local. Dropping this read never
                // retires the PTY and never emits native or content diagnostics.
                let fence = Some(identity::encode_fence(event.request.stamp().fence()));
                let transfer = event.request.transfer().get();
                let payload = match session.codec.major() {
                    1 => Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ResourceCancel(v1::ResourceCancel {
                            transfer,
                            fence,
                        })),
                    }),
                    2 => Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ResourceCancel(v2::ResourceCancel {
                            transfer,
                            fence,
                        })),
                    }),
                    _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
                };
                *slot = None;
                dispatcher
                    .send(
                        session,
                        transport,
                        &payload,
                        egress,
                        Some(event.reservation),
                    )
                    .await
            }
        }
    }
}
async fn next(
    slot: &mut Option<Transfer>,
) -> (
    GraphicsRequest,
    Option<Result<GraphicsChunk, ServerFailure>>,
) {
    match slot {
        Some(transfer) => (transfer.request, transfer.stream.next().await),
        None => std::future::pending().await,
    }
}
