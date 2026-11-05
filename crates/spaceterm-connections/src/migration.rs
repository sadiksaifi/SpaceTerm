//! Credential replacements for Data Generation steps, in the stores that keep Saved
//! Connections' credentials.
use std::sync::Arc;

use spaceterm_core::migration::{CredentialFailure, CredentialItem, Credentials};

use crate::credentials::{CredentialError, CredentialStore, Interaction, StoreKind};

/// Opens the store a credential reference names.
pub type OpenStore = dyn Fn(StoreKind) -> Result<Arc<dyn CredentialStore>, CredentialError>;

/// Copies and removes items in the stores `open` returns. Each operation is quiet, so a store
/// that would ask the person refuses instead.
pub struct StoreCredentials {
    open: Box<OpenStore>,
}

impl StoreCredentials {
    pub fn new(
        open: impl Fn(StoreKind) -> Result<Arc<dyn CredentialStore>, CredentialError> + 'static,
    ) -> Self {
        Self {
            open: Box::new(open),
        }
    }

    fn store(&self, item: &CredentialItem) -> Result<Arc<dyn CredentialStore>, CredentialFailure> {
        let kind: StoreKind = serde_json::from_value(serde_json::Value::String(item.store.clone()))
            .map_err(|_| CredentialFailure)?;
        (self.open)(kind).map_err(|_| CredentialFailure)
    }
}

impl Credentials for StoreCredentials {
    fn copy(
        &mut self,
        from: &CredentialItem,
        to: &CredentialItem,
    ) -> Result<(), CredentialFailure> {
        let secret = self
            .store(from)?
            .read(&from.key, Interaction::Quiet)
            .map_err(|_| CredentialFailure)?
            .ok_or(CredentialFailure)?;
        let target = self.store(to)?;
        // A migration that stopped before its marker made this copy already.
        match target.read(&to.key, Interaction::Quiet) {
            Ok(Some(existing)) if *existing == *secret => return Ok(()),
            Ok(Some(_)) => return Err(CredentialFailure),
            Ok(None) | Err(_) => {}
        }
        target
            .insert(&to.key, &secret)
            .map_err(|_| CredentialFailure)
    }

    fn remove(&mut self, item: &CredentialItem) -> Result<(), CredentialFailure> {
        self.store(item)?
            .remove(&item.key, Interaction::Quiet)
            .map_err(|_| CredentialFailure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use zeroize::Zeroizing;

    #[derive(Default)]
    struct Memory(Mutex<BTreeMap<[u8; 32], Vec<u8>>>);

    impl CredentialStore for Memory {
        fn kind(&self) -> StoreKind {
            StoreKind::Keychain
        }
        fn read(
            &self,
            key: &[u8; 32],
            _: Interaction,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
            Ok(self.0.lock().unwrap().get(key).cloned().map(Zeroizing::new))
        }
        fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
            let mut items = self.0.lock().unwrap();
            if items.contains_key(key) {
                return Err(CredentialError::Unavailable);
            }
            items.insert(*key, secret.to_vec());
            Ok(())
        }
        fn remove(&self, key: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn item(key: u8) -> CredentialItem {
        CredentialItem {
            store: "keychain".into(),
            key: [key; 32],
        }
    }

    #[test]
    fn a_copy_keeps_the_original_until_it_is_removed_and_repeats_safely() {
        let memory = Arc::new(Memory::default());
        memory.insert(&[1; 32], b"secret").unwrap();
        let open = Arc::clone(&memory);
        let mut credentials =
            StoreCredentials::new(move |_| Ok(Arc::clone(&open) as Arc<dyn CredentialStore>));
        credentials.copy(&item(1), &item(2)).unwrap();
        credentials.copy(&item(1), &item(2)).unwrap();
        assert_eq!(memory.0.lock().unwrap().len(), 2);
        credentials.remove(&item(1)).unwrap();
        assert_eq!(
            memory.0.lock().unwrap().get(&[2; 32]).map(Vec::as_slice),
            Some(b"secret".as_slice())
        );
        assert!(!memory.0.lock().unwrap().contains_key(&[1; 32]));
        memory.insert(&[3; 32], b"other").unwrap();
        assert_eq!(
            credentials.copy(&item(1), &item(3)),
            Err(CredentialFailure),
            "a missing source or a different item under the new key refuses"
        );
    }
}
