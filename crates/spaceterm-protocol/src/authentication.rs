//! Pinned-server, paired-device authentication over exact bounded transcript bytes.
//!
//! Transports supply peer-account and TLS checks separately. This module never
//! enrolls a device, loads a credential, starts a server, or grants native authority.

use crate::{
    ProtocolError,
    connection::{
        AuthenticatedConnection, ConnectionCodec, ConnectionParameters, RestrictedConnection,
    },
    limits::{MAX_HANDSHAKE_BYTES, NegotiatedLimits},
    negotiation::{Negotiator, SUPPORTED_MAJORS, validate_advertisement},
    preflight,
    wire::common,
};
use prost::Message;
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
    signature,
};
use std::sync::Arc;

/// Opaque protocol-level identity. Domain conversions belong in a separate module.
pub type Identity = [u8; 16];

/// A native signing capability. Implementations retain keys outside wire DTOs.
pub trait SigningKey: Send + Sync {
    fn public_key(&self) -> [u8; 32];
    fn sign(&self, transcript: &[u8]) -> Result<[u8; 64], ProtocolError>;
}

/// Enrolled server identity and Ed25519 pin. Neither transport location nor TLS alone replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPin {
    pub identity: Identity,
    pub public_key: [u8; 32],
}

/// The stable identities a successful handshake binds together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Principal {
    pub server: Identity,
    pub device: Identity,
    pub pairing: Identity,
    pub client: Identity,
}

/// Lookup result for one exact Client-to-pairing binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingRecord {
    pub principal: Principal,
    pub device_public_key: [u8; 32],
    pub active: bool,
}

/// Returns current authorization, not a cached pre-revocation snapshot.
/// Retained revoked records may be returned for restricted revocation/status only.
/// Server composition must serialize connection registration/readiness publication
/// with revocation and close registered ordinary connections when revocation commits.
pub trait PairingLookup {
    fn lookup(
        &self,
        server: Identity,
        pairing: Identity,
        client: Identity,
    ) -> Result<PairingRecord, ProtocolError>;
}

/// An ordinary terminal connection and a restricted retry are different type states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Ordinary,
    RevocationStatus,
}
impl Purpose {
    pub(crate) fn wire(self) -> i32 {
        match self {
            Self::Ordinary => 1,
            Self::RevocationStatus => 2,
        }
    }
    fn from_wire(value: i32) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Ordinary),
            2 => Ok(Self::RevocationStatus),
            _ => Err(ProtocolError::Malformed),
        }
    }
}

/// Explicit advertisements. Production defaults advertise both complete profiles.
#[derive(Debug, Clone)]
pub struct HandshakeConfig {
    pub majors: Vec<u32>,
    pub capabilities: Vec<u32>,
    pub limits: NegotiatedLimits,
}
impl Default for HandshakeConfig {
    fn default() -> Self {
        Self {
            majors: SUPPORTED_MAJORS.to_vec(),
            capabilities: Vec::new(),
            limits: NegotiatedLimits::default(),
        }
    }
}
impl HandshakeConfig {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_advertisement(&self.majors)?;
        if self
            .majors
            .iter()
            .any(|major| !SUPPORTED_MAJORS.contains(major))
        {
            return Err(ProtocolError::Incompatible);
        }
        validate_capabilities(&self.capabilities)?;
        self.limits.validate()?;
        Ok(())
    }
}

/// Only a completed handshake can construct either connection type.
pub enum Established {
    Ordinary(AuthenticatedConnection),
    Restricted(RestrictedConnection),
}
impl Established {
    fn new(codec: ConnectionCodec, purpose: Purpose) -> Self {
        match purpose {
            Purpose::Ordinary => Self::Ordinary(AuthenticatedConnection::new(codec)),
            Purpose::RevocationStatus => Self::Restricted(RestrictedConnection::new(codec)),
        }
    }
}

/// Client state retaining its original hello bytes until signed readiness.
pub struct ClientHandshake {
    pin: ServerPin,
    principal: Principal,
    purpose: Purpose,
    config: HandshakeConfig,
    signer: Arc<dyn SigningKey>,
    hello: Vec<u8>,
}

/// Client state after verifying the pinned server's selection and exact transcript.
pub struct AwaitingReady {
    pin: ServerPin,
    principal: Principal,
    purpose: Purpose,
    signer: Arc<dyn SigningKey>,
    hello: Vec<u8>,
    reply: Vec<u8>,
    device_proof: [u8; 64],
    binding: [u8; 32],
    major: u32,
    limits: NegotiatedLimits,
    capabilities: Vec<u32>,
    epoch: u64,
}

