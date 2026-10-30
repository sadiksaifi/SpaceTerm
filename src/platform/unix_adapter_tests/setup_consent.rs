use crate::ssh::provisioning::*;
use std::ffi::{OsStr, OsString};
use std::io::{self, Cursor, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use crate::direct_connections::credentials::{CredentialError, CredentialStore, Interaction};
use crate::platform::app_directories::AppDirectoryEnvironment;
use crate::platform::testing::{RecordingFilesystem, resolve_app_paths};
use crate::ssh::askpass::{
    AskPassAttempt, AskPassAttemptObservation, AskPassBrokerLease, AskPassLease, AskPassUnavailable,
};
use crate::ssh::process::{
    ProcessExit, ProcessSignal, SpawnedSshProcess, SshProcessMechanismError, SshProcessPipes,
    SshProcessSpawnRequest,
};

struct NoPrompt(Arc<AtomicUsize>);
impl AskPassLease for NoPrompt {
    fn entries(&self) -> Vec<(&'static str, &OsStr)> {
        vec![]
    }
    fn cancel(&self) {}
    fn capability(&self) -> &[u8] {
        b"fixture"
    }
}
impl AskPassAttemptFactory for NoPrompt {
    fn start_attempt(&self, _: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AskPassAttempt {
            lease: AskPassBrokerLease::new(Arc::new(Self(Arc::clone(&self.0)))),
            observation: AskPassAttemptObservation::default(),
        })
    }
}

struct NoCredentials;
impl CredentialStore for NoCredentials {
    fn kind(&self) -> spaceterm_connections::StoreKind {
        spaceterm_connections::StoreKind::SecretService
    }
    fn read(
        &self,
        _: &[u8; 32],
        _: Interaction,
    ) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, CredentialError> {
        panic!("consent must precede credential access")
    }
    fn insert(&self, _: &[u8; 32], _: &[u8]) -> Result<(), CredentialError> {
        panic!("consent must precede credential access")
    }
    fn remove(&self, _: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
        panic!("consent must precede credential access")
    }
}

#[derive(Clone)]
struct ProcessAdapter {
    discovery: Arc<Mutex<Vec<u8>>>,
    status: Arc<Mutex<Option<DirectStatus>>>,
    effects: Arc<Mutex<Vec<Vec<OsString>>>>,
    filesystem: Arc<RecordingFilesystem>,
    authentications: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    resolved: Arc<Mutex<Vec<u8>>>,
    effects_succeed: Arc<AtomicBool>,
    installed_discovery: Arc<Mutex<Option<Vec<u8>>>>,
    stall: Arc<(Mutex<StalledObservation>, Condvar)>,
}
enum FixtureProcess {
    Master,
    Captured(ProcessExit),
    Stalled,
}

#[derive(Default)]
struct StalledObservation {
    action: Option<&'static str>,
    entered: bool,
    killed: bool,
}

struct WithheldReady(Arc<(Mutex<StalledObservation>, Condvar)>);
impl Read for WithheldReady {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        let (state, changed) = &*self.0;
        let mut state = state.lock().unwrap();
        state.entered = true;
        changed.notify_all();
        while !state.killed {
            state = changed.wait(state).unwrap();
        }
        Ok(0)
    }
}

