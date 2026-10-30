use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use connections::{Interaction, StoreKind, UnpairState};
use spaceterm_server::direct::{
    ComparisonCode, DeviceCredential, Enrollment, PairedDevice, PairingClaim, RevocationReceipt,
    ServerPin,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

struct Fixture {
    root: PathBuf,
    store: ConnectionStore,
    credentials: Arc<Credentials>,
    network: Network,
    io: Io,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "spaceterm-connections-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir(&root).unwrap();
        let store = ConnectionStore::new(root.join("server/clients/cli"));
        let interrupted = Arc::new(AtomicBool::new(false));
        Self {
            root,
            store,
            credentials: Arc::new(Credentials::default()),
            network: Network {
                interrupted: interrupted.clone(),
                ..Default::default()
            },
            io: Io {
                interrupted,
                ..Default::default()
            },
        }
    }
    fn run(&mut self, command: &ConnectionCommand) -> CommandOutput {
        self.run_with(command, global())
    }
    fn run_with(&mut self, command: &ConnectionCommand, global: Global) -> CommandOutput {
        let provider = Provider {
            store: self.credentials.clone(),
            calls: AtomicUsize::new(0),
            unavailable: false,
        };
        Connections {
            identity: "spaceterm-development",
            global: &global,
            store: &self.store,
            credentials: &provider,
            held: &NoHeld,
            network: &self.network,
            io: &mut self.io,
        }
        .run(command)
    }
    fn insert(&self, id: u8, name: &str) -> SavedConnection {
        let key = [id; 32];
        let credential = DeviceCredential::generate().unwrap();
        self.credentials
            .insert(&key, credential.expose_bytes())
            .unwrap();
        let connection = SavedConnection {
            name: name.into(),
            route: [id; 32],
            endpoint: "quic://192.0.2.8:9231".into(),
            paired: paired(),
            credential: key,
            unpair: UnpairState::Paired,
        };
        self.store
            .commit_guarded(|rows| {
                rows.push(connection.clone());
                Ok(())
            })
            .unwrap();
        connection
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
fn global() -> Global {
    Global {
        server: None,
        agent: None,
        json: true,
        quiet: false,
        verbose: false,
        no_input: true,
        timeout: None,
    }
}
fn pin() -> ServerPin {
    ServerPin {
        build: "spaceterm-development".into(),
        server: [3; 32],
        public_key: [4; 32],
        certificate: [5; 32],
    }
}
fn paired() -> PairedDevice {
    PairedDevice {
        pin: pin(),
        pairing: [6; 32],
        client: [7; 32],
    }
}
fn invitation() -> String {
    let document = serde_json::json!({ "schema":1, "id":([2;32]), "endpoint":"quic://192.0.2.8:9231", "pin":pin(), "server_name":"Penguin Fixture", "token":([9;32]), "expires":2000000000u64, "valid_for_seconds":300 });
    format!(
        "spaceterm://pair/{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&document).unwrap())
    )
}
fn add(name: &str) -> ConnectionCommand {
    ConnectionCommand::Add {
        name: Some(name.into()),
        join: true,
        address: None,
    }
}
#[derive(Default)]
struct Credentials {
    values: Mutex<BTreeMap<[u8; 32], Zeroizing<Vec<u8>>>>,
    locked: AtomicBool,
    reads: Mutex<Vec<Interaction>>,
    attempted: Mutex<Zeroizing<Vec<u8>>>,
}
impl CredentialStore for Credentials {
    fn kind(&self) -> StoreKind {
        StoreKind::Keychain
    }
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        self.reads.lock().unwrap().push(interaction);
        if self.locked.load(Ordering::Relaxed) {
            return Err(CredentialError::InteractionRequired);
        }
        Ok(self.values.lock().unwrap().get(key).cloned())
    }
    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        *self.attempted.lock().unwrap() = Zeroizing::new(secret.to_vec());
        if self.locked.load(Ordering::Relaxed) {
            return Err(CredentialError::InteractionRequired);
        }
        let mut values = self.values.lock().unwrap();
        if values.contains_key(key) {
            return Err(CredentialError::Unavailable);
        }
        values.insert(*key, Zeroizing::new(secret.to_vec()));
        Ok(())
    }
    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError> {
        self.reads.lock().unwrap().push(interaction);
        if self.locked.load(Ordering::Relaxed) {
            return Err(CredentialError::InteractionRequired);
        }
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}
struct Provider {
    store: Arc<Credentials>,
    calls: AtomicUsize,
    unavailable: bool,
}
impl CredentialProvider for Provider {
    fn open(&self) -> Result<Arc<dyn CredentialStore>, CredentialError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.unavailable {
            Err(CredentialError::Unavailable)
        } else {
            Ok(self.store.clone())
        }
    }
}
struct NoHeld;
impl HeldOutcomes for NoHeld {
    fn count(&self, _: &[u8; 32]) -> Result<u32, Failure> {
        Ok(0)
    }
}
#[derive(Default)]
struct Network {
    submits: Mutex<Vec<[u8; 32]>>,
    cancels: Mutex<Vec<[u8; 32]>>,
    revocations: Mutex<Vec<[u8; 32]>>,
    barrier: Option<(PathBuf, String)>,
    fail_submit_once: AtomicBool,
    fail_cancel: AtomicBool,
    fail_revocation: AtomicBool,
    interrupt_finish: AtomicBool,
    interrupt_success: AtomicBool,
    interrupted: Arc<AtomicBool>,
}
impl PairingNetwork for Network {
    fn submit(&self, enrollment: &mut Enrollment) -> Result<ComparisonCode, DirectError> {
        self.submits.lock().unwrap().push(enrollment.claim_id());
        if let Some((root, name)) = &self.barrier {
            std::fs::write(root.join(format!("started-{name}")), b"").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while std::fs::read_dir(root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("started-"))
                .count()
                < 2
            {
                assert!(
                    Instant::now() < deadline,
                    "both joins must reach the network without holding connections.lock"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if self.fail_submit_once.swap(false, Ordering::Relaxed) {
            return Err(DirectError::Transport);
        }
        Ok(PairingClaim {
            id: enrollment.claim_id(),
            invitation_id: [2; 32],
            server: pin().server,
            device_public_key: [8; 32],
            device_name: "Fixture CLI".into(),
            expires_at_unix_seconds: 2000000000,
            exchange_hash: [0; 32],
        }
        .comparison_code())
    }
    fn finish(&self, _: &mut Enrollment) -> Result<Option<PairedDevice>, DirectError> {
        if self.interrupt_finish.load(Ordering::Relaxed) {
            self.interrupted.store(true, Ordering::Relaxed);
            return Ok(None);
        }
        if self.interrupt_success.load(Ordering::Relaxed) {
            self.interrupted.store(true, Ordering::Relaxed);
        }
        Ok(Some(paired()))
    }
    fn cancel(
        &self,
        enrollment: &mut Enrollment,
    ) -> Result<Option<RevocationReceipt>, DirectError> {
        self.cancels.lock().unwrap().push(enrollment.claim_id());
        if self.fail_cancel.load(Ordering::Relaxed) {
            Err(DirectError::Transport)
        } else {
            Ok(None)
        }
    }
    fn revoke(
        &self,
        connection: &SavedConnection,
        _: &DeviceCredential,
        _: Instant,
    ) -> Result<RevocationReceipt, DirectError> {
        self.revocations.lock().unwrap().push(connection.route);
        if self.fail_revocation.load(Ordering::Relaxed) {
            return Err(DirectError::Transport);
        }
        Ok(RevocationReceipt {
            server: connection.paired.pin.server,
            operation: connection.route,
            target: connection.paired.pairing,
        })
    }
}
#[derive(Default)]
struct Io {
    interrupted: Arc<AtomicBool>,
    events: Vec<ComparisonEvent>,
    reads: usize,
    broken: bool,
}
impl InteractionIo for Io {
    fn interactive(&self) -> bool {
        false
    }
    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Relaxed)
    }
    fn invitation(&mut self, _: Instant) -> Result<Zeroizing<String>, Failure> {
        self.reads += 1;
        Ok(Zeroizing::new(invitation()))
    }
    fn consent(&mut self, _: &Invitation, _: Instant) -> Result<bool, Failure> {
        panic!("noninteractive command cannot prompt")
    }
    fn confirm_removal(&mut self, _: &str, _: Instant) -> Result<bool, Failure> {
        panic!("noninteractive command cannot prompt")
    }
    fn comparison(&mut self, event: &ComparisonEvent, _: bool) -> Result<(), OutputError> {
        self.events.push(event.clone());
        if self.broken {
            Err(OutputError::BrokenPipe)
        } else {
            Ok(())
        }
    }
    fn wait(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[test]
fn connection_add_retries_the_same_claim_and_address_override_preserves_the_pin() {
    let mut fixture = Fixture::new();
    fixture
        .network
        .fail_submit_once
        .store(true, Ordering::Relaxed);
    let command = ConnectionCommand::Add {
        name: Some("penguin".into()),
        join: true,
        address: Some("Public.EXAMPLE:9443".into()),
    };
    let output = fixture.run(&command);
    assert_eq!(output.exit, 0, "{}", output.envelope);
    let records = fixture.store.load().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].endpoint, "quic://public.example:9443");
    assert_eq!(records[0].paired.pin, pin());
    assert_eq!(fixture.io.events.len(), 1);
    assert_eq!(fixture.io.events[0].data.code, "000000");
    let mut golden = output.envelope.clone();
    assert_eq!(
        golden["data"]["connection"]["route"]
            .as_str()
            .unwrap()
            .len(),
        67
    );
    golden["data"]["connection"]["route"] = format!("id:{}", "ab".repeat(32)).into();
    contract::golden::assert_golden("connection_add", &golden);
    contract::golden::assert_golden("connection_add_comparison", &fixture.io.events[0]);
    let claims = fixture.network.submits.lock().unwrap();
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0], claims[1]);
    assert!(fixture.store.pending_enrollments().unwrap().is_empty());
    assert_eq!(fixture.credentials.values.lock().unwrap().len(), 1);
}

