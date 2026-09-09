//! Complete, platform-neutral process launch policy before Native PTY construction.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::shell_integration::{
    ShellEnvironment, ShellIntegrationMode, ShellIntegrationPolicy, ShellIntegrationStatus,
    plan_shell_integration,
};
use crate::ssh::command::SshCommandSpec;
use crate::terminal::identity;

const FOREIGN_RUNTIME_ENVIRONMENT: &[&str] = &[
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ITERM_SESSION_ID",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_WINDOW_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "STY",
    "TERM_SESSION_ID",
    "TERMINAL_EMULATOR",
    "TMUX",
    "TMUX_PANE",
    "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID",
    "WEZTERM_CONFIG_FILE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_EXECUTABLE_DIR",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WT_PROFILE_ID",
    "WT_SESSION",
    "ZELLIJ",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_SESSION_NAME",
];
const SHELL_INTEGRATION_ENVIRONMENT: &[&str] = &[
    "SPACETERM_BASH_ENV",
    "SPACETERM_BASH_INJECT",
    "SPACETERM_SHELL_INTEGRATION_VERSION",
    "SPACETERM_SHELL_INTEGRATION_XDG_DIR",
    "SPACETERM_ZSH_ZDOTDIR",
];

/// Composition supplies the selected shell, resources, captured environment, and policy facts.
#[derive(Clone)]
pub(crate) struct ShellLaunchPlanner {
    shell: PathBuf,
    resources: PathBuf,
    environment: (ShellIntegrationMode, ShellEnvironment),
    policy: ShellIntegrationPolicy,
}

impl ShellLaunchPlanner {
    pub(crate) fn new(
        shell: PathBuf,
        resources: PathBuf,
        mode: ShellIntegrationMode,
        inherited: ShellEnvironment,
        policy: ShellIntegrationPolicy,
    ) -> Self {
        Self {
            shell,
            resources,
            environment: (mode, inherited),
            policy,
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
        )
    }

    #[cfg(test)]
    pub(crate) fn with_environment(
        mut self,
        mode: ShellIntegrationMode,
        inherited: ShellEnvironment,
    ) -> Self {
        self.environment = (mode, inherited);
        self
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.shell
            .file_name()
            .unwrap_or(self.shell.as_os_str())
            .to_string_lossy()
            .into_owned()
    }

    pub(crate) fn local(
        &self,
        working_directory: &Path,
    ) -> Result<PreparedShellLaunch, ShellLaunchFailure> {
        self.local_with_environment(working_directory, self.environment.0, &self.environment.1)
    }

    fn local_with_environment(
        &self,
        working_directory: &Path,
        mode: ShellIntegrationMode,
        inherited: &ShellEnvironment,
    ) -> Result<PreparedShellLaunch, ShellLaunchFailure> {
        validate_working_directory(working_directory)?;
        let integration =
            plan_shell_integration(&self.shell, &self.resources, mode, inherited, &self.policy);
        let terminal_identity = identity::launch_identity(&self.resources);
        let mut arguments = integration.arguments;
        arguments.push(OsString::from("-l"));
        let mut environment = integration.environment;
        environment.extend([
            ("TERM".into(), terminal_identity.term.into()),
            ("COLORTERM".into(), identity::COLORTERM.into()),
            (
                "TERM_PROGRAM".into(),
                identity::COMPATIBILITY_PROGRAM_NAME.into(),
            ),
            (
                "TERM_PROGRAM_VERSION".into(),
                identity::PROGRAM_VERSION.into(),
            ),
            ("SPACETERM".into(), "1".into()),
        ]);
        if let Some(terminfo) = &terminal_identity.terminfo {
            environment.push(("TERMINFO".into(), terminfo.as_os_str().to_owned()));
        }
        Ok(PreparedShellLaunch {
            executable: self.shell.clone(),
            arguments,
            working_directory: working_directory.to_owned(),
            inherit_environment: true,
            environment_removals: FOREIGN_RUNTIME_ENVIRONMENT
                .iter()
                .copied()
                .chain(["TERMINFO"])
                .map(OsString::from)
                .collect(),
            environment,
            integration: Some(integration.status),
            terminal_name: terminal_identity.term,
        })
    }
}

