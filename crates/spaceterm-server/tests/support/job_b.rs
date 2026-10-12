//! Authentication fixtures. No test credential is selectable by production main.
#![allow(dead_code)]

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::ServerEpoch,
    identity::{ClientId, ServerId},
};
use spaceterm_host::{
    ipc::{IpcListener, IpcTransport, PrivateIpcDirectory},
    transport::{AdmissionLimits, MessageLimit, MessageTransport},
};
use spaceterm_protocol::{ProtocolError, authentication::*};
use spaceterm_server::{api::VerifiedPrincipal, connection::*, contracts::*};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

pub fn server_id() -> ServerId {
    ServerId::new([1; 16])
}
pub fn local(n: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&n.to_be_bytes());
    bytes
}
pub fn principal(n: u64) -> Principal {
    Principal {
        server: server_id().bytes(),
        device: [2; 16],
        pairing: local(1),
        client: local(n),
    }
}

pub struct Key {
    pair: Ed25519KeyPair,
    pub signs: AtomicUsize,
}
impl Key {
    pub fn new(seed: u8) -> Arc<Self> {
        Arc::new(Self {
            pair: Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
            signs: AtomicUsize::new(0),
        })
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.pair.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        self.signs.fetch_add(1, Ordering::SeqCst);
        Ok(self.pair.sign(bytes).as_ref().try_into().unwrap())
    }
}
impl ServerCredentialPort for Key {}

pub struct Pairings {
    pub key: Arc<Key>,
    pub active: AtomicBool,
    pub reads: AtomicUsize,
}
impl PairingLookup for Pairings {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if server != server_id().bytes()
            || pairing != local(1)
            || ![local(1), local(2), local(3)].contains(&client)
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(PairingRecord {
            principal: Principal {
                server,
                device: [2; 16],
                pairing,
                client,
            },
            device_public_key: self.key.public_key(),
            active: self.active.load(Ordering::SeqCst),
        })
    }
}
impl PairingAuthorizationPort for Pairings {}

#[derive(Default)]
pub struct Authority {
    pub clients: Mutex<BTreeMap<ClientId, VerifiedPrincipal>>,
    pub registered: AtomicUsize,
    pub disconnected: AtomicUsize,
    pub register_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}
impl ConnectionAuthorityPort for Authority {
    fn register(
        &self,
        principal: VerifiedPrincipal,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move {
            self.registered.fetch_add(1, Ordering::SeqCst);
            self.clients
                .lock()
                .unwrap()
                .insert(principal.client(), principal);
            let gate = self.register_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                let permit = gate.acquire().await.unwrap();
                permit.forget();
            }
            Ok(())
        })
    }
    fn disconnect(
        &self,
        principal: VerifiedPrincipal,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move {
            self.disconnected.fetch_add(1, Ordering::SeqCst);
            let mut clients = self.clients.lock().unwrap();
            if clients.get(&principal.client()) == Some(&principal) {
                clients.remove(&principal.client());
            }
            Ok(())
        })
    }
}

