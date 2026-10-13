use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use spaceterm_host::transport::{
    AdmissionLimits, EnrolledServerKey, MessageLimit, MessageTransport, TransportError,
};
use spaceterm_host::wss::{RemoteAccessEnablement, WssListener, WssTransport};

fn tls() -> (Arc<rustls::ServerConfig>, EnrolledServerKey) {
    let key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let pin = EnrolledServerKey(key.public_key_raw().try_into().unwrap());
    let certificate = CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let configuration = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .unwrap();
    (Arc::new(configuration), pin)
}

fn enablement() -> RemoteAccessEnablement {
    RemoteAccessEnablement::from_durable_setting(
        [1; 16],
        vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
    )
    .unwrap()
}

fn admission() -> AdmissionLimits {
    AdmissionLimits::new(2, Duration::from_secs(1)).unwrap()
}

#[tokio::test]
async fn wss_cannot_listen_without_explicit_remote_access_enablement() {
    let (tls, _) = tls();
    assert_eq!(
        WssListener::bind(None, tls, MessageLimit::envelope(), admission())
            .await
            .err(),
        Some(TransportError::RemoteAccessDisabled)
    );
}

#[tokio::test]
async fn wss_binary_round_trip_uses_the_enrolled_key_and_disabling_closes_the_listener() {
    let (tls, pin) = tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        tls,
        MessageLimit::new(64).unwrap(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (client, server) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            pin,
            MessageLimit::new(64).unwrap(),
            Duration::from_secs(1)
        ),
        listener.accept()
    );
    let client = client.unwrap();
    let server = server.unwrap().finish_authentication().unwrap();
    assert_eq!(client.peer_facts().pinned_server_key, Some(pin));
    let mut client_progress = client
        .write_progress()
        .expect("native TCP progress below TLS");
    client_progress.borrow_and_update();
    client.send(b"envelope").await.unwrap();
    assert!(client_progress.has_changed().unwrap());
    assert_eq!(server.recv().await.unwrap(), Some(b"envelope".to_vec()));
    let mut server_progress = server
        .write_progress()
        .expect("native TCP progress below TLS");
    server_progress.borrow_and_update();
    server.send(b"reply").await.unwrap();
    assert!(server_progress.has_changed().unwrap());
    assert_eq!(client.recv().await.unwrap(), Some(b"reply".to_vec()));
    listener.disable().await;
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    assert_eq!(
        listener.accept().await.err(),
        Some(TransportError::RemoteAccessDisabled)
    );
}

#[tokio::test]
async fn wss_rejects_an_unenrolled_server_key_without_web_pki_fallback() {
    let (_, wrong_pin) = tls();
    let (tls, _) = tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        tls,
        MessageLimit::envelope(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (client, _) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            wrong_pin,
            MessageLimit::envelope(),
            Duration::from_secs(1)
        ),
        listener.accept()
    );
    assert_eq!(client.err(), Some(TransportError::TlsIdentityRejected));
}

fn raw_tls() -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
    let key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let certificate = CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    (Arc::new(server), Arc::new(client))
}

async fn raw_client(
    address: SocketAddr,
    configuration: Arc<rustls::ClientConfig>,
) -> tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
    let stream = tokio_rustls::TlsConnector::from(configuration)
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            tcp,
        )
        .await
        .unwrap();
    let (websocket, response) =
        tokio_tungstenite::client_async(format!("wss://{address}/spaceterm"), stream)
            .await
            .unwrap();
    assert!(!response.headers().contains_key("sec-websocket-extensions"));
    websocket
}

