use thiserror::Error;

use super::WindowId;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum WindowError {
    #[error("Window {0} does not belong to this collection")]
    WindowNotFound(WindowId),
    #[error("Window ID space is exhausted")]
    IdSpaceExhausted,
}

pub(crate) enum CloseWindowOutcome<T> {
    WindowClosed {
        closed_window_id: WindowId,
        active_window_id: WindowId,
        payload: T,
    },
    CloseOperatingSystemWindow,
}

struct WindowEntry<T> {
    id: WindowId,
    payload: T,
}

pub(crate) struct WindowCollection<T> {
    windows: Vec<WindowEntry<T>>,
    active_window_id: WindowId,
    next_window_id: u64,
}

impl<T> WindowCollection<T> {
    pub(crate) fn new(create_initial_payload: impl FnOnce(WindowId) -> T) -> Self {
        let initial_window_id = WindowId::from_raw(1);
        Self {
            windows: vec![WindowEntry {
                id: initial_window_id,
                payload: create_initial_payload(initial_window_id),
            }],
            active_window_id: initial_window_id,
            next_window_id: 2,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.windows.len()
    }

    pub(crate) const fn active_window_id(&self) -> WindowId {
        self.active_window_id
    }

    pub(crate) fn active_window(&self) -> &T {
        let Some(window) = self.window(self.active_window_id) else {
            unreachable!("the Active Window ID must always reference an owned Window")
        };
        window
    }

    pub(crate) fn window(&self, window_id: WindowId) -> Option<&T> {
        self.windows
            .iter()
            .find(|window| window.id == window_id)
            .map(|window| &window.payload)
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = (WindowId, &T)> {
        self.windows
            .iter()
            .map(|window| (window.id, &window.payload))
    }

    pub(crate) fn create_window(
        &mut self,
        create_payload: impl FnOnce(WindowId) -> T,
    ) -> Result<WindowId, WindowError> {
        let (window_id, next_window_id) = self.next_window_id()?;
        let payload = create_payload(window_id);
        self.windows.push(WindowEntry {
            id: window_id,
            payload,
        });
        self.active_window_id = window_id;
        self.next_window_id = next_window_id;
        Ok(window_id)
    }

    pub(crate) fn activate_window(&mut self, window_id: WindowId) -> Result<(), WindowError> {
        if !self.windows.iter().any(|window| window.id == window_id) {
            return Err(WindowError::WindowNotFound(window_id));
        }

        self.active_window_id = window_id;
        Ok(())
    }

    pub(crate) fn close_window(
        &mut self,
        window_id: WindowId,
    ) -> Result<CloseWindowOutcome<T>, WindowError> {
        let Some(index) = self
            .windows
            .iter()
            .position(|window| window.id == window_id)
        else {
            return Err(WindowError::WindowNotFound(window_id));
        };
        if self.windows.len() == 1 {
            return Ok(CloseWindowOutcome::CloseOperatingSystemWindow);
        }

        let closed_window = self.windows.remove(index);
        if self.active_window_id == window_id {
            let fallback_index = index.min(self.windows.len() - 1);
            self.active_window_id = self.windows[fallback_index].id;
        }

        Ok(CloseWindowOutcome::WindowClosed {
            closed_window_id: closed_window.id,
            active_window_id: self.active_window_id,
            payload: closed_window.payload,
        })
    }

    fn next_window_id(&self) -> Result<(WindowId, u64), WindowError> {
        let value = self.next_window_id;
        let next = value.checked_add(1).ok_or(WindowError::IdSpaceExhausted)?;
        Ok((WindowId::from_raw(value), next))
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
    fn new_should_create_one_valid_active_window() {
        let windows = WindowCollection::new(|_| "first");

        assert_eq!(
            (
                windows.len(),
                windows.active_window_id(),
                windows.active_window()
            ),
            (1, WindowId::new(1), &"first")
        );
    }

    #[test]
    fn iter_should_preserve_window_creation_order() {
        let mut windows = WindowCollection::new(|_| "first");
        windows.create_window(|_| "second").unwrap();
        windows.create_window(|_| "third").unwrap();

        let ordered_windows = windows.iter().collect::<Vec<_>>();

        assert_eq!(
            ordered_windows,
            vec![
                (WindowId::new(1), &"first"),
                (WindowId::new(2), &"second"),
                (WindowId::new(3), &"third"),
            ]
        );
    }

    #[test]
    fn create_window_should_create_and_activate_the_new_window() {
        let mut windows = WindowCollection::new(|_| "first");

        let created = windows.create_window(|_| "second").unwrap();

        assert_eq!(
            (
                created,
                windows.len(),
                windows.active_window_id(),
                windows.active_window()
            ),
            (WindowId::new(2), 2, WindowId::new(2), &"second")
        );
    }

    #[test]
    fn create_window_should_reject_exhausted_ids_before_creating_its_payload() {
        let mut windows = WindowCollection::new(|_| "first");
        windows.next_window_id = u64::MAX;
        let creations = Cell::new(0);

        let result = windows.create_window(|_| {
            creations.update(|count| count + 1);
            "second"
        });

        assert_eq!(
            (result, creations.get(), windows.len()),
            (Err(WindowError::IdSpaceExhausted), 0, 1)
        );
    }

    #[test]
    fn activate_window_should_select_an_owned_window() {
        let mut windows = WindowCollection::new(|_| "first");
        windows.create_window(|_| "second").unwrap();

        windows.activate_window(WindowId::new(1)).unwrap();

        assert_eq!(
            (windows.active_window_id(), windows.active_window()),
            (WindowId::new(1), &"first")
        );
    }

    #[test]
    fn activate_window_should_reject_an_unknown_id_without_changing_the_active_window() {
        let mut windows = WindowCollection::new(|_| "first");

        let result = windows.activate_window(WindowId::new(99));

        assert_eq!(
            (result, windows.active_window_id()),
            (
                Err(WindowError::WindowNotFound(WindowId::new(99))),
                WindowId::new(1)
            )
        );
    }

    #[test]
    fn close_window_should_preserve_the_active_window_when_closing_an_inactive_window() {
        let mut windows = WindowCollection::new(|_| "first");
        windows.create_window(|_| "second").unwrap();
        windows.create_window(|_| "third").unwrap();

        let outcome = windows.close_window(WindowId::new(1)).unwrap();

        let CloseWindowOutcome::WindowClosed {
            closed_window_id,
            active_window_id,
            payload,
        } = outcome
        else {
            panic!("closing one of multiple Windows must remove it")
        };
        assert_eq!(
            (closed_window_id, active_window_id, payload),
            (WindowId::new(1), WindowId::new(3), "first")
        );
    }

    #[test]
    fn close_window_should_focus_the_next_window_when_closing_the_active_middle_window() {
        let mut windows = WindowCollection::new(|_| "first");
        windows.create_window(|_| "second").unwrap();
        windows.create_window(|_| "third").unwrap();
        windows.activate_window(WindowId::new(2)).unwrap();

        let outcome = windows.close_window(WindowId::new(2)).unwrap();

        let CloseWindowOutcome::WindowClosed {
            active_window_id, ..
        } = outcome
        else {
            panic!("closing one of multiple Windows must remove it")
        };
        assert_eq!(active_window_id, WindowId::new(3));
    }

    #[test]
    fn close_window_should_focus_the_previous_window_when_closing_the_active_last_window() {
        let mut windows = WindowCollection::new(|_| "first");
        windows.create_window(|_| "second").unwrap();

        let outcome = windows.close_window(WindowId::new(2)).unwrap();

        let CloseWindowOutcome::WindowClosed {
            active_window_id, ..
        } = outcome
        else {
            panic!("closing one of multiple Windows must remove it")
        };
        assert_eq!(active_window_id, WindowId::new(1));
    }

    #[test]
    fn close_window_should_reject_an_unknown_id_without_mutation() {
        let mut windows = WindowCollection::new(|_| "first");

        let result = windows.close_window(WindowId::new(99));

        assert_eq!(
            (
                result.err(),
                windows.len(),
                windows.active_window_id(),
                windows.active_window()
            ),
            (
                Some(WindowError::WindowNotFound(WindowId::new(99))),
                1,
                WindowId::new(1),
                &"first"
            )
        );
    }

    #[test]
    fn closed_window_ids_should_not_be_reused() {
        let mut windows = WindowCollection::new(|_| "first");
        let second = windows.create_window(|_| "second").unwrap();
        windows.close_window(second).unwrap();

        let third = windows.create_window(|_| "third").unwrap();

        assert_eq!(third, WindowId::new(3));
    }

    #[test]
    fn close_window_should_request_operating_system_close_for_the_final_window() {
        let mut windows = WindowCollection::new(|_| "first");

        let outcome = windows.close_window(WindowId::new(1)).unwrap();

        assert!(matches!(
            outcome,
            CloseWindowOutcome::CloseOperatingSystemWindow
        ));
        assert_eq!(
            (
                windows.len(),
                windows.active_window_id(),
                windows.active_window()
            ),
            (1, WindowId::new(1), &"first")
        );
    }

    #[test]
    fn close_window_should_transfer_ownership_and_drop_each_payload_exactly_once() {
        let drops = Rc::new(Cell::new(0));
        let mut windows = WindowCollection::new(|_| DropProbe {
            drops: Rc::clone(&drops),
        });
        windows
            .create_window(|_| DropProbe {
                drops: Rc::clone(&drops),
            })
            .unwrap();

        let outcome = windows.close_window(WindowId::new(2)).unwrap();
        assert_eq!(drops.get(), 0);

        drop(outcome);
        assert_eq!(drops.get(), 1);

        drop(windows);
        assert_eq!(drops.get(), 2);
    }
}
