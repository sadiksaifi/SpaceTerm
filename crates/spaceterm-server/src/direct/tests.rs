use super::*;
use crate::connection::io::{Waker, spawn_link};
use crate::connection::link::{LinkError, RequestError, ServerLink};
use crate::connection::outbox::Outbox;
use crate::connection::quic::{QuicListener, QuicStream};
use crate::connection::service::{Dispatch, PaneService, Peer, SharedPaneService};
use crate::connection::sync::SyncWorker;
use crate::connection::unix::UnixCarrier;
use crate::protocol::{EffectDone, Input, Operation, Refusal, ReplyBody, RequestId, View};
use client::Messages;
use std::{
    net::UdpSocket,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// A service that refuses every operation.
pub(crate) struct Refusing;
impl PaneService for Refusing {
    fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
    fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
        Dispatch::reply(Err(Refusal::Unsupported))
    }
    fn input(&mut self, _: &Peer, _: Input) {}
    fn view(&mut self, _: &Peer, _: View) {}
    fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
    fn acknowledge(&mut self, _: &Peer, _: u64) {}
    fn disconnected(&mut self, _: &Peer) {}
    fn shutdown(&mut self) -> Result<(), ServerError> {
        Ok(())
    }
}

pub(crate) fn service(service: impl PaneService + 'static) -> SharedPaneService {
    Arc::new(Mutex::new(Box::new(service)))
}

/// A UDP port of `ip` that was free a moment ago.
pub(crate) fn free_udp(ip: std::net::IpAddr) -> std::net::SocketAddr {
    UdpSocket::bind((ip, 0)).unwrap().local_addr().unwrap()
}

/// Severs nothing, for registrations a test only inspects.
pub(crate) struct Quiet;
impl crate::connection::registry::Sever for Quiet {
    fn sever(&self) {}
}

/// Accepts the next QUIC connection of `state`'s listener and completes its handshake, for tests
/// that play the server's side of the pairing exchange by hand.
pub(crate) fn accept_raw(state: &host::Shared) -> QuicStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        if let Some((stream, _generation)) = state.lock().unwrap().accept() {
            break stream;
        }
        assert!(Instant::now() < deadline, "QUIC accept deadline");
        std::thread::sleep(Duration::from_millis(2));
    };
    stream.handshake(deadline).unwrap();
    stream
}

/// A listener of `state`'s certificate on a free loopback port, outside its policy.
pub(crate) fn listener(state: &host::Shared) -> (QuicListener, String) {
    let address = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let state = state.lock().unwrap();
    let (certificate, key) = state.certificate();
    let config = quic::server_config(certificate, key).unwrap();
    (
        QuicListener::bind(address, config).unwrap(),
        format!("quic://{address}"),
    )
}

/// Accepts the next connection of a test listener and completes its handshake.
pub(crate) fn accept_from(listener: &QuicListener) -> QuicStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        if let Some(stream) = listener.accept() {
            break stream;
        }
        assert!(Instant::now() < deadline, "QUIC accept deadline");
        std::thread::sleep(Duration::from_millis(2));
    };
    stream.handshake(deadline).unwrap();
    stream
}

/// Admits an authenticated connection as `client` and starts its link.
pub(crate) fn admitted(
    connection: DirectConnection,
    client: &[u8; 32],
    name: &str,
    _paired: &PairedDevice,
) -> Result<ServerLink, ServerError> {
    let carrier = connection.admit((client, &name.into()))?;
    Ok(spawn_link(carrier, Arc::new(|_| {})).unwrap())
}

/// Connects and admits a paired device under its own client identity.
pub(crate) fn connect(
    endpoint: &str,
    paired: &PairedDevice,
    credential: &DeviceCredential,
) -> ServerLink {
    let connection = DirectConnection::connect(endpoint, paired, credential).unwrap();
    admitted(connection, &paired.client, "Test", paired).unwrap()
}

pub(crate) fn request(link: &ServerLink, operation: Operation) -> Result<ReplyBody, RequestError> {
    link.request(operation).map_err(RequestError::Link)?.wait()
}

pub(crate) fn manage(
    link: &ServerLink,
    management: Management,
) -> Result<ManagementReply, DirectError> {
    match request(link, Operation::Direct(management)) {
        Ok(ReplyBody::Direct(reply)) => reply,
        other => panic!("unexpected reply {other:?}"),
    }
}

/// Waits until the server ends `link`, and returns why.
pub(crate) fn ended(link: &ServerLink) -> LinkError {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let crate::connection::link::Health::Closed(error) = link.health() {
            return error;
        }
        assert!(Instant::now() < deadline, "the server kept the connection");
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(crate) struct Fixture {
    root: std::path::PathBuf,
    pub(crate) paths: crate::host::Paths,
    pub(crate) state: host::Shared,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-direct-{}",
            u128::from_be_bytes(crypto::random().unwrap()[..16].try_into().unwrap())
        ));
        let paths = crate::host::Paths {
            identity: "spaceterm-development".into(),
            data: root.join("data"),
            runtime: root.join("runtime"),
        };
        crate::storage::Directory::open(&paths.data, true)
            .unwrap()
            .read_or_create()
            .unwrap();
        let state = host::State::open(&paths).unwrap();
        Self { root, paths, state }
    }
    pub(crate) fn management(&self, request: Management) -> Result<ManagementReply, DirectError> {
        self.state
            .lock()
            .unwrap()
            .manage(None, request)
            .map(|v| v.0)
    }
    pub(crate) fn enable(&self) {
        self.listen_at(std::net::Ipv4Addr::LOCALHOST.into());
    }
    /// Enables exposure on a free UDP port of `ip`. Another test can take the port between
    /// finding it and binding it, so a failed bind tries another.
    pub(crate) fn listen_at(&self, ip: std::net::IpAddr) -> std::net::SocketAddr {
        for _ in 0..8 {
            let address = free_udp(ip);
            self.management(Management::SetPolicy {
                enabled: true,
                policy: Some(NetworkPolicy::new(address).unwrap()),
            })
            .unwrap();
            let ManagementReply::Status(status) = self.management(Management::Status).unwrap()
            else {
                panic!()
            };
            if status.listener == (ListenerState::Listening { addr: address }) {
                return address;
            }
        }
        panic!("no free UDP port to listen on");
    }
    pub(super) fn invite(&self) -> Invitation {
        let ManagementReply::Invitation(text) = self.management(Management::Invite).unwrap() else {
            panic!()
        };
        Invitation::parse(&text).unwrap()
    }
    pub(super) fn authenticate(
        &self,
        credential: &DeviceCredential,
        action: wire::Action,
    ) -> Result<wire::Answer, DirectError> {
        let mut state = self.state.lock().unwrap();
        let challenge = state.challenge().unwrap();
        let proof = crypto::sign(
            credential.expose_bytes(),
            &wire::transcript("device", &challenge.pin, &challenge.nonce, &action).unwrap(),
        )
        .unwrap();
        state.authenticate(&challenge, &wire::Authentication { action, proof })
    }
    pub(super) fn claim(
        &self,
        invitation: &Invitation,
        credential: &DeviceCredential,
    ) -> PairingClaim {
        let answer = self
            .authenticate(
                credential,
                wire::Action::Claim {
                    id: Enrollment::prepare(invitation, "Test device".into(), credential)
                        .unwrap()
                        .claim_id(),
                    token: invitation.payload.token,
                    key: crypto::public(credential.expose_bytes()).unwrap(),
                    name: "Test device".into(),
                },
            )
            .unwrap();
        let wire::Answer::Claim(claim) = answer else {
            panic!()
        };
        let ManagementReply::Claims(claims) = self.management(Management::Claims).unwrap() else {
            panic!()
        };
        claims.into_iter().find(|c| c.id == claim.id).unwrap()
    }
    pub(crate) fn enroll(&self, credential: &DeviceCredential) -> PairedDevice {
        let invitation = self.invite();
        let claim = self.claim(&invitation, credential);
        self.management(Management::Approve(claim.clone())).unwrap();
        let wire::Answer::Enrollment(Some(paired)) = self
            .authenticate(
                credential,
                wire::Action::Finish {
                    claim: claim.id,
                    key: claim.device_public_key,
                },
            )
            .unwrap()
        else {
            panic!()
        };
        paired
    }
    pub(crate) fn serve_next(&self) -> std::thread::JoinHandle<Result<(), ServerError>> {
        self.serve_next_with(service(Refusing))
    }
    pub(crate) fn environment(&self, panes: SharedPaneService) -> crate::host::Environment {
        let (sync, _) = SyncWorker::spawn().unwrap();
        crate::host::Environment::for_tests(panes, sync, Some(Arc::clone(&self.state)))
    }
    pub(crate) fn serve_next_with(
        &self,
        panes: SharedPaneService,
    ) -> std::thread::JoinHandle<Result<(), ServerError>> {
        let state = Arc::clone(&self.state);
        let environment = self.environment(panes);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let accepted = state.lock().unwrap().accept();
                if let Some((stream, generation)) = accepted {
                    let (waker, wake) = Waker::new().unwrap();
                    return host::serve(stream, generation, state, &environment, waker, wake);
                }
                assert!(Instant::now() < deadline, "QUIC accept deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    }
    /// An account-local connection, already admitted, and its server thread.
    fn serve_local_with(
        &self,
        panes: SharedPaneService,
    ) -> (ServerLink, std::thread::JoinHandle<()>) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let environment = self.environment(panes);
        let worker = std::thread::spawn(move || {
            let (waker, wake) = Waker::new().unwrap();
            crate::host::serve_admitted(
                Box::new(UnixCarrier::new(server).unwrap()),
                Peer {
                    client: [9; 32],
                    connection: [10; 32],
                    pairing: None,
                },
                "Local".into(),
                Outbox::new(wake),
                waker,
                &environment,
            )
        });
        let link = spawn_link(
            Box::new(UnixCarrier::new(client).unwrap()),
            Arc::new(|_| {}),
        )
        .unwrap();
        (link, worker)
    }
    pub(crate) fn endpoint(&self) -> String {
        self.invite().endpoint().to_owned()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.lock().unwrap().stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn exposure_defaults_off_and_retains_only_the_explicit_listener_across_restart() {
    let fixture = Fixture::new();
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    assert_eq!((status.policy, status.listener), (None, ListenerState::Off));
    assert!(matches!(
        fixture.management(Management::Invite),
        Err(DirectError::Disabled)
    ));
    fixture.enable();
    let ManagementReply::Status(before) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    fixture.state.lock().unwrap().stop();
    let reopened = host::State::open(&fixture.paths).unwrap();
    let (ManagementReply::Status(after), _) = reopened
        .lock()
        .unwrap()
        .manage(None, Management::Status)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(before, after);
    reopened.lock().unwrap().stop();
}

#[test]
fn invitation_links_round_trip_the_endpoint_pin_secret_and_expiry() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let text = invitation.display_text();
    assert!(text.starts_with("spaceterm://pair/"));
    let parsed = Invitation::parse(&text).unwrap();
    assert_eq!(parsed.endpoint(), invitation.endpoint());
    assert_eq!(parsed.server_pin(), invitation.server_pin());
    assert_eq!(parsed.server_name(), invitation.server_name());
    assert_eq!(parsed.id(), invitation.id());
    assert_eq!(parsed.payload.token, invitation.payload.token);
    assert!(parsed.expires_at_unix_seconds() <= wire::now().unwrap() + 300);

    // Parsing a displayed link must preserve the capability, not mint another one.
    let credential = DeviceCredential::generate().unwrap();
    let claim = fixture.claim(&parsed, &credential);
    fixture.management(Management::Approve(claim)).unwrap();
    let other = DeviceCredential::generate().unwrap();
    assert!(matches!(
        fixture.authenticate(
            &other,
            wire::Action::Claim {
                id: Enrollment::prepare(&invitation, "Second device".into(), &other)
                    .unwrap()
                    .claim_id(),
                token: invitation.payload.token,
                key: crypto::public(other.expose_bytes()).unwrap(),
                name: "Second device".into(),
            }
        ),
        Err(DirectError::AlreadyDecided)
    ));
}

#[test]
fn malformed_links_cannot_change_the_transport_or_remove_the_invitation_secret() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    for (field, value) in [
        (
            "endpoint",
            serde_json::json!("ws://127.0.0.1:9231/spaceterm"),
        ),
        ("token", serde_json::json!(vec![0; 32])),
        ("valid_for_seconds", serde_json::json!(0)),
        ("valid_for_seconds", serde_json::json!(301)),
    ] {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let mut payload = serde_json::to_value(&invitation.payload).unwrap();
        payload[field] = value;
        let text = format!(
            "spaceterm://pair/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );
        assert!(
            Invitation::parse(&text).is_err(),
            "accepted changed {field}"
        );
    }
    for suffix in ["?redirect=elsewhere", "#secret", "/extra"] {
        assert!(
            Invitation::parse(&format!("{}{suffix}", invitation.display_text().as_str())).is_err()
        );
    }
}

#[test]
fn invalid_device_proof_cannot_consume_an_invitation_or_substitute_an_approval() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let action = wire::Action::Claim {
        id: Enrollment::prepare(&invitation, "Genuine".into(), &credential)
            .unwrap()
            .claim_id(),
        token: invitation.payload.token,
        key: crypto::public(credential.expose_bytes()).unwrap(),
        name: "Genuine".into(),
    };
    {
        let mut state = fixture.state.lock().unwrap();
        let challenge = state.challenge().unwrap();
        assert!(matches!(
            state.authenticate(
                &challenge,
                &wire::Authentication {
                    action,
                    proof: vec![0; 64]
                }
            ),
            Err(DirectError::AuthenticationRefused)
        ));
    }
    let claim = fixture.claim(&invitation, &credential);
    let mut wrong = claim.clone();
    wrong.device_public_key = [4; 32];
    assert!(matches!(
        fixture.management(Management::Approve(wrong)),
        Err(DirectError::Substituted)
    ));
    let mut wrong = claim.clone();
    wrong.server = [4; 32];
    assert!(matches!(
        fixture.management(Management::Approve(wrong)),
        Err(DirectError::WrongServer)
    ));
    fixture
        .management(Management::Approve(claim.clone()))
        .unwrap();
    assert!(matches!(
        fixture.management(Management::Approve(claim.clone())),
        Err(DirectError::AlreadyDecided)
    ));
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Claim {
                id: claim.id,
                token: invitation.payload.token,
                key: crypto::public(credential.expose_bytes()).unwrap(),
                name: "Replay".into()
            }
        ),
        Err(DirectError::Substituted)
    ));
}

