//! Headless native device keys. No desktop dependency or file-backed fallback.

use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{Ed25519KeyPair, KeyPair},
};
use spaceterm_client::pairing::credentials::{
    CredentialCreationReservation, CredentialDeletion, CredentialNamespace, CredentialPurpose,
    CredentialScope, CredentialScopeParts, CredentialUse, NativeCredentialAccess,
    NativeDeviceCredentials, PairingFuture, PreparedCredential,
};
use spaceterm_core::pairing::{CredentialReference, PairingFailure};
use spaceterm_protocol::{ProtocolError, authentication::SigningKey};
use std::sync::Arc;
use zeroize::Zeroizing;

/// The bytes cross only the native storage boundary, never the public key API.
pub(crate) trait NativeKeyStore: Send + Sync {
    fn read<'a>(
        &'a self,
        scope: &'a CredentialScope,
        access: NativeCredentialAccess,
    ) -> PairingFuture<'a, Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>>;
    /// Exclusive insertion. It must never replace an existing item.
    fn insert<'a>(
        &'a self,
        scope: &'a CredentialScope,
        secret: &'a [u8],
    ) -> PairingFuture<'a, Result<(), PairingFailure>>;
    /// Compare the exact item again at deletion, retaining its native identity.
    fn remove<'a>(
        &'a self,
        scope: &'a CredentialScope,
        expected: &'a [u8],
    ) -> PairingFuture<'a, Result<bool, PairingFailure>>;
}
static MUTATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
pub struct DeviceCredentials {
    store: Arc<dyn NativeKeyStore>,
}
impl DeviceCredentials {
    pub fn native() -> Self {
        #[cfg(target_os = "macos")]
        let store = Arc::new(super::macos::Keychain);
        #[cfg(target_os = "linux")]
        let store = Arc::new(super::linux::SecretService);
        Self { store }
    }
    #[cfg(test)]
    pub(crate) fn with_store(store: Arc<dyn NativeKeyStore>) -> Self {
        Self { store }
    }
    /// Allocate public reference metadata only. No seed or native effect exists
    /// until the one client journal durably reserves this scope and generation.
    pub fn new_scope(
        namespace: CredentialNamespace,
        purpose: CredentialPurpose,
    ) -> Result<CredentialScope, PairingFailure> {
        let mut slot = [0; 16];
        SystemRandom::new()
            .fill(&mut slot)
            .map_err(|_| PairingFailure::CredentialUnavailable)?;
        CredentialScope::new(CredentialScopeParts {
            namespace,
            purpose,
            reference: CredentialReference::new(slot),
        })
    }
    /// Bounded public namespace only. Routes, hostnames and private key bytes are
    /// absent. A random slot is scoped to build, purpose and original Server.
    pub fn locator(scope: &CredentialScope) -> String {
        let purpose = match scope.parts().purpose {
            CredentialPurpose::DeviceIdentity => "device".to_owned(),
            CredentialPurpose::PairingAuthentication { server } => {
                format!("pairing-{}", hex(&server.bytes()))
            }
        };
        format!(
            "spaceterm-client-v1.{}.{}.{}",
            scope.parts().namespace.as_str(),
            purpose,
            hex(&scope.reference().opaque())
        )
    }
}
fn hex(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(ALPHABET[(byte >> 4) as usize] as char);
        result.push(ALPHABET[(byte & 15) as usize] as char);
    }
    result
}
fn decode(secret: &[u8], public_key: [u8; 32]) -> Result<Zeroizing<[u8; 32]>, PairingFailure> {
    if secret.len() != 65 || secret[0] != 1 || secret[33..] != public_key {
        return Err(PairingFailure::CredentialIntegrity);
    }
    let mut seed = Zeroizing::new([0; 32]);
    seed.copy_from_slice(&secret[1..33]);
    let pair = Ed25519KeyPair::from_seed_unchecked(seed.as_slice())
        .map_err(|_| PairingFailure::CredentialIntegrity)?;
    if pair.public_key().as_ref() != public_key {
        return Err(PairingFailure::CredentialIntegrity);
    }
    Ok(seed)
}
struct LeasedSigner {
    seed: Zeroizing<[u8; 32]>,
    credential: PreparedCredential,
    lease: CredentialUse,
}
impl SigningKey for LeasedSigner {
    fn public_key(&self) -> [u8; 32] {
        self.credential.public_key
    }
    fn sign(&self, transcript: &[u8]) -> Result<[u8; 64], ProtocolError> {
        if transcript.len() > 2 * 1024 * 1024 {
            return Err(ProtocolError::LimitExceeded);
        }
        self.lease
            .validate(&self.credential.scope)
            .map_err(|_| ProtocolError::SigningUnavailable)?;
        // Ring owns temporary internal key material. Application-owned seeds
        // remain zeroizing and are released when the connection drops its lease.
        let pair = Ed25519KeyPair::from_seed_unchecked(self.seed.as_slice())
            .map_err(|_| ProtocolError::SigningUnavailable)?;
        let mut signature = Zeroizing::new([0; 64]);
        signature.copy_from_slice(pair.sign(transcript).as_ref());
        self.lease
            .validate(&self.credential.scope)
            .map_err(|_| ProtocolError::SigningUnavailable)?;
        Ok(*signature)
    }
}
impl NativeDeviceCredentials for DeviceCredentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let _mutation = MUTATION.lock().await;
            let rng = SystemRandom::new();
            let scope = reservation.scope().clone();
            let mut secret = Zeroizing::new(vec![0; 65]);
            secret[0] = 1;
            rng.fill(&mut secret[1..33])
                .map_err(|_| PairingFailure::CredentialUnavailable)?;
            let pair = Ed25519KeyPair::from_seed_unchecked(&secret[1..33])
                .map_err(|_| PairingFailure::CredentialIntegrity)?;
            let mut public_key = [0; 32];
            public_key.copy_from_slice(pair.public_key().as_ref());
            secret[33..].copy_from_slice(&public_key);
            self.store.insert(&scope, &secret).await?;
            // Even if this future is cancelled while native insertion continues,
            // the exact scope/generation/capacity remain in the one journal.
            Ok(PreparedCredential { scope, public_key })
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            let Some(secret) = self
                .store
                .read(scope, NativeCredentialAccess::Noninteractive)
                .await?
            else {
                return Ok(None);
            };
            if secret.len() != 65 {
                return Err(PairingFailure::CredentialIntegrity);
            }
            let mut public_key = [0; 32];
            public_key.copy_from_slice(&secret[33..]);
            let _seed = decode(&secret, public_key)?;
            Ok(Some(PreparedCredential {
                scope: scope.clone(),
                public_key,
            }))
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            let secret = self
                .store
                .read(&credential.scope, authorization.native_access())
                .await?
                .ok_or(PairingFailure::CredentialUnavailable)?;
            let seed = decode(&secret, credential.public_key)?;
            authorization.validate(&credential.scope)?;
            Ok(Arc::new(LeasedSigner {
                seed,
                credential: credential.clone(),
                lease: authorization.clone(),
            }) as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async move {
            if !matches!(
                credential.scope.parts().purpose,
                CredentialPurpose::PairingAuthentication { .. }
            ) {
                return Err(PairingFailure::AuthorizationDenied);
            }
            let _mutation = MUTATION.lock().await;
            let Some(secret) = self
                .store
                .read(&credential.scope, NativeCredentialAccess::Noninteractive)
                .await?
            else {
                return Ok(CredentialDeletion::AlreadyMissing);
            };
            let _seed = decode(&secret, credential.public_key)?;
            Ok(if self.store.remove(&credential.scope, &secret).await? {
                CredentialDeletion::Deleted
            } else {
                CredentialDeletion::AlreadyMissing
            })
        })
    }
}