pub struct Directory {
    pub path: PathBuf,
    pub directory: PrivateIpcDirectory,
}
impl Directory {
    pub fn new() -> Self {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).unwrap();
        let path = PathBuf::from(format!("/tmp/st-b{:016x}", u64::from_be_bytes(bytes)));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateIpcDirectory::open(&path).unwrap();
        Self { path, directory }
    }
    pub fn listener(&self) -> IpcListener {
        IpcListener::bind(
            self.directory.endpoint("s").unwrap(),
            MessageLimit::envelope(),
            AdmissionLimits::new(16, Duration::from_secs(5)).unwrap(),
        )
        .unwrap()
    }
    pub async fn connect(&self) -> IpcTransport {
        IpcTransport::connect(
            &self.directory.endpoint("s").unwrap(),
            MessageLimit::envelope(),
        )
        .await
        .unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub struct Fixture {
    pub key: Arc<Key>,
    pub device: Arc<Key>,
    pub pairings: Arc<Pairings>,
    pub authority: Arc<Authority>,
}
impl Fixture {
    pub fn new() -> Self {
        Self::with_seeds(1, 2)
    }
    pub fn with_seeds(server_seed: u8, device_seed: u8) -> Self {
        let key = Key::new(server_seed);
        let device = Key::new(device_seed);
        Self {
            key,
            pairings: Arc::new(Pairings {
                key: device.clone(),
                active: AtomicBool::new(true),
                reads: AtomicUsize::new(0),
            }),
            device,
            authority: Arc::new(Authority::default()),
        }
    }
    pub fn config(&self, uid: u32) -> AuthenticationConfig {
        AuthenticationConfig {
            server: server_id(),
            epoch: ServerEpoch::new(2),
            owning_uid: uid,
            handshake: HandshakeConfig {
                capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
                ..HandshakeConfig::default()
            },
            credentials: self.key.clone(),
            pairing: self.pairings.clone(),
            authority: self.authority.clone(),
            unauthenticated_limit: 16,
            authenticated_limit: 32,
            deadline: Duration::from_secs(5),
        }
    }
    pub async fn client_handshake(
        &self,
        client: &dyn MessageTransport,
        n: u64,
        major: u32,
        purpose: Purpose,
    ) -> Result<Established, ProtocolError> {
        let config = HandshakeConfig {
            majors: vec![major],
            capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
            ..HandshakeConfig::default()
        };
        let (request, hello) = ClientHandshake::begin(
            ServerPin {
                identity: server_id().bytes(),
                public_key: self.key.public_key(),
            },
            principal(n),
            purpose,
            config,
            self.device.clone(),
        )?;
        client.send(&hello).await.unwrap();
        let Some(reply) = client.recv().await.unwrap() else {
            return Err(ProtocolError::Authentication);
        };
        let (request, proof) = request.receive_server(&reply)?;
        client.send(&proof).await.unwrap();
        let Some(ready) = client.recv().await.unwrap() else {
            return Err(ProtocolError::Authentication);
        };
        request.receive_ready(&ready)
    }
}

pub fn tls(
    seed: u8,
) -> (
    Arc<rustls::ServerConfig>,
    spaceterm_host::transport::EnrolledServerKey,
) {
    let (server, pin, _) = tls_with_client(seed);
    (server, pin)
}

pub fn tls_with_client(
    seed: u8,
) -> (
    Arc<rustls::ServerConfig>,
    spaceterm_host::transport::EnrolledServerKey,
    Arc<rustls::ClientConfig>,
) {
    // RFC 8410 PKCS#8 seed representation exists only in this test composition.
    let mut encoded = vec![
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    encoded.extend_from_slice(&[seed; 32]);
    let private = rustls::pki_types::PrivatePkcs8KeyDer::from(encoded);
    let key = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&private, &rcgen::PKCS_ED25519).unwrap();
    let pin =
        spaceterm_host::transport::EnrolledServerKey(key.public_key_raw().try_into().unwrap());
    let certificate = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let configuration = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate.der().clone()], private.into())
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    (Arc::new(configuration), pin, Arc::new(client))
}

pub fn status(revision: u64) -> spaceterm_core::management::ServerStatus {
    use spaceterm_core::{
        authority::FieldVersion, management::*, organization::Versioned,
        remote_access::RemoteAccessPolicy,
    };
    use std::num::NonZeroU64;
    ServerStatus::new(ServerStatusParts {
        server: server_id(),
        epoch: ServerEpoch::new(2),
        revision: NonZeroU64::new(revision).unwrap(),
        running: Some(ServerVersion::new("0.9.1".into()).unwrap()),
        approved: Some(ServerVersion::new("0.9.0".into()).unwrap()),
        staged: Some(ServerVersion::new("0.9.2".into()).unwrap()),
        native_verification: NativeVerification::Unverified,
        capabilities: vec![CapabilityStatus {
            capability: 42,
            availability: CapabilityAvailability::AuthorizationRequired,
        }],
        remote_access: Versioned {
            version: FieldVersion::new(3),
            value: RemoteAccessPolicy::enabled(
                server_id(),
                vec!["127.0.0.1:1234".parse().unwrap()],
            )
            .unwrap(),
        },
        listeners: vec![ListenerStatus {
            generation: NonZeroU64::new(3).unwrap(),
            endpoint: "127.0.0.1:1234".parse().unwrap(),
            state: ListenerState::Unavailable,
        }],
        progress: vec![ManagementProgress {
            request: ManagementRequestReference::Native([4; 16]),
            state: ManagementProgressState::WaitingForCleanup,
            completed_units: 1,
            total_units: 2,
        }],
    })
    .unwrap()
}