#[test]
fn an_explicit_owner_rejection_crosses_quic_as_rejected_without_enrolling_the_device() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let server = fixture.serve_next();
    let mut enrollment =
        Enrollment::claim(&invitation, "Rejected device".into(), &credential).unwrap();
    server.join().unwrap().unwrap();
    let ManagementReply::Claims(claims) = fixture.management(Management::Claims).unwrap() else {
        panic!()
    };
    let claim = claims
        .into_iter()
        .find(|claim| claim.id == enrollment.claim_id())
        .unwrap();
    let server = fixture.serve_next();
    assert_eq!(enrollment.finish(), Ok(None));
    server.join().unwrap().unwrap();

    fixture.management(Management::Reject(claim.id)).unwrap();
    for _ in 0..2 {
        let server = fixture.serve_next();
        assert_eq!(enrollment.finish(), Err(DirectError::Rejected));
        server.join().unwrap().unwrap();
    }
    assert!(
        matches!(fixture.management(Management::Claims), Ok(ManagementReply::Claims(claims)) if claims.is_empty())
    );
    assert!(
        matches!(fixture.management(Management::Devices), Ok(ManagementReply::Devices(devices)) if devices.is_empty())
    );
    assert!(matches!(
        fixture.management(Management::Approve(claim)),
        Err(DirectError::AlreadyDecided)
    ));
}

#[test]
fn withdraw_and_signed_cancel_prevent_late_approval_of_the_original_claim() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let invitation = fixture.invite();
    let claim = fixture.claim(&invitation, &credential);
    fixture
        .management(Management::Withdraw(invitation.id()))
        .unwrap();
    assert!(matches!(
        fixture.management(Management::Approve(claim)),
        Err(DirectError::AlreadyDecided)
    ));
    let invitation = fixture.invite();
    let claim = fixture.claim(&invitation, &credential);
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Cancel {
                claim: claim.id,
                key: claim.device_public_key
            }
        ),
        Ok(wire::Answer::Cancelled(None))
    ));
    assert!(matches!(
        fixture.management(Management::Approve(claim)),
        Err(DirectError::AlreadyDecided)
    ));
}

#[test]
fn prepared_enrollment_cancels_before_submission_and_fences_every_late_retry() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    // No accept worker exists: preparation must perform no network operation.
    let mut enrollment =
        Enrollment::prepare(&invitation, "Prepared device".into(), &credential).unwrap();
    let id = enrollment.claim_id();
    assert!(
        matches!(fixture.management(Management::Claims), Ok(ManagementReply::Claims(claims)) if claims.is_empty())
    );

    // Another signing key cannot cancel this prepared original or consume its invitation.
    let stranger = DeviceCredential::generate().unwrap();
    assert!(matches!(
        fixture.authenticate(
            &stranger,
            wire::Action::Cancel {
                claim: id,
                key: crypto::public(stranger.expose_bytes()).unwrap(),
            }
        ),
        Err(DirectError::Expired)
    ));
    let mut cleanup = Enrollment::resume(
        invitation.endpoint(),
        invitation.server_pin().clone(),
        id,
        &credential,
    )
    .unwrap();
    assert_eq!(cleanup.submit(), Err(DirectError::InvalidRequest));
    let server = fixture.serve_next();
    assert_eq!(cleanup.cancel(), Ok(None));
    server.join().unwrap().unwrap();
    for _ in 0..2 {
        let server = fixture.serve_next();
        assert_eq!(enrollment.submit(), Err(DirectError::AlreadyDecided));
        server.join().unwrap().unwrap();
        let server = fixture.serve_next();
        assert_eq!(cleanup.cancel(), Ok(None));
        server.join().unwrap().unwrap();
    }
    assert!(
        matches!(fixture.management(Management::Claims), Ok(ManagementReply::Claims(claims)) if claims.is_empty())
    );
    assert!(
        matches!(fixture.management(Management::Devices), Ok(ManagementReply::Devices(devices)) if devices.is_empty())
    );
}

#[test]
fn both_devices_compare_the_first_authenticated_exchange_across_exact_retries() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let mut enrollment =
        Enrollment::prepare(&invitation, "Joining device".into(), &credential).unwrap();
    assert_eq!(enrollment.comparison_code(), None);
    let server = fixture.serve_next();
    let code = enrollment.submit().unwrap();
    server.join().unwrap().unwrap();
    let ManagementReply::Claims(claims) = fixture.management(Management::Claims).unwrap() else {
        panic!()
    };
    let claim = &claims[0];
    assert_eq!(Some(code), enrollment.comparison_code());
    assert_eq!(code, claim.comparison_code());
    assert_eq!(code.to_string().len(), 6);
    assert!(code.to_string().bytes().all(|byte| byte.is_ascii_digit()));

    // A new server nonce on retry must not change what the owner is approving.
    let server = fixture.serve_next();
    assert_eq!(enrollment.submit(), Ok(code));
    server.join().unwrap().unwrap();
    let mut changed = claim.clone();
    changed.exchange_hash[0] ^= 1;
    assert!(matches!(
        fixture.management(Management::Approve(changed)),
        Err(DirectError::Substituted)
    ));
    fixture
        .management(Management::Approve(claim.clone()))
        .unwrap();
    let server = fixture.serve_next();
    assert!(enrollment.finish().unwrap().is_some());
    server.join().unwrap().unwrap();
}

#[test]
fn a_pinned_enrollment_reply_cannot_substitute_the_invitation_identity() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let other = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let mut enrollment =
        Enrollment::prepare(&invitation, "Joining device".into(), &credential).unwrap();
    let state = Arc::clone(&fixture.state);
    let substituted = other.id();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let _ = deadline;
        let mut ws = accept_raw(&state);
        let challenge = state.lock().unwrap().challenge().unwrap();
        ws.send(&challenge).unwrap();
        let authentication: wire::Authentication = ws.receive().unwrap();
        let wire::Answer::Claim(mut claim) = state
            .lock()
            .unwrap()
            .authenticate(&challenge, &authentication)
            .unwrap()
        else {
            panic!()
        };
        // Change only the response at the network boundary. The signed claim,
        // exchange hash, device identity and original deadline remain genuine.
        claim.invitation_id = substituted;
        ws.send(&Ok::<_, DirectError>(wire::Answer::Claim(claim)))
            .unwrap();
        let _ = ws.finish(Instant::now() + Duration::from_secs(5));
    });
    assert_eq!(enrollment.submit(), Err(DirectError::Substituted));
    server.join().unwrap();
    assert_eq!(enrollment.comparison_code(), None);
}

#[test]
fn losing_the_first_reply_still_compares_the_original_authenticated_exchange() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let mut enrollment =
        Enrollment::prepare(&invitation, "Original device".into(), &credential).unwrap();
    let state = Arc::clone(&fixture.state);
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let _ = deadline;
        let mut ws = accept_raw(&state);
        let challenge = state.lock().unwrap().challenge().unwrap();
        ws.send(&challenge).unwrap();
        let authentication: wire::Authentication = ws.receive().unwrap();
        let wire::Answer::Claim(claim) = state
            .lock()
            .unwrap()
            .authenticate(&challenge, &authentication)
            .unwrap()
        else {
            panic!()
        };
        // Lose only the reply at the network boundary, after real authentication.
        drop(ws);
        claim
    });
    assert_eq!(enrollment.submit(), Err(DirectError::Transport));
    let original = server.join().unwrap();
    assert_eq!(enrollment.comparison_code(), None);
    let server = fixture.serve_next();
    assert_eq!(enrollment.submit(), Ok(original.comparison_code()));
    server.join().unwrap().unwrap();
    fixture.management(Management::Approve(original)).unwrap();
    let server = fixture.serve_next();
    assert!(enrollment.finish().unwrap().is_some());
    server.join().unwrap().unwrap();
}

#[test]
fn a_returned_comparison_must_belong_to_an_exchange_this_enrollment_signed() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    // Another live enrollment with this key claimed first. Its nonce and proofs
    // are not evidence observed by this independently prepared enrollment.
    let original = fixture.claim(&invitation, &credential);
    let mut other = Enrollment::prepare(&invitation, "Test device".into(), &credential).unwrap();
    assert_eq!(other.claim_id(), original.id);
    let server = fixture.serve_next();
    assert_eq!(other.submit(), Err(DirectError::Substituted));
    server.join().unwrap().unwrap();
    assert_eq!(other.comparison_code(), None);
}

#[test]
fn prepared_submission_replays_only_the_same_claim_key_and_name_after_a_lost_reply() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let mut enrollment =
        Enrollment::prepare(&invitation, "Original device".into(), &credential).unwrap();
    let id = enrollment.claim_id();
    for _ in 0..2 {
        let server = fixture.serve_next();
        enrollment.submit().unwrap();
        server.join().unwrap().unwrap();
    }
    let mut changed =
        Enrollment::prepare(&invitation, "Changed device".into(), &credential).unwrap();
    assert_eq!(changed.claim_id(), id);
    let server = fixture.serve_next();
    assert_eq!(changed.submit(), Err(DirectError::Substituted));
    server.join().unwrap().unwrap();
    let key = crypto::public(credential.expose_bytes()).unwrap();
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Claim {
                id: [99; 32],
                token: invitation.payload.token,
                key,
                name: "Original device".into(),
            }
        ),
        Err(DirectError::Substituted)
    ));
    let ManagementReply::Claims(claims) = fixture.management(Management::Claims).unwrap() else {
        panic!()
    };
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].id, id);
    fixture
        .management(Management::Approve(claims[0].clone()))
        .unwrap();
    let server = fixture.serve_next();
    let paired = enrollment.finish().unwrap().unwrap();
    server.join().unwrap().unwrap();
    let server = fixture.serve_next();
    let receipt = enrollment.cancel().unwrap().unwrap();
    server.join().unwrap().unwrap();
    assert_eq!(receipt.target, paired.pairing);
}

#[test]
fn prepared_claim_identity_binds_every_server_pin_field_and_the_signing_key() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let key = crypto::public(credential.expose_bytes()).unwrap();
    let digest = crypto::hash(&invitation.payload.token);
    let pin = invitation.server_pin();
    let original = wire::claim_id(pin, invitation.id(), digest, key).unwrap();
    let mut pins = vec![pin.clone(); 4];
    pins[0].build = "spaceterm-preflight".into();
    pins[1].server[0] ^= 1;
    pins[2].public_key[0] ^= 1;
    pins[3].certificate[0] ^= 1;
    for changed in pins {
        assert_ne!(
            wire::claim_id(&changed, invitation.id(), digest, key).unwrap(),
            original
        );
    }
    assert_ne!(
        wire::claim_id(pin, invitation.id(), digest, [99; 32]).unwrap(),
        original
    );
    assert_ne!(
        wire::claim_id(pin, [99; 32], digest, key).unwrap(),
        original
    );
    assert_ne!(
        wire::claim_id(pin, invitation.id(), [99; 32], key).unwrap(),
        original
    );
}

