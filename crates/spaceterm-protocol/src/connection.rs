//! Authenticated, sequenced envelopes shared by IPC and WSS.

use crate::{
    ProtocolError,
    authentication::{Principal, Purpose, SigningKey, decode_signed, get_identity, sign, verify},
    codec::Payload,
    limits::NegotiatedLimits,
    preflight,
    wire::common,
};
use prost::Message;
use std::sync::Arc;

pub(crate) struct ConnectionParameters {
    pub principal: Principal,
    pub purpose: Purpose,
    pub major: u32,
    pub limits: NegotiatedLimits,
    pub capabilities: Vec<u32>,
    pub epoch: u64,
    pub binding: [u8; 32],
    pub direction: common::Direction,
    pub signer: Arc<dyn SigningKey>,
    pub peer_key: [u8; 32],
}

pub(crate) struct ConnectionCodec {
    parameters: ConnectionParameters,
    send_sequence: u64,
    receive_sequence: u64,
    failed: bool,
}
impl ConnectionCodec {
    pub(crate) fn new(parameters: ConnectionParameters) -> Self {
        Self {
            parameters,
            send_sequence: 1,
            receive_sequence: 1,
            failed: false,
        }
    }
    fn encode(&mut self, payload: Vec<u8>) -> Result<Vec<u8>, ProtocolError> {
        if self.failed {
            return Err(ProtocolError::Authentication);
        }
        let next = self
            .send_sequence
            .checked_add(1)
            .ok_or(ProtocolError::Exhausted)?;
        let body = common::AuthenticatedEnvelope {
            direction: self.parameters.direction as i32,
            major: self.parameters.major,
            binding: self.parameters.binding.to_vec(),
            sequence: self.send_sequence,
            payload,
            purpose: self.parameters.purpose.wire(),
        };
        // Include the signature, both nested byte lengths and all envelope metadata
        // in frame admission before either serialized envelope allocation.
        let body_len = body.encoded_len();
        let overhead = 1 + varint_len(body_len) + body_len + 1 + 1 + 64;
        if overhead > self.parameters.limits.frame_bytes {
            return Err(ProtocolError::LimitExceeded);
        }
        let bytes = body.encode_to_vec();
        let signature = sign(
            self.parameters.signer.as_ref(),
            b"authenticated-envelope",
            &[&bytes],
        )?;
        let encoded = common::Signed {
            body: bytes,
            signature: signature.to_vec(),
        }
        .encode_to_vec();
        self.send_sequence = next;
        Ok(encoded)
    }

    fn decode(&mut self, bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        if self.failed {
            return Err(ProtocolError::Authentication);
        }
        let result = self.decode_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn decode_inner(&mut self, bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        let signed = decode_signed(bytes, self.parameters.limits.frame_bytes)?;
        verify(
            self.parameters.peer_key,
            b"authenticated-envelope",
            &[&signed.body],
            &signed.signature,
        )?;
        preflight::check(
            preflight::SPACETERM_COMMON_AUTHENTICATEDENVELOPE,
            &signed.body,
            self.parameters.limits,
        )?;
        let envelope = common::AuthenticatedEnvelope::decode(signed.body.as_slice())
            .map_err(|_| ProtocolError::Malformed)?;
        if envelope.major != self.parameters.major
            || envelope.binding != self.parameters.binding
            || envelope.direction != opposite(self.parameters.direction) as i32
            || envelope.purpose != self.parameters.purpose.wire()
        {
            return Err(ProtocolError::Authentication);
        }
        if envelope.sequence != self.receive_sequence {
            return Err(ProtocolError::Sequence);
        }
        self.receive_sequence = self
            .receive_sequence
            .checked_add(1)
            .ok_or(ProtocolError::Exhausted)?;
        Ok(envelope.payload)
    }
}

/// An ordinary connection can only be obtained from active-pairing readiness.
///
/// It cannot be constructed from identities, decoded DTOs, or a restricted retry.
///
/// ```compile_fail
/// use spaceterm_protocol::connection::AuthenticatedConnection;
/// let connection = AuthenticatedConnection::new();
/// ```
pub struct AuthenticatedConnection {
    codec: ConnectionCodec,
}
impl AuthenticatedConnection {
    pub(crate) fn new(codec: ConnectionCodec) -> Self {
        Self { codec }
    }
    pub fn principal(&self) -> Principal {
        self.codec.parameters.principal
    }
    pub fn major(&self) -> u32 {
        self.codec.parameters.major
    }
    pub fn limits(&self) -> NegotiatedLimits {
        self.codec.parameters.limits
    }
    pub fn capabilities(&self) -> &[u32] {
        &self.codec.parameters.capabilities
    }
    pub fn epoch(&self) -> u64 {
        self.codec.parameters.epoch
    }
    /// Identifies this exact handshake, including its independently fresh server nonce.
    pub fn binding(&self) -> [u8; 32] {
        self.codec.parameters.binding
    }

