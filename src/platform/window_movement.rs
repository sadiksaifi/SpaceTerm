use gpui::Window;
#[cfg(test)]
use std::cell::Cell;

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

pub(crate) trait OperatingSystemWindowDragPlatform {
    fn interaction_started(&self) -> Result<(), OperatingSystemWindowDragError>;
    fn start_window_move(&self, window: &Window) -> Result<(), OperatingSystemWindowDragError>;
    fn interaction_finished(&self);
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingOperatingSystemWindowDragPlatform {
    interaction_starts: Cell<usize>,
    move_requests: Cell<usize>,
    interaction_finishes: Cell<usize>,
}

#[cfg(test)]
impl RecordingOperatingSystemWindowDragPlatform {
    pub(crate) fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.interaction_starts.get(),
            self.move_requests.get(),
            self.interaction_finishes.get(),
            0,
        )
    }
}

#[cfg(test)]
impl OperatingSystemWindowDragPlatform for RecordingOperatingSystemWindowDragPlatform {
    fn interaction_started(&self) -> Result<(), OperatingSystemWindowDragError> {
        self.interaction_starts
            .set(self.interaction_starts.get() + 1);
        Ok(())
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
