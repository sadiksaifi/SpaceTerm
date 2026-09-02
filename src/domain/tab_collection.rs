use thiserror::Error;

use super::TabId;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum TabError {
    #[error("Tab {0} does not belong to this collection")]
    TabNotFound(TabId),
    #[error("Tab ID space is exhausted")]
    IdSpaceExhausted,
}

pub(crate) enum CloseTabOutcome<T> {
    TabClosed {
        closed_tab_id: TabId,
        active_tab_id: TabId,
        payload: T,
    },
    CloseWorkspace {
        final_tab_id: TabId,
    },
}

struct TabEntry<T> {
    id: TabId,
    payload: T,
}

pub(crate) struct TabCollection<T> {
    tabs: Vec<TabEntry<T>>,
    active_tab_id: TabId,
    next_tab_id: u64,
}

impl<T> TabCollection<T> {
    pub(crate) fn new(create_initial_payload: impl FnOnce(TabId) -> T) -> Self {
        let initial_tab_id = TabId::from_raw(1);
        Self {
            tabs: vec![TabEntry {
                id: initial_tab_id,
                payload: create_initial_payload(initial_tab_id),
            }],
            active_tab_id: initial_tab_id,
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

    pub(crate) fn tab(&self, tab_id: TabId) -> Option<&T> {
        self.tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| &tab.payload)
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = (TabId, &T)> {
        self.tabs.iter().map(|tab| (tab.id, &tab.payload))
    }

    pub(crate) fn create_tab(
        &mut self,
        create_payload: impl FnOnce(TabId) -> T,
    ) -> Result<TabId, TabError> {
        let (tab_id, next_tab_id) = self.next_tab_id()?;
        let payload = create_payload(tab_id);
        self.tabs.push(TabEntry {
            id: tab_id,
            payload,
        });
        self.active_tab_id = tab_id;
        self.next_tab_id = next_tab_id;
        Ok(tab_id)
    }

    pub(crate) fn activate_tab(&mut self, tab_id: TabId) -> Result<(), TabError> {
        if !self.tabs.iter().any(|tab| tab.id == tab_id) {
            return Err(TabError::TabNotFound(tab_id));
        }

        self.active_tab_id = tab_id;
        Ok(())
    }

    pub(crate) fn close_tab(&mut self, tab_id: TabId) -> Result<CloseTabOutcome<T>, TabError> {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return Err(TabError::TabNotFound(tab_id));
        };
        if self.tabs.len() == 1 {
            return Ok(CloseTabOutcome::CloseWorkspace {
                final_tab_id: tab_id,
            });
        }

        let closed_tab = self.tabs.remove(index);
        if self.active_tab_id == tab_id {
            let fallback_index = index.min(self.tabs.len() - 1);
            self.active_tab_id = self.tabs[fallback_index].id;
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

    struct DropProbe {
        drops: Rc<Cell<usize>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.update(|drops| drops + 1);
        }
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
}
