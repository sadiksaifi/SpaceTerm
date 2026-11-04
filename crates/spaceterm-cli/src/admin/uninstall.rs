//! `uninstall`: remove SpaceTerm Core's account service and its managed CLI launcher from this
//! machine. Workspaces, settings, pairings and installed versions remain. While SpaceTerm Core
//! runs, a preview names the Terminal Sessions that end and takes a confirmation first.
use super::{Context, Reply};
use crate::contract::{self, Code, Confirmation, Empty, Envelope, Failure, ServiceEffects};
use spaceterm_core::service::{CliUninstall, Service};

const COMMAND: &str = "uninstall";
const KEPT: &str = "Workspaces, settings, pairings and installed versions remain.";

pub fn run(context: &Context<'_>, token: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    if let Err(failure) = crate::interrupt::dispatch() {
        return Reply::failure(COMMAND, None, failure);
    }
    let result = Service::for_identity(context.identity)
        .and_then(|service| service.uninstall_cli(token))
        .map_err(contract::from_core_error);
    match result {
        Ok(CliUninstall::Completed) => Reply::new(
            &Envelope::ok(COMMAND, None, Empty {}),
            format!("SpaceTerm Core is uninstalled from this machine. {KEPT}\n"),
        ),
        Ok(CliUninstall::Preview {
            token,
            expires_at,
            sessions,
        }) => {
            let Some(expires_at) = i64::try_from(expires_at)
                .ok()
                .and_then(|value| jiff::Timestamp::from_second(value).ok())
            else {
                return Reply::failure(COMMAND, None, Failure::new(Code::StorageUnavailable));
            };
            let effects = ServiceEffects {
                ends_sessions: u32::try_from(sessions.len()).unwrap_or(u32::MAX),
                sessions: sessions.iter().map(|id| super::hex(id)).collect(),
                pending: None,
            };
            let text = format!(
                "Uninstall SpaceTerm Core from this machine? Every Terminal Session on it ends, for every client. {KEPT}\nConfirm with: spaceterm uninstall --confirm {token}\n"
            );
            Reply::new(
                &Envelope::confirmation(
                    COMMAND,
                    None,
                    Confirmation {
                        token,
                        expires_at: expires_at.to_string(),
                        effects,
                    },
                ),
                text,
            )
        }
        Err(failure) => Reply::failure(COMMAND, None, failure),
    }
}
