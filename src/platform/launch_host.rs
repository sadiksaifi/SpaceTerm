//! POSIX host facts selected by application composition for shell launch planning.

use std::path::{Path, PathBuf};

/// The installed resource root beside the executable, or the source tree assets in development.
pub(crate) fn resource_root() -> PathBuf {
    if let Ok(executable) = std::env::current_exe()
        && let Some(resources) = installed_resource_root(&executable)
        && resources.join("shell-integration").is_dir()
    {
        return resources;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// `SpaceTerm.app/Contents/MacOS/spaceterm` reads `SpaceTerm.app/Contents/Resources`.
#[cfg(target_os = "macos")]
fn installed_resource_root(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    Some(macos.parent()?.join("Resources"))
}

/// `<prefix>/bin/spaceterm` reads `<prefix>/share/spaceterm`.
#[cfg(target_os = "linux")]
fn installed_resource_root(executable: &Path) -> Option<PathBuf> {
    let prefix = executable.parent()?.parent()?;
    Some(prefix.join("share").join("spaceterm"))
}

pub(crate) fn user_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(fallback_shell)
}

#[cfg(target_os = "macos")]
fn fallback_shell() -> PathBuf {
    PathBuf::from("/bin/zsh")
}

/// The account login shell from the password database, else the POSIX shell.
#[cfg(target_os = "linux")]
fn fallback_shell() -> PathBuf {
    account_shell().unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

#[cfg(target_os = "linux")]
fn account_shell() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    let mut buffer = vec![0_u8; 4096];
    loop {
        // SAFETY: passwd is plain C data whose zero value is valid storage for getpwuid_r.
        let mut entry = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut result = std::ptr::null_mut();
        // SAFETY: entry, buffer, and result are writable for the call; getpwuid_r stores string
        // pointers into buffer, which outlives every read below.
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() || entry.pw_shell.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r succeeded, so pw_shell is a NUL-terminated string inside buffer.
        let shell = unsafe { CStr::from_ptr(entry.pw_shell) };
        let shell = Path::new(std::ffi::OsStr::from_bytes(shell.to_bytes()));
        return shell.is_absolute().then(|| shell.to_path_buf());
    }
}

/// macOS ships Bash 3.2 as `/bin/bash`, which lacks the hooks the integration script needs.
#[cfg(target_os = "macos")]
fn shell_integration_supported(shell: &Path) -> bool {
    shell != Path::new("/bin/bash")
}

#[cfg(target_os = "linux")]
fn shell_integration_supported(_shell: &Path) -> bool {
    true
}

/// The account name shown as the Local Terminal origin, from the login environment.
pub(crate) fn local_user() -> Option<String> {
    ["USER", "LOGNAME"].into_iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
    })
}

pub(crate) fn local_hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: `buffer` is writable for its complete length and gethostname writes at most that
    // many bytes. A missing terminator is handled by using the full initialized buffer.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    std::str::from_utf8(&buffer[..length])
        .ok()
        .filter(|hostname| !hostname.is_empty() && !hostname.chars().any(char::is_control))
        .map(ToOwned::to_owned)
}

/// macOS removes only the shared foreign terminal variables.
#[cfg(target_os = "macos")]
const HOST_RUNTIME_ENVIRONMENT: &[&str] = &[];

/// Desktop launch tokens and the markers of terminals that commonly start SpaceTerm on Linux.
#[cfg(target_os = "linux")]
const HOST_RUNTIME_ENVIRONMENT: &[&str] = &[
    "ALACRITTY_LOG",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "DESKTOP_STARTUP_ID",
    "GIO_LAUNCHED_DESKTOP_FILE",
    "GIO_LAUNCHED_DESKTOP_FILE_PID",
    "GNOME_TERMINAL_SCREEN",
    "GNOME_TERMINAL_SERVICE",
    "KITTY_INSTALLATION_DIR",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_WINDOW",
    "KONSOLE_VERSION",
    "TERMINATOR_DBUS_NAME",
    "TERMINATOR_DBUS_PATH",
    "TERMINATOR_UUID",
    "TILIX_ID",
    "VTE_VERSION",
    "WINDOWID",
    "XDG_ACTIVATION_TOKEN",
    "XTERM_LOCALE",
    "XTERM_SHELL",
    "XTERM_VERSION",
];

/// Capture every shell planning fact once at composition.
pub(crate) fn shell_launch_planner() -> super::shell_launch::ShellLaunchPlanner {
    shell_launch_planner_with(user_shell(), resource_root(), |key| std::env::var_os(key))
}

pub(super) fn shell_launch_planner_with(
    shell: PathBuf,
    resources: PathBuf,
    mut read: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> super::shell_launch::ShellLaunchPlanner {
    use super::shell_integration::{ShellEnvironment, ShellIntegrationPolicy, configured_mode};
    let policy = ShellIntegrationPolicy {
        supported: shell_integration_supported(&shell),
        path_list_separator: ':',
        fallback_xdg_data_dirs: "/usr/local/share:/usr/share".into(),
    };
    super::shell_launch::ShellLaunchPlanner::new(
        shell,
        resources,
        configured_mode(read("SPACETERM_SHELL_INTEGRATION").as_deref()),
        ShellEnvironment {
            xdg_data_dirs: read("XDG_DATA_DIRS"),
            zdotdir: read("ZDOTDIR"),
            env: read("ENV"),
        },
        policy,
        HOST_RUNTIME_ENVIRONMENT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_installed_resources_live_under_the_prefix_share_directory() {
        assert_eq!(
            installed_resource_root(Path::new("/opt/spaceterm/bin/spaceterm")),
            Some(PathBuf::from("/opt/spaceterm/share/spaceterm"))
        );
        assert!(shell_integration_supported(Path::new("/bin/bash")));
        assert!(fallback_shell().is_absolute());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_desktop_launch_variables_are_host_policy_not_shared_removals() {
        let directory = std::env::temp_dir();
        let removals = |planner: super::super::shell_launch::ShellLaunchPlanner| {
            planner
                .local(&directory)
                .unwrap()
                .environment_removals()
                .to_vec()
        };
        let host = removals(shell_launch_planner_with(
            "/fixture/zsh".into(),
            "/fixture/resources".into(),
            |_| None,
        ));
        let shared = removals(super::super::shell_launch::ShellLaunchPlanner::for_test(
            "/fixture/zsh".into(),
            "/fixture/resources".into(),
        ));
        for name in HOST_RUNTIME_ENVIRONMENT {
            assert!(
                host.contains(&(*name).into()),
                "{name} reaches Linux shells"
            );
            assert!(!shared.contains(&(*name).into()), "{name} is shared policy");
        }
        assert!(shared.iter().all(|name| host.contains(name)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_installed_resources_live_in_the_bundle() {
        assert_eq!(
            installed_resource_root(Path::new("/Applications/SpaceTerm.app/Contents/MacOS/spaceterm")),
            Some(PathBuf::from("/Applications/SpaceTerm.app/Contents/Resources"))
        );
        assert_eq!(installed_resource_root(Path::new("/usr/local/bin/spaceterm")), None);
        assert!(!shell_integration_supported(Path::new("/bin/bash")));
    }
}
