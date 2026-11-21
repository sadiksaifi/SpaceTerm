//! Pointer and wheel routing for the desktop terminal.
use super::route::{
    self, Modes, PointerButton, PointerPhase, PointerRoute, PointerRouter, WheelRoute,
};

#[test]
fn wheel_input_goes_to_the_program_under_todays_rules() {
    let modes = |mouse_tracking, alternate_screen, alternate_scroll, shift_override| Modes {
        mouse_tracking,
        alternate_screen,
        alternate_scroll,
        shift_override,
    };
    use WheelRoute::{Ignore, Program, Scrollback as Local};
    for (input, expected) in [
        (modes(false, false, false, false), Local),
        (modes(false, false, true, false), Local),
        (modes(true, false, false, false), Program),
        (modes(true, false, false, true), Local),
        (modes(true, true, true, false), Program),
        (modes(true, true, false, true), Ignore),
        (modes(false, true, true, false), Program),
        (modes(false, true, true, true), Program),
        (modes(false, true, false, false), Ignore),
    ] {
        assert_eq!(route::wheel_route(input), expected, "{input:?}");
    }
}

#[test]
fn a_press_decides_where_its_whole_pointer_gesture_goes() {
    use PointerPhase::{Motion, Press, Release};
    let tracking = Modes {
        mouse_tracking: true,
        ..Modes::default()
    };
    let shifted = Modes {
        shift_override: true,
        ..tracking
    };
    let plain = Modes::default();
    let left = Some(PointerButton::Left);
    let mut router = PointerRouter::default();
    assert_eq!(router.route(Press, left, tracking), PointerRoute::Program);
    // Releasing Shift or the program leaving mouse mode mid-gesture keeps the gesture's route.
    assert_eq!(router.route(Motion, left, plain), PointerRoute::Program);
    assert_eq!(router.route(Release, left, plain), PointerRoute::Program);
    assert_eq!(router.route(Press, left, shifted), PointerRoute::Selection);
    assert_eq!(
        router.route(Motion, left, tracking),
        PointerRoute::Selection
    );
    assert_eq!(
        router.route(Release, left, tracking),
        PointerRoute::Selection
    );
    assert_eq!(
        router.route(Motion, None, tracking),
        PointerRoute::Program,
        "hover reports"
    );
    assert_eq!(router.route(Motion, None, plain), PointerRoute::Ignore);
    assert_eq!(
        router.route(Press, Some(PointerButton::Other), plain),
        PointerRoute::Ignore
    );
    assert_eq!(
        router.route(Press, Some(PointerButton::Other), tracking),
        PointerRoute::Program
    );
}
