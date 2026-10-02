//! Retains exact-window visibility facts until the presentation owner releases its source.
use std::{cell::Cell, rc::Rc};

use gpui::{Subscription, Window};

use super::window_visibility::{WindowVisibility, WindowVisibilityFactory, WindowVisibilitySource};

pub(super) struct LinuxWindowVisibilityFactory;

impl WindowVisibilityFactory for LinuxWindowVisibilityFactory {
    fn capture(
        &self,
        window: &Window,
        changed: Box<dyn Fn()>,
    ) -> Option<Box<dyn WindowVisibilitySource>> {
        let visibility = Rc::new(Cell::new(window.visibility()));
        let observed = visibility.clone();
        let subscription = window.observe_window_visibility(move |visibility, _, _| {
            observed.set(visibility);
            changed();
        });
        Some(Box::new(LinuxWindowVisibilitySource {
            visibility,
            _subscription: subscription,
        }))
    }
}

struct LinuxWindowVisibilitySource {
    visibility: Rc<Cell<gpui::WindowVisibility>>,
    _subscription: Subscription,
}

impl WindowVisibilitySource for LinuxWindowVisibilitySource {
    fn current(&self) -> WindowVisibility {
        WindowVisibility {
            occluded: self.visibility.get() == gpui::WindowVisibility::Hidden,
            ..WindowVisibility::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn linux_window_visibility_tracks_changes_and_releases_its_observer(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = cx.add_window(|_, _| gpui::Empty);
        let notifications = Rc::new(Cell::new(0));
        let source = window
            .update(cx, |_, window, _| {
                let notifications = notifications.clone();
                LinuxWindowVisibilityFactory
                    .capture(
                        window,
                        Box::new(move || notifications.set(notifications.get() + 1)),
                    )
                    .unwrap()
            })
            .unwrap();
        cx.simulate_window_visibility_change(window.into(), gpui::WindowVisibility::Hidden);
        assert!(source.current().occluded);
        assert_eq!(notifications.get(), 1);
        cx.simulate_window_visibility_change(window.into(), gpui::WindowVisibility::Hidden);
        assert_eq!(notifications.get(), 1);
        cx.simulate_window_visibility_change(window.into(), gpui::WindowVisibility::Visible);
        assert!(!source.current().occluded);
        assert_eq!(notifications.get(), 2);
        drop(source);
        cx.simulate_window_visibility_change(window.into(), gpui::WindowVisibility::Hidden);
        assert_eq!(notifications.get(), 2);
    }

    #[gpui::test]
    fn linux_window_visibility_reports_captured_windows_visible(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let visibility = cx.update(|window, _| {
            LinuxWindowVisibilityFactory
                .capture(window, Box::new(|| {}))
                .map(|source| source.current())
        });
        assert_eq!(
            visibility,
            Some(WindowVisibility {
                minimized: false,
                occluded: false,
                live_resize: false,
            })
        );
    }
}
