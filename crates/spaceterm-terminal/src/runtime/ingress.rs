//! Bounded request lanes and cancellation-aware single-result replies.
use super::budget::BudgetLease;
use super::ports::{RuntimeFailure, RuntimeStage};
use crate::runtime_contracts::{EffectDisposition, RuntimeCommand};
use async_channel::{Receiver, Sender};
use spaceterm_core::content_queries::*;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub(crate) type DeliveryCheck<T> = Arc<dyn Fn(T) -> Result<T, RuntimeFailure> + Send + Sync>;
pub(crate) type ReadCheck<T> = DeliveryCheck<ContentReadResult<T>>;
pub struct RuntimeReply<T> {
    receiver: Receiver<Result<T, RuntimeFailure>>,
    _lease: Arc<BudgetLease>,
    check: Option<DeliveryCheck<T>>,
}
impl<T> RuntimeReply<T> {
    pub async fn receive(self) -> Result<T, RuntimeFailure> {
        let value = self
            .receiver
            .recv()
            .await
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        self.deliver(value)
    }
    /// Blocking-executor convenience. Never call on a UI or async reactor thread.
    pub fn receive_blocking(self) -> Result<T, RuntimeFailure> {
        let value = self
            .receiver
            .recv_blocking()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        self.deliver(value)
    }
    pub fn try_receive(&self) -> Result<Option<Result<T, RuntimeFailure>>, RuntimeFailure> {
        match self.receiver.try_recv() {
            Ok(value) => Ok(Some(self.deliver(value))),
            Err(async_channel::TryRecvError::Empty) => Ok(None),
            Err(_) => Err(RuntimeFailure::internal(RuntimeStage::Cleanup)),
        }
    }
}
pub(crate) struct ReplySender<T> {
    sender: Sender<Result<T, RuntimeFailure>>,
    _lease: Arc<BudgetLease>,
    _check: Option<DeliveryCheck<T>>,
}
pub(crate) fn reply<T>(lease: BudgetLease) -> (ReplySender<T>, RuntimeReply<T>) {
    checked_reply(lease, None)
}
pub(crate) fn checked_reply<T>(
    lease: BudgetLease,
    check: Option<DeliveryCheck<T>>,
) -> (ReplySender<T>, RuntimeReply<T>) {
    let (sender, receiver) = async_channel::bounded(1);
    let lease = Arc::new(lease);
    (
        ReplySender {
            sender,
            _lease: Arc::clone(&lease),
            _check: check.clone(),
        },
        RuntimeReply {
            receiver,
            _lease: lease,
            check,
        },
    )
}
impl<T> RuntimeReply<T> {
    fn deliver(&self, value: Result<T, RuntimeFailure>) -> Result<T, RuntimeFailure> {
        match (&self.check, value) {
            (Some(check), Ok(value)) => check(value),
            (_, value) => value,
        }
    }
}
impl<T> ReplySender<T> {
    pub fn cancelled(&self) -> bool {
        self.sender.is_closed()
    }
    pub fn complete(self, result: Result<T, RuntimeFailure>) {
        let _ = self.sender.try_send(result);
    }
}
impl<T> Drop for RuntimeReply<T> {
    fn drop(&mut self) {
        self.receiver.close();
    }
}
/// Two retained chunks; closing the stream cancels only this resource request.
type StreamCheck<T> = Box<dyn Fn(&T) -> Result<(), RuntimeFailure> + Send + Sync>;
#[derive(Default)]
pub(crate) struct StreamFailure(pub Mutex<Option<RuntimeFailure>>);
pub struct ResourceStream<T> {
    pub(crate) receiver: Receiver<Result<T, RuntimeFailure>>,
    pub(crate) _lease: Arc<BudgetLease>,
    pub(crate) failure: Arc<StreamFailure>,
    pub(crate) check: Option<StreamCheck<T>>,
    pub(crate) cancelled: bool,
}
impl<T> ResourceStream<T> {
    pub async fn next(&mut self) -> Result<Option<T>, RuntimeFailure> {
        let value = self.receiver.recv().await;
        self.deliver(value.ok())
    }
    pub fn next_blocking(&mut self) -> Result<Option<T>, RuntimeFailure> {
        let value = self.receiver.recv_blocking();
        self.deliver(value.ok())
    }
    pub fn try_next(&mut self) -> Result<Option<T>, RuntimeFailure> {
        match self.receiver.try_recv() {
            Ok(value) => self.deliver(Some(value)),
            Err(async_channel::TryRecvError::Empty) => Ok(None),
            Err(_) => self.deliver(None),
        }
    }
    fn deliver(
        &mut self,
        value: Option<Result<T, RuntimeFailure>>,
    ) -> Result<Option<T>, RuntimeFailure> {
        if self.cancelled {
            return Ok(None);
        }
        if let Some(error) = self
            .failure
            .0
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Content))?
            .take()
        {
            self.receiver.close();
            return Err(error);
        }
        match value {
            Some(Ok(value)) => {
                if let Some(check) = &self.check
                    && let Err(error) = check(&value)
                {
                    self.receiver.close();
                    return Err(error);
                }
                Ok(Some(value))
            }
            Some(Err(error)) => {
                self.receiver.close();
                Err(error)
            }
            None => Ok(None),
        }
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
    }
}
impl<T> Drop for ResourceStream<T> {
    fn drop(&mut self) {
        self.receiver.close();
    }
}
pub(crate) enum StreamSink {
    Copy(
        Sender<Result<CopyStreamItem, RuntimeFailure>>,
        Arc<StreamFailure>,
        Receiver<Result<CopyStreamItem, RuntimeFailure>>,
    ),
    Find(
        Sender<Result<FindStreamItem, RuntimeFailure>>,
        Arc<StreamFailure>,
        Receiver<Result<FindStreamItem, RuntimeFailure>>,
    ),
    Graphics(
        Sender<Result<GraphicsChunk, RuntimeFailure>>,
        Arc<StreamFailure>,
        Receiver<Result<GraphicsChunk, RuntimeFailure>>,
    ),
}
type QueryKey = (spaceterm_core::identity::ClientId, u64, u64, u64);
struct QueryEntry {
    generation: RequestGeneration,
    references: usize,
}
#[derive(Default)]
pub(crate) struct QueryRegistry(Mutex<BTreeMap<QueryKey, QueryEntry>>);
impl QueryRegistry {
    fn key(stamp: QueryStamp) -> QueryKey {
        (
            stamp.fence().connection().client,
            stamp.fence().connection().generation.get(),
            stamp.fence().context().get(),
            stamp.request().get(),
        )
    }
    pub fn register(
        self: &Arc<Self>,
        stamp: QueryStamp,
        version: u64,
    ) -> Result<Arc<QueryRegistration>, RuntimeFailure> {
        let mut entries = self
            .0
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Content))?;
        let key = Self::key(stamp);
        if entries.len() >= 256 && !entries.contains_key(&key) {
            return Err(RuntimeFailure::capacity(RuntimeStage::Content));
        }
        let entry = entries.entry(key).or_insert(QueryEntry {
            generation: stamp.generation(),
            references: 0,
        });
        if stamp.generation().get() < entry.generation.get() {
            return Err(RuntimeFailure::stale());
        }
        entry.generation = stamp.generation();
        entry.references = entry
            .references
            .checked_add(1)
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Content))?;
        Ok(Arc::new(QueryRegistration {
            registry: Arc::clone(self),
            key,
            version,
        }))
    }
    pub fn current(&self, stamp: QueryStamp) -> bool {
        self.0.lock().is_ok_and(|entries| {
            entries
                .get(&Self::key(stamp))
                .is_some_and(|entry| entry.generation == stamp.generation())
        })
    }
}
pub(crate) struct QueryRegistration {
    pub version: u64,
    registry: Arc<QueryRegistry>,
    key: QueryKey,
}
impl Drop for QueryRegistration {
    fn drop(&mut self) {
        let mut entries = self
            .registry
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = entries.get_mut(&self.key) {
            entry.references -= 1;
            if entry.references == 0 {
                entries.remove(&self.key);
            }
        }
    }
}
pub(crate) enum Request {
    Command {
        stamp: super::authority::AuthorityStamp,
        command: RuntimeCommand,
        reply: ReplySender<EffectDisposition>,
        input: BudgetLease,
    },
    Page(ContentPageRequest, ReplySender<ContentPageResult>, u64),
    Topology(TopologyPageRequest, ReplySender<TopologyPageResult>, u64),
    Resource(
        super::ports::ResourceRequest,
        StreamSink,
        Arc<BudgetLease>,
        Arc<QueryRegistration>,
    ),
}
pub(crate) struct Envelope {
    pub request: Request,
    pub _lease: BudgetLease,
}
#[derive(Default)]
pub(crate) struct Ingress(pub Mutex<VecDeque<Envelope>>);
