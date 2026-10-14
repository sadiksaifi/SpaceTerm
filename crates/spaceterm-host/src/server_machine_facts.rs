//! Read-only owning-process platform facts. No service, privacy, or listener effects.

use crate::startup::BuildIdentity;
use spaceterm_core::management::{
    MachineFactsAvailability, ServerAccessibilityNaming, ServerBindScope, ServerBindScopeOffer,
    ServerMachineFacts, ServerPermissionFacts, ServerPermissionState, ServerPlatform,
};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

/// Observe the actual host and build. Offers never open sockets or enable policy.
/// macOS privacy checks in the desktop require GUI frameworks and are not safe
/// headless owning-server queries. Their absence is explicit, not a guessed grant.
pub fn observe(build: BuildIdentity) -> ServerMachineFacts {
    let platform = if cfg!(target_os = "macos") {
        ServerPlatform::MacOs
    } else if cfg!(target_os = "linux") {
        ServerPlatform::Linux
    } else {
        ServerPlatform::Unknown
    };
    let state = match platform {
        // These are Apple-style inherited system privacy permissions, not Linux
        // filesystem access, device access, or a daemon-session capability probe.
        ServerPlatform::Linux => ServerPermissionState::NotNeeded,
        ServerPlatform::MacOs => ServerPermissionState::Unavailable,
        ServerPlatform::Unknown => ServerPermissionState::Unknown,
    };
    let permissions = ServerPermissionFacts {
        microphone: state,
        screen_recording: state,
        accessibility: state,
        accessibility_naming: if platform == ServerPlatform::Linux {
            ServerAccessibilityNaming::Accessibility
        } else {
            // Darwin versions do not determine the product's permission naming.
            ServerAccessibilityNaming::Unknown
        },
    };
    if platform == ServerPlatform::Unknown {
        return ServerMachineFacts {
            platform,
            permissions,
            ..ServerMachineFacts::default()
        };
    }
    let port = match build {
        BuildIdentity::Release => 47230,
        BuildIdentity::Preflight => 47231,
        BuildIdentity::Development => 47232,
    };
    ServerMachineFacts {
        platform,
        permissions,
        bind_scope_availability: MachineFactsAvailability::Available,
        bind_scopes: [
            (
                ServerBindScope::Loopback,
                Ipv4Addr::LOCALHOST,
                Ipv6Addr::LOCALHOST,
            ),
            (
                ServerBindScope::AllInterfaces,
                Ipv4Addr::UNSPECIFIED,
                Ipv6Addr::UNSPECIFIED,
            ),
        ]
        .into_iter()
        .map(|(scope, v4, v6)| ServerBindScopeOffer {
            scope,
            endpoints: vec![SocketAddr::from((v4, port)), SocketAddr::from((v6, port))],
        })
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owning_platform_offers_preserve_build_ports_without_listener_effects() {
        for (build, port) in [
            (BuildIdentity::Release, 47230),
            (BuildIdentity::Preflight, 47231),
            (BuildIdentity::Development, 47232),
        ] {
            let facts = observe(build);
            assert_eq!(observe(build), facts);
            if cfg!(any(target_os = "macos", target_os = "linux")) {
                assert_eq!(
                    facts.bind_scope_availability,
                    MachineFactsAvailability::Available
                );
                assert_eq!(facts.bind_scopes.len(), 2);
                assert_eq!(
                    facts.bind_scopes[0].endpoints,
                    vec![
                        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
                    ]
                );
                assert_eq!(
                    facts.bind_scopes[1].endpoints,
                    vec![
                        SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
                        SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
                    ]
                );
            }
            let expected = if cfg!(target_os = "linux") {
                ServerPermissionState::NotNeeded
            } else if cfg!(target_os = "macos") {
                ServerPermissionState::Unavailable
            } else {
                ServerPermissionState::Unknown
            };
            assert_eq!(facts.permissions.microphone, expected);
            assert_eq!(facts.permissions.screen_recording, expected);
            assert_eq!(facts.permissions.accessibility, expected);
        }
    }
}
