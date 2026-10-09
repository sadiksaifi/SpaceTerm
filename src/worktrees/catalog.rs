//! Which directories' Worktrees SpaceTerm reads, when it reads them again, and which git
//! directories it watches. A pure state machine: the store runs the effects it returns.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::WorktreeSnapshot;
use crate::repository_status::{RepositoryReadError, WatchDirectory};

/// One sidebar row's interest in the Worktrees of the repository containing a directory.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct WorktreeInterestId(u64);

impl WorktreeInterestId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Identifies one read so a late result for an older read is ignored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadGeneration(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CatalogEffect {
    Read {
        directory: PathBuf,
        generation: ReadGeneration,
    },
    /// Replace the watch for `common` with one over `directories`.
    Watch {
        common: PathBuf,
        directories: Vec<WatchDirectory>,
    },
    Unwatch {
        common: PathBuf,
    },
}

/// What an interest knows about its directory's Worktrees.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorktreeListing {
    /// No read has finished yet.
    Pending,
    /// The directory is not in a work tree repository.
    Outside,
    Listed(Arc<WorktreeSnapshot>),
}

/// The effects to run and whether any interest's snapshot changed.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct CatalogUpdate {
    pub(crate) effects: Vec<CatalogEffect>,
    pub(crate) changed: bool,
}

#[derive(Default)]
struct Entry {
    snapshot: Option<Arc<WorktreeSnapshot>>,
    /// A read has succeeded, so a `None` snapshot means the directory is outside a repository.
    listed: bool,
    reading: Option<ReadGeneration>,
    /// A change arrived during the read in flight, so read once more when it finishes.
    stale: bool,
}

#[derive(Default)]
pub(crate) struct WorktreeCatalog {
    interests: BTreeMap<WorktreeInterestId, PathBuf>,
    /// What each moved interest presented before its move, shown until its new directory's
    /// first read finishes.
    previous: BTreeMap<WorktreeInterestId, WorktreeListing>,
    entries: BTreeMap<PathBuf, Entry>,
    /// Each watched common directory and the directories its watch covers.
    watches: BTreeMap<PathBuf, Vec<WatchDirectory>>,
    next_generation: u64,
}

impl WorktreeCatalog {
    /// The Worktrees `id` presents now.
    pub(crate) fn listing(&self, id: WorktreeInterestId) -> WorktreeListing {
        let Some(entry) = self
            .interests
            .get(&id)
            .and_then(|directory| self.entries.get(directory))
        else {
            return WorktreeListing::Pending;
        };
        match &entry.snapshot {
            Some(snapshot) => WorktreeListing::Listed(Arc::clone(snapshot)),
            None if entry.listed => WorktreeListing::Outside,
            None => WorktreeListing::Pending,
        }
    }

    /// What `id` shows: its listing, or while a moved interest's new directory is first read,
    /// what it showed before the move, so a shell changing directory doesn't empty the row.
    pub(crate) fn presented(&self, id: WorktreeInterestId) -> WorktreeListing {
        match self.listing(id) {
            WorktreeListing::Pending => self
                .previous
                .get(&id)
                .cloned()
                .unwrap_or(WorktreeListing::Pending),
            listing => listing,
        }
    }

    /// Starts or moves an interest. Reading the same directory as another interest shares its read.
    pub(crate) fn set_directory(
        &mut self,
        id: WorktreeInterestId,
        directory: PathBuf,
    ) -> CatalogUpdate {
        if self.interests.get(&id) == Some(&directory) {
            return CatalogUpdate::default();
        }
        let previous = self.presented(id);
        let mut update = self.unregister(id);
        if previous != WorktreeListing::Pending {
            self.previous.insert(id, previous);
        }
        self.interests.insert(id, directory.clone());
        if self.entries.contains_key(&directory) {
            update.changed = true;
            return update;
        }
        self.entries.insert(directory.clone(), Entry::default());
        update.effects.push(self.read(&directory));
        update.changed = true;
        update
    }

    pub(crate) fn unregister(&mut self, id: WorktreeInterestId) -> CatalogUpdate {
        self.previous.remove(&id);
        let Some(directory) = self.interests.remove(&id) else {
            return CatalogUpdate::default();
        };
        let mut update = CatalogUpdate {
            changed: true,
            ..CatalogUpdate::default()
        };
        if self.interests.values().all(|other| *other != directory) {
            self.entries.remove(&directory);
            update.effects.extend(self.reconcile_watches());
        }
        update
    }

    /// Reads every directory again, as after the window activates.
    pub(crate) fn refresh_all(&mut self) -> CatalogUpdate {
        let directories: Vec<PathBuf> = self.entries.keys().cloned().collect();
        self.refresh(directories)
    }

