//! This client's one Workspace order across every presented server.
//!
//! The sidebar, the Workspace Switcher and the position shortcuts all read the Workspace
//! collection's order, so this module keeps that collection in the order the person arranged.
//! The order outlives server connections: a Workspace whose server is absent, or that this client
//! detached, keeps its place until its server's complete inventory says it was deleted. A
//! server reconnecting later therefore returns each Workspace to where the person left it,
//! whichever server arrives first. Each reference names the authenticated presentation that shows
//! the Workspace, so two saved routes to one server keep their own places.

use std::collections::BTreeSet;

use gpui::{Context, Task, Window};
use spaceterm_server::workspace_order::{WorkspaceOrder, WorkspaceReference};

use super::WorkspaceManager;
use crate::domain::WorkspaceId;
use crate::ui::shared_workspaces::ArrangementUnavailable;
use crate::ui::workspace_order_state::RetainedWorkspaceOrder;

/// The retained order and the storage that keeps it across launches.
#[derive(Default)]
pub(super) struct DisplayOrder {
    order: WorkspaceOrder,
    retained: Option<RetainedWorkspaceOrder>,
    /// Whether the saved order was read, or reading ended without one. Until then nothing is
    /// recorded, so Workspaces arriving early never replace the saved order.
    ready: bool,
    /// Whether the person arranged Workspaces while the saved order was being read. Their
    /// arrangement is newer than the saved one, so it wins once reading ends.
    reordered_while_reading: bool,
    /// The presented Workspaces whose creation the server hadn't confirmed at the last
    /// arrangement. They have no reference yet, so the order can't hold their places.
    pending_creations: BTreeSet<WorkspaceId>,
    /// Whether this window already said the order couldn't be read or saved.
    error_reported: bool,
    _read: Option<Task<()>>,
}

impl DisplayOrder {
    /// Adopts the order reading found, or keeps the current one when there is none. An
    /// arrangement the person made while reading is recorded over it, so Workspaces it doesn't
    /// present keep their saved places. Returns whether the presented Workspaces should still
    /// take their saved places.
    fn adopt_saved(
        &mut self,
        saved: Option<WorkspaceOrder>,
        presented: &[WorkspaceReference],
    ) -> bool {
        if let Some(saved) = saved {
            self.order = saved;
        }
        self.ready = true;
        if std::mem::take(&mut self.reordered_while_reading) {
            // A recording the order refuses leaves the saved order as it was.
            let _ = self.order.record(presented);
            return false;
        }
        true
    }
}

impl WorkspaceManager {
    /// Reads the saved order off the event thread, then arranges the presented Workspaces by it.
    /// A window without retained storage keeps its order for its own life.
    pub(super) fn follow_display_order(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.launch.workspace_order_connection() else {
            self.display_order.ready = true;
            return;
        };
        let retained = RetainedWorkspaceOrder::new(server);
        let read = retained.read();
        self.display_order.retained = Some(retained);
        self.display_order._read = Some(cx.spawn_in(window, async move |manager, cx| {
            read.await;
            let _ = manager.update_in(cx, |manager, window, cx| {
                // An order SpaceTerm couldn't read stays on disk untouched, and the window says
                // so. It still arranges its Workspaces for its own life.
                let saved = manager
                    .display_order
                    .retained
                    .as_ref()
                    .and_then(RetainedWorkspaceOrder::loaded)
                    .and_then(Result::ok);
                let previous = saved
                    .clone()
                    .unwrap_or_else(|| manager.display_order.order.clone());
                let presented: Vec<_> = manager
                    .presented_references()
                    .into_iter()
                    .map(|(_, reference)| reference)
                    .collect();
                let restores = manager.display_order.adopt_saved(saved, &presented);
                // Inventories that arrived while reading are complete, so they prune now.
                for key in manager.shared.keys() {
                    manager.prune_display_order(key);
                }
                if restores {
                    manager.restore_display_order();
                }
                manager.save_display_order(&previous);
                manager.report_client_view_error(window, cx);
                cx.notify();
            });
        }));
    }

    /// Writes the latest order before SpaceTerm quits.
    pub(super) fn flush_display_order(&mut self) {
        self.display_order.retained = None;
    }

    /// Why the retained order couldn't be read or saved, once per window.
    pub(super) fn take_display_order_error(&mut self) -> Option<ArrangementUnavailable> {
        let display_order = &mut self.display_order;
        if display_order.error_reported {
            return None;
        }
        let retained = display_order.retained.as_ref()?;
        let notice = if let Some(error) = retained.read_error() {
            ArrangementUnavailable::reading(error)
        } else if retained.error().is_some() {
            ArrangementUnavailable::Write
        } else {
            return None;
        };
        display_order.error_reported = true;
        Some(notice)
    }

    /// Applies the complete inventory the server at `key` published last: Workspaces it no
    /// longer has leave the order, and the presented Workspaces, including any it just added,
    /// take their retained places. A Workspace an unanswered edit hides from presentation is
    /// still in that inventory, so it keeps its place.
    pub(super) fn reconcile_display_order(&mut self, key: super::ServerKey) {
        if !self.display_order.ready {
            return;
        }
        let previous = self.display_order.order.clone();
        self.prune_display_order(key);
        self.restore_display_order();
        self.save_display_order(&previous);
    }

