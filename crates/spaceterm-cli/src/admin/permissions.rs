//! `permissions`: the System Permissions programs in this machine's Terminal Sessions inherit
//! from SpaceTerm Core, and the two ways to set one up.
//!
//! Only the machine itself sets these up: [route validation](crate::connection::route) refuses
//! a paired `--machine` before anything is sent, and SpaceTerm Core's lane refuses a paired
//! caller as well. Status and requests ask the running SpaceTerm Core over its account-local
//! lane, because it is the responsible process whose grants its Terminal Sessions receive.
//! `settings` opens System Settings from this command, which runs in the application the person
//! uses, because macOS brings System Settings forward only for the active application.
use super::{Context, Pending, Reply};
use crate::args::{PermissionName, PermissionsCommand};
use crate::contract::{
    Code, CorePermissions, Envelope, Failure, MachineRef, PermissionRequested, PermissionSettings,
};
use crate::state::RouteKey;
use spaceterm_core::permissions::{
    AccessibilityNaming, PermissionHolder, PermissionState, RequestOutcome, SystemPermission,
};
use spaceterm_core::service::lane::{LaneReply, LaneRequest};
use std::time::{Duration, Instant};

/// A probe answers within three seconds.
const LIMIT: Duration = Duration::from_secs(30);
/// Launch Services answers within a second. A slower `open` is stuck and is stopped.
const OPEN_LIMIT: Duration = Duration::from_secs(10);

pub fn run(context: &Context<'_>, command: Option<&PermissionsCommand>) -> Reply {
    let path = match command {
        None => "permissions",
        Some(PermissionsCommand::Request { .. }) => "permissions.request",
        Some(PermissionsCommand::Settings { .. }) => "permissions.settings",
    };
    if !context.is_local() {
        return Reply::failure(path, None, Failure::new(Code::LocalOnly));
    }
    let core = match super::local_ref(context.identity) {
        Ok(core) => core,
        Err(failure) => return Reply::failure(path, None, failure),
    };
    let request = match command {
        None => LaneRequest::Permissions,
        Some(PermissionsCommand::Request { permission }) => LaneRequest::RequestPermission {
            permission: permission.permission(),
        },
        Some(PermissionsCommand::Settings { permission }) => {
            return match open_settings(permission.permission()) {
                Ok(()) => {
                    let (envelope, text) = settings(core, permission.permission());
                    Reply::new(&envelope, text)
                }
                Err(failure) => Reply::failure(path, Some(core), failure),
            };
        }
    };
    let opened = Pending::open(context, &RouteKey::Local).and_then(|pending| {
        super::open_lane(
            context,
            &context.local_connection(),
            &RouteKey::Local,
            &pending,
        )
    });
    let mut opened = match opened {
        Ok(opened) => opened,
        Err(failure) => return Reply::failure(path, Some(core), failure),
    };
    let reply = opened
        .session
        .call(&request, Instant::now() + context.timeout(LIMIT))
        .map_err(super::lane_failure);
    // A request answers with the report it was made against, so its text can name the holder.
    let report = match command {
        Some(PermissionsCommand::Request { .. }) => opened
            .session
            .call(
                &LaneRequest::Permissions,
                Instant::now() + context.timeout(LIMIT),
            )
            .ok(),
        _ => None,
    };
    let recovered = opened.recovered;
    match (command, reply) {
        (None, Ok(LaneReply::Permissions(report))) => {
            let (mut envelope, text) = status(core, &report);
            envelope.data.recovered_operations = recovered;
            Reply::new(&envelope, text)
        }
        (
            Some(PermissionsCommand::Request { permission }),
            Ok(LaneReply::PermissionRequested(outcome)),
        ) => {
            let report = match report {
                Some(LaneReply::Permissions(report)) => Some(report),
                _ => None,
            };
            let (mut envelope, text) = requested(
                context,
                core,
                permission.permission(),
                outcome,
                report.as_ref(),
            );
            envelope.data.recovered_operations = recovered;
            Reply::new(&envelope, text)
        }
        (_, Ok(_)) => Reply::failure(path, Some(core), Failure::new(Code::ProtocolError)),
        (_, Err(failure)) => Reply::failure(path, Some(core), failure),
    }
}