impl SshProcessAdapter for ProcessAdapter {
    type Process = FixtureProcess;
    fn spawn(
        &self,
        request: SshProcessSpawnRequest,
    ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        if request
            .arguments()
            .iter()
            .any(|arg| arg == "ControlMaster=yes")
        {
            self.authentications.fetch_add(1, Ordering::SeqCst);
            let control = request
                .arguments()
                .iter()
                .find_map(|argument| {
                    argument
                        .to_str()
                        .and_then(|value| value.strip_prefix("ControlPath="))
                })
                .unwrap();
            self.filesystem.create_socket(Path::new(control));
            return Ok(SpawnedSshProcess::new(
                FixtureProcess::Master,
                SshProcessPipes::new(
                    Some(Box::new(std::io::sink())),
                    Some(Box::new(Cursor::new(
                        crate::ssh::command::SETUP_READY_PREAMBLE.to_vec(),
                    ))),
                    None,
                ),
            ));
        }
        let resolve = request.arguments().first().is_some_and(|arg| arg == "-G");
        let probe = request.arguments() == [OsString::from("-V")];
        if !resolve && !probe {
            for option in ["ProxyCommand=false", "ProxyJump=none", "BatchMode=yes"] {
                assert!(
                    request.arguments().iter().any(|arg| arg == option),
                    "channel permitted a new SSH authentication"
                );
            }
            assert_eq!(self.authentications.load(Ordering::SeqCst), 1);
        }
        let stalled = self.stall.0.lock().unwrap().action.is_some_and(|action| {
            request
                .arguments()
                .last()
                .is_some_and(|command| command.to_string_lossy().contains(action))
        });
        if stalled {
            if self.stall.0.lock().unwrap().action == Some("ssh-start") {
                self.effects
                    .lock()
                    .unwrap()
                    .push(request.arguments().to_vec());
            }
            return Ok(SpawnedSshProcess::new(
                FixtureProcess::Stalled,
                SshProcessPipes::new(
                    Some(Box::new(std::io::sink())),
                    Some(Box::new(WithheldReady(Arc::clone(&self.stall)))),
                    None,
                ),
            ));
        }
        let (success, stdout, stderr) = if resolve {
            (true, self.resolved.lock().unwrap().clone(), vec![])
        } else if request.arguments() == [OsString::from("-V")] {
            (true, vec![], b"OpenSSH_9.9p2\n".to_vec())
        } else if request
            .arguments()
            .last()
            .is_some_and(|command| command.to_string_lossy().contains("ssh-discover"))
        {
            (true, self.discovery.lock().unwrap().clone(), vec![])
        } else if request
            .arguments()
            .last()
            .is_some_and(|command| command.to_string_lossy().contains("ssh-direct-status"))
        {
            let status = self.status.lock().unwrap();
            (
                true,
                serde_json::to_vec(status.as_ref().expect("status boundary not configured"))
                    .unwrap(),
                vec![],
            )
        } else {
            self.effects
                .lock()
                .unwrap()
                .push(request.arguments().to_vec());
            let action = request.arguments().last().unwrap().to_string_lossy();
            if self.effects_succeed.load(Ordering::SeqCst) && action.contains("ssh-install") {
                *self.discovery.lock().unwrap() = self
                    .installed_discovery
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| serde_json::to_vec(&discovery(false)).unwrap());
                (true, vec![], vec![])
            } else if self.effects_succeed.load(Ordering::SeqCst) && action.contains("ssh-start") {
                *self.discovery.lock().unwrap() = serde_json::to_vec(&discovery(true)).unwrap();
                (true, vec![], vec![])
            } else {
                (false, vec![], vec![])
            }
        };
        let stdout = if !probe && !resolve {
            [crate::ssh::command::SETUP_READY_PREAMBLE, stdout.as_slice()].concat()
        } else {
            stdout
        };
        Ok(SpawnedSshProcess::new(
            FixtureProcess::Captured(ProcessExit::new(success, Some(if success { 0 } else { 1 }))),
            SshProcessPipes::new(
                Some(Box::new(std::io::sink())),
                Some(Box::new(Cursor::new(stdout))),
                Some(Box::new(Cursor::new(stderr))),
            ),
        ))
    }
    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        Ok(match process {
            FixtureProcess::Master | FixtureProcess::Stalled => None,
            FixtureProcess::Captured(exit) => Some(*exit),
        })
    }
    fn signal(
        &self,
        process: &mut Self::Process,
        _: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        if matches!(process, FixtureProcess::Stalled) {
            self.stall.0.lock().unwrap().killed = true;
            self.stall.1.notify_all();
        }
        Ok(())
    }
    fn reap(&self, _: Self::Process) -> Result<(), SshProcessMechanismError> {
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Fixture {
    catalog: PathBuf,
    adapter: ProcessAdapter,
    backend: NativeSshSetup<ProcessAdapter>,
    prompts: Arc<AtomicUsize>,
}
impl Fixture {
    fn new(discovery: Vec<u8>) -> Self {
        Self::with_filesystem(
            discovery,
            Arc::new(crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem),
        )
    }
    fn with_filesystem(discovery: Vec<u8>, filesystem: Arc<dyn HostConfigFilesystem>) -> Self {
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let suffix: String = suffix.iter().map(|byte| format!("{byte:02x}")).collect();
        let catalog = std::env::temp_dir().join(format!("spaceterm-consent-{suffix}"));
        std::fs::create_dir(&catalog).unwrap();
        let environment = AppDirectoryEnvironment {
            home: Some("/fixture/home".into()),
            ..Default::default()
        };
        let secure_filesystem = Arc::new(RecordingFilesystem::default());
        let paths = Arc::new(
            resolve_app_paths(
                &environment,
                Some("/fixture/runtime".into()),
                103,
                secure_filesystem.clone(),
            )
            .unwrap(),
        );
        let direct = DirectConnections::with_credentials(
            identity(),
            Arc::new(spaceterm_connections::records::PrivateRecords::new(
                Arc::new(crate::direct_connection_storage::DesktopRecordFile::new(
                    Arc::clone(&paths),
                )),
            )),
            Arc::new(NoCredentials),
        );
        let adapter = ProcessAdapter {
            discovery: Arc::new(Mutex::new(discovery)),
            status: Arc::default(),
            effects: Arc::default(),
            filesystem: secure_filesystem,
            authentications: Arc::default(),
            effects_succeed: Arc::default(),
            installed_discovery: Arc::default(),
            stall: Arc::default(),
            active: Arc::default(),
            resolved: Arc::new(Mutex::new(
                b"hostname configured.example\nport 2222\n".to_vec(),
            )),
        };
        let backend = NativeSshSetup::new(
            NativeSshSetupConfig {
                paths,
                filesystem,
                bootstrap: Arc::new(crate::platform::unix_ssh_process::UnixSshProcessAdapter),
                home: "/fixture/home".into(),
                startup: StartupSshEnvironment::for_test(None),
                executable: OpenSshExecutable::new("/fixture/ssh".into()).unwrap(),
                adapter: adapter.clone(),
                artifacts: ServerArtifactCatalog::new(Some(catalog.clone())),
            },
            direct,
        );
        Self {
            catalog,
            adapter,
            backend,
            prompts: Arc::default(),
        }
    }
    fn setup(
        &self,
        approvals: Arc<dyn SetupApprovalHandler>,
    ) -> Result<DirectRouteId, SetupFailure> {
        self.setup_at(None, approvals)
    }
    fn setup_at(
        &self,
        address: Option<SetupAddress>,
        approvals: Arc<dyn SetupApprovalHandler>,
    ) -> Result<DirectRouteId, SetupFailure> {
        self.setup_with_cancellation(address, approvals, &CancellationToken::default())
    }
    fn setup_with_cancellation(
        &self,
        address: Option<SetupAddress>,
        approvals: Arc<dyn SetupApprovalHandler>,
        cancellation: &CancellationToken,
    ) -> Result<DirectRouteId, SetupFailure> {
        let backend: &dyn SshSetupBackend = &self.backend;
        backend.setup(
            SetupRequest {
                destination: SshDestination::new("configured-server".into()).unwrap(),
                address,
                name: "Fixture".into(),
                prompts: Arc::new(NoPrompt(Arc::clone(&self.prompts))),
                approvals,
            },
            cancellation,
            &SetupProgress::new(|_| {}),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.catalog).unwrap();
    }
}

struct ChangedSignatureSnapshot;
impl HostConfigFilesystem for ChangedSignatureSnapshot {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, HostConfigFilesystemError> {
        crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem.canonicalize(path)
    }
    fn read_file_limited(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<Vec<u8>, HostConfigFilesystemError> {
        crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem
            .read_file_limited(path, limit)
    }
    fn read_artifact_file_limited(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<Vec<u8>, HostConfigFilesystemError> {
        let mut bytes = crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem
            .read_artifact_file_limited(path, limit)?;
        if path.file_name() == Some(OsStr::new("artifact.sig")) {
            bytes[0] ^= 1;
        }
        Ok(bytes)
    }
    fn read_directory_limited(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<Vec<PathBuf>, HostConfigFilesystemError> {
        crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem
            .read_directory_limited(path, limit)
    }
}

/// Uses the distribution verifier and tracked nonproduction signing trust unchanged.
fn write_signed_package(catalog: &Path, executable: &[u8]) {
    use base64::Engine;
    let target = "x86_64-unknown-linux-gnu";
    let directory = catalog.join(target);
    std::fs::create_dir_all(&directory).unwrap();
    let manifest = serde_json::json!({"schema":1,"source_sha256":null,"identity":identity(),"target":target,"commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","version":"1.0.0","protocol_major":spaceterm_server::protocol::MAJOR,"capabilities":["workspace_policy"]}).to_string().into_bytes();
    let mut entries = vec![
        ("server/build.json".to_owned(), manifest, 0o644),
        (
            "server/bin/spaceterm".to_owned(),
            executable.to_vec(),
            0o755,
        ),
        (
            "server/share/spaceterm/terminfo/x/xterm-spaceterm".to_owned(),
            b"terminfo".to_vec(),
            0o644,
        ),
    ];
    for path in serde_json::from_str::<Vec<String>>(include_str!(
        "../../../packaging/server-resources.json"
    ))
    .unwrap()
    {
        entries.push((format!("server/{path}"), b"resource".to_vec(), 0o644));
    }
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    for (path, contents, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        archive
            .append_data(&mut header, path, contents.as_slice())
            .unwrap();
    }
    let bytes = archive.into_inner().unwrap().finish().unwrap();
    let trust: serde_json::Value = serde_json::from_str(include_str!(
        "../../../packaging/server-nonproduction-trust.json"
    ))
    .unwrap();
    let seed = base64::engine::general_purpose::STANDARD
        .decode(trust["seed"].as_str().unwrap())
        .unwrap();
    let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    std::fs::write(directory.join("artifact.sig"), signer.sign(&bytes).as_ref()).unwrap();
    std::fs::write(directory.join("artifact.sha256"), hex_digest(&bytes)).unwrap();
    std::fs::write(directory.join("artifact.tar.gz"), bytes).unwrap();
}

struct Approvals<F>(F);
impl<F: Fn(SetupOffer, &CancellationToken) -> bool + Send + Sync> SetupApprovalHandler
    for Approvals<F>
{
    fn approve(&self, offer: SetupOffer, cancellation: &CancellationToken) -> bool {
        (self.0)(offer, cancellation)
    }
}

#[test]
fn setup_reoffers_changed_signed_bytes_without_reauthenticating() {
    let fixture = Fixture::new(b"SPACETERM_MISSING 501 x86_64-unknown-linux-gnu\n".to_vec());
    write_signed_package(&fixture.catalog, b"first authenticated executable");
    let catalog = fixture.catalog.clone();
    let active = Arc::clone(&fixture.adapter.active);
    let offered = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&offered);
    let result = fixture.setup(Arc::new(Approvals(move |offer, _: &CancellationToken| {
        assert!(matches!(offer, SetupOffer::Installation { .. }));
        assert_eq!(active.load(Ordering::SeqCst), 1);
        if observed.fetch_add(1, Ordering::SeqCst) == 0 {
            write_signed_package(&catalog, b"replacement authenticated executable");
            true
        } else {
            false
        }
    })));
    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert_eq!(offered.load(Ordering::SeqCst), 2);
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn uninstalled_stopped_server_offers_installation_before_startup_without_effects_on_decline() {
    let mut remote = discovery(false);
    remote.installed = false;
    remote.enabled = false;
    remote.start_observation = None;
    let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
    write_signed_package(&fixture.catalog, b"authenticated executable");
    let offers = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&offers);

    let result = fixture.setup(Arc::new(Approvals(move |offer, _: &CancellationToken| {
        observed.lock().unwrap().push(offer);
        false
    })));

    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert_eq!(
        offers.lock().unwrap().as_slice(),
        [SetupOffer::Installation {
            version: "1.0.0".into(),
            platform: ("linux".into(), "x86_64".into()),
        }]
    );
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    let counts = fixture.backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
}

#[test]
fn uninstalled_stopped_server_reoffers_changed_retained_identity_or_stop() {
    #[derive(Clone, Copy)]
    enum Change {
        Identity,
        Stop,
    }
    for change in [Change::Identity, Change::Stop] {
        let mut remote = discovery(false);
        remote.installed = false;
        remote.enabled = false;
        remote.start_observation = None;
        let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
        write_signed_package(&fixture.catalog, b"authenticated executable");
        let source = Arc::clone(&fixture.adapter.discovery);
        let offers = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&offers);

        let result = fixture.setup(Arc::new(Approvals(move |offer, _: &CancellationToken| {
            assert!(matches!(offer, SetupOffer::Installation { .. }));
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                let mut changed = remote.clone();
                match change {
                    Change::Identity => changed.server = Some([4; 32]),
                    Change::Stop => changed.stopped = false,
                }
                *source.lock().unwrap() = serde_json::to_vec(&changed).unwrap();
                true
            } else {
                false
            }
        })));

        assert_eq!(result, Err(SetupFailure::Cancelled));
        assert_eq!(offers.load(Ordering::SeqCst), 2);
        assert!(fixture.adapter.effects.lock().unwrap().is_empty());
        assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
        let counts = fixture.backend.process_counts();
        assert_eq!(counts.active(), 0);
        assert_eq!(counts.spawned, counts.reaped);
    }
}

#[test]
fn uninstalled_stopped_server_refuses_candidate_version_or_commit_mismatch_before_an_offer() {
    for change_commit in [false, true] {
        let mut remote = discovery(false);
        remote.installed = false;
        remote.enabled = false;
        remote.start_observation = None;
        if change_commit {
            remote.commit = "b".repeat(40);
        } else {
            remote.version = "2.0.0".into();
        }
        let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
        write_signed_package(&fixture.catalog, b"authenticated executable");

        let result = fixture.setup(Arc::new(Approvals(|_, _: &CancellationToken| {
            panic!("a different retained build must not be offered as the candidate")
        })));

        assert_eq!(
            result,
            Err(SetupFailure::Incompatible(
                IncompatibilityKind::Installation
            ))
        );
        assert!(fixture.adapter.effects.lock().unwrap().is_empty());
        assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
        let counts = fixture.backend.process_counts();
        assert_eq!(counts.active(), 0);
        assert_eq!(counts.spawned, counts.reaped);
    }
}

#[test]
fn uninstalled_stopped_server_reinstall_then_start_then_network_uses_one_authentication() {
    let mut remote = discovery(false);
    remote.installed = false;
    remote.enabled = false;
    remote.start_observation = None;
    let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
    write_signed_package(&fixture.catalog, b"authenticated executable");
    fixture
        .adapter
        .effects_succeed
        .store(true, Ordering::SeqCst);
    let mut network = status();
    network.policy = Some(NetworkPolicy::new("192.0.2.9:9231".parse().unwrap()).unwrap());
    *fixture.adapter.status.lock().unwrap() = Some(network);
    let active = Arc::clone(&fixture.adapter.active);
    let offers = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&offers);

    let result = fixture.setup(Arc::new(Approvals(move |offer, _: &CancellationToken| {
        assert_eq!(active.load(Ordering::SeqCst), 1);
        let accept = !matches!(offer, SetupOffer::Network { .. });
        observed.lock().unwrap().push(offer);
        accept
    })));

    assert_eq!(
        result,
        Err(SetupFailure::Unconfirmed(SetupOperation::Start))
    );
    assert_eq!(
        offers.lock().unwrap().as_slice(),
        [
            SetupOffer::Installation {
                version: "1.0.0".into(),
                platform: ("linux".into(), "x86_64".into()),
            },
            SetupOffer::Startup,
            SetupOffer::Network {
                address: SetupAddress::parse("configured.example").unwrap(),
                bind: "192.0.2.9:9231".parse().unwrap(),
                current: Some("192.0.2.9:9231".parse().unwrap()),
                enabled: false,
            },
        ]
    );
    assert_eq!(fixture.adapter.effects.lock().unwrap().len(), 2);
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    let counts = fixture.backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
}

