//! `reset`: replaces the saved data on this machine that this SpaceTerm can't read: SpaceTerm
//! Core's data and this CLI's state, each only when it waits for a reset. Without `--confirm` it
//! previews which data resets and why. A reset moves the files aside and never deletes them.
use super::{Context, Reply, offline_token};
use crate::contract::{self, Confirmation, Envelope, Failure, ResetEffects, ResetRoot};
use spaceterm_core::migration::{self, DataKind, PendingReset, ResetReason};
use std::time::SystemTime;

const COMMAND: &str = "reset";
const PURPOSE: &str = "reset";
const CORE_DATA: &str = "core_data";
const CLI_STATE: &str = "cli_state";

/// The data roots `reset` reads and resets. Tests script them.
trait Roots {
    /// Every root on this machine that waits for a reset.
    fn pending(&self) -> Result<Vec<ResetRoot>, Failure>;
    /// Resets `root`, one of [`Self::pending`]'s.
    fn reset(&self, root: &str) -> Result<(), Failure>;
}

struct Native<'a> {
    identity: &'a str,
}

impl Roots for Native<'_> {
    fn pending(&self) -> Result<Vec<ResetRoot>, Failure> {
        let root = |root, pending: Option<PendingReset>| {
            pending.map(|pending| ResetRoot {
                root,
                reason: pending.reason,
                kinds: pending.kinds,
            })
        };
        let core = migration::core_data::pending_reset(self.identity)
            .map_err(contract::from_core_error)?;
        let cli = crate::state::pending_reset(self.identity).map_err(contract::from_core_error)?;
        Ok([root(CORE_DATA, core), root(CLI_STATE, cli)]
            .into_iter()
            .flatten()
            .collect())
    }

    fn reset(&self, root: &str) -> Result<(), Failure> {
        let reset = match root {
            CORE_DATA => migration::core_data::reset_confirmed(self.identity),
            _ => crate::state::reset_confirmed(self.identity),
        };
        reset.map(|_| ()).map_err(contract::from_core_error)
    }
}

pub fn run(context: &Context<'_>, token: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    reset(
        &Native {
            identity: context.identity,
        },
        token,
        SystemTime::now(),
    )
}

fn reset(roots: &dyn Roots, token: Option<&str>, now: SystemTime) -> Reply {
    let pending = match roots.pending() {
        Ok(pending) => pending,
        Err(failure) => return Reply::failure(COMMAND, None, failure),
    };
    if pending.is_empty() {
        return Reply::new(
            &Envelope::ok(COMMAND, None, ResetEffects { reset: pending }),
            "No saved data on this machine needs a reset.\n".into(),
        );
    }
    let observation = serde_json::to_vec(&pending).unwrap_or_default();
    let Some(token) = token else {
        let issued = offline_token::issue(PURPOSE, &observation, now);
        let text = format!(
            "Reset {}? {} A reset moves the files aside and never deletes them.{}\nConfirm with: spaceterm reset --confirm {}\n",
            subjects(&pending),
            reason(&pending),
            consequences(&pending),
            issued.token,
        );
        return Reply::new(
            &Envelope::confirmation(
                COMMAND,
                None,
                Confirmation {
                    token: issued.token,
                    expires_at: issued.expires_at,
                    effects: ResetEffects {
                        reset: pending.clone(),
                    },
                },
            ),
            text,
        );
    };
    if let Err(failure) = offline_token::verify(PURPOSE, &observation, token, now) {
        return Reply::failure(COMMAND, None, failure);
    }
    for root in &pending {
        if let Err(failure) = roots.reset(root.root) {
            return Reply::failure(COMMAND, None, failure);
        }
    }
    let text = format!("Reset {}. The files moved aside.\n", subjects(&pending));
    Reply::new(
        &Envelope::ok(COMMAND, None, ResetEffects { reset: pending }),
        text,
    )
}

