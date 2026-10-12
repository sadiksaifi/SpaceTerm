//! Complete, platform-neutral process launch policy before Native PTY construction.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::shell_integration::{
    ShellEnvironment, ShellIntegrationMode, ShellIntegrationPolicy, ShellIntegrationStatus,
    plan_shell_integration,
};

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

/// Terminal identity facts supplied by the executable composing a shell launch.
/// No terminal engine or build identity is discovered by the host package.
#[derive(Clone)]
pub struct ShellTerminalIdentity {
    /// TERM when no injected packaged terminfo entry is discoverable.
    pub fallback_term: &'static str,
    /// COLORTERM exported to the shell.
    pub colorterm: &'static str,
    /// TERM_PROGRAM compatibility name exported to the shell.
    pub program_name: &'static str,
    /// TERM_PROGRAM_VERSION supplied by executable composition.
    pub program_version: &'static str,
    /// Optional packaged entry, checked again when a launch is prepared.
    pub terminfo: Option<ShellTerminfoIdentity>,
}

/// A packaged terminfo location and its terminal name.
#[derive(Clone)]
pub struct ShellTerminfoIdentity {
    /// Directory containing the compiled terminfo tree.
    pub directory: PathBuf,
    /// TERM to advertise only while its compiled entry is present.
    pub terminal_name: &'static str,
}

