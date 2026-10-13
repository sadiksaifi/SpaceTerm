#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

use spaceterm_host::{
    account_service::test_support::AccountServiceFixture,
    server_keys::ServerKeyStore,
    startup::{BuildIdentity, DesiredServerState},
    transport::{
        AdmissionLimits, EnrolledServerKey, MessageLimit, MessageTransport, TransportError,
    },
    wss::{RemoteAccessEnablement, WssListener, WssTransport, server_tls_config},
};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

#[tokio::test]
async fn real_identity_signs_tls_and_wrong_enrolled_key_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let identity = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let tls = server_tls_config(identity).unwrap();
    let enabled = RemoteAccessEnablement::from_durable_setting(
        registration.server_pin().identity,
        vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
    )
    .unwrap();
    let listener = Arc::new(
        WssListener::bind(
            Some(&enabled),
            tls,
            MessageLimit::envelope(),
            AdmissionLimits::new(2, Duration::from_secs(2)).unwrap(),
        )
        .await
        .unwrap(),
    );
    let address = listener.local_addresses().await.unwrap()[0];
    let pin = EnrolledServerKey(registration.server_pin().public_key);
    let (client, server) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            pin,
            MessageLimit::envelope(),
            Duration::from_secs(2)
        ),
        listener.accept()
    );
    let client = client.unwrap();
    let server = server.unwrap().finish_authentication().unwrap();
    assert_eq!(client.peer_facts().pinned_server_key, Some(pin));
    client.send(b"pinned identity").await.unwrap();
    assert_eq!(server.recv().await.unwrap().unwrap(), b"pinned identity");
    client.close().await.unwrap();
    server.close().await.unwrap();
    let (client, _) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            EnrolledServerKey([9; 32]),
            MessageLimit::envelope(),
            Duration::from_secs(2)
        ),
        listener.accept()
    );
    assert_eq!(client.err(), Some(TransportError::TlsIdentityRejected));
    listener.disable().await;
}
