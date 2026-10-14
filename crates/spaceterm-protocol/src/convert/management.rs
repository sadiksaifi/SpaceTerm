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
            fn encode_permission(value: ServerPermissionState) -> u32 {
                match value {
                    ServerPermissionState::Unknown => 0,
                    ServerPermissionState::Granted => 1,
                    ServerPermissionState::NotGranted => 2,
                    ServerPermissionState::NotNeeded => 3,
                    ServerPermissionState::Unavailable => 4,
                }
            }
            fn decode_permission(value: u32) -> ServerPermissionState {
                match value {
                    1 => ServerPermissionState::Granted,
                    2 => ServerPermissionState::NotGranted,
                    3 => ServerPermissionState::NotNeeded,
                    4 => ServerPermissionState::Unavailable,
                    _ => ServerPermissionState::Unknown,
                }
            }
            fn encode_machine_facts(value: &ServerMachineFacts) -> wire::ServerMachineFacts {
                wire::ServerMachineFacts {
                    platform: match value.platform {
                        ServerPlatform::Unknown => 0,
                        ServerPlatform::MacOs => 1,
                        ServerPlatform::Linux => 2,
                    },
                    permissions: Some(wire::ServerPermissionFacts {
                        microphone: encode_permission(value.permissions.microphone),
                        screen_recording: encode_permission(value.permissions.screen_recording),
                        accessibility: encode_permission(value.permissions.accessibility),
                        accessibility_naming: match value.permissions.accessibility_naming {
                            ServerAccessibilityNaming::Unknown => 0,
                            ServerAccessibilityNaming::Accessibility => 1,
                            ServerAccessibilityNaming::DeviceControl => 2,
                        },
                    }),
                    bind_scope_availability: match value.bind_scope_availability {
                        MachineFactsAvailability::Unknown => 0,
                        MachineFactsAvailability::Available => 1,
                        MachineFactsAvailability::Unavailable => 2,
                    },
                    bind_scopes: value
                        .bind_scopes
                        .iter()
                        .map(|offer| wire::ServerBindScopeOffer {
                            scope: match offer.scope {
                                ServerBindScope::Loopback => 1,
                                ServerBindScope::AllInterfaces => 2,
                            },
                            endpoints: offer
                                .endpoints
                                .iter()
                                .copied()
                                .map(encode_endpoint)
                                .collect(),
                        })
                        .collect(),
                }
            }
            fn decode_machine_facts(
                value: &wire::ServerMachineFacts,
            ) -> Result<ServerMachineFacts, ProtocolError> {
                if value.bind_scopes.len() > MAX_SERVER_BIND_SCOPES
                    || (matches!(value.bind_scope_availability, 0 | 2)
                        && !value.bind_scopes.is_empty())
                {
                    return Err(ProtocolError::Malformed);
                }
                let permissions = value
                    .permissions
                    .as_ref()
                    .map(|p| ServerPermissionFacts {
                        microphone: decode_permission(p.microphone),
                        screen_recording: decode_permission(p.screen_recording),
                        accessibility: decode_permission(p.accessibility),
                        accessibility_naming: match p.accessibility_naming {
                            1 => ServerAccessibilityNaming::Accessibility,
                            2 => ServerAccessibilityNaming::DeviceControl,
                            _ => ServerAccessibilityNaming::Unknown,
                        },
                    })
                    .unwrap_or_default();
                let mut bind_scopes = Vec::new();
                for offer in &value.bind_scopes {
                    if offer.endpoints.is_empty()
                        || offer.endpoints.len()
                            > spaceterm_core::remote_access::MAX_REMOTE_BIND_ENDPOINTS
                    {
                        return Err(ProtocolError::Malformed);
                    }
                    // Unsupported scopes still carry bounded, valid endpoint encodings.
                    let endpoints = offer
                        .endpoints
                        .iter()
                        .map(decode_endpoint)
                        .collect::<Result<Vec<_>, _>>()?;
                    if endpoints.iter().any(|endpoint| endpoint.port() == 0) {
                        return Err(ProtocolError::Malformed);
                    }
                    let scope = match offer.scope {
                        1 => ServerBindScope::Loopback,
                        2 => ServerBindScope::AllInterfaces,
                        _ => continue,
                    };
                    bind_scopes.push(ServerBindScopeOffer { scope, endpoints });
                }
                let bind_scope_availability = match value.bind_scope_availability {
                    0 => MachineFactsAvailability::Unknown,
                    1 if bind_scopes.is_empty() && !value.bind_scopes.is_empty() => {
                        MachineFactsAvailability::Unavailable
                    }
                    1 => MachineFactsAvailability::Available,
                    2 => MachineFactsAvailability::Unavailable,
                    // Validate known offers before decode_status discards them for
                    // an unsupported availability. No intermediate status escapes.
                    _ if !bind_scopes.is_empty() => MachineFactsAvailability::Available,
                    _ => MachineFactsAvailability::Unknown,
                };
                Ok(ServerMachineFacts {
                    platform: match value.platform {
                        1 => ServerPlatform::MacOs,
                        2 => ServerPlatform::Linux,
                        _ => ServerPlatform::Unknown,
                    },
                    permissions,
                    bind_scope_availability,
                    bind_scopes,
                })
            }
            /// Omit nested additions unknown to older authenticated peers.
            pub fn encode_status_for_capabilities(
                value: &ServerStatus,
                capabilities: &[u32],
            ) -> wire::ManagementStatus {
                let mut encoded = encode_status(value);
                if !capabilities.contains(&crate::negotiation::CAPABILITY_SERVER_MACHINE_FACTS) {
                    encoded.machine_facts = None;
                }
                encoded
            }
            pub fn encode_status(value: &ServerStatus) -> wire::ManagementStatus {
                let parts = value.parts();
                wire::ManagementStatus {
                    request: 0,
                    state: 1,
                    machine_facts: value.machine_facts().map(encode_machine_facts),
                    storage_state: match parts.runtime.storage {
                        StorageStatus::Unknown => 0,
                        StorageStatus::Ready => 1,
                        StorageStatus::Locked => 2,
                        StorageStatus::Unavailable => 3,
                        StorageStatus::RecoveryRequired => 4,
                    },
                    run_intent: match parts.runtime.intent {
                        ServerRunIntent::Unknown => 0,
                        ServerRunIntent::Running => 1,
                        ServerRunIntent::Stopped => 2,
                    },
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
                                ManagementRequestReference::OperationReference(id) => {
                                    (Some(encode_operation(id)), Vec::new())
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
                            }
                        })
                        .collect(),
                }
            }
            /// Progress is a comparison-only reference. It grants no operation
            /// execution or retry authority and needs no originating journal.
            pub fn decode_status(
                value: &wire::ManagementStatus,
                _resolve_digest: &dyn Fn(OperationId) -> Result<RequestDigest, ProtocolError>,
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
                                ManagementRequestReference::OperationReference(id)
                            }
                            (None, bytes) if bytes.len() == 16 => {
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
                    runtime: ServerRuntimeStatus {
                        storage: match value.storage_state {
                            0 => StorageStatus::Unknown,
                            1 => StorageStatus::Ready,
                            2 => StorageStatus::Locked,
                            3 => StorageStatus::Unavailable,
                            4 => StorageStatus::RecoveryRequired,
                            _ => return Err(ProtocolError::Malformed),
                        },
                        intent: match value.run_intent {
                            0 => ServerRunIntent::Unknown,
                            1 => ServerRunIntent::Running,
                            2 => ServerRunIntent::Stopped,
                            _ => return Err(ProtocolError::Malformed),
                        },
                    },
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
                .and_then(|status| match &value.machine_facts {
                    Some(facts) => {
                        let decoded = decode_machine_facts(facts)?;
                        let platform = decoded.platform;
                        let permissions = decoded.permissions;
                        let status = status
                            .with_machine_facts(decoded)
                            .map_err(|_| ProtocolError::Malformed)?;
                        if facts.bind_scope_availability > 2 {
                            status
                                .with_machine_facts(ServerMachineFacts {
                                    platform,
                                    permissions,
                                    bind_scope_availability: MachineFactsAvailability::Unknown,
                                    bind_scopes: Vec::new(),
                                })
                                .map_err(|_| ProtocolError::Malformed)
                        } else {
                            Ok(status)
                        }
                    }
                    None => Ok(status),
                })
            }
            /// Preserve the envelope while omitting unsupported nested comparisons.
            pub fn encode_publication_for_capabilities(
                value: &StatusPublication,
                capabilities: &[u32],
            ) -> wire::StatusPublication {
                let mut encoded = encode_publication(value);
                if !capabilities.contains(&crate::negotiation::CAPABILITY_SERVER_MACHINE_FACTS)
                    && let Some(status) = &mut encoded.status
                {
                    status.machine_facts = None;
                }
                encoded
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
            runtime: Default::default(),
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
    fn machine_facts() -> ServerMachineFacts {
        ServerMachineFacts {
            platform: ServerPlatform::Linux,
            permissions: ServerPermissionFacts {
                microphone: ServerPermissionState::NotNeeded,
                screen_recording: ServerPermissionState::Unavailable,
                accessibility: ServerPermissionState::NotGranted,
                accessibility_naming: ServerAccessibilityNaming::Accessibility,
            },
            bind_scope_availability: MachineFactsAvailability::Available,
            bind_scopes: vec![ServerBindScopeOffer {
                scope: ServerBindScope::Loopback,
                endpoints: vec![
                    "127.0.0.1:47230".parse().unwrap(),
                    "[::1]:47230".parse().unwrap(),
                ],
            }],
        }
    }
    #[test]
    fn machine_facts_attachment_rejects_unbounded_duplicate_and_inexact_offers() {
        let valid = machine_facts();
        let mut invalid = Vec::new();
        let mut value = valid.clone();
        value.bind_scopes = vec![value.bind_scopes[0].clone(); 3];
        invalid.push((value, StatusError::Limit));
        let mut value = valid.clone();
        value.bind_scopes.push(value.bind_scopes[0].clone());
        invalid.push((value, StatusError::InvalidBinding));
        for endpoints in [
            vec![],
            vec!["127.0.0.1:47230", "127.0.0.1:47230"],
            vec!["127.0.0.2:47230", "[::1]:47230"],
            vec!["127.0.0.1:0", "[::1]:0"],
            vec!["127.0.0.1:47230", "[::1]:47231"],
            vec!["127.0.0.1:47230", "[::1%1]:47230"],
        ] {
            let mut value = valid.clone();
            value.bind_scopes[0].endpoints = endpoints.iter().map(|v| v.parse().unwrap()).collect();
            invalid.push((value, StatusError::InvalidBinding));
        }
        let mut value = valid.clone();
        value.bind_scopes[0].endpoints = vec!["127.0.0.1:47230".parse().unwrap(); 9];
        invalid.push((value, StatusError::Limit));
        let mut value = valid;
        value.bind_scope_availability = MachineFactsAvailability::Unavailable;
        invalid.push((value, StatusError::InvalidBinding));
        for (facts, error) in invalid {
            assert_eq!(status().with_machine_facts(facts), Err(error));
        }
    }
    macro_rules! machine_tests {
        ($name:ident, $p:ident) => {
            #[test]
            fn $name() {
                let cap = [crate::negotiation::CAPABILITY_SERVER_MACHINE_FACTS];
                for permission in [
                    ServerPermissionState::Unknown,
                    ServerPermissionState::Granted,
                    ServerPermissionState::NotGranted,
                    ServerPermissionState::NotNeeded,
                    ServerPermissionState::Unavailable,
                ] {
                    let mut facts = machine_facts();
                    facts.permissions.microphone = permission;
                    let status = status().with_machine_facts(facts).unwrap();
                    let encoded = $p::encode_status_for_capabilities(&status, &cap);
                    assert_eq!(
                        $p::decode_status(&encoded, &|_| Err(ProtocolError::UnexpectedMessage)),
                        Ok(status.clone())
                    );
                    let omitted = $p::encode_status_for_capabilities(&status, &[]);
                    assert!(omitted.machine_facts.is_none());
                    assert_eq!(
                        $p::decode_status(&omitted, &|_| Err(ProtocolError::UnexpectedMessage)),
                        Ok(status.clone().without_machine_facts())
                    );
                    for publication in [
                        StatusPublication::Snapshot {
                            request: NonZeroU64::MIN,
                            generation: 1,
                            status: status.clone(),
                        },
                        StatusPublication::Changed {
                            request: NonZeroU64::MIN,
                            generation: 1,
                            base: NonZeroU64::new(6).unwrap(),
                            status,
                        },
                    ] {
                        let supported = $p::encode_publication_for_capabilities(&publication, &cap);
                        assert_eq!(
                            $p::decode_publication(&supported, &|_| Err(
                                ProtocolError::UnexpectedMessage
                            )),
                            Ok(publication.clone())
                        );
                        let omitted = $p::encode_publication_for_capabilities(&publication, &[]);
                        assert!(omitted.status.unwrap().machine_facts.is_none());
                    }
                }
                let unavailable = status()
                    .with_machine_facts(ServerMachineFacts {
                        platform: ServerPlatform::MacOs,
                        permissions: ServerPermissionFacts {
                            microphone: ServerPermissionState::Unavailable,
                            screen_recording: ServerPermissionState::Unavailable,
                            accessibility: ServerPermissionState::Unavailable,
                            ..Default::default()
                        },
                        bind_scope_availability: MachineFactsAvailability::Unavailable,
                        bind_scopes: vec![],
                    })
                    .unwrap();
                assert_eq!(
                    $p::decode_status(&$p::encode_status(&unavailable), &|_| Err(
                        ProtocolError::UnexpectedMessage
                    )),
                    Ok(unavailable)
                );
                let legacy = $p::decode_status(&$p::encode_status(&status()), &|_| {
                    Err(ProtocolError::UnexpectedMessage)
                })
                .unwrap();
                assert!(legacy.machine_facts().is_none());
                let mut wire =
                    $p::encode_status(&status().with_machine_facts(machine_facts()).unwrap());
                wire.machine_facts.as_mut().unwrap().bind_scopes[0].scope = 99;
                let decoded = $p::decode_status(&wire, &|_| Err(ProtocolError::UnexpectedMessage))
                    .expect("a future scope must be omitted without discarding known facts");
                let facts = decoded.machine_facts().unwrap();
                assert!(matches!(
                    facts.bind_scope_availability,
                    MachineFactsAvailability::Unknown | MachineFactsAvailability::Unavailable
                ));
                let mut expected = machine_facts();
                expected.bind_scope_availability = facts.bind_scope_availability;
                expected.bind_scopes.clear();
                assert_eq!(facts, &expected);
                wire.machine_facts.as_mut().unwrap().bind_scopes[0].scope = 2;
                assert_eq!(
                    $p::decode_status(&wire, &|_| Err(ProtocolError::UnexpectedMessage)),
                    Err(ProtocolError::Malformed)
                );
            }
        };
    }
    machine_tests!(
        major_1_machine_facts_preserve_unavailable_and_omit_for_legacy_peers,
        v1
    );
    machine_tests!(
        major_2_machine_facts_preserve_unavailable_and_omit_for_legacy_peers,
        v2
    );
    #[test]
    fn shared_progress_never_requires_the_initiating_clients_digest() {
        use spaceterm_core::identity::*;
        let server = ServerId::new([1; 16]);
        let stream = OperationStreamId::new(
            OperationStreamKey::new(server, 4).unwrap(),
            PairingId::new(server, 5).unwrap(),
            ClientId::new(server, 6).unwrap(),
        )
        .unwrap();
        let id = OperationId::new(stream, OperationSequence::new(1).unwrap());
        let mut parts = status().parts().clone();
        parts.progress[0].request = ManagementRequestReference::Operation(OperationBinding {
            id,
            digest: RequestDigest::from_bytes([8; 32]),
        });
        let status = ServerStatus::new(parts).unwrap();
        for decoded in [
            v1::decode_status(&v1::encode_status(&status), &|_| {
                panic!("shared progress is comparison-only")
            }),
            v2::decode_status(&v2::encode_status(&status), &|_| {
                panic!("shared progress is comparison-only")
            }),
        ] {
            assert!(decoded.is_ok());
        }
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
    #[test]
    fn both_majors_preserve_storage_and_deliberate_intent_without_granting_authority() {
        for storage in [
            StorageStatus::Ready,
            StorageStatus::Locked,
            StorageStatus::Unavailable,
            StorageStatus::RecoveryRequired,
        ] {
            for intent in [ServerRunIntent::Running, ServerRunIntent::Stopped] {
                let mut parts = status().parts().clone();
                parts.runtime = ServerRuntimeStatus { storage, intent };
                let status = ServerStatus::new(parts).unwrap();
                assert_eq!(
                    v1::decode_status(&v1::encode_status(&status), &|_| {
                        Err(ProtocolError::UnexpectedMessage)
                    }),
                    Ok(status.clone())
                );
                assert_eq!(
                    v2::decode_status(&v2::encode_status(&status), &|_| {
                        Err(ProtocolError::UnexpectedMessage)
                    }),
                    Ok(status)
                );
            }
        }
    }

    #[test]
    fn older_status_without_runtime_fields_stays_unknown_and_unknown_tags_are_rejected() {
        let mut wire = v2::encode_status(&status());
        wire.storage_state = 0;
        wire.run_intent = 0;
        let decoded = v2::decode_status(&wire, &|_| Err(ProtocolError::UnexpectedMessage)).unwrap();
        assert_eq!(decoded.parts().runtime, ServerRuntimeStatus::default());
        wire.storage_state = 99;
        assert_eq!(
            v2::decode_status(&wire, &|_| Err(ProtocolError::UnexpectedMessage)),
            Err(ProtocolError::Malformed)
        );
        wire.storage_state = 0;
        wire.run_intent = 99;
        assert_eq!(
            v2::decode_status(&wire, &|_| Err(ProtocolError::UnexpectedMessage)),
            Err(ProtocolError::Malformed)
        );
    }
}
