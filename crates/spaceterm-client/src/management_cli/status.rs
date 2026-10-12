//! One replaceable authenticated status value. Gaps request the worker's resync.
//! Waiters learn that the value changed, never how many publications arrived.

use std::sync::{Arc, Mutex};

use spaceterm_core::{
    authority::{ConnectionBinding, ServerEpoch},
    identity::ServerId,
    management::{ServerStatus, StatusPublication, StatusQuery},
};

use tokio::sync::watch;

use super::ports::ManagementFailure;

pub trait StatusResync: Send + Sync {
    /// Schedule the existing connection-worker query, without starting or enrolling.
    /// The worker coalesces requests into one pending query while a query is in flight.
    fn request_resync(&self);
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusUpdate {
    Replaced,
    Ignored,
    ResyncRequired,
}
struct SubscriptionState {
    latest: Option<ServerStatus>,
    // Gapped publications still establish a comparison floor for resync snapshots.
    observed_epoch: Option<ServerEpoch>,
    observed_revision: Option<std::num::NonZeroU64>,
    resync_required: bool,
}
/// Clones share a single latest value, not a queue of status or terminal data.
/// Each clone tracks which change it last observed.
#[derive(Clone)]
pub struct StatusSubscription {
    server: ServerId,
    query: StatusQuery,
    state: Arc<Mutex<SubscriptionState>>,
    resync: Arc<dyn StatusResync>,
    changes: watch::Sender<u64>,
    observed: watch::Receiver<u64>,
}
impl StatusSubscription {
    pub fn new(
        server: ServerId,
        query: StatusQuery,
        resync: Arc<dyn StatusResync>,
    ) -> Result<Self, ManagementFailure> {
        if server.bytes() == [0; 16]
            || query.connection.client.server() != server
            || query.connection.generation.get() == 0
            || query.generation == 0
            || query.known_epoch.is_some_and(|epoch| epoch.get() == 0)
            || (query.known_revision.is_some() && query.known_epoch.is_none())
            || !query.subscribe
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        let (changes, observed) = watch::channel(0);
        Ok(Self {
            server,
            query,
            state: Arc::new(Mutex::new(SubscriptionState {
                latest: None,
                observed_epoch: query.known_epoch,
                observed_revision: query.known_revision,
                resync_required: false,
            })),
            resync,
            changes,
            observed,
        })
    }
    pub fn server(&self) -> ServerId {
        self.server
    }
    pub fn connection(&self) -> ConnectionBinding {
        self.query.connection
    }
    /// Resolves once the latest value was replaced or a resync became required
    /// after this handle's previous observation. Bursts coalesce into one wake.
    pub async fn changed(&mut self) -> Result<(), ManagementFailure> {
        // Every handle owns a sender, so the channel cannot close while waiting.
        self.observed
            .changed()
            .await
            .map_err(|_| ManagementFailure::Offline)
    }
    fn publish_change(&self) {
        self.changes
            .send_modify(|count| *count = count.wrapping_add(1));
    }
    pub fn latest(&self) -> Result<Option<ServerStatus>, ManagementFailure> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?
            .latest
            .clone())
    }
    pub fn needs_resync(&self) -> Result<bool, ManagementFailure> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?
            .resync_required)
    }
    pub fn apply(&self, publication: StatusPublication) -> Result<StatusUpdate, ManagementFailure> {
        let (request, generation) = match &publication {
            StatusPublication::Snapshot {
                request,
                generation,
                ..
            }
            | StatusPublication::Changed {
                request,
                generation,
                ..
            }
            | StatusPublication::ResyncRequired {
                request,
                generation,
            } => (*request, *generation),
        };
        if request != self.query.request || generation != self.query.generation {
            return Ok(StatusUpdate::Ignored);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        let incoming = match &publication {
            StatusPublication::Snapshot { status, .. }
            | StatusPublication::Changed { status, .. } => Some(status),
            _ => None,
        };
        if incoming.is_some_and(|s| s.parts().server != self.server) {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        if incoming.is_some_and(|s| older_than(s, state.observed_epoch, state.observed_revision)) {
            if state.resync_required && matches!(publication, StatusPublication::Snapshot { .. }) {
                // A delayed query reply consumed a resync attempt without reaching
                // the observed floor. Schedule one replacement, not one per delta.
                drop(state);
                self.resync.request_resync();
                return Ok(StatusUpdate::ResyncRequired);
            }
            return Ok(StatusUpdate::Ignored);
        }
        if let Some(status) = incoming {
            state.observed_epoch = Some(status.parts().epoch);
            state.observed_revision = Some(status.parts().revision);
        }
        let gap = match &publication {
            StatusPublication::Changed { base, status, .. } => {
                state.resync_required
                    || state.latest.as_ref().is_none_or(|old| {
                        old.parts().epoch != status.parts().epoch
                            || old.parts().revision != *base
                            || status.parts().revision <= *base
                    })
            }
            StatusPublication::ResyncRequired { .. } => true,
            _ => false,
        };
        if gap {
            let schedule = !state.resync_required;
            state.resync_required = true;
            drop(state);
            if schedule {
                self.publish_change();
                self.resync.request_resync();
            }
            return Ok(StatusUpdate::ResyncRequired);
        }
        let Some(status) = incoming else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        state.latest = Some(status.clone());
        state.resync_required = false;
        drop(state);
        self.publish_change();
        Ok(StatusUpdate::Replaced)
    }
}

fn older_than(
    status: &ServerStatus,
    epoch: Option<ServerEpoch>,
    revision: Option<std::num::NonZeroU64>,
) -> bool {
    epoch.is_some_and(|epoch| {
        status.parts().epoch.get() < epoch.get()
            || (status.parts().epoch == epoch
                && revision.is_some_and(|revision| status.parts().revision < revision))
    })
}