#[test]
fn uninstalled_stopped_server_refuses_changed_retained_facts_after_reinstallation() {
    #[derive(Clone, Copy)]
    enum Change {
        Identity,
        Stop,
        Version,
        Commit,
    }
    for change in [
        Change::Identity,
        Change::Stop,
        Change::Version,
        Change::Commit,
    ] {
        let mut remote = discovery(false);
        remote.installed = false;
        remote.enabled = false;
        remote.start_observation = None;
        let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
        write_signed_package(&fixture.catalog, b"authenticated executable");
        fixture
            .adapter
            .effects_succeed
            .store(true, Ordering::SeqCst);
        let mut installed = discovery(false);
        match change {
            Change::Identity => {
                installed.server = Some([4; 32]);
                installed.start_observation.as_mut().unwrap().server = [4; 32];
            }
            Change::Stop => {
                installed.stopped = false;
                installed.start_observation.as_mut().unwrap().stopped = false;
            }
            Change::Version => {
                installed.version = "2.0.0".into();
                installed.start_observation.as_mut().unwrap().version = "2.0.0".into();
            }
            Change::Commit => {
                installed.commit = "b".repeat(40);
                installed.start_observation.as_mut().unwrap().commit = "b".repeat(40);
            }
        }
        *fixture.adapter.installed_discovery.lock().unwrap() =
            Some(serde_json::to_vec(&installed).unwrap());

        let result = fixture.setup(Arc::new(Approvals(|offer, _: &CancellationToken| {
            assert!(matches!(offer, SetupOffer::Installation { .. }));
            true
        })));

        assert_eq!(
            result,
            Err(SetupFailure::Unconfirmed(SetupOperation::Install))
        );
        assert_eq!(fixture.adapter.effects.lock().unwrap().len(), 1);
        assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
        let counts = fixture.backend.process_counts();
        assert_eq!(counts.active(), 0);
        assert_eq!(counts.spawned, counts.reaped);
    }
}

