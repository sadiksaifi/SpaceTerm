//! `server permissions`: the System Permissions programs in this machine's Terminal Sessions
//! inherit from its server, and the two ways to set one up.
//!
//! Only the server's own machine sets these up: [route validation](crate::connection::route)
//! refuses a remote `--server` before anything is sent, and the server's lane refuses a paired
//! caller as well. Each command asks the running server over its account-local lane, because the
//! server is the responsible process whose grants its Terminal Sessions receive.
use super::{Context, Pending, Reply};
use crate::args::{PermissionName, PermissionsCommand};
use crate::contract::{
    Code, Envelope, Failure, PermissionRequested, PermissionSettings, ServerPermissions, ServerRef,
};
use crate::state::RouteKey;
use spaceterm_server::permissions::{
    AccessibilityNaming, PermissionHolder, PermissionState, RequestOutcome, SystemPermission,
};
use spaceterm_server::service::lane::{LaneReply, LaneRequest};
use std::time::{Duration, Instant};

/// A probe answers within three seconds, and opening System Settings within ten.
const LIMIT: Duration = Duration::from_secs(30);

pub fn run(context: &Context<'_>, command: Option<&PermissionsCommand>) -> Reply {
    let path = match command {
        None => "server.permissions",
        Some(PermissionsCommand::Request { .. }) => "server.permissions.request",
        Some(PermissionsCommand::Settings { .. }) => "server.permissions.settings",
    };
    if !context.is_local() {
        return Reply::failure(path, None, Failure::new(Code::LocalOnly));
    }
    let server = match super::local_ref(context.identity) {
        Ok(server) => server,
        Err(failure) => return Reply::failure(path, None, failure),
    };
    let request = match command {
        None => LaneRequest::Permissions,
        Some(PermissionsCommand::Request { permission }) => LaneRequest::RequestPermission {
            permission: permission.permission(),
        },
        Some(PermissionsCommand::Settings { permission }) => LaneRequest::OpenPermissionSettings {
            permission: permission.permission(),
        },
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
        Err(failure) => return Reply::failure(path, Some(server), failure),
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
            let (mut envelope, text) = status(server, &report);
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
                server,
                permission.permission(),
                outcome,
                report.as_ref(),
            );
            envelope.data.recovered_operations = recovered;
            Reply::new(&envelope, text)
        }
        (
            Some(PermissionsCommand::Settings { permission }),
            Ok(LaneReply::PermissionSettingsOpened),
        ) => {
            let (mut envelope, text) = settings(server, permission.permission());
            envelope.data.recovered_operations = recovered;
            Reply::new(&envelope, text)
        }
        (_, Ok(_)) => Reply::failure(path, Some(server), Failure::new(Code::ProtocolError)),
        (_, Err(failure)) => Reply::failure(path, Some(server), failure),
    }
}

/// The `server.permissions` envelope and its text.
pub(crate) fn status(
    server: ServerRef,
    report: &spaceterm_server::permissions::PermissionsReport,
) -> (Envelope<ServerPermissions>, String) {
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
        "server.permissions",
        Some(server),
        ServerPermissions {
            permissions: report.permissions.clone(),
            accessibility_naming: naming,
            holder: report.holder.clone(),
        },
    );
    (envelope, text)
}

/// The `server.permissions.request` envelope and its text.
pub(crate) fn requested(
    context: &Context<'_>,
    server: ServerRef,
    permission: SystemPermission,
    outcome: RequestOutcome,
    report: Option<&spaceterm_server::permissions::PermissionsReport>,
) -> (Envelope<PermissionRequested>, String) {
    let naming = report.map_or_else(AccessibilityNaming::default, |report| {
        report.accessibility_naming
    });
    let holder = report.map_or("the server", |report| holder_name(&report.holder));
    let name = name(permission, naming);
    let settings = || {
        context.command_line(
            &server,
            &[
                "server",
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
        "server.permissions.request",
        Some(server),
        PermissionRequested {
            permission,
            outcome,
        },
    );
    (envelope, format!("{text}\n"))
}

/// The `server.permissions.settings` envelope and its text.
pub(crate) fn settings(
    server: ServerRef,
    permission: SystemPermission,
) -> (Envelope<PermissionSettings>, String) {
    let envelope = Envelope::ok(
        "server.permissions.settings",
        Some(server),
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
        (SystemPermission::ScreenRecording, _) => "Screen Recording",
        (SystemPermission::Accessibility, AccessibilityNaming::Accessibility) => "Accessibility",
        (SystemPermission::Accessibility, AccessibilityNaming::DeviceControl) => "Device Control",
    }
}

/// The System Settings list that holds the permission.
fn pane(permission: SystemPermission, naming: AccessibilityNaming) -> &'static str {
    match (permission, naming) {
        (SystemPermission::Microphone, _) => "Microphone",
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
        PermissionHolder::Server { name } => name,
        PermissionHolder::Launcher => "the app that started the server",
    }
}

fn holder_sentence(holder: &PermissionHolder) -> String {
    match holder {
        PermissionHolder::Server { name } => {
            format!("System Settings lists these permissions under {name}.")
        }
        PermissionHolder::Launcher => {
            "The app that started the server holds these permissions.".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Global;
    use spaceterm_server::permissions::{PermissionReading, PermissionsReport};

    fn server() -> ServerRef {
        ServerRef {
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

    fn global(server: Option<&str>) -> Global {
        Global {
            server: server.map(Into::into),
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
            server(),
            &report(PermissionHolder::Server {
                name: "SpaceTerm Server".into(),
            }),
        );
        crate::contract::golden::assert_golden("server_permissions", &envelope);
        assert_eq!(
            text,
            "Microphone        not requested\n\
             Screen Recording  allowed\n\
             Device Control    not allowed\n\
             System Settings lists these permissions under SpaceTerm Server.\n"
        );
        let (_, text) = status(server(), &report(PermissionHolder::Launcher));
        assert!(text.ends_with("The app that started the server holds these permissions.\n"));
    }

    #[test]
    fn a_request_says_what_the_person_does_next() {
        let global = global(None);
        let context = Context {
            identity: "spaceterm",
            global: &global,
        };
        let held = report(PermissionHolder::Server {
            name: "SpaceTerm Server".into(),
        });
        let text =
            |permission, outcome| requested(&context, server(), permission, outcome, Some(&held)).1;
        assert_eq!(
            text(SystemPermission::Microphone, RequestOutcome::Requested),
            "Answer the system prompt on this Mac.\n"
        );
        assert_eq!(
            text(SystemPermission::Accessibility, RequestOutcome::Requested),
            "Turn on SpaceTerm Server in System Settings > Privacy & Security > Device Control and Data Access. Open it with: spaceterm --server local server permissions settings accessibility\n"
        );
        assert_eq!(
            text(
                SystemPermission::Microphone,
                RequestOutcome::Decided {
                    state: PermissionState::NotGranted
                }
            ),
            "Microphone was denied. Turn on SpaceTerm Server in System Settings > Privacy & Security > Microphone. Open it with: spaceterm --server local server permissions settings microphone\n"
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
            server(),
            SystemPermission::Microphone,
            RequestOutcome::Requested,
            Some(&held),
        );
        crate::contract::golden::assert_golden("server_permissions_request", &envelope);
    }

    #[test]
    fn a_remote_server_is_refused_before_anything_is_sent() {
        let global = global(Some("studio"));
        let context = Context {
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
            assert_eq!(value["server"], serde_json::Value::Null);
            assert_eq!(exit, Code::LocalOnly.exit());
        }
    }
}