impl ClientHandshake {
    /// Generates a fresh client nonce. There is no production deterministic-nonce mode.
    pub fn begin(
        pin: ServerPin,
        principal: Principal,
        purpose: Purpose,
        config: HandshakeConfig,
        signer: Arc<dyn SigningKey>,
    ) -> Result<(Self, Vec<u8>), ProtocolError> {
        config.validate()?;
        validate_principal(principal)?;
        if principal.server != pin.identity {
            return Err(ProtocolError::Authentication);
        }
        let hello = common::ClientHello {
            expected_server: Some(identity(pin.identity)),
            device: Some(identity(principal.device)),
            pairing: Some(identity(principal.pairing)),
            client: Some(identity(principal.client)),
            nonce: nonce()?.to_vec(),
            majors: config.majors.clone(),
            capabilities: config.capabilities.clone(),
            limits: Some(config.limits.to_wire()),
            purpose: purpose.wire(),
        }
        .encode_to_vec();
        handshake_size(&hello)?;
        let outbound = hello.clone();
        Ok((
            Self {
                pin,
                principal,
                purpose,
                config,
                signer,
                hello,
            },
            outbound,
        ))
    }

    /// Verifies the server proof against the original hello, never a reconstructed hello.
    pub fn receive_server(
        self,
        signed_reply: &[u8],
    ) -> Result<(AwaitingReady, Vec<u8>), ProtocolError> {
        let signed = decode_signed(signed_reply, MAX_HANDSHAKE_BYTES)?;
        verify(
            self.pin.public_key,
            b"server-hello",
            &[&self.hello, &signed.body],
            &signed.signature,
        )?;
        let reply: common::ServerHello =
            decode_common(preflight::SPACETERM_COMMON_SERVERHELLO, &signed.body)?;
        if get_identity(reply.server.as_ref())? != self.pin.identity
            || Purpose::from_wire(reply.purpose)? != self.purpose
        {
            return Err(ProtocolError::Authentication);
        }
        Negotiator::verify(&self.config.majors, &reply.majors, reply.selected_major)?;
        validate_capabilities(&reply.capabilities)?;
        if reply
            .capabilities
            .iter()
            .any(|capability| !self.config.capabilities.contains(capability))
        {
            return Err(ProtocolError::Negotiation);
        }
        let limits = NegotiatedLimits::from_wire(
            reply
                .accepted_limits
                .as_ref()
                .ok_or(ProtocolError::Malformed)?,
        )?;
        if self.config.limits.intersect(limits)? != limits {
            return Err(ProtocolError::Negotiation);
        }
        let proof = sign(
            self.signer.as_ref(),
            b"device-proof",
            &[&self.hello, &signed.body],
        )?;
        let binding = transcript_binding(&self.hello, &signed.body);
        Ok((
            AwaitingReady {
                pin: self.pin,
                principal: self.principal,
                purpose: self.purpose,
                signer: self.signer,
                hello: self.hello,
                reply: signed.body,
                device_proof: proof,
                binding,
                major: reply.selected_major,
                limits,
                capabilities: reply.capabilities,
                epoch: reply.epoch,
            },
            proof.to_vec(),
        ))
    }
}

impl AwaitingReady {
    /// Readiness is server-signed and binds the verified device proof and purpose.
    pub fn receive_ready(self, signed_ready: &[u8]) -> Result<Established, ProtocolError> {
        let signed = decode_signed(signed_ready, MAX_HANDSHAKE_BYTES)?;
        verify(
            self.pin.public_key,
            b"ready",
            &[&self.hello, &self.reply, &signed.body],
            &signed.signature,
        )?;
        let ready: common::Ready = decode_common(preflight::SPACETERM_COMMON_READY, &signed.body)?;
        if ready.transcript != self.binding
            || ready.device_proof != self.device_proof
            || get_identity(ready.pairing.as_ref())? != self.principal.pairing
            || get_identity(ready.client.as_ref())? != self.principal.client
            || Purpose::from_wire(ready.purpose)? != self.purpose
        {
            return Err(ProtocolError::Authentication);
        }
        Ok(Established::new(
            ConnectionCodec::new(ConnectionParameters {
                principal: self.principal,
                purpose: self.purpose,
                major: self.major,
                limits: self.limits,
                capabilities: self.capabilities,
                epoch: self.epoch,
                binding: self.binding,
                direction: common::Direction::ClientToServer,
                signer: self.signer,
                peer_key: self.pin.public_key,
            }),
            self.purpose,
        ))
    }
}