#[test]
fn setup_verifies_the_captured_signature_instead_of_rereading_the_catalog_path() {
    let fixture = Fixture::with_filesystem(
        b"SPACETERM_MISSING 501 x86_64-unknown-linux-gnu\n".to_vec(),
        Arc::new(ChangedSignatureSnapshot),
    );
    write_signed_package(&fixture.catalog, b"authenticated executable");
    assert_eq!(
        fixture.setup(Arc::new(Approvals(|_, _: &CancellationToken| panic!(
            "invalid artifact must not be offered"
        )))),
        Err(SetupFailure::Refused(RefusalKind::InvalidArtifact))
    );
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

fn discovery(running: bool) -> AccountDiscovery {
    AccountDiscovery {
        schema: 1,
        account: 501,
        build: identity().into(),
        server: Some([1; 32]),
        installed: true,
        enabled: true,
        running,
        stopped: !running,
        protocol_majors: vec![spaceterm_server::protocol::MAJOR],
        capabilities: vec!["workspace_policy".into()],
        target: "x86_64-unknown-linux-gnu".into(),
        version: "1.0.0".into(),
        commit: "a".repeat(40),
        ssh_local_address: Some("192.0.2.9".parse().unwrap()),
        start_observation: Some(SetupStartObservation {
            account: 501,
            build: identity().into(),
            server: [1; 32],
            installation: "a".repeat(64),
            version: "1.0.0".into(),
            commit: "a".repeat(40),
            stopped: !running,
        }),
    }
}

fn status() -> DirectStatus {
    DirectStatus {
        pin: ServerPin {
            build: identity().into(),
            server: [1; 32],
            public_key: [2; 32],
            certificate: [3; 32],
        },
        policy: None,
        enabled: false,
        server_name: "Remote".into(),
        listener: spaceterm_server::direct::ListenerState::Off,
        network_addresses: Err(DirectError::Transport),
        abandoned_setups: Vec::new(),
        setup_generation: 0,
    }
}

#[test]
fn setup_reoffers_a_replacement_before_starting_with_one_authentication() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(false)).unwrap());
    let source = Arc::clone(&fixture.adapter.discovery);
    let offered = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&offered);
    let active = Arc::clone(&fixture.adapter.active);
    let result = fixture.setup(Arc::new(Approvals(move |offer, _: &CancellationToken| {
        assert_eq!(offer, SetupOffer::Startup);
        assert_eq!(active.load(Ordering::SeqCst), 1);
        if observed.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut replacement = discovery(false);
            replacement.server = Some([4; 32]);
            replacement.start_observation.as_mut().unwrap().server = [4; 32];
            *source.lock().unwrap() = serde_json::to_vec(&replacement).unwrap();
            true
        } else {
            false
        }
    })));
    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert_eq!(offered.load(Ordering::SeqCst), 2);
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn setup_derives_dns_from_effective_hostname_and_keeps_bind_and_ssh_port_separate() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(true)).unwrap());
    *fixture.adapter.status.lock().unwrap() = Some(status());
    *fixture.adapter.resolved.lock().unwrap() =
        b"hostname actual.tail.example\nport 2222\n".to_vec();
    let active = Arc::clone(&fixture.adapter.active);
    let result = fixture.setup(Arc::new(Approvals(move |offer, _: &CancellationToken| {
        assert_eq!(
            offer,
            SetupOffer::Network {
                address: SetupAddress::parse("actual.tail.example:9231").unwrap(),
                bind: "192.0.2.9:9231".parse().unwrap(),
                current: None,
                enabled: false,
            }
        );
        assert_eq!(active.load(Ordering::SeqCst), 1);
        false
    })));
    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn default_hostname_offers_the_authenticated_interface_instead_of_an_unrelated_listener() {
    let mut remote = discovery(true);
    remote.ssh_local_address = Some("100.64.0.9".parse().unwrap());
    let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
    let mut current = status();
    current.policy = Some(NetworkPolicy::new("192.168.1.9:9231".parse().unwrap()).unwrap());
    current.enabled = true;
    *fixture.adapter.status.lock().unwrap() = Some(current);
    *fixture.adapter.resolved.lock().unwrap() =
        b"hostname actual.tail.example\nport 2222\n".to_vec();

    let result = fixture.setup(Arc::new(Approvals(|offer, _: &CancellationToken| {
        assert_eq!(
            offer,
            SetupOffer::Network {
                address: SetupAddress::parse("actual.tail.example").unwrap(),
                bind: "100.64.0.9:9231".parse().unwrap(),
                current: Some("192.168.1.9:9231".parse().unwrap()),
                enabled: true,
            }
        );
        false
    })));

    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn default_hostname_preserves_a_configured_wildcard_bind_when_enabling_exposure() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(true)).unwrap());
    let mut current = status();
    current.policy = Some(NetworkPolicy::new("0.0.0.0:9231".parse().unwrap()).unwrap());
    *fixture.adapter.status.lock().unwrap() = Some(current);

    let result = fixture.setup(Arc::new(Approvals(|offer, _: &CancellationToken| {
        assert_eq!(
            offer,
            SetupOffer::Network {
                address: SetupAddress::parse("configured.example").unwrap(),
                bind: "0.0.0.0:9231".parse().unwrap(),
                current: Some("0.0.0.0:9231".parse().unwrap()),
                enabled: false,
            }
        );
        false
    })));

    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

