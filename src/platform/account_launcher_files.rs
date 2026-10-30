//! The account's command-line launcher, through the server's service code `install.sh` uses.
use std::path::{Path, PathBuf};

use crate::command_line_tool::{Launcher, LauncherFiles, LinkOutcome, ToolFailure};
use crate::server_connection::ServerError;
use crate::server_connection::service::{CliLauncher, Service};

pub(crate) struct AccountLauncherFiles {
    service: Service,
    launcher: PathBuf,
}

impl AccountLauncherFiles {
    /// Resolves the launcher of `identity` from the account record. Reads nothing else.
    pub(crate) fn new(identity: &str) -> Result<Self, ServerError> {
        let service = Service::for_identity(identity)?;
        let launcher = service.cli_launcher_path()?;
        Ok(Self { service, launcher })
    }
}

impl LauncherFiles for AccountLauncherFiles {
    fn read(&self) -> Result<Launcher, ToolFailure> {
        Ok(match self.service.cli_launcher().map_err(failure)? {
            CliLauncher::Missing => Launcher::Missing,
            CliLauncher::Linked => Launcher::Installed,
            CliLauncher::Stale => Launcher::Stale,
            CliLauncher::Foreign => Launcher::OtherLink,
            CliLauncher::NotLink => Launcher::OtherFile,
        })
    }

    /// Follows the approved server build. Without one, retains this desktop's own signed server
    /// package first, as the installer retains its archive.
    fn link(&self) -> Result<LinkOutcome, ToolFailure> {
        let linked = self
            .service
            .link_cli(|| {
                crate::local_startup::discover_candidate()
                    .map_err(ServerError::from)
                    .and_then(|candidate| candidate.stage())
                    .map(|installation| installation.directory)
            })
            .map_err(failure)?;
        Ok(if linked {
            LinkOutcome::Linked
        } else {
            LinkOutcome::Kept
        })
    }

    fn launcher(&self) -> PathBuf {
        self.launcher.clone()
    }

    /// Resolves links in the home directory, so a home reached through a link compares equal.
    fn directory(&self) -> PathBuf {
        let directory = self.launcher.parent().unwrap_or(Path::new("/"));
        std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_owned())
    }
}

fn failure(error: ServerError) -> ToolFailure {
    match error {
        ServerError::ServiceBusy => ToolFailure::Busy,
        ServerError::CandidateMissing => ToolFailure::NoServerPackage,
        ServerError::CandidateChanged => ToolFailure::RestartRequired,
        ServerError::RuntimeUnavailable
        | ServerError::IncompatibleProtocol
        | ServerError::MissingRequiredCapability => ToolFailure::Unverified,
        ServerError::UnsafeStorage => ToolFailure::UnsafeStorage,
        _ => ToolFailure::StorageUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_errors_become_the_tools_classifications() {
        for (error, expected) in [
            (ServerError::ServiceBusy, ToolFailure::Busy),
            (ServerError::CandidateMissing, ToolFailure::NoServerPackage),
            (ServerError::CandidateChanged, ToolFailure::RestartRequired),
            (ServerError::RuntimeUnavailable, ToolFailure::Unverified),
            (
                ServerError::MissingRequiredCapability,
                ToolFailure::Unverified,
            ),
            (ServerError::UnsafeStorage, ToolFailure::UnsafeStorage),
            (ServerError::Changed, ToolFailure::StorageUnavailable),
            (
                ServerError::StorageUnavailable,
                ToolFailure::StorageUnavailable,
            ),
        ] {
            assert_eq!(failure(error), expected);
        }
    }
}
