//! Whether wheel and pointer input reads the Scrollback locally or goes to the program.
//!
//! These are the rules the server applied before reading moved to the client. A program that
//! enabled mouse reporting receives the wheel and the pointer, unless the person holds Shift and
//! the Shift-selection setting overrides application mouse reporting. On the alternate screen,
//! which has no Scrollback, the wheel becomes arrow keys when the program enabled alternate
//! scrolling and does nothing otherwise.

/// Where one wheel event goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelRoute {
    /// Moves this client's view through the Scrollback Cache.
    Scrollback,
    /// Goes to the program as mouse wheel reports or alternate-scroll arrow keys.
    Program,
    /// Does nothing.
    Ignore,
}

/// The program modes a frame reports, with whether Shift overrides mouse reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modes {
    pub(crate) mouse_tracking: bool,
    pub(crate) alternate_screen: bool,
    pub(crate) alternate_scroll: bool,
    /// Shift is held and the Shift-selection setting overrides application mouse reporting.
    pub(crate) shift_override: bool,
}

pub(crate) fn wheel_route(modes: Modes) -> WheelRoute {
    let reports = modes.mouse_tracking && !modes.shift_override;
    let alternate_scroll =
        modes.alternate_screen && !modes.mouse_tracking && modes.alternate_scroll;
    if reports || alternate_scroll {
        WheelRoute::Program
    } else if modes.alternate_screen {
        WheelRoute::Ignore
    } else {
        WheelRoute::Scrollback
    }
}

/// Where one pointer event goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerRoute {
    /// Starts or continues a local Selection gesture.
    Selection,
    /// Goes to the program as a mouse report.
    Program,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerPhase {
    Press,
    Motion,
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerButton {
    Left,
    Other,
}

/// Routes pointer events. A press decides the route of its whole gesture, so a program that
/// received the press receives its motion and release.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PointerRouter {
    program: bool,
    selecting: bool,
}

impl PointerRouter {
    pub(crate) fn route(
        &mut self,
        phase: PointerPhase,
        button: Option<PointerButton>,
        modes: Modes,
    ) -> PointerRoute {
        let tracking = modes.mouse_tracking && !modes.shift_override;
        match phase {
            PointerPhase::Press => {
                self.program = tracking && button.is_some();
                self.selecting = !tracking && button == Some(PointerButton::Left);
            }
            PointerPhase::Motion if button.is_none() => {
                // Hover motion is reported only while the program tracks the mouse.
                return if self.program || modes.mouse_tracking {
                    PointerRoute::Program
                } else {
                    PointerRoute::Ignore
                };
            }
            PointerPhase::Motion | PointerPhase::Release => {}
        }
        let route = if self.program {
            PointerRoute::Program
        } else if self.selecting {
            PointerRoute::Selection
        } else {
            PointerRoute::Ignore
        };
        if phase == PointerPhase::Release {
            self.program = false;
            self.selecting = false;
        }
        route
    }

    /// Forgets the gesture, for example when the view is hidden.
    pub(crate) fn cancel(&mut self) {
        *self = Self::default();
    }
}
