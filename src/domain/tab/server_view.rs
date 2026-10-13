//! A read-only adapter that presents one server Tab.
//!
//! It keeps one payload for each arranged Pane, keyed by the server-qualified Pane ID. Membership
//! and the Root Pane come from the Tab record. Layout, focus and zoom come from this client's
//! personal [`PaneArrangement`]. It never allocates a shared ID, chooses a Root, or deletes
//! membership: a Pane leaves only when a newer arrangement no longer holds it.

use std::collections::BTreeMap;

use spaceterm_client::view::{ArrangementEdit, PaneArrangement, ViewError, ZoomState};
use spaceterm_core::identity::{PaneId, TabId};
use spaceterm_core::organization::TabRecord;
use thiserror::Error;

/// Why a server-backed adapter refused a record, an arrangement or a payload.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ServerViewError {
    /// The inputs describe another Workspace or Tab than the adapter presents, or disagree.
    #[error("the record and the personal view describe different targets")]
    Mismatch,
    /// The target is not presented, so it holds no payload.
    #[error("the target is not presented")]
    NotPresented,
}

/// One server Tab's arranged Panes and the payload that presents each one.
pub(crate) struct ServerTab<T> {
    record: TabRecord,
    arrangement: PaneArrangement,
    payloads: BTreeMap<PaneId, T>,
}

impl<T> ServerTab<T> {
    /// Presents `record` with `arrangement`. Payloads arrive later through [`Self::insert`].
    pub(crate) fn new(
        record: TabRecord,
        arrangement: PaneArrangement,
    ) -> Result<Self, ServerViewError> {
        if arrangement.tab() != record.id {
            return Err(ServerViewError::Mismatch);
        }
        Ok(Self {
            record,
            arrangement,
            payloads: BTreeMap::new(),
        })
    }

    /// Applies a newer record and arrangement of the same Tab. It returns the payloads of Panes
    /// the arrangement no longer holds, in Pane order. A refused input changes nothing.
    pub(crate) fn apply(
        &mut self,
        record: TabRecord,
        arrangement: PaneArrangement,
    ) -> Result<Vec<(PaneId, T)>, ServerViewError> {
        if record.id != self.record.id || arrangement.tab() != record.id {
            return Err(ServerViewError::Mismatch);
        }
        let released: Vec<PaneId> = self
            .payloads
            .keys()
            .copied()
            .filter(|pane| !arrangement.contains_pane(*pane))
            .collect();
        self.record = record;
        self.arrangement = arrangement;
        Ok(released
            .into_iter()
            .filter_map(|pane| self.payloads.remove(&pane).map(|payload| (pane, payload)))
            .collect())
    }

    pub(crate) const fn id(&self) -> TabId {
        self.record.id
    }

    pub(crate) const fn record(&self) -> &TabRecord {
        &self.record
    }

    /// The Root Pane the server records. Personal layout never chooses it.
    pub(crate) const fn root_pane(&self) -> PaneId {
        self.record.root_pane
    }

    pub(crate) const fn arrangement(&self) -> &PaneArrangement {
        &self.arrangement
    }

    /// The Focused Pane. An arrangement always holds at least one Pane.
    pub(crate) fn focused_pane(&self) -> PaneId {
        self.arrangement
            .focused_pane()
            .unwrap_or(self.record.root_pane)
    }

    pub(crate) const fn zoom(&self) -> ZoomState {
        self.arrangement.zoom()
    }

