use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        ClientHandshake, Established, HandshakeConfig, PairingLookup, PairingRecord, Principal,
        Purpose, ServerHandshake, ServerPin, SigningKey,
    },
    connection::AuthenticatedConnection,
    wire::common,
};
use std::{cell::Cell, sync::Arc};

pub struct TestKey(Ed25519KeyPair);
impl TestKey {
    pub fn seeded(seed: u8) -> Arc<Self> {
        Arc::new(Self(
            Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
        ))
    }
}
impl SigningKey for TestKey {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}

pub struct Pairings {
    pub record: PairingRecord,
    pub active: Cell<bool>,
}
impl PairingLookup for Pairings {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        if (server, pairing, client)
            != (
                self.record.principal.server,
                self.record.principal.pairing,
                self.record.principal.client,
            )
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(PairingRecord {
            active: self.active.get(),
            ..self.record
        })
    }
}

pub struct Fixture {
    pub server_key: Arc<TestKey>,
    pub device_key: Arc<TestKey>,
    pub pin: ServerPin,
    pub principal: Principal,
    pub pairings: Pairings,
}
impl Fixture {
    pub fn new() -> Self {
        let server_key = TestKey::seeded(41);
        let device_key = TestKey::seeded(42);
        let principal = Principal {
            server: [1; 16],
            device: [2; 16],
            pairing: [3; 16],
            client: [4; 16],
        };
        Self {
            pin: ServerPin {
                identity: principal.server,
                public_key: server_key.public_key(),
            },
            pairings: Pairings {
                record: PairingRecord {
                    principal,
                    device_public_key: device_key.public_key(),
                    active: true,
                },
                active: Cell::new(true),
            },
            principal,
            server_key,
            device_key,
        }
    }
    pub fn connect(&self, majors: &[u32]) -> (AuthenticatedConnection, AuthenticatedConnection) {
        let (client, server) = self.establish(majors, Purpose::Ordinary);
        let Established::Ordinary(client) = client else {
            panic!("ordinary client")
        };
        let Established::Ordinary(server) = server else {
            panic!("ordinary server")
        };
        (client, server)
    }
    pub fn establish(&self, majors: &[u32], purpose: Purpose) -> (Established, Established) {
        let config = HandshakeConfig {
            majors: majors.to_vec(),
            ..Default::default()
        };
        let (client, hello) = ClientHandshake::begin(
            self.pin,
            self.principal,
            purpose,
            config,
            self.device_key.clone(),
        )
        .unwrap();
        let (server, reply) = ServerHandshake::accept(
            self.principal.server,
            7,
            HandshakeConfig::default(),
            self.server_key.clone(),
            &self.pairings,
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (server, ready) = server.receive_device(&self.pairings, &proof).unwrap();
        (client.receive_ready(&ready).unwrap(), server)
    }
}

pub fn id(value: u8) -> common::Identity {
    common::Identity {
        value: vec![value; 16],
    }
}
pub fn entity(value: u64) -> common::Entity {
    common::Entity {
        server: Some(id(1)),
        local_id: value,
    }
}
pub fn operation_id() -> common::OperationId {
    common::OperationId {
        server: Some(id(1)),
        pairing: Some(id(3)),
        client: Some(id(4)),
        stream: Some(id(5)),
        sequence: 1,
    }
}
pub fn fence() -> common::EffectFence {
    common::EffectFence {
        session: Some(common::Session {
            pane: Some(entity(3)),
            server_epoch: 7,
            generation: 1,
        }),
        client: Some(id(4)),
        control_generation: 1,
        connection_generation: 1,
        context_generation: 1,
    }
}
