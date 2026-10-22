use super::*;
use crate::{host::Paths, storage::Directory};

pub(super) const MAX_DEVICES: usize = 256;
pub(super) const MAX_REVOCATIONS: usize = 512;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Device {
    pub claim: ClaimId,
    pub info: DeviceInfo,
    pub revoked: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revocation {
    pub actor: Option<PairingId>,
    pub receipt: RevocationReceipt,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub schema: u8,
    pub server: [u8; 32],
    pub key: Vec<u8>,
    pub certificate: Vec<u8>,
    pub policy: Option<NetworkPolicy>,
    pub devices: Vec<Device>,
    pub revocations: Vec<Revocation>,
}
impl Drop for Record {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.key);
    }
}

pub(super) struct Store {
    directory: Directory,
    server: [u8; 32],
    build: String,
}
impl Store {
    pub fn open(paths: &Paths) -> Result<Self, DirectError> {
        let directory = Directory::open(&paths.data, false)?;
        Ok(Self {
            server: directory.read()?.server,
            directory,
            build: paths.identity.clone(),
        })
    }
    pub fn load(&self) -> Result<Record, DirectError> {
        let _lock = self.directory.lock("direct.lock", false)?;
        let record = match self.directory.read_json::<Record>("direct.json")? {
            Some(record) => record,
            None => {
                let key = DeviceCredential::generate()?;
                let pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
                    &key.expose_bytes().into(),
                    &rcgen::PKCS_ED25519,
                )
                .map_err(|_| DirectError::StorageUnavailable)?;
                let certificate = rcgen::CertificateParams::new(vec!["spaceterm.invalid".into()])
                    .and_then(|params| params.self_signed(&pair))
                    .map_err(|_| DirectError::StorageUnavailable)?;
                let record = Record {
                    schema: 1,
                    server: self.server,
                    key: key.expose_bytes().to_vec(),
                    certificate: certificate.der().to_vec(),
                    policy: None,
                    devices: Vec::new(),
                    revocations: Vec::new(),
                };
                self.directory.write_json(c"direct.json", &record)?;
                record
            }
        };
        self.validate(&record)?;
        Ok(record)
    }
    fn validate(&self, record: &Record) -> Result<(), DirectError> {
        if record.schema != 1
            || record.server != self.server
            || record.devices.len() > MAX_DEVICES
            || record.revocations.len() > MAX_REVOCATIONS
            || record.key.len() > 256
            || record.certificate.len() > 8192
        {
            return Err(DirectError::StorageUnavailable);
        }
        crypto::public(&record.key)?;
        if let Some(policy) = &record.policy {
            NetworkPolicy::new(policy.bind).map_err(|_| DirectError::StorageUnavailable)?;
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut clients = std::collections::BTreeSet::new();
        let mut claims = std::collections::BTreeSet::new();
        for device in &record.devices {
            if device.info.pairing == [0; 32]
                || device.info.client == [0; 32]
                || device.info.public_key == [0; 32]
                || device.claim == [0; 32]
                || !claims.insert(device.claim)
                || !ids.insert(device.info.pairing)
                || !clients.insert(device.info.client)
            {
                return Err(DirectError::StorageUnavailable);
            }
        }
        let mut originals = std::collections::BTreeSet::new();
        for original in &record.revocations {
            if original.receipt.server != self.server
                || original.receipt.operation == [0; 32]
                || !record
                    .devices
                    .iter()
                    .any(|d| d.info.pairing == original.receipt.target && d.revoked)
                || original.actor.is_some_and(|actor| !ids.contains(&actor))
                || !originals.insert((original.actor, original.receipt.operation))
            {
                return Err(DirectError::StorageUnavailable);
            }
        }
        Ok(())
    }
    pub fn save(&self, record: &Record) -> Result<(), DirectError> {
        self.validate(record)?;
        self.directory
            .write_json(c"direct.json", record)
            .map_err(Into::into)
    }
    pub fn pin(&self, record: &Record) -> Result<ServerPin, DirectError> {
        Ok(ServerPin {
            build: self.build.clone(),
            server: self.server,
            public_key: crypto::public(&record.key)?,
            certificate: crypto::hash(&record.certificate),
        })
    }
}
