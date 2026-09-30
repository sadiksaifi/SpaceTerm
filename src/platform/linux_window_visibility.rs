//! Linux compositors expose no occlusion or live-resize facts through GPUI yet, so no exact
//! visibility source is captured and Panes render as visible.
use gpui::Window;

use super::window_visibility::{WindowVisibilityFactory, WindowVisibilitySource};

pub(super) struct LinuxWindowVisibilityFactory;

impl WindowVisibilityFactory for LinuxWindowVisibilityFactory {
    fn capture(&self, _: &Window, _: Box<dyn Fn()>) -> Option<Box<dyn WindowVisibilitySource>> {
        None
    }
}
