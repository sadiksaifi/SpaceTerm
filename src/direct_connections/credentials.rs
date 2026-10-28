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
        // Preflight is ad hoc signed. Its credentials must outlive each build's code identity.
        if identity == "spaceterm-preflight" {
            return Arc::new(macos::SecurityTool::new(identity));
        }
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
    let mut account = String::with_capacity(64);
    hex(key, &mut account);
    account
}

/// Appends lowercase hexadecimal digits.
fn hex(bytes: &[u8], text: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 15)]));
    }
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
    use super::{CredentialError, CredentialStore, Interaction, native};

    #[test]
    #[ignore = "writes two randomly scoped Development native credential items; requires explicit authorization"]
    fn unix_native_store_refuses_replacement_and_preserves_other_pairings()
    -> Result<(), CredentialError> {
        refuses_replacement_and_preserves_other_pairings(native("spaceterm-development").as_ref())
    }

    /// Uses a throwaway Keychain, so no prompt appears and no user Keychain changes.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_preflight_store_refuses_replacement_and_never_asks_quietly()
    -> Result<(), CredentialError> {
        use std::process::Command;

        let security = |arguments: &[&str]| {
            Command::new("/usr/bin/security")
                .args(arguments)
                .status()
                .is_ok_and(|status| status.success())
        };
        let mut scope = [0; 8];
        getrandom::fill(&mut scope).map_err(|_| CredentialError::Unavailable)?;
        let mut name = String::from("spaceterm-credentials-");
        super::hex(&scope, &mut name);
        let directory = std::env::temp_dir().join(name);
        std::fs::create_dir(&directory).map_err(|_| CredentialError::Unavailable)?;
        let keychain = directory.join("test.keychain-db");
        let path = keychain.to_str().ok_or(CredentialError::Unavailable)?;
        let mut password = String::new();
        super::hex(&scope, &mut password);
        let result = if security(&["create-keychain", "-p", &password, path]) {
            let store = super::macos::SecurityTool::at("spaceterm-preflight", keychain.clone());
            refuses_replacement_and_preserves_other_pairings(&store).and_then(|()| {
                let key = [7; 32];
                store.insert(&key, b"locked verification secret")?;
                if !security(&["lock-keychain", path]) {
                    return Err(CredentialError::Unavailable);
                }
                // A locked Keychain would make `security` ask; a quiet read refuses first.
                match store.read(&key, Interaction::Quiet) {
                    Err(CredentialError::InteractionRequired) => Ok(()),
                    _ => Err(CredentialError::Unavailable),
                }
            })
        } else {
            Err(CredentialError::Unavailable)
        };
        let deleted = security(&["delete-keychain", path]);
        let _ = std::fs::remove_dir_all(&directory);
        result.and(if deleted {
            Ok(())
        } else {
            Err(CredentialError::Unavailable)
        })
    }

    fn refuses_replacement_and_preserves_other_pairings(
        store: &dyn CredentialStore,
    ) -> Result<(), CredentialError> {
        let mut first = [0; 32];
        let mut second = [0; 32];
        getrandom::fill(&mut first).map_err(|_| CredentialError::Unavailable)?;
        getrandom::fill(&mut second).map_err(|_| CredentialError::Unavailable)?;
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