#[test]
fn approved_enrollment_and_cancellation_resolve_the_same_pairing_after_restart() {
    let mut fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let invitation = fixture.invite();
    let claim = fixture.claim(&invitation, &credential);
    let ManagementReply::Approved(info) = fixture
        .management(Management::Approve(claim.clone()))
        .unwrap()
    else {
        panic!()
    };
    fixture.state.lock().unwrap().stop();
    fixture.state = host::State::open(&fixture.paths).unwrap();
    let wire::Answer::Enrollment(Some(paired)) = fixture
        .authenticate(
            &credential,
            wire::Action::Finish {
                claim: claim.id,
                key: claim.device_public_key,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(paired.pairing, info.pairing);
    let newer = fixture.enroll(&credential);
    let wire::Answer::Cancelled(Some(original)) = fixture
        .authenticate(
            &credential,
            wire::Action::Cancel {
                claim: claim.id,
                key: claim.device_public_key,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    let wire::Answer::Cancelled(Some(repeated)) = fixture
        .authenticate(
            &credential,
            wire::Action::Cancel {
                claim: claim.id,
                key: claim.device_public_key,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(original, repeated);
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Connect {
                paired: newer.clone()
            }
        ),
        Ok(wire::Answer::Connected)
    ));
    let ManagementReply::Devices(devices) = fixture.management(Management::Devices).unwrap() else {
        panic!()
    };
    assert_eq!(
        devices
            .iter()
            .map(|device| device.pairing)
            .collect::<Vec<_>>(),
        vec![newer.pairing]
    );
    assert!(matches!(
        fixture.authenticate(&credential, wire::Action::Connect { paired }),
        Err(DirectError::Revoked)
    ));
}

#[test]
fn a_revoked_credential_can_repeat_only_its_exact_original_without_affecting_a_new_pairing() {
    let mut fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let old = fixture.enroll(&credential);
    let other = fixture.enroll(&credential);
    let before = std::fs::read(fixture.paths.data.join("direct.json")).unwrap();
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Revoke {
                actor: old.clone(),
                operation: crypto::random().unwrap(),
                target: other.pairing,
            }
        ),
        Err(DirectError::AuthenticationRefused)
    ));
    assert_eq!(
        before,
        std::fs::read(fixture.paths.data.join("direct.json")).unwrap()
    );
    let operation = crypto::random().unwrap();
    let wire::Answer::Revoked(original) = fixture
        .authenticate(
            &credential,
            wire::Action::Revoke {
                actor: old.clone(),
                operation,
                target: old.pairing,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    fixture.state.lock().unwrap().stop();
    fixture.state = host::State::open(&fixture.paths).unwrap();
    let new = fixture.enroll(&credential);
    assert_ne!(old.client, new.client);
    let wire::Answer::Revoked(repeated) = fixture
        .authenticate(
            &credential,
            wire::Action::Revoke {
                actor: old.clone(),
                operation,
                target: old.pairing,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(original, repeated);
    let before = std::fs::read(fixture.paths.data.join("direct.json")).unwrap();
    for _ in 0..8 {
        assert!(matches!(
            fixture.authenticate(
                &credential,
                wire::Action::Revoke {
                    actor: old.clone(),
                    operation: crypto::random().unwrap(),
                    target: old.pairing
                }
            ),
            Ok(wire::Answer::Revoked(_))
        ));
    }
    assert!(
        before == std::fs::read(fixture.paths.data.join("direct.json")).unwrap(),
        "retired self-confirmation must not allocate ledger entries"
    );
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Revoke {
                actor: old,
                operation,
                target: new.pairing
            }
        ),
        Err(DirectError::AuthenticationRefused)
    ));
    assert!(matches!(
        fixture.authenticate(&credential, wire::Action::Connect { paired: new }),
        Ok(wire::Answer::Connected)
    ));
}

#[test]
fn only_a_fully_admitted_device_updates_its_retained_last_connection_time() {
    fn devices(fixture: &Fixture) -> Vec<DeviceInfo> {
        let ManagementReply::Devices(devices) = fixture.management(Management::Devices).unwrap()
        else {
            panic!()
        };
        devices
    }
    let mut fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let target = fixture.enroll(&credential);
    // Reusing a key on a separate pairing must not share connection metadata.
    let untouched = fixture.enroll(&credential);
    let mut expected = devices(&fixture);
    assert!(
        expected
            .iter()
            .all(|device| device.last_connected_at_unix_seconds.is_none())
    );
    let endpoint = fixture.endpoint();
    let wrong_key = DeviceCredential::generate().unwrap();
    let server = fixture.serve_next();
    assert!(matches!(
        DirectConnection::connect(&endpoint, &target, &wrong_key),
        Err(DirectError::AuthenticationRefused)
    ));
    server.join().unwrap().unwrap();

    let server = fixture.serve_next();
    let connection = DirectConnection::connect(&endpoint, &target, &credential).unwrap();
    assert!(admitted(connection, &untouched.client, "Spoofed", &target).is_err());
    assert_eq!(
        server.join().unwrap(),
        Err(ServerError::AuthenticationRefused)
    );
    assert_eq!(devices(&fixture), expected);

    let before = wire::now().unwrap();
    let server = fixture.serve_next();
    let connection = connect(&endpoint, &target, &credential);
    let Ok(ManagementReply::Devices(observed)) = manage(&connection, Management::Devices) else {
        panic!()
    };
    let timestamp = observed
        .iter()
        .find(|device| device.pairing == target.pairing)
        .unwrap()
        .last_connected_at_unix_seconds
        .unwrap();
    assert!((before..=wire::now().unwrap()).contains(&timestamp));
    expected
        .iter_mut()
        .find(|device| device.pairing == target.pairing)
        .unwrap()
        .last_connected_at_unix_seconds = Some(timestamp);
    assert_eq!(observed, expected);
    drop(connection);
    let _ = server.join().unwrap();
    fixture.state.lock().unwrap().stop();
    fixture.state = host::State::open(&fixture.paths).unwrap();
    assert_eq!(devices(&fixture), expected);
}

#[test]
fn quic_validates_the_actual_pin_and_refuses_a_spoofed_terminal_client_identity() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    let server = fixture.serve_next();
    let mut wrong = paired.clone();
    wrong.pin.certificate[0] ^= 1;
    assert!(matches!(
        DirectConnection::connect(&endpoint, &wrong, &credential),
        Err(DirectError::PinMismatch)
    ));
    assert!(server.join().unwrap().is_err());
    let server = fixture.serve_next();
    let connection = DirectConnection::connect(&endpoint, &paired, &credential).unwrap();
    assert!(admitted(connection, &[9; 32], "Spoofed", &paired).is_err());
    assert_eq!(
        server.join().unwrap(),
        Err(ServerError::AuthenticationRefused)
    );
    let server = fixture.serve_next();
    let connection = connect(&endpoint, &paired, &credential);
    assert!(matches!(
        manage(&connection, Management::Status),
        Ok(ManagementReply::Status(_))
    ));
    drop(connection);
    let _ = server.join().unwrap();
}

#[test]
fn an_unadmitted_peer_cannot_send_a_message_larger_than_the_authentication_bound() {
    let fixture = Fixture::new();
    let (listener, endpoint) = listener(&fixture.state);
    let pin = fixture.state.lock().unwrap().pin.clone();
    let challenge = fixture.state.lock().unwrap().challenge();
    let server = std::thread::spawn(move || {
        let mut stream = accept_from(&listener);
        // The client accepts L0 once the server first writes on it.
        let _ = stream.send(&challenge.ok());
        stream.receive::<serde_json::Value>()
    });
    let credential = DeviceCredential::generate().unwrap();
    let mut stream = quic::dial(&endpoint, &pin, &credential).unwrap();
    let oversized = (wire::MAX_AUTH_FRAME as u32 + 1).to_be_bytes();
    let deadline = Instant::now() + Duration::from_secs(5);
    stream.send_bytes(&oversized, deadline).unwrap();
    stream
        .send_bytes(&vec![b'1'; wire::MAX_AUTH_FRAME + 1], deadline)
        .unwrap();
    assert_eq!(server.join().unwrap().err(), Some(DirectError::Transport));
}

#[test]
fn the_quic_carrier_moves_frames_larger_than_its_buffers_in_order() {
    use crate::connection::carrier::{Carrier, Offer, Priority};
    let fixture = Fixture::new();
    let (listener, endpoint) = listener(&fixture.state);
    let pin = fixture.state.lock().unwrap().pin.clone();
    // Whole frames of the largest size.
    let frames: Vec<Vec<u8>> = (0..3u8)
        .map(|index| {
            let mut frame = vec![0; crate::protocol::limits::MAX_FRAME_BYTES];
            let len = (frame.len() - 4) as u32;
            frame[..4].copy_from_slice(&len.to_be_bytes());
            frame[4] = crate::protocol::Kind::Frame as u8;
            frame[5..].fill(index);
            frame
        })
        .collect();
    let expected = frames.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_from(&listener);
        stream.send(&"open").unwrap();
        let mut carrier = stream.into_carrier();
        let mut queue = frames.into_iter();
        let mut held = queue.next();
        let deadline = Instant::now() + Duration::from_secs(10);
        while held.is_some() || carrier.flush().unwrap().pending {
            if let Some(frame) = &held
                && carrier.offer(frame, Priority::L1).unwrap() == Offer::Accepted
            {
                held = queue.next();
                continue;
            }
            carrier.flush().unwrap();
            let readiness = carrier.readiness();
            let until = readiness.deadline.map_or(deadline, |at| at.min(deadline));
            crate::connection::io::wait_fd(readiness.read, false, until);
            let _ = carrier.receive();
            assert!(Instant::now() < deadline);
        }
        carrier.close();
        while carrier.flush().unwrap().pending && Instant::now() < deadline {
            let readiness = carrier.readiness();
            let until = readiness.deadline.map_or(deadline, |at| at.min(deadline));
            crate::connection::io::wait_fd(readiness.read, false, until);
        }
    });
    let credential = DeviceCredential::generate().unwrap();
    let mut stream = quic::dial(&endpoint, &pin, &credential).unwrap();
    assert_eq!(stream.receive::<String>().unwrap(), "open");
    let mut carrier = stream.into_carrier();
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while received.len() < expected.len() {
        match carrier.receive().unwrap() {
            Some(frame) => {
                let mut bytes = ((frame.body.len() + 1) as u32).to_be_bytes().to_vec();
                bytes.push(frame.kind);
                bytes.extend_from_slice(&frame.body);
                received.push(bytes);
            }
            None => {
                carrier.flush().unwrap();
                let readiness = carrier.readiness();
                let until = readiness.deadline.map_or(deadline, |at| at.min(deadline));
                crate::connection::io::wait_fd(readiness.read, false, until);
                assert!(Instant::now() < deadline);
            }
        }
    }
    assert!(received == expected);
    server.join().unwrap();
}

#[test]
fn a_valid_tls_pin_does_not_replace_validation_of_the_servers_public_claim_proof() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let (listener, endpoint) = listener(&fixture.state);
    let mut challenge = fixture.state.lock().unwrap().challenge().unwrap();
    challenge.proof[0] ^= 1;
    let server = std::thread::spawn(move || {
        let mut stream = accept_from(&listener);
        stream.send(&challenge).unwrap();
        assert!(
            stream.receive::<wire::Authentication>().is_err(),
            "the client must refuse before supplying a device proof"
        );
    });
    assert!(matches!(
        DirectConnection::connect(&endpoint, &paired, &credential),
        Err(DirectError::AuthenticationRefused)
    ));
    server.join().unwrap();
}

