//! Exact close consent and confirmation-free closes under the inventory's control fence.
use crate::{
    contract::{CloseEffects, Code, ControlState, EndedSession, OtherController},
    list::PaneFacts,
    scope::Snapshot,
};
use spaceterm_server::{
    hierarchy::{CloseConfirmation, CloseTarget, Id, Mutation},
    labels,
    pane::metadata::{CommandState, Freshness, PromptZone},
};

pub enum Review {
    Direct(Mutation),
    Confirm {
        confirmation: CloseConfirmation,
        effects: CloseEffects,
    },
}

pub fn review(
    snapshot: &Snapshot,
    target: CloseTarget,
    actor: Id,
    live: &[PaneFacts],
) -> Result<Review, Code> {
    let mut confirmation = snapshot
        .inventory
        .close_confirmation(target)
        .ok_or(Code::Changed)?;
    let mut idle = true;
    let mut sessions = Vec::new();
    let mut others = Vec::new();
    for reviewed in &confirmation.panes {
        let pane = snapshot
            .inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .find(|p| p.id == reviewed.id)
            .ok_or(Code::Changed)?;
        if reviewed.session.is_some() {
            idle &= pane.presentation.as_deref().is_some_and(|p| {
                p.freshness == Freshness::Live
                    && (matches!(
                        p.command.as_ref().map(|c| c.state),
                        Some(CommandState::Finished { .. })
                    ) || matches!(p.prompt_zone, PromptZone::Prompt | PromptZone::CommandInput))
            });
        }
        let label = pane
            .presentation
            .as_deref()
            .map(labels::pane_label)
            .unwrap_or_else(|| labels::Label::from_parts("Terminal", ""));
        if let Some(session) = reviewed.session {
            sessions.push(EndedSession {
                pane: super::hex(&pane.id),
                session: super::hex(&session),
                label: label.line,
            });
        }
        let facts = live
            .iter()
            .find(|f| f.pane == pane.id)
            .ok_or(Code::ProtocolError)?;
        if facts.control.state == ControlState::Other {
            others.push(OtherController {
                pane: super::hex(&pane.id),
                client_name: facts
                    .control
                    .client_name
                    .clone()
                    .ok_or(Code::ProtocolError)?,
            });
        }
    }
    let own_controls = snapshot.inventory.close.as_ref().and_then(|live| {
        confirmation
            .panes
            .iter()
            .map(|p| {
                live.controls
                    .iter()
                    .find(|c| c.pane == p.id && c.session == p.session && c.owner == actor)
                    .cloned()
            })
            .collect::<Option<Vec<_>>>()
    });
    if matches!(target, CloseTarget::Pane { .. } | CloseTarget::Tab { .. })
        && !confirmation.removes_workspace
        && idle
        && own_controls.is_some()
    {
        confirmation.own_controls = own_controls;
        return Ok(Review::Direct(Mutation::Close {
            target,
            confirmation,
        }));
    }
    let ends_sessions = sessions.len().try_into().unwrap_or(u32::MAX);
    let effects = CloseEffects {
        ends_sessions,
        sessions,
        controlled_by_other_clients: others,
        removes_tab: confirmation.removes_tab,
        removes_workspace: confirmation.removes_workspace,
        fallback: None,
    };
    Ok(Review::Confirm {
        confirmation,
        effects,
    })
}

/// Revalidate the reviewed exact membership before journal dispatch. The server fences it again.
pub fn confirmed(
    snapshot: &Snapshot,
    target: CloseTarget,
    confirmation: CloseConfirmation,
) -> Result<Mutation, Code> {
    if snapshot.inventory.close_confirmation(target).as_ref() != Some(&confirmation) {
        return Err(Code::Changed);
    }
    Ok(Mutation::Close {
        target,
        confirmation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{PaneControl, PaneSession, SessionStateLabel};
    use crate::scope::fixtures::{fixture, id};
    use spaceterm_server::hierarchy::{CloseInventory, ConfirmedControl, ConfirmedPane};

    fn live(snapshot: &mut Snapshot) -> Vec<PaneFacts> {
        let panes: Vec<_> = snapshot
            .inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|p| p.id)
            .collect();
        snapshot.inventory.close = Some(CloseInventory {
            boot: id(9),
            sessions: panes
                .iter()
                .map(|p| ConfirmedPane {
                    id: *p,
                    session: Some(*p),
                })
                .collect(),
            controls: panes
                .iter()
                .map(|p| ConfirmedControl {
                    pane: *p,
                    session: Some(*p),
                    owner: id(5),
                    epoch: 1,
                })
                .collect(),
        });
        panes
            .into_iter()
            .map(|pane| PaneFacts {
                pane,
                session: PaneSession {
                    id: Some(super::super::hex(&pane)),
                    state: SessionStateLabel::Running,
                },
                control: PaneControl {
                    state: ControlState::Own,
                    client_name: None,
                },
            })
            .collect()
    }

    #[test]
    fn idle_own_closes_bind_exact_control_grants() {
        let mut snapshot = fixture();
        let facts = live(&mut snapshot);
        let target = CloseTarget::Pane {
            workspace: id(1),
            tab: id(11),
            pane: id(21),
        };
        let Review::Direct(Mutation::Close { confirmation, .. }) =
            review(&snapshot, target, id(5), &facts).unwrap()
        else {
            panic!("expected own close")
        };
        assert_eq!(confirmation.own_controls.unwrap()[0].epoch, 1);
        snapshot.inventory.close.as_mut().unwrap().controls[0].owner = id(6);
        assert!(matches!(
            review(&snapshot, target, id(5), &facts).unwrap(),
            Review::Confirm { .. }
        ));
    }

    #[test]
    fn busy_stale_and_workspace_removing_closes_require_consent() {
        let mut snapshot = fixture();
        let facts = live(&mut snapshot);
        let target = CloseTarget::Tab {
            workspace: id(1),
            tab: id(11),
        };
        snapshot.inventory.workspaces[0].tabs[0].panes[0]
            .presentation
            .as_mut()
            .unwrap()
            .prompt_zone = PromptZone::CommandOutput;
        assert!(matches!(
            review(&snapshot, target, id(5), &facts).unwrap(),
            Review::Confirm { .. }
        ));
        snapshot.inventory.workspaces[0].tabs[0].panes[0]
            .presentation
            .as_mut()
            .unwrap()
            .freshness = Freshness::Stale;
        assert!(matches!(
            review(&snapshot, target, id(5), &facts).unwrap(),
            Review::Confirm { .. }
        ));
        assert!(
            matches!(review(&snapshot, CloseTarget::Tab { workspace: id(2), tab: id(13) }, id(5), &facts).unwrap(), Review::Confirm { effects, .. } if effects.removes_workspace)
        );
    }

    #[test]
    fn reviewed_membership_is_never_broadened_by_a_later_session() {
        let mut snapshot = fixture();
        let facts = live(&mut snapshot);
        let target = CloseTarget::Workspace { workspace: id(1) };
        let Review::Confirm { confirmation, .. } =
            review(&snapshot, target, id(5), &facts).unwrap()
        else {
            panic!("expected consent")
        };
        snapshot.inventory.close.as_mut().unwrap().sessions[0].session = Some(id(99));
        assert!(matches!(
            confirmed(&snapshot, target, confirmation),
            Err(Code::Changed)
        ));
    }
}
