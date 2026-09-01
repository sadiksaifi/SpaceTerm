use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use super::destination::SshHostAlias;

#[derive(Clone, Default)]
pub(crate) struct ActiveSshAliasRegistry {
    state: Arc<Mutex<ActiveSshAliasState>>,
}

#[derive(Default)]
struct ActiveSshAliasState {
    counts: BTreeMap<SshHostAlias, usize>,
    mutations: BTreeSet<SshHostAlias>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SshAliasBusy;

impl ActiveSshAliasRegistry {
    pub(crate) fn acquire(&self, alias: SshHostAlias) -> Result<ActiveSshAliasLease, SshAliasBusy> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.mutations.contains(&alias) {
            return Err(SshAliasBusy);
        }
        let count = state.counts.entry(alias.clone()).or_default();
        *count = count.saturating_add(1);
        Ok(ActiveSshAliasLease {
            registry: self.clone(),
            alias: Some(alias),
        })
    }

    pub(crate) fn is_active(&self, alias: &SshHostAlias) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counts
            .get(alias)
            .is_some_and(|count| *count != 0)
    }

    pub(crate) fn begin_mutation(
        &self,
        aliases: impl IntoIterator<Item = SshHostAlias>,
    ) -> Result<ActiveSshAliasMutation, SshAliasBusy> {
        let aliases: BTreeSet<_> = aliases.into_iter().collect();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if aliases.iter().any(|alias| {
            state.counts.get(alias).is_some_and(|count| *count != 0)
                || state.mutations.contains(alias)
        }) {
            return Err(SshAliasBusy);
        }
        state.mutations.extend(aliases.iter().cloned());
        Ok(ActiveSshAliasMutation {
            registry: self.clone(),
            aliases,
        })
    }

    fn release(&self, alias: &SshHostAlias) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = state.counts.get_mut(alias) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.counts.remove(alias);
            }
        }
    }
}

pub(crate) struct ActiveSshAliasMutation {
    registry: ActiveSshAliasRegistry,
    aliases: BTreeSet<SshHostAlias>,
}

impl Drop for ActiveSshAliasMutation {
    fn drop(&mut self) {
        let mut state = self
            .registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for alias in &self.aliases {
            state.mutations.remove(alias);
        }
    }
}

pub(crate) struct ActiveSshAliasLease {
    registry: ActiveSshAliasRegistry,
    alias: Option<SshHostAlias>,
}

impl ActiveSshAliasLease {
    pub(crate) fn try_duplicate(&self) -> Result<Self, SshAliasBusy> {
        let alias = self.alias.as_ref().ok_or(SshAliasBusy)?;
        self.registry.acquire(alias.clone())
    }
}

impl Drop for ActiveSshAliasLease {
    fn drop(&mut self) {
        if let Some(alias) = self.alias.take() {
            self.registry.release(&alias);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias() -> SshHostAlias {
        SshHostAlias::new("work".to_owned()).unwrap()
    }

    #[test]
    fn duplicate_alias_leases_should_remain_active_until_every_owner_releases() {
        let registry = ActiveSshAliasRegistry::default();
        let alias = alias();
        let first = registry.acquire(alias.clone()).unwrap();
        let second = registry.acquire(alias.clone()).unwrap();

        drop(first);
        assert!(registry.is_active(&alias));
        drop(second);
        assert!(!registry.is_active(&alias));
    }

    #[test]
    fn mutation_and_connection_leases_should_exclude_each_other_atomically() {
        let registry = ActiveSshAliasRegistry::default();
        let alias = alias();
        let mutation = registry.begin_mutation([alias.clone()]).unwrap();
        assert!(registry.acquire(alias.clone()).is_err());
        drop(mutation);

        let connection = registry.acquire(alias.clone()).unwrap();
        assert!(registry.begin_mutation([alias.clone()]).is_err());
        drop(connection);
        assert!(registry.begin_mutation([alias]).is_ok());
    }

    #[test]
    fn duplicated_lease_should_keep_the_alias_active_after_the_connection_releases() {
        let registry = ActiveSshAliasRegistry::default();
        let alias = alias();
        let connection = registry.acquire(alias.clone()).unwrap();
        let workspace = connection.try_duplicate().unwrap();

        drop(connection);
        assert!(registry.is_active(&alias));
        drop(workspace);
        assert!(!registry.is_active(&alias));
    }

    #[test]
    fn duplicate_workspace_pins_should_release_only_after_the_final_owner() {
        let registry = ActiveSshAliasRegistry::default();
        let alias = alias();
        let connection = registry.acquire(alias.clone()).unwrap();
        let first = connection.try_duplicate().unwrap();
        let second = connection.try_duplicate().unwrap();

        drop(connection);
        drop(first);
        assert!(registry.is_active(&alias));
        drop(second);
        assert!(!registry.is_active(&alias));
    }
}