#[tokio::test]
async fn wss_rejects_text_even_when_the_message_fits_the_bound() {
    use futures_util::SinkExt;
    let (server_tls, client_tls) = raw_tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        server_tls,
        MessageLimit::new(8).unwrap(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (mut client, server) = tokio::join!(raw_client(address, client_tls), listener.accept());
    let server = server.unwrap().finish_authentication().unwrap();
    client
        .send(tokio_tungstenite::tungstenite::Message::Text("text".into()))
        .await
        .unwrap();
    assert_eq!(server.recv().await, Err(TransportError::WebSocketRejected));
}

#[tokio::test]
async fn wss_bounds_frames_and_the_fragmented_message_aggregate() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    for frames in [
        vec![Frame::message(vec![1; 9], OpCode::Data(Data::Binary), true)],
        vec![
            Frame::message(vec![1; 8], OpCode::Data(Data::Binary), false),
            Frame::message(vec![1; 1], OpCode::Data(Data::Continue), true),
        ],
    ] {
        let (server_tls, client_tls) = raw_tls();
        let listener = WssListener::bind(
            Some(&enablement()),
            server_tls,
            MessageLimit::new(8).unwrap(),
            admission(),
        )
        .await
        .unwrap();
        let address = listener.local_addresses().await.unwrap()[0];
        let (mut client, server) = tokio::join!(raw_client(address, client_tls), listener.accept());
        let server = server.unwrap().finish_authentication().unwrap();
        for frame in frames {
            client.send(Message::Frame(frame)).await.unwrap();
        }
        assert_eq!(server.recv().await, Err(TransportError::MessageTooLarge));
    }
}

#[tokio::test]
async fn disabling_wss_interrupts_an_accept_already_waiting_on_a_socket() {
    let (tls, _) = tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        tls,
        MessageLimit::envelope(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (accepted, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(listener.accept(), async {
            tokio::task::yield_now().await;
            listener.disable().await;
        })
    })
    .await
    .unwrap();
    assert_eq!(accepted.err(), Some(TransportError::RemoteAccessDisabled));
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn wss_tls_handshake_deadlines_release_the_admission_slot() {
    let (tls, pin) = tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        tls,
        MessageLimit::envelope(),
        AdmissionLimits::new(1, Duration::from_millis(100)).unwrap(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let _idle_peer = tokio::net::TcpStream::connect(address).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .err(),
        Some(TransportError::HandshakeExpired)
    );
    let (client, server) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            pin,
            MessageLimit::envelope(),
            Duration::from_secs(1)
        ),
        listener.accept()
    );
    client.unwrap();
    server.unwrap().finish_authentication().unwrap();
}

#[tokio::test]
async fn wss_can_send_during_a_pending_receive_and_close_wakes_it() {
    let (tls, pin) = tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        tls,
        MessageLimit::envelope(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (client, server) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            pin,
            MessageLimit::envelope(),
            Duration::from_secs(1)
        ),
        listener.accept()
    );
    let client = client.unwrap();
    let server = server.unwrap().finish_authentication().unwrap();
    let (received, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(server.recv(), async {
            server.send(b"duplex").await.unwrap();
            assert_eq!(client.recv().await.unwrap(), Some(b"duplex".to_vec()));
            server.close().await.unwrap();
        })
    })
    .await
    .unwrap();
    assert_eq!(received.unwrap(), None);
    assert_eq!(client.recv().await.unwrap(), None);
}

#[tokio::test]
async fn wss_receives_binary_after_ping_while_outbound_messages_are_backpressured() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let (server_tls, client_tls) = raw_tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        server_tls,
        MessageLimit::envelope(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (mut client, server) = tokio::join!(raw_client(address, client_tls), listener.accept());
    let server = server.unwrap().finish_authentication().unwrap();
    socket2::SockRef::from(client.get_ref().get_ref().0)
        .set_recv_buffer_size(4096)
        .unwrap();
    let message = vec![1; MessageLimit::envelope().bytes()];
    let mut outbound = Box::pin(async {
        for _ in 0..32 {
            server.send(&message).await.unwrap();
        }
    });
    // Poll, but do not cancel, the current send. The raw peer has not read any
    // envelopes, so more than its bounded socket/TLS buffers cannot complete.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut outbound)
            .await
            .is_err()
    );
    client
        .send(Message::Ping(b"probe".to_vec().into()))
        .await
        .unwrap();
    client
        .send(Message::Binary(b"input".to_vec().into()))
        .await
        .unwrap();
    let received = tokio::select! {
        () = &mut outbound => panic!("outbound traffic completed without the peer reading"),
        received = tokio::time::timeout(Duration::from_secs(1), server.recv()) => received,
    };
    // Release the active writer even on the red path, so the fixture cannot retain it.
    drop(outbound);
    server.close().await.unwrap();
    assert_eq!(received.unwrap().unwrap(), Some(b"input".to_vec()));
}

