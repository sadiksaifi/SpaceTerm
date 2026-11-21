//! Injected storage for one immutable secret per direct-network pairing.
use zeroize::Zeroizing;

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

pub trait CredentialStore: Send + Sync {
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError>;

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError>;

    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError>;
}

/// Removes `key`'s item for a removal the person asked for. It tries quietly first, so the
/// system asks the person only when it refuses to remove the item without them, as it does for
/// an item another SpaceTerm program saved.
pub fn remove_asked(store: &dyn CredentialStore, key: &[u8; 32]) -> Result<(), CredentialError> {
    match store.remove(key, Interaction::Quiet) {
        Err(CredentialError::InteractionRequired) => store.remove(key, Interaction::Requested),
        quiet => quiet,
    }
}
