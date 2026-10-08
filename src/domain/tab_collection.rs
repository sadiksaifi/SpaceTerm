use crate::close_confirmation::{CloseContinuation, CloseTabOutcome, HierarchyClose};
use thiserror::Error;

use super::{TabId, WorktreeId};

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum TabError {
    #[error("Tab {0} does not belong to this collection")]
    TabNotFound(TabId),
    #[error("Tab ID space is exhausted")]
    IdSpaceExhausted,
    #[error("Tab position {position} is outside a collection of {len} Tabs")]
    PositionOutOfRange { position: usize, len: usize },
}

struct TabEntry<T> {
    id: TabId,
    scope: Option<WorktreeId>,
    payload: T,
}

/// A step through the Tab order, as Next Tab, Previous Tab, and Move Tab take it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabStep {
    Previous,
    Next,
}

/// A Workspace's Tabs in one order, each tagged with the Worktree it belongs to.
///
/// Tabs of one Worktree stay adjacent in the order. Navigation, reordering, and positions act
/// within the Active Tab's Worktree, the Active Worktree; a Tab with no Worktree belongs to the
/// Workspace itself. Closing the final Tab of the whole collection still closes the Workspace.
pub(crate) struct TabCollection<T> {
    tabs: Vec<TabEntry<T>>,
    active_tab_id: TabId,
    root_tab_id: TabId,
    /// Tabs from least to most recently activated.
    activation_order: Vec<TabId>,
    next_tab_id: u64,
}

