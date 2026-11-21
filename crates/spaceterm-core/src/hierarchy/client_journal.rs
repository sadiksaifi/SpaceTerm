//! Native ownership of the portable journal's private file and exclusive writer lease.
use super::Id;
use crate::{CoreError, Directory, Lock};
use spaceterm_client::workspace::JournalStore;

use spaceterm_client::workspace::journal::held_journal;
pub use spaceterm_client::workspace::journal::{
    ClientJournal, ClientOperation, UNSEEN_OUTCOMES, readable_journal,
};

struct NativeJournal {
    directory: Directory,
    _lock: Lock,
}

impl JournalStore for NativeJournal {
    fn read(&self) -> Result<Option<Vec<u8>>, CoreError> {
        self.directory.read_bytes("operations.json")
    }

    fn replace(&self, bytes: &[u8]) -> Result<(), CoreError> {
        self.directory.write_bytes(c"operations.json", bytes)
    }
}

pub(crate) fn open_client_journal(
    directory: Directory,
    core: Id,
    client: Id,
) -> Result<ClientJournal, CoreError> {
    ClientJournal::open(journal_writer(directory)?, core, client)
}

pub(crate) fn journal_writer(directory: Directory) -> Result<Box<dyn JournalStore>, CoreError> {
    let lock = directory.lock("operations.lock", true)?;
    Ok(Box::new(NativeJournal {
        directory,
        _lock: lock,
    }))
}

pub(crate) fn open_locked_client_journal(
    directory: Directory,
    lock: Lock,
    core: Id,
    client: Id,
) -> Result<ClientJournal, CoreError> {
    ClientJournal::open(
        Box::new(NativeJournal {
            directory,
            _lock: lock,
        }),
        core,
        client,
    )
}

/// Counts held Original Operations and unacknowledged receipts without claiming the writer.
pub fn held_client_journal(directory: &Directory) -> Result<u32, CoreError> {
    held_journal(directory.read_bytes("operations.json")?)
}

/// Opens a fixture journal under the same writer lease a live edit loop takes.
#[cfg(feature = "test-support")]
pub fn open_client_journal_in(
    path: &std::path::Path,
    core: Id,
    client: Id,
) -> Result<ClientJournal, CoreError> {
    open_client_journal(Directory::open(path, true)?, core, client)
}
