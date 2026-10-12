//! Desktop identity and SSH conversion for the headless shell launch planner.

use std::path::{Path, PathBuf};

use super::shell_integration::{ShellEnvironment, ShellIntegrationMode, ShellIntegrationPolicy};
use crate::ssh::command::SshCommandSpec;
use crate::terminal::identity;
use spaceterm_host::shell_launch::{
    PreparedRemoteShellCommand, RemoteShellLaunchParts, ShellEnvironmentDenylist,
    ShellTerminalIdentity, ShellTerminfoIdentity,
};
pub(crate) use spaceterm_host::shell_launch::{PreparedShellLaunch, ShellLaunchFailure};

#[derive(Clone)]
pub(crate) struct ShellLaunchPlanner {
    planner: spaceterm_host::shell_launch::ShellLaunchPlanner,
    resources: PathBuf,
}

impl ShellLaunchPlanner {
    pub(crate) fn new(
        shell: PathBuf,
        resources: PathBuf,
        mode: ShellIntegrationMode,
        inherited: ShellEnvironment,
        policy: ShellIntegrationPolicy,
        host_runtime_environment: &'static [&'static str],
    ) -> Self {
        let terminal_identity = ShellTerminalIdentity {
            fallback_term: identity::TERM_FALLBACK,
            colorterm: identity::COLORTERM,
            program_name: identity::COMPATIBILITY_PROGRAM_NAME,
            program_version: identity::PROGRAM_VERSION,
            terminfo: Some(ShellTerminfoIdentity {
                directory: resources.join("terminfo"),
                terminal_name: identity::TERM_NAME,
            }),
        };
        let denylist = host_runtime_environment
            .iter()
            .fold(ShellEnvironmentDenylist::default(), |denylist, name| {
                denylist.with_name(*name)
            });
        Self {
            planner: spaceterm_host::shell_launch::ShellLaunchPlanner::new(
                shell,
                resources.clone(),
                mode,
                inherited,
                policy,
                terminal_identity,
            )
            .with_environment_denylist(denylist),
            resources,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(shell: PathBuf, resources: PathBuf) -> Self {
        Self::new(
            shell,
            resources,
            ShellIntegrationMode::Automatic,
            ShellEnvironment::default(),
            ShellIntegrationPolicy::fixture(),
            &[],
        )
    }

    #[cfg(test)]
    pub(crate) fn with_environment(
        mut self,
        mode: ShellIntegrationMode,
        inherited: ShellEnvironment,
    ) -> Self {
        self.planner = self.planner.with_environment(mode, inherited);
        self
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.planner.fallback_title()
    }

    pub(crate) fn local(
        &self,
        directory: &Path,
    ) -> Result<PreparedShellLaunch, ShellLaunchFailure> {
        let launch_identity = identity::launch_identity(&self.resources);
        self.planner
            .clone()
            .with_terminal_identity(ShellTerminalIdentity {
                fallback_term: launch_identity.term,
                colorterm: identity::COLORTERM,
                program_name: identity::COMPATIBILITY_PROGRAM_NAME,
                program_version: identity::PROGRAM_VERSION,
                terminfo: launch_identity
                    .terminfo
                    .map(|directory| ShellTerminfoIdentity {
                        directory,
                        terminal_name: identity::TERM_NAME,
                    }),
            })
            .local(directory)
    }
}

impl PreparedRemoteShellCommand for SshCommandSpec {
    fn into_remote_shell_parts(self) -> Result<RemoteShellLaunchParts, ShellLaunchFailure> {
        let (executable, arguments, environment) = self
            .into_pane_launch_parts()
            .map_err(|_| ShellLaunchFailure::TerminalSessionChannelUnavailable)?;
        let (working_directory, environment) = environment.into_pane_launch_environment();
        Ok(RemoteShellLaunchParts {
            executable,
            arguments,
            working_directory,
            environment,
            terminal_name: identity::TERM_FALLBACK,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RemoteDirectory, SshDestination};
    use crate::ssh::command::{
        PreparedSshTerminalSessionChannelError, RemotePaneShellCommandBuilder,
        ValidatedRemoteLoginShell,
    };

    #[test]
    fn desktop_launch_rechecks_packaged_identity_and_exports_the_engine_version() {
        let resources = crate::terminal::testing::ShellResourcesFixture::new();
        let planner =
            ShellLaunchPlanner::for_test("/fixture/zsh".into(), resources.path().to_owned());
        let directory = std::env::temp_dir();
        let fallback = planner.local(&directory).unwrap();
        assert_eq!(fallback.terminal_name(), identity::TERM_FALLBACK);
        let entry = resources.path().join("terminfo/x/xterm-spaceterm");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, b"compiled fixture").unwrap();
        let packaged = planner.local(&directory).unwrap();
        assert_eq!(packaged.terminal_name(), identity::TERM_NAME);
        assert!(packaged.environment().contains(&(
            "TERM_PROGRAM_VERSION".into(),
            identity::PROGRAM_VERSION.into(),
        )));
        std::fs::remove_file(entry).unwrap();
        assert_eq!(
            planner.local(&directory).unwrap().terminal_name(),
            identity::TERM_FALLBACK
        );
    }

    #[test]
    fn remote_launch_preserves_exact_command_consumes_once_and_excludes_local_markers() {
        let context = crate::ssh::testing::SshConnectionFixture::new(
            SshDestination::new("user@remote".to_owned()).unwrap(),
        );
        let directory = RemoteDirectory::new("~/private project".to_owned()).unwrap();
        let shell = ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap();
        let command = RemotePaneShellCommandBuilder::new(&directory, &shell)
            .build()
            .unwrap();
        let channel = context.prepare_terminal_session_channel(command);
        let taken = channel.take().unwrap();
        let expected_arguments = taken.arguments().to_vec();
        let launch = PreparedShellLaunch::remote(&std::env::temp_dir(), taken).unwrap();
        assert!(matches!(
            channel.take(),
            Err(PreparedSshTerminalSessionChannelError::AlreadyConsumed)
        ));
        assert_eq!(launch.executable(), "/test/ssh");
        assert_eq!(launch.arguments(), expected_arguments);
        assert_eq!(launch.working_directory(), std::env::temp_dir());
        assert!(!launch.inherit_environment());
        assert_eq!(launch.integration_status(), None);
        assert_eq!(
            launch.environment(),
            [
                ("HOME".into(), std::env::temp_dir().into_os_string()),
                ("PATH".into(), "/fixture/bin".into()),
                ("TERM".into(), "xterm-256color".into())
            ]
        );
        let launch = launch.with_environment_value("SSH_ASKPASS_TOKEN", "sensitive-secret-value");
        assert_eq!(
            format!("{launch:?}"),
            "PreparedShellLaunch { integration: None, .. }"
        );
    }
}