impl<T> TabCollection<T> {
    pub(crate) fn new(create_initial_payload: impl FnOnce(TabId) -> T) -> Self {
        let initial_tab_id = TabId::from_raw(1);
        Self {
            tabs: vec![TabEntry {
                id: initial_tab_id,
                scope: None,
                payload: create_initial_payload(initial_tab_id),
            }],
            active_tab_id: initial_tab_id,
            root_tab_id: initial_tab_id,
            activation_order: vec![initial_tab_id],
            next_tab_id: 2,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) const fn active_tab_id(&self) -> TabId {
        self.active_tab_id
    }

    pub(crate) fn active_tab(&self) -> &T {
        let Some(tab) = self.tab(self.active_tab_id) else {
            unreachable!("the Active Tab ID must always reference an owned Tab")
        };
        tab
    }

    #[allow(dead_code, reason = "Worktree activation uses it next")]
    pub(crate) const fn root_tab_id(&self) -> TabId {
        self.root_tab_id
    }

    pub(crate) fn root_tab(&self) -> &T {
        self.tab(self.root_tab_id)
            .expect("the Root Tab must belong to the Workspace")
    }

    pub(crate) fn tab(&self, tab_id: TabId) -> Option<&T> {
        self.tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| &tab.payload)
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = (TabId, &T)> {
        self.tabs.iter().map(|tab| (tab.id, &tab.payload))
    }

    /// The Active Tab's Worktree.
    pub(crate) fn active_scope(&self) -> Option<WorktreeId> {
        self.tabs[self.active_index()].scope
    }

    #[allow(dead_code, reason = "Worktree activation uses it next")]
    pub(crate) fn scope_of(&self, tab_id: TabId) -> Result<Option<WorktreeId>, TabError> {
        Ok(self.tabs[self.index_of(tab_id)?].scope)
    }

    #[allow(dead_code, reason = "Worktree activation uses it next")]
    /// The Tabs of one Worktree, in Tab order.
    pub(crate) fn tabs_in(&self, scope: Option<WorktreeId>) -> impl Iterator<Item = (TabId, &T)> {
        self.tabs
            .iter()
            .filter(move |tab| tab.scope == scope)
            .map(|tab| (tab.id, &tab.payload))
    }

    #[allow(dead_code, reason = "Worktree activation uses it next")]
    pub(crate) fn has_tabs_in(&self, scope: Option<WorktreeId>) -> bool {
        self.tabs.iter().any(|tab| tab.scope == scope)
    }

    #[allow(dead_code, reason = "Worktree activation uses it next")]
    /// The most recently activated Tab of one Worktree.
    pub(crate) fn most_recent_tab_in(&self, scope: Option<WorktreeId>) -> Option<TabId> {
        self.activation_order
            .iter()
            .rev()
            .copied()
            .find(|&tab_id| self.scope_of(tab_id) == Ok(scope))
    }

    /// Creates and activates a Tab in the Active Worktree.
    pub(crate) fn create_tab(
        &mut self,
        create_payload: impl FnOnce(TabId) -> T,
    ) -> Result<TabId, TabError> {
        self.create_tab_in(self.active_scope(), create_payload)
    }

    /// Creates and activates a Tab after the last Tab of `scope`, or at the end when it has none.
    pub(crate) fn create_tab_in(
        &mut self,
        scope: Option<WorktreeId>,
        create_payload: impl FnOnce(TabId) -> T,
    ) -> Result<TabId, TabError> {
        let (tab_id, next_tab_id) = self.next_tab_id()?;
        let payload = create_payload(tab_id);
        let position = self
            .tabs
            .iter()
            .rposition(|tab| tab.scope == scope)
            .map_or(self.tabs.len(), |index| index + 1);
        self.tabs.insert(
            position,
            TabEntry {
                id: tab_id,
                scope,
                payload,
            },
        );
        self.next_tab_id = next_tab_id;
        self.set_active(tab_id);
        Ok(tab_id)
    }

    #[allow(dead_code, reason = "Worktree activation uses it next")]
    /// Moves a Tab into another Worktree, after that Worktree's last Tab.
    pub(crate) fn set_tab_scope(
        &mut self,
        tab_id: TabId,
        scope: Option<WorktreeId>,
    ) -> Result<(), TabError> {
        let index = self.index_of(tab_id)?;
        if self.tabs[index].scope == scope {
            return Ok(());
        }
        let mut tab = self.tabs.remove(index);
        tab.scope = scope;
        let position = self
            .tabs
            .iter()
            .rposition(|tab| tab.scope == scope)
            .map_or(self.tabs.len(), |index| index + 1);
        self.tabs.insert(position, tab);
        Ok(())
    }

    pub(crate) fn activate_tab(&mut self, tab_id: TabId) -> Result<(), TabError> {
        self.index_of(tab_id)?;
        self.set_active(tab_id);
        Ok(())
    }

    /// Moves a Tab to `position` among its Worktree's Tabs and reports whether the order changed.
    ///
    /// The Active Tab and the Root Tab keep their identities; only presentation order changes.
    pub(crate) fn move_tab(&mut self, tab_id: TabId, position: usize) -> Result<bool, TabError> {
        let index = self.index_of(tab_id)?;
        let siblings = self.indices_in(self.tabs[index].scope);
        let Some(&target) = siblings.get(position) else {
            return Err(TabError::PositionOutOfRange {
                position,
                len: siblings.len(),
            });
        };
        if index == target {
            return Ok(false);
        }

        let tab = self.tabs.remove(index);
        self.tabs.insert(target, tab);
        Ok(true)
    }

    /// The Tab one step from the Active Tab, wrapping around the ends of the Active Worktree.
    pub(crate) fn neighbor_of_active_tab(&self, step: TabStep) -> TabId {
        let siblings = self.indices_in(self.active_scope());
        let len = siblings.len();
        let index = siblings
            .iter()
            .position(|&index| self.tabs[index].id == self.active_tab_id)
            .expect("the Active Tab must belong to the Active Worktree");
        let neighbor = match step {
            TabStep::Previous => (index + len - 1) % len,
            TabStep::Next => (index + 1) % len,
        };
        self.tabs[siblings[neighbor]].id
    }

    /// Moves the Active Tab one step within its Worktree and reports whether the order changed.
    ///
    /// The Active Tab stays put at either end of its Worktree instead of wrapping around.
    pub(crate) fn move_active_tab(&mut self, step: TabStep) -> bool {
        self.step_index(self.active_index(), step)
    }

    /// Moves a Tab one step within its Worktree and reports whether the order changed.
    ///
    /// The Tab stays put at either end of its Worktree instead of wrapping around.
    pub(crate) fn step_tab(&mut self, tab_id: TabId, step: TabStep) -> Result<bool, TabError> {
        let index = self.index_of(tab_id)?;
        Ok(self.step_index(index, step))
    }

    fn step_index(&mut self, index: usize, step: TabStep) -> bool {
        let scope = self.tabs[index].scope;
        let position = match step {
            TabStep::Previous => index.checked_sub(1),
            TabStep::Next => Some(index + 1).filter(|&position| position < self.tabs.len()),
        };
        let Some(position) = position.filter(|&position| self.tabs[position].scope == scope) else {
            return false;
        };
        self.tabs.swap(index, position);
        true
    }

    fn indices_in(&self, scope: Option<WorktreeId>) -> Vec<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.scope == scope)
            .map(|(index, _)| index)
            .collect()
    }

    fn index_of(&self, tab_id: TabId) -> Result<usize, TabError> {
        self.tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .ok_or(TabError::TabNotFound(tab_id))
    }

    fn active_index(&self) -> usize {
        self.index_of(self.active_tab_id)
            .expect("the Active Tab ID must always reference an owned Tab")
    }

    fn set_active(&mut self, tab_id: TabId) {
        self.activation_order.retain(|&id| id != tab_id);
        self.activation_order.push(tab_id);
        self.active_tab_id = tab_id;
    }

    pub(crate) fn close_tab(&mut self, tab_id: TabId) -> Result<CloseTabOutcome<T>, TabError> {
        let index = self.index_of(tab_id)?;
        if HierarchyClose::Tab.resolve(self.tabs.len()) == CloseContinuation::Parent {
            return Ok(CloseTabOutcome::CloseWorkspace {
                final_tab_id: tab_id,
            });
        }

        let closed_tab = self.tabs.remove(index);
        self.activation_order.retain(|&id| id != tab_id);
        let scope = closed_tab.scope;
        if self.root_tab_id == tab_id {
            self.root_tab_id = self
                .tabs
                .iter()
                .find(|tab| tab.scope == scope)
                .unwrap_or(&self.tabs[0])
                .id;
        }
        if self.active_tab_id == tab_id {
            let siblings = self.indices_in(scope);
            let fallback = match siblings.iter().position(|&sibling| sibling >= index) {
                Some(position) => self.tabs[siblings[position]].id,
                None => match siblings.last() {
                    Some(&sibling) => self.tabs[sibling].id,
                    None => *self
                        .activation_order
                        .last()
                        .expect("a remaining Tab must have been activated"),
                },
            };
            self.set_active(fallback);
        }

        Ok(CloseTabOutcome::TabClosed {
            closed_tab_id: closed_tab.id,
            active_tab_id: self.active_tab_id,
            payload: closed_tab.payload,
        })
    }

    fn next_tab_id(&self) -> Result<(TabId, u64), TabError> {
        let value = self.next_tab_id;
        let next = value.checked_add(1).ok_or(TabError::IdSpaceExhausted)?;
        Ok((TabId::from_raw(value), next))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn root_tab_should_ignore_focus_and_reordering_and_promote_on_close() {
        let mut tabs = TabCollection::new(|_| "initial");
        let second = tabs.create_tab(|_| "second").unwrap();
        tabs.create_tab(|_| "third").unwrap();
        assert!(tabs.move_tab(TabId::new(1), 1).unwrap());
        assert_eq!(tabs.root_tab(), &"initial");
        tabs.close_tab(TabId::new(1)).unwrap();
        assert_eq!(tabs.root_tab(), &"second");
        tabs.close_tab(second).unwrap();
        assert_eq!(tabs.root_tab(), &"third");
    }

    struct DropProbe {
        drops: Rc<Cell<usize>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.update(|drops| drops + 1);
        }
    }

    #[test]
    fn move_tab_should_reorder_without_changing_the_active_or_root_tab() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();
        let third = tabs.create_tab(|_| "third").unwrap();
        tabs.activate_tab(TabId::new(2)).unwrap();

        let moved_forward = tabs.move_tab(TabId::new(1), 2).unwrap();
        let moved_back = tabs.move_tab(third, 0).unwrap();
        let unchanged = tabs.move_tab(third, 0).unwrap();

        assert_eq!(
            (
                moved_forward,
                moved_back,
                unchanged,
                tabs.iter().map(|(_, tab)| *tab).collect::<Vec<_>>(),
                tabs.active_tab(),
                tabs.root_tab(),
            ),
            (
                true,
                true,
                false,
                vec!["third", "second", "first"],
                &"second",
                &"first",
            )
        );
    }

    fn order(tabs: &TabCollection<&'static str>) -> Vec<&'static str> {
        tabs.iter().map(|(_, tab)| *tab).collect()
    }

    #[test]
    fn neighbor_of_active_tab_should_wrap_around_the_tab_order() {
        let mut tabs = TabCollection::new(|_| "first");
        let second = tabs.create_tab(|_| "second").unwrap();
        let third = tabs.create_tab(|_| "third").unwrap();
        let first = TabId::new(1);

        let from_last = (
            tabs.neighbor_of_active_tab(TabStep::Next),
            tabs.neighbor_of_active_tab(TabStep::Previous),
        );
        tabs.activate_tab(first).unwrap();
        let from_first = (
            tabs.neighbor_of_active_tab(TabStep::Next),
            tabs.neighbor_of_active_tab(TabStep::Previous),
        );

        assert_eq!((from_last, from_first), ((first, second), (second, third)));
        let single = TabCollection::new(|_| "only");
        assert_eq!(single.neighbor_of_active_tab(TabStep::Next), first);
        assert_eq!(single.neighbor_of_active_tab(TabStep::Previous), first);
    }

    #[test]
    fn move_active_tab_should_step_within_the_tab_order_without_wrapping() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();
        let third = tabs.create_tab(|_| "third").unwrap();

        assert!(!tabs.move_active_tab(TabStep::Next));
        assert_eq!(order(&tabs), ["first", "second", "third"]);
        assert!(tabs.move_active_tab(TabStep::Previous));
        assert_eq!(order(&tabs), ["first", "third", "second"]);
        assert!(tabs.move_active_tab(TabStep::Previous));
        assert_eq!(order(&tabs), ["third", "first", "second"]);
        assert!(!tabs.move_active_tab(TabStep::Previous));
        assert_eq!(order(&tabs), ["third", "first", "second"]);
        assert!(tabs.move_active_tab(TabStep::Next));
        assert_eq!(order(&tabs), ["first", "third", "second"]);
        assert_eq!((tabs.active_tab_id(), tabs.root_tab()), (third, &"first"));
    }

    #[test]
    fn move_tab_should_reject_an_unknown_tab_or_position_without_mutation() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();

        let unknown = tabs.move_tab(TabId::new(99), 0);
        let beyond = tabs.move_tab(TabId::new(1), 2);

        assert_eq!(
            (
                unknown,
                beyond,
                tabs.iter().map(|(_, tab)| *tab).collect::<Vec<_>>(),
            ),
            (
                Err(TabError::TabNotFound(TabId::new(99))),
                Err(TabError::PositionOutOfRange {
                    position: 2,
                    len: 2
                }),
                vec!["first", "second"],
            )
        );
    }

    #[test]
    fn new_should_create_one_valid_active_tab() {
        let tabs = TabCollection::new(|_| "first");

        assert_eq!(
            (tabs.len(), tabs.active_tab_id(), tabs.active_tab()),
            (1, TabId::new(1), &"first")
        );
    }

    #[test]
    fn iter_should_preserve_tab_creation_order() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();
        tabs.create_tab(|_| "third").unwrap();

        let ordered_tabs = tabs.iter().collect::<Vec<_>>();

        assert_eq!(
            ordered_tabs,
            vec![
                (TabId::new(1), &"first"),
                (TabId::new(2), &"second"),
                (TabId::new(3), &"third"),
            ]
        );
    }

    #[test]
    fn create_tab_should_create_and_activate_the_new_tab() {
        let mut tabs = TabCollection::new(|_| "first");

        let created = tabs.create_tab(|_| "second").unwrap();

        assert_eq!(
            (created, tabs.len(), tabs.active_tab_id(), tabs.active_tab()),
            (TabId::new(2), 2, TabId::new(2), &"second")
        );
    }

    #[test]
    fn create_tab_should_reject_exhausted_ids_before_creating_its_payload() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.next_tab_id = u64::MAX;
        let creations = Cell::new(0);

        let result = tabs.create_tab(|_| {
            creations.update(|count| count + 1);
            "second"
        });

        assert_eq!(
            (result, creations.get(), tabs.len()),
            (Err(TabError::IdSpaceExhausted), 0, 1)
        );
    }

    #[test]
    fn activate_tab_should_select_an_owned_tab() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();

        tabs.activate_tab(TabId::new(1)).unwrap();

        assert_eq!(
            (tabs.active_tab_id(), tabs.active_tab()),
            (TabId::new(1), &"first")
        );
    }

    #[test]
    fn activate_tab_should_reject_an_unknown_id_without_changing_the_active_tab() {
        let mut tabs = TabCollection::new(|_| "first");

        let result = tabs.activate_tab(TabId::new(99));

        assert_eq!(
            (result, tabs.active_tab_id()),
            (Err(TabError::TabNotFound(TabId::new(99))), TabId::new(1))
        );
    }

    #[test]
    fn close_tab_should_preserve_the_active_tab_when_closing_an_inactive_tab() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();
        tabs.create_tab(|_| "third").unwrap();

        let outcome = tabs.close_tab(TabId::new(1)).unwrap();

        let CloseTabOutcome::TabClosed {
            closed_tab_id,
            active_tab_id,
            payload,
        } = outcome
        else {
            panic!("closing one of multiple Tabs must remove it")
        };
        assert_eq!(
            (closed_tab_id, active_tab_id, payload),
            (TabId::new(1), TabId::new(3), "first")
        );
    }

    #[test]
    fn close_tab_should_focus_the_next_tab_when_closing_the_active_middle_tab() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();
        tabs.create_tab(|_| "third").unwrap();
        tabs.activate_tab(TabId::new(2)).unwrap();

        let outcome = tabs.close_tab(TabId::new(2)).unwrap();

        let CloseTabOutcome::TabClosed { active_tab_id, .. } = outcome else {
            panic!("closing one of multiple Tabs must remove it")
        };
        assert_eq!(active_tab_id, TabId::new(3));
    }

    #[test]
    fn close_tab_should_focus_the_previous_tab_when_closing_the_active_last_tab() {
        let mut tabs = TabCollection::new(|_| "first");
        tabs.create_tab(|_| "second").unwrap();

        let outcome = tabs.close_tab(TabId::new(2)).unwrap();

        let CloseTabOutcome::TabClosed { active_tab_id, .. } = outcome else {
            panic!("closing one of multiple Tabs must remove it")
        };
        assert_eq!(active_tab_id, TabId::new(1));
    }

    #[test]
    fn close_tab_should_reject_an_unknown_id_without_mutation() {
        let mut tabs = TabCollection::new(|_| "first");

        let result = tabs.close_tab(TabId::new(99));

        assert_eq!(
            (
                result.err(),
                tabs.len(),
                tabs.active_tab_id(),
                tabs.active_tab()
            ),
            (
                Some(TabError::TabNotFound(TabId::new(99))),
                1,
                TabId::new(1),
                &"first"
            )
        );
    }

    #[test]
    fn closed_tab_ids_should_not_be_reused() {
        let mut tabs = TabCollection::new(|_| "first");
        let second = tabs.create_tab(|_| "second").unwrap();
        tabs.close_tab(second).unwrap();

        let third = tabs.create_tab(|_| "third").unwrap();

        assert_eq!(third, TabId::new(3));
    }

    #[test]
    fn close_tab_should_request_workspace_close_for_the_final_tab() {
        let mut tabs = TabCollection::new(|_| "first");

        let outcome = tabs.close_tab(TabId::new(1)).unwrap();

        let CloseTabOutcome::CloseWorkspace { final_tab_id } = outcome else {
            panic!("closing the final Tab must request its Workspace close")
        };
        assert_eq!(final_tab_id, TabId::new(1));
        assert_eq!(
            (tabs.len(), tabs.active_tab_id(), tabs.active_tab()),
            (1, TabId::new(1), &"first")
        );
    }

    #[test]
    fn close_tab_should_transfer_ownership_and_drop_each_payload_exactly_once() {
        let drops = Rc::new(Cell::new(0));
        let mut tabs = TabCollection::new(|_| DropProbe {
            drops: Rc::clone(&drops),
        });
        tabs.create_tab(|_| DropProbe {
            drops: Rc::clone(&drops),
        })
        .unwrap();

        let outcome = tabs.close_tab(TabId::new(2)).unwrap();
        assert_eq!(drops.get(), 0);

        drop(outcome);
        assert_eq!(drops.get(), 1);

        drop(tabs);
        assert_eq!(drops.get(), 2);
    }

    fn scoped(tabs: &TabCollection<&'static str>, scope: Option<WorktreeId>) -> Vec<&'static str> {
        tabs.tabs_in(scope).map(|(_, tab)| *tab).collect()
    }

    const MAIN: Option<WorktreeId> = Some(WorktreeId::new(1));
    const FEATURE: Option<WorktreeId> = Some(WorktreeId::new(2));

    /// main-1, main-2 in the main Worktree, then feature-1, feature-2 in another Worktree.
    fn two_worktrees() -> TabCollection<&'static str> {
        let mut tabs = TabCollection::new(|_| "main-1");
        tabs.set_tab_scope(TabId::new(1), MAIN).unwrap();
        tabs.create_tab(|_| "main-2").unwrap();
        tabs.create_tab_in(FEATURE, |_| "feature-1").unwrap();
        tabs.create_tab(|_| "feature-2").unwrap();
        tabs
    }

    #[test]
    fn new_tabs_should_join_the_active_worktree_after_its_last_tab() {
        let mut tabs = two_worktrees();
        tabs.activate_tab(TabId::new(1)).unwrap();

        let created = tabs.create_tab(|_| "main-3").unwrap();

        assert_eq!(
            (
                order(&tabs),
                scoped(&tabs, MAIN),
                scoped(&tabs, FEATURE),
                tabs.active_tab_id(),
                tabs.active_scope(),
            ),
            (
                vec!["main-1", "main-2", "main-3", "feature-1", "feature-2"],
                vec!["main-1", "main-2", "main-3"],
                vec!["feature-1", "feature-2"],
                created,
                MAIN,
            )
        );
    }

    #[test]
    fn tab_navigation_should_stay_within_the_active_worktree() {
        let mut tabs = two_worktrees();
        let feature_1 = TabId::new(3);
        let feature_2 = TabId::new(4);

        let wrapped = tabs.neighbor_of_active_tab(TabStep::Next);
        let previous = tabs.neighbor_of_active_tab(TabStep::Previous);
        let stepped_past_the_end = tabs.move_active_tab(TabStep::Next);
        let stepped_back = tabs.move_active_tab(TabStep::Previous);
        let stepped_past_the_start = tabs.move_active_tab(TabStep::Previous);

        assert_eq!(
            (
                wrapped,
                previous,
                stepped_past_the_end,
                stepped_back,
                stepped_past_the_start,
                scoped(&tabs, FEATURE),
                scoped(&tabs, MAIN),
            ),
            (
                feature_1,
                feature_1,
                false,
                true,
                false,
                vec!["feature-2", "feature-1"],
                vec!["main-1", "main-2"],
            )
        );
        assert_eq!(tabs.active_tab_id(), feature_2);
    }

    #[test]
    fn move_tab_should_take_a_position_within_the_tabs_worktree() {
        let mut tabs = two_worktrees();

        let moved = tabs.move_tab(TabId::new(3), 1).unwrap();
        let beyond = tabs.move_tab(TabId::new(3), 2);

        assert_eq!(
            (moved, beyond, scoped(&tabs, FEATURE), scoped(&tabs, MAIN)),
            (
                true,
                Err(TabError::PositionOutOfRange {
                    position: 2,
                    len: 2
                }),
                vec!["feature-2", "feature-1"],
                vec!["main-1", "main-2"],
            )
        );
    }

    #[test]
    fn closing_a_worktrees_last_tab_should_return_to_the_most_recent_tab_elsewhere() {
        let mut tabs = two_worktrees();
        tabs.activate_tab(TabId::new(1)).unwrap();
        tabs.activate_tab(TabId::new(4)).unwrap();
        tabs.close_tab(TabId::new(3)).unwrap();

        let outcome = tabs.close_tab(TabId::new(4)).unwrap();

        let CloseTabOutcome::TabClosed { active_tab_id, .. } = outcome else {
            panic!("closing one of multiple Tabs must remove it")
        };
        assert_eq!(
            (
                active_tab_id,
                tabs.active_scope(),
                tabs.has_tabs_in(FEATURE)
            ),
            (TabId::new(1), MAIN, false)
        );
    }

    #[test]
    fn closing_the_root_tab_should_promote_a_tab_in_its_worktree() {
        let mut tabs = TabCollection::new(|_| "feature-1");
        tabs.set_tab_scope(TabId::new(1), FEATURE).unwrap();
        tabs.create_tab_in(MAIN, |_| "main-1").unwrap();
        tabs.create_tab_in(FEATURE, |_| "feature-2").unwrap();

        tabs.close_tab(TabId::new(1)).unwrap();

        assert_eq!(tabs.root_tab(), &"feature-2");
    }

    #[test]
    fn the_final_tab_should_still_close_the_workspace_when_other_worktrees_are_empty() {
        let mut tabs = two_worktrees();
        for tab_id in [1, 2, 3] {
            tabs.close_tab(TabId::new(tab_id)).unwrap();
        }

        let outcome = tabs.close_tab(TabId::new(4)).unwrap();

        assert!(matches!(
            outcome,
            CloseTabOutcome::CloseWorkspace { final_tab_id } if final_tab_id == TabId::new(4)
        ));
    }

    #[test]
    fn most_recent_tab_in_should_name_the_last_activated_tab_of_a_worktree() {
        let mut tabs = two_worktrees();
        tabs.activate_tab(TabId::new(3)).unwrap();
        tabs.activate_tab(TabId::new(2)).unwrap();

        assert_eq!(
            (
                tabs.most_recent_tab_in(FEATURE),
                tabs.most_recent_tab_in(MAIN),
                tabs.most_recent_tab_in(None),
            ),
            (Some(TabId::new(3)), Some(TabId::new(2)), None)
        );
    }
}
