//! Account-local startup composition for the desktop host.
//!
//! The UI host runs this operation off its event thread and presents the typed outcome.
use spaceterm_server::{LocalStartup, ServerError, StartupOutcome, service::Service};
use std::path::{Path, PathBuf};

/// Prepare or reuse the account service. Only explicit Start reverses deliberate Stop.
/// Existing service policy bypasses candidate discovery.
pub fn run(cancelled: impl Fn() -> bool) -> Result<StartupOutcome, ServerError> {
    let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
    if owned_foreground() {
        if cancelled() {
            return Ok(StartupOutcome::Cancelled);
        }
        let server = spaceterm_server::LocalServer::for_identity(identity)?;
        let status = server.refresh()?;
        return Ok(StartupOutcome::Ready { server, status });
    }
    LocalStartup::for_identity(identity)?.run(
        || {
            discover_candidate()
                .map_err(ServerError::from)
                .and_then(|candidate| candidate.stage().map(|installation| installation.directory))
        },
        cancelled,
    )
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
pub(crate) fn supervise_development_client() -> Result<(), ServerError> {
    if !owned_foreground() {
        return Ok(());
    }
    let endpoint =
        std::env::var_os("SPACETERM_DEVELOPMENT_OWNER_SOCKET").ok_or(ServerError::StartupFailed)?;
    let mut owner = std::os::unix::net::UnixStream::connect(endpoint)
        .map_err(|_| ServerError::StartupFailed)?;
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
    metadata: spaceterm_server::service::CandidateMetadata,
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
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn protocol_major(&self) -> u16 {
        self.metadata.protocol_major
    }
    pub fn capabilities(&self) -> &[String] {
        &self.metadata.capabilities
    }
    pub fn is_approved(
        &self,
        installation: &spaceterm_server::service::RetainedInstallation,
    ) -> bool {
        installation
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            == Some(self.digest())
    }

    /// Revalidate and retain this exact candidate. No server starts and no policy changes.
    pub fn stage(&self) -> Result<spaceterm_server::service::RetainedInstallation, ServerError> {
        let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
        Service::for_identity(identity)?.provision_candidate(
            &self.paths.archive,
            &self.paths.signature,
            &self.digest,
            1,
            &["workspace_policy"],
        )
    }
}

/// Bounded classifications for candidate availability and verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateError {
    /// This build includes no candidate: no pinned digest or no artifact pair to read.
    Unavailable,
    ClientRestartRequired,
    Verification(ServerError),
}

impl From<CandidateError> for ServerError {
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
    let digest = env!("SPACETERM_SERVER_SHA256");
    if digest.is_empty() {
        return Err(CandidateError::Unavailable);
    }
    let paths = artifact_paths(
        cfg!(spaceterm_packaged),
        env!("SPACETERM_TARGET") == "aarch64-apple-darwin",
        &std::env::current_exe().map_err(|_| CandidateError::Unavailable)?,
        std::env::var_os("SPACETERM_SERVER_ARCHIVE").map(PathBuf::from),
        std::env::var_os("SPACETERM_SERVER_SIGNATURE").map(PathBuf::from),
    )?
    .present()?;
    let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
    let metadata = Service::for_identity(identity)
        .and_then(|service| {
            service.inspect_candidate(
                &paths.archive,
                &paths.signature,
                digest,
                1,
                &["workspace_policy"],
            )
        })
        .map_err(|error| match error {
            ServerError::CandidateChanged => CandidateError::ClientRestartRequired,
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
            root.join("Resources/server-artifact")
        } else {
            root.join("share/spaceterm/server-artifact")
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
                "/Applications/SpaceTerm.app/Contents/Resources/server-artifact/artifact.tar.gz"
                    .into(),
                "/Applications/SpaceTerm.app/Contents/Resources/server-artifact/artifact.sig"
                    .into(),
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
    fn missing_and_unverified_candidates_stay_distinct_server_errors() {
        assert_eq!(
            ServerError::from(CandidateError::Unavailable),
            ServerError::CandidateMissing
        );
        assert_eq!(
            ServerError::from(CandidateError::Verification(
                ServerError::RuntimeUnavailable
            )),
            ServerError::RuntimeUnavailable
        );
        assert_eq!(
            ServerError::from(CandidateError::ClientRestartRequired),
            ServerError::CandidateChanged
        );
    }

    #[test]
    fn linux_package_uses_its_independent_server_archive() {
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
                "/opt/spaceterm/share/spaceterm/server-artifact/artifact.tar.gz".into(),
                "/opt/spaceterm/share/spaceterm/server-artifact/artifact.sig".into(),
            )
        );
    }
}
