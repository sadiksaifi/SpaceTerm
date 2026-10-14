//! Local bytes use the signed distribution verifier and real isolated installation.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "../../spaceterm-distribution/tests/support/account_artifact.rs"]
mod signed_fixture;

use spaceterm_client::management_cli::ManagementFailure;
use spaceterm_client_native::native::artifacts::{
    ArtifactTransport, LocalArtifactSource, ServerArtifacts,
};
use spaceterm_distribution::{
    BuildChannel, DistributionFailure, ServerArchiveVerifier, ServerPlatform, UpdateKey,
    VerifiedAccountServerArtifact,
    server_feed::{MAX_ARCHIVE_BYTES, MAX_FEED_BYTES, MAX_FEED_SIGNATURE_BYTES},
    verify_account_server_feed,
};
use spaceterm_host::{
    account_service::{
        NativeServiceProcesses, RegisteredServer, ServiceClock, ServiceFailure,
        ServiceInstallations, ServiceManager, ServiceOutcome, ServiceRequestState, ServiceTime,
        test_support::{
            AccountServiceFixture,
            native_managers::{self, CommandReply, RecordingRunner},
        },
    },
    server_installation::test_support::existing_store,
    startup::{BuildIdentity, DesiredServerState},
};
use std::{path::PathBuf, sync::Arc};

const FEED_URL: &str = "https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/latest-server-development-darwin-arm64.json";
const ARCHIVE_URL: &str = "https://github.com/sadiksaifi/SpaceTerm/releases/download/vdev.010101010101/spaceterm-server.tar.gz";

