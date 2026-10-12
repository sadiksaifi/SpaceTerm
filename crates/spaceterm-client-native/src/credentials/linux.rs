//! Explicit encrypted org.freedesktop.Secrets access. No portal/file backend,
//! plain-session fallback, unlock prompt, collection creation or desktop linkage.

use super::adapter::{DeviceCredentials, NativeKeyStore};
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use hkdf::Hkdf;
use num_bigint_dig::BigUint;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::Sha256;
use spaceterm_client::pairing::credentials::{
    CredentialScope, NativeCredentialAccess, PairingFuture,
};
use spaceterm_core::pairing::PairingFailure;
use std::collections::HashMap;
use zbus::{
    Connection, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
use zeroize::Zeroizing;

pub(super) struct SecretService;
const DESTINATION: &str = "org.freedesktop.secrets";
const ROOT: &str = "/org/freedesktop/secrets";
const ALGORITHM: &str = "dh-ietf1024-sha256-aes128-cbc-pkcs7";
const PRIME: [u8; 128] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xc9, 0x0f, 0xda, 0xa2, 0x21, 0x68, 0xc2, 0x34,
    0xc4, 0xc6, 0x62, 0x8b, 0x80, 0xdc, 0x1c, 0xd1, 0x29, 0x02, 0x4e, 0x08, 0x8a, 0x67, 0xcc, 0x74,
    0x02, 0x0b, 0xbe, 0xa6, 0x3b, 0x13, 0x9b, 0x22, 0x51, 0x4a, 0x08, 0x79, 0x8e, 0x34, 0x04, 0xdd,
    0xef, 0x95, 0x19, 0xb3, 0xcd, 0x3a, 0x43, 0x1b, 0x30, 0x2b, 0x0a, 0x6d, 0xf2, 0x5f, 0x14, 0x37,
    0x4f, 0xe1, 0x35, 0x6d, 0x6d, 0x51, 0xc2, 0x45, 0xe4, 0x85, 0xb5, 0x76, 0x62, 0x5e, 0x7e, 0xc6,
    0xf4, 0x4c, 0x42, 0xe9, 0xa6, 0x37, 0xed, 0x6b, 0x0b, 0xff, 0x5c, 0xb6, 0xf4, 0x06, 0xb7, 0xed,
    0xee, 0x38, 0x6b, 0xfb, 0x5a, 0x89, 0x9f, 0xa5, 0xae, 0x9f, 0x24, 0x11, 0x7c, 0x4b, 0x1f, 0xe6,
    0x49, 0x28, 0x66, 0x51, 0xec, 0xe6, 0x53, 0x81, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];
fn unavailable<T>(_: T) -> PairingFailure {
    PairingFailure::CredentialUnavailable
}
fn integrity<T>(_: T) -> PairingFailure {
    PairingFailure::CredentialIntegrity
}

