use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::destination::SshHostAlias;

#[derive(Clone, Default)]
pub(crate) struct ActiveSshAliasRegistry {
    counts: Arc<Mutex<BTreeMap<SshHostAlias, usize>>>,
}

impl ActiveSshAliasRegistry {
    pub(crate) fn acquire(&self, alias: SshHostAlias) -> ActiveSshAliasLease {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = counts.entry(alias.clone()).or_default();
        *count = count.saturating_add(1);
        ActiveSshAliasLease {
            registry: self.clone(),
            alias: Some(alias),
        }
    }

    pub(crate) fn is_active(&self, alias: &SshHostAlias) -> bool {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(alias)
            .is_some_and(|count| *count != 0)
    }

    fn release(&self, alias: &SshHostAlias) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = counts.get_mut(alias) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(alias);
            }
        }
    }
}

pub(crate) struct ActiveSshAliasLease {
    registry: ActiveSshAliasRegistry,
    alias: Option<SshHostAlias>,
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
        let first = registry.acquire(alias.clone());
        let second = registry.acquire(alias.clone());

        drop(first);
        assert!(registry.is_active(&alias));
        drop(second);
        assert!(!registry.is_active(&alias));
    }
}
