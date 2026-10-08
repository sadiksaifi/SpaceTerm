use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A Workspace-scoped handle for one Worktree. Handles are never reused within a Workspace.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct WorktreeId(u64);

impl WorktreeId {
    #[cfg(test)]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for WorktreeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The repository a Worktree belongs to, identified by its Main Worktree's root as git reports it.
///
/// Every Worktree of a repository reports the same Main Worktree first, so this identity is
/// stable across Worktrees without normalizing git's common directory.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RepositoryIdentity {
    main_root: PathBuf,
}

impl RepositoryIdentity {
    pub(crate) fn new(main_root: PathBuf) -> Self {
        Self { main_root }
    }

    pub(crate) fn main_root(&self) -> &Path {
        &self.main_root
    }
}

impl fmt::Debug for RepositoryIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryIdentity")
            .field("main_root", &"<redacted>")
            .finish()
    }
}

/// One Worktree of one repository: its repository and its root as git reports it.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct WorktreeKey {
    repository: RepositoryIdentity,
    root: PathBuf,
}

impl WorktreeKey {
    pub(crate) fn new(repository: RepositoryIdentity, root: PathBuf) -> Self {
        Self { repository, root }
    }

    pub(crate) const fn repository(&self) -> &RepositoryIdentity {
        &self.repository
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

impl fmt::Debug for WorktreeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorktreeKey")
            .field("repository", &self.repository)
            .field("root", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("Worktree ID space is exhausted")]
pub(crate) struct WorktreeIdSpaceExhausted;

/// Assigns one stable [`WorktreeId`] to each Worktree a Workspace refers to.
///
/// A Worktree keeps its handle while any Tab or listed row refers to it, so Tabs tagged with a
/// Worktree stay attached when the Worktree is listed again after a refresh.
pub(crate) struct WorktreeRegistry {
    ids: BTreeMap<WorktreeKey, WorktreeId>,
    keys: BTreeMap<WorktreeId, WorktreeKey>,
    next: u64,
}

impl Default for WorktreeRegistry {
    fn default() -> Self {
        Self {
            ids: BTreeMap::new(),
            keys: BTreeMap::new(),
            next: 1,
        }
    }
}

impl WorktreeRegistry {
    /// The handle for `key`, assigning a new one when the Worktree is not registered.
    pub(crate) fn id_for(
        &mut self,
        key: &WorktreeKey,
    ) -> Result<WorktreeId, WorktreeIdSpaceExhausted> {
        if let Some(&id) = self.ids.get(key) {
            return Ok(id);
        }
        let id = WorktreeId(self.next);
        self.next = self.next.checked_add(1).ok_or(WorktreeIdSpaceExhausted)?;
        self.ids.insert(key.clone(), id);
        self.keys.insert(id, key.clone());
        Ok(id)
    }

    pub(crate) fn id(&self, key: &WorktreeKey) -> Option<WorktreeId> {
        self.ids.get(key).copied()
    }

    pub(crate) fn key(&self, id: WorktreeId) -> Option<&WorktreeKey> {
        self.keys.get(&id)
    }

    /// Drops a handle that nothing refers to any more. A later [`Self::id_for`] assigns a new one.
    pub(crate) fn forget(&mut self, id: WorktreeId) {
        if let Some(key) = self.keys.remove(&id) {
            self.ids.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(main: &str, root: &str) -> WorktreeKey {
        WorktreeKey::new(RepositoryIdentity::new(main.into()), root.into())
    }

    #[test]
    fn registry_should_keep_one_handle_per_worktree_and_never_reuse_a_forgotten_one() {
        let mut registry = WorktreeRegistry::default();
        let main = registry.id_for(&key("/src/app", "/src/app")).unwrap();
        let linked = registry.id_for(&key("/src/app", "/wt/feature")).unwrap();
        let again = registry.id_for(&key("/src/app", "/wt/feature")).unwrap();

        registry.forget(linked);
        let relisted = registry.id_for(&key("/src/app", "/wt/feature")).unwrap();

        assert_eq!(again, linked);
        assert_ne!(main, linked);
        assert_ne!(relisted, linked);
        assert_eq!(registry.key(linked), None);
        assert_eq!(
            registry.key(relisted),
            Some(&key("/src/app", "/wt/feature"))
        );
    }

    #[test]
    fn worktree_values_should_redact_paths_in_debug_output() {
        let debug = format!("{:?}", key("/home/someone/app", "/home/someone/wt"));

        assert!(!debug.contains("someone"), "{debug}");
    }
}
