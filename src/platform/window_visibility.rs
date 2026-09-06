//! Exact-window visibility facts. Rendering and observation scheduling remain caller-owned.

use gpui::Window;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WindowVisibility {
    pub(crate) minimized: bool,
    pub(crate) occluded: bool,
    pub(crate) live_resize: bool,
}

/// Owns observation of one exact window until dropped.
pub(crate) trait WindowVisibilitySource {
    fn current(&self) -> WindowVisibility;
}

pub(crate) trait WindowVisibilityFactory {
    /// `changed` must enqueue a wakeup, never synchronously reenter GPUI.
    fn capture(
        &self,
        window: &Window,
        changed: Box<dyn Fn()>,
    ) -> Option<Box<dyn WindowVisibilitySource>>;
}

#[cfg(test)]
pub(crate) use recording::RecordingWindowVisibilityFactory;

#[cfg(test)]
mod recording {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::{Rc, Weak};

    use gpui::WindowId;

    use super::*;

    #[derive(Default)]
    pub(crate) struct RecordingWindowVisibilityFactory {
        windows: RefCell<HashMap<WindowId, WindowRecord>>,
        captures: RefCell<Vec<WindowId>>,
        drops: Rc<Cell<usize>>,
    }

    #[derive(Default)]
    struct WindowRecord {
        visibility: Rc<Cell<WindowVisibility>>,
        sources: Vec<Weak<SourceState>>,
    }

    struct SourceState {
        visibility: Rc<Cell<WindowVisibility>>,
        changed: Box<dyn Fn()>,
        drops: Rc<Cell<usize>>,
    }

    impl Drop for SourceState {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    struct RecordingSource(Rc<SourceState>);

    impl WindowVisibilitySource for RecordingSource {
        fn current(&self) -> WindowVisibility {
            self.0.visibility.get()
        }
    }

    impl WindowVisibilityFactory for RecordingWindowVisibilityFactory {
        fn capture(
            &self,
            window: &Window,
            changed: Box<dyn Fn()>,
        ) -> Option<Box<dyn WindowVisibilitySource>> {
            let id = window.window_handle().window_id();
            self.captures.borrow_mut().push(id);
            let mut windows = self.windows.borrow_mut();
            let record = windows.entry(id).or_default();
            let source = Rc::new(SourceState {
                visibility: Rc::clone(&record.visibility),
                changed,
                drops: Rc::clone(&self.drops),
            });
            record.sources.push(Rc::downgrade(&source));
            Some(Box::new(RecordingSource(source)))
        }
    }

    impl RecordingWindowVisibilityFactory {
        pub(crate) fn set_visibility(&self, id: WindowId, visibility: WindowVisibility) {
            let sources = {
                let mut windows = self.windows.borrow_mut();
                let record = windows.entry(id).or_default();
                record.visibility.set(visibility);
                record.sources.retain(|source| source.strong_count() != 0);
                record
                    .sources
                    .iter()
                    .filter_map(Weak::upgrade)
                    .collect::<Vec<_>>()
            };
            for source in sources {
                (source.changed)();
            }
        }

        pub(crate) fn captured_windows(&self) -> Vec<WindowId> {
            self.captures.borrow().clone()
        }

        pub(crate) fn drop_count(&self) -> usize {
            self.drops.get()
        }
    }
}
