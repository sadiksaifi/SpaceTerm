use super::*;
use crate::connection::io::{Waker, spawn_link};
use crate::connection::link::{LinkError, RequestError, ServerLink};
use crate::connection::outbox::Outbox;
use crate::connection::service::{Dispatch, PaneService, Peer, SharedPaneService};
use crate::connection::sync::SyncWorker;
use crate::connection::unix::UnixCarrier;
use crate::connection::wss::WssCarrier;
use crate::protocol::{EffectDone, Input, Operation, Refusal, ReplyBody, RequestId, View};
use std::{
    net::TcpListener,
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

/// Admits an authenticated WSS connection as `client` and starts its link.
pub(crate) fn admitted(
    connection: DirectConnection,
    client: &[u8; 32],
    name: &str,
    paired: &PairedDevice,
) -> Result<ServerLink, ServerError> {
    let carrier = connection
        .into_transport()
        .admit((client, &name.into()), &paired.pin)?;
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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        self.management(Management::SetPolicy {
            enabled: true,
            policy: Some(NetworkPolicy::new(address).unwrap()),
        })
        .unwrap();
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
                let tcp = state.lock().unwrap().accept();
                if let Some((tcp, generation)) = tcp {
                    let (waker, wake) = Waker::new().unwrap();
                    return host::serve(tcp, generation, state, &environment, waker, wake);
                }
                assert!(Instant::now() < deadline, "WSS accept deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    }
    /// Serves the next QUIC connection of the prototype.
    #[cfg(feature = "iroh")]
    pub(crate) fn serve_next_quic(
        &self,
        panes: SharedPaneService,
    ) -> std::thread::JoinHandle<Result<(), ServerError>> {
        let state = Arc::clone(&self.state);
        let environment = self.environment(panes);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let accepted = state.lock().unwrap().accept_quic();
                if let Some((stream, generation)) = accepted {
                    let (waker, wake) = Waker::new().unwrap();
                    return host::serve_quic(stream, generation, state, &environment, waker, wake);
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
fn an_explicit_owner_rejection_crosses_wss_as_rejected_without_enrolling_the_device() {
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
        let tcp = loop {
            if let Some((tcp, _generation)) = state.lock().unwrap().accept() {
                break tcp;
            }
            assert!(Instant::now() < deadline, "WSS accept deadline");
            std::thread::sleep(Duration::from_millis(2));
        };
        let (certificate, key, challenge) = {
            let state = state.lock().unwrap();
            let (certificate, key) = state.certificate();
            (
                certificate.to_vec(),
                zeroize::Zeroizing::new(key.to_vec()),
                state.challenge().unwrap(),
            )
        };
        let mut ws = wss::accept(tcp, &certificate, &key).unwrap();
        wss::send(&mut ws, &challenge).unwrap();
        let authentication: wire::Authentication = wss::receive(&mut ws).unwrap();
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
        wss::send(&mut ws, &Ok::<_, DirectError>(wire::Answer::Claim(claim))).unwrap();
    });
    assert_eq!(enrollment.submit(), Err(DirectError::Substituted));
    server.join().unwrap();
    assert_eq!(enrollment.comparison_code(), None);
}

#[test]
fn losing_the_first_wss_reply_still_compares_the_original_authenticated_exchange() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    let credential = DeviceCredential::generate().unwrap();
    let mut enrollment =
        Enrollment::prepare(&invitation, "Original device".into(), &credential).unwrap();
    let state = Arc::clone(&fixture.state);
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let tcp = loop {
            if let Some((tcp, _generation)) = state.lock().unwrap().accept() {
                break tcp;
            }
            assert!(Instant::now() < deadline, "WSS accept deadline");
            std::thread::sleep(Duration::from_millis(2));
        };
        let (certificate, key, challenge) = {
            let state = state.lock().unwrap();
            let (certificate, key) = state.certificate();
            (
                certificate.to_vec(),
                zeroize::Zeroizing::new(key.to_vec()),
                state.challenge().unwrap(),
            )
        };
        let mut ws = wss::accept(tcp, &certificate, &key).unwrap();
        wss::send(&mut ws, &challenge).unwrap();
        let authentication: wire::Authentication = wss::receive(&mut ws).unwrap();
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
fn wss_validates_the_actual_pin_and_refuses_a_spoofed_terminal_client_identity() {
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
fn an_unadmitted_peer_cannot_send_a_message_larger_than_the_preamble_bound() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (certificate, key, pin) = {
        let state = fixture.state.lock().unwrap();
        let (certificate, key) = state.certificate();
        (
            certificate.to_vec(),
            zeroize::Zeroizing::new(key.to_vec()),
            state.pin.clone(),
        )
    };
    let server = std::thread::spawn(move || {
        let mut ws = wss::accept(listener.accept().unwrap().0, &certificate, &key).unwrap();
        wss::receive::<serde_json::Value>(&mut ws)
    });
    let mut ws = wss::connect(&format!("wss://{address}/spaceterm"), &pin).unwrap();
    let oversized = vec![b'1'; crate::connection::wss::PREAMBLE_MESSAGE_BYTES + 1];
    let _ = ws.write(tungstenite::Message::Binary(oversized.into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while ws.flush().is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(server.join().unwrap().err(), Some(DirectError::Transport));
}

#[test]
fn the_wss_carrier_moves_frames_larger_than_its_socket_buffers_in_order() {
    use crate::connection::carrier::{Carrier, Offer, Priority};
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (certificate, key, pin) = {
        let state = fixture.state.lock().unwrap();
        let (certificate, key) = state.certificate();
        (
            certificate.to_vec(),
            zeroize::Zeroizing::new(key.to_vec()),
            state.pin.clone(),
        )
    };
    // Whole frames of the largest size, each its own WebSocket message.
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
        let ws = wss::accept(listener.accept().unwrap().0, &certificate, &key).unwrap();
        let mut carrier = WssCarrier::new(ws);
        let mut queue = frames.into_iter();
        let mut held = queue.next();
        while let Some(frame) = &held {
            match carrier.offer(frame, Priority::L1).unwrap() {
                Offer::Accepted => held = queue.next(),
                Offer::Full => {
                    let fd = carrier.readiness().write.unwrap();
                    crate::connection::io::wait_fd(
                        fd,
                        true,
                        Instant::now() + Duration::from_secs(5),
                    );
                }
            }
            carrier.flush().unwrap();
        }
        while carrier.flush().unwrap().pending {
            let fd = carrier.readiness().write.unwrap();
            crate::connection::io::wait_fd(fd, true, Instant::now() + Duration::from_secs(5));
        }
        carrier.close();
        while carrier.flush().unwrap().pending {}
    });
    let ws = wss::connect(&format!("wss://{address}/spaceterm"), &pin).unwrap();
    let mut carrier = WssCarrier::new(ws);
    let mut received = Vec::new();
    while received.len() < expected.len() {
        match carrier.receive().unwrap() {
            Some(frame) => {
                let mut bytes = ((frame.body.len() + 1) as u32).to_be_bytes().to_vec();
                bytes.push(frame.kind);
                bytes.extend_from_slice(&frame.body);
                received.push(bytes);
            }
            None => {
                let fd = carrier.readiness().read;
                assert!(crate::connection::io::wait_fd(
                    fd,
                    false,
                    Instant::now() + Duration::from_secs(5)
                ));
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (certificate, key, mut challenge) = {
        let state = fixture.state.lock().unwrap();
        let (certificate, key) = state.certificate();
        (
            certificate.to_vec(),
            zeroize::Zeroizing::new(key.to_vec()),
            state.challenge().unwrap(),
        )
    };
    challenge.proof[0] ^= 1;
    let server = std::thread::spawn(move || {
        let mut ws = wss::accept(listener.accept().unwrap().0, &certificate, &key).unwrap();
        wss::send(&mut ws, &challenge).unwrap();
        assert!(
            wss::receive::<wire::Authentication>(&mut ws).is_err(),
            "the client must refuse before supplying a device proof"
        );
    });
    assert!(matches!(
        DirectConnection::connect(&format!("wss://{address}/spaceterm"), &paired, &credential),
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
    let socket = || {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (listener.accept().unwrap().0, client)
    };
    let (registered, _registered_peer) = socket();
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
            &registered,
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
    let (late, _peer) = socket();
    assert_eq!(
        state.register(
            &paired,
            challenge.generation,
            [6; 32],
            Outbox::new(Arc::new(|| {})),
            &late
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
fn a_tcp_socket_accepted_before_policy_replacement_cannot_join_the_new_listener() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let endpoint = fixture.endpoint();
    let client =
        std::thread::spawn(move || DirectConnection::connect(&endpoint, &paired, &credential));
    let deadline = Instant::now() + Duration::from_secs(5);
    let (tcp, generation) = loop {
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
            tcp,
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
        let policy = change_address.then(|| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            NetworkPolicy::new(listener.local_addr().unwrap()).unwrap()
        });
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let policy = status
        .policy
        .clone()
        .unwrap_or_else(|| NetworkPolicy::new(listener.local_addr().unwrap()).unwrap());
    operation[..8].copy_from_slice(&status.setup_generation.to_be_bytes());
    Management::Setup {
        generation: status.setup_generation,
        pin: status.pin,
        expected_policy: status.policy,
        expected_enabled: status.enabled,
        endpoint: format!("wss://{}/spaceterm", policy.bind),
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
fn prepared_setup_can_be_cancelled_over_wss_before_admission_and_after_restart() {
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
            _ => *endpoint = format!("wss://another.example:{}/spaceterm", policy.bind.port()),
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
fn lost_setup_reply_uses_the_original_claim_for_key_bound_wss_cleanup() {
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
fn paired_wss_authority_cannot_prepare_enroll_or_cancel_account_setup() {
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
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let policy = NetworkPolicy::new(socket.local_addr().unwrap()).unwrap();
    drop(socket);
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
fn dns_wss_connections_reconnect_with_the_saved_name_and_require_the_original_pin() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let port = Endpoint::from_url(&fixture.endpoint()).unwrap().port();
    let endpoint = format!("wss://localhost:{port}/spaceterm");
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
    *endpoint = format!("wss://public.example:{public_port}/spaceterm");
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
            *endpoint = format!("wss://other.example:{public_port}/spaceterm");
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
            // Both handles finish WSS authentication before either starts admission.
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
fn a_tcp_close_before_tls_is_a_transport_failure() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("wss://{}/spaceterm", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        // This peer closes before sending any TLS certificate or device admission proof.
        drop(socket);
    });
    let started = Instant::now();
    let result = DirectConnection::connect(&endpoint, &paired, &credential);
    peer.join().unwrap();
    if let Err(error) = &result
        && *error != DirectError::Transport
    {
        eprintln!(
            "DIAGNOSTIC direct-admission cycle=0 stage=tcp-close-before-tls elapsed_ms={} error={error:?}",
            started.elapsed().as_millis()
        );
    }
    assert!(
        matches!(result, Err(DirectError::Transport)),
        "a TCP close before TLS authentication must remain a transport failure"
    );
}

#[test]
fn invalid_tls_data_is_still_an_authentication_refusal() {
    use std::io::{Read, Write};

    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("wss://{}/spaceterm", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut hello = [0; 4096];
        assert!(socket.read(&mut hello).unwrap() > 0);
        // An invalid TLS content type must not turn into a retryable socket failure.
        socket.write_all(&[0xff, 0x03, 0x03, 0, 0]).unwrap();
        socket.shutdown(std::net::Shutdown::Write).unwrap();
        let _ = std::io::copy(&mut socket, &mut std::io::sink());
    });
    let result = DirectConnection::connect(&endpoint, &paired, &credential);
    peer.join().unwrap();
    assert!(matches!(result, Err(DirectError::AuthenticationRefused)));
}

/// Measures a WSS request's round trip on loopback: a paired client's request crosses TLS and
/// the WebSocket to the server's I/O thread, is dispatched, and its reply returns. Run it with
/// `mise run bench:latency:wss`.
#[test]
#[ignore = "a measurement; run with `mise run bench:latency:wss`"]
fn wss_request_round_trip_on_loopback() {
    const ROUNDS: usize = 2000;
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let paired = fixture.enroll(&credential);
    let server = fixture.serve_next();
    let connection = connect(&fixture.endpoint(), &paired, &credential);
    let operation = || Operation::ReleaseAttachment {
        attachment: crate::attachments::Attachment {
            workspace: [1; 32],
            generation: 1,
        },
    };
    // Warm the connection, the allocator and the TLS session.
    for _ in 0..200 {
        let _ = request(&connection, operation());
    }
    let mut samples: Vec<Duration> = (0..ROUNDS)
        .map(|_| {
            let started = Instant::now();
            assert!(matches!(
                request(&connection, operation()),
                Err(RequestError::Refused(Refusal::Unsupported))
            ));
            started.elapsed()
        })
        .collect();
    samples.sort();
    let percentile = |p: usize| samples[(ROUNDS * p / 100).min(ROUNDS - 1)];
    println!(
        "WSS request round trip on loopback over {ROUNDS} requests: p50 {:?}, p95 {:?}, p99 {:?}, max {:?}",
        percentile(50),
        percentile(95),
        percentile(99),
        samples[ROUNDS - 1]
    );
    assert!(
        percentile(50) < Duration::from_millis(1),
        "a WSS request takes under 1 ms on loopback"
    );
    drop(connection);
    let _ = server.join().unwrap();
}

#[cfg(feature = "iroh")]
mod quic_prototype {
    use super::*;
    use crate::connection::quic::{QuicStream, secret_key};

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
        let connection = DirectConnection::connect_quic(endpoint, paired, credential).unwrap();
        admitted(connection, &paired.client, "Test", paired).unwrap()
    }

    #[test]
    fn a_paired_device_is_admitted_over_quic_and_its_requests_round_trip() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next_quic(service(Refusing));
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

    /// iroh's TLS proves the device's key. Proofs signed by an enrolled credential do not admit a
    /// connection whose iroh key is another device's.
    #[test]
    fn quic_refuses_a_device_proof_from_another_iroh_key() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let other = DeviceCredential::generate().unwrap();
        let server = fixture.serve_next_quic(service(Refusing));
        let address = Endpoint::from_url(&fixture.endpoint()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let addresses = address.resolve(deadline).unwrap();
        let secret = secret_key(other.expose_bytes(), &other.public_key().unwrap()).unwrap();
        let mut stream =
            QuicStream::connect(secret, &paired.pin.public_key, &addresses, deadline).unwrap();
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
        assert!(answer.is_err());
        assert!(matches!(
            server.join().unwrap(),
            Err(ServerError::AuthenticationRefused)
        ));
    }

    #[test]
    fn a_client_reaches_only_the_pinned_server_key() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let mut paired = fixture.enroll(&credential);
        paired.pin.public_key = DeviceCredential::generate().unwrap().public_key().unwrap();
        assert!(matches!(
            DirectConnection::connect_quic(&fixture.endpoint(), &paired, &credential),
            Err(DirectError::Transport)
        ));
    }

    #[test]
    fn revocation_ends_a_quic_connection_with_a_typed_goodbye() {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next_quic(service(Refusing));
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

    /// Measures a QUIC request's round trip on loopback, as `wss_request_round_trip_on_loopback`
    /// does for WSS. Run it with `mise run bench:latency:quic`.
    #[test]
    #[ignore = "a measurement; run with `mise run bench:latency:quic`"]
    fn quic_request_round_trip_on_loopback() {
        const ROUNDS: usize = 2000;
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next_quic(service(Refusing));
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
