//! Both-major authenticated status conversions. Native observations retain their
//! verification class. Endpoint values are domain data, never diagnostic metadata.

use super::identity::*;
use crate::ProtocolError;
use spaceterm_core::{
    authority::*, management::*, operations::*, organization::Versioned,
    remote_access::RemoteAccessPolicy,
};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    num::NonZeroU64,
};

fn nonzero(value: u64) -> Result<NonZeroU64, ProtocolError> {
    NonZeroU64::new(value).ok_or(ProtocolError::Malformed)
}

macro_rules! profile {
    ($profile:ident) => {
        pub mod $profile {
            use super::*;
            use crate::wire::$profile as wire;

            pub fn encode_endpoint(value: SocketAddr) -> wire::RemoteEndpoint {
                match value {
                    SocketAddr::V4(value) => wire::RemoteEndpoint {
                        address: value.ip().octets().to_vec(),
                        port: u32::from(value.port()),
                        flowinfo: 0,
                        scope_id: 0,
                    },
                    SocketAddr::V6(value) => wire::RemoteEndpoint {
                        address: value.ip().octets().to_vec(),
                        port: u32::from(value.port()),
                        flowinfo: value.flowinfo(),
                        scope_id: value.scope_id(),
                    },
                }
            }
            pub fn decode_endpoint(
                value: &wire::RemoteEndpoint,
            ) -> Result<SocketAddr, ProtocolError> {
                let port = u16::try_from(value.port).map_err(|_| ProtocolError::Malformed)?;
                match value.address.as_slice() {
                    [a, b, c, d] if value.flowinfo == 0 && value.scope_id == 0 => Ok(
                        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(*a, *b, *c, *d)), port),
                    ),
                    bytes if bytes.len() == 16 => {
                        let octets: [u8; 16] =
                            bytes.try_into().map_err(|_| ProtocolError::Malformed)?;
                        Ok(SocketAddr::V6(SocketAddrV6::new(
                            Ipv6Addr::from(octets),
                            port,
                            value.flowinfo,
                            value.scope_id,
                        )))
                    }
                    _ => Err(ProtocolError::Malformed),
                }
            }
            pub fn encode_policy(
                value: &Versioned<RemoteAccessPolicy>,
            ) -> wire::RemoteAccessPolicy {
                wire::RemoteAccessPolicy {
                    server: Some(encode_global(value.value.server().bytes())),
                    endpoints: value
                        .value
                        .endpoints()
                        .iter()
                        .copied()
                        .map(encode_endpoint)
                        .collect(),
                    version: value.version.get(),
                }
            }
            pub fn decode_policy(
                value: &wire::RemoteAccessPolicy,
            ) -> Result<Versioned<RemoteAccessPolicy>, ProtocolError> {
                let server = decode_server(required(&value.server)?)?;
                let endpoints = value
                    .endpoints
                    .iter()
                    .map(decode_endpoint)
                    .collect::<Result<Vec<_>, _>>()?;
                let policy = if endpoints.is_empty() {
                    RemoteAccessPolicy::disabled(server)
                } else {
                    RemoteAccessPolicy::enabled(server, endpoints)
                        .map_err(|_| ProtocolError::Malformed)?
                };
                Ok(Versioned {
                    value: policy,
                    version: FieldVersion::new(value.version),
                })
            }
            pub fn encode_query(value: StatusQuery) -> wire::StatusQuery {
                wire::StatusQuery {
                    request: value.request.get(),
                    request_generation: value.generation,
                    known_epoch: value.known_epoch.map_or(0, ServerEpoch::get),
                    known_revision: value.known_revision.map_or(0, NonZeroU64::get),
                    subscribe: value.subscribe,
                    cancel: false,
                }
            }
            /// The connection is supplied by authentication, never a wire claim.
            pub fn decode_query(
                value: &wire::StatusQuery,
                connection: ConnectionBinding,
            ) -> Result<StatusQuery, ProtocolError> {
                if connection.generation.get() == 0 || value.cancel {
                    return Err(ProtocolError::Malformed);
                }
                Ok(StatusQuery {
                    connection,
                    request: nonzero(value.request)?,
                    generation: value.request_generation,
                    known_epoch: (value.known_epoch != 0)
                        .then(|| ServerEpoch::new(value.known_epoch)),
                    known_revision: NonZeroU64::new(value.known_revision),
                    subscribe: value.subscribe,
                })
            }
            pub fn encode_status(value: &ServerStatus) -> wire::ManagementStatus {
                let parts = value.parts();
                wire::ManagementStatus {
                    request: 0,
                    state: 1,
                    epoch: parts.epoch.get(),
                    runtime_version: parts
                        .running
                        .as_ref()
                        .map_or(String::new(), |v| v.as_str().to_owned()),
                    approved_version: parts
                        .approved
                        .as_ref()
                        .map_or(String::new(), |v| v.as_str().to_owned()),
                    staged_version: parts
                        .staged
                        .as_ref()
                        .map_or(String::new(), |v| v.as_str().to_owned()),
                    remote_access_enabled: parts.remote_access.value.is_enabled(),
                    majors: crate::negotiation::SUPPORTED_MAJORS.to_vec(),
                    failure: None,
                    server: Some(encode_global(parts.server.bytes())),
                    status_revision: parts.revision.get(),
                    native_verified: parts.native_verification == NativeVerification::Verified,
                    capabilities: parts
                        .capabilities
                        .iter()
                        .map(|v| wire::CapabilityStatus {
                            capability: v.capability,
                            availability: match v.availability {
                                CapabilityAvailability::Available => 1,
                                CapabilityAvailability::AuthorizationRequired => 2,
                                CapabilityAvailability::Unavailable => 3,
                                CapabilityAvailability::Unsupported => 4,
                            },
                        })
                        .collect(),
                    remote_access: Some(encode_policy(&parts.remote_access)),
                    listeners: parts
                        .listeners
                        .iter()
                        .map(|v| wire::ListenerStatus {
                            generation: v.generation.get(),
                            endpoint: Some(encode_endpoint(v.endpoint)),
                            state: match v.state {
                                ListenerState::Binding => 1,
                                ListenerState::Listening => 2,
                                ListenerState::Closing => 3,
                                ListenerState::Unavailable => 4,
                            },
                        })
                        .collect(),
                    progress: parts
                        .progress
                        .iter()
                        .map(|v| {
                            let (operation, native_request) = match v.request {
                                ManagementRequestReference::Operation(binding) => {
                                    (Some(encode_operation(binding.id)), Vec::new())
                                }
                                ManagementRequestReference::Native(bytes) => (None, bytes.to_vec()),
                            };
                            wire::ManagementProgress {
                                operation,
                                native_request,
                                state: match v.state {
                                    ManagementProgressState::Pending => 1,
                                    ManagementProgressState::Draining => 2,
                                    ManagementProgressState::WaitingForCleanup => 3,
                                    ManagementProgressState::Completed => 4,
                                    ManagementProgressState::Unavailable => 5,
                                    ManagementProgressState::RecoveryRequired => 6,
                                },
                                completed_units: v.completed_units,
                                total_units: v.total_units,
                                operation_digest: match v.request {
                                    ManagementRequestReference::Operation(binding) => {
                                        binding.digest.bytes().to_vec()
                                    }
                                    ManagementRequestReference::Native(_) => Vec::new(),
                                },
                            }
                        })
                        .collect(),
                }
            }
            /// Progress carries authenticated operation comparison data for every
            /// Client. The callback reads legacy records without the additive
            /// digest field; it never supplies a fabricated binding.
            pub fn decode_status(
                value: &wire::ManagementStatus,
                resolve_digest: &dyn Fn(OperationId) -> Result<RequestDigest, ProtocolError>,
            ) -> Result<ServerStatus, ProtocolError> {
                if value.failure.is_some() || value.state != 1 {
                    return Err(ProtocolError::UnexpectedMessage);
                }
                let server = decode_server(required(&value.server)?)?;
                let remote_access = decode_policy(required(&value.remote_access)?)?;
                if value.remote_access_enabled != remote_access.value.is_enabled()
                    || remote_access.value.server() != server
                {
                    return Err(ProtocolError::Malformed);
                }
                let version = |v: &String| {
                    if v.is_empty() {
                        Ok(None)
                    } else {
                        ServerVersion::new(v.clone())
                            .map(Some)
                            .map_err(|_| ProtocolError::Malformed)
                    }
                };
                let capabilities = value
                    .capabilities
                    .iter()
                    .map(|v| {
                        Ok(CapabilityStatus {
                            capability: v.capability,
                            availability: match v.availability {
                                1 => CapabilityAvailability::Available,
                                2 => CapabilityAvailability::AuthorizationRequired,
                                3 => CapabilityAvailability::Unavailable,
                                4 => CapabilityAvailability::Unsupported,
                                _ => return Err(ProtocolError::Malformed),
                            },
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let listeners = value
                    .listeners
                    .iter()
                    .map(|v| {
                        Ok(ListenerStatus {
                            generation: nonzero(v.generation)?,
                            endpoint: decode_endpoint(required(&v.endpoint)?)?,
                            state: match v.state {
                                1 => ListenerState::Binding,
                                2 => ListenerState::Listening,
                                3 => ListenerState::Closing,
                                4 => ListenerState::Unavailable,
                                _ => return Err(ProtocolError::Malformed),
                            },
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let progress = value
                    .progress
                    .iter()
                    .map(|v| {
                        let request = match (&v.operation, v.native_request.as_slice()) {
                            (Some(operation), []) => {
                                let id = decode_operation(operation)?;
                                ManagementRequestReference::Operation(OperationBinding {
                                    id,
                                    digest: if v.operation_digest.is_empty() {
                                        resolve_digest(id)?
                                    } else {
                                        RequestDigest::from_bytes(
                                            v.operation_digest
                                                .as_slice()
                                                .try_into()
                                                .map_err(|_| ProtocolError::Malformed)?,
                                        )
                                    },
                                })
                            }
                            (None, bytes) if bytes.len() == 16 && v.operation_digest.is_empty() => {
                                ManagementRequestReference::Native(
                                    bytes.try_into().map_err(|_| ProtocolError::Malformed)?,
                                )
                            }
                            _ => return Err(ProtocolError::Malformed),
                        };
                        Ok(ManagementProgress {
                            request,
                            state: match v.state {
                                1 => ManagementProgressState::Pending,
                                2 => ManagementProgressState::Draining,
                                3 => ManagementProgressState::WaitingForCleanup,
                                4 => ManagementProgressState::Completed,
                                5 => ManagementProgressState::Unavailable,
                                6 => ManagementProgressState::RecoveryRequired,
                                _ => return Err(ProtocolError::Malformed),
                            },
                            completed_units: v.completed_units,
                            total_units: v.total_units,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                ServerStatus::new(ServerStatusParts {
                    server,
                    epoch: ServerEpoch::new(value.epoch),
                    revision: nonzero(value.status_revision)?,
                    running: version(&value.runtime_version)?,
                    approved: version(&value.approved_version)?,
                    staged: version(&value.staged_version)?,
                    native_verification: if value.native_verified {
                        NativeVerification::Verified
                    } else {
                        NativeVerification::Unverified
                    },
                    capabilities,
                    remote_access,
                    listeners,
                    progress,
                })
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_publication(value: &StatusPublication) -> wire::StatusPublication {
                match value {
                    StatusPublication::Snapshot {
                        request,
                        generation,
                        status,
                    } => wire::StatusPublication {
                        request: request.get(),
                        request_generation: *generation,
                        base: 0,
                        status: Some(encode_status(status)),
                        resync_required: false,
                    },
                    StatusPublication::Changed {
                        request,
                        generation,
                        base,
                        status,
                    } => wire::StatusPublication {
                        request: request.get(),
                        request_generation: *generation,
                        base: base.get(),
                        status: Some(encode_status(status)),
                        resync_required: false,
                    },
                    StatusPublication::ResyncRequired {
                        request,
                        generation,
                    } => wire::StatusPublication {
                        request: request.get(),
                        request_generation: *generation,
                        base: 0,
                        status: None,
                        resync_required: true,
                    },
                }
            }
            pub fn decode_publication(
                value: &wire::StatusPublication,
                resolve_digest: &dyn Fn(OperationId) -> Result<RequestDigest, ProtocolError>,
            ) -> Result<StatusPublication, ProtocolError> {
                let request = nonzero(value.request)?;
                if value.resync_required {
                    if value.status.is_some() || value.base != 0 {
                        return Err(ProtocolError::Malformed);
                    }
                    return Ok(StatusPublication::ResyncRequired {
                        request,
                        generation: value.request_generation,
                    });
                }
                let status = decode_status(required(&value.status)?, resolve_digest)?;
                match NonZeroU64::new(value.base) {
                    None => Ok(StatusPublication::Snapshot {
                        request,
                        generation: value.request_generation,
                        status,
                    }),
                    Some(base) if base < status.parts().revision => {
                        Ok(StatusPublication::Changed {
                            request,
                            generation: value.request_generation,
                            base,
                            status,
                        })
                    }
                    _ => Err(ProtocolError::Malformed),
                }
            }
        }
    };
}
profile!(v1);
profile!(v2);

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::identity::ServerId;
    fn status() -> ServerStatus {
        let server = ServerId::new([1; 16]);
        ServerStatus::new(ServerStatusParts {
            server,
            epoch: ServerEpoch::new(3),
            revision: NonZeroU64::new(7).unwrap(),
            running: Some(ServerVersion::new("1.2.3".to_owned()).unwrap()),
            approved: None,
            staged: Some(ServerVersion::new("1.2.4".to_owned()).unwrap()),
            native_verification: NativeVerification::Unverified,
            capabilities: vec![CapabilityStatus {
                capability: 1,
                availability: CapabilityAvailability::AuthorizationRequired,
            }],
            remote_access: Versioned {
                version: FieldVersion::new(2),
                value: RemoteAccessPolicy::enabled(server, vec!["[::1]:5050".parse().unwrap()])
                    .unwrap(),
            },
            listeners: vec![ListenerStatus {
                generation: NonZeroU64::new(4).unwrap(),
                endpoint: "[::1]:5050".parse().unwrap(),
                state: ListenerState::Unavailable,
            }],
            progress: vec![ManagementProgress {
                request: ManagementRequestReference::Native([3; 16]),
                state: ManagementProgressState::WaitingForCleanup,
                completed_units: 1,
                total_units: 3,
            }],
        })
        .unwrap()
    }
    macro_rules! tests {
        ($name:ident, $p:ident) => {
            #[test]
            fn $name() {
                let status = status();
                let wire = $p::encode_status(&status);
                assert_eq!(
                    $p::decode_status(&wire, &|_| Err(ProtocolError::UnexpectedMessage)),
                    Ok(status)
                );
                let mut substituted = wire.clone();
                substituted.remote_access_enabled = false;
                assert_eq!(
                    $p::decode_status(&substituted, &|_| Err(ProtocolError::UnexpectedMessage)),
                    Err(ProtocolError::Malformed)
                );
                let mut substituted = wire;
                let duplicate = substituted.remote_access.as_ref().unwrap().endpoints[0].clone();
                substituted
                    .remote_access
                    .as_mut()
                    .unwrap()
                    .endpoints
                    .push(duplicate);
                assert_eq!(
                    $p::decode_status(&substituted, &|_| Err(ProtocolError::UnexpectedMessage)),
                    Err(ProtocolError::Malformed)
                );
            }
        };
    }
    tests!(
        major_1_preserves_native_verification_policy_listener_failure_and_progress,
        v1
    );
    tests!(
        major_2_preserves_native_verification_policy_listener_failure_and_progress,
        v2
    );
    macro_rules! progress_tests {
        ($name:ident, $profile:ident) => {
            #[test]
            fn $name() {
                use spaceterm_core::{
                    identity::*, operation_ledger::canonical::CanonicalCodec,
                    operation_ledger_contracts::CanonicalOperationRequest,
                };
                let mut parts = status().parts().clone();
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(parts.server, 9).unwrap(),
                    PairingId::new(parts.server, 2).unwrap(),
                    ClientId::new(parts.server, 3).unwrap(),
                )
                .unwrap();
                let binding = CanonicalOperationRequest::new(
                    OperationId::new(stream, OperationSequence::new(1).unwrap()),
                    Command::AssessEndAllSessions {
                        server: parts.server,
                    },
                    &CanonicalCodec,
                )
                .unwrap()
                .binding();
                parts.progress.push(ManagementProgress {
                    request: ManagementRequestReference::Operation(binding),
                    state: ManagementProgressState::Draining,
                    completed_units: 0,
                    total_units: 1,
                });
                let value = ServerStatus::new(parts).unwrap();
                let mut wire = $profile::encode_status(&value);
                assert_eq!(
                    $profile::decode_status(&wire, &|_| panic!(
                        "authenticated progress cannot require local operation ownership"
                    )),
                    Ok(value.clone())
                );
                wire.progress[1].operation_digest.pop();
                assert_eq!(
                    $profile::decode_status(&wire, &|_| panic!(
                        "malformed digest cannot fall back"
                    )),
                    Err(ProtocolError::Malformed)
                );
                wire.progress[1].operation_digest.clear();
                assert_eq!(
                    $profile::decode_status(&wire, &|id| {
                        assert_eq!(id, binding.id);
                        Ok(binding.digest)
                    }),
                    Ok(value.clone())
                );
                assert_eq!(
                    $profile::decode_status(&wire, &|_| Err(ProtocolError::Malformed)),
                    Err(ProtocolError::Malformed)
                );
                let mut wire = $profile::encode_status(&value);
                wire.progress[0].operation_digest = binding.digest.bytes().to_vec();
                assert_eq!(
                    $profile::decode_status(&wire, &|_| panic!("native progress has no operation")),
                    Err(ProtocolError::Malformed)
                );
            }
        };
    }
    progress_tests!(
        major_1_progress_carries_checked_digest_and_reads_legacy_local_bindings,
        v1
    );
    progress_tests!(
        major_2_progress_carries_checked_digest_and_reads_legacy_local_bindings,
        v2
    );
}