/// A server on every interface keeps listening there when a client adds it over another of its
/// addresses, such as a tailnet name next to an earlier LAN route. Setup asks no network approval
/// and goes on to enroll the address this client reached; the fixture's storage stops it there.
#[test]
fn enabled_every_interface_exposure_enrolls_another_route_without_a_network_offer() {
    let mut remote = discovery(true);
    remote.ssh_local_address = Some("100.64.0.9".parse().unwrap());
    let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
    let mut current = status();
    current.policy = Some(NetworkPolicy::every_interface(9231).unwrap());
    current.enabled = true;
    *fixture.adapter.status.lock().unwrap() = Some(current);
    *fixture.adapter.resolved.lock().unwrap() =
        b"hostname penguin.tail.example\nport 22\n".to_vec();

    let result = fixture.setup(Arc::new(Approvals(|offer, _: &CancellationToken| {
        panic!("unexpected offer {offer:?}");
    })));

    assert_eq!(
        result,
        Err(SetupFailure::Unavailable(UnavailableKind::Storage))
    );
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn setup_reoffers_changed_policy_while_holding_the_same_authentication() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(true)).unwrap());
    *fixture.adapter.status.lock().unwrap() = Some(status());
    let source = Arc::clone(&fixture.adapter.status);
    let offered = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&offered);
    let result = fixture.setup(Arc::new(Approvals(move |offer, _: &CancellationToken| {
        assert!(matches!(offer, SetupOffer::Network { .. }));
        if observed.fetch_add(1, Ordering::SeqCst) == 0 {
            source.lock().unwrap().as_mut().unwrap().policy =
                Some(NetworkPolicy::new("192.0.2.10:9231".parse().unwrap()).unwrap());
            true
        } else {
            false
        }
    })));
    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert_eq!(offered.load(Ordering::SeqCst), 2);
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn installation_start_and_network_approval_share_one_authentication_lifecycle() {
    let fixture = Fixture::new(b"SPACETERM_MISSING 501 x86_64-unknown-linux-gnu\n".to_vec());
    write_signed_package(&fixture.catalog, b"authenticated executable");
    fixture
        .adapter
        .effects_succeed
        .store(true, Ordering::SeqCst);
    *fixture.adapter.status.lock().unwrap() = Some(status());
    let active = Arc::clone(&fixture.adapter.active);
    let offers = Arc::new(Mutex::new(vec![]));
    let observed = Arc::clone(&offers);
    let result = fixture.setup(Arc::new(Approvals(
        move |offer: SetupOffer, _: &CancellationToken| {
            assert_eq!(active.load(Ordering::SeqCst), 1);
            let accept = !matches!(offer, SetupOffer::Network { .. });
            observed.lock().unwrap().push(offer);
            accept
        },
    )));
    assert_eq!(
        result,
        Err(SetupFailure::Unconfirmed(SetupOperation::Start))
    );
    assert!(matches!(
        offers.lock().unwrap().as_slice(),
        [
            SetupOffer::Installation { .. },
            SetupOffer::Startup,
            SetupOffer::Network { .. }
        ]
    ));
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.adapter.effects.lock().unwrap().len(), 2);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn cancelling_during_approval_closes_authentication_before_setup_returns() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(false)).unwrap());
    let active = Arc::clone(&fixture.adapter.active);
    let result = fixture.setup(Arc::new(Approvals(
        move |_, cancellation: &CancellationToken| {
            assert_eq!(active.load(Ordering::SeqCst), 1);
            cancellation.cancel();
            true
        },
    )));
    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn optional_dns_override_preserves_configured_bind_with_an_independent_public_port() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(true)).unwrap());
    let mut current = status();
    current.policy = Some(NetworkPolicy::new("192.168.1.9:9231".parse().unwrap()).unwrap());
    *fixture.adapter.status.lock().unwrap() = Some(current);
    let address = SetupAddress::parse("public.example:9443").unwrap();
    let expected = address.clone();
    let result = fixture.setup_at(
        Some(address),
        Arc::new(Approvals(move |offer, _: &CancellationToken| {
            assert_eq!(
                offer,
                SetupOffer::Network {
                    address: expected.clone(),
                    bind: "192.168.1.9:9231".parse().unwrap(),
                    current: Some("192.168.1.9:9231".parse().unwrap()),
                    enabled: false
                }
            );
            false
        })),
    );
    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn optional_custom_port_uses_the_requested_port_when_no_bind_is_configured() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(true)).unwrap());
    *fixture.adapter.status.lock().unwrap() = Some(status());
    let address = SetupAddress::parse("public.example:9443").unwrap();
    let expected = address.clone();

    let result = fixture.setup_at(
        Some(address),
        Arc::new(Approvals(move |offer, _: &CancellationToken| {
            assert_eq!(
                offer,
                SetupOffer::Network {
                    address: expected.clone(),
                    bind: "192.0.2.9:9443".parse().unwrap(),
                    current: None,
                    enabled: false,
                }
            );
            false
        })),
    );

    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn fresh_explicit_server_local_ip_uses_the_requested_interface_and_port() {
    let mut remote = discovery(true);
    remote.ssh_local_address = Some("100.69.199.110".parse().unwrap());
    let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
    let mut current = status();
    current.network_addresses = Ok(spaceterm_server::direct::NetworkAddresses {
        available: vec![
            "100.69.199.110".parse().unwrap(),
            "192.168.1.41".parse().unwrap(),
        ],
        suggested: Some("100.69.199.110".parse().unwrap()),
    });
    *fixture.adapter.status.lock().unwrap() = Some(current);
    let address = SetupAddress::parse("192.168.1.41:45366").unwrap();
    let expected = address.clone();

    let result = fixture.setup_at(
        Some(address),
        Arc::new(Approvals(move |offer, _: &CancellationToken| {
            assert_eq!(
                offer,
                SetupOffer::Network {
                    address: expected.clone(),
                    bind: "192.168.1.41:45366".parse().unwrap(),
                    current: None,
                    enabled: false,
                }
            );
            false
        })),
    );

    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    let counts = fixture.backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
}

