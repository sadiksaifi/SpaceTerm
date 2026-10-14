//! Disposable read jobs with one unconsumed runtime item per job.
use super::*;
use spaceterm_protocol::convert::content::reads::{self as codec, ReadQuery};
use tokio::sync::oneshot;

const MAX_READS: usize = spaceterm_protocol::limits::MAX_CONCURRENT_TRANSFERS;
struct Job {
    query: ReadQuery,
    task: AbortHandle,
}
enum Item {
    Topology(Box<TopologyPageResult>),
    Copy(CopyStreamItem),
    Find(FindStreamItem),
    Failed(ContentReadFailure),
}
impl Item {
    fn finished(&self) -> bool {
        !matches!(
            self,
            Self::Copy(CopyStreamItem::Chunk(_)) | Self::Find(FindStreamItem::Chunk(_))
        )
    }
    fn needs_content_check(&self) -> bool {
        match self {
            Self::Failed(_) => false,
            Self::Topology(value) => matches!(value.as_ref(), ContentReadResult::Ready(_)),
            _ => true,
        }
    }
    fn stamp(&self) -> Option<QueryStamp> {
        match self {
            Self::Copy(CopyStreamItem::Chunk(value)) => Some(value.stamp()),
            Self::Copy(CopyStreamItem::Complete(value)) => Some(value.stamp()),
            Self::Copy(CopyStreamItem::Failed { stamp, .. }) => Some(*stamp),
            Self::Find(FindStreamItem::Chunk(value)) => Some(value.stamp()),
            Self::Find(
                FindStreamItem::Complete { stamp, .. } | FindStreamItem::Failed { stamp, .. },
            ) => Some(*stamp),
            Self::Topology(value) => Some(match value.as_ref() {
                ContentReadResult::Ready(value) => value.stamp(),
                ContentReadResult::Rejected { request, .. }
                | ContentReadResult::ResyncRequired { request, .. } => *request,
            }),
            Self::Failed(_) => None,
        }
    }
}
// Split lazily so even a one-byte negotiated chunk limit retains only one
// source chunk and one fragment, rather than allocating a fragment vector.
enum PendingItems {
    Copy {
        request: Box<CopyRequest>,
        chunk: CopyChunk,
        sent: usize,
        maximum: usize,
    },
    Single(Option<Item>),
}
impl Iterator for PendingItems {
    type Item = Item;
    fn next(&mut self) -> Option<Item> {
        match self {
            Self::Single(value) => value.take(),
            Self::Copy {
                request,
                chunk,
                sent,
                maximum,
            } => {
                let remaining = &chunk.bytes()[*sent..];
                if remaining.is_empty() {
                    return None;
                }
                let mut length = remaining.len().min(*maximum);
                while length > 0 && std::str::from_utf8(&remaining[..length]).is_err() {
                    length -= 1;
                }
                if length == 0 {
                    *sent = chunk.bytes().len();
                    return Some(Item::Failed(ContentReadFailure::Capacity));
                }
                let value = CopyChunk::new(
                    request,
                    chunk.transfer(),
                    chunk.part(),
                    chunk.offset() + *sent as u64,
                    chunk.total(),
                    remaining[..length].to_vec(),
                );
                *sent += length;
                Some(match value {
                    Ok(value) => Item::Copy(CopyStreamItem::Chunk(value)),
                    Err(_) => Item::Failed(ContentReadFailure::Capacity),
                })
            }
        }
    }
}
pub(in crate::protocol_dispatch) struct ReadEvent {
    query: ReadQuery,
    item: Item,
    consumed: oneshot::Sender<()>,
    admitted: bool,
}
pub(super) struct ReadConnection {
    limits: spaceterm_protocol::limits::NegotiatedLimits,
    jobs: BTreeMap<u64, Job>,
    sender: mpsc::Sender<ReadEvent>,
    receiver: mpsc::Receiver<ReadEvent>,
}
impl Drop for ReadConnection {
    fn drop(&mut self) {
        self.reset();
    }
}
impl ReadConnection {
    pub(super) fn new(limits: spaceterm_protocol::limits::NegotiatedLimits) -> Self {
        let (sender, receiver) = mpsc::channel(2 * MAX_READS);
        Self {
            limits,
            jobs: BTreeMap::new(),
            sender,
            receiver,
        }
    }
    pub(super) fn reset(&mut self) {
        for (_, job) in std::mem::take(&mut self.jobs) {
            job.task.abort();
        }
    }
    pub(super) fn remove_pane(&mut self, pane: PaneId) {
        self.jobs.retain(|_, job| {
            if job.query.stamp().base().session.pane == pane {
                job.task.abort();
                false
            } else {
                true
            }
        });
    }
    pub(super) async fn next(&mut self) -> Option<ReadEvent> {
        self.receiver.recv().await
    }
    pub(super) async fn admit(
        &mut self,
        payload: &Payload,
        principal: VerifiedPrincipal,
        handle: &ServerHandle,
    ) -> Result<bool, ServerFailure> {
        let query = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ContentQuery(value)),
            }) if matches!(value.kind, 3..=5) => {
                codec::v1::decode_query(value).map_err(protocol_failure)?
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ContentQuery(value)),
            }) if matches!(value.kind, 3..=5) => {
                codec::v2::decode_query(value).map_err(protocol_failure)?
            }
            _ => return Ok(false),
        };
        let stamp = query.stamp();
        let id = stamp.request().get();
        if stamp.fence().connection() != principal.binding()
            || stamp.base().session.epoch != principal.epoch()
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        if let ReadQuery::Cancel { .. } = query {
            if self
                .jobs
                .get(&id)
                .is_some_and(|job| job.query.stamp() == stamp && job.query.kind() == query.kind())
                && let Some(job) = self.jobs.remove(&id)
            {
                job.task.abort();
                self.reject(job.query, ContentReadFailure::Cancelled)?;
            }
            return Ok(true);
        }
        if let Some(old) = self.jobs.get(&id) {
            if stamp.generation() <= old.query.stamp().generation() {
                self.reject(query, ContentReadFailure::StaleAuthority)?;
                return Ok(true);
            }
            if let Some(old) = self.jobs.remove(&id) {
                old.task.abort();
            }
        }
        match current(handle, principal, stamp.fence()).await {
            Ok(true) => {}
            result => {
                self.reject(
                    query,
                    result
                        .err()
                        .map_or(ContentReadFailure::StaleAuthority, read_failure),
                )?;
                return Ok(true);
            }
        }
        if self.jobs.len() >= self.limits.concurrent_transfers.min(MAX_READS) {
            self.reject(query, ContentReadFailure::Capacity)?;
            return Ok(true);
        }
        let query = match query {
            ReadQuery::Topology(request) => ReadQuery::Topology(
                TopologyPageRequest::new(
                    request.stamp(),
                    request.continuation(),
                    PageLimits::new(
                        request.limits().rows(),
                        request.limits().cells(),
                        request.limits().bytes().min(PAGE_BYTES),
                    )
                    .map_err(|_| failure(ServerFailureClass::Capacity))?,
                )
                .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
            ),
            query => query,
        };
        let sender = self.sender.clone();
        let worker_query = query.clone();
        let handle = handle.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = run(worker_query.clone(), principal, handle, sender.clone()).await {
                let (consumed, _) = oneshot::channel();
                let _ = sender
                    .send(ReadEvent {
                        query: worker_query,
                        item: Item::Failed(read_failure(error)),
                        consumed,
                        admitted: true,
                    })
                    .await;
            }
        })
        .abort_handle();
        self.jobs.insert(id, Job { query, task });
        Ok(true)
    }
    fn reject(&self, query: ReadQuery, reason: ContentReadFailure) -> Result<(), ServerFailure> {
        let (consumed, _) = oneshot::channel();
        self.sender
            .try_send(ReadEvent {
                query,
                item: Item::Failed(reason),
                consumed,
                admitted: false,
            })
            .map_err(|_| failure(ServerFailureClass::Capacity))
    }
    pub(super) async fn publish(
        &mut self,
        event: ReadEvent,
        dispatcher: &ProtocolDispatcher,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        handle: &ServerHandle,
        egress: &mut ConnectionEgress,
    ) -> Result<(), ServerFailure> {
        let stamp = event.query.stamp();
        let id = stamp.request().get();
        if event.admitted
            && self
                .jobs
                .get(&id)
                .is_none_or(|job| job.query != event.query)
        {
            return Ok(());
        }
        if event.item.stamp().is_some_and(|value| value != stamp) {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        let maximum = self
            .limits
            .resource_chunk_bytes
            .min(MAX_RESOURCE_CHUNK_BYTES)
            .min(self.limits.frame_bytes.saturating_sub(4608));
        let items = match (&event.query, event.item) {
            (ReadQuery::Copy(request), Item::Copy(CopyStreamItem::Chunk(chunk))) => {
                PendingItems::Copy {
                    request: Box::new(*request),
                    chunk,
                    sent: 0,
                    maximum,
                }
            }
            (_, item) => PendingItems::Single(Some(item)),
        };
        let mut finished = false;
        for mut item in items {
            match current(handle, session.principal, stamp.fence()).await {
                Ok(true) => {}
                result => {
                    item = Item::Failed(
                        result
                            .err()
                            .map_or(ContentReadFailure::StaleAuthority, read_failure),
                    )
                }
            }
            // The worker validates each produced item. Recheck its exact content
            // base after queueing, including immediately before final completion.
            if item.needs_content_check() {
                match exact_content(handle, session.principal, stamp).await {
                    Ok(true) => {}
                    Ok(false) => item = Item::Failed(ContentReadFailure::StaleAuthority),
                    Err(error) => item = Item::Failed(read_failure(error)),
                }
            }
            if !matches!(item, Item::Failed(_))
                && item_budget(&item) as usize + 512 > self.limits.frame_bytes
            {
                item = Item::Failed(ContentReadFailure::Capacity);
            }
            let bytes = item_budget(&item);
            let reservation = match egress.reserve_content_reply(bytes) {
                Ok(value) => value,
                Err(EgressFailure::Capacity) => {
                    item = Item::Failed(ContentReadFailure::Capacity);
                    egress.reserve_reliable(4096).map_err(egress_failure)?
                }
                Err(error) => return Err(egress_failure(error)),
            };
            finished = item.finished();
            macro_rules! payload {
                ($p:ident, $major:ident) => {{
                    let kind = match &item {
                        Item::Topology(value) => $p::message::Kind::ContentReadResponse(Box::new(
                            codec::$p::encode_topology_result(value),
                        )),
                        Item::Copy(value) => {
                            $p::message::Kind::CopyEvent(codec::$p::encode_copy_event(value))
                        }
                        Item::Find(value) => {
                            $p::message::Kind::FindEvent(codec::$p::encode_find_event(value))
                        }
                        Item::Failed(reason) => match event.query.kind() {
                            3 => $p::message::Kind::ContentReadResponse(Box::new(
                                codec::$p::encode_topology_result(&ContentReadResult::Rejected {
                                    request: stamp,
                                    reason: *reason,
                                }),
                            )),
                            4 => $p::message::Kind::FindEvent(codec::$p::encode_find_event(
                                &FindStreamItem::Failed {
                                    stamp,
                                    reason: *reason,
                                },
                            )),
                            _ => $p::message::Kind::CopyEvent(codec::$p::encode_copy_event(
                                &CopyStreamItem::Failed {
                                    stamp,
                                    reason: *reason,
                                },
                            )),
                        },
                    };
                    Payload::$major($p::Message { kind: Some(kind) })
                }};
            }
            let payload = match session.codec.major() {
                1 => payload!(v1, Major1),
                2 => payload!(v2, Major2),
                _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
            };
            dispatcher
                .send(session, transport, &payload, egress, Some(reservation))
                .await?;
            if finished {
                break;
            }
        }
        let _ = event.consumed.send(());
        if finished
            && event.admitted
            && let Some(job) = self.jobs.remove(&id)
        {
            job.task.abort();
        }
        Ok(())
    }
}
fn read_failure(error: ServerFailure) -> ContentReadFailure {
    match error.class {
        ServerFailureClass::StaleAuthority => ContentReadFailure::StaleAuthority,
        ServerFailureClass::Exhausted => ContentReadFailure::Exhausted,
        ServerFailureClass::RuntimeUnavailable => ContentReadFailure::Retired,
        _ => ContentReadFailure::Capacity,
    }
}
fn item_budget(item: &Item) -> u32 {
    match item {
        Item::Copy(CopyStreamItem::Chunk(value)) => (value.bytes().len() + 4096) as u32,
        Item::Find(FindStreamItem::Chunk(value)) => (value.matches().len() * 512 + 4096) as u32,
        Item::Topology(value) => match value.as_ref() {
            ContentReadResult::Ready(value) => (value.rows().len() * 256 + 4096) as u32,
            _ => 4096,
        },
        _ => 4096,
    }
}
async fn exact_content(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    stamp: QueryStamp,
) -> Result<bool, ServerFailure> {
    let access = handle
        .runtime_access(principal, stamp.base().session)
        .await?;
    let request = TopologyPageRequest::new(
        stamp,
        None,
        PageLimits::new(1, 16384, 8192).map_err(|_| failure(ServerFailureClass::Capacity))?,
    )
    .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
    Ok(matches!(
        access.handle.read_topology(request)?.await?,
        ContentReadResult::Ready(_)
    ))
}
async fn deliver(sender: &mpsc::Sender<ReadEvent>, query: &ReadQuery, item: Item) -> bool {
    let (consumed, receipt) = oneshot::channel();
    sender
        .send(ReadEvent {
            query: query.clone(),
            item,
            consumed,
            admitted: true,
        })
        .await
        .is_ok()
        && receipt.await.is_ok()
}
// Native formatting can emit one short fragment per row. Coalesce only
// contiguous bytes of one part, never a whole selection or rich document.
const COPY_BATCH_BYTES: usize = 64 * 1024;
struct CopyBuffer {
    transfer: ResourceTransferId,
    part: CopyPart,
    offset: u64,
    total: u64,
    bytes: Vec<u8>,
}
impl CopyBuffer {
    fn matches(&self, chunk: &CopyChunk) -> bool {
        self.transfer == chunk.transfer()
            && self.part == chunk.part()
            && self.total == chunk.total()
            && self.offset + self.bytes.len() as u64 == chunk.offset()
            && self.bytes.len() + chunk.bytes().len() <= COPY_BATCH_BYTES
    }
    fn start(chunk: &CopyChunk) -> Self {
        Self {
            transfer: chunk.transfer(),
            part: chunk.part(),
            offset: chunk.offset(),
            total: chunk.total(),
            bytes: chunk.bytes().to_vec(),
        }
    }
    fn finish(self, request: &CopyRequest) -> Result<CopyStreamItem, ServerFailure> {
        Ok(CopyStreamItem::Chunk(
            CopyChunk::new(
                request,
                self.transfer,
                self.part,
                self.offset,
                self.total,
                self.bytes,
            )
            .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
        ))
    }
}
async fn flush_find(
    sender: &mpsc::Sender<ReadEvent>,
    query: &ReadQuery,
    request: &FindRequest,
    matches: &mut Vec<FindMatch>,
    continuation: Option<FindContinuation>,
    index: &mut u64,
) -> Result<bool, ServerFailure> {
    if matches.is_empty() {
        return Ok(true);
    }
    let chunk = FindChunk::new(request, *index, std::mem::take(matches), continuation)
        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
    *index = index
        .checked_add(1)
        .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
    Ok(deliver(
        sender,
        query,
        Item::Find(FindStreamItem::Chunk(Box::new(chunk))),
    )
    .await)
}
async fn run(
    query: ReadQuery,
    principal: VerifiedPrincipal,
    handle: ServerHandle,
    sender: mpsc::Sender<ReadEvent>,
) -> Result<(), ServerFailure> {
    let access = handle
        .runtime_access(principal, query.stamp().base().session)
        .await?;
    match &query {
        ReadQuery::Topology(request) => {
            deliver(
                &sender,
                &query,
                Item::Topology(Box::new(access.handle.read_topology(*request)?.await?)),
            )
            .await;
        }
        ReadQuery::Copy(request) => {
            let mut stream = access.handle.prepare_copy(*request)?;
            let mut buffer: Option<CopyBuffer> = None;
            loop {
                let item = match stream.next().await {
                    Some(Ok(CopyStreamItem::Chunk(chunk))) => {
                        if chunk.stamp() != request.stamp() {
                            return Err(failure(ServerFailureClass::ScopeMismatch));
                        }
                        if buffer
                            .as_ref()
                            .is_some_and(|buffer| !buffer.matches(&chunk))
                            && let Some(previous) = buffer.take()
                            && !deliver(&sender, &query, Item::Copy(previous.finish(request)?))
                                .await
                        {
                            break;
                        }
                        if chunk.bytes().len() >= COPY_BATCH_BYTES {
                            Item::Copy(CopyStreamItem::Chunk(chunk))
                        } else {
                            if let Some(buffer) = &mut buffer {
                                buffer.bytes.extend_from_slice(chunk.bytes());
                            } else {
                                buffer = Some(CopyBuffer::start(&chunk));
                            }
                            continue;
                        }
                    }
                    Some(Ok(value)) => {
                        if let Some(previous) = buffer.take()
                            && !deliver(&sender, &query, Item::Copy(previous.finish(request)?))
                                .await
                        {
                            break;
                        }
                        Item::Copy(value)
                    }
                    Some(Err(error)) => Item::Failed(read_failure(error)),
                    None => Item::Failed(ContentReadFailure::Cancelled),
                };
                let finished = item.finished();
                if !deliver(&sender, &query, item).await || finished {
                    break;
                }
            }
        }
        ReadQuery::Find(request) => {
            let mut stream = access.handle.find(request.clone())?;
            let mut matches = Vec::new();
            let mut continuation = None;
            let mut source_index = 0;
            let mut index = 0;
            loop {
                let item = match stream.next().await {
                    Some(Ok(FindStreamItem::Chunk(chunk))) => {
                        if chunk.stamp() != request.stamp() || chunk.index() != source_index {
                            return Err(failure(ServerFailureClass::ScopeMismatch));
                        }
                        source_index = source_index
                            .checked_add(1)
                            .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
                        if matches.len() + chunk.matches().len() > MAX_FIND_CHUNK_MATCHES
                            && !flush_find(
                                &sender,
                                &query,
                                request,
                                &mut matches,
                                continuation,
                                &mut index,
                            )
                            .await?
                        {
                            break;
                        }
                        matches.extend_from_slice(chunk.matches());
                        continuation = chunk.continuation();
                        if matches.len() == MAX_FIND_CHUNK_MATCHES
                            && !flush_find(
                                &sender,
                                &query,
                                request,
                                &mut matches,
                                continuation,
                                &mut index,
                            )
                            .await?
                        {
                            break;
                        }
                        continue;
                    }
                    Some(Ok(FindStreamItem::Complete { stamp, chunks })) => {
                        if stamp != request.stamp() || chunks != source_index {
                            return Err(failure(ServerFailureClass::ScopeMismatch));
                        }
                        if !flush_find(
                            &sender,
                            &query,
                            request,
                            &mut matches,
                            continuation,
                            &mut index,
                        )
                        .await?
                        {
                            break;
                        }
                        Item::Find(FindStreamItem::Complete {
                            stamp,
                            chunks: index,
                        })
                    }
                    Some(Ok(value)) => Item::Find(value),
                    Some(Err(error)) => Item::Failed(read_failure(error)),
                    None => Item::Failed(ContentReadFailure::Cancelled),
                };
                let finished = item.finished();
                if !deliver(&sender, &query, item).await || finished {
                    break;
                }
            }
        }
        ReadQuery::Cancel { .. } => {}
    }
    Ok(())
}
