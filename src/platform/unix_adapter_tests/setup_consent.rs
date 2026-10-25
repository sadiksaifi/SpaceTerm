use crate::ssh::provisioning::*;
use std::ffi::{OsStr, OsString};
use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;

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

struct NoPrompt;
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
        Ok(AskPassAttempt {
            lease: AskPassBrokerLease::new(Arc::new(Self)),
            observation: AskPassAttemptObservation::default(),
        })
    }
}

struct NoCredentials;
impl CredentialStore for NoCredentials {
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
}
impl SshProcessAdapter for ProcessAdapter {
    type Process = ProcessExit;
    fn spawn(
        &self,
        request: SshProcessSpawnRequest,
    ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
        let (success, stdout, stderr) = if request.arguments() == [OsString::from("-V")] {
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
            .is_some_and(|command| command.to_string_lossy().contains("setup-direct-status"))
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
            (false, vec![], vec![])
        };
        let stdout = if request.arguments() != [OsString::from("-V")] {
            [crate::ssh::command::SETUP_READY_PREAMBLE, stdout.as_slice()].concat()
        } else {
            stdout
        };
        Ok(SpawnedSshProcess::new(
            ProcessExit::new(success, Some(if success { 0 } else { 1 })),
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
        Ok(Some(*process))
    }
    fn signal(
        &self,
        _: &mut Self::Process,
        _: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        Ok(())
    }
    fn reap(&self, _: Self::Process) -> Result<(), SshProcessMechanismError> {
        Ok(())
    }
}

struct Fixture {
    catalog: PathBuf,
    adapter: ProcessAdapter,
    backend: NativeSshSetup<ProcessAdapter>,
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
        let paths = Arc::new(
            resolve_app_paths(
                &environment,
                Some("/fixture/runtime".into()),
                103,
                Arc::new(RecordingFilesystem::default()),
            )
            .unwrap(),
        );
        let direct = DirectConnections::with_credentials(
            identity(),
            Arc::clone(&paths),
            Arc::new(NoCredentials),
        );
        let adapter = ProcessAdapter {
            discovery: Arc::new(Mutex::new(discovery)),
            status: Arc::default(),
            effects: Arc::default(),
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
        }
    }
    fn setup(&self, approvals: SetupApprovals) -> Result<DirectRouteId, SetupFailure> {
        let backend: &dyn SshSetupBackend = &self.backend;
        backend.setup(
            SetupRequest {
                destination: SshDestination::new("configured-server".into()).unwrap(),
                endpoint: "192.0.2.1:9231".parse().unwrap(),
                name: "Fixture".into(),
                prompts: Arc::new(NoPrompt),
                approvals,
            },
            &CancellationToken::default(),
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
    let manifest = serde_json::json!({"schema":1,"identity":identity(),"target":target,"commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","version":"1.0.0","protocol_major":1,"capabilities":["workspace_policy"]}).to_string().into_bytes();
    let mut entries = vec![
        ("server/build.json".to_owned(), manifest, 0o644),
        (
            "server/bin/spaceterm-server".to_owned(),
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

#[test]
fn setup_reoffers_changed_signed_bytes_before_dispatching_installation() {
    let fixture = Fixture::new(b"SPACETERM_MISSING 501 x86_64-unknown-linux-gnu\n".to_vec());
    write_signed_package(&fixture.catalog, b"first authenticated executable");
    let Err(SetupFailure::InstallationOffered {
        version, approval, ..
    }) = fixture.setup(SetupApprovals::default())
    else {
        panic!("setup must offer the verified installation");
    };
    write_signed_package(&fixture.catalog, b"replacement authenticated executable");
    let result = fixture.setup(SetupApprovals {
        install: Some(approval),
        ..Default::default()
    });
    assert!(
        matches!(result, Err(SetupFailure::InstallationOffered { version: current, .. }) if current == version)
    );
    assert!(
        fixture.adapter.effects.lock().unwrap().is_empty(),
        "changed package consent reached an installation effect"
    );
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn setup_verifies_the_captured_signature_instead_of_rereading_the_catalog_path() {
    let fixture = Fixture::with_filesystem(
        b"SPACETERM_MISSING 501 x86_64-unknown-linux-gnu\n".to_vec(),
        Arc::new(ChangedSignatureSnapshot),
    );
    write_signed_package(&fixture.catalog, b"authenticated executable");
    assert_eq!(
        fixture.setup(SetupApprovals::default()),
        Err(SetupFailure::Refused(RefusalKind::InvalidArtifact))
    );
    assert!(fixture.adapter.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn setup_reoffers_a_replacement_server_before_dispatching_startup() {
    let mut discovery = AccountDiscovery {
        schema: 1,
        account: 501,
        build: identity().into(),
        server: Some([1; 32]),
        installed: true,
        enabled: true,
        running: false,
        stopped: true,
        protocol_majors: vec![1],
        capabilities: vec!["workspace_policy".into()],
        target: "x86_64-unknown-linux-gnu".into(),
        version: "1.0.0".into(),
        commit: "a".repeat(40),
        start_observation: Some(SetupStartObservation {
            account: 501,
            build: identity().into(),
            server: [1; 32],
            installation: "a".repeat(64),
            version: "1.0.0".into(),
            commit: "a".repeat(40),
            stopped: true,
        }),
    };
    let fixture = Fixture::new(serde_json::to_vec(&discovery).unwrap());
    let Err(SetupFailure::StartupRequired { approval }) = fixture.setup(SetupApprovals::default())
    else {
        panic!("setup must request startup authorization");
    };
    discovery.server = Some([2; 32]);
    discovery.start_observation.as_mut().unwrap().server = [2; 32];
    *fixture.adapter.discovery.lock().unwrap() = serde_json::to_vec(&discovery).unwrap();
    assert!(matches!(
        fixture.setup(SetupApprovals {
            start: Some(approval),
            ..Default::default()
        }),
        Err(SetupFailure::StartupRequired { .. })
    ));
    assert!(
        fixture.adapter.effects.lock().unwrap().is_empty(),
        "replacement server consent reached a startup effect"
    );
    assert_eq!(fixture.backend.process_counts().active(), 0);
}

#[test]
fn setup_reoffers_changed_server_or_policy_before_preparing_network_exposure() {
    use spaceterm_server::direct::ListenerState;
    for change_server in [true, false] {
        let mut discovery = AccountDiscovery {
            schema: 1,
            account: 501,
            build: identity().into(),
            server: Some([1; 32]),
            installed: true,
            enabled: true,
            running: true,
            stopped: false,
            protocol_majors: vec![1],
            capabilities: vec!["workspace_policy".into()],
            target: "x86_64-unknown-linux-gnu".into(),
            version: "1.0.0".into(),
            commit: "a".repeat(40),
            start_observation: None,
        };
        let fixture = Fixture::new(serde_json::to_vec(&discovery).unwrap());
        let mut status = DirectStatus {
            pin: ServerPin {
                build: identity().into(),
                server: [1; 32],
                public_key: [2; 32],
                certificate: [3; 32],
            },
            policy: None,
            server_name: "Remote".into(),
            listener: ListenerState::Off,
            network_addresses: Err(DirectError::Transport),
            setup_generation: 0,
        };
        *fixture.adapter.status.lock().unwrap() = Some(status.clone());
        let Err(SetupFailure::NetworkAuthorizationRequired { approval, .. }) =
            fixture.setup(SetupApprovals::default())
        else {
            panic!("setup must request deliberate exposure authorization");
        };
        if change_server {
            discovery.server = Some([4; 32]);
            status.pin.server = [4; 32];
            status.pin.public_key = [5; 32];
            status.pin.certificate = [6; 32];
            *fixture.adapter.discovery.lock().unwrap() = serde_json::to_vec(&discovery).unwrap();
        } else {
            // A concurrent change to the requested endpoint also invalidates the old offer.
            status.policy = Some(NetworkPolicy::new("192.0.2.1:9231".parse().unwrap()).unwrap());
        }
        *fixture.adapter.status.lock().unwrap() = Some(status);
        let result = fixture.setup(SetupApprovals {
            expose_network: Some(approval.clone()),
            ..Default::default()
        });
        assert!(
            matches!(result, Err(SetupFailure::NetworkAuthorizationRequired { approval: current, .. }) if current != approval)
        );
        assert!(
            fixture.adapter.effects.lock().unwrap().is_empty(),
            "stale network consent reached setup preparation or exposure"
        );
        assert_eq!(fixture.backend.process_counts().active(), 0);
    }
}