#[test]
fn fresh_explicit_nonlocal_ip_preserves_the_authenticated_bind_for_nat() {
    let mut remote = discovery(true);
    remote.ssh_local_address = Some("100.69.199.110".parse().unwrap());
    let fixture = Fixture::new(serde_json::to_vec(&remote).unwrap());
    let mut current = status();
    current.network_addresses = Ok(spaceterm_server::direct::NetworkAddresses {
        available: vec![
            "100.69.199.110".parse().unwrap(),
            "192.168.1.41".parse().unwrap(),
        ],
        suggested: Some("192.168.1.41".parse().unwrap()),
    });
    *fixture.adapter.status.lock().unwrap() = Some(current);
    let address = SetupAddress::parse("203.0.113.41:45366").unwrap();
    let expected = address.clone();

    let result = fixture.setup_at(
        Some(address),
        Arc::new(Approvals(move |offer, _: &CancellationToken| {
            assert_eq!(
                offer,
                SetupOffer::Network {
                    address: expected.clone(),
                    bind: "100.69.199.110:45366".parse().unwrap(),
                    current: None,
                    enabled: false,
                }
            );
            false
        })),
    );

    assert_eq!(result, Err(SetupFailure::Cancelled));
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    let counts = fixture.backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
}

#[test]
fn setup_discovery_times_out_before_the_slave_ready_preamble_and_reaps_the_attempt() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(true)).unwrap());
    fixture.adapter.stall.0.lock().unwrap().action = Some("ssh-discover");
    let cancellation = CancellationToken::default();
    let token = cancellation.clone();
    let (completed, outcome) = mpsc::sync_channel(1);

    let (entered, before_watchdog, result) = std::thread::scope(|scope| {
        let setup = &fixture;
        let worker = scope.spawn(move || {
            let result = setup.setup_with_cancellation(
                None,
                Arc::new(Approvals(|_, _: &CancellationToken| {
                    panic!("stalled discovery must not reach approval")
                })),
                &token,
            );
            let _ = completed.send(result);
        });
        let (state, changed) = &*fixture.adapter.stall;
        let (state, _) = changed
            .wait_timeout_while(state.lock().unwrap(), Duration::from_secs(10), |state| {
                !state.entered
            })
            .unwrap();
        let entered = state.entered;
        drop(state);
        let observed = if entered {
            outcome
                .recv_timeout(OBSERVATION + Duration::from_secs(3))
                .ok()
        } else {
            None
        };
        let before_watchdog = observed.is_some();
        // Old source needs this cancellation to release its indefinitely stalled reader.
        cancellation.cancel();
        let result = observed.unwrap_or_else(|| {
            outcome
                .recv_timeout(Duration::from_secs(3))
                .expect("watchdog cancellation must finish owned cleanup")
        });
        worker.join().unwrap();
        (entered, before_watchdog, result)
    });

    assert!(entered, "the slave must withhold its ready preamble");
    assert!(
        before_watchdog,
        "discovery exceeded its observation deadline"
    );
    assert_eq!(
        result,
        Err(SetupFailure::Unavailable(UnavailableKind::HostUnreachable))
    );
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    let counts = fixture.backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
}

