//! Shell integration plans selected from injected compatibility and environment facts.

use std::ffi::OsString;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Whether automatic integration may be planned.
pub enum ShellIntegrationMode {
    /// Apply integration when supported resources exist.
    Automatic,
    /// Integration was explicitly disabled.
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A supported shell integration dialect.
pub enum ShellKind {
    /// Bash integration.
    Bash,
    /// Elvish integration.
    Elvish,
    /// Fish integration.
    Fish,
    /// Nushell integration.
    Nushell,
    /// Zsh integration.
    Zsh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Bounded result of shell integration planning.
pub enum ShellIntegrationStatus {
    /// Integration was applied for the selected shell.
    Applied(ShellKind),
    /// Integration was explicitly disabled.
    Disabled,
    /// The shell or injected compatibility policy is unsupported.
    Unsupported,
    /// A required integration resource is unavailable.
    MissingResources,
}

#[derive(Clone, Default, Eq, PartialEq)]
/// Captured parent search-path facts used only for integration planning.
pub struct ShellEnvironment {
    /// Parent XDG_DATA_DIRS, preserving native encoding.
    pub xdg_data_dirs: Option<OsString>,
    /// Parent ZDOTDIR for the user's zsh configuration.
    pub zdotdir: Option<OsString>,
    /// Parent ENV for the user's bash configuration.
    pub env: Option<OsString>,
}

#[derive(Clone, Eq, PartialEq)]
/// Arguments and environment selected before native process construction.
pub struct ShellIntegrationPlan {
    /// Classified integration result.
    pub status: ShellIntegrationStatus,
    /// Shell arguments that precede the login-shell argument.
    pub arguments: Vec<OsString>,
    /// Explicit environment additions for the integration resources.
    pub environment: Vec<(OsString, OsString)>,
}

/// Captured compatibility and search-path facts, selected independently of the host at planning.
#[derive(Clone)]
pub struct ShellIntegrationPolicy {
    /// Whether composition approves the selected shell's integration.
    pub supported: bool,
    /// Native search-path separator.
    pub path_list_separator: char,
    /// Search path when the parent supplies no XDG_DATA_DIRS.
    pub fallback_xdg_data_dirs: OsString,
}

impl ShellIntegrationPolicy {
    #[cfg(any(test, feature = "test-support"))]
    /// Fixed portable policy for injected test composition.
    pub fn fixture() -> Self {
        Self {
            supported: true,
            path_list_separator: ':',
            fallback_xdg_data_dirs: "/fixture/share".into(),
        }
    }
}

/// Prepend without lossy conversion or ambient platform path-list operations.
fn prepend_path(
    root: &std::ffi::OsStr,
    prior: &std::ffi::OsStr,
    separator: char,
) -> Option<OsString> {
    if !separator.is_ascii() || separator.is_ascii_control() {
        return None;
    }
    if root.as_encoded_bytes().contains(&(separator as u8)) {
        return None;
    }
    let mut value = root.to_owned();
    if !prior.is_empty() {
        value.push(separator.to_string());
        value.push(prior);
    }
    Some(value)
}

/// Plan supported integration using only the supplied resources and captured facts.
pub fn plan_shell_integration(
    shell: &Path,
    resource_root: &Path,
    mode: ShellIntegrationMode,
    inherited: &ShellEnvironment,
    policy: &ShellIntegrationPolicy,
) -> ShellIntegrationPlan {
    if mode == ShellIntegrationMode::Disabled {
        return empty_plan(ShellIntegrationStatus::Disabled);
    }
    let Some(kind) = detect_shell(shell) else {
        return empty_plan(ShellIntegrationStatus::Unsupported);
    };
    if !policy.supported {
        return empty_plan(ShellIntegrationStatus::Unsupported);
    }

    let integration_root = resource_root.join("shell-integration");
    let required = match kind {
        ShellKind::Bash => integration_root.join("bash/spaceterm.bash"),
        ShellKind::Elvish => integration_root.join("elvish/lib/spaceterm-integration.elv"),
        ShellKind::Fish => {
            integration_root.join("fish/vendor_conf.d/spaceterm-shell-integration.fish")
        }
        ShellKind::Nushell => integration_root.join("nushell/vendor/autoload/spaceterm.nu"),
        ShellKind::Zsh => integration_root.join("zsh/.zshenv"),
    };
    if !required.is_file() {
        return empty_plan(ShellIntegrationStatus::MissingResources);
    }

    let mut arguments = Vec::new();
    let mut environment = vec![(
        OsString::from("SPACETERM_SHELL_INTEGRATION_VERSION"),
        OsString::from("1"),
    )];
    match kind {
        ShellKind::Bash => {
            arguments.push(OsString::from("--posix"));
            environment.push((OsString::from("ENV"), required.into_os_string()));
            environment.push((OsString::from("SPACETERM_BASH_INJECT"), OsString::from("1")));
            if let Some(value) = &inherited.env {
                environment.push((OsString::from("SPACETERM_BASH_ENV"), value.clone()));
            }
        }
        ShellKind::Elvish | ShellKind::Fish | ShellKind::Nushell => {
            let xdg_root = integration_root.into_os_string();
            let prior = inherited
                .xdg_data_dirs
                .as_deref()
                .unwrap_or(&policy.fallback_xdg_data_dirs);
            let Some(xdg) = prepend_path(&xdg_root, prior, policy.path_list_separator) else {
                return empty_plan(ShellIntegrationStatus::Unsupported);
            };
            environment.push((
                OsString::from("SPACETERM_SHELL_INTEGRATION_XDG_DIR"),
                xdg_root,
            ));
            environment.push((OsString::from("XDG_DATA_DIRS"), xdg));
            if kind == ShellKind::Nushell {
                arguments.extend([
                    OsString::from("--execute"),
                    OsString::from("use spaceterm *; install"),
                ]);
            }
        }
        ShellKind::Zsh => {
            environment.push((
                OsString::from("ZDOTDIR"),
                integration_root.join("zsh").into_os_string(),
            ));
            if let Some(value) = &inherited.zdotdir {
                environment.push((OsString::from("SPACETERM_ZSH_ZDOTDIR"), value.clone()));
            }
        }
    }
    ShellIntegrationPlan {
        status: ShellIntegrationStatus::Applied(kind),
        arguments,
        environment,
    }
}

/// Interpret an injected SPACETERM_SHELL_INTEGRATION value.
pub fn configured_mode(value: Option<&std::ffi::OsStr>) -> ShellIntegrationMode {
    match value.and_then(std::ffi::OsStr::to_str) {
        Some(value)
            if matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false"
            ) =>
        {
            ShellIntegrationMode::Disabled
        }
        _ => ShellIntegrationMode::Automatic,
    }
}

fn empty_plan(status: ShellIntegrationStatus) -> ShellIntegrationPlan {
    ShellIntegrationPlan {
        status,
        arguments: Vec::new(),
        environment: Vec::new(),
    }
}

fn detect_shell(shell: &Path) -> Option<ShellKind> {
    match shell.file_name()?.to_str()? {
        "bash" => Some(ShellKind::Bash),
        "elvish" => Some(ShellKind::Elvish),
        "fish" => Some(ShellKind::Fish),
        "nu" => Some(ShellKind::Nushell),
        "zsh" => Some(ShellKind::Zsh),
        _ => None,
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::pty::shell_resources_fixture::ShellResourcesFixture;

    #[test]
    fn configured_mode_uses_only_the_supplied_value() {
        for (value, expected) in [
            (None, ShellIntegrationMode::Automatic),
            (Some(""), ShellIntegrationMode::Automatic),
            (Some("0"), ShellIntegrationMode::Disabled),
            (Some(" Off "), ShellIntegrationMode::Disabled),
            (Some("FALSE"), ShellIntegrationMode::Disabled),
            (Some("true"), ShellIntegrationMode::Automatic),
            (Some("unknown"), ShellIntegrationMode::Automatic),
        ] {
            assert_eq!(configured_mode(value.map(std::ffi::OsStr::new)), expected);
        }
    }

    #[test]
    fn path_lists_use_only_the_supplied_separator_and_preserve_empty_entries() {
        for (root, prior, separator, expected) in [
            ("/resources", "/one:/two", ':', Some("/resources:/one:/two")),
            ("/resources", ";/two;", ';', Some("/resources;;/two;")),
            ("/resources", "", ';', Some("/resources")),
            ("/bad;root", "/one", ';', None),
            ("/resources", "/one", '\0', None),
        ] {
            assert_eq!(
                prepend_path(root.as_ref(), prior.as_ref(), separator).as_deref(),
                expected.map(std::ffi::OsStr::new)
            );
        }
    }

    #[test]
    fn captured_fallback_and_compatibility_control_the_plan() {
        let resources = ShellResourcesFixture::new();
        let mut policy = ShellIntegrationPolicy {
            supported: true,
            path_list_separator: ';',
            fallback_xdg_data_dirs: "/fixture/one;/fixture/two".into(),
        };
        let plan = plan_shell_integration(
            Path::new("/fixture/fish"),
            resources.path(),
            ShellIntegrationMode::Automatic,
            &ShellEnvironment::default(),
            &policy,
        );
        let mut expected = resources.path().join("shell-integration").into_os_string();
        expected.push(";/fixture/one;/fixture/two");
        assert!(
            plan.environment
                .contains(&("XDG_DATA_DIRS".into(), expected))
        );
        policy.supported = false;
        let plan = plan_shell_integration(
            Path::new("/fixture/bash"),
            resources.path(),
            ShellIntegrationMode::Automatic,
            &ShellEnvironment::default(),
            &policy,
        );
        assert_eq!(plan.status, ShellIntegrationStatus::Unsupported);
        assert!(plan.arguments.is_empty() && plan.environment.is_empty());
    }

    #[test]
    fn supported_shells_receive_isolated_startup_plans() {
        let fixture = ShellResourcesFixture::new();
        let resources = fixture.path();
        let inherited = ShellEnvironment {
            xdg_data_dirs: Some(OsString::from("/opt/share")),
            zdotdir: Some(OsString::from("/Users/me/.config/zsh")),
            env: Some(OsString::from("/Users/me/.shenv")),
        };

        let zsh = plan_shell_integration(
            Path::new("/bin/zsh"),
            resources,
            ShellIntegrationMode::Automatic,
            &inherited,
            &ShellIntegrationPolicy::fixture(),
        );
        assert_eq!(zsh.status, ShellIntegrationStatus::Applied(ShellKind::Zsh));
        assert!(zsh.environment.iter().any(|(name, value)| {
            name == "SPACETERM_ZSH_ZDOTDIR" && value == "/Users/me/.config/zsh"
        }));

        let fish = plan_shell_integration(
            Path::new("/fixture/bin/fish"),
            resources,
            ShellIntegrationMode::Automatic,
            &inherited,
            &ShellIntegrationPolicy::fixture(),
        );
        assert_eq!(
            fish.status,
            ShellIntegrationStatus::Applied(ShellKind::Fish)
        );
        assert!(fish.environment.iter().any(|(name, value)| {
            name == "XDG_DATA_DIRS"
                && value
                    .to_string_lossy()
                    .ends_with("shell-integration:/opt/share")
        }));

        let nu = plan_shell_integration(
            Path::new("/usr/local/bin/nu"),
            resources,
            ShellIntegrationMode::Automatic,
            &inherited,
            &ShellIntegrationPolicy::fixture(),
        );
        assert_eq!(
            nu.status,
            ShellIntegrationStatus::Applied(ShellKind::Nushell)
        );
        assert_eq!(nu.arguments, ["--execute", "use spaceterm *; install"]);

        let bash = plan_shell_integration(
            Path::new("/fixture/bin/bash"),
            resources,
            ShellIntegrationMode::Automatic,
            &inherited,
            &ShellIntegrationPolicy::fixture(),
        );
        assert_eq!(
            bash.status,
            ShellIntegrationStatus::Applied(ShellKind::Bash)
        );
        assert_eq!(bash.arguments, ["--posix"]);
        assert!(
            bash.environment.iter().any(|(name, value)| {
                name == "SPACETERM_BASH_ENV" && value == "/Users/me/.shenv"
            })
        );

        let elvish = plan_shell_integration(
            Path::new("/usr/local/bin/elvish"),
            resources,
            ShellIntegrationMode::Automatic,
            &inherited,
            &ShellIntegrationPolicy::fixture(),
        );
        assert_eq!(
            elvish.status,
            ShellIntegrationStatus::Applied(ShellKind::Elvish)
        );
        assert!(elvish.environment.iter().any(|(name, value)| {
            name == "XDG_DATA_DIRS"
                && value
                    .to_string_lossy()
                    .ends_with("shell-integration:/opt/share")
        }));
    }

    #[test]
    fn disabled_unsupported_and_missing_resources_leave_launch_untouched() {
        let inherited = ShellEnvironment::default();
        let fixture = ShellResourcesFixture::new();
        let resources = fixture.path();
        let cases = [
            plan_shell_integration(
                Path::new("/bin/zsh"),
                resources,
                ShellIntegrationMode::Disabled,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            ),
            plan_shell_integration(
                Path::new("/bin/sh"),
                resources,
                ShellIntegrationMode::Automatic,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            ),
            plan_shell_integration(
                Path::new("/bin/zsh"),
                Path::new("/private/tmp/spaceterm-missing-resources"),
                ShellIntegrationMode::Automatic,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            ),
            plan_shell_integration(
                Path::new("/fixture/unsupported"),
                resources,
                ShellIntegrationMode::Automatic,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            ),
        ];

        assert_eq!(cases[0].status, ShellIntegrationStatus::Disabled);
        assert_eq!(cases[1].status, ShellIntegrationStatus::Unsupported);
        assert_eq!(cases[2].status, ShellIntegrationStatus::MissingResources);
        assert_eq!(cases[3].status, ShellIntegrationStatus::Unsupported);
        assert!(
            cases
                .iter()
                .all(|plan| plan.arguments.is_empty() && plan.environment.is_empty())
        );
    }
}