fn derive(private: &BigUint, peer: &[u8]) -> Result<Zeroizing<[u8; 16]>, PairingFailure> {
    if peer.is_empty() || peer.len() > 128 {
        return Err(PairingFailure::CredentialIntegrity);
    }
    let prime = BigUint::from_bytes_be(&PRIME);
    let public = BigUint::from_bytes_be(peer);
    if public <= BigUint::from(1u8) || public >= &prime - BigUint::from(1u8) {
        return Err(PairingFailure::CredentialIntegrity);
    }
    let shared = Zeroizing::new(public.modpow(private, &prime));
    let shared_bytes = Zeroizing::new(shared.to_bytes_be());
    let mut padded = Zeroizing::new([0; 128]);
    padded[128 - shared_bytes.len()..].copy_from_slice(&shared_bytes);
    let mut key = Zeroizing::new([0; 16]);
    Hkdf::<Sha256>::new(None, padded.as_slice())
        .expand(&[], key.as_mut_slice())
        .map_err(integrity)?;
    Ok(key)
}
fn encrypt(key: &[u8; 16], iv: &[u8; 16], secret: &[u8]) -> Result<Vec<u8>, PairingFailure> {
    if secret.len() != 65 {
        return Err(PairingFailure::CredentialIntegrity);
    }
    let mut buffer = Zeroizing::new(vec![0; 80]);
    let len = cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into())
        .encrypt_padded_b2b_mut::<Pkcs7>(secret, &mut buffer)
        .map_err(integrity)?
        .len();
    Ok(buffer[..len].to_vec())
}
fn decrypt(
    key: &[u8; 16],
    iv: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, PairingFailure> {
    if iv.len() != 16 || ciphertext.len() != 80 {
        return Err(PairingFailure::CredentialIntegrity);
    }
    let mut buffer = Zeroizing::new(ciphertext.to_vec());
    let len = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
        .map_err(integrity)?
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(integrity)?
        .len();
    if len != 65 {
        return Err(PairingFailure::CredentialIntegrity);
    }
    buffer.truncate(len);
    Ok(buffer)
}
struct Session {
    connection: Connection,
    path: OwnedObjectPath,
    key: Zeroizing<[u8; 16]>,
}
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);
impl Session {
    async fn open() -> Result<Self, PairingFailure> {
        let connection = zbus::connection::Builder::session()
            .map_err(unavailable)?
            .method_timeout(std::time::Duration::from_secs(10))
            .build()
            .await
            .map_err(unavailable)?;
        let private = {
            let mut bytes = Zeroizing::new([0; 32]);
            SystemRandom::new()
                .fill(bytes.as_mut_slice())
                .map_err(unavailable)?;
            Zeroizing::new(BigUint::from_bytes_be(bytes.as_slice()))
        };
        let public = BigUint::from(2u8)
            .modpow(&private, &BigUint::from_bytes_be(&PRIME))
            .to_bytes_be();
        let service = Proxy::new(
            &connection,
            DESTINATION,
            ROOT,
            "org.freedesktop.Secret.Service",
        )
        .await
        .map_err(unavailable)?;
        let (peer, path): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &(ALGORITHM, Value::from(public)))
            .await
            .map_err(unavailable)?;
        let peer: Vec<u8> = peer.try_into().map_err(integrity)?;
        if path.as_str() == "/" {
            return Err(PairingFailure::CredentialIntegrity);
        }
        let key = derive(&private, &peer)?;
        drop(service);
        Ok(Self {
            connection,
            path,
            key,
        })
    }
    async fn proxy<'a>(
        &'a self,
        path: &'a str,
        interface: &'static str,
    ) -> Result<Proxy<'a>, PairingFailure> {
        Proxy::new(&self.connection, DESTINATION, path, interface)
            .await
            .map_err(unavailable)
    }
    async fn collection(&self) -> Result<OwnedObjectPath, PairingFailure> {
        let service = self.proxy(ROOT, "org.freedesktop.Secret.Service").await?;
        let collection: OwnedObjectPath = service
            .call("ReadAlias", &("default",))
            .await
            .map_err(unavailable)?;
        if collection.as_str() == "/" {
            return Err(PairingFailure::CredentialUnavailable);
        }
        let proxy = self
            .proxy(collection.as_str(), "org.freedesktop.Secret.Collection")
            .await?;
        if proxy
            .get_property::<bool>("Locked")
            .await
            .map_err(unavailable)?
        {
            return Err(PairingFailure::CredentialUnavailable);
        }
        Ok(collection)
    }
    async fn item(
        &self,
        scope: &CredentialScope,
    ) -> Result<Option<OwnedObjectPath>, PairingFailure> {
        // The default alias is only a creation destination. Search the service
        // inventory for a retained slot, including locked collections, so alias
        // changes cannot turn unavailable keys into AlreadyMissing cleanup.
        let service = self.proxy(ROOT, "org.freedesktop.Secret.Service").await?;
        let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
            .call("SearchItems", &(attributes(scope),))
            .await
            .map_err(unavailable)?;
        if unlocked.len().saturating_add(locked.len()) > 1
            || unlocked
                .iter()
                .chain(locked.iter())
                .any(|item| item.as_str() == "/")
        {
            return Err(PairingFailure::CredentialIntegrity);
        }
        if !locked.is_empty() {
            return Err(PairingFailure::CredentialUnavailable);
        }
        Ok(unlocked.pop())
    }
    async fn read_item(
        &self,
        item: &OwnedObjectPath,
    ) -> Result<Zeroizing<Vec<u8>>, PairingFailure> {
        let proxy = self
            .proxy(item.as_str(), "org.freedesktop.Secret.Item")
            .await?;
        if proxy
            .get_property::<bool>("Locked")
            .await
            .map_err(unavailable)?
        {
            return Err(PairingFailure::CredentialUnavailable);
        }
        let (session, iv, ciphertext, content_type): Secret = proxy
            .call("GetSecret", &(&self.path,))
            .await
            .map_err(unavailable)?;
        if session != self.path || content_type != "application/octet-stream" {
            return Err(PairingFailure::CredentialIntegrity);
        }
        decrypt(&self.key, &iv, &ciphertext)
    }
    async fn remove(
        &self,
        scope: &CredentialScope,
        expected: &[u8],
    ) -> Result<bool, PairingFailure> {
        let Some(item) = self.item(scope).await? else {
            return Ok(false);
        };
        if self.read_item(&item).await?.as_slice() != expected {
            return Err(PairingFailure::CredentialIntegrity);
        }
        let proxy = self
            .proxy(item.as_str(), "org.freedesktop.Secret.Item")
            .await?;
        let prompt: OwnedObjectPath = proxy.call("Delete", &()).await.map_err(unavailable)?;
        if prompt.as_str() != "/" {
            return Err(PairingFailure::CredentialUnavailable);
        }
        Ok(true)
    }
}
fn attributes(scope: &CredentialScope) -> HashMap<String, String> {
    HashMap::from([
        ("application".into(), "spaceterm-client-v1".into()),
        ("scope".into(), DeviceCredentials::locator(scope)),
    ])
}
impl NativeKeyStore for SecretService {
    fn read<'a>(
        &'a self,
        scope: &'a CredentialScope,
        _access: NativeCredentialAccess,
    ) -> PairingFuture<'a, Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>> {
        Box::pin(async move {
            let session = Session::open().await?;
            match session.item(scope).await? {
                Some(item) => session.read_item(&item).await.map(Some),
                None => Ok(None),
            }
        })
    }
    fn insert<'a>(
        &'a self,
        scope: &'a CredentialScope,
        secret: &'a [u8],
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            let session = Session::open().await?;
            if session.item(scope).await?.is_some() {
                return Err(PairingFailure::CredentialIntegrity);
            }
            let collection = session.collection().await?;
            let proxy = session
                .proxy(collection.as_str(), "org.freedesktop.Secret.Collection")
                .await?;
            let mut iv = [0; 16];
            SystemRandom::new().fill(&mut iv).map_err(unavailable)?;
            let secret = (
                session.path.clone(),
                iv.to_vec(),
                encrypt(&session.key, &iv, secret)?,
                "application/octet-stream".to_string(),
            );
            let properties = HashMap::from([
                (
                    "org.freedesktop.Secret.Item.Label",
                    Value::from("SpaceTerm client credential"),
                ),
                (
                    "org.freedesktop.Secret.Item.Attributes",
                    Value::from(attributes(scope)),
                ),
            ]);
            let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = proxy
                .call("CreateItem", &(properties, secret, false))
                .await
                .map_err(unavailable)?;
            if item.as_str() == "/" || prompt.as_str() != "/" {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(())
        })
    }
    fn remove<'a>(
        &'a self,
        scope: &'a CredentialScope,
        expected: &'a [u8],
    ) -> PairingFuture<'a, Result<bool, PairingFailure>> {
        Box::pin(async move {
            let session = Session::open().await?;
            session.remove(scope, expected).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encrypted_secret_service_agreement_and_bounded_secret_round_trip() {
        let a = BigUint::from(112233u32);
        let b = BigUint::from(223344u32);
        let prime = BigUint::from_bytes_be(&PRIME);
        let a_key = derive(&a, &BigUint::from(2u8).modpow(&b, &prime).to_bytes_be()).unwrap();
        let b_key = derive(&b, &BigUint::from(2u8).modpow(&a, &prime).to_bytes_be()).unwrap();
        assert_eq!(a_key, b_key);
        let ciphertext = encrypt(&a_key, &[3; 16], &[7; 65]).unwrap();
        assert_ne!(ciphertext, vec![7; 65]);
        assert_eq!(
            decrypt(&b_key, &[3; 16], &ciphertext).unwrap().as_slice(),
            &[7; 65]
        );
        assert!(decrypt(&b_key, &[3; 15], &ciphertext).is_err());
        assert!(derive(&a, &[1]).is_err());
        assert!(derive(&a, &[2; 129]).is_err());
    }
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        sync::{Arc, Mutex},
    };

    struct Bus(Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Inventory {
        locked: Arc<Mutex<bool>>,
        ambiguous: Arc<Mutex<bool>>,
        expected: HashMap<String, String>,
    }
    #[zbus::interface(name = "org.freedesktop.Secret.Service")]
    impl Inventory {
        fn read_alias(&self, _alias: &str) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/collection/B").unwrap()
        }
        fn search_items(
            &self,
            attributes: HashMap<String, String>,
        ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
            assert_eq!(attributes, self.expected);
            let original = OwnedObjectPath::try_from("/collection/A/original").unwrap();
            if *self.ambiguous.lock().unwrap() {
                return (
                    vec![
                        original,
                        OwnedObjectPath::try_from("/collection/B/duplicate").unwrap(),
                    ],
                    vec![],
                );
            }
            if *self.locked.lock().unwrap() {
                (vec![], vec![original])
            } else {
                (vec![original], vec![])
            }
        }
    }
    struct EmptyDefault;
    #[zbus::interface(name = "org.freedesktop.Secret.Collection")]
    impl EmptyDefault {
        #[zbus(property)]
        fn locked(&self) -> bool {
            false
        }
        fn search_items(&self, _attributes: HashMap<String, String>) -> Vec<OwnedObjectPath> {
            vec![]
        }
    }
    struct OriginalItem {
        deleted: Arc<Mutex<usize>>,
    }
    #[zbus::interface(name = "org.freedesktop.Secret.Item")]
    impl OriginalItem {
        #[zbus(property)]
        fn locked(&self) -> bool {
            false
        }
        fn get_secret(&self, session: OwnedObjectPath) -> Secret {
            let iv = [4; 16];
            (
                session,
                iv.to_vec(),
                encrypt(&[7; 16], &iv, &[9; 65]).unwrap(),
                "application/octet-stream".into(),
            )
        }
        fn delete(&self) -> OwnedObjectPath {
            *self.deleted.lock().unwrap() += 1;
            OwnedObjectPath::try_from("/").unwrap()
        }
    }
    #[tokio::test]
    async fn retained_slot_resolution_survives_default_alias_change_and_rejects_locked_or_ambiguous_matches()
     {
        // Isolated test bus, not the user's credential service or collection.
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _bus = Bus(child);
        let scope = CredentialScope::new(spaceterm_client::pairing::credentials::CredentialScopeParts {
            namespace: spaceterm_client::pairing::credentials::CredentialNamespace::new("dev.spaceterm.test".into()).unwrap(),
            purpose: spaceterm_client::pairing::credentials::CredentialPurpose::PairingAuthentication { server: spaceterm_core::identity::ServerId::new([1;16]) },
            reference: spaceterm_core::pairing::CredentialReference::new([2;16]),
        }).unwrap();
        let locked = Arc::new(Mutex::new(false));
        let ambiguous = Arc::new(Mutex::new(false));
        let deleted = Arc::new(Mutex::new(0));
        let _service = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name(DESTINATION)
            .unwrap()
            .serve_at(
                ROOT,
                Inventory {
                    locked: locked.clone(),
                    ambiguous: ambiguous.clone(),
                    expected: attributes(&scope),
                },
            )
            .unwrap()
            .serve_at("/collection/B", EmptyDefault)
            .unwrap()
            .serve_at(
                "/collection/A/original",
                OriginalItem {
                    deleted: deleted.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let session = Session {
            connection: zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build()
                .await
                .unwrap(),
            path: OwnedObjectPath::try_from("/session/test").unwrap(),
            key: Zeroizing::new([7; 16]),
        };
        assert_eq!(
            session.item(&scope).await.unwrap().unwrap().as_str(),
            "/collection/A/original",
            "retained slots must not follow the mutable default alias"
        );
        let item = session.item(&scope).await.unwrap().unwrap();
        assert_eq!(session.read_item(&item).await.unwrap().as_slice(), &[9; 65]);
        assert!(session.remove(&scope, &[9; 65]).await.unwrap());
        assert_eq!(*deleted.lock().unwrap(), 1);
        *locked.lock().unwrap() = true;
        assert_eq!(
            session.remove(&scope, &[9; 65]).await,
            Err(PairingFailure::CredentialUnavailable)
        );
        assert_eq!(*deleted.lock().unwrap(), 1);
        assert_eq!(
            session.item(&scope).await,
            Err(PairingFailure::CredentialUnavailable)
        );
        *ambiguous.lock().unwrap() = true;
        assert_eq!(
            session.item(&scope).await,
            Err(PairingFailure::CredentialIntegrity)
        );
    }
}