/// Server state with the exact received hello and generated reply.
pub struct ServerHandshake {
    principal: Principal,
    purpose: Purpose,
    signer: Arc<dyn SigningKey>,
    device_public_key: [u8; 32],
    hello: Vec<u8>,
    reply: Vec<u8>,
    binding: [u8; 32],
    major: u32,
    limits: NegotiatedLimits,
    capabilities: Vec<u32>,
    epoch: u64,
}
impl ServerHandshake {
    /// Builds a signed reply after checking the expected server and original binding.
    pub fn accept(
        server: Identity,
        epoch: u64,
        config: HandshakeConfig,
        signer: Arc<dyn SigningKey>,
        pairings: &dyn PairingLookup,
        hello_bytes: &[u8],
    ) -> Result<(Self, Vec<u8>), ProtocolError> {
        config.validate()?;
        if epoch == 0 || server == [0; 16] {
            return Err(ProtocolError::Malformed);
        }
        let hello: common::ClientHello =
            decode_common(preflight::SPACETERM_COMMON_CLIENTHELLO, hello_bytes)?;
        if get_identity(hello.expected_server.as_ref())? != server {
            return Err(ProtocolError::Authentication);
        }
        let principal = Principal {
            server,
            device: get_identity(hello.device.as_ref())?,
            pairing: get_identity(hello.pairing.as_ref())?,
            client: get_identity(hello.client.as_ref())?,
        };
        let purpose = Purpose::from_wire(hello.purpose)?;
        let record = checked_pairing(pairings, principal, purpose)?;
        let major = Negotiator::select(&hello.majors, &config.majors)?;
        // The server independently verifies its own result before signing it.
        Negotiator::verify(&hello.majors, &config.majors, major)?;
        validate_capabilities(&hello.capabilities)?;
        let capabilities: Vec<_> = config
            .capabilities
            .iter()
            .copied()
            .filter(|capability| hello.capabilities.contains(capability))
            .collect();
        let limits = config.limits.intersect(NegotiatedLimits::from_wire(
            hello.limits.as_ref().ok_or(ProtocolError::Malformed)?,
        )?)?;
        let reply = common::ServerHello {
            server: Some(identity(server)),
            nonce: nonce()?.to_vec(),
            epoch,
            connection_nonce: nonce()?.to_vec(),
            majors: config.majors,
            selected_major: major,
            capabilities: capabilities.clone(),
            accepted_limits: Some(limits.to_wire()),
            purpose: purpose.wire(),
        }
        .encode_to_vec();
        handshake_size(&reply)?;
        let signature = sign(signer.as_ref(), b"server-hello", &[hello_bytes, &reply])?;
        let outbound = common::Signed {
            body: reply.clone(),
            signature: signature.to_vec(),
        }
        .encode_to_vec();
        handshake_size(&outbound)?;
        let binding = transcript_binding(hello_bytes, &reply);
        Ok((
            Self {
                principal,
                purpose,
                signer,
                device_public_key: record.device_public_key,
                hello: hello_bytes.to_vec(),
                reply,
                binding,
                major,
                limits,
                capabilities,
                epoch,
            },
            outbound,
        ))
    }

    /// Rechecks revocation and the Client binding immediately before signed readiness.
    pub fn receive_device(
        self,
        pairings: &dyn PairingLookup,
        device_proof: &[u8],
    ) -> Result<(Established, Vec<u8>), ProtocolError> {
        if device_proof.len() != 64 {
            return Err(ProtocolError::Authentication);
        }
        let record = checked_pairing(pairings, self.principal, self.purpose)?;
        if record.device_public_key != self.device_public_key {
            return Err(ProtocolError::PairingDenied);
        }
        verify(
            record.device_public_key,
            b"device-proof",
            &[&self.hello, &self.reply],
            device_proof,
        )?;
        let ready = common::Ready {
            transcript: self.binding.to_vec(),
            device_proof: device_proof.to_vec(),
            pairing: Some(identity(self.principal.pairing)),
            client: Some(identity(self.principal.client)),
            purpose: self.purpose.wire(),
        }
        .encode_to_vec();
        let signature = sign(
            self.signer.as_ref(),
            b"ready",
            &[&self.hello, &self.reply, &ready],
        )?;
        let outbound = common::Signed {
            body: ready,
            signature: signature.to_vec(),
        }
        .encode_to_vec();
        handshake_size(&outbound)?;
        // A native signer may block while revocation commits. Do not publish its
        // readiness proof using the authorization read before that native call.
        let current = checked_pairing(pairings, self.principal, self.purpose)?;
        if current.device_public_key != record.device_public_key {
            return Err(ProtocolError::PairingDenied);
        }
        let connection = Established::new(
            ConnectionCodec::new(ConnectionParameters {
                principal: self.principal,
                purpose: self.purpose,
                major: self.major,
                limits: self.limits,
                capabilities: self.capabilities,
                epoch: self.epoch,
                binding: self.binding,
                direction: common::Direction::ServerToClient,
                signer: self.signer,
                peer_key: record.device_public_key,
            }),
            self.purpose,
        );
        Ok((connection, outbound))
    }
}

