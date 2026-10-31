use super::*;
use crate::connection::quic::QuicStream;
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
    Endpoint::from_url(&payload.endpoint)?;
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
fn exchange_checked(
    endpoint: &str,
    pin: &ServerPin,
    credential: &DeviceCredential,
    action: wire::Action,
    expected_name: Option<&str>,
    exchanges: Option<&mut Vec<[u8; 32]>>,
    deadline: Option<std::time::Instant>,
) -> Result<(QuicStream, wire::Answer), DirectError> {
    let deadline = Some(
        deadline
            .unwrap_or_else(|| std::time::Instant::now() + quic::HANDSHAKE_LIMIT)
            .min(std::time::Instant::now() + quic::HANDSHAKE_LIMIT),
    );
    let mut stream = quic::dial_until(endpoint, pin, credential, deadline)?;
    let answer = exchange_on(
        &mut quic::DeadlineMessages::new(&mut stream, deadline),
        pin,
        credential,
        action,
        expected_name,
        exchanges,
    )?;
    Ok((stream, answer))
}
/// The pairing messages of one authenticated connection, as length-bounded JSON.
pub(super) trait Messages {
    fn send(&mut self, value: &impl Serialize) -> Result<(), DirectError>;
    fn receive<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, DirectError>;
}
/// Verifies the server's challenge on `stream`, proves the device and returns the server's answer.
pub(super) fn exchange_on(
    stream: &mut impl Messages,
    pin: &ServerPin,
    credential: &DeviceCredential,
    action: wire::Action,
    expected_name: Option<&str>,
    exchanges: Option<&mut Vec<[u8; 32]>>,
) -> Result<wire::Answer, DirectError> {
    let challenge: wire::Challenge = stream.receive()?;
    if challenge.pin != *pin {
        return Err(DirectError::WrongServer);
    }
    // Major 4 froze authentication for the service lane. Work admission checks its own major.
    if challenge.major < 4 {
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
    stream.send(&authentication)?;
    let answer: Result<wire::Answer, DirectError> = stream.receive()?;
    answer
}
pub(super) fn connect(
    endpoint: &str,
    paired: &PairedDevice,
    credential: &DeviceCredential,
    deadline: Option<std::time::Instant>,
) -> Result<DirectConnection, DirectError> {
    let (stream, answer) = exchange_checked(
        endpoint,
        &paired.pin,
        credential,
        wire::Action::Connect {
            paired: paired.clone(),
        },
        None,
        None,
        deadline,
    )?;
    if !matches!(answer, wire::Answer::Connected) {
        return Err(DirectError::AuthenticationRefused);
    }
    Ok(DirectConnection {
        stream: Box::new(stream),
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
        deadline: None,
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
        enrollment.deadline,
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
    let (_, answer) = exchange_checked(
        &enrollment.endpoint,
        &enrollment.pin,
        &enrollment.credential,
        wire::Action::Finish {
            claim: enrollment.claim,
            key: crypto::public(enrollment.credential.expose_bytes())?,
        },
        None,
        None,
        enrollment.deadline,
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
    deadline: Option<std::time::Instant>,
) -> Result<RevocationReceipt, DirectError> {
    let (_, answer) = exchange_checked(
        endpoint,
        &actor.pin,
        credential,
        wire::Action::Revoke {
            actor: actor.clone(),
            operation,
            target,
        },
        None,
        None,
        deadline,
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
    let (_, answer) = exchange_checked(
        &enrollment.endpoint,
        &enrollment.pin,
        &enrollment.credential,
        wire::Action::Cancel {
            claim: enrollment.claim,
            key: crypto::public(enrollment.credential.expose_bytes())?,
        },
        None,
        None,
        enrollment.deadline,
    )?;
    match answer {
        wire::Answer::Cancelled(receipt) => Ok(receipt),
        _ => Err(DirectError::InvalidRequest),
    }
}

impl DirectConnection {
    #[cfg(feature = "client")]
    pub(crate) fn paired(&self) -> &PairedDevice {
        &self.paired
    }
    /// Admits this client on the authenticated connection and returns its carrier.
    #[cfg_attr(
        not(any(feature = "client", test)),
        allow(dead_code, reason = "the desktop admits and carries the connection")
    )]
    pub(crate) fn admit<'a>(
        self,
        client: impl Into<crate::protocol::Caller<'a>>,
    ) -> Result<Box<dyn crate::connection::carrier::Carrier>, ServerError> {
        let mut stream = *self.stream;
        crate::protocol::admit_transport(
            &mut stream,
            client,
            &self.paired.pin.server,
            &self.paired.pin.build,
        )?;
        Ok(Box::new(stream.into_carrier()))
    }
    /// Admits this client to the service lane on the authenticated connection, whatever the
    /// server's work major, and returns the stream the lane's JSON frames travel on.
    #[cfg_attr(
        not(any(feature = "client", test)),
        allow(dead_code, reason = "the CLI administers paired servers over the lane")
    )]
    pub(crate) fn admit_service_lane(
        self,
        caller: crate::protocol::Caller<'_>,
    ) -> Result<Box<crate::connection::quic::QuicStream>, ServerError> {
        let mut stream = self.stream;
        crate::protocol::admit_service_lane(
            stream.as_mut(),
            caller,
            &self.paired.pin.server,
            &self.paired.pin.build,
        )?;
        Ok(stream)
    }
}
