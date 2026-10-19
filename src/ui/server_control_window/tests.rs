use std::rc::Rc;

use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, TestAppContext, VisualTestContext};

use super::*;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
};
use crate::settings::storage::testing::MemoryStorage;
use spaceterm_server::service::Service;

struct RecordingMovement;

impl WindowMovementFactory for RecordingMovement {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
    }
}

fn open_window(step: Step, cx: &mut TestAppContext) -> gpui::AnyWindowHandle {
    open_window_for(Operation::Start, step, cx)
}

fn open_window_for(
    operation: Operation,
    step: Step,
    cx: &mut TestAppContext,
) -> gpui::AnyWindowHandle {
    let settings = crate::settings::Settings::load(MemoryStorage::with_document(
        &crate::settings::SettingsDocument::default(),
    ));
    // Construction only resolves account paths. This test performs no service operation.
    let controller = Controller::new(Service::for_identity("spaceterm-development").unwrap());
    cx.update(|cx| {
        crate::ui::appearance_runtime::install(
            settings,
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::app::init(
            cx,
            Rc::new(
                crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
            ),
            Rc::new(
                crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
            ),
        )
        .expect("application actions should install");
        open(
            controller,
            operation,
            step,
            Rc::new(RecordingMovement),
            Box::new(|_| {}),
            cx,
        )
        .expect("the control window should open");
    });
    cx.run_until_parked();
    let [window] = cx.windows()[..] else {
        panic!("one control window should open");
    };
    window
}

/// Presses and releases Space, which buttons activate on release.
fn press_space(window: &mut VisualTestContext) {
    window.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse("space").unwrap(),
        prefer_character_input: false,
        is_held: false,
    });
    window.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("space").unwrap(),
    });
    window.run_until_parked();
}

#[gpui::test]
fn tab_reaches_cancel_first_and_space_chooses_it(cx: &mut TestAppContext) {
    let window = open_window(
        Step::Authorize {
            declined: false,
            stopped: false,
        },
        cx,
    );
    let window = &mut VisualTestContext::from_window(window, cx);

    // Cancel comes before the step that asks the operating system for a password.
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("server-control-cancel-keyboard-focus")
            .is_some(),
        "Tab shows keyboard focus on Cancel"
    );
    press_space(window);

    assert!(
        window.windows().is_empty(),
        "Space on the focused Cancel closes the window without authorizing"
    );
}

#[gpui::test]
fn shift_tab_reaches_the_last_choice(cx: &mut TestAppContext) {
    let window = open_window(Step::Failed(ServerError::StartupFailed), cx);
    let window = &mut VisualTestContext::from_window(window, cx);

    window.simulate_keystrokes("shift-tab");
    window.run_until_parked();
    press_space(window);

    assert!(
        window.windows().is_empty(),
        "Space on the focused OK closes the window"
    );
}

#[gpui::test]
fn close_all_windows_keeps_a_window_whose_operation_is_running(cx: &mut TestAppContext) {
    open_window(Step::Working(Operation::Stop), cx);
    cx.update(|cx| cx.dispatch_action(&crate::app::CloseAllWindows));
    cx.run_until_parked();

    // Closing would abandon the admitted Stop's result and the wait that exiting relies on.
    assert_eq!(cx.windows().len(), 1);
}

#[gpui::test]
fn close_all_windows_cancels_a_window_that_is_only_asking(cx: &mut TestAppContext) {
    open_window(
        Step::Authorize {
            declined: false,
            stopped: false,
        },
        cx,
    );
    cx.update(|cx| cx.dispatch_action(&crate::app::CloseAllWindows));
    cx.run_until_parked();

    assert!(cx.windows().is_empty());
}

/// Restart stops the server before it starts it again. When starting then needs authorization,
/// the window asks for it and continues as Start, so the person never confirms a second Restart.
#[gpui::test]
fn a_restart_that_needs_authorization_continues_as_start(cx: &mut TestAppContext) {
    let window = open_window_for(Operation::Restart, Step::Working(Operation::Restart), cx);
    let control = window.downcast::<ServerControlWindow>().unwrap();
    control
        .update(cx, |this, window, cx| {
            this.run(
                Task::ready(Err(ServerError::AuthorizationRequired)),
                window,
                cx,
            );
        })
        .unwrap();
    cx.run_until_parked();

    let (operation, authorizing) = control
        .read_with(cx, |this, _| {
            (
                this.operation,
                matches!(
                    this.step,
                    Some(Step::Authorize {
                        declined: false,
                        stopped: true,
                    })
                ),
            )
        })
        .unwrap();
    assert!(authorizing, "the window explains startup authorization");
    assert_eq!(operation, Operation::Start);
    assert!(!cx.update(|cx| crate::status_menu::operations_running(cx)));
}