fn platform() -> ServerPlatform {
    if cfg!(target_os = "macos") {
        ServerPlatform::DarwinArm64
    } else {
        ServerPlatform::LinuxX86_64
    }
}
fn private_root() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let parent = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        spaceterm_host::account_service::TrustedAccountFacts::current()
            .unwrap()
            .home()
            .to_owned()
    };
    // Native service configuration must not inherit group-write access from umask.
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .unwrap()
}
struct LocalFixture {
    root: tempfile::TempDir,
    source: LocalArtifactSource,
    key: UpdateKey,
}
impl LocalFixture {
    fn new(build: BuildChannel) -> Self {
        let root = private_root();
        // Reuse the native header fixture used by host installation tests. No
        // downloaded or fixture executable is executed by these manager fakes.
        let mut payload = [0; 64];
        if platform() == ServerPlatform::DarwinArm64 {
            payload[..4].copy_from_slice(&0xfeedfacf_u32.to_le_bytes());
            payload[4..8].copy_from_slice(&0x0100000c_u32.to_le_bytes());
            payload[12..16].copy_from_slice(&2_u32.to_le_bytes());
        } else {
            payload[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
            payload[16..18].copy_from_slice(&2_u16.to_le_bytes());
            payload[18..20].copy_from_slice(&62_u16.to_le_bytes());
        }
        let (key, feed, signature, archive) =
            signed_fixture::artifact_fixture_with_payload(build, platform(), &payload, 1);
        let feed_path = root.path().join("candidate.json");
        let archive_path = root.path().join("candidate.tar.gz");
        std::fs::write(&feed_path, feed).unwrap();
        std::fs::write(root.path().join("candidate.json.sig"), signature).unwrap();
        std::fs::write(&archive_path, archive).unwrap();
        Self {
            root,
            source: LocalArtifactSource::new(feed_path, archive_path).unwrap(),
            key,
        }
    }
    fn verify(
        &self,
        key: &UpdateKey,
    ) -> Result<VerifiedAccountServerArtifact, DistributionFailure> {
        let feed = self.source.get(FEED_URL, MAX_FEED_BYTES).unwrap();
        let signature = self
            .source
            .get(&format!("{FEED_URL}.sig"), MAX_FEED_SIGNATURE_BYTES)
            .unwrap();
        let feed = verify_account_server_feed(
            key,
            BuildChannel::Development,
            platform(),
            &feed,
            &signature,
            1,
        )?;
        let bytes = self
            .source
            .get(ARCHIVE_URL, feed.document().archive.size as usize)
            .unwrap();
        ServerArchiveVerifier::new().verify_account_owned(key, feed, bytes)
    }
}

#[test]
fn local_source_refuses_the_test_signing_key_under_production_release_trust() {
    let fixture = LocalFixture::new(BuildChannel::Development);
    assert!(matches!(
        ServerArtifacts::new(Arc::new(fixture.source.clone()))
            .download_artifact(BuildIdentity::Development, None),
        Err(ManagementFailure::UnsafeNativeState)
    ));
}

#[test]
fn local_signed_archive_tampering_wrong_key_and_wrong_identity_are_refused() {
    let fixture = LocalFixture::new(BuildChannel::Development);
    assert!(fixture.verify(&fixture.key).is_ok());
    assert!(fixture.verify(&UpdateKey::release().unwrap()).is_err());
    let archive_path = fixture.root.path().join("candidate.tar.gz");
    let mut archive = std::fs::read(&archive_path).unwrap();
    archive[0] ^= 1;
    std::fs::write(archive_path, archive).unwrap();
    assert_eq!(
        fixture.verify(&fixture.key).err(),
        Some(DistributionFailure::ArchiveRejected)
    );
    let wrong_identity = LocalFixture::new(BuildChannel::Preflight);
    assert_eq!(
        wrong_identity.verify(&wrong_identity.key).err(),
        Some(DistributionFailure::IdentityMismatch)
    );
}

#[test]
fn explicit_local_files_refuse_symlinks_nonregular_files_and_download_bound_plus_one() {
    use std::os::unix::fs::symlink;
    let fixture = LocalFixture::new(BuildChannel::Development);
    for (name, url, bound) in [
        ("candidate.json", FEED_URL.to_owned(), MAX_FEED_BYTES),
        (
            "candidate.json.sig",
            format!("{FEED_URL}.sig"),
            MAX_FEED_SIGNATURE_BYTES,
        ),
        (
            "candidate.tar.gz",
            ARCHIVE_URL.to_owned(),
            MAX_ARCHIVE_BYTES as usize,
        ),
    ] {
        let path = fixture.root.path().join(name);
        let original = std::fs::read(&path).unwrap();
        let target = fixture.root.path().join("target");
        std::fs::write(&target, &original).unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        assert_eq!(
            fixture.source.get(&url, bound),
            Err(ManagementFailure::UnsafeNativeState)
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            fixture.source.get(&url, bound),
            Err(ManagementFailure::UnsafeNativeState)
        );
        std::fs::remove_dir(&path).unwrap();
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(bound as u64 + 1).unwrap();
        assert_eq!(
            fixture.source.get(&url, bound),
            Err(ManagementFailure::LimitExceeded)
        );
        std::fs::write(path, original).unwrap();
    }
    // O_NONBLOCK must precede regular-file admission, otherwise a FIFO can
    // block forever before the verifier sees any bytes.
    let feed_path = fixture.root.path().join("candidate.json");
    std::fs::remove_file(&feed_path).unwrap();
    use std::os::unix::ffi::OsStrExt;
    let fifo_name = std::ffi::CString::new(feed_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated pathname is live and names this isolated test root.
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    assert_eq!(
        fixture.source.get(FEED_URL, MAX_FEED_BYTES),
        Err(ManagementFailure::UnsafeNativeState)
    );
    assert!(!format!("{:?}", fixture.source).contains(fixture.root.path().to_str().unwrap()));
}

struct Installations(UpdateKey);
impl ServiceInstallations for Installations {
    fn approved_executable(
        &self,
        registration: &RegisteredServer,
    ) -> Result<PathBuf, ServiceFailure> {
        Ok(existing_store(registration, self.0.clone())?
            .retain_approved()?
            .path()
            .to_owned())
    }
    fn uninstall(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Err(ServiceFailure::SupervisorUnavailable)
    }
}
struct Clock;
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: 1,
        })
    }
}

