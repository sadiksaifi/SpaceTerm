//! The frozen Invitation Link and direct authentication frames.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

/// The longest Invitation Link text the reader accepts.
const MAX_INVITATION_BYTES: usize = 8192;
const INVITATION_PREFIX: &str = "spaceterm://pair/";

/// The ALPN of a paired QUIC connection.
pub const ALPN: &[u8] = b"spaceterm/1";
/// The maximum size of one pairing authentication payload in bytes.
pub const MAX_AUTH_FRAME: usize = 16 * 1024;
/// How long an Invitation Link can be used.
pub const INVITATION_SECONDS: u64 = 300;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invite {
    pub schema: u8,
    pub id: ClaimId,
    pub endpoint: String,
    pub pin: MachinePin,
    pub machine_name: String,
    pub token: [u8; 32],
    pub expires: u64,
    pub valid_for_seconds: u64,
}
impl Invite {
    /// Encodes the released Invitation Link payload. The returned secret text is zeroized.
    pub fn encode(&self) -> Result<zeroize::Zeroizing<String>, DirectError> {
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(self).map_err(|_| DirectError::InvalidInvitation)?,
        );
        Ok(zeroize::Zeroizing::new(format!(
            "{INVITATION_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(&bytes)
        )))
    }

    /// Decodes the frozen payload. The caller validates the machine and endpoint before use.
    pub fn decode(text: &str) -> Result<Self, DirectError> {
        if text.len() > MAX_INVITATION_BYTES {
            return Err(DirectError::InvalidInvitation);
        }
        let encoded = text
            .strip_prefix(INVITATION_PREFIX)
            .ok_or(DirectError::InvalidInvitation)?;
        let bytes = zeroize::Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| DirectError::InvalidInvitation)?,
        );
        serde_json::from_slice(&bytes).map_err(|_| DirectError::InvalidInvitation)
    }
}

impl Drop for Invite {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.token);
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub major: u16,
    pub pin: MachinePin,
    pub nonce: [u8; 32],
    pub generation: [u8; 32],
    pub machine_name: String,
    pub proof: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
