//! Client-side window moves through the compositor's interactive move request.
use std::rc::Rc;

use gpui::Window;

use super::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform, WindowMovementFactory,
};

pub(super) struct LinuxWindowMovementFactory;

impl WindowMovementFactory for LinuxWindowMovementFactory {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        Rc::new(LinuxWindowDragPlatform)
    }
}

struct LinuxWindowDragPlatform;

impl OperatingSystemWindowDragPlatform for LinuxWindowDragPlatform {
    fn interaction_started(&self) -> Result<(), OperatingSystemWindowDragError> {
        Ok(())
    }

    fn start_window_move(&self, window: &Window) -> Result<(), OperatingSystemWindowDragError> {
        window.start_window_move();
        Ok(())
    }

    fn interaction_finished(&self) {}

    fn show_window_menu(&self, window: &Window, position: gpui::Point<gpui::Pixels>) {
        window.titlebar_right_click(position);
    }
}