async fn install_through_native_fake(systemd: bool) {
    let artifact = LocalFixture::new(BuildChannel::Development);
    let verified = artifact.verify(&artifact.key).unwrap();
    let root = private_root();
    let root_path = root.path().canonicalize().unwrap();
    let replies = if systemd {
        let mut replies = Vec::new();
        replies.push(CommandReply::success(b"yes\n"));
        replies.extend([
            CommandReply::success(
                format!(
                    "{}/.config/systemd/user.control {}/.config/systemd/user\n",
                    root_path.display(),
                    root_path.display()
                )
                .as_bytes(),
            ),
            CommandReply::success(b""), // reload
            CommandReply::success(b""), // enable
            CommandReply::success(b""), // start
            CommandReply::success(
                b"LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\n",
            ),
        ]);
        replies
    } else {
        vec![
            CommandReply::success(b""),                           // configure domain
            CommandReply::success(b""),                           // enable
            CommandReply::success(b""),                           // start domain
            CommandReply::success(b"disabled services = {\n}\n"), // not suppressed
            CommandReply::exit(113, b""),                         // no existing loaded service
            CommandReply::success(b""),                           // bootstrap
            CommandReply::success(b""),                           // kickstart
            CommandReply::success(b""),                           // status domain
            CommandReply::success(b"disabled services = {\n}\n"), // not suppressed
            CommandReply::success(b"service = {\n state = running\n}\n"),
        ]
    };
    let runner = Arc::new(RecordingRunner::new(replies));
    let installations = Arc::new(Installations(artifact.key.clone()));
    let managers = native_managers::managers(
        root.path(),
        runner.clone(),
        installations.clone(),
        DesiredServerState::Running,
    )
    .unwrap();
    let manager: Arc<dyn ServiceManager> = if systemd {
        Arc::new(managers.systemd)
    } else {
        Arc::new(managers.launchd)
    };
    let processes = Arc::new(
        NativeServiceProcesses::current(installations)
            .unwrap()
            .with_manager(manager.clone()),
    );
    let account_root = private_root();
    let account =
        AccountServiceFixture::new(account_root.path(), BuildIdentity::Development).unwrap();
    let supervisor = account.with_installation_trust(
        account.with_providers(manager, processes, Arc::new(Clock), None),
        artifact.key,
    );
    let request = supervisor.allocate_request().unwrap();
    let progress = supervisor
        .install(request, account.confirm_install(verified))
        .await
        .unwrap_or_else(|error| {
            panic!("install failed: {error:?}; calls={:?}", runner.arguments())
        });
    assert!(matches!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Installed { reused: false, .. })
    ));
    let registration = supervisor.discover().unwrap().unwrap();
    assert_eq!(registration.desired_state(), DesiredServerState::Running);
    assert!(registration.approved_installation().is_some());
    let calls = runner.arguments();
    if systemd {
        assert!(
            calls
                .iter()
                .any(|args| args.iter().any(|arg| arg == "start"))
        );
        assert!(
            root_path
                .join(".config/systemd/user/spaceterm-development-server.service")
                .is_file()
        );
    } else {
        assert!(
            calls
                .iter()
                .any(|args| args.first().is_some_and(|arg| arg == "bootstrap"))
        );
        assert!(
            root_path
                .join(
                    "Library/LaunchAgents/io.github.sadiksaifi.spaceterm-development.server.plist"
                )
                .is_file()
        );
    }
}

#[tokio::test]
async fn local_signed_fixture_installs_through_systemd_supervisor_fake() {
    install_through_native_fake(true).await;
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_signed_fixture_installs_through_launchd_supervisor_fake() {
    install_through_native_fake(false).await;
}