    /// Applies one personal edit and reports whether the arrangement changed. A refused edit
    /// changes nothing. Only the arrangement changes: membership and Roots stay as recorded.
    pub(crate) fn arrange(&mut self, edit: ArrangementEdit) -> Result<bool, ViewError> {
        let mut next = self.arrangement.clone();
        match edit {
            ArrangementEdit::Focus(pane) => {
                next.focus_pane(pane)?;
            }
            ArrangementEdit::FocusDirection(direction) => {
                next.focus_in_direction(direction)?;
            }
            ArrangementEdit::FocusPrevious => {
                next.focus_previous();
            }
            ArrangementEdit::FocusNext => {
                next.focus_next();
            }
            ArrangementEdit::ToggleZoom => {
                next.toggle_zoom();
            }
            ArrangementEdit::Move {
                pane,
                target,
                edge,
                target_size,
                metrics,
            } => {
                next.move_pane(pane, target, edge, target_size, metrics)?;
            }
            ArrangementEdit::SetSplitAxis {
                split,
                axis,
                available,
                metrics,
            } => {
                next.set_split_axis(split, axis, available, metrics)?;
            }
            ArrangementEdit::Resize {
                split,
                available,
                metrics,
                ratio,
            } => {
                next.resize_split(split, available, metrics, ratio)?;
            }
        }
        if next == self.arrangement {
            return Ok(false);
        }
        self.arrangement = next;
        Ok(true)
    }

    /// Restores the already-resolved target of history navigation without recording a new focus.
    /// The portable focus cycle visits every Pane. The Pane count bounds replay after membership
    /// changes, and a missing target or exhausted layout revision leaves the arrangement alone.
    pub(crate) fn focus_in_cycle(&mut self, pane: PaneId) -> bool {
        if !self.arrangement.contains_pane(pane) || self.focused_pane() == pane {
            return false;
        }
        let mut next = self.arrangement.clone();
        for _ in 0..next.pane_count() {
            if next.focus_next().is_none() {
                return false;
            }
            if next.focused_pane() == Some(pane) {
                self.arrangement = next;
                return true;
            }
        }
        false
    }

    pub(crate) fn pane(&self, pane: PaneId) -> Option<&T> {
        self.payloads.get(&pane)
    }

    /// Arranged Panes with their payloads, in visual order. A Pane without a payload is skipped.
    pub(crate) fn panes(&self) -> impl Iterator<Item = (PaneId, &T)> {
        self.arrangement
            .pane_ids()
            .into_iter()
            .filter_map(|pane| self.payloads.get(&pane).map(|payload| (pane, payload)))
    }

    /// Arranged Panes that have no payload yet, in visual order.
    pub(crate) fn missing(&self) -> Vec<PaneId> {
        self.arrangement
            .pane_ids()
            .into_iter()
            .filter(|pane| !self.payloads.contains_key(pane))
            .collect()
    }

    /// Presents an arranged Pane with `payload`. It returns the payload it replaces, if any.
    pub(crate) fn insert(
        &mut self,
        pane: PaneId,
        payload: T,
    ) -> Result<Option<T>, ServerViewError> {
        if !self.arrangement.contains_pane(pane) {
            return Err(ServerViewError::NotPresented);
        }
        Ok(self.payloads.insert(pane, payload))
    }
}

#[cfg(test)]
mod tests {
    use spaceterm_client::view::{
        ArrangementEdit, ArrangementMetrics, ClientViewState, FocusDirection, PaneEdge, PaneSize,
        ZoomState,
    };
    use spaceterm_core::organization::{SplitAxis, SplitEdge};

    use super::{ServerTab, ServerViewError};
    use crate::domain::tab_collection::server_view::testing::{A, B, Org, Replica, pane, tab};

