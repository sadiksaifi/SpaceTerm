use std::ffi::OsString;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellIntegrationMode {
    Automatic,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    Bash,
    Elvish,
    Fish,
    Nushell,
    Zsh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellIntegrationStatus {
    Applied(ShellKind),
    Disabled,
    Unsupported,
    MissingResources,
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct ShellEnvironment {
    pub xdg_data_dirs: Option<OsString>,
    pub zdotdir: Option<OsString>,
    pub env: Option<OsString>,
    /// The PATH a Session's shell inherits, which the Session's own CLI goes in front of.
    pub path: Option<OsString>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ShellIntegrationPlan {
    pub status: ShellIntegrationStatus,
    pub(super) arguments: Vec<OsString>,
    pub(super) environment: Vec<(OsString, OsString)>,
}

/// Captured compatibility and search-path facts, selected independently of the host at planning.
#[derive(Clone)]
pub struct ShellIntegrationPolicy {
    /// Whether the shell can run SpaceTerm's startup files at all.
    pub supported: bool,
    pub path_list_separator: char,
    pub fallback_xdg_data_dirs: OsString,
}

impl ShellIntegrationPolicy {
    #[cfg(test)]
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

/// `root` first on the `separator`-delimited `prior` list. Every other entry keeps its order,
/// including empty entries, which name the current directory. Only other copies of `root` go.
pub(super) fn prepend_path_once(
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
    for entry in prior
        .as_encoded_bytes()
        .split(|byte| *byte == separator as u8)
        .filter(|entry| *entry != root.as_encoded_bytes())
    {
        value.push(separator.to_string());
        // SAFETY: `entry` is a run of `prior`'s encoded bytes split only at an ASCII byte, which
        // `OsStr::from_encoded_bytes_unchecked` accepts.
        value.push(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(entry) });
    }
    Some(value)
}

/// Plans the shell's SpaceTerm startup files. Every shell that can run them runs them after the
/// user's own startup files, so they put the Session's CLI first on PATH again even when shell
/// integration is disabled or the shell has none. Only applied integration also sets
/// `SPACETERM_SHELL_INTEGRATION_VERSION`, which turns on prompt marks and reports.
pub fn plan_shell_integration(
    shell: &Path,
    resource_root: &Path,
    mode: ShellIntegrationMode,
    inherited: &ShellEnvironment,
    policy: &ShellIntegrationPolicy,
) -> ShellIntegrationPlan {
    let startup = detect_startup(shell).filter(|_| policy.supported);
    let applied = match startup {
        Some(Startup::Integrated(kind)) => Some(kind),
        _ => None,
    };
    let status = match (mode, applied) {
        (ShellIntegrationMode::Disabled, _) => ShellIntegrationStatus::Disabled,
        (ShellIntegrationMode::Automatic, None) => ShellIntegrationStatus::Unsupported,
        (ShellIntegrationMode::Automatic, Some(kind)) => ShellIntegrationStatus::Applied(kind),
    };
    let Some(startup) = startup else {
        return empty_plan(status);
    };
    match startup_plan(startup, resource_root, inherited, policy) {
        Ok(mut plan) => {
            if let ShellIntegrationStatus::Applied(kind) = status {
                plan.environment.insert(
                    0,
                    (
                        OsString::from("SPACETERM_SHELL_INTEGRATION_VERSION"),
                        OsString::from("1"),
                    ),
                );
                // Fish 4 marks its prompts itself, without the Prompt Owner. SpaceTerm's
                // integration marks them instead, and fish reads its features only at startup.
                if kind == ShellKind::Fish {
                    plan.arguments.extend([
                        OsString::from("--features"),
                        OsString::from("no-mark-prompt"),
                    ]);
                }
            }
            plan.status = status;
            plan
        }
        Err(failure) => empty_plan(match status {
            ShellIntegrationStatus::Applied(_) => failure,
            status => status,
        }),
    }
}

/// How a shell reaches SpaceTerm's startup files.
#[derive(Clone, Copy)]
enum Startup {
    /// A shell with SpaceTerm's integration.
    Integrated(ShellKind),
    /// A POSIX shell that reads `ENV` after its login profile. Its file only restores PATH.
    Posix,
}

fn startup_plan(
    startup: Startup,
    resource_root: &Path,
    inherited: &ShellEnvironment,
    policy: &ShellIntegrationPolicy,
) -> Result<ShellIntegrationPlan, ShellIntegrationStatus> {
    let integration_root = resource_root.join("shell-integration");
    let required = match startup {
        Startup::Posix => integration_root.join("sh/spaceterm.sh"),
        Startup::Integrated(ShellKind::Bash) => integration_root.join("bash/spaceterm.bash"),
        Startup::Integrated(ShellKind::Elvish) => {
            integration_root.join("elvish/lib/spaceterm-integration.elv")
        }
        Startup::Integrated(ShellKind::Fish) => {
            integration_root.join("fish/vendor_conf.d/spaceterm-shell-integration.fish")
        }
        Startup::Integrated(ShellKind::Nushell) => {
            integration_root.join("nushell/vendor/autoload/spaceterm.nu")
        }
        Startup::Integrated(ShellKind::Zsh) => integration_root.join("zsh/.zshenv"),
    };
    if !required.is_file() {
        return Err(ShellIntegrationStatus::MissingResources);
    }

    let mut arguments = Vec::new();
    let mut environment = Vec::new();
    match startup {
        Startup::Posix => {
            environment.push((OsString::from("ENV"), required.into_os_string()));
            if let Some(value) = &inherited.env {
                environment.push((OsString::from("SPACETERM_SH_ENV"), value.clone()));
            }
        }
        Startup::Integrated(ShellKind::Bash) => {
            arguments.push(OsString::from("--posix"));
            environment.push((OsString::from("ENV"), required.into_os_string()));
            environment.push((OsString::from("SPACETERM_BASH_INJECT"), OsString::from("1")));
            if let Some(value) = &inherited.env {
                environment.push((OsString::from("SPACETERM_BASH_ENV"), value.clone()));
            }
        }
        Startup::Integrated(kind @ (ShellKind::Elvish | ShellKind::Fish | ShellKind::Nushell)) => {
            let xdg_root = integration_root.into_os_string();
            let prior = inherited
                .xdg_data_dirs
                .as_deref()
                .unwrap_or(&policy.fallback_xdg_data_dirs);
            let Some(xdg) = prepend_path(&xdg_root, prior, policy.path_list_separator) else {
                return Err(ShellIntegrationStatus::Unsupported);
            };
            environment.push((
                OsString::from("SPACETERM_SHELL_INTEGRATION_XDG_DIR"),
                xdg_root,
            ));
            environment.push((OsString::from("XDG_DATA_DIRS"), xdg));
            if kind == ShellKind::Elvish {
                let rc = resource_root.join("shell-integration/elvish/rc.elv");
                if !rc.is_file() {
                    return Err(ShellIntegrationStatus::MissingResources);
                }
                arguments.extend([OsString::from("-rc"), rc.into_os_string()]);
            }
            if kind == ShellKind::Nushell {
                arguments.extend([
                    OsString::from("--execute"),
                    OsString::from("use spaceterm *; install"),
                ]);
            }
        }
        Startup::Integrated(ShellKind::Zsh) => {
            environment.push((
                OsString::from("ZDOTDIR"),
                integration_root.join("zsh").into_os_string(),
            ));
            if let Some(value) = &inherited.zdotdir {
                environment.push((OsString::from("SPACETERM_ZSH_ZDOTDIR"), value.clone()));
            }
        }
    }
    Ok(ShellIntegrationPlan {
        status: ShellIntegrationStatus::Unsupported,
        arguments,
        environment,
    })
}

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

fn detect_startup(shell: &Path) -> Option<Startup> {
    Some(match shell.file_name()?.to_str()? {
        "bash" => Startup::Integrated(ShellKind::Bash),
        "elvish" => Startup::Integrated(ShellKind::Elvish),
        "fish" => Startup::Integrated(ShellKind::Fish),
        "nu" => Startup::Integrated(ShellKind::Nushell),
        "zsh" => Startup::Integrated(ShellKind::Zsh),
        // Other POSIX shells, such as ksh, read their own rc file when ENV is unset, and a
        // replaced ENV would skip it.
        "sh" | "dash" => Startup::Posix,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::terminal::testing::ShellResourcesFixture;

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
            path: None,
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
        assert_eq!(fish.arguments, ["--features", "no-mark-prompt"]);
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
        assert_eq!(
            elvish.arguments,
            [
                OsString::from("-rc"),
                resources
                    .join("shell-integration/elvish/rc.elv")
                    .into_os_string(),
            ]
        );
        assert!(elvish.environment.iter().any(|(name, value)| {
            name == "XDG_DATA_DIRS"
                && value
                    .to_string_lossy()
                    .ends_with("shell-integration:/opt/share")
        }));
    }

    /// Disabled integration and shells without integration still run SpaceTerm's startup files,
    /// which restore PATH, but never turn on prompt marks and reports.
    #[test]
    fn disabled_and_integrationless_shells_receive_only_the_startup_files() {
        let fixture = ShellResourcesFixture::new();
        let resources = fixture.path();
        let integration = resources.join("shell-integration");
        let inherited = ShellEnvironment {
            env: Some(OsString::from("/Users/me/.shrc")),
            ..ShellEnvironment::default()
        };
        let plan = |shell: &str, mode| {
            plan_shell_integration(
                Path::new(shell),
                resources,
                mode,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            )
        };
        let cases = [
            (
                plan("/bin/zsh", ShellIntegrationMode::Disabled),
                ShellIntegrationStatus::Disabled,
                Vec::<&str>::new(),
                ("ZDOTDIR", integration.join("zsh").into_os_string()),
            ),
            (
                plan("/bin/bash", ShellIntegrationMode::Disabled),
                ShellIntegrationStatus::Disabled,
                vec!["--posix"],
                (
                    "ENV",
                    integration.join("bash/spaceterm.bash").into_os_string(),
                ),
            ),
            (
                plan("/opt/bin/fish", ShellIntegrationMode::Disabled),
                ShellIntegrationStatus::Disabled,
                Vec::new(),
                (
                    "SPACETERM_SHELL_INTEGRATION_XDG_DIR",
                    integration.clone().into_os_string(),
                ),
            ),
            (
                plan("/bin/sh", ShellIntegrationMode::Automatic),
                ShellIntegrationStatus::Unsupported,
                Vec::new(),
                ("ENV", integration.join("sh/spaceterm.sh").into_os_string()),
            ),
        ];
        for (plan, status, arguments, (name, value)) in cases {
            assert_eq!(plan.status, status);
            assert_eq!(plan.arguments, arguments);
            assert!(
                plan.environment
                    .contains(&(OsString::from(name), value.clone())),
                "{name}"
            );
            assert!(
                plan.environment
                    .iter()
                    .all(|(name, _)| name != "SPACETERM_SHELL_INTEGRATION_VERSION")
            );
        }
        let sh = plan("/bin/dash", ShellIntegrationMode::Disabled);
        assert!(
            sh.environment
                .contains(&("SPACETERM_SH_ENV".into(), "/Users/me/.shrc".into()))
        );
    }

    #[test]
    fn unknown_shells_and_missing_resources_leave_launch_untouched() {
        let inherited = ShellEnvironment::default();
        let fixture = ShellResourcesFixture::new();
        let cases = [
            plan_shell_integration(
                Path::new("/bin/zsh"),
                Path::new("/private/tmp/spaceterm-missing-resources"),
                ShellIntegrationMode::Automatic,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            ),
            plan_shell_integration(
                Path::new("/bin/sh"),
                Path::new("/private/tmp/spaceterm-missing-resources"),
                ShellIntegrationMode::Disabled,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            ),
            plan_shell_integration(
                Path::new("/fixture/ksh"),
                fixture.path(),
                ShellIntegrationMode::Automatic,
                &inherited,
                &ShellIntegrationPolicy::fixture(),
            ),
        ];

        assert_eq!(cases[0].status, ShellIntegrationStatus::MissingResources);
        assert_eq!(cases[1].status, ShellIntegrationStatus::Disabled);
        assert_eq!(cases[2].status, ShellIntegrationStatus::Unsupported);
        assert!(
            cases
                .iter()
                .all(|plan| plan.arguments.is_empty() && plan.environment.is_empty())
        );
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "unix_adapter_tests/shell_integration.rs"]
mod unix_adapter_tests;
