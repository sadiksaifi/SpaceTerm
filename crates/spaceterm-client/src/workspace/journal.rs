//! The retained Original Operation and the outcomes its client has not presented.
use crate::CoreError;
use crate::hierarchy::{Id, Mutation, RetainedOutcome};
use serde::{Deserialize, Serialize};

/// One route and Client Identity's journal bytes. The adapter holds its exclusive writer
/// lease for its entire lifetime. The replacement must be atomic and durable on success,
/// and a read after a possible publication must synchronize the bytes it reports.
pub trait JournalStore: Send {
    fn read(&self) -> Result<Option<Vec<u8>>, CoreError>;
    fn replace(&self, bytes: &[u8]) -> Result<(), CoreError>;
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ClientOperation {
    pub sequence: u64,
    pub mutation: Mutation,
}
/// How many retired outcomes the client keeps until it presents them. Beyond this, the oldest
/// is forgotten, and the client counts it so it can say that some went unseen.
pub const UNSEEN_OUTCOMES: usize = 32;
/// Whether `value` is a `operations.json` this build reads.
pub fn readable_journal(value: &serde_json::Value) -> bool {
    JournalRecord::deserialize(value).is_ok()
}
#[derive(Default, Serialize, Deserialize)]
struct JournalRecord {
    core: Id,
    client: Id,
    pending: Option<ClientOperation>,
    last: Option<RetainedOutcome>,
    /// Retired outcomes the client has not presented yet, oldest first. This is the client's
    /// own record; the server's acknowledgement retires an outcome independently.
    unseen: Vec<RetainedOutcome>,
    /// How many unseen outcomes the bound ever forgot, and how many of those the client has
    /// said went unseen. Both only grow, so recording a presentation again changes nothing.
    forgotten: u64,
    forgotten_presented: u64,
}
/// Pending shared edits live outside Settings and are fenced to their original authority.
pub struct ClientJournal {
    store: Box<dyn JournalStore>,
    record: JournalRecord,
    available: bool,
}
impl ClientJournal {
    /// Opens a held journal bound to this exact Machine and Client Identity.
    pub fn open(store: Box<dyn JournalStore>, core: Id, client: Id) -> Result<Self, CoreError> {
        let record = read(store.read()?)?.unwrap_or(JournalRecord {
            core,
            client,
            ..Default::default()
        });
        if record.core != core || record.client != client {
            return Err(CoreError::AuthenticationRefused);
        }
        Ok(Self {
            store,
            record,
            available: true,
        })
    }

    /// A failed write whose publication cannot be synchronized requires reopening the journal.
    pub fn verify_storage(&self) -> Result<(), CoreError> {
        if self.available {
            Ok(())
        } else {
            Err(CoreError::StorageUnavailable)
        }
    }