    fn arranged(replica: &Replica, tab_local: u64) -> ServerTab<&'static str> {
        let record = replica.tab_record(tab_local);
        let arrangement = replica.arrangement(1, tab_local);
        ServerTab::new(record, arrangement).unwrap()
    }

    fn split_replica() -> Replica {
        let mut org = Org::new(A);
        org.add_workspace(1, 1, 1)
            .split(1, 2, 1, SplitAxis::Horizontal, SplitEdge::After);
        Replica::attached(org, 1)
    }

    #[test]
    fn desktop_server_tab_reads_its_root_from_the_record_and_releases_only_unarranged_panes() {
        let mut replica = split_replica();
        let mut view = arranged(&replica, 1);
        assert_eq!(view.missing(), vec![pane(A, 1), pane(A, 2)]);
        assert_eq!(view.insert(pane(A, 1), "one"), Ok(None));
        assert_eq!(view.insert(pane(A, 2), "two"), Ok(None));
        assert_eq!(
            view.insert(pane(A, 9), "unknown"),
            Err(ServerViewError::NotPresented)
        );
        assert_eq!(view.root_pane(), pane(A, 1));

        // The server deletes the Root Pane and names Pane 2 the Root. The adapter follows the
        // record rather than choosing a successor itself.
        replica
            .org
            .relink(2, spaceterm_core::organization::PaneCreation::Initial)
            .delete_pane(1)
            .set_root_pane(1, 2)
            .publish();
        replica.reconcile_ready(&[]);
        let released = view
            .apply(replica.tab_record(1), replica.arrangement(1, 1))
            .unwrap();
        assert_eq!(released, vec![(pane(A, 1), "one")]);
        assert_eq!(view.root_pane(), pane(A, 2));
        assert_eq!(view.panes().collect::<Vec<_>>(), vec![(pane(A, 2), &"two")]);
        assert!(view.missing().is_empty());
    }

    #[test]
    fn desktop_server_tab_refuses_another_tab_or_server() {
        let mut org = Org::new(A);
        org.add_workspace(1, 1, 1).add_tab(1, 2, 2, None);
        let replica = Replica::attached(org, 1);
        let mut view = arranged(&replica, 1);
        assert_eq!(
            view.apply(replica.tab_record(2), replica.arrangement(1, 2))
                .err(),
            Some(ServerViewError::Mismatch)
        );
        assert_eq!(
            ServerTab::<()>::new(replica.tab_record(1), replica.arrangement(1, 2)).err(),
            Some(ServerViewError::Mismatch)
        );
        let mut other = Org::new(B);
        other.add_workspace(1, 1, 1);
        let other = Replica::attached(other, 1);
        assert_eq!(view.id(), tab(A, 1));
        assert_eq!(
            view.apply(other.tab_record(1), other.arrangement(1, 1))
                .err(),
            Some(ServerViewError::Mismatch)
        );
        assert_eq!(view.id(), tab(A, 1));
    }

    #[test]
    fn desktop_server_tab_personal_edits_match_the_client_view() {
        let mut replica = split_replica();
        let mut view = arranged(&replica, 1);
        let metrics = ArrangementMetrics {
            minimum_leaf: PaneSize::new(10.0, 10.0).unwrap(),
            divider: 1.0,
        };
        let space = PaneSize::new(1000.0, 1000.0).unwrap();
        let split = match view.arrangement().tree().node() {
            spaceterm_client::view::PaneNodeRef::Split { id, .. } => id,
            spaceterm_client::view::PaneNodeRef::Pane(_) => unreachable!("two Panes split"),
        };
        let edits = [
            ArrangementEdit::FocusDirection(FocusDirection::Right),
            ArrangementEdit::ToggleZoom,
            ArrangementEdit::ToggleZoom,
            ArrangementEdit::Resize {
                split,
                available: space,
                metrics,
                ratio: 0.25,
            },
            ArrangementEdit::Move {
                pane: pane(A, 1),
                target: pane(A, 2),
                edge: PaneEdge::Bottom,
                target_size: space,
                metrics,
            },
        ];
        let mut personal: ClientViewState = replica.personal.clone();
        for edit in edits {
            let changed = view.arrange(edit).unwrap();
            let update = personal
                .arrange(replica.workspace_id(1), tab(A, 1), edit)
                .unwrap();
            assert_eq!(changed, !update.is_empty());
            assert_eq!(
                Some(view.arrangement()),
                personal
                    .workspace(replica.workspace_id(1))
                    .and_then(|workspace| workspace.arrangement(tab(A, 1)))
            );
        }
        assert_eq!(view.zoom(), ZoomState::Restored);
        assert_eq!(view.focused_pane(), pane(A, 1));
        // A refused edit leaves the arrangement as it was.
        let before = view.arrangement().clone();
        assert!(view.arrange(ArrangementEdit::Focus(pane(B, 1))).is_err());
        assert_eq!(view.arrangement(), &before);
        replica.reconcile_ready(&[]);
        assert_eq!(view.root_pane(), pane(A, 1));
    }
}
