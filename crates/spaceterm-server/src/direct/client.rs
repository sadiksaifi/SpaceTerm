use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

pub(super) fn invitation(payload: wire::Invite) -> Result<Invitation, DirectError> {
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(&payload).map_err(|_| DirectError::InvalidInvitation)?,
    );
    let text =
        zeroize::Zeroizing::new(format!("spaceterm-pair:{}", URL_SAFE_NO_PAD.encode(&bytes)));
    Ok(Invitation { payload, text })
}
pub(super) fn parse_invitation(text: &str) -> Result<Invitation, DirectError> {
    if text.len() > 8192 {
        return Err(DirectError::InvalidInvitation);
    }
    let encoded = text
        .strip_prefix("spaceterm-pair:")
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
        || payload.server_name.len() > 192
        || ClientName::from(payload.server_name.as_str()).as_str() != payload.server_name
    {
        return Err(DirectError::InvalidInvitation);
    }
    wss::validate_endpoint(&payload.endpoint)?;
    if payload.expires <= wire::now()? {
        return Err(DirectError::Expired);
    }
    invitation(payload)
}
fn exchange(
    endpoint: &str,
    pin: &ServerPin,
    credential: &DeviceCredential,
    action: wire::Action,
) -> Result<(wss::Ws, wire::Answer), DirectError> {
    exchange_checked(endpoint, pin, credential, action, None)
}
fn exchange_checked(
    endpoint: &str,
    pin: &ServerPin,
    credential: &DeviceCredential,
    action: wire::Action,
    expected_name: Option<&str>,
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
    wss::send(&mut ws, &wire::Authentication { action, proof })?;
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
    if invitation.payload.expires <= wire::now()? {
        return Err(DirectError::Expired);
    }
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
    })
}
pub(super) fn submit(enrollment: &Enrollment) -> Result<(), DirectError> {
    let (invitation, name) = enrollment
        .prepared
        .as_ref()
        .ok_or(DirectError::InvalidRequest)?;
    if invitation.expires <= wire::now()? {
        return Err(DirectError::Expired);
    }
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
    )?;
    match answer {
        wire::Answer::Claim(id) if id == enrollment.claim => Ok(()),
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
