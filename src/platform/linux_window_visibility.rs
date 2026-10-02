//! Linux compositors expose no minimization, occlusion, or live-resize facts through GPUI yet, so
//! every captured window reports itself visible and its Panes keep presenting.
use gpui::Window;

use super::window_visibility::{WindowVisibility, WindowVisibilityFactory, WindowVisibilitySource};

pub(super) struct LinuxWindowVisibilityFactory;

impl WindowVisibilityFactory for LinuxWindowVisibilityFactory {
    fn capture(&self, _: &Window, _: Box<dyn Fn()>) -> Option<Box<dyn WindowVisibilitySource>> {
        Some(Box::new(LinuxWindowVisibilitySource))
    }
}

/// Reports a constant visible state, so it never signals a change.
struct LinuxWindowVisibilitySource;

impl WindowVisibilitySource for LinuxWindowVisibilitySource {
    fn current(&self) -> WindowVisibility {
        WindowVisibility::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