/// An owned, non-cloneable launch. Debug never exposes command, path, or environment values.
pub(crate) struct PreparedShellLaunch {
    executable: PathBuf,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    inherit_environment: bool,
    environment_removals: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    integration: Option<ShellIntegrationStatus>,
    terminal_name: &'static str,
}

impl PreparedShellLaunch {
    pub(crate) fn remote(
        local_home: &Path,
        command: SshCommandSpec,
    ) -> Result<Self, ShellLaunchFailure> {
        validate_working_directory(local_home)?;
        let (executable, arguments, prepared_environment) = command
            .into_pane_launch_parts()
            .map_err(|_| ShellLaunchFailure::RemoteChannelUnavailable)?;
        let (working_directory, inherit_environment, environment_removals, mut environment) =
            if let Some(environment) = prepared_environment {
                let (home, entries) = environment.into_pane_launch_environment();
                (home, false, Vec::new(), entries)
            } else {
                (
                    local_home.to_owned(),
                    true,
                    FOREIGN_RUNTIME_ENVIRONMENT
                        .iter()
                        .chain(SHELL_INTEGRATION_ENVIRONMENT)
                        .copied()
                        .chain(["TERMINFO"])
                        .map(OsString::from)
                        .collect(),
                    Vec::new(),
                )
            };
        environment.push(("TERM".into(), identity::TERM_FALLBACK.into()));
        Ok(Self {
            executable,
            arguments,
            working_directory,
            inherit_environment,
            environment_removals,
            environment,
            integration: None,
            terminal_name: identity::TERM_FALLBACK,
        })
    }

    pub(crate) fn terminal_name(&self) -> &'static str {
        self.terminal_name
    }

    pub(crate) fn executable(&self) -> &OsStr {
        self.executable.as_os_str()
    }
    pub(crate) fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    pub(crate) fn working_directory(&self) -> &Path {
        &self.working_directory
    }
    pub(crate) fn inherit_environment(&self) -> bool {
        self.inherit_environment
    }
    pub(crate) fn environment_removals(&self) -> &[OsString] {
        &self.environment_removals
    }
    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    #[cfg(test)]
    pub(crate) fn for_test(working_directory: PathBuf) -> Self {
        let mut launch = ShellLaunchPlanner::for_test(
            "/fixture/zsh".into(),
            "/fixture/missing-resources".into(),
        )
        .local_with_environment(
            &std::env::temp_dir(),
            ShellIntegrationMode::Disabled,
            &ShellEnvironment::default(),
        )
        .unwrap();
        launch.working_directory = working_directory;
        launch
    }
}

impl fmt::Debug for PreparedShellLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedShellLaunch")
            .field("integration", &self.integration)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ShellLaunchFailure {
    #[error("Shell launch directory is unavailable; select an existing directory and retry")]
    DirectoryUnavailable,
    #[error("Remote Terminal Session Channel is unavailable; reconnect and retry")]
    RemoteChannelUnavailable,
}

