//! Storage for one immutable secret per direct-network pairing.

use std::sync::Arc;

use zeroize::Zeroizing;

#[cfg(any(target_os = "linux", test))]
mod file;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interaction {
    Quiet,
    Requested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialError {
    InteractionRequired,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    Keychain,
    SecretService,
    PrivateFile,
}

pub trait CredentialStore: Send + Sync {
    fn kind(&self) -> StoreKind;
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError>;

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError>;

    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError>;
}

/// Enrollment selects once; later access opens only the backend retained by its record.
pub trait CredentialProvider {
    fn select(&self) -> Result<Arc<dyn CredentialStore>, CredentialError>;
    fn open(&self, kind: StoreKind) -> Result<Arc<dyn CredentialStore>, CredentialError>;
}

pub fn native(identity: &str) -> Result<Arc<dyn CredentialStore>, CredentialError> {
    if !matches!(
        identity,
        "spaceterm" | "spaceterm-preflight" | "spaceterm-development"
    ) {
        return Err(CredentialError::Unavailable);
    }
    #[cfg(target_os = "macos")]
    {
        // Preflight is ad hoc signed. Its credentials must outlive each build's code identity.
        if identity == "spaceterm-preflight" {
            return Ok(Arc::new(macos::SecurityTool::new(identity)));
        }
        Ok(Arc::new(macos::Keychain::new(identity)))
    }
    #[cfg(target_os = "linux")]
    {
        {
            linux::available()?;
            Ok(Arc::new(linux::SecretService::new(identity)))
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(CredentialError::Unavailable)
}

/// CLI policy only. Desktop callers continue to use `native` directly.
pub fn for_cli_enrollment(identity: &str) -> Result<Arc<dyn CredentialStore>, CredentialError> {
    let native = native(identity);
    #[cfg(target_os = "linux")]
    {
        cli_store(native, || private_files(identity))
    }
    #[cfg(not(target_os = "linux"))]
    native
}

/// Opens exactly the recorded backend. Native failures never grant file access.
pub fn for_cli_record(
    identity: &str,
    kind: StoreKind,
) -> Result<Arc<dyn CredentialStore>, CredentialError> {
    match kind {
        StoreKind::PrivateFile => {
            #[cfg(target_os = "linux")]
            {
                private_files(identity)
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(CredentialError::Unavailable)
            }
        }
        StoreKind::Keychain | StoreKind::SecretService => {
            let store = native(identity)?;
            if store.kind() != kind {
                return Err(CredentialError::Unavailable);
            }
            Ok(store)
        }
    }
}

#[cfg(target_os = "linux")]
fn private_files(identity: &str) -> Result<Arc<dyn CredentialStore>, CredentialError> {
    if !matches!(
        identity,
        "spaceterm" | "spaceterm-preflight" | "spaceterm-development"
    ) {
        return Err(CredentialError::Unavailable);
    }
    let directories = spaceterm_directories::AppDirectories::resolve(identity)
        .map_err(|_| CredentialError::Unavailable)?;
    Ok(Arc::new(file::PrivateFiles(
        directories.data.join("cli-credentials"),
    )))
}

#[cfg(any(target_os = "linux", test))]
fn cli_store(
    native: Result<Arc<dyn CredentialStore>, CredentialError>,
    fallback: impl FnOnce() -> Result<Arc<dyn CredentialStore>, CredentialError>,
) -> Result<Arc<dyn CredentialStore>, CredentialError> {
    match native {
        Err(CredentialError::Unavailable) => fallback(),
        other => other,
    }
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

pub struct Unavailable;

impl CredentialStore for Unavailable {
    fn kind(&self) -> StoreKind {
        #[cfg(target_os = "macos")]
        {
            StoreKind::Keychain
        }
        #[cfg(not(target_os = "macos"))]
        {
            StoreKind::SecretService
        }
    }
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
        refuses_replacement_and_preserves_other_pairings(native("spaceterm-development")?.as_ref())
    }

    pub(super) fn refuses_replacement_and_preserves_other_pairings(
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
