use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

pub(super) fn invitation(payload: wire::Invite) -> Result<Invitation, DirectError> {
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(&payload).map_err(|_| DirectError::InvalidInvitation)?,
    );
    let text = zeroize::Zeroizing::new(format!(
        "spaceterm://pair/{}",
        URL_SAFE_NO_PAD.encode(&bytes)
    ));
    Ok(Invitation { payload, text })
}
pub(super) fn parse_invitation(text: &str) -> Result<Invitation, DirectError> {
    if text.len() > 8192 {
        return Err(DirectError::InvalidInvitation);
    }
    let encoded = text
        .strip_prefix("spaceterm://pair/")
        .ok_or(DirectError::InvalidInvitation)?;
    let bytes = zeroize::Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| DirectError::InvalidInvitation)?,
    );
    let payload: wire::Invite =
        serde_json::from_slice(&bytes).map_err(|_| DirectError::InvalidInvitation)?;
    if payload.schema != 1
        || payload.token == [0; 32]
        || payload.id == [0; 32]
        || payload.pin.server == [0; 32]
        || payload.pin.public_key == [0; 32]
        || payload.pin.certificate == [0; 32]
        || !valid_build(&payload.pin.build)
        || payload.server_name.len() > 192
        || ClientName::from(payload.server_name.as_str()).as_str() != payload.server_name
    {
        return Err(DirectError::InvalidInvitation);
    }
    wss::validate_endpoint(&payload.endpoint)?;
    if payload.expires == 0
        || payload.expires > storage::MAX_CONNECTION_TIME
        || payload.valid_for_seconds == 0
        || payload.valid_for_seconds > wire::INVITATION_SECONDS
    {
        return Err(DirectError::InvalidInvitation);
    }
    invitation(payload)
}
pub(super) fn valid_build(build: &str) -> bool {
    !build.is_empty()
        && build.len() <= 128
        && build
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn exchange(
    endpoint: &str,
    pin: &ServerPin,
    credential: &DeviceCredential,
    action: wire::Action,
) -> Result<(wss::Ws, wire::Answer), DirectError> {
    exchange_checked(endpoint, pin, credential, action, None, None)
}
fn exchange_checked(
    endpoint: &str,
    pin: &ServerPin,
    credential: &DeviceCredential,
    action: wire::Action,
    expected_name: Option<&str>,
    exchanges: Option<&mut Vec<[u8; 32]>>,
) -> Result<(wss::Ws, wire::Answer), DirectError> {
    let mut ws = wss::connect(endpoint, pin)?;
    let challenge: wire::Challenge = wss::receive(&mut ws)?;
    if challenge.pin != *pin {
        return Err(DirectError::WrongServer);
    }
    if challenge.major != crate::protocol::MAJOR {
        return Err(DirectError::IncompatibleProtocol);
    }
    if ClientName::from(challenge.server_name.as_str()).as_str() != challenge.server_name {
        return Err(DirectError::AuthenticationRefused);
    }
    crypto::verify(
        &pin.public_key,
        &wire::transcript(
            "server",
            pin,
            &challenge.nonce,
            &(
                challenge.major,
                &challenge.server_name,
                &challenge.generation,
            ),
        )?,
        &challenge.proof,
    )?;
    if expected_name.is_some_and(|name| name != challenge.server_name) {
        return Err(DirectError::Substituted);
    }
    let proof = crypto::sign(
        credential.expose_bytes(),
        &wire::transcript("device", pin, &challenge.nonce, &action)?,
    )?;
    let authentication = wire::Authentication { action, proof };
    if let Some(exchanges) = exchanges {
        if exchanges.len() >= 64 {
            return Err(DirectError::Capacity);
        }
        // Keep possible admitted originals even when sending or receiving fails.
        // This live-only bounded set contains no invitation or device proof.
        exchanges.push(wire::exchange_hash(&challenge, &authentication)?);
    }
    wss::send(&mut ws, &authentication)?;
    let answer: Result<wire::Answer, DirectError> = wss::receive(&mut ws)?;
    Ok((ws, answer?))
}
pub(super) fn connect(
    endpoint: &str,
    paired: &PairedDevice,
    credential: &DeviceCredential,
) -> Result<DirectConnection, DirectError> {
    let (ws, answer) = exchange(
        endpoint,
        &paired.pin,
        credential,
        wire::Action::Connect {
            paired: paired.clone(),
        },
    )?;
    if !matches!(answer, wire::Answer::Connected) {
        return Err(DirectError::AuthenticationRefused);
    }
    Ok(DirectConnection {
        bridge: wss::Bridge::start(ws)?,
        paired: paired.clone(),
    })
}
pub(super) fn prepare(
    invitation: &Invitation,
    name: ClientName,
    credential: &DeviceCredential,
) -> Result<Enrollment, DirectError> {
    let claim = wire::claim_id(
        &invitation.payload.pin,
        invitation.payload.id,
        crypto::hash(&invitation.payload.token),
        crypto::public(credential.expose_bytes())?,
    )?;
    Ok(Enrollment {
        endpoint: invitation.payload.endpoint.clone(),
        pin: invitation.payload.pin.clone(),
        claim,
        credential: DeviceCredential::from_bytes(credential.expose_bytes())?,
        prepared: Some((invitation.payload.clone(), name)),
        admitted: None,
        exchanges: Vec::new(),
    })
}
pub(super) fn submit(enrollment: &mut Enrollment) -> Result<ComparisonCode, DirectError> {
    let (invitation, name) = enrollment
        .prepared
        .as_ref()
        .ok_or(DirectError::InvalidRequest)?;
    let (_, answer) = exchange_checked(
        &enrollment.endpoint,
        &enrollment.pin,
        &enrollment.credential,
        wire::Action::Claim {
            id: enrollment.claim,
            token: invitation.token,
            key: crypto::public(enrollment.credential.expose_bytes())?,
            name: name.clone(),
        },
        Some(&invitation.server_name),
        Some(&mut enrollment.exchanges),
    )?;
    match answer {
        wire::Answer::Claim(claim)
            if claim.id == enrollment.claim
                && claim.invitation_id == invitation.id
                && claim.server == enrollment.pin.server
                && claim.device_public_key
                    == crypto::public(enrollment.credential.expose_bytes())?
                && claim.device_name == *name
                && claim.expires_at_unix_seconds == invitation.expires =>
        {
            // Both devices independently hash the original verified exchange. An
            // exact retry may resolve an earlier submission whose response was lost.
            if !enrollment.exchanges.contains(&claim.exchange_hash)
                || enrollment
                    .admitted
                    .as_ref()
                    .is_some_and(|previous| previous != &claim)
            {
                return Err(DirectError::Substituted);
            }
            let code = claim.comparison_code();
            enrollment.admitted = Some(claim);
            Ok(code)
        }
        _ => Err(DirectError::Substituted),
    }
}
pub(super) fn finish(enrollment: &Enrollment) -> Result<Option<PairedDevice>, DirectError> {
    let (_, answer) = exchange(
        &enrollment.endpoint,
        &enrollment.pin,
        &enrollment.credential,
        wire::Action::Finish {
            claim: enrollment.claim,
            key: crypto::public(enrollment.credential.expose_bytes())?,
        },
    )?;
    match answer {
        wire::Answer::Enrollment(paired) => Ok(paired),
        _ => Err(DirectError::InvalidRequest),
    }
}
pub(super) fn revoke(
    endpoint: &str,
    actor: &PairedDevice,
    credential: &DeviceCredential,
    operation: OperationId,
    target: PairingId,
) -> Result<RevocationReceipt, DirectError> {
    let (_, answer) = exchange(
        endpoint,
        &actor.pin,
        credential,
        wire::Action::Revoke {
            actor: actor.clone(),
            operation,
            target,
        },
    )?;
    match answer {
        wire::Answer::Revoked(receipt)
            if receipt.server == actor.pin.server
                && receipt.operation == operation
                && receipt.target == target =>
        {
            Ok(receipt)
        }
        _ => Err(DirectError::AuthenticationRefused),
    }
}
pub(super) fn cancel(enrollment: &Enrollment) -> Result<Option<RevocationReceipt>, DirectError> {
    let (_, answer) = exchange(
        &enrollment.endpoint,
        &enrollment.pin,
        &enrollment.credential,
        wire::Action::Cancel {
            claim: enrollment.claim,
            key: crypto::public(enrollment.credential.expose_bytes())?,
        },
    )?;
    match answer {
        wire::Answer::Cancelled(receipt) => Ok(receipt),
        _ => Err(DirectError::InvalidRequest),
    }
}

impl std::io::Read for DirectConnection {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.bridge, b)
    }
}
impl std::io::Write for DirectConnection {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut self.bridge, b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.bridge)
    }
}
impl crate::transport::Transport for DirectConnection {
    fn read_fd(&self) -> std::os::fd::RawFd {
        crate::transport::Transport::read_fd(&self.bridge)
    }
    fn write_fd(&self) -> std::os::fd::RawFd {
        crate::transport::Transport::write_fd(&self.bridge)
    }
}
impl DirectConnection {
    #[cfg(feature = "client")]
    pub(crate) fn paired(&self) -> &PairedDevice {
        &self.paired
    }
}
