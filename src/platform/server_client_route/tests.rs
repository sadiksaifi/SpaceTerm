use super::connector;
use spaceterm_client::{
    api::{ConnectionTarget, RouteDescription, RouteParts},
    failure::ClientFailureClass,
    ports::{ConnectionPurpose, RouteConnector},
};
use spaceterm_core::{
    identity::{ClientId, DeviceId, PairingId, ServerId},
    pairing::{CredentialReference, LocalPairingGeneration},
};
use spaceterm_protocol::authentication::ServerPin;

#[tokio::test]
async fn desktop_route_does_not_create_an_ssh_fallback_without_native_configuration() {
    let server = ServerId::new([1; 16]);
    let target = ConnectionTarget::new(
        ServerPin {
            identity: server.bytes(),
            public_key: [2; 32],
        },
        DeviceId::new([3; 16]),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
        RouteDescription::new(RouteParts::Ssh {
            destination: "selected-host".into(),
            remote_socket: "/private/server".into(),
        })
        .unwrap(),
        CredentialReference::new([4; 16]),
        LocalPairingGeneration::new(1).unwrap(),
    )
    .unwrap();
    let connector = connector(vec![], None).unwrap();
    let error = match connector.open(&target, ConnectionPurpose::Ordinary).await {
        Ok(_) => panic!("desktop route manufactured native SSH authority"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::UnsafeEndpoint);
}

#[tokio::test]
async fn desktop_saved_ssh_route_uses_captured_native_configuration() {
    use super::{SshStartup, retain_runtime, ssh_configuration};
    use crate::platform::{
        app_directories::AppDirectoryEnvironment, unix_secure_filesystem::UnixSecureFilesystem,
    };
    use std::{os::unix::fs::PermissionsExt, sync::Arc};
    let mut suffix = [0; 4];
    getrandom::fill(&mut suffix).unwrap();
    #[cfg(target_os = "macos")]
    let parent = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let parent = spaceterm_host::account_service::TrustedAccountFacts::current()
        .unwrap()
        .home()
        .to_owned();
    let root = parent.join(format!("stsr-{:x}", u32::from_be_bytes(suffix)));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = root.join("ssh");
    let arguments = root.join("arguments");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 1\n",
            arguments.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let paths = crate::platform::testing::resolve_app_paths(
        &AppDirectoryEnvironment {
            home: Some(root.clone().into_os_string()),
            xdg_config_home: Some(root.join("config").into_os_string()),
            xdg_data_home: Some(root.join("data").into_os_string()),
            xdg_state_home: Some(root.join("state").into_os_string()),
            xdg_cache_home: Some(root.join("cache").into_os_string()),
            xdg_runtime_dir: Some(root.join("runtime").into_os_string()),
            ..Default::default()
        },
        None,
        104,
        Arc::new(UnixSecureFilesystem),
    )
    .unwrap();
    let (configuration, owner) = ssh_configuration(
        &paths,
        &SshStartup {
            executable,
            home: root.clone(),
            environment: vec![("PATH".into(), "/usr/bin:/bin".into())],
        },
        None,
    )
    .unwrap();
    let routes = retain_runtime(connector(vec![], Some(configuration)).unwrap(), owner);
    let server = ServerId::new([8; 16]);
    let target = ConnectionTarget::new(
        ServerPin {
            identity: server.bytes(),
            public_key: [9; 32],
        },
        DeviceId::new([3; 16]),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
        RouteDescription::new(RouteParts::Ssh {
            destination: "saved-host".into(),
            remote_socket: "/remote/server".into(),
        })
        .unwrap(),
        CredentialReference::new([4; 16]),
        LocalPairingGeneration::new(1).unwrap(),
    )
    .unwrap();
    let error = match routes.open(&target, ConnectionPurpose::Ordinary).await {
        Ok(_) => panic!("failed OpenSSH process admitted a route"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::Unavailable);
    let captured = std::fs::read_to_string(arguments).unwrap();
    assert!(captured.contains(paths.managed_ssh_config().to_str().unwrap()));
    assert!(captured.contains("saved-host"));
    assert!(captured.contains(":/remote/server"));
    assert!(captured.contains("BatchMode=yes"));
    drop(routes);
    drop(paths);
    std::fs::remove_dir_all(root).unwrap();
}