#[test]
fn a_retired_exposure_generation_refuses_inflight_proof_and_late_registration_after_reenable() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let challenge = fixture.state.lock().unwrap().challenge().unwrap();
    let outbox = Outbox::new(Arc::new(|| {}));
    fixture
        .state
        .lock()
        .unwrap()
        .register(
            &paired,
            challenge.generation,
            [5; 32],
            outbox.clone(),
            Box::new(Quiet),
        )
        .unwrap();
    let action = wire::Action::Connect {
        paired: paired.clone(),
    };
    let proof = crypto::sign(
        credential.expose_bytes(),
        &wire::transcript("device", &paired.pin, &challenge.nonce, &action).unwrap(),
    )
    .unwrap();
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    fixture
        .management(Management::SetPolicy {
            enabled: true,
            policy: status.policy,
        })
        .unwrap();
    let mut state = fixture.state.lock().unwrap();
    assert!(
        outbox.is_closing(),
        "a request already read from the retired peer is never dispatched under the replacement policy"
    );
    assert!(matches!(
        state.authenticate(&challenge, &wire::Authentication { action, proof }),
        Err(DirectError::Disabled)
    ));
    assert_eq!(
        state.register(
            &paired,
            challenge.generation,
            [6; 32],
            Outbox::new(Arc::new(|| {})),
            Box::new(Quiet)
        ),
        Err(DirectError::Disabled)
    );
    assert_eq!(
        state.authorized(&paired),
        Ok(()),
        "the pairing remains valid under new explicit exposure"
    );
}

#[test]
fn a_connection_accepted_before_policy_replacement_cannot_join_the_new_listener() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    let client =
        std::thread::spawn(move || DirectConnection::connect(&endpoint, &paired, &credential));
    let deadline = Instant::now() + Duration::from_secs(5);
    let (stream, generation) = loop {
        if let Some(accepted) = fixture.state.lock().unwrap().accept() {
            break accepted;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    fixture
        .management(Management::SetPolicy {
            enabled: true,
            policy: status.policy,
        })
        .unwrap();
    let environment = fixture.environment(service(Refusing));
    let (waker, wake) = Waker::new().unwrap();
    assert_eq!(
        host::serve(
            stream,
            generation,
            Arc::clone(&fixture.state),
            &environment,
            waker,
            wake
        ),
        Err(ServerError::Direct(DirectError::Disabled))
    );
    assert!(client.join().unwrap().is_err());
}

#[test]
fn revocation_closes_every_authorized_connection_and_notifies_only_the_target_runtime_identity() {
    struct Runtime(Arc<Mutex<Vec<[u8; 32]>>>);
    impl PaneService for Runtime {
        fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
        fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
            Dispatch::reply(Ok(ReplyBody::Done))
        }
        fn input(&mut self, _: &Peer, _: Input) {}
        fn view(&mut self, _: &Peer, _: View) {}
        fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
        fn acknowledge(&mut self, _: &Peer, _: u64) {}
        fn revoked(&mut self, client: [u8; 32]) {
            self.0.lock().unwrap().push(client);
        }
        fn disconnected(&mut self, _: &Peer) {}
        fn shutdown(&mut self) -> Result<(), ServerError> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    fixture.enable();
    let target_key = DeviceCredential::generate().unwrap();
    let target = fixture.enroll(&target_key);
    let actor_key = DeviceCredential::generate().unwrap();
    let actor = fixture.enroll(&actor_key);
    let endpoint = fixture.endpoint();
    let retired = Arc::new(Mutex::new(Vec::new()));
    let runtime = service(Runtime(Arc::clone(&retired)));
    let one = fixture.serve_next_with(Arc::clone(&runtime));
    let first = connect(&endpoint, &target, &target_key);
    let two = fixture.serve_next_with(Arc::clone(&runtime));
    let second = connect(&endpoint, &target, &target_key);
    let three = fixture.serve_next_with(Arc::clone(&runtime));
    let other = connect(&endpoint, &actor, &actor_key);
    assert!(matches!(
        manage(
            &other,
            Management::Revoke {
                operation: crypto::random().unwrap(),
                target: target.pairing,
            }
        ),
        Err(DirectError::AuthenticationRefused)
    ));
    assert!(matches!(
        manage(&other, Management::Status),
        Ok(ManagementReply::Status(_))
    ));
    assert!(retired.lock().unwrap().is_empty());
    let (local, local_worker) = fixture.serve_local_with(Arc::clone(&runtime));
    assert!(matches!(
        manage(
            &local,
            Management::Revoke {
                operation: crypto::random().unwrap(),
                target: target.pairing,
            }
        ),
        Ok(ManagementReply::Revoked(_))
    ));
    drop(local);
    local_worker.join().unwrap();
    assert_eq!(*retired.lock().unwrap(), vec![target.client]);
    // The live connections of the revoked pairing end at once with a typed goodbye.
    for link in [&first, &second] {
        assert_eq!(
            ended(link),
            LinkError::Goodbye(crate::protocol::Fatal::Revoked)
        );
    }
    assert_eq!(other.health(), crate::connection::link::Health::Live);
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    assert_eq!(
        ended(&other),
        LinkError::Goodbye(crate::protocol::Fatal::Revoked)
    );
    drop((first, second, other));
    for server in [one, two, three] {
        let _ = server.join().unwrap();
    }
    let server = fixture.serve_next();
    // Re-enable only by explicit account authority; revoked credentials still cannot join.
    fixture.enable();
    let endpoint = fixture.endpoint();
    assert!(matches!(
        DirectConnection::connect(&endpoint, &target, &target_key),
        Err(DirectError::Revoked)
    ));
    let _ = server.join().unwrap();
}

#[test]
fn a_paired_policy_request_is_refused_without_mutation_or_retiring_its_connection() {
    for change_address in [false, true] {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next();
        let connection = connect(&fixture.endpoint(), &paired, &credential);
        let policy = change_address
            .then(|| NetworkPolicy::new(free_udp(std::net::Ipv4Addr::LOCALHOST.into())).unwrap());
        // The admission reply precedes the retained connection timestamp. A server request
        // completes after that metadata write and fences this baseline.
        assert!(matches!(
            manage(&connection, Management::Status),
            Ok(ManagementReply::Status(_))
        ));
        let before = std::fs::read(fixture.paths.data.join("direct.json")).unwrap();
        // Refusal leaves this connection usable for the request already in flight behind it.
        let refused = connection
            .request(Operation::Direct(Management::SetPolicy {
                enabled: policy.is_some(),
                policy,
            }))
            .unwrap();
        let next = connection
            .request(Operation::Direct(Management::Status))
            .unwrap();
        assert!(matches!(
            refused.wait(),
            Ok(ReplyBody::Direct(Err(DirectError::AuthenticationRefused)))
        ));
        assert!(matches!(
            next.wait(),
            Ok(ReplyBody::Direct(Ok(ManagementReply::Status(_))))
        ));
        assert!(
            before == std::fs::read(fixture.paths.data.join("direct.json")).unwrap(),
            "a refused policy request must leave the complete retained record unchanged"
        );
        drop(connection);
        let _ = server.join().unwrap();
    }
}

#[test]
fn a_paired_protocol_connection_cannot_write_native_stop_policy() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let server = fixture.serve_next();
    let connection = connect(&fixture.endpoint(), &paired, &credential);
    assert!(matches!(
        request(&connection, Operation::Shutdown),
        Err(RequestError::Refused(Refusal::Unsupported))
    ));
    assert!(!fixture.paths.data.join("service.json").exists());
    assert!(matches!(
        manage(&connection, Management::Status),
        Ok(ManagementReply::Status(_))
    ));
    drop(connection);
    let _ = server.join().unwrap();
}

fn setup(
    fixture: &Fixture,
    mut operation: OperationId,
    credential: &DeviceCredential,
) -> Management {
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    let policy = status.policy.clone().unwrap_or_else(|| {
        NetworkPolicy::new(free_udp(std::net::Ipv4Addr::LOCALHOST.into())).unwrap()
    });
    operation[..8].copy_from_slice(&status.setup_generation.to_be_bytes());
    Management::Setup {
        generation: status.setup_generation,
        pin: status.pin,
        expected_policy: status.policy,
        expected_enabled: status.enabled,
        endpoint: format!("quic://{}", policy.bind),
        policy,
        operation,
        public_key: credential.public_key().unwrap(),
        name: "Setup device".into(),
    }
}
fn reserve(fixture: &Fixture, request: &Management) {
    let Management::Setup {
        generation,
        pin,
        expected_policy,
        expected_enabled,
        endpoint,
        policy,
        operation,
        public_key,
        name,
    } = request.clone()
    else {
        panic!()
    };
    assert!(matches!(
        fixture.management(Management::PrepareSetup {
            generation,
            pin,
            expected_policy,
            expected_enabled,
            endpoint,
            policy,
            operation,
            public_key,
            name
        }),
        Ok(ManagementReply::SetupPrepared)
    ));
}

#[test]
fn prepared_setup_can_be_cancelled_over_quic_before_admission_and_after_restart() {
    let fixture = Fixture::new();
    fixture.enable();
    let endpoint = fixture.endpoint();
    let credential = DeviceCredential::generate().unwrap();
    let request = setup(&fixture, [90; 32], &credential);
    reserve(&fixture, &request);
    let Management::Setup { pin, operation, .. } = &request else {
        panic!()
    };
    let mut cleanup = Enrollment::resume(&endpoint, pin.clone(), *operation, &credential).unwrap();
    let worker = fixture.serve_next();
    assert_eq!(cleanup.cancel().unwrap(), None);
    worker.join().unwrap().unwrap();
    fixture.state.lock().unwrap().stop();
    let reopened = host::State::open(&fixture.paths).unwrap();
    let result = reopened.lock().unwrap().manage(None, request);
    assert!(matches!(result, Err(DirectError::AlreadyDecided)));
    assert!(
        matches!(reopened.lock().unwrap().manage(None, Management::Devices), Ok((ManagementReply::Devices(devices), _)) if devices.is_empty())
    );
    reopened.lock().unwrap().stop();
}

#[test]
fn account_setup_cancellation_checks_the_original_pin_and_fences_admission_with_network_off() {
    let fixture = Fixture::new();
    let credential = DeviceCredential::generate().unwrap();
    let operation = [95; 32];
    let request = setup(&fixture, operation, &credential);
    reserve(&fixture, &request);
    let Management::Setup { pin, operation, .. } = &request else {
        panic!()
    };
    let operation = *operation;
    let mut changed = pin.clone();
    changed.certificate[0] ^= 1;
    assert!(matches!(
        fixture.management(Management::CancelSetup {
            pin: changed,
            operation,
        }),
        Err(DirectError::PinMismatch)
    ));
    // A refusal cannot retire or alter the original prepared operation.
    reserve(&fixture, &request);
    for _ in 0..2 {
        assert!(matches!(
            fixture.management(Management::CancelSetup {
                pin: pin.clone(),
                operation,
            }),
            Ok(ManagementReply::SetupCancelled {
                pin: confirmed,
                operation: confirmed_operation,
            }) if confirmed == *pin && confirmed_operation == operation
        ));
    }
    fixture.state.lock().unwrap().stop();
    let reopened = host::State::open(&fixture.paths).unwrap();
    assert!(matches!(
        reopened.lock().unwrap().manage(None, request),
        Err(DirectError::AlreadyDecided)
    ));
    assert!(matches!(
        reopened.lock().unwrap().manage(None, Management::Status),
        Ok((ManagementReply::Status(status), _))
            if !status.enabled && status.listener == ListenerState::Off
    ));
    reopened.lock().unwrap().stop();
}

#[test]
fn setup_retries_recover_one_pairing_after_lost_reply_and_reject_substituted_originals() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let request = setup(&fixture, [91; 32], &credential);
    assert!(matches!(
        fixture.management(request.clone()),
        Err(DirectError::InvalidRequest)
    ));
    reserve(&fixture, &request);
    let ManagementReply::Setup(paired) = fixture.management(request.clone()).unwrap() else {
        panic!()
    };
    fixture.state.lock().unwrap().stop();
    let reopened = host::State::open(&fixture.paths).unwrap();
    let (ManagementReply::Setup(retried), _) = reopened
        .lock()
        .unwrap()
        .manage(None, request.clone())
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(retried, paired);
    assert!(
        matches!(reopened.lock().unwrap().manage(None, Management::Devices), Ok((ManagementReply::Devices(devices), _)) if devices.len() == 1)
    );
    for changed_field in 0..7 {
        let mut changed = request.clone();
        let Management::Setup {
            pin,
            expected_policy,
            expected_enabled,
            endpoint,
            policy,
            public_key,
            name,
            ..
        } = &mut changed
        else {
            panic!()
        };
        match changed_field {
            0 => pin.server[0] ^= 1,
            1 => policy
                .bind
                .set_port(if policy.bind.port() == 1 { 2 } else { 1 }),
            2 => public_key[0] ^= 1,
            3 => *name = "Another client".into(),
            4 => *expected_policy = None,
            5 => *expected_enabled = !*expected_enabled,
            _ => *endpoint = format!("quic://another.example:{}", policy.bind.port()),
        }
        assert!(matches!(
            reopened.lock().unwrap().manage(None, changed),
            Err(DirectError::PinMismatch | DirectError::Substituted)
        ));
    }
    reopened.lock().unwrap().stop();
}

