//! `reset`: reset this CLI's saved data that this version can't read, after a preview names the
//! kinds of data it replaces. The files move aside under the state root and are never deleted.
//! Only the CLI's own state root changes: SpaceTerm Core's Machine ID, pairings and Workspaces
//! live in its data root, which this command never touches.
use super::{Context, Reply};
use crate::contract::{Confirmation, Envelope, ResetData, ResetEffects, kind_names};
use crate::state::{Reset, ResetReport};
use spaceterm_core::migration::ResetReason;
use spaceterm_core::update::ordering::Floor;

const COMMAND: &str = "reset";

pub fn run(context: &Context<'_>, token: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    if let Err(failure) = crate::interrupt::dispatch() {
        return Reply::failure(COMMAND, None, failure);
    }
    match crate::state::reset(context.identity, token) {
        Ok(report) => reply(report, crate::state::build_floor(), &|token| {
            super::command_line(
                "local",
                context.agent().as_ref(),
                &["reset", "--confirm", token],
            )
        }),
        Err(failure) => Reply::failure(COMMAND, None, failure),
    }
}

/// The reply for `report` in a build whose Update Floor is `floor`. `confirm` is the command
/// line that confirms a preview's token.
fn reply(report: ResetReport, floor: Option<Floor>, confirm: &dyn Fn(&str) -> String) -> Reply {
    let (mut envelope, text) = match report.reset {
        Reset::NotNeeded => (
            Envelope::ok(COMMAND, None, ResetData { reset: Vec::new() }).erased(),
            "This CLI's saved data needs no reset.\n".to_owned(),
        ),
        Reset::Done { kinds } => {
            let text = format!(
                "Reset this CLI's {}. The previous files were set aside.\n",
                kind_names(&kinds)
            );
            (
                Envelope::ok(COMMAND, None, ResetData { reset: kinds }).erased(),
                text,
            )
        }
        Reset::Preview {
            reason,
            kinds,
            token,
        } => {
            // Only state too old for this build has a Stepping Stone that carries it forward.
            let floor = floor.filter(|_| reason == ResetReason::TooOld);
            let mut text = format!(
                "Reset this CLI's {}? This version can't read them. The files move aside and are not deleted. SpaceTerm Core's Machine ID, pairings and Workspaces stay.",
                kind_names(&kinds)
            );
            if let Some(floor) = floor {
                text.push_str(&format!(
                    " To keep them, install SpaceTerm Core {floor} first instead."
                ));
            }
            text.push_str(&format!("\nConfirm with: {}\n", confirm(&token.token)));
            (
                Envelope::confirmation(
                    COMMAND,
                    None,
                    Confirmation {
                        token: token.token,
                        expires_at: token.expires_at,
                        effects: ResetEffects {
                            reason,
                            kinds,
                            floor,
                        },
                    },
                )
                .erased(),
                text,
            )
        }
    };
    envelope.warnings.extend(report.notice);
    Reply::new(&envelope, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::offline_token::Issued;
    use crate::contract::{Mode, Warning};
    use spaceterm_core::migration::DataKind;

    fn preview(reason: ResetReason) -> ResetReport {
        ResetReport {
            reset: Reset::Preview {
                reason,
                kinds: vec![DataKind::SavedConnections, DataKind::ClientRecords],
                token: Issued {
                    token: "c1-00".into(),
                    expires_at: "2026-11-05T12:00:00Z".into(),
                },
            },
            notice: None,
        }
    }

    fn written(reply: &Reply, json: bool) -> (u8, String) {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mode = Mode {
            json,
            ..Mode::default()
        };
        let exit = reply.write_to(mode, false, &mut stdout, &mut stderr);
        (exit, String::from_utf8(stdout).unwrap())
    }

    #[test]
    fn a_preview_names_the_data_it_replaces_and_the_stepping_stone_that_keeps_it() {
        let floor = Floor::parse("1.1.0").ok();
        let confirm = |token: &str| format!("spaceterm --machine local reset --confirm {token}");
        let too_old = reply(preview(ResetReason::TooOld), floor, &confirm);
        assert_eq!(
            written(&too_old, false),
            (
                3,
                "Reset this CLI's saved machines with their credential references and client records? This version can't read them. The files move aside and are not deleted. SpaceTerm Core's Machine ID, pairings and Workspaces stay. To keep them, install SpaceTerm Core 1.1.0 first instead.\nConfirm with: spaceterm --machine local reset --confirm c1-00\n".to_owned()
            )
        );
        let (_, json) = written(&too_old, true);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "confirmation_required");
        assert_eq!(
            value["data"]["confirmation"]["effects"],
            serde_json::json!({
                "reason": "too_old",
                "kinds": ["saved_connections", "client_records"],
                "floor": "1.1.0",
            })
        );
        // Data this build can't migrate has no Stepping Stone to keep it.
        let (_, text) = written(
            &reply(preview(ResetReason::Invalid), floor, &confirm),
            false,
        );
        assert!(!text.contains("1.1.0"), "{text}");
    }

    #[test]
    fn a_reset_reports_the_data_it_replaced_and_the_automatic_reset_before_it() {
        let reply = reply(
            ResetReport {
                reset: Reset::Done {
                    kinds: vec![DataKind::SavedConnections],
                },
                notice: Some(Warning::data_reset(vec![DataKind::View])),
            },
            None,
            &|_| unreachable!(),
        );
        let (exit, json) = written(&reply, true);
        assert_eq!(exit, 0);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["data"],
            serde_json::json!({"reset": ["saved_connections"]})
        );
        assert_eq!(value["warnings"][0]["code"], "data_reset");
        assert_eq!(
            value["warnings"][0]["details"],
            serde_json::json!({"kinds": ["view"]})
        );
    }
}
