//! Account-local startup composition for the desktop host.
//!
//! The UI host runs this operation off its event thread and presents the typed outcome.
use spaceterm_core::{CoreError, LocalStartup, StartupOutcome, service::Service};
use std::path::{Path, PathBuf};

/// Prepare or reuse the account service. Only explicit Start reverses deliberate Stop.
/// Existing service policy bypasses candidate discovery.
pub fn run(cancelled: impl Fn() -> bool) -> Result<StartupOutcome, CoreError> {
    let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
    if owned_foreground() {
        if cancelled() {
            return Ok(StartupOutcome::Cancelled);
        }
        let core = spaceterm_core::MachineConnection::for_identity(identity)?;
        let status = core.refresh_cancellable(&cancelled)?;
        return Ok(StartupOutcome::Ready { core, status });
    }
    LocalStartup::for_identity(identity)?.run(
        || {
            discover_candidate()
                .map_err(CoreError::from)
                .and_then(|candidate| candidate.stage().map(|installation| installation.directory))
        },
        cancelled,
    )
}

/// Whether macOS keeps this account's SpaceTerm Core from running because its background item is
/// off in Login Items. Reading it changes nothing. The service itself refuses every start
/// meanwhile, with [`CoreError::LoginItemOff`].
pub fn background_item_off() -> bool {
    let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
    Service::for_identity(identity).is_ok_and(|service| service.login_item_off())
}

/// Opens System Settings at Login Items & Extensions, where the person turns SpaceTerm Core on.
/// Call it on the event thread, so macOS brings System Settings forward.
pub fn open_login_items() {
    #[cfg(all(target_os = "macos", feature = "desktop"))]
    crate::platform::macos_system_settings::open(crate::platform::macos_login_items::LOGIN_ITEMS);
}

/// Offers to move a packaged SpaceTerm out of the disk image it was opened from, before any
/// window opens, since only an installed SpaceTerm installs SpaceTerm Core. Returns whether
/// SpaceTerm must quit now. Call it on the event thread.
pub fn leave_disk_image() -> bool {
    #[cfg(all(target_os = "macos", feature = "desktop"))]
    {
        cfg!(spaceterm_packaged) && crate::platform::macos_app_location::leave_disk_image()
    }
    #[cfg(not(all(target_os = "macos", feature = "desktop")))]
    false
}

/// Only source Development launch may bypass the retained service lifecycle.
pub(crate) fn owned_foreground() -> bool {
    !cfg!(spaceterm_packaged)
        && crate::application_identity::ApplicationIdentity::current().directory_name()
            == "spaceterm-development"
        && std::env::var_os("SPACETERM_DEVELOPMENT_FOREGROUND").as_deref()
            == Some(std::ffi::OsStr::new("1"))
}

/// Called before graphical startup, including Workbench and update previews.
/// LaunchServices need not make the client our child: the private owner connection controls exit.
pub(crate) fn supervise_development_client() -> Result<(), CoreError> {
    if !owned_foreground() {
        return Ok(());
    }
    let endpoint =
        std::env::var_os("SPACETERM_DEVELOPMENT_OWNER_SOCKET").ok_or(CoreError::StartupFailed)?;
    let mut owner =
        std::os::unix::net::UnixStream::connect(endpoint).map_err(|_| CoreError::StartupFailed)?;
    std::thread::spawn(move || {
        use std::io::Read;
        let _ = owner.read(&mut [0]);
        std::process::exit(0);
    });
    Ok(())
}

/// Authenticated candidate declarations and the exact source pair to revalidate during staging.
#[derive(Clone, Debug)]
pub struct Candidate {
    metadata: spaceterm_core::service::CandidateMetadata,
    digest: String,
    paths: ArtifactPaths,
}

impl Candidate {
    pub fn version(&self) -> &str {
        &self.metadata.version
    }
    pub fn commit(&self) -> &str {
        &self.metadata.commit
    }
    /// The version and Update Floor its signed build document names, as a push declares them.
    pub fn release(&self) -> Result<spaceterm_core::update::ordering::Candidate, CoreError> {
        self.metadata.candidate()
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// The signed archive, verified when this candidate was discovered.
    pub fn archive(&self) -> &Path {
        &self.paths.archive
    }
    /// The archive's raw signature.
    pub fn signature(&self) -> &Path {
        &self.paths.signature
    }
    pub fn is_approved(
        &self,
        installation: &spaceterm_core::service::RetainedInstallation,
    ) -> bool {
        installation
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            == Some(self.digest())
    }

    /// Revalidate and retain this exact candidate. No server starts and no policy changes.
    pub fn stage(&self) -> Result<spaceterm_core::service::RetainedInstallation, CoreError> {
        let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
        Service::for_identity(identity)?.provision_candidate(
            &self.paths.archive,
            &self.paths.signature,
            &self.digest,
            spaceterm_core::protocol::MAJOR,
        )
    }
}

/// Bounded classifications for candidate availability and verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateError {
    /// This build includes no candidate: no pinned digest or no artifact pair to read.
    Unavailable,
    ClientRestartRequired,
    Verification(CoreError),
}

impl From<CandidateError> for CoreError {
    /// A missing candidate stays distinct from one that failed verification.
    fn from(error: CandidateError) -> Self {
        match error {
            CandidateError::Unavailable => Self::CandidateMissing,
            CandidateError::ClientRestartRequired => Self::CandidateChanged,
            CandidateError::Verification(error) => error,
        }
    }
}