#[test]
fn setup_requires_the_observed_policy_before_preparation_and_enrollment() {
    let fixture = Fixture::new();
    let credential = DeviceCredential::generate().unwrap();
    let stale = setup(&fixture, [96; 32], &credential);
    fixture.enable();
    let Management::Setup {
        generation,
        pin,
        expected_policy,
        expected_enabled,
        endpoint,
        policy,
        operation,
        public_key,
        name,
    } = stale
    else {
        panic!()
    };
    assert!(matches!(
        fixture.management(Management::PrepareSetup {
            generation,
            pin,
            expected_policy,
            expected_enabled,
            endpoint,
            policy,
            operation,
            public_key,
            name,
        }),
        Err(DirectError::PolicyChanged)
    ));

    let request = setup(&fixture, [97; 32], &credential);
    reserve(&fixture, &request);
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    fixture.state.lock().unwrap().stop();
    let reopened = host::State::open(&fixture.paths).unwrap();
    assert!(matches!(
        reopened.lock().unwrap().manage(None, request),
        Err(DirectError::PolicyChanged)
    ));
    assert!(matches!(
        reopened.lock().unwrap().manage(None, Management::Devices),
        Ok((ManagementReply::Devices(devices), _)) if devices.is_empty()
    ));
    assert!(matches!(
        reopened.lock().unwrap().manage(None, Management::Status),
        Ok((ManagementReply::Status(status), _)) if !status.enabled
    ));
    reopened.lock().unwrap().stop();
}

#[test]
fn admitted_setup_retry_returns_its_original_pairing_without_replacing_a_changed_policy() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let request = setup(&fixture, [98; 32], &credential);
    reserve(&fixture, &request);
    let ManagementReply::Setup(original) = fixture.management(request.clone()).unwrap() else {
        panic!()
    };
    fixture
        .management(Management::SetPolicy {
            enabled: false,
            policy: None,
        })
        .unwrap();
    reserve(&fixture, &request);
    assert!(matches!(
        fixture.management(request),
        Ok(ManagementReply::Setup(paired)) if paired == original
    ));
    assert!(matches!(
        fixture.management(Management::Status),
        Ok(ManagementReply::Status(status)) if !status.enabled
    ));
}

#[test]
fn lost_setup_reply_uses_the_original_claim_for_key_bound_quic_cleanup() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let operation = [92; 32];
    let request = setup(&fixture, operation, &credential);
    let Management::Setup { operation, .. } = &request else {
        panic!()
    };
    let operation = *operation;
    reserve(&fixture, &request);
    let ManagementReply::Setup(paired) = fixture.management(request.clone()).unwrap() else {
        panic!()
    };
    let mut cleanup = Enrollment::resume(
        &fixture.endpoint(),
        paired.pin.clone(),
        operation,
        &credential,
    )
    .unwrap();
    let wrong = DeviceCredential::generate().unwrap();
    let mut wrong_cleanup =
        Enrollment::resume(&fixture.endpoint(), paired.pin.clone(), operation, &wrong).unwrap();
    let worker = fixture.serve_next();
    assert_eq!(wrong_cleanup.cancel(), Err(DirectError::Substituted));
    worker.join().unwrap().unwrap();
    for _ in 0..2 {
        let worker = fixture.serve_next();
        assert_eq!(cleanup.cancel().unwrap().unwrap().target, paired.pairing);
        worker.join().unwrap().unwrap();
    }
    assert!(matches!(
        fixture.management(request),
        Err(DirectError::Revoked)
    ));
    let next = setup(&fixture, [93; 32], &credential);
    reserve(&fixture, &next);
    let ManagementReply::Setup(later) = fixture.management(next).unwrap() else {
        panic!()
    };
    assert_ne!(later.pairing, paired.pairing);
    let worker = fixture.serve_next();
    cleanup.cancel().unwrap();
    worker.join().unwrap().unwrap();
    let worker = fixture.serve_next();
    drop(DirectConnection::connect(&fixture.endpoint(), &later, &credential).unwrap());
    let _ = worker.join().unwrap();
}

#[test]
fn paired_quic_authority_cannot_prepare_enroll_or_cancel_account_setup() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let request = setup(&fixture, [94; 32], &credential);
    reserve(&fixture, &request);
    let Management::Setup {
        generation,
        pin,
        expected_policy,
        expected_enabled,
        endpoint,
        policy,
        operation,
        public_key,
        name,
    } = request.clone()
    else {
        panic!()
    };
    let worker = fixture.serve_next();
    let connection = connect(&fixture.endpoint(), &paired, &credential);
    for action in [
        Management::PrepareSetup {
            generation,
            pin: pin.clone(),
            expected_policy,
            expected_enabled,
            endpoint,
            policy,
            operation,
            public_key,
            name,
        },
        request.clone(),
        Management::CancelSetup { pin, operation },
    ] {
        assert!(matches!(
            manage(&connection, action),
            Err(DirectError::AuthenticationRefused)
        ));
    }
    drop(connection);
    let _ = worker.join().unwrap();
    assert!(matches!(
        fixture.management(request),
        Ok(ManagementReply::Setup(_))
    ));
}

#[test]
fn completed_cancellations_retire_capacity_and_reject_delayed_prepare_and_admission() {
    let mut fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let retired = setup(&fixture, [101; 32], &credential);
    for _ in 0..257 {
        let request = setup(&fixture, [101; 32], &credential);
        reserve(&fixture, &request);
        let Management::Setup { pin, operation, .. } = request else {
            panic!()
        };
        assert!(matches!(
            fixture.management(Management::CancelSetup { pin, operation }),
            Ok(ManagementReply::SetupCancelled { .. })
        ));
    }
    fixture.state.lock().unwrap().stop();
    fixture.state = host::State::open(&fixture.paths).unwrap();
    let Management::Setup {
        generation,
        pin,
        expected_policy,
        expected_enabled,
        endpoint,
        policy,
        operation,
        public_key,
        name,
    } = retired.clone()
    else {
        panic!()
    };
    assert!(matches!(
        fixture.management(Management::PrepareSetup {
            generation,
            pin,
            expected_policy,
            expected_enabled,
            endpoint,
            policy,
            operation,
            public_key,
            name
        }),
        Err(DirectError::AlreadyDecided)
    ));
    assert!(matches!(
        fixture.management(retired),
        Err(DirectError::AlreadyDecided)
    ));
    let next = setup(&fixture, [102; 32], &credential);
    reserve(&fixture, &next);
    assert!(matches!(
        fixture.management(next),
        Ok(ManagementReply::Setup(_))
    ));
}

#[test]
fn unknown_current_generation_requires_account_cancellation_to_fence_delayed_preparation() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let request = setup(&fixture, [103; 32], &credential);
    let Management::Setup {
        pin,
        operation,
        generation,
        expected_policy,
        expected_enabled,
        endpoint,
        policy,
        public_key,
        name,
    } = request
    else {
        panic!()
    };
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Cancel {
                claim: operation,
                key: public_key
            }
        ),
        Err(DirectError::Expired)
    ));
    let mut future = operation;
    future[..8].copy_from_slice(&(generation + 1).to_be_bytes());
    assert!(matches!(
        fixture.management(Management::CancelSetup {
            pin: pin.clone(),
            operation: future
        }),
        Err(DirectError::Expired)
    ));
    assert!(matches!(
        fixture.management(Management::CancelSetup {
            pin: pin.clone(),
            operation
        }),
        Ok(ManagementReply::SetupCancelled { .. })
    ));
    assert!(matches!(
        fixture.management(Management::PrepareSetup {
            generation,
            pin,
            expected_policy,
            expected_enabled,
            endpoint,
            policy,
            operation,
            public_key,
            name
        }),
        Err(DirectError::AlreadyDecided)
    ));
    assert!(matches!(
        fixture.authenticate(
            &credential,
            wire::Action::Cancel {
                claim: operation,
                key: public_key
            }
        ),
        Ok(wire::Answer::Cancelled(None))
    ));
}

#[test]
fn configured_listener_survives_off_on_and_can_be_selected_before_exposure() {
    let mut fixture = Fixture::new();
    assert!(matches!(
        fixture.management(Management::SetPolicy {
            policy: None,
            enabled: true
        }),
        Err(DirectError::InvalidRequest)
    ));
    let policy = NetworkPolicy::new(free_udp(std::net::Ipv4Addr::LOCALHOST.into())).unwrap();
    fixture
        .management(Management::SetPolicy {
            policy: Some(policy.clone()),
            enabled: false,
        })
        .unwrap();
    for enabled in [false, true, false, true, false] {
        let ManagementReply::Status(status) = fixture
            .management(Management::SetPolicy {
                policy: None,
                enabled,
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(status.policy, Some(policy.clone()));
        assert_eq!(status.enabled, enabled);
        assert_eq!(
            status.listener,
            if enabled {
                ListenerState::Listening { addr: policy.bind }
            } else {
                ListenerState::Off
            }
        );
        fixture.state.lock().unwrap().stop();
        fixture.state = host::State::open(&fixture.paths).unwrap();
        let ManagementReply::Status(reopened) = fixture.management(Management::Status).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (reopened.policy, reopened.enabled),
            (Some(policy.clone()), enabled)
        );
    }
}

#[test]
fn dns_connections_reconnect_with_the_saved_name_and_require_the_original_pin() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let port = Endpoint::from_url(&fixture.endpoint()).unwrap().port();
    let endpoint = format!("quic://localhost:{port}");
    for _ in 0..2 {
        let worker = fixture.serve_next();
        drop(connect(&endpoint, &paired, &credential));
        let _ = worker.join().unwrap();
    }
    let mut wrong = paired;
    wrong.pin.certificate[0] ^= 1;
    let worker = fixture.serve_next();
    assert!(matches!(
        DirectConnection::connect(&endpoint, &wrong, &credential),
        Err(DirectError::PinMismatch)
    ));
    let _ = worker.join().unwrap();
}

#[test]
fn client_clock_cannot_expire_or_reject_an_invitation_before_server_observation() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    for expires in [1, wire::now().unwrap() + 600] {
        let mut payload = invitation.payload.clone();
        payload.expires = expires;
        let encoded = client::invitation(payload).unwrap();
        let parsed = Invitation::parse(&encoded.display_text()).unwrap();
        assert!(Enrollment::prepare(&parsed, "Clock skew".into(), &credential).is_ok());
    }
}

#[test]
fn setup_preserves_distinct_public_and_listener_ports_across_reopen_and_exact_replay() {
    let mut fixture = Fixture::new();
    let credential = DeviceCredential::generate().unwrap();
    let mut request = setup(&fixture, [104; 32], &credential);
    let Management::Setup {
        endpoint, policy, ..
    } = &mut request
    else {
        panic!()
    };
    let bind = policy.bind;
    let public_port = if bind.port() == 9443 { 9444 } else { 9443 };
    *endpoint = format!("quic://public.example:{public_port}");
    assert_ne!(public_port, bind.port());
    reserve(&fixture, &request);

    // A proxy/NAT endpoint and its exact upstream listener remain independently bound
    // when the durable reservation is reloaded before admission.
    fixture.state.lock().unwrap().stop();
    fixture.state = host::State::open(&fixture.paths).unwrap();
    for change_bind in [false, true] {
        let mut substituted = request.clone();
        let Management::Setup {
            endpoint, policy, ..
        } = &mut substituted
        else {
            panic!()
        };
        if change_bind {
            policy.bind.set_port(public_port);
        } else {
            *endpoint = format!("quic://other.example:{public_port}");
        }
        assert!(matches!(
            fixture.management(substituted),
            Err(DirectError::Substituted)
        ));
    }
    let ManagementReply::Setup(original) = fixture.management(request.clone()).unwrap() else {
        panic!()
    };
    fixture.state.lock().unwrap().stop();
    fixture.state = host::State::open(&fixture.paths).unwrap();
    let ManagementReply::Setup(repeated) = fixture.management(request).unwrap() else {
        panic!()
    };
    assert_eq!(repeated, original);
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    assert_eq!(status.policy.unwrap().bind, bind);
    assert!(status.enabled);
}