pub enum Action {
    Connect {
        paired: PairedDevice,
    },
    Claim {
        id: ClaimId,
        token: [u8; 32],
        key: [u8; 32],
        name: ClientName,
    },
    Finish {
        claim: ClaimId,
        key: [u8; 32],
    },
    Cancel {
        claim: ClaimId,
        key: [u8; 32],
    },
    Revoke {
        actor: PairedDevice,
        operation: OperationId,
        target: PairingId,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authentication {
    pub action: Action,
    pub proof: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
pub enum Answer {
    Connected,
    Claim(PairingClaim),
    Enrollment(Option<PairedDevice>),
    Revoked(RevocationReceipt),
    Cancelled(Option<RevocationReceipt>),
}

pub fn transcript<T: Serialize>(
    purpose: &str,
    pin: &MachinePin,
    nonce: &[u8; 32],
    action: &T,
) -> Result<zeroize::Zeroizing<Vec<u8>>, DirectError> {
    serde_json::to_vec(&("spaceterm-direct-1", purpose, pin, nonce, action))
        .map(zeroize::Zeroizing::new)
        .map_err(|_| DirectError::InvalidRequest)
}
/// The token digest remains a secret invitation capability. A client retains only
/// this one-way, key-bound claim ID for cancellation, never the invitation itself.
pub fn claim_id(
    pin: &MachinePin,
    invitation: ClaimId,
    token_digest: [u8; 32],
    key: [u8; 32],
) -> Result<ClaimId, DirectError> {
    let bytes = transcript("prepared-claim", pin, &invitation, &(token_digest, key))?;
    Ok(Sha256::new().update(&bytes).finalize())
}
pub fn exchange_hash(
    challenge: &Challenge,
    authentication: &Authentication,
) -> Result<[u8; 32], DirectError> {
    let bytes = transcript(
        "pairing-comparison",
        &challenge.pin,
        &challenge.nonce,
        &(&challenge.proof, &authentication.proof),
    )?;
    Ok(Sha256::new().update(&bytes).finalize())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Management {
    Status,
    /// Temporary provisioning, admitted only over owning-account IPC.
    PrepareSetup {
        generation: u64,
        pin: MachinePin,
        expected_policy: Option<NetworkPolicy>,
        expected_enabled: bool,
        endpoint: String,
        policy: NetworkPolicy,
        operation: OperationId,
        public_key: [u8; 32],
        name: ClientName,
    },
    Setup {
        generation: u64,
        pin: MachinePin,
        expected_policy: Option<NetworkPolicy>,
        expected_enabled: bool,
        endpoint: String,
        policy: NetworkPolicy,
        operation: OperationId,
        public_key: [u8; 32],
        name: ClientName,
    },
    /// Revokes the setup's pairing. With `restore`, it then puts Network Access back in the
    /// same step, only while Network Access is still what this setup installed.
    CancelSetup {
        pin: MachinePin,
        operation: OperationId,
        restore: Option<super::NetworkRestore>,
    },
    SetPolicy {
        policy: Option<NetworkPolicy>,
        enabled: bool,
    },
    /// Opens an invitation that advertises `advertise`, a `HOST[:PORT]` authority, when given.
    Invite {
        advertise: Option<String>,
    },
    Withdraw(ClaimId),
    /// Undecided claims, of one invitation when `invitation` names it.
    Claims {
        invitation: Option<ClaimId>,
    },
    Approve(PairingClaim),
    Reject(ClaimId),
    Devices,
    Revoke {
        operation: OperationId,
        target: PairingId,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub enum ManagementReply {
    Status(DirectStatus),
    SetupPrepared,
    Setup(PairedDevice),
    SetupCancelled {
        pin: MachinePin,
        operation: OperationId,
        /// What happened to Network Access, when the cancellation asked to restore it.
        restored: Option<super::RestoredAccess>,
    },
    Invitation(String),
    Claims(Vec<PairingClaim>),
    Approved(DeviceInfo),
    Rejected,
    Devices(Vec<DeviceInfo>),
    Revoked(RevocationReceipt),
}
impl std::fmt::Debug for ManagementReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirectManagementReply")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_link_preserves_the_released_payload_bytes() {
        let json = concat!(
            r#"{"schema":1,"id":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"#,
            r#""endpoint":"quic://machine.example:9231","pin":{"build":"spaceterm","#,
            r#""core":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"#,
            r#""public_key":[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3],"#,
            r#""certificate":[4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4]},"#,
            r#""machine_name":"Machine","token":[5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5],"#,
            r#""expires":1700000000,"valid_for_seconds":300}"#,
        );
        let link = format!("spaceterm://pair/{}", URL_SAFE_NO_PAD.encode(json));
        let invite = Invite::decode(&link).unwrap();
        assert_eq!(invite.encode().unwrap().as_str(), link);
    }

    #[test]
    fn comparison_hash_preserves_the_released_transcript() {
        let challenge = Challenge {
            major: 1,
            pin: MachinePin {
                build: "spaceterm".into(),
                core: [2; 32],
                public_key: [3; 32],
                certificate: [4; 32],
            },
            nonce: [5; 32],
            generation: [0; 32],
            machine_name: "Machine".into(),
            proof: vec![6, 7, 8],
        };
        let authentication = Authentication {
            action: Action::Finish {
                claim: [0; 32],
                key: [0; 32],
            },
            proof: vec![9, 10],
        };
        let hash = exchange_hash(&challenge, &authentication).unwrap();
        assert_eq!(
            hash,
            [
                0xd2, 0x96, 0x30, 0xde, 0x82, 0x5a, 0x57, 0x09, 0x3b, 0xef, 0xa7, 0xef, 0x14, 0x3f,
                0xd4, 0x18, 0x47, 0x2e, 0xd1, 0x99, 0xd8, 0x64, 0x32, 0xe5, 0x93, 0xb8, 0xe1, 0x35,
                0xdb, 0x3d, 0x55, 0xae,
            ],
        );
    }
}