/// Read the pinned candidate without staging files or changing service policy.
/// Run off the UI thread. Native executable verification occurs during explicit staging.
pub fn discover_candidate() -> Result<Candidate, CandidateError> {
    let digest = env!("SPACETERM_CLI_SHA256");
    if digest.is_empty() {
        return Err(CandidateError::Unavailable);
    }
    let paths = artifact_paths(
        cfg!(spaceterm_packaged),
        env!("SPACETERM_TARGET") == "aarch64-apple-darwin",
        &std::env::current_exe().map_err(|_| CandidateError::Unavailable)?,
        std::env::var_os("SPACETERM_CLI_ARCHIVE").map(PathBuf::from),
        std::env::var_os("SPACETERM_CLI_SIGNATURE").map(PathBuf::from),
    )?
    .present()?;
    let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
    let metadata = Service::for_identity(identity)
        .and_then(|service| {
            service.inspect_candidate(
                &paths.archive,
                &paths.signature,
                digest,
                spaceterm_core::protocol::MAJOR,
            )
        })
        .map_err(|error| match error {
            CoreError::CandidateChanged => CandidateError::ClientRestartRequired,
            other => CandidateError::Verification(other),
        })?;
    Ok(Candidate {
        metadata,
        paths,
        digest: digest.to_owned(),
    })
}

#[derive(Clone, Debug)]
struct ArtifactPaths {
    archive: PathBuf,
    signature: PathBuf,
}

impl ArtifactPaths {
    /// Both files exist. An unreadable file is left for verification to refuse.
    fn present(self) -> Result<Self, CandidateError> {
        let absent = |path: &Path| matches!(path.try_exists(), Ok(false));
        if absent(&self.archive) || absent(&self.signature) {
            return Err(CandidateError::Unavailable);
        }
        Ok(self)
    }
}

fn artifact_paths(
    packaged: bool,
    macos: bool,
    executable: &Path,
    archive: Option<PathBuf>,
    signature: Option<PathBuf>,
) -> Result<ArtifactPaths, CandidateError> {
    if packaged {
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or(CandidateError::Unavailable)?;
        let resources = if macos {
            root.join("Resources/core-artifact")
        } else {
            root.join("share/spaceterm/core-artifact")
        };
        return Ok(ArtifactPaths {
            archive: resources.join("artifact.tar.gz"),
            signature: resources.join("artifact.sig"),
        });
    }
    match (archive, signature) {
        (Some(archive), Some(signature)) => Ok(ArtifactPaths { archive, signature }),
        _ => Err(CandidateError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_build_ignores_source_artifact_overrides() {
        let paths = artifact_paths(
            true,
            true,
            Path::new("/Applications/SpaceTerm.app/Contents/MacOS/spaceterm"),
            Some("/tmp/substituted.tar.gz".into()),
            Some("/tmp/substituted.sig".into()),
        )
        .unwrap();
        assert_eq!(
            (paths.archive, paths.signature),
            (
                "/Applications/SpaceTerm.app/Contents/Resources/core-artifact/artifact.tar.gz"
                    .into(),
                "/Applications/SpaceTerm.app/Contents/Resources/core-artifact/artifact.sig".into(),
            )
        );
    }

    #[test]
    fn source_build_requires_an_explicit_complete_artifact_pair() {
        for (archive, signature) in [
            (None, None),
            (Some("/tmp/server.tar.gz".into()), None),
            (None, Some("/tmp/server.sig".into())),
        ] {
            assert!(matches!(
                artifact_paths(
                    false,
                    false,
                    Path::new("/tmp/spaceterm"),
                    archive,
                    signature
                ),
                Err(CandidateError::Unavailable)
            ));
        }
        let paths = artifact_paths(
            false,
            false,
            Path::new("/tmp/spaceterm"),
            Some("/approved/server.tar.gz".into()),
            Some("/approved/server.sig".into()),
        )
        .unwrap();
        assert_eq!(paths.archive, Path::new("/approved/server.tar.gz"));
        assert_eq!(paths.signature, Path::new("/approved/server.sig"));
    }

    /// A package whose server artifact was never included is missing, not unverifiable, so
    /// startup can say which one happened.
    #[test]
    fn an_absent_artifact_file_is_missing_rather_than_unverified() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-candidate-presence-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let paths = ArtifactPaths {
            archive: root.join("artifact.tar.gz"),
            signature: root.join("artifact.sig"),
        };
        assert!(matches!(
            paths.clone().present(),
            Err(CandidateError::Unavailable)
        ));
        std::fs::write(&paths.archive, b"archive").unwrap();
        assert!(
            matches!(paths.clone().present(), Err(CandidateError::Unavailable)),
            "an archive without its signature is still missing"
        );
        std::fs::write(&paths.signature, b"signature").unwrap();
        assert!(paths.present().is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_and_unverified_candidates_stay_distinct_core_errors() {
        assert_eq!(
            CoreError::from(CandidateError::Unavailable),
            CoreError::CandidateMissing
        );
        assert_eq!(
            CoreError::from(CandidateError::Verification(CoreError::RuntimeUnavailable)),
            CoreError::RuntimeUnavailable
        );
        assert_eq!(
            CoreError::from(CandidateError::ClientRestartRequired),
            CoreError::CandidateChanged
        );
    }

    #[test]
    fn linux_package_uses_its_independent_core_archive() {
        let paths = artifact_paths(
            true,
            false,
            Path::new("/opt/spaceterm/bin/spaceterm"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            (paths.archive, paths.signature),
            (
                "/opt/spaceterm/share/spaceterm/core-artifact/artifact.tar.gz".into(),
                "/opt/spaceterm/share/spaceterm/core-artifact/artifact.sig".into(),
            )
        );
    }
}
