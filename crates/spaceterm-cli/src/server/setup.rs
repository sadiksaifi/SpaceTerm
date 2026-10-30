//! `server setup`: make this machine a server, on the machine itself.
//!
//! The steps run through the existing service code, in order: install the service from the
//! installation the launcher points at, authorize boot startup on Linux, start the server
//! unless it was deliberately stopped, and set Network Access. Each step reports `done` or
//! `already_done`, so a repeated setup changes only what its flags name. Setup never reverses a
//! deliberate Stop, and it needs no credential store.
use super::{Context, Reply};
use crate::args::{Network, Toggle};
use crate::contract::{
    self, Code, Details, Envelope, Failure, Launcher, ServerSetup, SetupNetwork, SetupStep,
    SetupStepName, StepResult, UnsupportedSessionDetails, UnsupportedSessionReason, Version,
};
use spaceterm_server::direct::{DEFAULT_PORT, NetworkPolicy};
use spaceterm_server::service::lane::Offline;
use spaceterm_server::service::{LingerAuthorization, Service};
use std::path::{Path, PathBuf};

const COMMAND: &str = "server.setup";

/// Where the launcher points, and whether that is one of this identity's installations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherState {
    pub launcher: Launcher,
    pub installation: Option<PathBuf>,
}

/// Reads `~/.local/bin/<identity>` without following anything else. A link into
/// `installations/<digest>/bin/` names that installation.
pub fn launcher(home: &Path, installations: &Path, identity: &str) -> LauncherState {
    let link = home.join(".local/bin").join(identity);
    let Ok(target) = std::fs::read_link(&link) else {
        return LauncherState {
            launcher: if link.symlink_metadata().is_ok() {
                Launcher::Foreign
            } else {
                Launcher::Missing
            },
            installation: None,
        };
    };
    let target = if target.is_absolute() {
        target
    } else {
        link.parent().map(|v| v.join(&target)).unwrap_or(target)
    };
    let installation = target
        .parent()
        .filter(|bin| bin.file_name().is_some_and(|v| v == "bin"))
        .and_then(Path::parent)
        .filter(|installation| installation.parent() == Some(installations))
        .filter(|installation| {
            installation
                .file_name()
                .and_then(|v| v.to_str())
                .is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .map(Path::to_owned);
    LauncherState {
        launcher: if installation.is_some() {
            Launcher::Linked
        } else {
            Launcher::Foreign
        },
        installation,
    }
}

/// Whether `~/.local/bin` is on `PATH`.
pub fn path_ok(home: &Path, path: Option<&std::ffi::OsStr>) -> bool {
    let bin = home.join(".local/bin");
    path.is_some_and(|path| std::env::split_paths(path).any(|entry| entry == bin))
}

/// The installation this executable runs from, when it is one of this identity's.
fn running_installation(installations: &Path) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let installation = executable.parent()?.parent()?;
    (installation.parent() == Some(installations)).then(|| installation.to_owned())
}

/// A Mac server runs only in a logged-in GUI session, because its LaunchAgent loads there.
#[cfg(target_os = "macos")]
fn gui_session() -> bool {
    // SAFETY: geteuid has no preconditions.
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    std::process::Command::new("/bin/launchctl")
        .args(["print", &domain])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn gui_session() -> bool {
    true
}

fn step(step: SetupStepName, result: StepResult) -> SetupStep {
    SetupStep { step, result }
}

pub fn run(context: &Context<'_>, network: Option<&Network>, automatic: Option<Toggle>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    if !gui_session() {
        return Reply::failure(
            COMMAND,
            None,
            Failure::with(
                Code::UnsupportedSession,
                Details::UnsupportedSession(UnsupportedSessionDetails {
                    reason: UnsupportedSessionReason::NoGuiSession,
                }),
            ),
        );
    }
    if network.is_none() && !context.interactive() {
        return Reply::failure(COMMAND, None, Failure::new(Code::InteractionRequired));
    }
    match setup(context, network, automatic) {
        Ok((data, text)) => {
            let server = super::local_ref(context.identity).ok();
            Reply::new(&Envelope::ok(COMMAND, server, data), text)
        }
        Err(failure) => Reply::failure(COMMAND, super::local_ref(context.identity).ok(), failure),
    }
}

fn setup(
    context: &Context<'_>,
    network: Option<&Network>,
    automatic: Option<Toggle>,
) -> Result<(ServerSetup, String), Failure> {
    let service = Service::for_identity(context.identity).map_err(contract::from_server_error)?;
    let installations = service.installations_directory();
    let home = service
        .account_home()
        .ok_or(Failure::new(Code::StorageUnavailable))?;
    let launcher = launcher(&home, &installations, context.identity);
    let mut steps = Vec::new();
    let before = service.status().map_err(contract::from_server_error)?;
    crate::interrupt::dispatch()?;
    // Linux starts the server only with boot startup authorized, so authorize it before the
    // first start. macOS starts at login and has no such step.
    let linger = match service.linger().map_err(contract::from_server_error)? {
        None => StepResult::Skipped,
        Some(true) => StepResult::AlreadyDone,
        Some(false) if context.interactive() => {
            service
                .enable_linger(LingerAuthorization::Terminal)
                .map_err(contract::from_server_error)?;
            StepResult::Done
        }
        Some(false) => return Err(Failure::new(Code::AuthorizationRequired)),
    };
    let install = if before.installed {
        StepResult::AlreadyDone
    } else {
        let installation = launcher
            .installation
            .clone()
            .or_else(|| running_installation(&installations))
            .ok_or(Failure::new(Code::NoCandidate))?;
        service
            .install(&installation)
            .map_err(contract::from_server_error)?;
        StepResult::Done
    };
    steps.push(step(SetupStepName::InstallService, install));
    steps.push(step(SetupStepName::Linger, linger));
    let status = service.status().map_err(contract::from_server_error)?;
    let start = if status.stopped {
        // Only `server start` reverses a deliberate Stop.
        StepResult::Skipped
    } else if status.running {
        if before.running || install == StepResult::AlreadyDone {
            StepResult::AlreadyDone
        } else {
            StepResult::Done
        }
    } else {
        service.connect().map_err(contract::from_server_error)?;
        StepResult::Done
    };
    steps.push(step(SetupStepName::Start, start));
    let connection = context.local_connection()?;
    let network_step = match network {
        None => StepResult::Skipped,
        Some(network) => set_network(&connection, network)?,
    };
    steps.push(step(SetupStepName::Network, network_step));
    let automatic_step = match automatic {
        None => StepResult::Skipped,
        Some(toggle) => {
            let enabled = toggle == Toggle::On;
            if service
                .status()
                .map_err(contract::from_server_error)?
                .enabled
                == enabled
            {
                StepResult::AlreadyDone
            } else {
                service
                    .set_automatic_start(enabled)
                    .map_err(contract::from_server_error)?;
                StepResult::Done
            }
        }
    };
    steps.push(step(SetupStepName::AutomaticStart, automatic_step));
    let status = service.status().map_err(contract::from_server_error)?;
    let lane = Offline::new(context.identity)
        .and_then(|offline| offline.admin().status(None))
        .map_err(contract::from_server_error)?;
    let network = match connection.direct_status() {
        Ok(direct) => SetupNetwork {
            enabled: direct.enabled,
            bind: direct.policy.as_ref().map(|v| v.bind.to_string()),
            addresses: direct
                .network_addresses
                .map(|v| v.available)
                .unwrap_or_default(),
        },
        Err(_) => SetupNetwork {
            enabled: lane.network.enabled,
            bind: lane.network.bind.clone(),
            addresses: Vec::new(),
        },
    };
    let data = ServerSetup {
        steps,
        runtime: Version {
            version: lane.runtime.version.clone(),
            commit: lane.runtime.commit.clone(),
            sha256: lane.runtime.sha256.clone(),
        },
        network,
        automatic_start: status.enabled,
        launcher: launcher.launcher,
        path_ok: path_ok(&home, std::env::var_os("PATH").as_deref()),
    };
    let text = text(&data);
    Ok((data, text))
}

/// Applies `--network` through the running server. An unchanged policy is `already_done`. The
/// server readies the new listener before it changes anything, so an address it can't listen at
/// fails the step and keeps the current Network Access.
fn set_network(
    connection: &spaceterm_server::ServerConnection,
    network: &Network,
) -> Result<StepResult, Failure> {
    let current = connection
        .direct_status()
        .map_err(contract::from_direct_error)?;
    let (policy, enabled) = match network {
        Network::Off => (None, false),
        Network::All => {
            let port = current
                .policy
                .as_ref()
                .map_or(DEFAULT_PORT, |policy| policy.bind.port());
            (
                Some(NetworkPolicy::every_interface(port).map_err(contract::from_direct_error)?),
                true,
            )
        }
        Network::Address(policy) => (Some(policy.clone()), true),
    };
    let unchanged =
        current.enabled == enabled && (!enabled || current.policy.as_ref() == policy.as_ref());
    if unchanged {
        return Ok(StepResult::AlreadyDone);
    }
    let status = connection
        .set_direct_policy(policy.or(current.policy), enabled)
        .map_err(contract::from_direct_error)?;
    if let spaceterm_server::direct::ListenerState::Failed(error) = status.listener {
        return Err(contract::from_direct_error(error));
    }
    Ok(StepResult::Done)
}

fn text(data: &ServerSetup) -> String {
    let mut out = String::new();
    for step in &data.steps {
        let name = match step.step {
            SetupStepName::InstallService => "Install service",
            SetupStepName::Linger => "Boot startup",
            SetupStepName::Start => "Start server",
            SetupStepName::Network => "Network Access",
            SetupStepName::AutomaticStart => "Automatic start",
        };
        let result = match step.result {
            StepResult::Done => "done",
            StepResult::AlreadyDone => "already done",
            StepResult::Skipped => "skipped",
        };
        out.push_str(&format!("{name}: {result}\n"));
    }
    out.push_str(&format!("Version: {}\n", data.runtime.version));
    match (&data.network.bind, data.network.enabled) {
        (Some(bind), true) => out.push_str(&format!("Network Access: on at {bind}\n")),
        _ => out.push_str("Network Access: off\n"),
    }
    if !data.path_ok {
        out.push_str("Add ~/.local/bin to PATH to run spaceterm by name.\n");
    }
    out.push_str("Next, pair a device with: spaceterm server pair\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_names_only_this_identitys_installations() {
        let root = std::env::temp_dir().join(format!("spaceterm-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let installations = root.join("data/installations");
        let digest = "ab".repeat(32);
        std::fs::create_dir_all(installations.join(&digest).join("bin")).unwrap();
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        assert_eq!(
            launcher(&home, &installations, "spaceterm").launcher,
            Launcher::Missing
        );
        std::os::unix::fs::symlink(
            installations.join(&digest).join("bin/spaceterm"),
            home.join(".local/bin/spaceterm"),
        )
        .unwrap();
        assert_eq!(
            launcher(&home, &installations, "spaceterm"),
            LauncherState {
                launcher: Launcher::Linked,
                installation: Some(installations.join(&digest)),
            }
        );
        std::os::unix::fs::symlink("/usr/bin/true", home.join(".local/bin/spaceterm-preflight"))
            .unwrap();
        assert_eq!(
            launcher(&home, &installations, "spaceterm-preflight").launcher,
            Launcher::Foreign
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn path_ok_requires_the_launcher_directory_itself() {
        let home = Path::new("/home/someone");
        assert!(path_ok(
            home,
            Some("/usr/bin:/home/someone/.local/bin".as_ref())
        ));
        assert!(!path_ok(
            home,
            Some("/usr/bin:/home/someone/.local".as_ref())
        ));
        assert!(!path_ok(home, None));
    }
}
