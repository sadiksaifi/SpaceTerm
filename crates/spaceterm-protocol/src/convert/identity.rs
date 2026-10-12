//! Exact qualified identity conversion. Values identify records, never authority.

use crate::{ProtocolError, wire::common};
use spaceterm_core::{authority::*, identity::*, operations::*};

pub fn encode_global(bytes: [u8; 16]) -> common::Identity {
    common::Identity {
        value: bytes.to_vec(),
    }
}

pub fn decode_global(value: &common::Identity) -> Result<[u8; 16], ProtocolError> {
    let bytes = value
        .value
        .as_slice()
        .try_into()
        .map_err(|_| ProtocolError::Malformed)?;
    if bytes == [0; 16] {
        return Err(ProtocolError::Malformed);
    }
    Ok(bytes)
}

pub fn encode_local(local: u64) -> common::Identity {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&local.to_be_bytes());
    encode_global(bytes)
}

pub fn decode_local(value: &common::Identity) -> Result<u64, ProtocolError> {
    let bytes = decode_global(value)?;
    if bytes[..8] != [0; 8] {
        return Err(ProtocolError::Malformed);
    }
    let mut low = [0; 8];
    low.copy_from_slice(&bytes[8..]);
    Ok(u64::from_be_bytes(low))
}

pub fn decode_server(value: &common::Identity) -> Result<ServerId, ProtocolError> {
    decode_global(value).map(ServerId::new)
}

macro_rules! entity {
    ($ty:ident, $encode:ident, $decode:ident) => {
        pub fn $encode(value: $ty) -> common::Entity {
            common::Entity {
                server: Some(encode_global(value.server().bytes())),
                local_id: value.local(),
            }
        }
        pub fn $decode(value: &common::Entity) -> Result<$ty, ProtocolError> {
            $ty::new(decode_server(required(&value.server)?)?, value.local_id)
                .map_err(|_| ProtocolError::Malformed)
        }
    };
}
entity!(WorkspaceId, encode_workspace, decode_workspace);
entity!(TabId, encode_tab, decode_tab);
entity!(PaneId, encode_pane, decode_pane);
entity!(WorktreeId, encode_worktree, decode_worktree);
entity!(RepositoryId, encode_repository, decode_repository);
entity!(PairingId, encode_pairing, decode_pairing);
entity!(ClientId, encode_client, decode_client);
entity!(OperationStreamKey, encode_stream_key, decode_stream_key);

pub(crate) fn required<T>(value: &Option<T>) -> Result<&T, ProtocolError> {
    value.as_ref().ok_or(ProtocolError::Malformed)
}

pub fn encode_stream(value: OperationStreamId) -> common::OperationStream {
    common::OperationStream {
        key: Some(encode_stream_key(value.key())),
        pairing: Some(encode_pairing(value.pairing())),
        client: Some(encode_client(value.client())),
    }
}

pub fn decode_stream(value: &common::OperationStream) -> Result<OperationStreamId, ProtocolError> {
    OperationStreamId::new(
        decode_stream_key(required(&value.key)?)?,
        decode_pairing(required(&value.pairing)?)?,
        decode_client(required(&value.client)?)?,
    )
    .map_err(|_| ProtocolError::Malformed)
}

pub fn encode_operation(value: OperationId) -> common::OperationId {
    let stream = value.stream();
    common::OperationId {
        server: Some(encode_global(stream.server().bytes())),
        pairing: Some(encode_local(stream.pairing().local())),
        client: Some(encode_local(stream.client().local())),
        stream: Some(encode_local(stream.key().local())),
        sequence: value.sequence().get(),
    }
}