    /// Reads again every directory whose repository shares `common`, after its watch fired or a
    /// Worktree was created or removed there.
    pub(crate) fn repository_changed(&mut self, common: &PathBuf) -> CatalogUpdate {
        let directories = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.common_directory == *common)
            })
            .map(|(directory, _)| directory.clone())
            .collect();
        self.refresh(directories)
    }

    pub(crate) fn read_finished(
        &mut self,
        directory: &PathBuf,
        generation: ReadGeneration,
        result: Result<Option<WorktreeSnapshot>, RepositoryReadError>,
    ) -> CatalogUpdate {
        let Some(entry) = self.entries.get_mut(directory) else {
            return CatalogUpdate::default();
        };
        if entry.reading != Some(generation) {
            return CatalogUpdate::default();
        }
        entry.reading = None;
        let mut update = CatalogUpdate::default();
        // A failed read keeps the last snapshot rather than hiding open Worktrees.
        if let Ok(snapshot) = result {
            let snapshot = snapshot.map(Arc::new);
            if entry.snapshot != snapshot || !entry.listed {
                entry.snapshot = snapshot;
                entry.listed = true;
                update.changed = true;
            }
        }
        if std::mem::take(&mut entry.stale) {
            update.effects.push(self.read(directory));
        }
        update.effects.extend(self.reconcile_watches());
        update
    }

    /// Presents `snapshot` for `directory` as if a read had just listed it.
    #[cfg(test)]
    pub(crate) fn present(
        &mut self,
        directory: &std::path::Path,
        snapshot: Option<WorktreeSnapshot>,
    ) -> CatalogUpdate {
        let Some(entry) = self.entries.get_mut(directory) else {
            return CatalogUpdate::default();
        };
        entry.reading = None;
        entry.snapshot = snapshot.map(Arc::new);
        entry.listed = true;
        CatalogUpdate {
            effects: self.reconcile_watches(),
            changed: true,
        }
    }

    fn refresh(&mut self, directories: Vec<PathBuf>) -> CatalogUpdate {
        let mut update = CatalogUpdate::default();
        for directory in directories {
            let Some(entry) = self.entries.get_mut(&directory) else {
                continue;
            };
            if entry.reading.is_some() {
                entry.stale = true;
            } else {
                update.effects.push(self.read(&directory));
            }
        }
        update
    }

    fn read(&mut self, directory: &PathBuf) -> CatalogEffect {
        self.next_generation += 1;
        let generation = ReadGeneration(self.next_generation);
        if let Some(entry) = self.entries.get_mut(directory) {
            entry.reading = Some(generation);
        }
        CatalogEffect::Read {
            directory: directory.clone(),
            generation,
        }
    }

    /// Watches each read repository's common directory, and its `worktrees` directory once git
    /// has created it for a linked Worktree.
    fn reconcile_watches(&mut self) -> Vec<CatalogEffect> {
        let mut wanted: BTreeMap<PathBuf, Vec<WatchDirectory>> = BTreeMap::new();
        for snapshot in self
            .entries
            .values()
            .filter_map(|entry| entry.snapshot.as_ref())
        {
            let common = snapshot.common_directory.clone();
            let mut directories = vec![WatchDirectory {
                path: common.clone(),
                recursive: false,
            }];
            if snapshot.worktrees.len() > 1 {
                directories.push(WatchDirectory {
                    path: common.join("worktrees"),
                    recursive: true,
                });
            }
            let current = wanted.entry(common).or_default();
            if directories.len() > current.len() {
                *current = directories;
            }
        }
        let mut effects = Vec::new();
        let stale: BTreeSet<PathBuf> = self
            .watches
            .keys()
            .filter(|common| !wanted.contains_key(*common))
            .cloned()
            .collect();
        for common in stale {
            self.watches.remove(&common);
            effects.push(CatalogEffect::Unwatch { common });
        }
        for (common, directories) in wanted {
            if self.watches.get(&common) != Some(&directories) {
                self.watches.insert(common.clone(), directories.clone());
                effects.push(CatalogEffect::Watch {
                    common,
                    directories,
                });
            }
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RepositoryIdentity;
    use crate::worktrees::listing::{WorktreeHead, WorktreeRecord};

    const ROW: WorktreeInterestId = WorktreeInterestId::new(1);
    const OTHER_ROW: WorktreeInterestId = WorktreeInterestId::new(2);

    fn snapshot(roots: &[&str]) -> WorktreeSnapshot {
        WorktreeSnapshot {
            repository: RepositoryIdentity::new(roots[0].into()),
            current: Some(0),
            common_directory: format!("{}/.git", roots[0]).into(),
            worktrees: roots
                .iter()
                .map(|root| WorktreeRecord {
                    root: (*root).into(),
                    head: WorktreeHead::Branch("main".into()),
                    locked: false,
                    missing: false,
                })
                .collect(),
        }
    }

    fn read_of(update: &CatalogUpdate) -> (PathBuf, ReadGeneration) {
        update
            .effects
            .iter()
            .find_map(|effect| match effect {
                CatalogEffect::Read {
                    directory,
                    generation,
                } => Some((directory.clone(), *generation)),
                _ => None,
            })
            .expect("a read")
    }

    #[test]
    fn a_moved_interest_should_present_its_last_listing_until_the_new_directory_is_read() {
        let mut catalog = WorktreeCatalog::default();
        let (directory, generation) = read_of(&catalog.set_directory(ROW, "/src/app".into()));
        catalog.read_finished(&directory, generation, Ok(Some(snapshot(&["/src/app"]))));
        let before = catalog.presented(ROW);

        let (moved, generation) = read_of(&catalog.set_directory(ROW, "/src/app/lib".into()));
        let while_reading = (catalog.listing(ROW), catalog.presented(ROW));
        catalog.read_finished(&moved, generation, Ok(None));

        assert!(matches!(before, WorktreeListing::Listed(_)));
        assert_eq!(while_reading, (WorktreeListing::Pending, before));
        assert_eq!(catalog.presented(ROW), WorktreeListing::Outside);
        catalog.unregister(ROW);
        catalog.set_directory(ROW, "/src/other".into());
        assert_eq!(
            catalog.presented(ROW),
            WorktreeListing::Pending,
            "a new interest has nothing to show yet"
        );
    }

    #[test]
    fn rows_reading_one_directory_should_share_one_read_and_watch() {
        let mut catalog = WorktreeCatalog::default();
        let first = catalog.set_directory(ROW, "/src/app".into());
        let second = catalog.set_directory(OTHER_ROW, "/src/app".into());
        let (directory, generation) = read_of(&first);

        let finished =
            catalog.read_finished(&directory, generation, Ok(Some(snapshot(&["/src/app"]))));

        assert!(second.effects.is_empty());
        assert_eq!(
            finished,
            CatalogUpdate {
                effects: vec![CatalogEffect::Watch {
                    common: "/src/app/.git".into(),
                    directories: vec![WatchDirectory {
                        path: "/src/app/.git".into(),
                        recursive: false
                    }],
                }],
                changed: true,
            }
        );
        assert_eq!(catalog.listing(OTHER_ROW), catalog.listing(ROW));
        assert!(matches!(catalog.listing(ROW), WorktreeListing::Listed(_)));
    }

    #[test]
    fn a_change_during_a_read_should_read_once_more_and_watch_new_worktrees() {
        let mut catalog = WorktreeCatalog::default();
        let (directory, first) = read_of(&catalog.set_directory(ROW, "/src/app".into()));
        catalog.read_finished(&directory, first, Ok(Some(snapshot(&["/src/app"]))));
        let (_, second) = read_of(&catalog.repository_changed(&"/src/app/.git".into()));

        let coalesced = catalog.repository_changed(&"/src/app/.git".into());
        let finished = catalog.read_finished(
            &directory,
            second,
            Ok(Some(snapshot(&["/src/app", "/wt/feature"]))),
        );

        assert!(coalesced.effects.is_empty());
        assert!(finished.changed);
        assert!(matches!(finished.effects[0], CatalogEffect::Read { .. }));
        assert_eq!(
            finished.effects[1],
            CatalogEffect::Watch {
                common: "/src/app/.git".into(),
                directories: vec![
                    WatchDirectory {
                        path: "/src/app/.git".into(),
                        recursive: false
                    },
                    WatchDirectory {
                        path: "/src/app/.git/worktrees".into(),
                        recursive: true
                    },
                ],
            }
        );
    }

    #[test]
    fn a_late_or_failed_read_should_not_replace_the_snapshot() {
        let mut catalog = WorktreeCatalog::default();
        let (directory, first) = read_of(&catalog.set_directory(ROW, "/src/app".into()));
        catalog.read_finished(&directory, first, Ok(Some(snapshot(&["/src/app"]))));
        let (_, second) = read_of(&catalog.refresh_all());

        let late = catalog.read_finished(&directory, first, Ok(None));
        let failed =
            catalog.read_finished(&directory, second, Err(RepositoryReadError::Unavailable));

        assert_eq!((late, failed.changed), (CatalogUpdate::default(), false));
        assert!(matches!(catalog.listing(ROW), WorktreeListing::Listed(_)));
    }

    #[test]
    fn the_last_row_leaving_a_repository_should_stop_its_watch() {
        let mut catalog = WorktreeCatalog::default();
        let (directory, generation) = read_of(&catalog.set_directory(ROW, "/src/app".into()));
        catalog.read_finished(&directory, generation, Ok(Some(snapshot(&["/src/app"]))));

        let moved = catalog.set_directory(ROW, "/tmp".into());

        assert_eq!(
            moved.effects[..2],
            [
                CatalogEffect::Unwatch {
                    common: "/src/app/.git".into()
                },
                CatalogEffect::Read {
                    directory: "/tmp".into(),
                    generation: ReadGeneration(2)
                },
            ]
        );
        assert_eq!(catalog.listing(ROW), WorktreeListing::Pending);
        let (directory, generation) = read_of(&moved);
        catalog.read_finished(&directory, generation, Ok(None));
        assert_eq!(catalog.listing(ROW), WorktreeListing::Outside);
    }
}