    /// Forgets the Workspaces the server behind `key` no longer has, by the last complete inventory
    /// that binding read. Other bindings' Workspaces keep their places.
    fn prune_display_order(&mut self, key: super::ServerKey) {
        if let Some(shared) = self.shared.get(key)
            && let Some(owner) = shared.owner()
            && let Some(inventory) = shared.latest_inventory()
        {
            let surviving: Vec<_> = inventory
                .workspaces
                .iter()
                .map(|workspace| workspace.id)
                .collect();
            self.display_order.order.reconcile_owner(owner, &surviving);
        }
    }

    /// Records the order the person just arranged. Absent Workspaces keep their places. Before
    /// the saved order is read, the arrangement waits for it and then wins over it.
    pub(super) fn record_display_order(&mut self) {
        self.note_pending_creations();
        if !self.display_order.ready {
            self.display_order.reordered_while_reading = true;
            return;
        }
        let previous = self.display_order.order.clone();
        let presented: Vec<_> = self
            .presented_references()
            .into_iter()
            .map(|(_, reference)| reference)
            .collect();
        // A recording the order refuses leaves the retained order as it was.
        let _ = self.display_order.order.record(&presented);
        self.save_display_order(&previous);
    }

    /// Moves the presented Workspaces into their retained order, then records it, so a Workspace
    /// seen for the first time joins the end. A Workspace with no reference yet keeps its
    /// position, and the referenced ones share the positions they already hold. A creation the
    /// server just confirmed also keeps its position, which may be where the person moved it
    /// while it waited.
    fn restore_display_order(&mut self) {
        let restorable: Vec<_> = self
            .presented_references()
            .into_iter()
            .filter(|(workspace_id, _)| {
                !self.display_order.pending_creations.contains(workspace_id)
            })
            .collect();
        let references: Vec<_> = restorable.iter().map(|(_, reference)| *reference).collect();
        let restored = self.display_order.order.restore(&references);
        let local = |reference: &WorkspaceReference| {
            restorable
                .iter()
                .find(|(_, candidate)| candidate == reference)
                .map(|(workspace_id, _)| *workspace_id)
        };
        let mut arranged = restored.iter().filter_map(local);
        let target: Vec<WorkspaceId> = self
            .workspaces
            .iter()
            .map(|workspace| {
                if restorable
                    .iter()
                    .any(|(workspace_id, _)| *workspace_id == workspace.id())
                {
                    arranged.next().unwrap_or(workspace.id())
                } else {
                    workspace.id()
                }
            })
            .collect();
        for (position, workspace_id) in target.into_iter().enumerate() {
            if let Err(error) = self.workspaces.move_workspace(workspace_id, position) {
                Self::report_workspace_error("arrange", error);
            }
        }
        let presented: Vec<_> = self
            .presented_references()
            .into_iter()
            .map(|(_, reference)| reference)
            .collect();
        // A recording the order refuses leaves the retained order as it was.
        let _ = self.display_order.order.record(&presented);
        self.note_pending_creations();
    }

    /// Notes which presented Workspaces still wait for their server to confirm their creation.
    fn note_pending_creations(&mut self) {
        let pending = self
            .workspaces
            .iter()
            .map(|workspace| workspace.id())
            .filter(|workspace_id| {
                self.shared
                    .owning(*workspace_id)
                    .is_some_and(|shared| shared.is_provisional(*workspace_id))
            })
            .collect();
        self.display_order.pending_creations = pending;
    }

    /// Saves the order when it differs from `previous`, including a change pruning alone made.
    fn save_display_order(&self, previous: &WorkspaceOrder) {
        if self.display_order.order != *previous
            && let Some(retained) = &self.display_order.retained
        {
            retained.save(self.display_order.order.clone());
        }
    }

    /// Each presented Workspace a server has published, with its reference, in Workspace order.
    fn presented_references(&self) -> Vec<(WorkspaceId, WorkspaceReference)> {
        self.workspaces
            .iter()
            .filter_map(|workspace| {
                let reference = self
                    .shared
                    .owning(workspace.id())?
                    .workspace_reference(workspace.id())?;
                Some((workspace.id(), reference))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(owner: u8, workspace: u8) -> WorkspaceReference {
        WorkspaceReference {
            owner: spaceterm_server::workspace_order::OwnerId::from_bytes([owner; 32]),
            workspace: [workspace; 32],
        }
    }

    /// A reorder made while a slow read was still running is newer than the saved order, so it
    /// wins once reading ends. A Workspace it doesn't present keeps its saved place.
    #[test]
    fn a_reorder_during_the_read_should_win_over_the_saved_order() {
        let (here, far, absent) = (reference(1, 1), reference(2, 1), reference(1, 2));
        let mut saved = WorkspaceOrder::default();
        saved.record(&[absent, here, far]).unwrap();
        let mut display_order = DisplayOrder {
            reordered_while_reading: true,
            ..DisplayOrder::default()
        };

        let restores = display_order.adopt_saved(Some(saved), &[far, here]);

        assert!(!restores, "the person's arrangement stays as they left it");
        assert_eq!(
            display_order.order.restore(&[here, far, absent]),
            [absent, far, here]
        );
    }

    /// Without a reorder during the read, the presented Workspaces take their saved places.
    #[test]
    fn without_a_reorder_the_saved_order_should_arrange_the_workspaces() {
        let (here, far) = (reference(1, 1), reference(2, 1));
        let mut saved = WorkspaceOrder::default();
        saved.record(&[far, here]).unwrap();
        let mut display_order = DisplayOrder::default();

        let restores = display_order.adopt_saved(Some(saved.clone()), &[here, far]);

        assert!(restores);
        assert_eq!(display_order.order, saved);
    }
}
