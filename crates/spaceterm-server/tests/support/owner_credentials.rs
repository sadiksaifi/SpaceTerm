use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::pairing::credentials::*;
use spaceterm_core::pairing::PairingFailure;
use spaceterm_protocol::{ProtocolError, authentication::SigningKey};
use std::sync::{Arc, Mutex};

struct Key(Ed25519KeyPair);
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}

/// Only the uncontrolled native credential service is replaced. The production
/// credential reservation, authorization and journal transitions remain real.
pub struct MemoryCredentials {
    device: Arc<Key>,
    authentication: Arc<Key>,
    items: Mutex<Vec<PreparedCredential>>,
}
impl MemoryCredentials {
    pub fn new() -> Self {
        Self {
            device: Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[41; 32]).unwrap())),
            authentication: Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap())),
            items: Mutex::new(Vec::new()),
        }
    }
    pub fn created(&self) -> usize {
        self.items.lock().unwrap().len()
    }
    fn key(&self, scope: &CredentialScope) -> &Arc<Key> {
        if scope.parts().purpose == CredentialPurpose::DeviceIdentity {
            &self.device
        } else {
            &self.authentication
        }
    }
}
impl NativeDeviceCredentials for MemoryCredentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let scope = reservation.scope().clone();
            let credential = PreparedCredential {
                public_key: self.key(&scope).public_key(),
                scope,
            };
            let mut items = self.items.lock().unwrap();
            assert!(!items.iter().any(|item| item.scope == credential.scope));
            items.push(credential.clone());
            Ok(credential)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|item| &item.scope == scope)
                .cloned())
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            assert!(self.items.lock().unwrap().contains(credential));
            let key = self.key(&credential.scope);
            assert_eq!(key.public_key(), credential.public_key);
            Ok(key.clone() as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("owner enrollment must not delete native credentials") })
    }
}