#[test]
fn interrupted_join_cancels_only_its_retained_claim_or_reports_unconfirmed() {
    for cancellation_fails in [false, true] {
        let mut fixture = Fixture::new();
        fixture
            .network
            .interrupt_finish
            .store(true, Ordering::Relaxed);
        fixture
            .network
            .fail_cancel
            .store(cancellation_fails, Ordering::Relaxed);
        let output = fixture.run(&add("penguin"));
        assert_eq!(
            output.exit,
            if cancellation_fails { 7 } else { 130 },
            "{}",
            output.envelope
        );
        assert!(fixture.store.load().unwrap().is_empty());
        assert_eq!(
            *fixture.network.cancels.lock().unwrap(),
            *fixture.network.submits.lock().unwrap()
        );
        assert_eq!(
            fixture.store.pending_enrollments().unwrap().len(),
            usize::from(cancellation_fails)
        );
        assert_eq!(
            fixture.credentials.values.lock().unwrap().len(),
            usize::from(cancellation_fails)
        );
        if cancellation_fails {
            fixture.network.fail_cancel.store(false, Ordering::Relaxed);
            let client = PairingClient {
                store: &fixture.store,
                credentials: fixture.credentials.as_ref(),
                network: &fixture.network,
            };
            assert_eq!(
                client.recover(Instant::now() + Duration::from_secs(1)),
                Ok(0)
            );
            assert!(fixture.credentials.values.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn remove_requires_the_exact_unexpired_record_then_keeps_offline_revocation_pending() {
    let mut fixture = Fixture::new();
    let saved = fixture.insert(1, "penguin");
    let preview = fixture.run(&ConnectionCommand::Remove {
        name: saved.name.clone(),
        confirm: None,
    });
    assert_eq!(preview.exit, 3);
    let token = preview.envelope["data"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        fixture.store.load().unwrap().as_slice(),
        std::slice::from_ref(&saved)
    );
    assert!(fixture.network.revocations.lock().unwrap().is_empty());
    fixture
        .network
        .fail_revocation
        .store(true, Ordering::Relaxed);
    let removed = fixture.run(&ConnectionCommand::Remove {
        name: saved.name.clone(),
        confirm: Some(token),
    });
    assert_eq!(removed.exit, 0);
    assert_eq!(removed.envelope["data"]["revocation"], "pending");
    assert!(fixture.store.load().unwrap().is_empty());
    assert!(
        fixture
            .credentials
            .values
            .lock()
            .unwrap()
            .contains_key(&saved.credential)
    );
    let attempts = fixture.network.revocations.lock().unwrap().len();
    let listed = fixture.run(&ConnectionCommand::List);
    assert_eq!(
        listed.envelope["data"]["connections"][0]["unpair_pending"],
        true
    );
    assert_eq!(
        fixture.network.revocations.lock().unwrap().len(),
        attempts,
        "list is passive"
    );
    fixture
        .network
        .fail_revocation
        .store(false, Ordering::Relaxed);
    let other = fixture.insert(2, "other");
    let preview = fixture
        .store
        .preview_remove(&other.name, unix_now().unwrap())
        .unwrap();
    let output = fixture.run(&ConnectionCommand::Remove {
        name: other.name,
        confirm: Some(format!("c1-{}", hex(&preview.token))),
    });
    assert_eq!(output.exit, 0);
    assert!(fixture.store.pending().unwrap().is_empty());
    assert!(fixture.credentials.values.lock().unwrap().is_empty());
}

#[test]
fn stale_and_expired_removal_tokens_change_nothing() {
    let mut fixture = Fixture::new();
    let saved = fixture.insert(1, "penguin");
    let preview = fixture.store.preview_remove(&saved.name, 1).unwrap();
    let output = fixture.run(&ConnectionCommand::Remove {
        name: saved.name.clone(),
        confirm: Some(format!("c1-{}", hex(&preview.token))),
    });
    assert_eq!(output.envelope["error"]["code"], "changed");
    let preview = fixture
        .store
        .preview_remove(&saved.name, unix_now().unwrap())
        .unwrap();
    fixture
        .store
        .commit_guarded(|rows| {
            rows[0].endpoint = "quic://new.example:9443".into();
            Ok(())
        })
        .unwrap();
    let output = fixture.run(&ConnectionCommand::Remove {
        name: saved.name,
        confirm: Some(format!("c1-{}", hex(&preview.token))),
    });
    assert_eq!(output.envelope["error"]["code"], "changed");
    assert_eq!(fixture.store.load().unwrap().len(), 1);
    assert!(fixture.store.pending().unwrap().is_empty());
    assert!(fixture.network.revocations.lock().unwrap().is_empty());
}

#[test]
fn list_never_constructs_a_credential_store_or_opens_a_connection() {
    let mut fixture = Fixture::new();
    fixture.insert(1, "penguin");
    let provider = Provider {
        store: fixture.credentials.clone(),
        calls: AtomicUsize::new(0),
        unavailable: true,
    };
    let output = Connections {
        identity: "spaceterm-development",
        global: &global(),
        store: &fixture.store,
        credentials: &provider,
        held: &NoHeld,
        network: &fixture.network,
        io: &mut fixture.io,
    }
    .run(&ConnectionCommand::List);
    assert_eq!(output.exit, 0);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
    assert!(fixture.network.submits.lock().unwrap().is_empty());
    assert!(fixture.network.revocations.lock().unwrap().is_empty());
    assert_eq!(output.envelope["data"]["connections"][0]["name"], "penguin");
    #[cfg(target_os = "macos")]
    contract::golden::assert_golden("connection_list_keychain", &output.envelope);
    #[cfg(not(target_os = "macos"))]
    contract::golden::assert_golden("connection_list_secret_service", &output.envelope);
}

#[test]
fn no_store_and_locked_no_input_paths_have_bounded_errors_and_no_plaintext() {
    for unavailable in [false, true] {
        let mut fixture = Fixture::new();
        fixture.credentials.locked.store(true, Ordering::Relaxed);
        let provider = Provider {
            store: fixture.credentials.clone(),
            calls: AtomicUsize::new(0),
            unavailable,
        };
        let output = Connections {
            identity: "spaceterm-development",
            global: &global(),
            store: &fixture.store,
            credentials: &provider,
            held: &NoHeld,
            network: &fixture.network,
            io: &mut fixture.io,
        }
        .run(&add("penguin"));
        assert_eq!(output.exit, if unavailable { 5 } else { 3 });
        assert_eq!(
            output.envelope["error"]["code"],
            if unavailable {
                "credential_unavailable"
            } else {
                "credential_interaction_required"
            }
        );
        assert!(!output.envelope.to_string().contains(&invitation()));
        assert!(fixture.network.submits.lock().unwrap().is_empty());
        assert!(fixture.credentials.values.lock().unwrap().is_empty());
        assert!(
            fixture
                .credentials
                .reads
                .lock()
                .unwrap()
                .iter()
                .all(|interaction| *interaction == Interaction::Quiet)
        );
        if !unavailable {
            let bytes = std::fs::read(fixture.store_path()).unwrap();
            assert!(
                !bytes
                    .windows(invitation().len())
                    .any(|part| part == invitation().as_bytes())
            );
            let secret = fixture.credentials.attempted.lock().unwrap();
            assert!(!secret.is_empty());
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|part| part == secret.as_slice())
            );
        }
    }
}
impl Fixture {
    fn store_path(&self) -> PathBuf {
        self.root.join("server/clients/cli/connections.json")
    }
}

#[test]
fn invalid_usage_and_missing_consent_precede_every_effect_and_never_echo_input() {
    for command in [
        ConnectionCommand::Add {
            name: Some("local".into()),
            join: true,
            address: None,
        },
        ConnectionCommand::Add {
            name: None,
            join: true,
            address: Some("private-invalid-address/path".into()),
        },
        ConnectionCommand::Add {
            name: None,
            join: false,
            address: None,
        },
        ConnectionCommand::Remove {
            name: "penguin".into(),
            confirm: Some("private-invalid-token".into()),
        },
    ] {
        let mut fixture = Fixture::new();
        let output = fixture.run(&command);
        assert!([2, 3].contains(&output.exit));
        assert_eq!(fixture.io.reads, 0);
        assert!(!fixture.store_path().exists());
        assert!(!output.envelope.to_string().contains("private-invalid"));
    }
}

#[test]
fn a_broken_comparison_pipe_cancels_the_unfinished_join_and_exits_silently() {
    let mut fixture = Fixture::new();
    fixture.io.broken = true;
    let output = fixture.run(&add("penguin"));
    assert_eq!(output.exit, 141);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        output.render((&global()).into(), &mut stdout, &mut stderr),
        141
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(fixture.store.pending_enrollments().unwrap().is_empty());
    assert!(fixture.credentials.values.lock().unwrap().is_empty());
}

#[test]
fn server_selection_is_local_without_state_and_remote_prefix_ambiguity_stays_in_data() {
    let fixture = Fixture::new();
    assert_eq!(
        route::selected(Some("local"), Some("penguin")).unwrap(),
        "local"
    );
    assert_eq!(route::selected(None, Some("penguin")).unwrap(), "penguin");
    assert!(
        route::resolve("local", &fixture.store)
            .unwrap_or_else(|_| panic!("local must not read storage"))
            .is_local()
    );
    assert!(!fixture.store_path().exists());
    let mut first = fixture.insert(1, "private-first-label");
    let mut second = fixture.insert(2, "private-second-label");
    first.route = [0xaa; 32];
    second.route = [0xaa; 32];
    second.route[31] = 1;
    fixture
        .store
        .commit_guarded(|rows| {
            *rows = vec![first, second];
            Ok(())
        })
        .unwrap();
    let error = route::resolve("id:aaaaaa", &fixture.store).err().unwrap();
    assert_eq!(error.failure.code, Code::AmbiguousTarget);
    assert_eq!(error.data.candidates.len(), 2);
    let envelope = Envelope::error("list", None, error.failure, error.data);
    let json = serde_json::to_value(envelope).unwrap();
    assert!(!json["error"].to_string().contains("private-"));
    assert!(json["data"].to_string().contains("private-first-label"));
    let selected = route::resolve("private-second-label", &fixture.store)
        .unwrap_or_else(|_| panic!("exact saved name"));
    assert_eq!(selected.reference(&[0; 32]).id, hex(&pin().server));
}

#[test]
fn confirmed_removal_has_the_frozen_envelope() {
    let mut fixture = Fixture::new();
    let connection = fixture.insert(1, "penguin");
    let preview = fixture
        .store
        .preview_remove(&connection.name, unix_now().unwrap())
        .unwrap();
    let output = fixture.run(&ConnectionCommand::Remove {
        name: connection.name,
        confirm: Some(format!("c1-{}", hex(&preview.token))),
    });
    assert_eq!(output.exit, 0);
    contract::golden::assert_golden("connection_remove", &output.envelope);
}

#[test]
fn a_live_join_is_skipped_by_recovery_and_a_published_join_is_never_cancelled() {
    let fixture = Fixture::new();
    let invitation = Invitation::parse(&invitation()).unwrap();
    let first = PairingClient {
        store: &fixture.store,
        credentials: fixture.credentials.as_ref(),
        network: &fixture.network,
    };
    let second = PairingClient {
        store: &fixture.store,
        credentials: fixture.credentials.as_ref(),
        network: &fixture.network,
    };
    let mut join = first
        .begin(
            &invitation,
            "penguin".into(),
            "Fixture CLI".into(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    join.submit().unwrap();
    assert_eq!(
        second.recover(Instant::now() + Duration::from_secs(1)),
        Ok(0)
    );
    assert!(fixture.network.cancels.lock().unwrap().is_empty());
    join.finish().unwrap().unwrap();
    drop(join);
    assert_eq!(
        second.recover(Instant::now() + Duration::from_secs(1)),
        Ok(0)
    );
    assert!(fixture.network.cancels.lock().unwrap().is_empty());
    assert_eq!(fixture.store.load().unwrap().len(), 1);
}

#[test]
fn connection_process_add() {
    let Some(root) = std::env::var_os("SPACETERM_TEST_CONNECTION_ROOT") else {
        return;
    };
    let name = std::env::var("SPACETERM_TEST_CONNECTION_NAME").unwrap();
    let mut fixture = Fixture::new();
    let root = PathBuf::from(root);
    fixture.store = ConnectionStore::new(root.join("server/clients/cli"));
    fixture.network.barrier = Some((root, name.clone()));
    let output = fixture.run(&add(&name));
    assert_eq!(output.exit, 0, "{}", output.envelope);
}

#[test]
fn concurrent_connection_add_processes_preserve_the_desktop_fixture() {
    let fixture = Fixture::new();
    let bytes =
        include_bytes!("../../../spaceterm-connections/tests/fixtures/desktop-connections.json");
    let desktop = fixture.root.join("direct-device-connections.json");
    std::fs::write(&desktop, bytes).unwrap();
    let mut children = Vec::new();
    for name in ["first", "second"] {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "connection::tests::connection_process_add",
                    "--nocapture",
                ])
                .env("SPACETERM_TEST_CONNECTION_ROOT", &fixture.root)
                .env("SPACETERM_TEST_CONNECTION_NAME", name)
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let rows = fixture.store.load().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.name == "first"));
    assert!(rows.iter().any(|row| row.name == "second"));
    assert_eq!(std::fs::read(&desktop).unwrap(), bytes);
    assert!(fixture.store.pending_enrollments().unwrap().is_empty());
}

#[test]
fn interruption_after_confirmed_publication_reports_the_saved_connection_without_unpairing() {
    let mut fixture = Fixture::new();
    fixture
        .network
        .interrupt_success
        .store(true, Ordering::Relaxed);
    let output = fixture.run(&add("penguin"));
    assert_eq!(output.exit, 130);
    assert_eq!(output.envelope["error"]["code"], "interrupted");
    assert_eq!(output.envelope["data"]["connection"]["name"], "penguin");
    assert_eq!(fixture.store.load().unwrap().len(), 1);
    assert!(fixture.store.pending_enrollments().unwrap().is_empty());
    assert!(fixture.network.cancels.lock().unwrap().is_empty());
}