/// The `permissions` envelope and its text.
pub(crate) fn status(
    core: MachineRef,
    report: &spaceterm_core::permissions::PermissionsReport,
) -> (Envelope<CorePermissions>, String) {
    let naming = report.accessibility_naming;
    let width = report
        .permissions
        .iter()
        .map(|reading| name(reading.permission, naming).len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for reading in &report.permissions {
        text.push_str(&format!(
            "{:width$}  {}\n",
            name(reading.permission, naming),
            state(reading.state),
        ));
    }
    text.push_str(&holder_sentence(&report.holder));
    text.push('\n');
    let envelope = Envelope::ok(
        "permissions",
        Some(core),
        CorePermissions {
            permissions: report.permissions.clone(),
            accessibility_naming: naming,
            holder: report.holder.clone(),
        },
    );
    (envelope, text)
}

/// The `permissions.request` envelope and its text.
pub(crate) fn requested(
    context: &Context<'_>,
    core: MachineRef,
    permission: SystemPermission,
    outcome: RequestOutcome,
    report: Option<&spaceterm_core::permissions::PermissionsReport>,
) -> (Envelope<PermissionRequested>, String) {
    let naming = report.map_or_else(AccessibilityNaming::default, |report| {
        report.accessibility_naming
    });
    let product = spaceterm_core::service::bundle_name(context.identity)
        .and_then(|name| name.strip_suffix(".app"))
        .unwrap_or("SpaceTerm");
    let holder = report.map_or(product, |report| holder_name(&report.holder));
    let name = name(permission, naming);
    let settings = || {
        context.command_line(
            &core,
            &[
                "permissions",
                "settings",
                PermissionName::of(permission).spelling(),
            ],
        )
    };
    let text = match outcome {
        RequestOutcome::Requested if permission.prompts() => {
            "Answer the system prompt on this Mac.".to_owned()
        }
        RequestOutcome::Requested => format!(
            "Turn on {holder} in System Settings > Privacy & Security > {}. Open it with: {}",
            pane(permission, naming),
            settings()
        ),
        RequestOutcome::Decided {
            state: PermissionState::Granted,
        } => format!("{name} is already allowed."),
        RequestOutcome::Decided {
            state: PermissionState::Restricted,
        } => format!("An administrator restricts {name} on this Mac."),
        RequestOutcome::Decided { .. } => format!(
            "{name} was denied. Turn on {holder} in System Settings > Privacy & Security > {}. Open it with: {}",
            pane(permission, naming),
            settings()
        ),
    };
    let envelope = Envelope::ok(
        "permissions.request",
        Some(core),
        PermissionRequested {
            permission,
            outcome,
        },
    );
    (envelope, format!("{text}\n"))
}

/// Opens System Settings at the list that holds `permission`, through Launch Services.
fn open_settings(permission: SystemPermission) -> Result<(), Failure> {
    if !cfg!(target_os = "macos") {
        return Err(Failure::new(Code::Unsupported));
    }
    let mut child = std::process::Command::new("/usr/bin/open")
        .arg(spaceterm_core::permissions::settings_uri(permission))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| Failure::new(Code::PermissionFailed))?;
    let deadline = Instant::now() + OPEN_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(Failure::new(Code::PermissionFailed)),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::new(Code::PermissionFailed));
            }
        }
    }
}

/// The `permissions.settings` envelope and its text.
pub(crate) fn settings(
    core: MachineRef,
    permission: SystemPermission,
) -> (Envelope<PermissionSettings>, String) {
    let envelope = Envelope::ok(
        "permissions.settings",
        Some(core),
        PermissionSettings { permission },
    );
    (
        envelope,
        format!(
            "Opened System Settings > Privacy & Security > {}.\n",
            pane(permission, AccessibilityNaming::default())
        ),
    )
}

/// The permission's name in running text, as System Settings names it.
fn name(permission: SystemPermission, naming: AccessibilityNaming) -> &'static str {
    match (permission, naming) {
        (SystemPermission::Microphone, _) => "Microphone",
        (SystemPermission::Camera, _) => "Camera",
        (SystemPermission::ScreenRecording, _) => "Screen Recording",
        (SystemPermission::Accessibility, AccessibilityNaming::Accessibility) => "Accessibility",
        (SystemPermission::Accessibility, AccessibilityNaming::DeviceControl) => "Device Control",
    }
}

/// The System Settings list that holds the permission.
fn pane(permission: SystemPermission, naming: AccessibilityNaming) -> &'static str {
    match (permission, naming) {
        (SystemPermission::Microphone, _) => "Microphone",
        (SystemPermission::Camera, _) => "Camera",
        (SystemPermission::ScreenRecording, _) => "Screen & System Audio Recording",
        (SystemPermission::Accessibility, AccessibilityNaming::Accessibility) => "Accessibility",
        (SystemPermission::Accessibility, AccessibilityNaming::DeviceControl) => {
            "Device Control and Data Access"
        }
    }
}

fn state(state: PermissionState) -> &'static str {
    match state {
        PermissionState::NotRequested => "not requested",
        PermissionState::Granted => "allowed",
        PermissionState::NotGranted => "not allowed",
        PermissionState::Restricted => "restricted by an administrator",
    }
}

fn holder_name(holder: &PermissionHolder) -> &str {
    match holder {
        PermissionHolder::Core { name } => name,
        PermissionHolder::Launcher => "the app that started SpaceTerm",
    }
}

