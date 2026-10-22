//! Native storage for one immutable secret per direct-network pairing.

use std::sync::Arc;

use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Interaction {
    Quiet,
    Requested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CredentialError {
    InteractionRequired,
    Unavailable,
}

pub(crate) trait CredentialStore: Send + Sync {
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError>;

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError>;

    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError>;
}

pub(crate) fn native(identity: &str) -> Arc<dyn CredentialStore> {
    if !matches!(
        identity,
        "spaceterm" | "spaceterm-preflight" | "spaceterm-development"
    ) {
        return Arc::new(Unavailable);
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::Keychain::new(identity))
    }
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::SecretService::new(identity))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Arc::new(Unavailable)
}

fn account(key: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut account = String::with_capacity(64);
    for byte in key {
        account.push(char::from(HEX[usize::from(byte >> 4)]));
        account.push(char::from(HEX[usize::from(byte & 15)]));
    }
    account
}

struct Unavailable;

impl CredentialStore for Unavailable {
    fn read(
        &self,
        _: &[u8; 32],
        _: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        Err(CredentialError::Unavailable)
    }

    fn insert(&self, _: &[u8; 32], _: &[u8]) -> Result<(), CredentialError> {
        Err(CredentialError::Unavailable)
    }

    fn remove(&self, _: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
        Err(CredentialError::Unavailable)
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod native_tests {
    use super::{CredentialError, Interaction, native};

    #[test]
    #[ignore = "writes two randomly scoped Development native credential items; requires explicit authorization"]
    fn unix_native_store_refuses_replacement_and_preserves_other_pairings()
    -> Result<(), CredentialError> {
        let mut first = [0; 32];
        let mut second = [0; 32];
        getrandom::fill(&mut first).map_err(|_| CredentialError::Unavailable)?;
        getrandom::fill(&mut second).map_err(|_| CredentialError::Unavailable)?;
        let store = native("spaceterm-development");
        let exercise = || {
            store.insert(&first, b"first native verification secret")?;
            store.insert(&second, b"second native verification secret")?;
            if store.insert(&first, b"must never replace").is_ok() {
                return Err(CredentialError::Unavailable);
            }
            let value = store
                .read(&first, Interaction::Quiet)?
                .ok_or(CredentialError::Unavailable)?;
            if value.as_slice() != b"first native verification secret" {
                return Err(CredentialError::Unavailable);
            }
            store.remove(&first, Interaction::Quiet)?;
            if store.read(&first, Interaction::Quiet)?.is_some() {
                return Err(CredentialError::Unavailable);
            }
            let value = store
                .read(&second, Interaction::Quiet)?
                .ok_or(CredentialError::Unavailable)?;
            if value.as_slice() != b"second native verification secret" {
                return Err(CredentialError::Unavailable);
            }
            Ok(())
        };
        let result = exercise();
        let first_cleanup = store.remove(&first, Interaction::Quiet);
        let second_cleanup = store.remove(&second, Interaction::Quiet);
        result.and(first_cleanup).and(second_cleanup)
    }
}