/// "SpaceTerm Core's Machine ID and this CLI's saved machines".
fn subjects(pending: &[ResetRoot]) -> String {
    let named: Vec<String> = pending
        .iter()
        .map(|root| {
            let owner = if root.root == CORE_DATA {
                "SpaceTerm Core\u{2019}s"
            } else {
                "this CLI\u{2019}s"
            };
            format!("{owner} {}", migration::nouns(&root.kinds))
        })
        .collect();
    named.join(" and ")
}

fn reason(pending: &[ResetRoot]) -> &'static str {
    if pending
        .iter()
        .any(|root| root.reason == ResetReason::TooOld)
    {
        "This SpaceTerm can\u{2019}t read them, because a SpaceTerm too old to update directly saved them. To keep them, install an earlier SpaceTerm release instead, then update from it."
    } else {
        "This SpaceTerm can\u{2019}t carry them forward from the version that saved them."
    }
}

fn consequences(pending: &[ResetRoot]) -> String {
    let has = |kind| pending.iter().any(|root| root.kinds.contains(&kind));
    let mut text = String::new();
    if has(DataKind::MachineId) || has(DataKind::Pairings) {
        text.push_str(" Paired devices must pair with this machine again.");
    }
    if has(DataKind::SavedConnections) {
        text.push_str(" Machines this CLI reached must be added again.");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Scripted {
        pending: RefCell<Vec<ResetRoot>>,
        reset: RefCell<Vec<String>>,
    }

    impl Roots for Scripted {
        fn pending(&self) -> Result<Vec<ResetRoot>, Failure> {
            Ok(self.pending.borrow().clone())
        }
        fn reset(&self, root: &str) -> Result<(), Failure> {
            self.reset.borrow_mut().push(root.into());
            self.pending
                .borrow_mut()
                .retain(|pending| pending.root != root);
            Ok(())
        }
    }

    fn json(reply: &Reply) -> serde_json::Value {
        serde_json::to_value(&reply.envelope).unwrap()
    }

    fn scripted() -> Scripted {
        Scripted {
            pending: RefCell::new(vec![
                ResetRoot {
                    root: CORE_DATA,
                    reason: ResetReason::TooOld,
                    kinds: vec![DataKind::WorkspaceOrganization, DataKind::MachineId],
                },
                ResetRoot {
                    root: CLI_STATE,
                    reason: ResetReason::Invalid,
                    kinds: vec![DataKind::SavedConnections],
                },
            ]),
            reset: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn a_preview_resets_nothing_and_its_token_resets_exactly_what_it_named() {
        let roots = scripted();
        let now = SystemTime::now();
        let preview = reset(&roots, None, now);
        assert!(roots.reset.borrow().is_empty());
        assert!(preview.stdout.starts_with(
            "Reset SpaceTerm Core\u{2019}s Workspace organization and Machine ID and this CLI\u{2019}s saved machines? This SpaceTerm can\u{2019}t read them, because a SpaceTerm too old to update directly saved them."
        ));
        let token = json(&preview)["data"]["confirmation"]["token"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            json(&preview)["data"]["confirmation"]["effects"]["reset"][0]["kinds"],
            serde_json::json!(["workspace_organization", "machine_id"])
        );

        let done = reset(&roots, Some(&token), now);
        assert_eq!(json(&done)["status"], "ok", "{}", done.stdout);
        assert_eq!(*roots.reset.borrow(), [CORE_DATA, CLI_STATE]);
        // Nothing waits any more, so the same token resets nothing again.
        assert_eq!(
            reset(&roots, Some(&token), now).stdout,
            "No saved data on this machine needs a reset.\n"
        );
        assert_eq!(roots.reset.borrow().len(), 2);
    }

    #[test]
    fn a_token_for_other_pending_data_resets_nothing() {
        let roots = scripted();
        let now = SystemTime::now();
        let preview = reset(&roots, None, now);
        let token = json(&preview)["data"]["confirmation"]["token"]
            .as_str()
            .unwrap()
            .to_owned();
        roots.pending.borrow_mut()[1]
            .kinds
            .push(DataKind::ClientRecords);
        let refused = reset(&roots, Some(&token), now);
        assert_eq!(json(&refused)["error"]["code"], "changed");
        assert!(roots.reset.borrow().is_empty());
    }
}