fn holder_sentence(holder: &PermissionHolder) -> String {
    match holder {
        PermissionHolder::Core { name } => {
            format!("System Settings lists these permissions under {name}.")
        }
        PermissionHolder::Launcher => {
            "The app that started SpaceTerm holds these permissions.".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Global;
    use spaceterm_core::permissions::{PermissionReading, PermissionsReport};

    fn core() -> MachineRef {
        MachineRef {
            id: "ab".repeat(32),
            name: "local".into(),
            route: "local".into(),
        }
    }

    fn report(holder: PermissionHolder) -> PermissionsReport {
        PermissionsReport {
            permissions: vec![
                PermissionReading {
                    permission: SystemPermission::Microphone,
                    state: PermissionState::NotRequested,
                },
                PermissionReading {
                    permission: SystemPermission::Camera,
                    state: PermissionState::NotGranted,
                },
                PermissionReading {
                    permission: SystemPermission::ScreenRecording,
                    state: PermissionState::Granted,
                },
                PermissionReading {
                    permission: SystemPermission::Accessibility,
                    state: PermissionState::NotGranted,
                },
            ],
            accessibility_naming: AccessibilityNaming::DeviceControl,
            holder,
            resettable: true,
        }
    }

    fn global(core: Option<&str>) -> Global {
        Global {
            machine: core.map(Into::into),
            agent: None,
            json: true,
            quiet: false,
            verbose: false,
            no_input: true,
            timeout: None,
        }
    }

    #[test]
    fn the_status_names_each_permission_and_its_holder() {
        let (envelope, text) = status(
            core(),
            &report(PermissionHolder::Core {
                name: "SpaceTerm".into(),
            }),
        );
        crate::contract::golden::assert_golden("permissions", &envelope);
        assert_eq!(
            text,
            "Microphone        not requested\n\
             Camera            not allowed\n\
             Screen Recording  allowed\n\
             Device Control    not allowed\n\
             System Settings lists these permissions under SpaceTerm.\n"
        );
        let (_, text) = status(core(), &report(PermissionHolder::Launcher));
        assert!(text.ends_with("The app that started SpaceTerm holds these permissions.\n"));
    }

    #[test]
    fn a_request_says_what_the_person_does_next() {
        let global = global(None);
        let context = Context {
            credentials: crate::connection::CliCredentials::new("spaceterm"),
            facts: Default::default(),
            identity: "spaceterm",
            global: &global,
        };
        let held = report(PermissionHolder::Core {
            name: "SpaceTerm".into(),
        });
        let text =
            |permission, outcome| requested(&context, core(), permission, outcome, Some(&held)).1;
        assert_eq!(
            text(SystemPermission::Microphone, RequestOutcome::Requested),
            "Answer the system prompt on this Mac.\n"
        );
        assert_eq!(
            text(SystemPermission::Camera, RequestOutcome::Requested),
            "Answer the system prompt on this Mac.\n"
        );
        assert_eq!(
            text(
                SystemPermission::Camera,
                RequestOutcome::Decided {
                    state: PermissionState::NotGranted
                }
            ),
            "Camera was denied. Turn on SpaceTerm in System Settings > Privacy & Security > Camera. Open it with: spaceterm --machine local permissions settings camera\n"
        );
        assert_eq!(
            text(SystemPermission::Accessibility, RequestOutcome::Requested),
            "Turn on SpaceTerm in System Settings > Privacy & Security > Device Control and Data Access. Open it with: spaceterm --machine local permissions settings accessibility\n"
        );
        assert_eq!(
            text(
                SystemPermission::Microphone,
                RequestOutcome::Decided {
                    state: PermissionState::NotGranted
                }
            ),
            "Microphone was denied. Turn on SpaceTerm in System Settings > Privacy & Security > Microphone. Open it with: spaceterm --machine local permissions settings microphone\n"
        );
        assert_eq!(
            text(
                SystemPermission::ScreenRecording,
                RequestOutcome::Decided {
                    state: PermissionState::Granted
                }
            ),
            "Screen Recording is already allowed.\n"
        );
        let (envelope, _) = requested(
            &context,
            core(),
            SystemPermission::Microphone,
            RequestOutcome::Requested,
            Some(&held),
        );
        crate::contract::golden::assert_golden("permissions_request", &envelope);
    }

    #[test]
    fn a_paired_machine_is_refused_before_anything_is_sent() {
        let global = global(Some("studio"));
        let context = Context {
            credentials: crate::connection::CliCredentials::new("spaceterm-development"),
            facts: Default::default(),
            identity: "spaceterm-development",
            global: &global,
        };
        for command in [
            None,
            Some(PermissionsCommand::Request {
                permission: PermissionName::Microphone,
            }),
            Some(PermissionsCommand::Settings {
                permission: PermissionName::ScreenRecording,
            }),
        ] {
            let reply = run(&context, command.as_ref());
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let exit = reply.write(
                crate::contract::Mode {
                    json: true,
                    ..crate::contract::Mode::default()
                },
                &mut stdout,
                &mut stderr,
            );
            let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(value["error"]["code"], "local_only", "{command:?}");
            assert_eq!(value["machine"], serde_json::Value::Null);
            assert_eq!(exit, Code::LocalOnly.exit());
        }
    }
}
