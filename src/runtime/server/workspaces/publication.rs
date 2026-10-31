//! Pushes Workspace Inventory to every connection once the revision it reflects is retained.
//!
//! The host stages an inventory under the `PaneService` lock with the newest journal revision. A
//! staged inventory reaches connections only after the journal has written that revision, so no
//! client ever sees organization the server could still lose. The write that retains it releases
//! it from the sync worker, before the replies that wait for the same revision.
use super::Id;
use super::journal::{Journal, Revision};
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::hierarchy::Inventory;
use spaceterm_server::protocol::InventoryRevision;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
pub(super) struct Publication(Mutex<State>);

#[derive(Default)]
struct State {
    subscribers: BTreeMap<Id, Subscriber>,
    /// Inventories waiting for their revision, oldest first.
    staged: VecDeque<Staged>,
    published: Option<Inventory>,
}

/// One admitted connection.
struct Subscriber {
    device: Id,
    outbox: Outbox,
    /// The next operation sequence last pushed to this client.
    sent: Option<u64>,
}

/// An inventory and each subscribed client's next operation sequence, as of `revision`.
pub(super) struct Staged {
    pub revision: Option<Revision>,
    pub inventory: Inventory,
    pub sequences: BTreeMap<Id, u64>,
}

impl Publication {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn subscribe(&self, device: Id, connection: Id, outbox: Outbox) {
        self.state().subscribers.insert(
            connection,
            Subscriber {
                device,
                outbox,
                sent: None,
            },
        );
    }

    pub(super) fn unsubscribe(&self, connection: Id) {
        self.state().subscribers.remove(&connection);
    }

    /// The clients with a subscribed connection.
    pub(super) fn devices(&self) -> std::collections::BTreeSet<Id> {
        let state = self.state();
        state.subscribers.values().map(|s| s.device).collect()
    }

    pub(super) fn outbox(&self, connection: Id) -> Option<Outbox> {
        self.state()
            .subscribers
            .get(&connection)
            .map(|subscriber| subscriber.outbox.clone())
    }

    /// Pushes `staged` now if its revision is already retained, or once a write retains it.
    pub(super) fn stage(&self, staged: Staged, journal: Option<&Journal>) {
        let mut state = self.state();
        let retained = match (staged.revision, journal) {
            (Some(revision), Some(journal)) => journal.retained(revision),
            _ => true,
        };
        if retained {
            // Everything staged earlier is older.
            state.staged.clear();
            state.push(&staged);
        } else {
            state.staged.push_back(staged);
        }
    }

    /// A write retained `written`: pushes the newest staged inventory it covers and forgets
    /// older ones.
    pub(super) fn release(&self, written: Revision) {
        let mut state = self.state();
        let mut newest = None;
        while let Some(staged) = state.staged.front() {
            match staged.revision {
                Some(revision) if written.covers(revision) => {
                    newest = state.staged.pop_front();
                }
                // A revision lost to a failed write never becomes retained.
                Some(revision) if revision.lost_by(written) => {
                    state.staged.pop_front();
                }
                _ => break,
            }
        }
        if let Some(staged) = newest {
            state.push(&staged);
        }
    }
}

impl State {
    /// Pushes to each connection that has not received this inventory and sequence.
    fn push(&mut self, staged: &Staged) {
        let changed = self.published.as_ref() != Some(&staged.inventory);
        for subscriber in self.subscribers.values_mut() {
            let Some(next_sequence) = staged.sequences.get(&subscriber.device).copied() else {
                continue;
            };
            if changed || subscriber.sent != Some(next_sequence) {
                // A closing connection no longer needs it.
                let _ = subscriber.outbox.inventory(InventoryRevision {
                    inventory: staged.inventory.clone(),
                    next_sequence,
                });
                subscriber.sent = Some(next_sequence);
            }
        }
        self.published = Some(staged.inventory.clone());
    }
}
