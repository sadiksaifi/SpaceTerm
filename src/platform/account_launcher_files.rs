//! The account's command-line launcher, through the server's service code `install.sh` uses.
use std::path::PathBuf;

use crate::command_line_tool::{Launcher, LauncherFiles, LinkOutcome, ToolFailure};
use crate::machine_connection::CoreError;
use crate::machine_connection::service::{CliLauncher, CliLink, Service};

pub(crate) struct AccountLauncherFiles {
    service: Service,
    launcher: PathBuf,
    temporary: PathBuf,
}

impl AccountLauncherFiles {
    /// Resolves the launcher of `identity` from the account record. Reads nothing else.
    pub(crate) fn new(identity: &str) -> Result<Self, CoreError> {
        let service = Service::for_identity(identity)?;
        let launcher = service.cli_launcher_path()?;
        let temporary = service.cli_temporary_path()?;
        Ok(Self {
            service,
            launcher,
            temporary,
        })
    }
}

impl LauncherFiles for AccountLauncherFiles {
    fn read(&self) -> Result<Launcher, ToolFailure> {
        Ok(match self.service.cli_launcher().map_err(failure)? {
            CliLauncher::Missing => Launcher::Missing,
            CliLauncher::Linked => Launcher::Installed,
            CliLauncher::Stale => Launcher::Stale,
            CliLauncher::Outdated => Launcher::Outdated,
            CliLauncher::Foreign => Launcher::OtherLink,
            CliLauncher::NotLink => Launcher::OtherFile,
            CliLauncher::Blocked => Launcher::Blocked,
        })
    }

    /// Follows the approved server build. Without one, retains this desktop's own signed server
    /// package first, as the installer retains its archive.
    fn link(&self) -> Result<LinkOutcome, ToolFailure> {
        let linked = self
            .service
            .link_cli(|| {
                crate::local_startup::discover_candidate()
                    .map_err(CoreError::from)
                    .and_then(|candidate| candidate.stage())
                    .map(|installation| installation.directory)
            })
            .map_err(failure)?;
        Ok(match linked {
            CliLink::Linked => LinkOutcome::Linked,
            CliLink::Kept => LinkOutcome::Kept,
            CliLink::Blocked => LinkOutcome::Blocked,
        })
    }

    fn launcher(&self) -> PathBuf {
        self.launcher.clone()
    }

    fn temporary(&self) -> PathBuf {
        self.temporary.clone()
    }
}

fn failure(error: CoreError) -> ToolFailure {
    match error {
        CoreError::ServiceBusy => ToolFailure::Busy,
        CoreError::CandidateMissing => ToolFailure::NoCorePackage,
        CoreError::CandidateChanged => ToolFailure::RestartRequired,
        CoreError::RuntimeUnavailable | CoreError::IncompatibleProtocol(_) => {
            ToolFailure::Unverified
        }
        CoreError::UnsafeStorage => ToolFailure::UnsafeStorage,
        _ => ToolFailure::StorageUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_errors_become_the_tools_classifications() {
        for (error, expected) in [
            (CoreError::ServiceBusy, ToolFailure::Busy),
            (CoreError::CandidateMissing, ToolFailure::NoCorePackage),
            (CoreError::CandidateChanged, ToolFailure::RestartRequired),
            (CoreError::RuntimeUnavailable, ToolFailure::Unverified),
            (
                CoreError::IncompatibleProtocol(
                    spaceterm_core::protocol::version::Incompatible::ListenerTooOld,
                ),
                ToolFailure::Unverified,
            ),
            (CoreError::UnsafeStorage, ToolFailure::UnsafeStorage),
            (CoreError::Changed, ToolFailure::StorageUnavailable),
            (
                CoreError::StorageUnavailable,
                ToolFailure::StorageUnavailable,
            ),
        ] {
            assert_eq!(failure(error), expected);
        }
    }
}
