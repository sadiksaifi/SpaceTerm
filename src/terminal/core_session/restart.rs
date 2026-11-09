//! A Core restart's bounded wait and shared progress wording.
use spaceterm_core::update::facts::{Operation, UpdateFacts};
use std::time::Duration;

pub(crate) type PaneRestartCauses =
    std::sync::Arc<std::sync::Mutex<Vec<spaceterm_core::client_view::PaneRestart>>>;

pub(crate) const RECONNECT_LIMIT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoreRestart {
    Updating,
    Restarting,
}

impl CoreRestart {
    pub(crate) fn of(facts: &UpdateFacts) -> Option<Self> {
        match facts.operation {
            Some(Operation::Restarting { .. }) => Some(Self::Updating),
            Some(Operation::RestartingSameVersion) => Some(Self::Restarting),
            _ => None,
        }
    }

    pub(crate) fn label(self, machine: &str) -> String {
        match self {
            Self::Updating => format!("Updating {machine}…"),
            Self::Restarting => format!("Restarting {machine}…"),
        }
    }
}

/// Retains the cause only until a new Terminal Session or another Core boot replaces it.
/// A target announced before a restart is never proof that a version changed.
pub(crate) fn observe_panes(
    retained: &mut Vec<spaceterm_core::client_view::PaneRestart>,
    inventory: &spaceterm_core::hierarchy::Inventory,
    running: &spaceterm_core::update::facts::Version,
) {
    use spaceterm_core::client_view::PaneRestart;
    let Some(close) = &inventory.close else {
        return;
    };
    let panes = inventory
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| &tab.panes)
        .collect::<Vec<_>>();
    retained.retain(|saved| panes.iter().any(|pane| pane.id == saved.pane));
    for pane in panes {
        let has_session = pane.presentation.is_some()
            || close
                .sessions
                .iter()
                .any(|confirmed| confirmed.id == pane.id && confirmed.session.is_some());
        match retained.iter_mut().find(|saved| saved.pane == pane.id) {
            Some(saved) => {
                if saved.boot != close.boot {
                    saved.updated = (saved.running != *running).then(|| running.clone());
                    saved.boot = close.boot;
                    saved.running = running.clone();
                }
                if has_session {
                    saved.updated = None;
                }
            }
            None => retained.push(PaneRestart {
                pane: pane.id,
                boot: close.boot,
                running: running.clone(),
                updated: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::workspaces::tests::{inventory, pane, tab};
    use spaceterm_core::{hierarchy::Axis, update::facts::Version};

    #[test]
    fn a_panes_update_cause_survives_relaunch_and_ends_with_a_new_session() {
        let mut inventory = inventory(
            1,
            vec![tab([2; 32], vec![pane([3; 32], None, Axis::Right)])],
        );
        let first = Version::parse("0.11.1").unwrap();
        let second = Version::parse("0.11.2").unwrap();
        let mut saved = Vec::new();
        observe_panes(&mut saved, &inventory, &first);
        inventory.close.as_mut().unwrap().boot = [10; 32];
        observe_panes(&mut saved, &inventory, &second);
        let serialized = serde_json::to_vec(&saved).unwrap();
        let mut reopened = serde_json::from_slice(&serialized).unwrap();
        observe_panes(&mut reopened, &inventory, &second);
        assert_eq!(reopened[0].updated.as_ref(), Some(&second));
        inventory.close.as_mut().unwrap().sessions[0].session = Some([5; 32]);
        observe_panes(&mut reopened, &inventory, &second);
        assert_eq!(reopened[0].updated, None);
        inventory.close.as_mut().unwrap().sessions[0].session = None;
        inventory.close.as_mut().unwrap().boot = [11; 32];
        observe_panes(&mut reopened, &inventory, &second);
        assert_eq!(reopened[0].updated, None);
    }

    #[test]
    fn a_restart_returning_to_the_same_version_replaces_an_older_update_cause() {
        let mut inventory = inventory(
            1,
            vec![tab([2; 32], vec![pane([3; 32], None, Axis::Right)])],
        );
        let first = Version::parse("0.11.1").unwrap();
        let second = Version::parse("0.11.2").unwrap();
        let mut saved = Vec::new();
        observe_panes(&mut saved, &inventory, &first);
        inventory.close.as_mut().unwrap().boot = [10; 32];
        observe_panes(&mut saved, &inventory, &second);
        assert_eq!(saved[0].updated.as_ref(), Some(&second));
        inventory.close.as_mut().unwrap().boot = [11; 32];
        observe_panes(&mut saved, &inventory, &second);
        assert_eq!(saved[0].updated, None);
    }
}