#[test]
fn setup_start_times_out_before_the_slave_ready_preamble_as_unconfirmed_and_reaps_the_attempt() {
    let fixture = Fixture::new(serde_json::to_vec(&discovery(false)).unwrap());
    fixture.adapter.stall.0.lock().unwrap().action = Some("ssh-start");
    let cancellation = CancellationToken::default();
    let token = cancellation.clone();
    let (completed, outcome) = mpsc::sync_channel(1);

    let (entered, before_watchdog, result) = std::thread::scope(|scope| {
        let setup = &fixture;
        let worker = scope.spawn(move || {
            let result = setup.setup_with_cancellation(
                None,
                Arc::new(Approvals(|offer, _: &CancellationToken| {
                    assert_eq!(offer, SetupOffer::Startup);
                    true
                })),
                &token,
            );
            let _ = completed.send(result);
        });
        let (state, changed) = &*fixture.adapter.stall;
        let (state, _) = changed
            .wait_timeout_while(state.lock().unwrap(), Duration::from_secs(10), |state| {
                !state.entered
            })
            .unwrap();
        let entered = state.entered;
        drop(state);
        let observed = if entered {
            outcome
                .recv_timeout(OBSERVATION + Duration::from_secs(3))
                .ok()
        } else {
            None
        };
        let before_watchdog = observed.is_some();
        // Cleanup cancellation cannot itself count as observing the admitted startup.
        cancellation.cancel();
        let result = observed.unwrap_or_else(|| {
            outcome
                .recv_timeout(Duration::from_secs(3))
                .expect("watchdog cancellation must finish owned cleanup")
        });
        worker.join().unwrap();
        (entered, before_watchdog, result)
    });

    assert!(
        entered,
        "the dispatched startup must withhold its ready preamble"
    );
    assert!(before_watchdog, "startup exceeded its observation deadline");
    assert_eq!(
        result,
        Err(SetupFailure::Unconfirmed(SetupOperation::Start))
    );
    assert_eq!(fixture.adapter.effects.lock().unwrap().len(), 1);
    assert_eq!(fixture.adapter.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.prompts.load(Ordering::SeqCst), 1);
    let counts = fixture.backend.process_counts();
    assert_eq!(counts.active(), 0);
    assert_eq!(counts.spawned, counts.reaped);
}
