use super::*;
use spaceterm_core::content_queries::*;

#[derive(Clone, Debug)]
pub enum PaneReadRequest {
    Page(ContentPageRequest),
    Topology(TopologyPageRequest),
    Copy(CopyRequest),
    Find(FindRequest),
    Graphics(GraphicsRequest),
}
impl PaneReadRequest {
    pub fn stamp(&self) -> QueryStamp {
        match self {
            Self::Page(r) => r.stamp(),
            Self::Topology(r) => r.stamp(),
            Self::Copy(r) => r.stamp(),
            Self::Find(r) => r.stamp(),
            Self::Graphics(r) => r.stamp(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum PaneReadItem {
    Page(ContentPageResult),
    Topology(TopologyPageResult),
    Copy(CopyStreamItem),
    Find(FindStreamItem),
    Graphics(ContentReadResult<GraphicsChunk>),
}
pub trait PaneReadSource: Send {
    /// Cancellation and drop cancel only this read, never a runtime or grant.
    fn next(&mut self) -> ClientFuture<'_, Option<PaneReadItem>>;
}
pub struct PaneReadStream {
    source: Box<dyn PaneReadSource>,
    request: PaneReadRequest,
    permit: EffectPermit,
    _slot: tokio::sync::OwnedSemaphorePermit,
    limits: spaceterm_protocol::limits::NegotiatedLimits,
    transfer: Option<ResourceTransferId>,
    offsets: [u64; 2],
    totals: [Option<u64>; 2],
    chunks: u64,
    terminal: bool,
}
impl PaneReadStream {
    pub async fn next(&mut self) -> Result<Option<PaneReadItem>, ClientFailure> {
        if !self.permit.is_current() {
            self.terminal = true;
            return Err(stale());
        }
        if self.terminal {
            return Ok(None);
        }
        let item = match self.source.next().await {
            Ok(item) => item,
            Err(error) => {
                self.terminal = true;
                return Err(error);
            }
        };
        if !self.permit.is_current() {
            self.terminal = true;
            return Err(stale());
        }
        let Some(item) = item else {
            self.terminal = true;
            return Err(invalid_peer());
        };
        if let Err(error) = self.validate(&item) {
            self.terminal = true;
            return Err(error);
        }
        Ok(Some(item))
    }
    fn validate(&mut self, item: &PaneReadItem) -> Result<(), ClientFailure> {
        let stamp = self.request.stamp();
        match (&self.request, item) {
            (
                PaneReadRequest::Page(request),
                PaneReadItem::Page(ContentReadResult::Ready(reply)),
            ) => {
                if reply.stamp() != stamp {
                    return Err(stale());
                }
                let parts = reply.page().parts();
                let cells: usize = parts.rows.iter().map(|row| row.cells().len()).sum();
                if parts.base != stamp.base()
                    || parts.rows.len()
                        > usize::from(request.limits().rows()).min(self.limits.semantic_rows)
                    || cells > (request.limits().cells() as usize).min(self.limits.semantic_cells)
                    || reply.page().accounted_bytes() > request.limits().bytes() as usize
                {
                    return Err(resource_limit());
                }
                self.terminal = true;
            }
            (PaneReadRequest::Page(_), PaneReadItem::Page(result)) => {
                check_rejection(result, stamp)?;
                self.terminal = true;
            }
            (
                PaneReadRequest::Topology(request),
                PaneReadItem::Topology(ContentReadResult::Ready(page)),
            ) => {
                if page.stamp() != stamp
                    || page.rows().len()
                        > usize::from(request.limits().rows()).min(self.limits.semantic_rows)
                {
                    return Err(stale());
                }
                self.terminal = true;
            }
            (PaneReadRequest::Topology(_), PaneReadItem::Topology(result)) => {
                check_rejection(result, stamp)?;
                self.terminal = true;
            }
            (PaneReadRequest::Copy(request), PaneReadItem::Copy(CopyStreamItem::Chunk(chunk))) => {
                if chunk.stamp() != stamp
                    || self
                        .transfer
                        .is_some_and(|transfer| transfer != chunk.transfer())
                    || chunk.bytes().len() > self.limits.resource_chunk_bytes
                {
                    return Err(invalid_peer());
                }
                let index = if chunk.part() == CopyPart::Plain {
                    0
                } else {
                    1
                };
                if index == 1 && !request.options().include_html {
                    return Err(invalid_peer());
                }
                if chunk.offset() != self.offsets[index]
                    || self.totals[index].is_some_and(|total| total != chunk.total())
                {
                    return Err(invalid_peer());
                }
                self.offsets[index] = self.offsets[index]
                    .checked_add(chunk.bytes().len() as u64)
                    .ok_or_else(exhausted)?;
                self.totals[index] = Some(chunk.total());
                self.transfer = Some(chunk.transfer());
            }
            (
                PaneReadRequest::Copy(request),
                PaneReadItem::Copy(CopyStreamItem::Complete(completion)),
            ) => {
                if completion.stamp() != stamp
                    || self
                        .transfer
                        .is_some_and(|transfer| transfer != completion.transfer())
                    || completion.plain_bytes() != self.offsets[0]
                    || self.totals[0].is_some_and(|total| total != completion.plain_bytes())
                    || completion.html().is_some() != request.options().include_html
                    || completion.html().is_some_and(|(length, _)| {
                        length != self.offsets[1]
                            || self.totals[1].is_some_and(|total| total != length)
                    })
                {
                    return Err(invalid_peer());
                }
                self.terminal = true;
            }
            (
                PaneReadRequest::Copy(_),
                PaneReadItem::Copy(CopyStreamItem::Failed {
                    stamp: received, ..
                }),
            )
            | (
                PaneReadRequest::Find(_),
                PaneReadItem::Find(FindStreamItem::Failed {
                    stamp: received, ..
                }),
            ) => {
                if *received != stamp {
                    return Err(stale());
                }
                self.terminal = true;
            }
            (PaneReadRequest::Find(_), PaneReadItem::Find(FindStreamItem::Chunk(chunk))) => {
                if chunk.stamp() != stamp || chunk.index() != self.chunks {
                    return Err(stale());
                }
                self.chunks = self.chunks.checked_add(1).ok_or_else(exhausted)?;
            }
            (
                PaneReadRequest::Find(_),
                PaneReadItem::Find(FindStreamItem::Complete {
                    stamp: received,
                    chunks,
                }),
            ) => {
                if *received != stamp || *chunks != self.chunks {
                    return Err(stale());
                }
                self.terminal = true;
            }
            (
                PaneReadRequest::Graphics(request),
                PaneReadItem::Graphics(ContentReadResult::Ready(chunk)),
            ) => {
                if chunk.request() != *request
                    || chunk.offset() as u64 != self.offsets[0]
                    || chunk.bytes().len() > self.limits.resource_chunk_bytes
                {
                    return Err(invalid_peer());
                }
                self.offsets[0] = self.offsets[0]
                    .checked_add(chunk.bytes().len() as u64)
                    .ok_or_else(exhausted)?;
                self.terminal = chunk.complete();
            }
            (PaneReadRequest::Graphics(_), PaneReadItem::Graphics(result)) => {
                check_rejection(result, stamp)?;
                self.terminal = true;
            }
            _ => return Err(invalid_peer()),
        }
        Ok(())
    }
}
fn check_rejection<T>(
    result: &ContentReadResult<T>,
    stamp: QueryStamp,
) -> Result<(), ClientFailure> {
    match result {
        ContentReadResult::ResyncRequired { request, current }
            if *request == stamp && current.session == stamp.base().session =>
        {
            Ok(())
        }
        ContentReadResult::Rejected { request, .. } if *request == stamp => Ok(()),
        _ => Err(stale()),
    }
}
impl PaneChannel {
    pub async fn read(
        &self,
        permit: &EffectPermit,
        request: PaneReadRequest,
    ) -> Result<PaneReadStream, ClientFailure> {
        if !matches!(permit.kind, EffectKind::Read | EffectKind::NativeRead)
            || !Weak::ptr_eq(&permit.channel, &Arc::downgrade(&self.inner))
            || !permit.is_current()
            || request.stamp().fence() != permit.fence
        {
            return Err(stale());
        }
        let mut permit = permit.clone();
        if matches!(request, PaneReadRequest::Copy(_) | PaneReadRequest::Find(_)) {
            permit.kind = EffectKind::NativeRead;
            if !permit.is_current() {
                return Err(stale());
            }
        }
        let (slot, limits) = {
            let servers = self.inner.shared.servers.lock().map_err(|_| stale())?;
            let server = servers.get(&self.inner.pane.server()).ok_or_else(stale)?;
            (
                Arc::clone(&server.transfers)
                    .try_acquire_owned()
                    .map_err(|_| resource_limit())?,
                server.info.limits,
            )
        };
        if let PaneReadRequest::Page(page) = &request {
            self.inner
                .projection
                .lock()
                .map_err(|_| stale())?
                .expect_page(page.stamp())?;
        }
        let source = self
            .inner
            .shared
            .peer
            .read(permit.clone(), request.clone())
            .await?;
        if !permit.is_current() {
            return Err(stale());
        }
        let offset = if let PaneReadRequest::Graphics(request) = &request {
            u64::from(request.offset())
        } else {
            0
        };
        Ok(PaneReadStream {
            source,
            request,
            permit,
            _slot: slot,
            limits,
            transfer: None,
            offsets: [offset, 0],
            totals: [None, None],
            chunks: 0,
            terminal: false,
        })
    }
}
