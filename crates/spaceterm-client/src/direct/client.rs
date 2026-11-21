use super::*;
use crate::connection::quic::QuicStream;

pub fn invitation(payload: wire::Invite) -> Result<Invitation, DirectError> {
    Ok(Invitation {
        text: payload.encode()?,
        payload,
    })
}
pub fn parse_invitation(text: &str) -> Result<Invitation, DirectError> {
    let payload = wire::Invite::decode(text)?;
    if payload.schema != 1
        || payload.token == [0; 32]
        || payload.id == [0; 32]
        || payload.pin.core == [0; 32]
        || payload.pin.public_key == [0; 32]
        || payload.pin.certificate == [0; 32]
        || !valid_build(&payload.pin.build)
        || payload.machine_name.len() > 192
        || ClientName::from(payload.machine_name.as_str()).as_str() != payload.machine_name
    {
        return Err(DirectError::InvalidInvitation);
    }
    Endpoint::from_url(&payload.endpoint)?;
    if payload.expires == 0
        || payload.expires > MAX_CONNECTION_TIME
        || payload.valid_for_seconds == 0
        || payload.valid_for_seconds > wire::INVITATION_SECONDS
    {
        return Err(DirectError::InvalidInvitation);
    }
    invitation(payload)
}
pub fn valid_build(build: &str) -> bool {
    !build.is_empty()
        && build.len() <= 128
        && build
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn exchange_checked(
    endpoint: &str,
    pin: &MachinePin,
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
pub trait Messages {
    fn send(&mut self, value: &impl Serialize) -> Result<(), DirectError>;
    fn receive<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, DirectError>;
}
/// Verifies the server's challenge on `stream`, proves the device and returns the server's answer.
pub fn exchange_on(
    stream: &mut impl Messages,
    pin: &MachinePin,
    credential: &DeviceCredential,
    action: wire::Action,
    expected_name: Option<&str>,
    exchanges: Option<&mut Vec<[u8; 32]>>,
) -> Result<wire::Answer, DirectError> {
    let challenge: wire::Challenge = stream.receive()?;
    if challenge.pin != *pin {
        return Err(DirectError::WrongMachine);
    }
    if ClientName::from(challenge.machine_name.as_str()).as_str() != challenge.machine_name {
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
                &challenge.machine_name,
                &challenge.generation,
            ),
        )?,
        &challenge.proof,
    )?;
    if expected_name.is_some_and(|name| name != challenge.machine_name) {
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
pub fn connect(
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
        path: None,
    })
}
pub fn prepare(
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
pub fn submit(enrollment: &mut Enrollment) -> Result<ComparisonCode, DirectError> {
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
        Some(&invitation.machine_name),
        Some(&mut enrollment.exchanges),
        enrollment.deadline,
    )?;
    match answer {
        wire::Answer::Claim(claim)
            if claim.id == enrollment.claim
                && claim.invitation_id == invitation.id
                && claim.core == enrollment.pin.core
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
pub fn finish(enrollment: &Enrollment) -> Result<Option<PairedDevice>, DirectError> {
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
pub fn revoke(
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
            if receipt.core == actor.pin.core
                && receipt.operation == operation
                && receipt.target == target =>
        {
            Ok(receipt)
        }
        _ => Err(DirectError::AuthenticationRefused),
    }
}
pub fn cancel(enrollment: &Enrollment) -> Result<Option<RevocationReceipt>, DirectError> {
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
    pub fn paired(&self) -> &PairedDevice {
        &self.paired
    }
    /// Admits this client on the authenticated connection within the admission budget and
    /// returns its carrier.
    #[cfg(any(test, feature = "core-transport"))]
    pub fn admit<'a>(
        self,
        client: impl Into<crate::admission::Caller<'a>>,
    ) -> Result<
        (
            Box<dyn crate::connection::carrier::Carrier>,
            crate::protocol::Negotiated,
        ),
        crate::admission::Refused,
    > {
        self.admit_by(
            client,
            std::time::Instant::now() + crate::admission::CONNECTION_LIMIT,
        )
    }
    /// Admits this client on the authenticated connection and returns its carrier. No admission
    /// frame is sent or waited for after `deadline`.
    pub fn admit_by<'a>(
        self,
        client: impl Into<crate::admission::Caller<'a>>,
        deadline: std::time::Instant,
    ) -> Result<
        (
            Box<dyn crate::connection::carrier::Carrier>,
            crate::protocol::Negotiated,
        ),
        crate::admission::Refused,
    > {
        let mut stream = *self.stream;
        let negotiated = crate::admission::admit_transport_by(
            &mut stream,
            client,
            &self.paired.pin.core,
            &self.paired.pin.build,
            deadline,
        )?;
        Ok((
            Box::new(stream.into_carrier_with_path(self.path.as_ref().and_then(|open| open()))),
            negotiated,
        ))
    }
    /// Admits this client to the service lane on the authenticated connection, which needs a
    /// shared service major whatever the work majors, and returns the stream the lane's JSON
    /// frames travel on.
    #[cfg(any(test, feature = "core-transport"))]
    pub fn admit_service_lane(
        self,
        caller: crate::admission::Caller<'_>,
    ) -> Result<
        (
            Box<crate::connection::quic::QuicStream>,
            crate::protocol::Negotiated,
        ),
        crate::admission::Refused,
    > {
        self.admit_service_lane_by(
            caller,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        )
    }
    /// [`Self::admit_service_lane`] that sends no admission frame once `deadline` has passed.
    pub fn admit_service_lane_by(
        self,
        caller: crate::admission::Caller<'_>,
        deadline: std::time::Instant,
    ) -> Result<
        (
            Box<crate::connection::quic::QuicStream>,
            crate::protocol::Negotiated,
        ),
        crate::admission::Refused,
    > {
        let mut stream = self.stream;
        let negotiated = crate::admission::admit_service_lane(
            stream.as_mut(),
            caller,
            &self.paired.pin.core,
            &self.paired.pin.build,
            deadline,
        )?;
        Ok((stream, negotiated))
    }
}