fn checked_pairing(
    pairings: &dyn PairingLookup,
    principal: Principal,
    purpose: Purpose,
) -> Result<PairingRecord, ProtocolError> {
    validate_principal(principal)?;
    let record = pairings.lookup(principal.server, principal.pairing, principal.client)?;
    if record.principal != principal || (purpose == Purpose::Ordinary && !record.active) {
        return Err(ProtocolError::PairingDenied);
    }
    Ok(record)
}
fn validate_principal(value: Principal) -> Result<(), ProtocolError> {
    if [value.server, value.device, value.pairing, value.client].contains(&[0; 16]) {
        return Err(ProtocolError::Malformed);
    }
    Ok(())
}
fn validate_capabilities(values: &[u32]) -> Result<(), ProtocolError> {
    if values.len() > 16 || values.contains(&0) || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ProtocolError::Malformed);
    }
    Ok(())
}
fn nonce() -> Result<[u8; 32], ProtocolError> {
    let mut result = [0; 32];
    SystemRandom::new()
        .fill(&mut result)
        .map_err(|_| ProtocolError::RandomUnavailable)?;
    Ok(result)
}
pub(crate) fn identity(value: Identity) -> common::Identity {
    common::Identity {
        value: value.to_vec(),
    }
}
pub(crate) fn get_identity(value: Option<&common::Identity>) -> Result<Identity, ProtocolError> {
    let result: Identity = value
        .ok_or(ProtocolError::Malformed)?
        .value
        .as_slice()
        .try_into()
        .map_err(|_| ProtocolError::Malformed)?;
    if result == [0; 16] {
        return Err(ProtocolError::Malformed);
    }
    Ok(result)
}
fn handshake_size(bytes: &[u8]) -> Result<(), ProtocolError> {
    if bytes.len() > MAX_HANDSHAKE_BYTES {
        return Err(ProtocolError::LimitExceeded);
    }
    Ok(())
}
pub(crate) fn decode_common<T: Message + Default>(
    schema: usize,
    bytes: &[u8],
) -> Result<T, ProtocolError> {
    handshake_size(bytes)?;
    preflight::check(schema, bytes, NegotiatedLimits::default())?;
    T::decode(bytes).map_err(|_| ProtocolError::Malformed)
}
pub(crate) fn decode_signed(bytes: &[u8], maximum: usize) -> Result<common::Signed, ProtocolError> {
    if bytes.len() > maximum {
        return Err(ProtocolError::LimitExceeded);
    }
    preflight::check(
        preflight::SPACETERM_COMMON_SIGNED,
        bytes,
        NegotiatedLimits::default(),
    )?;
    common::Signed::decode(bytes).map_err(|_| ProtocolError::Malformed)
}

// Every component, including the domain label, has an explicit length. The
// transcript is unambiguous even when protobuf admits alternate field order.
fn signing_bytes(domain: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut result = Vec::new();
    for part in [b"SpaceTerm Protocol Ed25519 v1".as_slice(), domain]
        .into_iter()
        .chain(parts.iter().copied())
    {
        result.extend_from_slice(&(part.len() as u64).to_be_bytes());
        result.extend_from_slice(part);
    }
    result
}
pub(crate) fn sign(
    signer: &dyn SigningKey,
    domain: &[u8],
    parts: &[&[u8]],
) -> Result<[u8; 64], ProtocolError> {
    signer.sign(&signing_bytes(domain, parts))
}
pub(crate) fn verify(
    key: [u8; 32],
    domain: &[u8],
    parts: &[&[u8]],
    proof: &[u8],
) -> Result<(), ProtocolError> {
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(&signing_bytes(domain, parts), proof)
        .map_err(|_| ProtocolError::Authentication)
}
fn transcript_binding(hello: &[u8], reply: &[u8]) -> [u8; 32] {
    let value = digest::digest(
        &digest::SHA256,
        &signing_bytes(b"connection-binding", &[hello, reply]),
    );
    let mut result = [0; 32];
    result.copy_from_slice(value.as_ref());
    result
}