#[tokio::test]
async fn wss_replies_to_ping_without_an_application_send() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (server_tls, client_tls) = raw_tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        server_tls,
        MessageLimit::envelope(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (mut client, server) = tokio::join!(raw_client(address, client_tls), listener.accept());
    let server = server.unwrap().finish_authentication().unwrap();
    client
        .send(Message::Ping(b"probe".to_vec().into()))
        .await
        .unwrap();
    let mut receive = server.recv();
    let pong = tokio::select! {
        result = &mut receive => panic!("ping is not an application envelope: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(1), client.next()) => result.unwrap().unwrap().unwrap(),
    };
    assert_eq!(pong, Message::Pong(b"probe".to_vec().into()));
    drop(receive);
    server.close().await.unwrap();
}

#[tokio::test]
async fn an_ipv6_wildcard_enablement_does_not_also_bind_ipv4() {
    let (tls, _) = tls();
    let scope = RemoteAccessEnablement::from_durable_setting(
        [1; 16],
        vec![SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))],
    )
    .unwrap();
    let listener = WssListener::bind(Some(&scope), tls, MessageLimit::envelope(), admission())
        .await
        .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    assert!(
        tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, address.port()))
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, address.port()))
            .await
            .is_ok()
    );
    listener.disable().await;
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, address.port()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_enrolled_key_does_not_bypass_certificate_expiry() {
    let key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let pin = EnrolledServerKey(key.public_key_raw().try_into().unwrap());
    let mut parameters = CertificateParams::new(vec!["localhost".into()]).unwrap();
    parameters.not_before = rcgen::date_time_ymd(2000, 1, 1);
    parameters.not_after = rcgen::date_time_ymd(2001, 1, 1);
    let certificate = parameters.self_signed(&key).unwrap();
    let configuration = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .unwrap();
    let listener = WssListener::bind(
        Some(&enablement()),
        Arc::new(configuration),
        MessageLimit::envelope(),
        admission(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (client, _) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            pin,
            MessageLimit::envelope(),
            Duration::from_secs(1)
        ),
        listener.accept()
    );
    assert_eq!(client.err(), Some(TransportError::TlsIdentityRejected));
}

#[tokio::test]
async fn wss_application_handshake_deadlines_disconnect_idle_connections() {
    let (tls, pin) = tls();
    let listener = WssListener::bind(
        Some(&enablement()),
        tls,
        MessageLimit::envelope(),
        AdmissionLimits::new(1, Duration::from_millis(100)).unwrap(),
    )
    .await
    .unwrap();
    let address = listener.local_addresses().await.unwrap()[0];
    let (client, pending) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            pin,
            MessageLimit::envelope(),
            Duration::from_secs(1)
        ),
        listener.accept()
    );
    let client = client.unwrap();
    let pending = pending.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), client.recv())
            .await
            .unwrap()
            .unwrap(),
        None
    );
    assert_eq!(
        pending.finish_authentication().err(),
        Some(TransportError::HandshakeExpired)
    );
    let (second_client, second_server) = tokio::join!(
        WssTransport::connect(
            address,
            "localhost",
            pin,
            MessageLimit::envelope(),
            Duration::from_secs(1)
        ),
        listener.accept()
    );
    second_client.unwrap();
    second_server.unwrap().finish_authentication().unwrap();
}
