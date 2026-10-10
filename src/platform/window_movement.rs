use gpui::Window;
#[cfg(test)]
use std::cell::Cell;

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "desktops whose toolkit owns window moves report no native failures"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum OperatingSystemWindowDragError {
    #[error("the application is unavailable")]
    Application,
    #[error("the current pointer event is not a primary mouse-down")]
    MouseDownEvent,
    #[error("the GPUI Operating-System Window view is unavailable")]
    NativeView,
    #[error("the Operating-System Window is unavailable")]
    NativeWindow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WindowMoveStart {
    #[default]
    AwaitMovement,
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(
            dead_code,
            reason = "only macOS starts window moves during the mouse-down callback"
        )
    )]
    Started,
}

pub(crate) trait OperatingSystemWindowDragPlatform {
    /// Called synchronously from the primary mouse-down callback. A host may start its native
    /// interaction now or wait for `start_window_move` after the control's movement threshold.
    fn interaction_started(
        &self,
        window: &Window,
    ) -> Result<WindowMoveStart, OperatingSystemWindowDragError>;
    fn start_window_move(&self, window: &Window) -> Result<(), OperatingSystemWindowDragError>;
    fn show_window_menu(&self, _: &Window, _: gpui::Point<gpui::Pixels>) {}
    fn interaction_finished(&self);
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingOperatingSystemWindowDragPlatform {
    interaction_starts: Cell<usize>,
    move_requests: Cell<usize>,
    interaction_finishes: Cell<usize>,
    immediate_handoff: Cell<bool>,
}

#[cfg(test)]
impl RecordingOperatingSystemWindowDragPlatform {
    pub(crate) fn handoff_on_press(&self) {
        self.immediate_handoff.set(true);
    }

    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        (
            self.interaction_starts.get(),
            self.move_requests.get(),
            self.interaction_finishes.get(),
        )
    }
}

#[cfg(test)]
impl OperatingSystemWindowDragPlatform for RecordingOperatingSystemWindowDragPlatform {
    fn interaction_started(
        &self,
        window: &Window,
    ) -> Result<WindowMoveStart, OperatingSystemWindowDragError> {
        self.interaction_starts
            .set(self.interaction_starts.get() + 1);
        if self.immediate_handoff.get() {
            self.start_window_move(window)?;
            Ok(WindowMoveStart::Started)
        } else {
            Ok(WindowMoveStart::AwaitMovement)
        }
    }

    fn start_window_move(&self, _: &Window) -> Result<(), OperatingSystemWindowDragError> {
        self.move_requests.set(self.move_requests.get() + 1);
        Ok(())
    }

    fn interaction_finished(&self) {
        self.interaction_finishes
            .set(self.interaction_finishes.get() + 1);
    }
}

pub(crate) trait WindowMovementFactory {
    fn create(&self) -> std::rc::Rc<dyn OperatingSystemWindowDragPlatform>;
}
