//! Account-local Settings facts. Remote routes cannot select this provider.

use crate::ui::settings_window::server_runtime::{
    BindScope, ServerPermission, ServerPermissionKind, ServerPermissionState, ServerPermissions,
    ServerServiceLifecycle, SettingsMachineFacts,
};

pub(crate) fn lifecycle() -> ServerServiceLifecycle {
    if cfg!(target_os = "macos") {
        ServerServiceLifecycle::LoginSession
    } else {
        ServerServiceLifecycle::BootWithLingering
    }
}

pub(crate) fn machine_facts(build: spaceterm_host::startup::BuildIdentity) -> SettingsMachineFacts {
    // A retained WSS route must survive service restart. Never offer ephemeral port zero.
    let port = match build {
        spaceterm_host::startup::BuildIdentity::Release => 47230,
        spaceterm_host::startup::BuildIdentity::Preflight => 47231,
        spaceterm_host::startup::BuildIdentity::Development => 47232,
    };
    // macOS TCC belongs to the server executable, not this client. Until the server reports
    // those facts, do not display this desktop process's permissions as server permissions.
    let permissions = if cfg!(target_os = "linux") {
        Some(ServerPermissions {
            naming: crate::platform::permission_access::AccessibilityNaming::Accessibility,
            entries: [
                ServerPermissionKind::Microphone,
                ServerPermissionKind::ScreenRecording,
                ServerPermissionKind::Accessibility,
            ]
            .into_iter()
            .map(|kind| ServerPermission {
                kind,
                state: ServerPermissionState::NotNeeded,
            })
            .collect(),
        })
    } else {
        None
    };
    let scope =
        |id: &'static str, label: &'static str, endpoints: Vec<std::net::SocketAddr>| BindScope {
            id: id.into(),
            label: label.into(),
            addresses: endpoints
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
                .into(),
            endpoints,
        };
    SettingsMachineFacts {
        permissions,
        scopes: vec![
            scope(
                "loopback",
                "This computer only",
                vec![
                    std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
                ],
            ),
            scope(
                "all-interfaces",
                "All network interfaces",
                vec![
                    std::net::SocketAddr::from(([0, 0, 0, 0], port)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
                ],
            ),
        ],
    }
}