fn validate_working_directory(directory: &Path) -> Result<(), ShellLaunchFailure> {
    if directory.metadata().is_ok_and(|metadata| metadata.is_dir()) {
        Ok(())
    } else {
        Err(ShellLaunchFailure::DirectoryUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RemoteDirectory, SshDestination};
    use crate::platform::shell_integration::ShellKind;
    use crate::ssh::command::{
        PreparedSshPaneChannelError, RemotePaneShellCommandBuilder, SshCommandContext,
        ValidatedRemoteLoginShell,
    };

    fn env_value<'a>(launch: &'a PreparedShellLaunch, key: &str) -> Option<&'a OsStr> {
        launch
            .environment()
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_os_str())
    }

    #[test]
    fn local_launch_prepares_every_supported_shell_and_preserves_login_order() {
        let resources = crate::terminal::testing::ShellResourcesFixture::new();
        let inherited = ShellEnvironment {
            xdg_data_dirs: Some("/custom/share".into()),
            zdotdir: Some("/private/zsh config".into()),
            env: Some("/private/bash env".into()),
        };
        for (shell, kind, arguments) in [
            ("/opt/bin/bash", ShellKind::Bash, vec!["--posix", "-l"]),
            ("/opt/bin/elvish", ShellKind::Elvish, vec!["-l"]),
            ("/opt/bin/fish", ShellKind::Fish, vec!["-l"]),
            (
                "/opt/bin/nu",
                ShellKind::Nushell,
                vec!["--execute", "use spaceterm *; install", "-l"],
            ),
            ("/bin/zsh", ShellKind::Zsh, vec!["-l"]),
        ] {
            let directory = std::env::temp_dir();
            let launch = ShellLaunchPlanner::new(
                shell.into(),
                resources.path().to_path_buf(),
                ShellIntegrationMode::Automatic,
                inherited.clone(),
                ShellIntegrationPolicy::fixture(),
            )
            .local(&directory)
            .unwrap();
            assert_eq!(launch.executable(), OsStr::new(shell));
            assert_eq!(launch.arguments(), arguments);
            assert_eq!(launch.working_directory(), directory);
            assert_eq!(
                launch.integration,
                Some(ShellIntegrationStatus::Applied(kind))
            );
            assert!(launch.inherit_environment());
            assert_eq!(
                env_value(&launch, "SPACETERM_SHELL_INTEGRATION_VERSION"),
                Some(OsStr::new("1"))
            );
            match kind {
                ShellKind::Bash => assert_eq!(
                    env_value(&launch, "SPACETERM_BASH_ENV"),
                    inherited.env.as_deref()
                ),
                ShellKind::Zsh => assert_eq!(
                    env_value(&launch, "SPACETERM_ZSH_ZDOTDIR"),
                    inherited.zdotdir.as_deref()
                ),
                _ => {
                    let mut expected = resources
                        .path()
                        .to_path_buf()
                        .join("shell-integration")
                        .into_os_string();
                    expected.push(":/custom/share");
                    assert_eq!(
                        env_value(&launch, "XDG_DATA_DIRS"),
                        Some(expected.as_os_str())
                    );
                }
            }
        }
    }

    #[test]
    fn local_fallbacks_keep_login_arguments_and_terminal_identity() {
        let resources = crate::terminal::testing::ShellResourcesFixture::new();
        for (shell, mode, root, status) in [
            (
                "/bin/zsh",
                ShellIntegrationMode::Disabled,
                resources.path().to_path_buf(),
                ShellIntegrationStatus::Disabled,
            ),
            (
                "/fixture/unsupported",
                ShellIntegrationMode::Automatic,
                resources.path().to_path_buf(),
                ShellIntegrationStatus::Unsupported,
            ),
            (
                "/bin/unknown",
                ShellIntegrationMode::Automatic,
                resources.path().to_path_buf(),
                ShellIntegrationStatus::Unsupported,
            ),
            (
                "/bin/zsh",
                ShellIntegrationMode::Automatic,
                resources.path().to_path_buf().join("missing"),
                ShellIntegrationStatus::MissingResources,
            ),
        ] {
            let launch = ShellLaunchPlanner::new(
                shell.into(),
                root,
                mode,
                ShellEnvironment::default(),
                ShellIntegrationPolicy::fixture(),
            )
            .local(&std::env::temp_dir())
            .unwrap();
            assert_eq!(launch.arguments(), ["-l"]);
            assert_eq!(launch.integration, Some(status));
            assert_eq!(
                env_value(&launch, "SPACETERM_SHELL_INTEGRATION_VERSION"),
                None
            );
            assert_eq!(
                env_value(&launch, "TERM"),
                Some(OsStr::new("xterm-256color"))
            );
            assert_eq!(
                env_value(&launch, "COLORTERM"),
                Some(OsStr::new("truecolor"))
            );
            assert_eq!(
                env_value(&launch, "TERM_PROGRAM"),
                Some(OsStr::new("ghostty"))
            );
            assert_eq!(
                env_value(&launch, "TERM_PROGRAM_VERSION"),
                Some(OsStr::new(identity::PROGRAM_VERSION))
            );
            assert_eq!(env_value(&launch, "SPACETERM"), Some(OsStr::new("1")));
            assert!(FOREIGN_RUNTIME_ENVIRONMENT.iter().all(|name| {
                launch
                    .environment_removals()
                    .contains(&OsString::from(name))
            }));
            assert!(
                launch
                    .environment_removals()
                    .contains(&OsString::from("TERMINFO"))
            );
        }
    }

    #[test]
    fn packaged_identity_requires_a_discoverable_entry_at_the_injected_location() {
        let root =
            std::env::temp_dir().join(format!("spaceterm-launch-resources-{}", std::process::id()));
        let entry = root.join("terminfo/78/xterm-spaceterm");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, b"compiled fixture").unwrap();
        let planner = ShellLaunchPlanner::for_test("/fixture/zsh".into(), root.clone())
            .with_environment(ShellIntegrationMode::Automatic, ShellEnvironment::default());
        let launch = planner.local(&std::env::temp_dir()).unwrap();
        assert_eq!(
            env_value(&launch, "TERM"),
            Some(OsStr::new("xterm-spaceterm"))
        );
        assert_eq!(
            env_value(&launch, "TERMINFO"),
            Some(root.join("terminfo").as_os_str())
        );
        std::fs::remove_dir_all(&root).unwrap();
        let launch = planner.local(&std::env::temp_dir()).unwrap();
        assert_eq!(
            env_value(&launch, "TERM"),
            Some(OsStr::new("xterm-256color"))
        );
        assert_eq!(env_value(&launch, "TERMINFO"), None);
    }

    #[test]
    fn launch_validation_preserves_spelling_and_redacts_missing_and_file_paths() {
        let resources = crate::terminal::testing::ShellResourcesFixture::new();
        let planner = ShellLaunchPlanner::for_test(
            "/sensitive/shell/zsh".into(),
            resources.path().to_path_buf(),
        )
        .with_environment(ShellIntegrationMode::Automatic, ShellEnvironment::default());
        let directory = std::env::temp_dir().join(".");
        assert_eq!(
            planner.local(&directory).unwrap().working_directory(),
            directory
        );
        for directory in [
            resources
                .path()
                .to_path_buf()
                .join("missing-private-project"),
            resources
                .path()
                .to_path_buf()
                .join("shell-integration/zsh/.zshenv"),
        ] {
            let error = planner.local(&directory).unwrap_err();
            assert_eq!(error, ShellLaunchFailure::DirectoryUnavailable);
            assert!(!format!("{error:?} {error}").contains("private-project"));
            assert!(std::error::Error::source(&error).is_none());
        }
    }

    #[test]
    fn remote_fallback_preserves_exact_command_consumes_once_and_removes_local_markers() {
        let context = SshCommandContext::new(
            crate::ssh::command::OpenSshExecutable::for_test(),
            "/private/config/ssh_config".into(),
            SshDestination::new("user@remote".to_owned()).unwrap(),
            "/private/runtime/control.sock".into(),
        )
        .unwrap();
        let directory = RemoteDirectory::new("~/private project".to_owned()).unwrap();
        let shell = ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap();
        let command = RemotePaneShellCommandBuilder::new(&directory, &shell)
            .build()
            .unwrap();
        let channel = context.prepare_pane_channel(command);
        let taken = channel.take().unwrap();
        let expected_arguments = taken.arguments().to_vec();
        let launch = PreparedShellLaunch::remote(&std::env::temp_dir(), taken).unwrap();
        assert!(matches!(
            channel.take(),
            Err(PreparedSshPaneChannelError::AlreadyConsumed)
        ));
        assert_eq!(launch.executable(), "/test/ssh");
        assert_eq!(launch.arguments(), expected_arguments);
        assert_eq!(launch.working_directory(), std::env::temp_dir());
        assert!(launch.inherit_environment());
        assert_eq!(launch.integration, None);
        assert_eq!(
            launch.environment(),
            [(OsString::from("TERM"), OsString::from("xterm-256color"))]
        );
        assert!(
            FOREIGN_RUNTIME_ENVIRONMENT
                .iter()
                .chain(SHELL_INTEGRATION_ENVIRONMENT)
                .chain([&"TERMINFO"])
                .all(|name| launch
                    .environment_removals()
                    .contains(&OsString::from(name)))
        );
        let mut launch = launch;
        launch
            .environment
            .push(("SSH_ASKPASS_TOKEN".into(), "sensitive-secret-value".into()));
        assert_eq!(
            format!("{launch:?}"),
            "PreparedShellLaunch { integration: None, .. }"
        );
    }
}
