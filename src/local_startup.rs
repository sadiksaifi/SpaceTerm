//! Account-local startup composition for the desktop host.
//!
//! The UI host runs this operation off its event thread and presents the typed outcome. Normal
//! launch integration remains with the Opus-owned phase of issue #591.
use spaceterm_server::{LocalStartup, ServerError, StartupOutcome, service::Service};
use std::path::{Path, PathBuf};

/// Prepare or reuse the current build's account service without opening credentials or UI.
///
/// Existing service policy bypasses artifact discovery. Fresh setup verifies the independent
/// archive against this desktop's exact build. Packaged applications ignore source overrides;
/// source builds require both explicit artifact paths. Only explicit Service::start reverses Stop.
pub fn run(cancelled: impl Fn() -> bool) -> Result<StartupOutcome, ServerError> {
    let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
    let startup = LocalStartup::for_identity(identity)?;
    startup.run(
        || {
            let paths = artifact_paths(
                cfg!(spaceterm_packaged),
                env!("SPACETERM_TARGET") == "aarch64-apple-darwin",
                &std::env::current_exe().map_err(|_| ServerError::RuntimeUnavailable)?,
                std::env::var_os("SPACETERM_SERVER_ARCHIVE").map(PathBuf::from),
                std::env::var_os("SPACETERM_SERVER_SIGNATURE").map(PathBuf::from),
            )?;
            Service::for_identity(identity)?
                .provision(
                    &paths.archive,
                    &paths.signature,
                    env!("SPACETERM_TARGET"),
                    env!("SPACETERM_COMMIT"),
                    env!("SPACETERM_VERSION"),
                )
                .map(|installation| installation.directory)
        },
        cancelled,
    )
}

struct ArtifactPaths {
    archive: PathBuf,
    signature: PathBuf,
}

fn artifact_paths(
    packaged: bool,
    macos: bool,
    executable: &Path,
    archive: Option<PathBuf>,
    signature: Option<PathBuf>,
) -> Result<ArtifactPaths, ServerError> {
    if packaged {
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or(ServerError::RuntimeUnavailable)?;
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
        _ => Err(ServerError::RuntimeUnavailable),
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
                Err(ServerError::RuntimeUnavailable)
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
