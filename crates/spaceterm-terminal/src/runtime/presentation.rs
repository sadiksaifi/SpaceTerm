//! Replaceable revision notifications, separate from retained lifecycle facts.
use super::owner::Shared;
use super::ports::{RuntimeFailure, RuntimeStage};
use async_channel::{Receiver, Sender};
use spaceterm_core::authority::{ContentRevision, EffectFence};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationUpdate {
    pub fence: EffectFence,
    pub revision: ContentRevision,
    pub metadata_changed: bool,
}
pub struct PresentationSubscription {
    shared: Arc<Shared>,
    version: u64,
    receiver: Receiver<PresentationUpdate>,
}
impl PresentationSubscription {
    pub async fn next(&mut self) -> Result<Option<PresentationUpdate>, RuntimeFailure> {
        match self.receiver.recv().await {
            Ok(update) => self.validate(update).map(Some),
            Err(_) => Ok(None),
        }
    }
    pub fn next_blocking(&mut self) -> Result<Option<PresentationUpdate>, RuntimeFailure> {
        match self.receiver.recv_blocking() {
            Ok(update) => self.validate(update).map(Some),
            Err(_) => Ok(None),
        }
    }
    pub fn try_next(&mut self) -> Result<Option<PresentationUpdate>, RuntimeFailure> {
        match self.receiver.try_recv() {
            Ok(update) => self.validate(update).map(Some),
            Err(_) => Ok(None),
        }
    }
    fn validate(&self, update: PresentationUpdate) -> Result<PresentationUpdate, RuntimeFailure> {
        if !self.shared.authority.read()?.permits_stamp(
            super::authority::AuthorityStamp {
                fence: update.fence,
                version: self.version,
            },
            None,
            Some(update.revision),
        ) {
            return Err(RuntimeFailure::stale());
        }
        Ok(update)
    }
}
impl Drop for PresentationSubscription {
    fn drop(&mut self) {
        self.receiver.close();
    }
}
struct Lane {
    fence: EffectFence,
    version: u64,
    sender: Sender<PresentationUpdate>,
    replacement: Receiver<PresentationUpdate>,
}
#[derive(Default)]
pub(crate) struct Presentation(Mutex<Vec<Lane>>);
impl Presentation {
    pub fn subscribe(
        &self,
        shared: Arc<Shared>,
        fence: EffectFence,
    ) -> Result<PresentationSubscription, RuntimeFailure> {
        let authority = shared.authority.read()?;
        let revision = shared.revision();
        if !authority.permits(fence, None, Some(revision)) {
            return Err(RuntimeFailure::stale());
        }
        let mut lanes = self
            .0
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Content))?;
        lanes.retain(|lane| !lane.sender.is_closed());
        if lanes.len() >= shared.factory.limits.subscriptions {
            return Err(RuntimeFailure::capacity(RuntimeStage::Content));
        }
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(PresentationUpdate {
            fence,
            revision,
            metadata_changed: true,
        });
        let version = authority.version;
        lanes.push(Lane {
            fence,
            version,
            sender,
            replacement: receiver.clone(),
        });
        drop(lanes);
        drop(authority);
        Ok(PresentationSubscription {
            shared,
            receiver,
            version,
        })
    }
    pub fn publish(&self, shared: &Shared, metadata_changed: bool) {
        let Ok(authority) = shared.authority.read() else {
            return;
        };
        let mut lanes = self.0.lock().unwrap_or_else(|error| error.into_inner());
        lanes.retain(|lane| {
            if lane.sender.is_closed()
                || authority.version != lane.version
                || !authority.permits(lane.fence, None, Some(shared.revision()))
            {
                lane.sender.close();
                return false;
            }
            let update = PresentationUpdate {
                fence: lane.fence,
                revision: shared.revision(),
                metadata_changed,
            };
            if let Err(async_channel::TrySendError::Full(mut update)) = lane.sender.try_send(update)
            {
                if let Ok(previous) = lane.replacement.try_recv() {
                    update.metadata_changed |= previous.metadata_changed;
                }
                let _ = lane.sender.try_send(update);
            }
            true
        });
    }
    pub fn clear(&self) {
        for lane in self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
        {
            lane.sender.close();
        }
    }
}