impl ShellTerminalIdentity {
    fn launch_identity(&self) -> (&'static str, Option<&Path>) {
        if let Some(terminfo) = &self.terminfo
            && let Some(first) = terminfo.terminal_name.as_bytes().first()
            && (terminfo
                .directory
                .join(format!("{first:02x}"))
                .join(terminfo.terminal_name)
                .is_file()
                || terminfo
                    .directory
                    .join(char::from(*first).to_string())
                    .join(terminfo.terminal_name)
                    .is_file())
        {
            (terminfo.terminal_name, Some(&terminfo.directory))
        } else {
            (self.fallback_term, None)
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn fixture() -> Self {
        Self {
            fallback_term: "xterm-256color",
            colorterm: "truecolor",
            program_name: "ghostty",
            program_version: "fixture-version",
            terminfo: None,
        }
    }
}

/// Extendable exclusions for parent environment variables.
/// The default excludes foreign terminal state, inherited Prompt Owner, terminfo,
/// and every SPACETERM_SERVER_* and SPACETERM_STARTUP_* variable.
#[derive(Clone)]
pub struct ShellEnvironmentDenylist {
    names: Vec<OsString>,
    prefixes: Vec<OsString>,
}

impl Default for ShellEnvironmentDenylist {
    fn default() -> Self {
        Self {
            names: FOREIGN_RUNTIME_ENVIRONMENT
                .iter()
                .copied()
                .chain(["TERMINFO", "SPACETERM_PROMPT_OWNER"])
                .map(OsString::from)
                .collect(),
            prefixes: vec!["SPACETERM_SERVER_".into(), "SPACETERM_STARTUP_".into()],
        }
    }
}

impl ShellEnvironmentDenylist {
    /// Exclude another exact parent environment name.
    pub fn with_name(mut self, name: impl Into<OsString>) -> Self {
        self.names.push(name.into());
        self
    }

    /// Exclude every name with another prefix, without converting native names to Unicode.
    pub fn with_prefix(mut self, prefix: impl Into<OsString>) -> Self {
        self.prefixes.push(prefix.into());
        self
    }

    fn denies(&self, name: &OsStr) -> bool {
        self.names.iter().any(|denied| denied == name) || self.denies_prefix(name)
    }

    fn denies_prefix(&self, name: &OsStr) -> bool {
        self.prefixes.iter().any(|prefix| {
            name.as_encoded_bytes()
                .starts_with(prefix.as_encoded_bytes())
        })
    }
}

/// Consumed, transport-prepared launch data. It carries no inherited environment authority.
/// Adapters must validate and consume their own channel before returning these facts.
pub struct RemoteShellLaunchParts {
    /// Native executable selected by the transport adapter.
    pub executable: PathBuf,
    /// Exact arguments selected by the transport adapter.
    pub arguments: Vec<OsString>,
    /// Validated local transport working directory.
    pub working_directory: PathBuf,
    /// Explicit transport environment, without the parent's variables.
    pub environment: Vec<(OsString, OsString)>,
    /// Transport-compatible TERM value.
    pub terminal_name: &'static str,
}

/// Native transport adapter for consuming a single prepared remote shell command.
pub trait PreparedRemoteShellCommand {
    /// Consume transport authority and return launch facts or a bounded failure classification.
    fn into_remote_shell_parts(self) -> Result<RemoteShellLaunchParts, ShellLaunchFailure>;
}

/// Composition supplies the selected shell, resources, captured environment, and policy facts.
#[derive(Clone)]
pub struct ShellLaunchPlanner {
    shell: PathBuf,
    resources: PathBuf,
    environment: (ShellIntegrationMode, ShellEnvironment),
    policy: ShellIntegrationPolicy,
    denylist: ShellEnvironmentDenylist,
    identity: ShellTerminalIdentity,
}

impl ShellLaunchPlanner {
    /// Capture local launch policy and terminal identity supplied by composition.
    pub fn new(
        shell: PathBuf,
        resources: PathBuf,
        mode: ShellIntegrationMode,
        inherited: ShellEnvironment,
        policy: ShellIntegrationPolicy,
        identity: ShellTerminalIdentity,
    ) -> Self {
        Self {
            shell,
            resources,
            environment: (mode, inherited),
            policy,
            denylist: ShellEnvironmentDenylist::default(),
            identity,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Fixed identity and integration policy for injected test composition.
    pub fn for_test(shell: PathBuf, resources: PathBuf) -> Self {
        Self::new(
            shell,
            resources,
            ShellIntegrationMode::Automatic,
            ShellEnvironment::default(),
            ShellIntegrationPolicy::fixture(),
            ShellTerminalIdentity::fixture(),
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Replace captured integration facts in test composition.
    pub fn with_environment(
        mut self,
        mode: ShellIntegrationMode,
        inherited: ShellEnvironment,
    ) -> Self {
        self.environment = (mode, inherited);
        self
    }

    /// Extend the inherited environment exclusions selected by composition.
    pub fn with_environment_denylist(mut self, denylist: ShellEnvironmentDenylist) -> Self {
        self.denylist = denylist;
        self
    }

    /// Replace terminal identity facts supplied by the composing executable.
    pub fn with_terminal_identity(mut self, identity: ShellTerminalIdentity) -> Self {
        self.identity = identity;
        self
    }

    /// Shell basename used before terminal metadata supplies a title.
    pub fn fallback_title(&self) -> String {
        self.shell
            .file_name()
            .unwrap_or(self.shell.as_os_str())
            .to_string_lossy()
            .into_owned()
    }

    /// Validate a directory and prepare a non-cloneable login-shell launch.
    pub fn local(
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
        // Captured parent values can otherwise escape through integration-specific aliases.
        let retained = |name: &str, value: &Option<OsString>| {
            value
                .as_ref()
                .filter(|_| !self.denylist.denies(OsStr::new(name)))
                .cloned()
        };
        let inherited = ShellEnvironment {
            xdg_data_dirs: retained("XDG_DATA_DIRS", &inherited.xdg_data_dirs),
            zdotdir: retained("ZDOTDIR", &inherited.zdotdir),
            env: retained("ENV", &inherited.env),
        };
        let integration =
            plan_shell_integration(&self.shell, &self.resources, mode, &inherited, &self.policy);
        let (term, terminfo) = self.identity.launch_identity();
        let mut arguments = integration.arguments;
        arguments.push(OsString::from("-l"));
        let mut environment = integration.environment;
        environment.extend([
            ("TERM".into(), term.into()),
            ("COLORTERM".into(), self.identity.colorterm.into()),
            ("TERM_PROGRAM".into(), self.identity.program_name.into()),
            (
                "TERM_PROGRAM_VERSION".into(),
                self.identity.program_version.into(),
            ),
            ("SPACETERM".into(), "1".into()),
        ]);
        if let Some(terminfo) = &terminfo {
            environment.push(("TERMINFO".into(), terminfo.as_os_str().to_owned()));
        }
        Ok(PreparedShellLaunch {
            executable: self.shell.clone(),
            arguments,
            working_directory: working_directory.to_owned(),
            inherit_environment: true,
            denylist: self.denylist.clone(),
            environment,
            integration: Some(integration.status),
            terminal_name: term,
        })
    }
}

/// An owned, non-cloneable launch. Debug never exposes command, path, or environment values.
///
/// ```compile_fail
/// fn duplicate(launch: spaceterm_host::shell_launch::PreparedShellLaunch) {
///     let _ = launch.clone();
/// }
/// ```
pub struct PreparedShellLaunch {
    executable: PathBuf,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    inherit_environment: bool,
    denylist: ShellEnvironmentDenylist,
    environment: Vec<(OsString, OsString)>,
    integration: Option<ShellIntegrationStatus>,
    terminal_name: &'static str,
}

impl PreparedShellLaunch {
    /// Consume a transport-prepared command without inheriting the host environment.
    pub fn remote(
        local_home: &Path,
        command: impl PreparedRemoteShellCommand,
    ) -> Result<Self, ShellLaunchFailure> {
        validate_working_directory(local_home)?;
        let parts = command.into_remote_shell_parts()?;
        validate_working_directory(&parts.working_directory)?;
        // Remote commands already carry a complete environment, so preserve the empty
        // exact-name removal contract while retaining mandatory private-prefix exclusions.
        let denylist = ShellEnvironmentDenylist {
            names: Vec::new(),
            ..ShellEnvironmentDenylist::default()
        };
        let mut environment = parts.environment;
        environment.retain(|(name, _)| !denylist.denies_prefix(name));
        environment.push(("TERM".into(), parts.terminal_name.into()));
        Ok(Self {
            executable: parts.executable,
            arguments: parts.arguments,
            working_directory: parts.working_directory,
            inherit_environment: false,
            denylist,
            environment,
            integration: None,
            terminal_name: parts.terminal_name,
        })
    }

    /// Sets one more variable for the shell, replacing any inherited value.
    pub fn with_environment_value(mut self, name: &str, value: &str) -> Self {
        if !self.denylist.denies_prefix(OsStr::new(name)) {
            self.environment.push((name.into(), value.into()));
        }
        self
    }

    /// TERM selected for this prepared launch.
    pub fn terminal_name(&self) -> &'static str {
        self.terminal_name
    }

    /// Native executable consumed by the adapter.
    pub fn executable(&self) -> &OsStr {
        self.executable.as_os_str()
    }
    /// Exact native argument order.
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    /// Validated starting directory, with its spelling preserved.
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }
    /// Whether the adapter may inherit non-denied parent environment variables.
    pub fn inherit_environment(&self) -> bool {
        self.inherit_environment
    }
    /// Exact parent names to remove; prefix filtering also applies through the denial predicate.
    pub fn environment_removals(&self) -> &[OsString] {
        &self.denylist.names
    }
    /// Whether a parent environment variable must be excluded before process creation.
    pub fn denies_inherited_environment(&self, name: &OsStr) -> bool {
        self.denylist.denies(name)
    }

    /// The result of local integration planning, or `None` for transport-prepared commands.
    pub fn integration_status(&self) -> Option<ShellIntegrationStatus> {
        self.integration
    }

    /// Explicit environment values, including the composing Session's fresh Prompt Owner.
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    #[cfg(any(test, feature = "test-support"))]
    /// A launch that never reaches a native process in injected tests.
    pub fn for_test(working_directory: PathBuf) -> Self {
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
/// Bounded launch rejection without paths, command text, or environment values.
pub enum ShellLaunchFailure {
    #[error("Shell launch directory is unavailable; select an existing directory and retry")]
    /// Starting directory is missing or is not a directory.
    DirectoryUnavailable,
    #[error("Remote Terminal Session Channel is unavailable; reconnect and retry")]
    /// The transport-prepared channel cannot be consumed.
    TerminalSessionChannelUnavailable,
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
    use crate::shell_integration::ShellKind;
    fn env_value<'a>(launch: &'a PreparedShellLaunch, key: &str) -> Option<&'a OsStr> {
        launch
            .environment()
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_os_str())
    }

    #[cfg(unix)]
    #[test]
    fn private_prefixes_cannot_bypass_exclusions_with_non_unicode_native_names() {
        use std::os::unix::ffi::OsStringExt;
        let launch =
            ShellLaunchPlanner::for_test("/fixture/zsh".into(), "/fixture/resources".into())
                .local(&std::env::temp_dir())
                .unwrap();
        for prefix in ["SPACETERM_SERVER_", "SPACETERM_STARTUP_"] {
            let mut bytes = prefix.as_bytes().to_vec();
            bytes.push(0xff);
            assert!(launch.denies_inherited_environment(&OsString::from_vec(bytes)));
        }
        let launch = launch
            .with_environment_value("SPACETERM_SERVER_NEW_SECRET", "fixture-value")
            .with_environment_value("SPACETERM_STARTUP_NEW_SECRET", "fixture-value");
        assert!(env_value(&launch, "SPACETERM_SERVER_NEW_SECRET").is_none());
        assert!(env_value(&launch, "SPACETERM_STARTUP_NEW_SECRET").is_none());
    }

    #[test]
    fn denied_captured_environment_values_are_not_reexported_by_integration() {
        let resources = crate::pty::shell_resources_fixture::ShellResourcesFixture::new();
        for (shell, variable) in [
            ("/bin/bash", "ENV"),
            ("/bin/zsh", "ZDOTDIR"),
            ("/bin/fish", "XDG_DATA_DIRS"),
        ] {
            let inherited = ShellEnvironment {
                env: Some("secret-bearing-fixture".into()),
                zdotdir: Some("secret-bearing-fixture".into()),
                xdg_data_dirs: Some("secret-bearing-fixture".into()),
            };
            let launch = ShellLaunchPlanner::new(
                shell.into(),
                resources.path().to_owned(),
                ShellIntegrationMode::Automatic,
                inherited,
                ShellIntegrationPolicy::fixture(),
                ShellTerminalIdentity::fixture(),
            )
            .with_environment_denylist(ShellEnvironmentDenylist::default().with_name(variable))
            .local(&std::env::temp_dir())
            .unwrap();
            assert!(launch.environment().iter().all(|(_, value)| {
                !value
                    .as_encoded_bytes()
                    .windows(b"secret-bearing-fixture".len())
                    .any(|window| window == b"secret-bearing-fixture")
            }));
        }
    }

    #[test]
    fn local_launch_prepares_every_supported_shell_and_preserves_login_order() {
        let resources = crate::pty::shell_resources_fixture::ShellResourcesFixture::new();
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
                ShellTerminalIdentity::fixture(),
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
        let resources = crate::pty::shell_resources_fixture::ShellResourcesFixture::new();
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
                ShellTerminalIdentity::fixture(),
            )
            .with_environment_denylist(
                ShellEnvironmentDenylist::default().with_name("HOST_LAUNCH_MARKER"),
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
                Some(OsStr::new("fixture-version"))
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
            assert!(
                launch
                    .environment_removals()
                    .contains(&OsString::from("HOST_LAUNCH_MARKER"))
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
        let planner = ShellLaunchPlanner::new(
            "/fixture/zsh".into(),
            root.clone(),
            ShellIntegrationMode::Automatic,
            ShellEnvironment::default(),
            ShellIntegrationPolicy::fixture(),
            ShellTerminalIdentity {
                terminfo: Some(ShellTerminfoIdentity {
                    directory: root.join("terminfo"),
                    terminal_name: "xterm-spaceterm",
                }),
                ..ShellTerminalIdentity::fixture()
            },
        )
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
        let resources = crate::pty::shell_resources_fixture::ShellResourcesFixture::new();
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
}
