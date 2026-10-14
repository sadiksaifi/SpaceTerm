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
    items: Mutex<Vec<(PreparedCredential, Arc<Key>)>>,
    lost: Mutex<Vec<CredentialScope>>,
}
impl MemoryCredentials {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            lost: Mutex::new(Vec::new()),
        }
    }
    pub fn created(&self) -> usize {
        self.items.lock().unwrap().len()
    }
    pub fn lose_device_key(&self) {
        let items = self.items.lock().unwrap();
        let scope = items
            .iter()
            .rev()
            .find(|(item, _)| item.scope.parts().purpose == CredentialPurpose::DeviceIdentity)
            .unwrap()
            .0
            .scope
            .clone();
        self.lost.lock().unwrap().push(scope);
    }
    pub fn lose_authentication_key(&self) {
        let items = self.items.lock().unwrap();
        let scope = items
            .iter()
            .rev()
            .find(|(item, _)| {
                matches!(
                    item.scope.parts().purpose,
                    CredentialPurpose::PairingAuthentication { .. }
                )
            })
            .unwrap()
            .0
            .scope
            .clone();
        self.lost.lock().unwrap().push(scope);
    }
}
impl NativeDeviceCredentials for MemoryCredentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let scope = reservation.scope().clone();
            let mut items = self.items.lock().unwrap();
            assert!(!items.iter().any(|(item, _)| item.scope == scope));
            let seed = u8::try_from(items.len() + 41).unwrap();
            let key = Arc::new(Key(
                Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
            ));
            let credential = PreparedCredential {
                public_key: key.public_key(),
                scope,
            };
            items.push((credential.clone(), key));
            Ok(credential)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            if self.lost.lock().unwrap().contains(scope) {
                return Ok(None);
            }
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|(item, _)| &item.scope == scope)
                .map(|(item, _)| item.clone()))
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            if self.lost.lock().unwrap().contains(&credential.scope) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            let items = self.items.lock().unwrap();
            let (_, key) = items
                .iter()
                .find(|(item, _)| item == credential)
                .ok_or(PairingFailure::CredentialUnavailable)?;
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