#[test]
fn paired_administration_is_refused_without_changing_invitations_claims_or_devices() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let other_key = DeviceCredential::generate().unwrap();
    let other = fixture.enroll(&other_key);
    let invitation = fixture.invite();
    let pending_key = DeviceCredential::generate().unwrap();
    let claim = fixture.claim(&invitation, &pending_key);
    let setup = setup(&fixture, [77; 32], &pending_key);
    let Management::Setup { pin, operation, .. } = &setup else {
        panic!()
    };
    let cancel = Management::CancelSetup {
        pin: pin.clone(),
        operation: *operation,
    };
    let server = fixture.serve_next();
    let connection = connect(&fixture.endpoint(), &paired, &credential);
    // Observe the completed admission timestamp write before measuring admin effects.
    assert!(matches!(
        manage(&connection, Management::Status),
        Ok(ManagementReply::Status(_))
    ));
    let before = std::fs::read(fixture.paths.data.join("direct.json")).unwrap();
    for request in [
        Management::Invite,
        Management::Withdraw(invitation.id()),
        Management::Approve(claim.clone()),
        Management::Reject(claim.id),
        Management::Revoke {
            operation: crypto::random().unwrap(),
            target: other.pairing,
        },
        setup,
        cancel,
    ] {
        assert!(matches!(
            manage(&connection, request),
            Err(DirectError::AuthenticationRefused)
        ));
    }
    for request in [Management::Status, Management::Claims, Management::Devices] {
        assert!(manage(&connection, request).is_ok());
    }
    assert!(
        before == std::fs::read(fixture.paths.data.join("direct.json")).unwrap(),
        "refused administration must leave the complete retained record unchanged"
    );
    assert!(matches!(
        fixture.management(Management::Approve(claim)),
        Ok(ManagementReply::Approved(_))
    ));
    assert!(matches!(
        fixture.authenticate(&other_key, wire::Action::Connect { paired: other }),
        Ok(wire::Answer::Connected)
    ));
    drop(connection);
    let _ = server.join().unwrap();
}

#[test]
fn end_all_sessions_requires_account_local_admission_but_workspace_mutations_do_not() {
    #[derive(Default)]
    struct Effects {
        session_ended: bool,
        workspace_created: bool,
        requests: usize,
    }
    struct Runtime(Arc<Mutex<Effects>>);
    impl PaneService for Runtime {
        fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
        fn request(&mut self, _: &Peer, _: RequestId, operation: Operation) -> Dispatch {
            let mut effects = self.0.lock().unwrap();
            effects.requests += 1;
            match operation {
                Operation::Mutate {
                    mutation: crate::hierarchy::Mutation::EndAllSessions { .. },
                    ..
                } => effects.session_ended = true,
                Operation::Mutate {
                    mutation: crate::hierarchy::Mutation::CreateWorkspace { .. },
                    ..
                } => effects.workspace_created = true,
                _ => panic!("unexpected PaneService dispatch"),
            }
            Dispatch::reply(Ok(ReplyBody::Done))
        }
        fn input(&mut self, _: &Peer, _: Input) {}
        fn view(&mut self, _: &Peer, _: View) {}
        fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
        fn acknowledge(&mut self, _: &Peer, _: u64) {}
        fn disconnected(&mut self, _: &Peer) {}
        fn shutdown(&mut self) -> Result<(), ServerError> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    fixture.enable();
    let key = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&key);
    let effects = Arc::new(Mutex::new(Effects::default()));
    let runtime = service(Runtime(Arc::clone(&effects)));
    let server = fixture.serve_next_with(Arc::clone(&runtime));
    let connection = connect(&fixture.endpoint(), &paired, &key);
    let end = || Operation::Mutate {
        sequence: 1,
        mutation: crate::hierarchy::Mutation::EndAllSessions {
            confirmation: crate::hierarchy::CloseInventory {
                boot: [1; 32],
                sessions: vec![crate::hierarchy::ConfirmedPane {
                    id: [2; 32],
                    session: Some([3; 32]),
                }],
                controls: Vec::new(),
            },
        },
    };
    assert!(matches!(
        request(&connection, end()),
        Err(RequestError::Refused(Refusal::Unsupported))
    ));
    {
        let effects = effects.lock().unwrap();
        assert!(!effects.session_ended);
        assert_eq!(effects.requests, 0);
    }
    assert!(matches!(
        request(
            &connection,
            Operation::Mutate {
                sequence: 1,
                mutation: crate::hierarchy::Mutation::CreateWorkspace {
                    name: "Remote Workspace".into(),
                    directory: None,
                },
            }
        ),
        Ok(ReplyBody::Done)
    ));
    assert!(effects.lock().unwrap().workspace_created);
    let (local, local_worker) = fixture.serve_local_with(Arc::clone(&runtime));
    assert!(matches!(request(&local, end()), Ok(ReplyBody::Done)));
    {
        let effects = effects.lock().unwrap();
        assert!(effects.session_ended);
        assert_eq!(effects.requests, 2);
    }
    drop(local);
    local_worker.join().unwrap();
    drop(connection);
    let _ = server.join().unwrap();
}