    pub fn pending(&self) -> Option<&ClientOperation> {
        self.record.pending.as_ref()
    }
    /// Drops a pending operation the client knows was never applied. Nothing dispatches it again.
    pub fn abandon(&mut self) -> Result<(), CoreError> {
        self.verify_storage()?;
        if self.record.pending.take().is_none() {
            return Ok(());
        }
        self.save()
    }
    /// Forgets the retired outcome of `sequence` once the server confirmed it forgot it too,
    /// for a client that presents each outcome itself when it arrives.
    pub fn settled(&mut self, sequence: u64) -> Result<(), CoreError> {
        self.verify_storage()?;
        if self
            .record
            .last
            .as_ref()
            .is_none_or(|last| last.sequence != sequence)
        {
            return Ok(());
        }
        self.record.last = None;
        self.record
            .unseen
            .retain(|unseen| unseen.sequence != sequence);
        self.save()
    }
    pub fn last(&self) -> Option<&RetainedOutcome> {
        self.record.last.as_ref()
    }
    /// Retired outcomes the client has not presented yet, oldest first.
    pub fn unseen(&self) -> &[RetainedOutcome] {
        &self.record.unseen
    }
    /// How many unseen outcomes the bound forgot in total, and how many of those the client has
    /// said went unseen.
    pub fn forgotten(&self) -> (u64, u64) {
        (self.record.forgotten, self.record.forgotten_presented)
    }
    /// Records that the client said the first `total` forgotten outcomes went unseen.
    pub fn forgotten_presented(&mut self, total: u64) -> Result<(), CoreError> {
        self.verify_storage()?;
        let total = total.min(self.record.forgotten);
        if total <= self.record.forgotten_presented {
            return Ok(());
        }
        self.record.forgotten_presented = total;
        self.save()
    }
    /// Records that the client presented the outcome of `sequence`, so it is never presented
    /// again. A sequence that is not unseen changes nothing.
    pub fn presented(&mut self, sequence: u64) -> Result<(), CoreError> {
        self.verify_storage()?;
        let before = self.record.unseen.len();
        self.record
            .unseen
            .retain(|unseen| unseen.sequence != sequence);
        if self.record.unseen.len() == before {
            return Ok(());
        }
        self.save()
    }
    pub fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), CoreError> {
        self.verify_storage()?;
        if self.record.pending.is_some() {
            return Err(CoreError::Unsupported);
        }
        self.record.pending = Some(ClientOperation { sequence, mutation });
        let Ok(bytes) = serde_json::to_vec(&self.record) else {
            self.record.pending = None;
            return Err(CoreError::StorageUnavailable);
        };
        self.replace(&bytes)
    }
    /// Retires the pending operation with its validated outcome in one write. Acknowledge the
    /// outcome to the server only after this succeeds; [`Self::last`] keeps it to acknowledge
    /// again after a reconnect. An outcome the client must present also stays in
    /// [`Self::unseen`] until [`Self::presented`], whatever retires after it.
    pub fn retire(&mut self, result: RetainedOutcome) -> Result<(), CoreError> {
        self.verify_storage()?;
        let pending = self.record.pending.as_ref().ok_or(CoreError::Protocol)?;
        if pending.sequence != result.sequence || pending.mutation != result.mutation {
            return Err(CoreError::Protocol);
        }
        if result.outcome.needs_presentation() {
            if self.record.unseen.len() >= UNSEEN_OUTCOMES {
                self.record.unseen.remove(0);
                self.record.forgotten = self.record.forgotten.saturating_add(1);
            }
            self.record.unseen.push(result.clone());
        }
        self.record.last = Some(result);
        self.record.pending = None;
        self.save()
    }
    fn save(&mut self) -> Result<(), CoreError> {
        let bytes = serde_json::to_vec(&self.record).map_err(|_| CoreError::StorageUnavailable)?;
        self.replace(&bytes)
    }
    fn replace(&mut self, bytes: &[u8]) -> Result<(), CoreError> {
        if let Err(error) = self.store.replace(bytes) {
            // Re-read the durable result before issuing another operation from this journal.
            self.available = false;
            let reread = self.store.read()?;
            let installed = reread.as_deref() == Some(bytes);
            let retained = read(reread)?.ok_or(CoreError::StorageUnavailable)?;
            if retained.core != self.record.core || retained.client != self.record.client {
                return Err(CoreError::AuthenticationRefused);
            }
            self.record = retained;
            self.available = true;
            if !installed {
                return Err(error);
            }
        }
        Ok(())
    }
}

/// Counts the pending operation and last retired receipt in a retained journal.
pub fn held_journal(bytes: Option<Vec<u8>>) -> Result<u32, CoreError> {
    Ok(read(bytes)?.map_or(0, |record| {
        u32::from(record.pending.is_some()) + u32::from(record.last.is_some())
    }))
}

fn read(bytes: Option<Vec<u8>>) -> Result<Option<JournalRecord>, CoreError> {
    bytes
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| CoreError::UnsafeStorage)
}
