use super::*;

pub(super) const MAX_AUTH_FRAME: usize = 16 * 1024;
pub(super) const INVITATION_SECONDS: u64 = 300;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Invite {
    pub schema: u8,
    pub id: ClaimId,
    pub endpoint: String,
    pub pin: ServerPin,
    pub server_name: String,
    pub token: [u8; 32],
    pub expires: u64,
}
impl Drop for Invite {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.token);
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Challenge {
    pub major: u16,
    pub pin: ServerPin,
    pub nonce: [u8; 32],
    pub generation: [u8; 32],
    pub server_name: String,
    pub proof: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
pub(super) enum Action {
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
pub(super) struct Authentication {
    pub action: Action,
    pub proof: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
pub(super) enum Answer {
    Connected,
    Claim(ClaimId),
    Enrollment(Option<PairedDevice>),
    Revoked(RevocationReceipt),
    Cancelled(Option<RevocationReceipt>),
}

pub(super) fn transcript<T: Serialize>(
    purpose: &str,
    pin: &ServerPin,
    nonce: &[u8; 32],
    action: &T,
) -> Result<zeroize::Zeroizing<Vec<u8>>, DirectError> {
    serde_json::to_vec(&("spaceterm-direct-1", purpose, pin, nonce, action))
        .map(zeroize::Zeroizing::new)
        .map_err(|_| DirectError::InvalidRequest)
}
/// The token digest remains a secret invitation capability. A client retains only
/// this one-way, key-bound claim ID for cancellation, never the invitation itself.
pub(super) fn claim_id(
    pin: &ServerPin,
    invitation: ClaimId,
    token_digest: [u8; 32],
    key: [u8; 32],
) -> Result<ClaimId, DirectError> {
    Ok(crypto::hash(&transcript(
        "prepared-claim",
        pin,
        &invitation,
        &(token_digest, key),
    )?))
}
pub(super) fn now() -> Result<u64, DirectError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| DirectError::StorageUnavailable)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Management {
    Status,
    SetPolicy(Option<NetworkPolicy>),
    Invite,
    Withdraw(ClaimId),
    Claims,
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
