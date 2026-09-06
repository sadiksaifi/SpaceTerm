use std::ffi::OsString;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellIntegrationMode {
    Automatic,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellKind {
    Bash,
    Elvish,
    Fish,
    Nushell,
    Zsh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellIntegrationStatus {
    Applied(ShellKind),
    Disabled,
    Unsupported,
    MissingResources,
}

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct ShellEnvironment {
    pub(crate) xdg_data_dirs: Option<OsString>,
    pub(crate) zdotdir: Option<OsString>,
    pub(crate) env: Option<OsString>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ShellIntegrationPlan {
    pub(crate) status: ShellIntegrationStatus,
    pub(super) arguments: Vec<OsString>,
    pub(super) environment: Vec<(OsString, OsString)>,
}

impl ShellEnvironment {
    pub(crate) fn capture() -> Self {
        Self {
            xdg_data_dirs: std::env::var_os("XDG_DATA_DIRS"),
            zdotdir: std::env::var_os("ZDOTDIR"),
            env: std::env::var_os("ENV"),
        }
    }
}

pub(crate) fn plan_shell_integration(
    shell: &Path,
    resource_root: &Path,
    mode: ShellIntegrationMode,
    inherited: &ShellEnvironment,
) -> ShellIntegrationPlan {
    if mode == ShellIntegrationMode::Disabled {
        return empty_plan(ShellIntegrationStatus::Disabled);
    }
    let Some(kind) = detect_shell(shell) else {
        return empty_plan(ShellIntegrationStatus::Unsupported);
    };
    if kind == ShellKind::Bash && shell == Path::new("/bin/bash") {
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
                .clone()
                .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));
            let mut xdg = xdg_root.clone();
            if !prior.is_empty() {
                xdg.push(":");
                xdg.push(prior);
            }
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

pub(crate) fn configured_mode() -> ShellIntegrationMode {
    match std::env::var("SPACETERM_SHELL_INTEGRATION") {
        Ok(value)
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
    use crate::terminal::testing::ShellResourcesFixture;

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
            ),
            plan_shell_integration(
                Path::new("/bin/sh"),
                resources,
                ShellIntegrationMode::Automatic,
                &inherited,
            ),
            plan_shell_integration(
                Path::new("/bin/zsh"),
                Path::new("/private/tmp/spaceterm-missing-resources"),
                ShellIntegrationMode::Automatic,
                &inherited,
            ),
            plan_shell_integration(
                Path::new("/bin/bash"),
                resources,
                ShellIntegrationMode::Automatic,
                &inherited,
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

#[cfg(test)]
#[path = "macos_adapter_tests/shell_integration.rs"]
mod macos_adapter_tests;
