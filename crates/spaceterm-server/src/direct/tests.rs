use super::*;
use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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
        self.management(Management::SetPolicy(Some(
            NetworkPolicy::new(address).unwrap(),
        )))
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
        self.serve_next_with(Arc::new(Mutex::new(None)))
    }
    fn serve_next_with(
        &self,
        panes: Arc<Mutex<Option<Box<dyn crate::pane::PaneService>>>>,
    ) -> std::thread::JoinHandle<Result<(), ServerError>> {
        let state = Arc::clone(&self.state);
        let environment = crate::host::Environment {
            panes,
            direct: Some(Arc::clone(&state)),
            paths: self.paths.clone(),
            stopping: Arc::default(),
        };
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let tcp = state.lock().unwrap().accept();
                if let Some((tcp, generation)) = tcp {
                    return host::serve(tcp, generation, state, environment);
                }
                assert!(Instant::now() < deadline, "WSS accept deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
        })
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
fn malformed_links_cannot_extend_the_enrollment_lifetime_or_replace_trust_with_dns() {
    let fixture = Fixture::new();
    fixture.enable();
    let invitation = fixture.invite();
    for (field, value) in [
        ("expires", serde_json::json!(wire::now().unwrap() + 600)),
        ("endpoint", serde_json::json!("wss://example.com/spaceterm")),
        (
            "endpoint",
            serde_json::json!("ws://127.0.0.1:9231/spaceterm"),
        ),
        ("token", serde_json::json!(vec![0; 32])),
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
        Err(DirectError::Revoked)
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
    let mut connection = DirectConnection::connect(&endpoint, &target, &credential).unwrap();
    assert!(
        crate::protocol::admit_transport(
            &mut connection,
            (&untouched.client, &"Spoofed".into()),
            &target.pin.server,
            &target.pin.build,
            &[1],
        )
        .is_err()
    );
    drop(connection);
    assert_eq!(
        server.join().unwrap(),
        Err(ServerError::AuthenticationRefused)
    );
    assert_eq!(devices(&fixture), expected);

    let before = wire::now().unwrap();
    let server = fixture.serve_next();
    let mut connection = DirectConnection::connect(&endpoint, &target, &credential).unwrap();
    crate::protocol::admit_transport(
        &mut connection,
        (&target.client, &"Admitted device".into()),
        &target.pin.server,
        &target.pin.build,
        &[1],
    )
    .unwrap();
    let mut protocol = crate::protocol::Wire::terminal(&mut connection).unwrap();
    protocol
        .write(&crate::pane::Request::Direct(Management::Devices))
        .unwrap();
    let crate::pane::Reply::Direct(Ok(ManagementReply::Devices(observed))) =
        protocol.read().unwrap()
    else {
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
    let mut connection = DirectConnection::connect(&endpoint, &paired, &credential).unwrap();
    assert!(
        crate::protocol::admit_transport(
            &mut connection,
            (&[9; 32], &"Spoofed".into()),
            &paired.pin.server,
            &paired.pin.build,
            &[1]
        )
        .is_err()
    );
    drop(connection);
    assert_eq!(
        server.join().unwrap(),
        Err(ServerError::AuthenticationRefused)
    );
    let server = fixture.serve_next();
    let mut connection = DirectConnection::connect(&endpoint, &paired, &credential).unwrap();
    crate::protocol::admit_transport(
        &mut connection,
        (&paired.client, &"Real device".into()),
        &paired.pin.server,
        &paired.pin.build,
        &[1],
    )
    .unwrap();
    let mut wire = crate::protocol::Wire::terminal(&mut connection).unwrap();
    wire.write(&crate::pane::Request::Direct(Management::Status))
        .unwrap();
    let reply: crate::pane::Reply = wire.read().unwrap();
    assert!(matches!(
        reply,
        crate::pane::Reply::Direct(Ok(ManagementReply::Status(_)))
    ));
    drop(connection);
    let _ = server.join().unwrap();
}

#[test]
fn the_wss_byte_bridge_transfers_bulk_frames_larger_than_its_bounded_socket_buffers() {
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
        let mut bridge = wss::Bridge::start(
            wss::accept(listener.accept().unwrap().0, &certificate, &key).unwrap(),
        )
        .unwrap();
        let mut wire = crate::protocol::Wire::terminal(&mut bridge).unwrap();
        let bytes = wire.read_frame().unwrap();
        wire.write_frame(&bytes).unwrap();
        // Keep the owner alive until the peer consumes the final buffered bytes.
        let _ = wire.read_frame();
    });
    let mut bridge =
        wss::Bridge::start(wss::connect(&format!("wss://{address}/spaceterm"), &pin).unwrap())
            .unwrap();
    let bytes: Vec<_> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let mut wire = crate::protocol::Wire::terminal(&mut bridge).unwrap();
    wire.write_frame(&bytes).unwrap();
    assert_eq!(wire.read_frame().unwrap(), bytes);
    drop(bridge);
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
    let (registered_stream, _registered_peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let peer = fixture
        .state
        .lock()
        .unwrap()
        .register(&paired, &registered_stream, challenge.generation)
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
    fixture.management(Management::SetPolicy(None)).unwrap();
    fixture
        .management(Management::SetPolicy(status.policy))
        .unwrap();
    let mut state = fixture.state.lock().unwrap();
    assert_eq!(
        state.authorized_peer(&peer),
        Err(DirectError::Disabled),
        "an already-read request from the retired peer has no authority under the replacement policy"
    );
    assert!(matches!(
        state.authenticate(&challenge, &wire::Authentication { action, proof }),
        Err(DirectError::Disabled)
    ));
    let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    assert_eq!(
        state.register(&paired, &stream, challenge.generation),
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
    fixture.management(Management::SetPolicy(None)).unwrap();
    fixture
        .management(Management::SetPolicy(status.policy))
        .unwrap();
    let environment = crate::host::Environment {
        panes: Arc::new(Mutex::new(None)),
        direct: Some(Arc::clone(&fixture.state)),
        paths: fixture.paths.clone(),
        stopping: Arc::default(),
    };
    assert_eq!(
        host::serve(tcp, generation, Arc::clone(&fixture.state), environment),
        Err(ServerError::Direct(DirectError::Disabled))
    );
    assert!(client.join().unwrap().is_err());
}

#[test]
fn revocation_closes_every_authorized_connection_and_notifies_only_the_target_runtime_identity() {
    struct Runtime(Arc<Mutex<Vec<[u8; 32]>>>);
    impl crate::pane::PaneService for Runtime {
        fn request(
            &mut self,
            _client: [u8; 32],
            _connection: [u8; 32],
            _request: crate::pane::Request,
        ) -> crate::pane::Reply {
            crate::pane::Reply::Unchanged
        }
        fn revoked(&mut self, client: [u8; 32]) {
            self.0.lock().unwrap().push(client);
        }
        fn disconnected(&mut self, _connection: [u8; 32]) {}
        fn shutdown(&mut self) -> Result<(), ServerError> {
            Ok(())
        }
    }
    fn admit(
        endpoint: &str,
        paired: &PairedDevice,
        credential: &DeviceCredential,
    ) -> DirectConnection {
        let mut connection = DirectConnection::connect(endpoint, paired, credential).unwrap();
        crate::protocol::admit_transport(
            &mut connection,
            (&paired.client, &"Test".into()),
            &paired.pin.server,
            &paired.pin.build,
            &[1],
        )
        .unwrap();
        connection
    }
    let fixture = Fixture::new();
    fixture.enable();
    let target_key = DeviceCredential::generate().unwrap();
    let target = fixture.enroll(&target_key);
    let actor_key = DeviceCredential::generate().unwrap();
    let actor = fixture.enroll(&actor_key);
    let endpoint = fixture.endpoint();
    let retired = Arc::new(Mutex::new(Vec::new()));
    let runtime: Arc<Mutex<Option<Box<dyn crate::pane::PaneService>>>> =
        Arc::new(Mutex::new(Some(Box::new(Runtime(Arc::clone(&retired))))));
    let one = fixture.serve_next_with(Arc::clone(&runtime));
    let mut first = admit(&endpoint, &target, &target_key);
    let two = fixture.serve_next_with(Arc::clone(&runtime));
    let mut second = admit(&endpoint, &target, &target_key);
    let three = fixture.serve_next_with(Arc::clone(&runtime));
    let mut other = admit(&endpoint, &actor, &actor_key);
    {
        let mut wire = crate::protocol::Wire::terminal(&mut other).unwrap();
        wire.write(&crate::pane::Request::Direct(Management::Revoke {
            operation: crypto::random().unwrap(),
            target: target.pairing,
        }))
        .unwrap();
        let reply: crate::pane::Reply = wire.read().unwrap();
        assert!(matches!(
            reply,
            crate::pane::Reply::Direct(Ok(ManagementReply::Revoked(_)))
        ));
        wire.write(&crate::pane::Request::Direct(Management::Status))
            .unwrap();
        assert!(matches!(
            wire.read::<crate::pane::Reply>().unwrap(),
            crate::pane::Reply::Direct(Ok(ManagementReply::Status(_)))
        ));
    }
    assert_eq!(*retired.lock().unwrap(), vec![target.client]);
    assert!(
        crate::protocol::Wire::terminal(&mut first)
            .unwrap()
            .read_frame()
            .is_err()
    );
    assert!(
        crate::protocol::Wire::terminal(&mut second)
            .unwrap()
            .read_frame()
            .is_err()
    );
    fixture.management(Management::SetPolicy(None)).unwrap();
    assert!(
        crate::protocol::Wire::terminal(&mut other)
            .unwrap()
            .read_frame()
            .is_err()
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
fn a_policy_reply_retires_the_direct_issuer_without_processing_its_pipelined_request() {
    use std::io::{Read, Write};
    for change_address in [false, true] {
        let fixture = Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let paired = fixture.enroll(&credential);
        let server = fixture.serve_next();
        let mut connection =
            DirectConnection::connect(&fixture.endpoint(), &paired, &credential).unwrap();
        crate::protocol::admit_transport(
            &mut connection,
            (&paired.client, &"Policy owner".into()),
            &paired.pin.server,
            &paired.pin.build,
            &[crate::protocol::MAJOR],
        )
        .unwrap();
        let policy = change_address.then(|| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            NetworkPolicy::new(listener.local_addr().unwrap()).unwrap()
        });
        // Encode both requests before one socket write, so retirement cannot race
        // the test while it is still trying to enqueue the follow-up frame.
        let (mut encoded, mut pending) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut wire = crate::protocol::Wire::terminal(&mut encoded).unwrap();
        wire.write(&crate::pane::Request::Direct(Management::SetPolicy(
            policy.clone(),
        )))
        .unwrap();
        wire.write(&crate::pane::Request::Capabilities).unwrap();
        encoded.shutdown(std::net::Shutdown::Write).unwrap();
        let mut requests = Vec::new();
        pending.read_to_end(&mut requests).unwrap();
        connection.write_all(&requests).unwrap();
        let mut wire = crate::protocol::Wire::terminal(&mut connection).unwrap();
        let reply = wire.read::<crate::pane::Reply>().unwrap();
        assert!(
            matches!(reply, crate::pane::Reply::Direct(Ok(ManagementReply::Status(status))) if status.policy == policy)
        );
        assert!(
            wire.read_frame().is_err(),
            "the retired issuer must receive no follow-up reply"
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
    let mut connection =
        DirectConnection::connect(&fixture.endpoint(), &paired, &credential).unwrap();
    crate::protocol::admit_transport(
        &mut connection,
        (&paired.client, &"Device".into()),
        &paired.pin.server,
        &paired.pin.build,
        &[1],
    )
    .unwrap();
    let mut wire = crate::protocol::Wire::terminal(&mut connection).unwrap();
    wire.write(&crate::pane::Request::Shutdown).unwrap();
    assert!(matches!(
        wire.read::<crate::pane::Reply>().unwrap(),
        crate::pane::Reply::Refused(crate::pane::Refusal::Unsupported)
    ));
    assert!(!fixture.paths.data.join("service.json").exists());
    wire.write(&crate::pane::Request::Direct(Management::Status))
        .unwrap();
    assert!(matches!(
        wire.read::<crate::pane::Reply>().unwrap(),
        crate::pane::Reply::Direct(Ok(ManagementReply::Status(_)))
    ));
    drop(connection);
    let _ = server.join().unwrap();
}

fn setup(fixture: &Fixture, operation: OperationId, credential: &DeviceCredential) -> Management {
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let policy = status
        .policy
        .clone()
        .unwrap_or_else(|| NetworkPolicy::new(listener.local_addr().unwrap()).unwrap());
    Management::Setup {
        pin: status.pin,
        expected_policy: status.policy,
        policy,
        operation,
        public_key: credential.public_key().unwrap(),
        name: "Setup device".into(),
    }
}
fn reserve(fixture: &Fixture, request: &Management) {
    let Management::Setup {
        pin,
        expected_policy,
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
            pin,
            expected_policy,
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
    let Management::Setup { pin, .. } = &request else {
        panic!()
    };
    let mut cleanup = Enrollment::resume(&endpoint, pin.clone(), [90; 32], &credential).unwrap();
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
    let Management::Setup { pin, .. } = &request else {
        panic!()
    };
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
            if status.policy.is_none() && status.listener == ListenerState::Off
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
    for changed_field in 0..5 {
        let mut changed = request.clone();
        let Management::Setup {
            pin,
            expected_policy,
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
            _ => *expected_policy = None,
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
        pin,
        expected_policy,
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
            pin,
            expected_policy,
            policy,
            operation,
            public_key,
            name,
        }),
        Err(DirectError::PolicyChanged)
    ));

    let request = setup(&fixture, [97; 32], &credential);
    reserve(&fixture, &request);
    fixture.management(Management::SetPolicy(None)).unwrap();
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
        Ok((ManagementReply::Status(status), _)) if status.policy.is_none()
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
    fixture.management(Management::SetPolicy(None)).unwrap();
    reserve(&fixture, &request);
    assert!(matches!(
        fixture.management(request),
        Ok(ManagementReply::Setup(paired)) if paired == original
    ));
    assert!(matches!(
        fixture.management(Management::Status),
        Ok(ManagementReply::Status(status)) if status.policy.is_none()
    ));
}

#[test]
fn lost_setup_reply_uses_the_original_claim_for_key_bound_wss_cleanup() {
    let fixture = Fixture::new();
    fixture.enable();
    let credential = DeviceCredential::generate().unwrap();
    let operation = [92; 32];
    let request = setup(&fixture, operation, &credential);
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
        pin,
        expected_policy,
        policy,
        operation,
        public_key,
        name,
    } = request.clone()
    else {
        panic!()
    };
    let worker = fixture.serve_next();
    let mut connection =
        DirectConnection::connect(&fixture.endpoint(), &paired, &credential).unwrap();
    crate::protocol::admit_transport(
        &mut connection,
        (&paired.client, &"Paired client".into()),
        &paired.pin.server,
        &paired.pin.build,
        &[crate::protocol::MAJOR],
    )
    .unwrap();
    let mut wire = crate::protocol::Wire::terminal(&mut connection).unwrap();
    for action in [
        Management::PrepareSetup {
            pin: pin.clone(),
            expected_policy,
            policy,
            operation,
            public_key,
            name,
        },
        request.clone(),
        Management::CancelSetup { pin, operation },
    ] {
        wire.write(&crate::pane::Request::Direct(action)).unwrap();
        assert!(matches!(
            wire.read().unwrap(),
            crate::pane::Reply::Direct(Err(DirectError::AuthenticationRefused))
        ));
    }
    drop(connection);
    let _ = worker.join().unwrap();
    assert!(matches!(
        fixture.management(request),
        Ok(ManagementReply::Setup(_))
    ));
}