    /// Returns one WSS binary message. IPC adds the framing module's length prefix.
    pub fn encode(&mut self, payload: &Payload) -> Result<Vec<u8>, ProtocolError> {
        if payload.major() != self.major() {
            return Err(ProtocolError::Negotiation);
        }
        let bytes = payload.encode_with_limits(self.limits())?;
        ordinary_field_capabilities(payload, self.capabilities())?;
        ordinary_direction(
            preflight::payload_tag(&bytes)?,
            self.codec.parameters.direction,
            self.capabilities(),
        )?;
        self.codec.encode(bytes)
    }

    /// Verifies exact signed bytes, binding, direction, major and strict next sequence
    /// before admitting and decoding the major-specific payload. Failure is terminal.
    pub fn decode(&mut self, bytes: &[u8]) -> Result<Payload, ProtocolError> {
        let result = (|| {
            let payload = self.codec.decode(bytes)?;
            ordinary_direction(
                preflight::payload_tag(&payload)?,
                opposite(self.codec.parameters.direction),
                self.capabilities(),
            )?;
            let payload = Payload::decode_with_limits(self.major(), &payload, self.limits())?;
            ordinary_field_capabilities(&payload, self.capabilities())?;
            Ok(payload)
        })();
        if result.is_err() {
            self.codec.failed = true;
        }
        result
    }
}

/// Self-revocation/status only. There is deliberately no upgrade or ordinary codec.
/// Retaining this capability cannot recreate terminal access after local removal.
///
/// ```compile_fail
/// use spaceterm_protocol::connection::{AuthenticatedConnection, RestrictedConnection};
/// fn upgrade(connection: RestrictedConnection) -> AuthenticatedConnection {
///     connection.into_ordinary()
/// }
/// ```
pub struct RestrictedConnection {
    codec: ConnectionCodec,
}
impl RestrictedConnection {
    pub(crate) fn new(codec: ConnectionCodec) -> Self {
        Self { codec }
    }
    pub fn principal(&self) -> Principal {
        self.codec.parameters.principal
    }
    pub fn major(&self) -> u32 {
        self.codec.parameters.major
    }

    pub fn limits(&self) -> NegotiatedLimits {
        self.codec.parameters.limits
    }

    pub fn encode(
        &mut self,
        payload: &common::RestrictedMessage,
    ) -> Result<Vec<u8>, ProtocolError> {
        if payload.encoded_len() > self.codec.parameters.limits.management_bytes {
            return Err(ProtocolError::LimitExceeded);
        }
        let bytes = payload.encode_to_vec();
        preflight::check(
            preflight::SPACETERM_COMMON_RESTRICTEDMESSAGE,
            &bytes,
            self.codec.parameters.limits,
        )?;
        self.validate(payload, self.codec.parameters.direction)?;
        self.codec.encode(bytes)
    }

    pub fn decode(&mut self, bytes: &[u8]) -> Result<common::RestrictedMessage, ProtocolError> {
        let result = (|| {
            let payload = self.codec.decode(bytes)?;
            if payload.len() > self.codec.parameters.limits.management_bytes {
                return Err(ProtocolError::LimitExceeded);
            }
            preflight::check(
                preflight::SPACETERM_COMMON_RESTRICTEDMESSAGE,
                &payload,
                self.codec.parameters.limits,
            )?;
            let message = common::RestrictedMessage::decode(payload.as_slice())
                .map_err(|_| ProtocolError::Malformed)?;
            self.validate(&message, opposite(self.codec.parameters.direction))?;
            Ok(message)
        })();
        if result.is_err() {
            self.codec.failed = true;
        }
        result
    }