pub fn decode_operation(value: &common::OperationId) -> Result<OperationId, ProtocolError> {
    let server = decode_server(required(&value.server)?)?;
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server, decode_local(required(&value.stream)?)?)
            .map_err(|_| ProtocolError::Malformed)?,
        PairingId::new(server, decode_local(required(&value.pairing)?)?)
            .map_err(|_| ProtocolError::Malformed)?,
        ClientId::new(server, decode_local(required(&value.client)?)?)
            .map_err(|_| ProtocolError::Malformed)?,
    );
    // Keep errors classified. No decoded identity is included in the failure.
    let stream = match stream {
        Ok(value) => value,
        Err(_) => return Err(ProtocolError::Malformed),
    };
    Ok(OperationId::new(
        stream,
        OperationSequence::new(value.sequence).map_err(|_| ProtocolError::Malformed)?,
    ))
}

pub fn encode_session(value: SessionKey) -> common::Session {
    common::Session {
        pane: Some(encode_pane(value.pane)),
        server_epoch: value.epoch.get(),
        generation: value.generation.get(),
    }
}

pub fn decode_session(value: &common::Session) -> Result<SessionKey, ProtocolError> {
    if value.server_epoch == 0 {
        return Err(ProtocolError::Malformed);
    }
    Ok(SessionKey {
        pane: decode_pane(required(&value.pane)?)?,
        epoch: ServerEpoch::new(value.server_epoch),
        generation: SessionGeneration::new(value.generation),
    })
}

/// The legacy fence has no content field. Content queries carry that comparison
/// separately in QueryStamp; callers must pass it explicitly when decoding.
pub fn encode_fence(value: EffectFence) -> common::EffectFence {
    common::EffectFence {
        session: Some(encode_session(value.session())),
        client: Some(encode_local(value.grant().client().local())),
        control_generation: value.grant().generation().get(),
        connection_generation: value.connection().generation.get(),
        context_generation: value.context().get(),
    }
}

pub fn decode_fence(
    value: &common::EffectFence,
    content: Option<ContentRevision>,
) -> Result<EffectFence, ProtocolError> {
    let session = decode_session(required(&value.session)?)?;
    if value.control_generation == 0
        || value.connection_generation == 0
        || value.context_generation == 0
    {
        return Err(ProtocolError::Malformed);
    }
    let client = ClientId::new(
        session.pane.server(),
        decode_local(required(&value.client)?)?,
    )
    .map_err(|_| ProtocolError::Malformed)?;
    let grant = ControlGrant::new(
        session.pane,
        client,
        ControlGeneration::new(value.control_generation),
    )
    .map_err(|_| ProtocolError::Malformed)?;
    EffectFence::new(
        session,
        grant,
        ConnectionBinding::new(
            client,
            ConnectionGeneration::new(value.connection_generation),
        ),
        ContextGeneration::new(value.context_generation),
        content,
    )
    .map_err(|_| ProtocolError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_handles_reject_alternative_representations_and_zero() {
        for n in [1, u64::MAX] {
            let wire = encode_local(n);
            assert_eq!(&wire.value[..8], &[0; 8]);
            assert_eq!(decode_local(&wire), Ok(n));
            let mut substituted = wire;
            substituted.value[0] = 1;
            assert_eq!(decode_local(&substituted), Err(ProtocolError::Malformed));
        }
        assert_eq!(
            decode_local(&encode_global([0; 16])),
            Err(ProtocolError::Malformed)
        );
    }

    #[test]
    fn qualified_stream_rejects_foreign_server_and_preserves_original_operation() {
        let server = ServerId::new([1; 16]);
        let stream = OperationStreamId::new(
            OperationStreamKey::new(server, 4).unwrap(),
            PairingId::new(server, 2).unwrap(),
            ClientId::new(server, 3).unwrap(),
        )
        .unwrap();
        let operation = OperationId::new(stream, OperationSequence::new(9).unwrap());
        assert_eq!(
            decode_operation(&encode_operation(operation)),
            Ok(operation)
        );
        let mut wire = encode_stream(stream);
        wire.client.as_mut().unwrap().server = Some(encode_global([2; 16]));
        assert_eq!(decode_stream(&wire), Err(ProtocolError::Malformed));
    }
}