#[cfg(feature = "client")]
#[test]
fn concurrent_shared_links_of_one_pairing_each_admit_and_answer() {
    use crate::ServerConnection;
    use std::sync::Barrier;

    struct Connector {
        endpoint: String,
        paired: PairedDevice,
        credential: Arc<DeviceCredential>,
        admitted: Arc<Barrier>,
    }
    impl DirectConnector for Connector {
        fn connect(&self) -> Result<DirectConnection, DirectError> {
            let result = DirectConnection::connect(&self.endpoint, &self.paired, &self.credential);
            // Both handles finish QUIC authentication before either starts admission.
            self.admitted.wait();
            result
        }
    }

    let fixture = Fixture::new();
    fixture.enable();
    let credential = Arc::new(DeviceCredential::generate().unwrap());
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    // More cycles than the registry holds at once exercise unregistration.
    for _ in 0..68 {
        let admitted = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|route| {
                ServerConnection::direct(
                    &paired.pin.build,
                    [42 + route; 32],
                    paired.clone(),
                    Arc::new(Connector {
                        endpoint: endpoint.clone(),
                        paired: paired.clone(),
                        credential: Arc::clone(&credential),
                        admitted: Arc::clone(&admitted),
                    }),
                )
                .unwrap()
            })
            .collect();
        let hosts = [fixture.serve_next(), fixture.serve_next()];
        let workers: Vec<_> = handles
            .into_iter()
            .map(|server| {
                std::thread::spawn(move || {
                    server.refresh()?;
                    // A live shared link answers without another connection.
                    let link = server.link()?;
                    match request(&link, Operation::Direct(Management::Status)) {
                        Ok(ReplyBody::Direct(Ok(ManagementReply::Status(_)))) => Ok(()),
                        _ => Err(ServerError::Protocol),
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        for host in hosts {
            let _ = host.join().unwrap();
        }
    }
}

#[test]
fn a_closed_server_port_is_a_transport_failure() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let closed = UdpSocket::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("quic://{}", closed.local_addr().unwrap());
    drop(closed);
    let started = Instant::now();
    assert_eq!(
        DirectConnection::connect(&endpoint, &paired, &credential).err(),
        Some(DirectError::Transport)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

mod quic_connections {
    use super::*;

    /// A request the refusing service answers without effect.
    fn nothing() -> Operation {
        Operation::ReleaseAttachment {
            attachment: crate::attachments::Attachment {
                workspace: [1; 32],
                generation: 1,
            },
        }
    }

    fn connect_quic(
        endpoint: &str,
        paired: &PairedDevice,
        credential: &DeviceCredential,
    ) -> ServerLink {
        let connection = DirectConnection::connect(endpoint, paired, credential).unwrap();
        admitted(connection, &paired.client, "Test", paired).unwrap()
    }

    #[test]
    fn a_paired_device_is_admitted_over_quic_and_its_requests_round_trip() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next_with(service(Refusing));
        let link = connect_quic(&fixture.endpoint(), &paired, &credential);
        for _ in 0..20 {
            assert!(matches!(
                request(&link, nothing()),
                Err(RequestError::Refused(Refusal::Unsupported))
            ));
        }
        drop(link);
        server.join().unwrap().unwrap();
    }

    /// The client's TLS certificate proves its key. Proofs signed by an enrolled credential do not
    /// admit a connection whose certificate is another device's.
    #[test]
    fn quic_refuses_a_device_proof_from_another_certificate() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let other = DeviceCredential::generate().unwrap();
        let server = fixture.serve_next_with(service(Refusing));
        let mut stream = quic::dial(&fixture.endpoint(), &paired.pin, &other).unwrap();
        let answer = client::exchange_on(
            &mut stream,
            &paired.pin,
            &credential,
            wire::Action::Connect {
                paired: paired.clone(),
            },
            None,
            None,
        );
        assert_eq!(answer.err(), Some(DirectError::AuthenticationRefused));
        server.join().unwrap().unwrap();
    }

    #[test]
    fn a_client_reaches_only_the_pinned_server_certificate() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let mut paired = fixture.enroll(&credential);
        paired.pin.certificate = [7; 32];
        let server = fixture.serve_next_with(service(Refusing));
        assert!(matches!(
            DirectConnection::connect(&fixture.endpoint(), &paired, &credential),
            Err(DirectError::PinMismatch)
        ));
        assert!(server.join().unwrap().is_err());
    }

    #[test]
    fn a_device_certificate_names_the_device_key() {
        let credential = DeviceCredential::generate().unwrap();
        let certificate = quic::tests_certificate(credential.expose_bytes());
        assert_eq!(
            quic::certificate_key(&certificate),
            Some(credential.public_key().unwrap())
        );
    }

    #[test]
    fn revocation_ends_a_quic_connection_with_a_typed_goodbye() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next_with(service(Refusing));
        let link = connect_quic(&fixture.endpoint(), &paired, &credential);
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        fixture
            .management(Management::Revoke {
                operation: crypto::random().unwrap(),
                target: paired.pairing,
            })
            .unwrap();
        assert_eq!(
            ended(&link),
            LinkError::Goodbye(crate::protocol::Fatal::Revoked)
        );
        drop(link);
        let _ = server.join().unwrap();
    }

    /// A raw frame of `kind` with `size` bytes in all, its body filled with `fill`.
    fn raw(kind: crate::protocol::Kind, size: usize, fill: u8) -> Vec<u8> {
        let mut frame = vec![fill; size];
        let len = (size - 4) as u32;
        frame[..4].copy_from_slice(&len.to_be_bytes());
        frame[4] = kind as u8;
        frame
    }

    /// Drives `carrier` until it has received `count` frames or `deadline` passes.
    fn receive_frames(
        carrier: &mut dyn crate::connection::carrier::Carrier,
        count: usize,
        deadline: Instant,
    ) -> Vec<crate::protocol::RawFrame> {
        let mut frames = Vec::new();
        while frames.len() < count {
            match carrier.receive() {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => {
                    carrier.flush().unwrap();
                    let readiness = carrier.readiness();
                    let until = readiness.deadline.map_or(deadline, |at| at.min(deadline));
                    crate::connection::io::wait_fd(readiness.read, false, until);
                    assert!(Instant::now() < deadline, "frames did not arrive");
                }
                Err(_) => break,
            }
        }
        frames
    }

    /// Offers `frames` in order and drives `carrier` until the peer has them, then closes.
    fn send_frames(
        carrier: &mut dyn crate::connection::carrier::Carrier,
        frames: Vec<(Vec<u8>, crate::connection::carrier::Priority)>,
        deadline: Instant,
    ) {
        use crate::connection::carrier::Offer;
        let mut queue = frames.into_iter();
        let mut held = queue.next();
        loop {
            while let Some((frame, priority)) = &held {
                if carrier.offer(frame, *priority).unwrap() != Offer::Accepted {
                    break;
                }
                held = queue.next();
            }
            let pending = carrier.flush().unwrap().pending;
            if held.is_none() && !pending {
                break;
            }
            let readiness = carrier.readiness();
            let until = readiness.deadline.map_or(deadline, |at| at.min(deadline));
            crate::connection::io::wait_fd(readiness.read, false, until);
            let _ = carrier.receive();
            assert!(Instant::now() < deadline, "frames did not leave");
        }
        carrier.close();
        while carrier.flush().is_ok_and(|flush| flush.pending) && Instant::now() < deadline {
            let readiness = carrier.readiness();
            let until = readiness.deadline.map_or(deadline, |at| at.min(deadline));
            crate::connection::io::wait_fd(readiness.read, false, until);
        }
    }

    /// The server writes L0 before L1, so without the client's rule a goodbye written after a
    /// large frame would arrive first.
    #[test]
    fn a_goodbye_never_overtakes_a_frame_in_flight() {
        use crate::connection::carrier::Priority;
        use crate::protocol::Kind;
        let fixture = Fixture::new();
        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let large = raw(Kind::Frame, crate::protocol::limits::MAX_FRAME_BYTES, 7);
        let goodbye = crate::protocol::frame::encode_server(
            &crate::protocol::ServerMessage::Goodbye(crate::protocol::Fatal::Stopping),
        )
        .unwrap()
        .frames
        .remove(0);
        let sent = (large.clone(), goodbye.clone());
        let server = std::thread::spawn(move || {
            let mut stream = accept_from(&listener);
            stream.send(&"open").unwrap();
            let mut carrier = stream.into_carrier();
            send_frames(
                &mut carrier,
                vec![(sent.0, Priority::L1), (sent.1, Priority::L0)],
                Instant::now() + Duration::from_secs(10),
            );
        });
        let credential = DeviceCredential::generate().unwrap();
        let mut stream = quic::dial(&endpoint, &pin, &credential).unwrap();
        assert_eq!(stream.receive::<String>().unwrap(), "open");
        let mut carrier = stream.into_carrier();
        let frames = receive_frames(&mut carrier, 2, Instant::now() + Duration::from_secs(10));
        let kinds: Vec<u8> = frames.iter().map(|frame| frame.kind).collect();
        assert_eq!(kinds, [Kind::Frame as u8, Kind::Goodbye as u8]);
        server.join().unwrap();
    }

    /// The client writes every level on one stream in the order it offers frames, so a
    /// cancellation never overtakes its request and later input never overtakes an earlier
    /// native effect completion.
    #[test]
    fn client_frames_arrive_in_the_order_offered() {
        use crate::connection::carrier::Priority;
        use crate::protocol::Kind;
        let fixture = Fixture::new();
        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let server = std::thread::spawn(move || {
            let mut stream = accept_from(&listener);
            stream.send(&"open").unwrap();
            let mut carrier = stream.into_carrier();
            receive_frames(&mut carrier, 4, Instant::now() + Duration::from_secs(10))
                .iter()
                .map(|frame| frame.kind)
                .collect::<Vec<_>>()
        });
        let credential = DeviceCredential::generate().unwrap();
        let mut stream = quic::dial(&endpoint, &pin, &credential).unwrap();
        assert_eq!(stream.receive::<String>().unwrap(), "open");
        let mut carrier = stream.into_carrier();
        send_frames(
            &mut carrier,
            vec![
                (raw(Kind::Request, 60 * 1024, 1), Priority::L0),
                (raw(Kind::Cancel, 64, 2), Priority::L1),
                (raw(Kind::EffectDone, 16 * 1024, 3), Priority::L1),
                (raw(Kind::Input, 64, 4), Priority::L0),
            ],
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(
            server.join().unwrap(),
            [
                Kind::Request as u8,
                Kind::Cancel as u8,
                Kind::EffectDone as u8,
                Kind::Input as u8
            ]
        );
    }

    /// A switch between networks moves the client to a new socket and source port. The server
    /// validates the new path; the session continues without a second admission.
    #[test]
    fn a_client_on_a_new_socket_keeps_its_session_without_admission() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Admissions(Arc<AtomicUsize>);
        impl PaneService for Admissions {
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                Dispatch::reply(Err(Refusal::Unsupported))
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                Ok(())
            }
        }
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let admissions = Arc::new(AtomicUsize::new(0));
        let server = fixture.serve_next_with(service(Admissions(Arc::clone(&admissions))));
        let mut stream = quic::dial(&fixture.endpoint(), &paired.pin, &credential).unwrap();
        let answer = client::exchange_on(
            &mut stream,
            &paired.pin,
            &credential,
            wire::Action::Connect {
                paired: paired.clone(),
            },
            None,
            None,
        );
        assert!(matches!(answer, Ok(wire::Answer::Connected)));
        crate::protocol::admit_transport(
            &mut stream,
            (&paired.client, &"Test".into()),
            &paired.pin.server,
            &paired.pin.build,
            &[crate::protocol::MAJOR],
        )
        .unwrap();
        let carrier = stream.into_carrier();
        let rebind = carrier.rebinder();
        let link = spawn_link(Box::new(carrier), Arc::new(|_| {})).unwrap();
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        for _ in 0..3 {
            rebind.store(true, Ordering::Release);
            for _ in 0..5 {
                assert!(matches!(
                    request(&link, nothing()),
                    Err(RequestError::Refused(Refusal::Unsupported))
                ));
            }
        }
        assert_eq!(admissions.load(Ordering::SeqCst), 1);
        assert_eq!(link.health(), crate::connection::link::Health::Live);
        drop(link);
        let _ = server.join().unwrap();
    }

    /// A certificate whose subject key is ECDSA proves nothing about an Ed25519 key it carries
    /// elsewhere, and a handshake signed with ECDSA does not authenticate a device.
    #[test]
    fn an_ecdsa_certificate_cannot_claim_an_embedded_device_key() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let victim = credential.public_key().unwrap();
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec![crate::connection::quic::SERVER_NAME.into()])
                .unwrap();
        let mut embedded = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        embedded.extend_from_slice(&victim);
        params.custom_extensions = vec![rcgen::CustomExtension::from_oid_content(
            &[1, 3, 6, 1, 4, 1, 99999, 1],
            embedded.clone(),
        )];
        let certificate = params.self_signed(&pair).unwrap().der().clone();
        assert!(
            certificate
                .windows(embedded.len())
                .any(|window| window == embedded.as_slice()),
            "the certificate carries the victim's key as Ed25519 SubjectPublicKeyInfo bytes"
        );
        assert_eq!(quic::certificate_key(&certificate), None);

        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(mut stream) = listener.accept() {
                    return stream.handshake(deadline).is_ok();
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(pair.serialize_der().into());
        let seen = Arc::new(quic::Verification::default());
        let config = quic::presenting(&pin, certificate, key, Arc::clone(&seen)).unwrap();
        let address = Endpoint::from_url(&endpoint)
            .unwrap()
            .resolve(Instant::now() + Duration::from_secs(5))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        assert!(QuicStream::connect(config, &address, deadline, &seen.verified).is_err());
        assert!(
            !server.join().unwrap(),
            "the server must refuse the handshake"
        );
    }

    /// An Initial packet from an address the server has not seen answer a retry creates no
    /// connection and takes no slot, so spoofed sources cannot hold them. A real client still
    /// connects through the retry.
    #[test]
    fn an_unvalidated_initial_gets_a_retry_and_takes_no_slot() {
        let fixture = Fixture::new();
        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let credential = DeviceCredential::generate().unwrap();
        let address = Endpoint::from_url(&endpoint)
            .unwrap()
            .resolve(Instant::now() + Duration::from_secs(5))
            .unwrap()[0];
        // A client's first Initial packet, sent from a socket that never answers.
        let config = quic::presenting(
            &pin,
            quic::tests_certificate(credential.expose_bytes()).into(),
            rustls::pki_types::PrivateKeyDer::Pkcs8(credential.expose_bytes().to_vec().into()),
            Arc::default(),
        )
        .unwrap();
        let mut client = noq_proto::Endpoint::new(Arc::default(), None, true);
        let (_, mut connection) = client
            .connect(Instant::now(), config, address, "spaceterm.invalid")
            .unwrap();
        let mut initial = Vec::new();
        let transmit = connection
            .poll_transmit(Instant::now(), std::num::NonZeroUsize::MIN, &mut initial)
            .unwrap();
        let spoofer = UdpSocket::bind("127.0.0.1:0").unwrap();
        spoofer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        for _ in 0..40 {
            spoofer.send_to(&initial[..transmit.size], address).unwrap();
        }
        let mut reply = [0; 2048];
        let (count, _) = spoofer.recv_from(&mut reply).unwrap();
        // A long header whose type bits name Retry.
        assert!(
            count > 0 && reply[0] & 0xf0 == 0xf0,
            "the answer is a retry"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            listener.accept().is_none(),
            "no connection waits for a slot"
        );

        let server = std::thread::spawn(move || {
            let mut accepted = accept_from(&listener);
            accepted.send(&"open").unwrap();
            accepted
        });
        let mut stream = quic::dial(&endpoint, &pin, &credential).unwrap();
        assert_eq!(stream.receive::<String>().unwrap(), "open");
        drop(server.join().unwrap());
    }

    /// Turning exposure off and on at the same address while a client is connected takes the
    /// listener back instead of binding a socket the old connection still holds, and that
    /// connection still receives its goodbye.
    #[test]
    fn toggling_exposure_with_a_connected_client_keeps_the_listener() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let endpoint = fixture.endpoint();
        let server = fixture.serve_next();
        let link = connect(&endpoint, &paired, &credential);
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap()
        else {
            panic!()
        };
        for enabled in [false, true] {
            fixture
                .management(Management::SetPolicy {
                    enabled,
                    policy: status.policy.clone(),
                })
                .unwrap();
        }
        let ManagementReply::Status(after) = fixture.management(Management::Status).unwrap() else {
            panic!()
        };
        assert_eq!(after.listener, status.listener);
        assert_eq!(
            ended(&link),
            LinkError::Goodbye(crate::protocol::Fatal::Revoked)
        );
        drop(link);
        let _ = server.join().unwrap();
        let server = fixture.serve_next();
        let link = connect(&endpoint, &paired, &credential);
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        drop(link);
        let _ = server.join().unwrap();
    }

    /// A server that stopped receiving, as while it says goodbye, still takes the
    /// acknowledgements and credit that let its writes finish, and does not spin meanwhile.
    #[test]
    fn a_server_that_stopped_receiving_still_finishes_writing() {
        use crate::connection::carrier::{Carrier, Offer, Priority};
        use crate::protocol::Kind;
        let fixture = Fixture::new();
        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let frames = 4;
        let server = std::thread::spawn(move || {
            let mut stream = accept_from(&listener);
            stream.send(&"open").unwrap();
            let mut carrier = stream.into_carrier();
            let deadline = Instant::now() + Duration::from_secs(20);
            let (mut sent, mut turns) = (0, 0);
            let frame = raw(Kind::Frame, crate::protocol::limits::MAX_FRAME_BYTES, 9);
            // Never `receive`: only offer, flush and wait, as the I/O thread does once it
            // stopped reading.
            loop {
                turns += 1;
                if sent < frames && carrier.offer(&frame, Priority::L1).unwrap() == Offer::Accepted
                {
                    sent += 1;
                    continue;
                }
                let pending = carrier.flush().unwrap().pending;
                if sent == frames && !pending {
                    // Closing waits until the client acknowledged every stream to its end.
                    carrier.close();
                    if !carrier.flush().unwrap().pending {
                        break;
                    }
                }
                assert!(Instant::now() < deadline, "the writes never finished");
                let readiness = carrier.readiness();
                let until = readiness.deadline.map_or(deadline, |at| at.min(deadline));
                crate::connection::io::wait_fd(
                    readiness.progress.unwrap_or(readiness.read),
                    false,
                    until,
                );
            }
            turns
        });
        let credential = DeviceCredential::generate().unwrap();
        let mut stream = quic::dial(&endpoint, &pin, &credential).unwrap();
        assert_eq!(stream.receive::<String>().unwrap(), "open");
        let mut carrier = stream.into_carrier();
        let received = receive_frames(
            &mut carrier,
            frames,
            Instant::now() + Duration::from_secs(20),
        );
        assert_eq!(received.len(), frames);
        // Acknowledge the server's close until it is done.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server.is_finished() && Instant::now() < deadline {
            let _ = carrier.receive();
            let _ = carrier.flush();
            let readiness = carrier.readiness();
            let until = readiness.deadline.map_or(deadline, |at| {
                at.min(Instant::now() + Duration::from_millis(50))
            });
            crate::connection::io::wait_fd(readiness.read, false, until);
        }
        let turns = server.join().unwrap();
        assert!(turns < 20_000, "the server spun for {turns} turns");
    }

    /// A level whose stream waits on flow control is full on its own; another level is still
    /// taken.
    #[test]
    fn a_blocked_bulk_stream_leaves_the_other_levels_open() {
        use crate::connection::carrier::{Carrier, Offer, Priority};
        use crate::protocol::Kind;
        let fixture = Fixture::new();
        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let server = std::thread::spawn(move || {
            let mut stream = accept_from(&listener);
            stream.send(&"open").unwrap();
            let mut carrier = stream.into_carrier();
            let bulk = raw(Kind::BulkReply, crate::protocol::limits::MAX_FRAME_BYTES, 2);
            let mut offers = Vec::new();
            for _ in 0..4 {
                offers.push(carrier.offer(&bulk, Priority::L2).unwrap());
                carrier.flush().unwrap();
            }
            let ping = crate::protocol::frame::encode_server(
                &crate::protocol::ServerMessage::Goodbye(crate::protocol::Fatal::Stopping),
            )
            .unwrap()
            .frames
            .remove(0);
            (offers, carrier.offer(&ping, Priority::L0).unwrap())
        });
        let credential = DeviceCredential::generate().unwrap();
        let mut stream = quic::dial(&endpoint, &pin, &credential).unwrap();
        assert_eq!(stream.receive::<String>().unwrap(), "open");
        // The client never reads, so L2 runs out of credit.
        let (offers, control) = server.join().unwrap();
        assert!(offers.contains(&Offer::Full), "L2 must fill up: {offers:?}");
        assert_eq!(control, Offer::Accepted);
        drop(stream);
    }

    /// The 33rd connection waiting for the host is refused at once.
    #[test]
    fn connections_past_the_pending_bound_are_refused() {
        let fixture = Fixture::new();
        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let waiting: Vec<_> = (0..32)
            .map(|_| {
                let (endpoint, pin) = (endpoint.clone(), pin.clone());
                std::thread::spawn(move || {
                    let credential = DeviceCredential::generate().unwrap();
                    let _ = quic::dial(&endpoint, &pin, &credential);
                })
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(4);
        while listener.waiting() < 32 {
            assert!(
                Instant::now() < deadline,
                "only {} waited",
                listener.waiting()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let credential = DeviceCredential::generate().unwrap();
        let started = Instant::now();
        assert!(quic::dial(&endpoint, &pin, &credential).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(listener.waiting(), 32);
        drop(listener);
        for thread in waiting {
            thread.join().unwrap();
        }
    }

    /// A handshake that either side refuses is an authentication refusal, not a transport
    /// failure to retry.
    #[test]
    fn a_refused_tls_handshake_is_an_authentication_refusal() {
        let fixture = Fixture::new();
        let (listener, endpoint) = listener(&fixture.state);
        let pin = fixture.state.lock().unwrap().pin.clone();
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate =
            rcgen::CertificateParams::new(vec![crate::connection::quic::SERVER_NAME.into()])
                .unwrap()
                .self_signed(&pair)
                .unwrap()
                .der()
                .clone();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(mut stream) = listener.accept() {
                    return stream.handshake(deadline).is_ok();
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(pair.serialize_der().into());
        let seen = Arc::new(quic::Verification::default());
        let config = quic::presenting(&pin, certificate, key, Arc::clone(&seen)).unwrap();
        let address = Endpoint::from_url(&endpoint)
            .unwrap()
            .resolve(Instant::now() + Duration::from_secs(5))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let refused = QuicStream::connect(config, &address, deadline, &seen.verified);
        assert_eq!(
            refused.err().map(quic::transport),
            Some(DirectError::AuthenticationRefused)
        );
        assert!(!server.join().unwrap());
    }

    /// A UDP relay between clients and a server. Each client source gets its own upstream
    /// socket, so the server sees a new address when a client moves. `blackhole` drops a source's
    /// traffic both ways, as a network that vanished under a still-valid address.
    struct Relay {
        address: std::net::SocketAddr,
        sources: Arc<Mutex<Vec<std::net::SocketAddr>>>,
        blackhole: Arc<Mutex<Option<std::net::SocketAddr>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Relay {
        fn new(server: std::net::SocketAddr) -> Self {
            use std::sync::atomic::Ordering;
            let front = UdpSocket::bind("127.0.0.1:0").unwrap();
            front.set_nonblocking(true).unwrap();
            let address = front.local_addr().unwrap();
            let sources: Arc<Mutex<Vec<std::net::SocketAddr>>> = Arc::default();
            let blackhole: Arc<Mutex<Option<std::net::SocketAddr>>> = Arc::default();
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (seen, hole, stopping) = (
                Arc::clone(&sources),
                Arc::clone(&blackhole),
                Arc::clone(&stop),
            );
            let thread = std::thread::spawn(move || {
                let mut upstreams: Vec<(std::net::SocketAddr, UdpSocket)> = Vec::new();
                let mut buffer = [0; 65_536];
                while !stopping.load(Ordering::Acquire) {
                    let mut idle = true;
                    while let Ok((count, source)) = front.recv_from(&mut buffer) {
                        idle = false;
                        if *hole.lock().unwrap() == Some(source) {
                            continue;
                        }
                        if !upstreams.iter().any(|(known, _)| *known == source) {
                            let upstream = UdpSocket::bind("127.0.0.1:0").unwrap();
                            upstream.connect(server).unwrap();
                            upstream.set_nonblocking(true).unwrap();
                            upstreams.push((source, upstream));
                            seen.lock().unwrap().push(source);
                        }
                        let (_, upstream) = upstreams
                            .iter()
                            .find(|(known, _)| *known == source)
                            .unwrap();
                        let _ = upstream.send(&buffer[..count]);
                    }
                    for (source, upstream) in &upstreams {
                        while let Ok(count) = upstream.recv(&mut buffer) {
                            idle = false;
                            if *hole.lock().unwrap() != Some(*source) {
                                let _ = front.send_to(&buffer[..count], source);
                            }
                        }
                    }
                    if idle {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            });
            Self {
                address,
                sources,
                blackhole,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for Relay {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The path under a client dies while its address stays valid. The client's unanswered ping
    /// makes it move to a new socket; the server validates the new path, and the session goes on
    /// with no second admission and no end, so the client keeps its Pane control.
    #[test]
    fn an_unanswered_ping_moves_the_client_to_a_new_path_and_keeps_its_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Session {
            admitted: Arc<AtomicUsize>,
            ended: Arc<AtomicUsize>,
        }
        impl PaneService for Session {
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {
                self.admitted.fetch_add(1, Ordering::SeqCst);
            }
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                Dispatch::reply(Err(Refusal::Unsupported))
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {
                self.ended.fetch_add(1, Ordering::SeqCst);
            }
            fn shutdown(&mut self) -> Result<(), ServerError> {
                Ok(())
            }
        }
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server_address = Endpoint::from_url(&fixture.endpoint())
            .unwrap()
            .resolve(Instant::now() + Duration::from_secs(5))
            .unwrap()[0];
        let relay = Relay::new(server_address);
        let (admitted, ended) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let server = fixture.serve_next_with(service(Session {
            admitted: Arc::clone(&admitted),
            ended: Arc::clone(&ended),
        }));
        let link = connect(&format!("quic://{}", relay.address), &paired, &credential);
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        let first = relay.sources.lock().unwrap()[0];
        *relay.blackhole.lock().unwrap() = Some(first);
        // The link pings after 5 s of silence and suspects its path 2.5 s later.
        let deadline = Instant::now() + crate::protocol::limits::DEAD_AFTER;
        while relay.sources.lock().unwrap().len() < 2 {
            assert!(Instant::now() < deadline, "the client never moved");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        assert_eq!(link.health(), crate::connection::link::Health::Live);
        assert_eq!(admitted.load(Ordering::SeqCst), 1, "no second admission");
        assert_eq!(ended.load(Ordering::SeqCst), 0, "the session never ended");
        drop(link);
        let _ = server.join().unwrap();
    }

    fn listener_state(fixture: &Fixture) -> ListenerState {
        let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap()
        else {
            panic!()
        };
        status.listener
    }

    /// Moving exposure from the wildcard to one address of the same port binds at once when
    /// no connection holds the old socket.
    #[test]
    fn changing_the_address_without_connections_binds_at_once() {
        let fixture = Fixture::new();
        let wildcard = fixture.listen_at(std::net::Ipv4Addr::UNSPECIFIED.into());
        let specific =
            std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), wildcard.port());
        fixture
            .management(Management::SetPolicy {
                enabled: true,
                policy: Some(NetworkPolicy::new(specific).unwrap()),
            })
            .unwrap();
        assert_eq!(
            listener_state(&fixture),
            ListenerState::Listening { addr: specific }
        );
    }

    /// While a connection of the old address still holds its socket, the new address waits as
    /// `Starting`, then binds once that connection ended with its goodbye.
    #[test]
    fn changing_the_address_with_a_live_connection_binds_once_it_ends() {
        let fixture = Fixture::new();
        let specific = fixture.listen_at(std::net::Ipv4Addr::LOCALHOST.into());
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next();
        let link = connect(&fixture.endpoint(), &paired, &credential);
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        let wildcard =
            std::net::SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), specific.port());
        fixture
            .management(Management::SetPolicy {
                enabled: true,
                policy: Some(NetworkPolicy::new(wildcard).unwrap()),
            })
            .unwrap();
        assert!(matches!(
            listener_state(&fixture),
            ListenerState::Starting { .. } | ListenerState::Listening { .. }
        ));
        assert_eq!(
            ended(&link),
            LinkError::Goodbye(crate::protocol::Fatal::Revoked)
        );
        drop(link);
        let _ = server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while listener_state(&fixture) != (ListenerState::Listening { addr: wildcard }) {
            assert!(
                Instant::now() < deadline,
                "still {:?}",
                listener_state(&fixture)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let server = fixture.serve_next();
        let link = connect(&format!("quic://{specific}"), &paired, &credential);
        assert!(matches!(
            request(&link, nothing()),
            Err(RequestError::Refused(Refusal::Unsupported))
        ));
        drop(link);
        let _ = server.join().unwrap();
    }

    /// Another QUIC service on an address, one with another ALPN, refuses the protocol before
    /// the pinned server could be verified. That is an incompatibility, not a refused proof,
    /// and the client goes on to the next address.
    #[test]
    fn another_quic_service_is_incompatible_and_the_next_address_still_connects() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        // A QUIC service with this server's certificate and another protocol.
        let other = {
            let state = fixture.state.lock().unwrap();
            let (certificate, key) = state.certificate();
            let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.to_vec().into()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.to_vec().into()),
            )
            .unwrap();
            tls.alpn_protocols = vec![b"other/1".to_vec()];
            let config = crate::connection::quic::server_config(tls).unwrap();
            QuicListener::bind(free_udp(std::net::Ipv4Addr::LOCALHOST.into()), config).unwrap()
        };
        let other_address = other.address();
        let elsewhere = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Some(mut stream) = other.accept() {
                    let _ = stream.handshake(deadline);
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let alone =
            DirectConnection::connect(&format!("quic://{other_address}"), &paired, &credential);
        assert_eq!(alone.err(), Some(DirectError::IncompatibleProtocol));
        elsewhere.join().unwrap();

        let real = Endpoint::from_url(&fixture.endpoint())
            .unwrap()
            .resolve(Instant::now() + Duration::from_secs(5))
            .unwrap()[0];
        let server = fixture.serve_next();
        let seen = Arc::new(quic::Verification::default());
        let config = quic::presenting(
            &paired.pin,
            quic::tests_certificate(credential.expose_bytes()).into(),
            rustls::pki_types::PrivateKeyDer::Pkcs8(credential.expose_bytes().to_vec().into()),
            Arc::clone(&seen),
        )
        .unwrap();
        // The first address speaks another protocol; the second is the server.
        let stream = QuicStream::connect(
            config,
            &[other_address, real],
            Instant::now() + Duration::from_secs(5),
            &seen.verified,
        );
        assert!(stream.is_ok());
        drop(stream);
        let _ = server.join();
    }

    /// Measures a paired request's round trip over QUIC on loopback. Run it with
    /// `mise run bench:latency:quic`.
    #[test]
    #[ignore = "a measurement; run with `mise run bench:latency:quic`"]
    fn quic_request_round_trip_on_loopback() {
        const ROUNDS: usize = 2000;
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next_with(service(Refusing));
        let link = connect_quic(&fixture.endpoint(), &paired, &credential);
        let operation = nothing;
        for _ in 0..200 {
            let _ = request(&link, operation());
        }
        let mut samples: Vec<Duration> = (0..ROUNDS)
            .map(|_| {
                let started = Instant::now();
                assert!(matches!(
                    request(&link, operation()),
                    Err(RequestError::Refused(Refusal::Unsupported))
                ));
                started.elapsed()
            })
            .collect();
        samples.sort();
        let percentile = |p: usize| samples[(ROUNDS * p / 100).min(ROUNDS - 1)];
        println!(
            "QUIC request round trip on loopback over {ROUNDS} requests: p50 {:?}, p95 {:?}, p99 {:?}, max {:?}",
            percentile(50),
            percentile(95),
            percentile(99),
            samples[ROUNDS - 1]
        );
        drop(link);
        let _ = server.join().unwrap();
    }
}
