//! Bounded immutable browsing observations and provider-owned cursor authority.
use super::native::{NativeDirectoryProvider, classify, io_failure};
use crate::git::filesystem::RetainedDirectory;
use spaceterm_core::organization::ServerDirectory;
use spaceterm_core::server_directories::*;
use std::num::NonZeroU64;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
const MAX_ENTRIES: usize = 16_384;
const MAX_OBSERVATION_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(120);
pub(crate) struct BrowseObservation {
    context: DirectoryContext,
    directory: ServerDirectory,
    observation: NonZeroU64,
    lease: Arc<dyn RetainedDirectory>,
    stamp: (i64, i64, i64, i64),
    entries: Vec<DirectoryEntry>,
    position: usize,
    bytes: usize,
    created: Instant,
}
impl NativeDirectoryProvider {
    /// Read one page from a principal-bound immutable directory observation.
    pub fn browse(
        &self,
        context: DirectoryContext,
        directory: &ServerDirectory,
        cursor: Option<&DirectoryCursor>,
    ) -> Result<DirectoryPage, DirectoryFailure> {
        if context.server() != self.server {
            return Err(DirectoryFailure::WrongServer);
        }
        let path = self.path(directory)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| DirectoryFailure::Unavailable)?;
        state
            .observations
            .retain(|_, observation| observation.created.elapsed() < LIFETIME);
        let mut observation = if let Some(cursor) = cursor {
            if !cursor.matches(context, directory, None) {
                return Err(DirectoryFailure::StaleCursor);
            }
            // Compare before consuming: a forged principal must not evict the original observation.
            let existing = state
                .observations
                .get(&cursor.parts().random)
                .ok_or(DirectoryFailure::StaleCursor)?;
            if existing.context != context
                || existing.directory != *directory
                || existing.observation != cursor.parts().observation
            {
                return Err(DirectoryFailure::StaleCursor);
            }
            state
                .observations
                .remove(&cursor.parts().random)
                .ok_or(DirectoryFailure::StaleCursor)?
        } else {
            if state.observations.len() >= 24
                || state
                    .observations
                    .values()
                    .filter(|o| o.context == context)
                    .count()
                    >= 8
            {
                return Err(DirectoryFailure::Capacity);
            }
            let lease = self.retain(directory)?;
            let stamp = stamp(&path)?;
            let mut entries = Vec::new();
            let mut bytes = 256 + directory.bytes().len();
            for (count, entry) in std::fs::read_dir(&path).map_err(io_failure)?.enumerate() {
                if count == MAX_ENTRIES {
                    return Err(DirectoryFailure::Capacity);
                }
                let entry = entry.map_err(io_failure)?;
                let metadata = entry.metadata().map_err(io_failure)?;
                if !metadata.is_dir() {
                    continue;
                }
                let name = DirectoryEntryName::new(entry.file_name().as_encoded_bytes().to_vec())?;
                let child = self.value(&path.join(entry.file_name()))?;
                bytes = bytes
                    .checked_add(128 + name.bytes().len() + child.bytes().len())
                    .ok_or(DirectoryFailure::Capacity)?;
                if bytes > MAX_OBSERVATION_BYTES {
                    return Err(DirectoryFailure::Capacity);
                }
                entries.push(DirectoryEntry {
                    name,
                    directory: child,
                    selectable: true,
                });
            }
            entries.sort_by(|a, b| a.name.bytes().cmp(b.name.bytes()));
            lease.revalidate().map_err(classify)?;
            if stamp != self::stamp(&path)? {
                return Err(DirectoryFailure::StaleCursor);
            }
            if state.observations.values().map(|o| o.bytes).sum::<usize>() + bytes > MAX_CACHE_BYTES
            {
                return Err(DirectoryFailure::Capacity);
            }
            state.next_observation = state
                .next_observation
                .checked_add(1)
                .ok_or(DirectoryFailure::Capacity)?;
            let observation =
                NonZeroU64::new(state.next_observation).ok_or(DirectoryFailure::Capacity)?;
            BrowseObservation {
                context,
                directory: directory.clone(),
                observation,
                lease,
                stamp,
                entries,
                position: 0,
                bytes,
                created: Instant::now(),
            }
        };
        observation
            .lease
            .revalidate()
            .map_err(|_| DirectoryFailure::StaleCursor)?;
        if observation.stamp != stamp(&path).map_err(|_| DirectoryFailure::StaleCursor)? {
            return Err(DirectoryFailure::StaleCursor);
        }
        // Account for a continuation even when this turns out to be the final page.
        let mut bytes = 512 + 2 * directory.bytes().len();
        let mut entries = Vec::new();
        while let Some(entry) = observation.entries.get(observation.position) {
            let size = 128 + entry.name.bytes().len() + entry.directory.bytes().len();
            if entries.len() == MAX_DIRECTORY_PAGE_ENTRIES
                || bytes + size > MAX_DIRECTORY_PAGE_BYTES
            {
                break;
            }
            bytes += size;
            entries.push(entry.clone());
            observation.position += 1;
        }
        let next = if observation.position < observation.entries.len() {
            let mut random = [0; 32];
            getrandom::fill(&mut random).map_err(|_| DirectoryFailure::Unavailable)?;
            if random == [0; 32] || state.observations.contains_key(&random) {
                return Err(DirectoryFailure::Unavailable);
            }
            Some(DirectoryCursor::new(DirectoryCursorParts {
                context,
                scope: directory.clone(),
                observation: observation.observation,
                random,
            })?)
        } else {
            None
        };
        let page = DirectoryPage::new(DirectoryPageParts {
            context,
            directory: directory.clone(),
            observation: observation.observation,
            entries,
            next: next.clone(),
        })?;
        if let Some(next) = next {
            state.observations.insert(next.parts().random, observation);
        }
        Ok(page)
    }
}
fn stamp(path: &std::path::Path) -> Result<(i64, i64, i64, i64), DirectoryFailure> {
    let metadata = std::fs::metadata(path).map_err(io_failure)?;
    Ok((
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ))
}