    /// Pairing extension uses the same established principal and sequence.
    pub fn encode_pairing_extension(
        &mut self,
        payload: &crate::wire::pairing::RestrictedExtension,
        codec: &dyn crate::pairing_adapter::RestrictedExtensionCodec,
    ) -> Result<Vec<u8>, ProtocolError> {
        let bytes = codec.encode_restricted_extension(
            payload,
            self.principal(),
            self.codec.parameters.direction,
            self.limits(),
        )?;
        self.codec.encode(bytes)
    }
    /// Invalid extension reception fails the established restricted connection.
    pub fn decode_pairing_extension(
        &mut self,
        bytes: &[u8],
        codec: &dyn crate::pairing_adapter::RestrictedExtensionCodec,
    ) -> Result<crate::wire::pairing::RestrictedExtension, ProtocolError> {
        let result = (|| {
            let payload = self.codec.decode(bytes)?;
            codec.decode_restricted_extension(
                &payload,
                self.principal(),
                opposite(self.codec.parameters.direction),
                self.limits(),
            )
        })();
        if result.is_err() {
            self.codec.failed = true;
        }
        result
    }

    fn validate(
        &self,
        value: &common::RestrictedMessage,
        direction: common::Direction,
    ) -> Result<(), ProtocolError> {
        use common::restricted_message::Kind;
        let (operation, target) = match (&value.kind, direction) {
            (Some(Kind::Revoke(request)), common::Direction::ClientToServer) => {
                (&request.operation, &request.target_pairing)
            }
            (Some(Kind::Status(request)), common::Direction::ClientToServer) => {
                (&request.operation, &request.target_pairing)
            }
            (Some(Kind::Receipt(receipt)), common::Direction::ServerToClient) => {
                (&receipt.operation, &receipt.target_pairing)
            }
            (Some(Kind::Failure(_)), common::Direction::ServerToClient) => return Ok(()),
            _ => return Err(ProtocolError::UnexpectedMessage),
        };
        let principal = self.principal();
        let operation = operation.as_ref().ok_or(ProtocolError::Malformed)?;
        if get_identity(target.as_ref())? != principal.pairing
            || get_identity(operation.server.as_ref())? != principal.server
            || get_identity(operation.pairing.as_ref())? != principal.pairing
            || get_identity(operation.client.as_ref())? != principal.client
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(())
    }
}

fn ordinary_direction(
    tag: u32,
    direction: common::Direction,
    capabilities: &[u32],
) -> Result<(), ProtocolError> {
    if let Some((_, capability, client, server)) = crate::negotiation::EXTENSION_ENVELOPES
        .iter()
        .find(|entry| entry.0 == tag)
    {
        if !capabilities.contains(capability) {
            return Err(ProtocolError::Negotiation);
        }
        return if (direction == common::Direction::ClientToServer && *client)
            || (direction == common::Direction::ServerToClient && *server)
        {
            Ok(())
        } else {
            Err(ProtocolError::UnexpectedMessage)
        };
    }
    let client_request = matches!(
        tag,
        1 | 4 | 6 | 7 | 9 | 11 | 13 | 15 | 17 | 19 | 20 | 22 | 24 | 28 | 30
    );
    let server_response = matches!(
        tag,
        2 | 3 | 5 | 8 | 10 | 12 | 14 | 16 | 17 | 18 | 21 | 23 | 25 | 26 | 27 | 29 | 31
    );
    if (direction == common::Direction::ClientToServer && client_request)
        || (direction == common::Direction::ServerToClient && server_response)
    {
        Ok(())
    } else {
        Err(ProtocolError::UnexpectedMessage)
    }
}
fn opposite(direction: common::Direction) -> common::Direction {
    match direction {
        common::Direction::ClientToServer => common::Direction::ServerToClient,
        _ => common::Direction::ClientToServer,
    }
}
fn varint_len(mut value: usize) -> usize {
    let mut length = 1;
    while value >= 128 {
        value >>= 7;
        length += 1;
    }
    length
}

/// Nested additions require their own authenticated capability. Structural
/// admission runs first, and an outgoing rejection allocates no envelope sequence.
fn ordinary_field_capabilities(
    payload: &Payload,
    capabilities: &[u32],
) -> Result<(), ProtocolError> {
    macro_rules! organization_comparisons {
        ($message:expr, $p:ident) => {{
            use crate::wire::$p;
            match &$message.kind {
                Some($p::message::Kind::OrganizationSnapshot(value)) => {
                    value.tabs.iter().any(|tab| tab.membership_version != 0)
                        || value
                            .panes
                            .iter()
                            .any(|pane| pane.runtime_failure_class.is_some())
                }
                Some($p::message::Kind::OrganizationDelta(value)) => {
                    value.tabs.iter().any(|tab| tab.membership_version != 0)
                        || value
                            .panes
                            .iter()
                            .any(|pane| pane.runtime_failure_class.is_some())
                }
                _ => false,
            }
        }};
    }
    let comparisons = match payload {
        Payload::Major1(message) => organization_comparisons!(message, v1),
        Payload::Major2(message) => organization_comparisons!(message, v2),
    };
    if comparisons
        && !capabilities.contains(&crate::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS)
    {
        return Err(ProtocolError::Negotiation);
    }
    macro_rules! terminal_stream_fields {
        ($message:expr, $p:ident) => {{
            use crate::wire::$p;
            match &$message.kind {
                Some($p::message::Kind::CanonicalSemanticPage(page)) => page.columns != 0,
                Some($p::message::Kind::ContentReadResponse(response)) => matches!(&response.result,
                    Some($p::content_read_response::Result::Page(page)) if page.columns != 0),
                _ => false,
            }
        }};
    }
    let terminal_fields = match payload {
        Payload::Major1(message) => terminal_stream_fields!(message, v1),
        Payload::Major2(message) => terminal_stream_fields!(message, v2),
    };
    if terminal_fields && !capabilities.contains(&crate::negotiation::CAPABILITY_TERMINAL_STREAMS) {
        return Err(ProtocolError::Negotiation);
    }
    macro_rules! notification_fields {
        ($message:expr, $p:ident) => {{
            use crate::wire::$p;
            matches!(&$message.kind, Some($p::message::Kind::NativeRequest(request))
                if matches!(request.action, Some($p::native_request::Action::Notification(_))))
                || matches!(&$message.kind, Some($p::message::Kind::NativeCompletion(completion)) if completion.disposition == 8)
        }};
    }
    let notifications = match payload {
        Payload::Major1(message) => notification_fields!(message, v1),
        Payload::Major2(message) => notification_fields!(message, v2),
    };
    if notifications && !capabilities.contains(&crate::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS)
    {
        return Err(ProtocolError::Negotiation);
    }
    macro_rules! canonical {
        ($message:expr, $p:ident) => {{
            use crate::wire::$p;
            match &$message.kind {
                Some($p::message::Kind::Outcome(outcome)) => matches!(outcome.result,
                    Some($p::outcome::Result::ControlUnavailable(_)
                    | $p::outcome::Result::StartupAwaitingMeasurement(_)
                    | $p::outcome::Result::SessionStarted(_)
                    | $p::outcome::Result::LegacyCreatedWorkspace(_)
                    | $p::outcome::Result::LegacyCreatedTab(_)
                    | $p::outcome::Result::LegacyCreatedPane(_)
                    | $p::outcome::Result::LegacyCreatedWorktree(_)
                    | $p::outcome::Result::LegacyControlAcquired(_)
                    | $p::outcome::Result::LegacyAssessed(_)
                    | $p::outcome::Result::OperationFailure(_)))
                    || matches!(&outcome.result, Some($p::outcome::Result::Conflict(value))
                        if value.directory_value.is_some() || value.remote_access.is_some() || value.server.is_some()),
                Some($p::message::Kind::OrganizationDelta(delta)) => !delta.repositories.is_empty() || !delta.deleted_repositories.is_empty(),
                Some($p::message::Kind::StartupIntent(intent)) => intent.measured_grid.is_some(),
                Some($p::message::Kind::RuntimeCommand(command)) => matches!(command.action, Some($p::runtime_command::Action::MeasuredResize(_)
                    | $p::runtime_command::Action::ClearScreenAndScrollback(_)
                    | $p::runtime_command::Action::CellPixelMetrics(_))),
                Some($p::message::Kind::Operation(operation)) => matches!(&operation.action, Some($p::operation::Action::IdentifiedStartup(intent)) if intent.measured_grid.is_some()),
                _ => false,
            }
        }};
    }
    let required = match payload {
        Payload::Major1(message) => canonical!(message, v1),
        Payload::Major2(message) => canonical!(message, v2),
    };
    if required && !capabilities.contains(&crate::negotiation::CAPABILITY_CANONICAL_DOMAIN) {
        Err(ProtocolError::Negotiation)
    } else {
        Ok(())
    }
}
