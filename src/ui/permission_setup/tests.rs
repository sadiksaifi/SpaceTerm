//! Permission Setup driven through a scripted capability and a scripted System Settings window.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, Bounds, DisplayId, Entity, ExternalDragPayload, FileDragIcon, Modifiers,
    MouseButton, Pixels, TestAppContext, VisualTestContext, WindowHandle, bounds, point, px, size,
};

use super::{
    AUTHORIZATION_INTERVALS, COVERED_TRACKING_INTERVAL, OPENING_SETTLE, OPENING_TIMEOUT,
    PermissionSetup, PermissionSetupFailure, PermissionSetupStatus, SetupGuide, SetupStep,
    TRACKING_INTERVAL,
};
use crate::appearance::{Appearance, SettingsDocument};
use crate::application_identity::ApplicationIdentity;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::computer_use_access::testing::ScriptedComputerUseAccess;
use crate::platform::computer_use_access::{
    ComputerUseAccessError, ComputerUseAuthorization, ComputerUsePermission,
};
use crate::platform::setup_guide_host::testing::ScriptedSetupGuideHost;
use crate::platform::setup_guide_host::{SetupGuideHost as _, SystemSettingsWindow};
use crate::ui::appearance_runtime;
use crate::ui::settings_window::test_support::MemoryStorage;

use ComputerUseAuthorization::{Granted, NotGranted};
use ComputerUsePermission::{Accessibility, ScreenRecording};

struct Fixture {
    access: Rc<ScriptedComputerUseAccess>,
    host: Arc<ScriptedSetupGuideHost>,
    setup: Entity<PermissionSetup>,
    display: DisplayId,
}

fn install(
    screen_recording: ComputerUseAuthorization,
    accessibility: ComputerUseAuthorization,
    cx: &mut TestAppContext,
) -> Fixture {
    let settings = crate::settings::UserSettings::load(MemoryStorage::with_document(
        &SettingsDocument::default(),
    ));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Light));
    let access = ScriptedComputerUseAccess::new(Ok(screen_recording), Ok(accessibility));
    let host = ScriptedSetupGuideHost::new();
    let (setup, display) = cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        let setup = PermissionSetup::create(access.clone(), host.clone(), cx);
        let display = cx.primary_display().expect("a display").id();
        (setup, display)
    });
    Fixture {
        access,
        host,
        setup,
        display,
    }
}

impl Fixture {
    fn start(&self, permissions: &[ComputerUsePermission], cx: &mut TestAppContext) {
        self.setup
            .update(cx, |setup, cx| setup.start(permissions, cx));
        cx.run_until_parked();
    }

    fn current(&self, cx: &mut TestAppContext) -> Option<(ComputerUsePermission, SetupStep)> {
        self.setup.read_with(cx, |setup, _| setup.current())
    }

    fn status(
        &self,
        permission: ComputerUsePermission,
        cx: &mut TestAppContext,
    ) -> PermissionSetupStatus {
        self.setup
            .read_with(cx, |setup, _| setup.status(permission))
    }

    /// Shows System Settings in front at `frame` and lets the setup follow it once.
    fn show_settings(&self, frame: Bounds<Pixels>, cx: &mut TestAppContext) {
        self.host.set_window(SystemSettingsWindow::Frontmost {
            display: self.display,
            content: frame,
        });
        tick(cx);
    }

    fn set_window(&self, window: SystemSettingsWindow, cx: &mut TestAppContext) {
        self.host.set_window(window);
        tick(cx);
    }
}

fn settings_frame() -> Bounds<Pixels> {
    bounds(point(px(400.0), px(100.0)), size(px(715.0), px(560.0)))
}

/// Lets the setup look at System Settings at least once, however it last found it, and lets a
/// newly opened list settle.
fn tick(cx: &mut TestAppContext) {
    cx.executor()
        .advance_clock(COVERED_TRACKING_INTERVAL.max(OPENING_SETTLE) + TRACKING_INTERVAL);
    cx.run_until_parked();
}

fn guide(cx: &mut TestAppContext) -> Option<WindowHandle<SetupGuide>> {
    cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find_map(|window| window.downcast::<SetupGuide>())
    })
}

fn guide_context(cx: &mut TestAppContext) -> &mut VisualTestContext {
    let handle: AnyWindowHandle = guide(cx).expect("the guide is open").into();
    let cx = VisualTestContext::from_window(handle, cx).into_mut();
    cx.run_until_parked();
    cx
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn a_setup_opens_system_settings_and_docks_the_guide_inside_it(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);

    fixture.start(&[ScreenRecording], cx);

    assert_eq!(*fixture.access.prepared.borrow(), [ScreenRecording]);
    assert_eq!(*fixture.access.opened.borrow(), [ScreenRecording]);
    assert_eq!(
        fixture.current(cx),
        Some((ScreenRecording, SetupStep::Opening))
    );
    assert_eq!(
        fixture.status(ScreenRecording, cx),
        PermissionSetupStatus::Running
    );
    assert_eq!(
        fixture.status(Accessibility, cx),
        PermissionSetupStatus::Idle
    );
    // System Settings is still launching, so nothing is presented yet.
    tick(cx);
    assert!(guide(cx).is_none());

    fixture.show_settings(settings_frame(), cx);

    assert_eq!(
        fixture.current(cx),
        Some((ScreenRecording, SetupStep::Guiding))
    );
    let handle = guide(cx).expect("the guide opens on System Settings");
    let frame = cx
        .update(|cx| handle.update(cx, |_, window, _| window.bounds()))
        .expect("the guide is open");
    assert_eq!(frame.bottom(), settings_frame().bottom() - px(12.0));
    assert_eq!(frame.size.height, super::guide::GUIDING_HEIGHT);
    assert!(frame.left() > settings_frame().left() && frame.right() < settings_frame().right());
    assert_eq!(fixture.host.glass_requests(), 1);
}

#[gpui::test]
fn the_guide_follows_system_settings_and_hides_while_it_is_covered(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[Accessibility], cx);
    fixture.show_settings(settings_frame(), cx);
    let handle: AnyWindowHandle = guide(cx).expect("the guide is open").into();

    let moved = bounds(point(px(200.0), px(140.0)), settings_frame().size);
    fixture.show_settings(moved, cx);

    let (requested, display) = *cx
        .window_bounds_requests(handle)
        .last()
        .expect("the guide moved with System Settings");
    assert_eq!(requested.bottom(), moved.bottom() - px(12.0));
    assert_eq!(display, Some(fixture.display));
    // An unchanged frame asks for no move.
    let requests = cx.window_bounds_requests(handle).len();
    fixture.show_settings(moved, cx);
    assert_eq!(cx.window_bounds_requests(handle).len(), requests);

    fixture.set_window(SystemSettingsWindow::Covered, cx);
    assert!(guide(cx).is_none());
    assert_eq!(
        fixture.current(cx),
        Some((Accessibility, SetupStep::Guiding))
    );

    fixture.show_settings(moved, cx);
    assert!(guide(cx).is_some());

    // Closing System Settings ends the setup without a failure.
    fixture.set_window(SystemSettingsWindow::Closed, cx);
    assert!(guide(cx).is_none());
    assert_eq!(fixture.current(cx), None);
    assert_eq!(
        fixture.status(Accessibility, cx),
        PermissionSetupStatus::Idle
    );
}

#[gpui::test]
fn a_grant_read_while_guiding_completes_the_setup(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);
    assert_eq!(fixture.access.observers(), 1);

    // Screen Recording reports no change, so the setup reads it on its own schedule.
    fixture.access.set(ScreenRecording, Ok(Granted));
    for _ in 0..AUTHORIZATION_INTERVALS {
        tick(cx);
    }

    assert_eq!(
        fixture.current(cx),
        Some((ScreenRecording, SetupStep::Granted))
    );
    // Nothing waits, so the guide offers no Continue and closing it finishes the setup.
    let guide = guide_context(cx);
    assert!(guide.debug_bounds("setup-guide-application").is_none());
    assert!(guide.debug_bounds("setup-guide-continue").is_none());

    click("setup-guide-close", guide);

    assert_eq!(fixture.current(cx), None);
    assert!(self::guide(cx).is_none());
    assert_eq!(fixture.access.observers(), 0);
}

#[gpui::test]
fn a_reported_change_completes_the_setup_at_once(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[Accessibility], cx);
    fixture.show_settings(settings_frame(), cx);

    fixture.access.set(Accessibility, Ok(Granted));
    fixture.access.report_change();
    cx.run_until_parked();

    assert_eq!(
        fixture.current(cx),
        Some((Accessibility, SetupStep::Granted))
    );
}

#[gpui::test]
fn a_setup_skips_a_granted_permission_and_continues_to_the_next(cx: &mut TestAppContext) {
    let fixture = install(Granted, NotGranted, cx);

    fixture.start(&[ScreenRecording, Accessibility, ScreenRecording], cx);

    assert_eq!(
        *fixture.access.prepared.borrow(),
        [ScreenRecording, Accessibility]
    );
    assert_eq!(*fixture.access.opened.borrow(), [Accessibility]);
    assert_eq!(
        fixture.current(cx),
        Some((Accessibility, SetupStep::Opening))
    );
}

#[gpui::test]
fn a_granted_permission_continues_to_the_next_only_when_asked(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[Accessibility, ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);
    {
        // While guiding, the drag is the only action besides closing.
        let guide = guide_context(cx);
        assert!(guide.debug_bounds("setup-guide-application").is_some());
        assert!(guide.debug_bounds("setup-guide-continue").is_none());
    }

    fixture.access.set(Accessibility, Ok(Granted));
    fixture.access.report_change();
    cx.run_until_parked();
    assert_eq!(*fixture.access.opened.borrow(), [Accessibility]);

    let guide = guide_context(cx);
    click("setup-guide-continue", guide);

    assert_eq!(
        *fixture.access.prepared.borrow(),
        [Accessibility, ScreenRecording]
    );
    assert_eq!(
        *fixture.access.opened.borrow(),
        [Accessibility, ScreenRecording]
    );
    assert_eq!(
        fixture.current(cx),
        Some((ScreenRecording, SetupStep::Opening))
    );
    // The guide returns once System Settings shows the next list.
    assert!(self::guide(cx).is_none());
    fixture.show_settings(settings_frame(), cx);
    assert!(self::guide(cx).is_some());
}

/// After a grant the person may return to SpaceTerm, which hides the guide. A request for another
/// permission then moves on at once instead of waiting for Continue in the hidden guide.
#[gpui::test]
fn a_request_after_a_grant_opens_the_next_list(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);
    fixture.access.set(ScreenRecording, Ok(Granted));
    fixture.access.report_change();
    cx.run_until_parked();
    fixture.set_window(SystemSettingsWindow::Covered, cx);
    assert!(guide(cx).is_none());

    fixture.start(&[Accessibility], cx);

    assert_eq!(
        *fixture.access.opened.borrow(),
        [ScreenRecording, Accessibility]
    );
    assert_eq!(
        fixture.current(cx),
        Some((Accessibility, SetupStep::Opening))
    );
}

/// System Settings keeps showing the previous list for a moment after it is asked for another, so
/// the guide waits before pointing at "the list above".
#[gpui::test]
fn the_guide_waits_for_a_new_list_to_settle(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.host.set_window(SystemSettingsWindow::Frontmost {
        display: fixture.display,
        content: settings_frame(),
    });

    cx.executor().advance_clock(TRACKING_INTERVAL);
    cx.run_until_parked();
    assert!(guide(cx).is_none());

    cx.executor().advance_clock(OPENING_SETTLE);
    cx.run_until_parked();
    assert!(guide(cx).is_some());
}

/// While System Settings is covered the person cannot change a grant, so the setup reads none and
/// starts no verification.
#[gpui::test]
fn a_covered_setup_reads_no_authorization(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);
    // Leaving System Settings reads once, so a grant made just before is found.
    let reads = fixture.access.reads();
    fixture.set_window(SystemSettingsWindow::Covered, cx);
    assert_eq!(fixture.access.reads(), reads + 1);

    for _ in 0..AUTHORIZATION_INTERVALS * 2 {
        tick(cx);
    }

    assert_eq!(fixture.access.reads(), reads + 1);
    assert_eq!(
        fixture.current(cx),
        Some((ScreenRecording, SetupStep::Guiding))
    );
}

#[gpui::test]
fn a_second_request_joins_the_running_setup(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[Accessibility], cx);
    fixture.show_settings(settings_frame(), cx);

    fixture.start(&[Accessibility, ScreenRecording], cx);

    // System Settings comes forward again at the current list, and the new permission waits.
    assert_eq!(
        *fixture.access.opened.borrow(),
        [Accessibility, Accessibility]
    );
    assert_eq!(
        fixture.current(cx),
        Some((Accessibility, SetupStep::Guiding))
    );
    assert_eq!(
        fixture.status(ScreenRecording, cx),
        PermissionSetupStatus::Running
    );
}

#[gpui::test]
fn closing_the_guide_cancels_and_leaves_system_settings_alone(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);

    let guide = guide_context(cx);
    click("setup-guide-close", guide);

    assert_eq!(fixture.current(cx), None);
    assert!(self::guide(cx).is_none());
    assert_eq!(
        fixture.status(ScreenRecording, cx),
        PermissionSetupStatus::Idle
    );
    assert_eq!(fixture.access.opened.borrow().len(), 1);
}

/// Dragging the application out of the guide hands its bundle to the system, which is what a
/// System Settings list accepts, drawn as a copy of the row held where the pointer took it.
#[gpui::test]
fn dragging_the_application_out_of_the_guide_offers_its_bundle(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);
    let handle: AnyWindowHandle = guide(cx).expect("the guide is open").into();
    let row = {
        let guide = guide_context(cx);
        let row = guide
            .debug_bounds("setup-guide-application")
            .expect("the application is offered");
        let start = row.center();
        guide.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        guide.simulate_mouse_move(
            start + point(px(10.0), px(0.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        guide.simulate_mouse_move(
            point(px(-1.0), start.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        row
    };

    let drawn = fixture.host.drawn_rows();
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].size, row.size);
    assert_eq!(drawn[0].name, ApplicationIdentity::current().display_name());
    let payloads = cx.external_drag_payloads(handle);
    let [ExternalDragPayload::Files(files)] = payloads.as_slice() else {
        panic!("one file drag");
    };
    assert_eq!(
        files.entries(),
        [("/Applications/SpaceTerm.app".into(), true)]
    );
    let FileDragIcon::Image {
        size,
        cursor_offset,
        ..
    } = files.icon()
    else {
        panic!("the drag shows the row");
    };
    assert_eq!(*size, row.size);
    assert!(
        Bounds::new(point(px(0.0), px(0.0)), row.size).contains(cursor_offset),
        "the pointer holds the row"
    );
}

/// A setup that ends while its guide handles an event still closes the guide once the event ends.
#[gpui::test]
fn a_guide_busy_with_an_event_closes_after_it(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);
    let handle = guide(cx).expect("the guide is open");

    let setup = fixture.setup.clone();
    handle
        .update(cx, |_, _, cx| {
            setup.update(cx, |setup, cx| setup.cancel(cx))
        })
        .expect("the guide handles the event");
    cx.run_until_parked();

    assert!(guide(cx).is_none());
    assert_eq!(fixture.current(cx), None);
}

/// A guide busy with an event stays the one guide; tracking never opens a second.
#[gpui::test]
fn a_guide_busy_with_an_event_is_not_replaced(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[ScreenRecording], cx);
    fixture.show_settings(settings_frame(), cx);
    let handle = guide(cx).expect("the guide is open");

    let setup = fixture.setup.clone();
    let located = fixture.host.locate_system_settings();
    handle
        .update(cx, |_, _, cx| {
            setup.update(cx, |setup, cx| setup.follow(located, cx));
        })
        .expect("the guide handles the event");
    cx.run_until_parked();

    let guides = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .filter(|window| window.downcast::<SetupGuide>().is_some())
            .count()
    });
    assert_eq!(guides, 1);
}

/// The guide says when the setup removed an earlier entry, and otherwise says to turn on an entry
/// the list may still hold.
#[gpui::test]
fn the_guide_reports_whether_the_setup_cleared_an_entry(cx: &mut TestAppContext) {
    for resettable in [true, false] {
        let fixture = install(NotGranted, NotGranted, cx);
        fixture.access.resettable.set(resettable);
        fixture.start(&[ScreenRecording], cx);
        fixture.show_settings(settings_frame(), cx);

        let cleared = fixture.setup.read_with(cx, |setup, _| {
            setup.presentation().map(|shown| shown.cleared)
        });
        assert_eq!(cleared, Some(resettable));
        fixture.setup.update(cx, |setup, cx| setup.cancel(cx));
        cx.run_until_parked();
    }
}

#[gpui::test]
fn a_failed_open_ends_the_setup_with_a_failure(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture
        .access
        .open_failure
        .set(Some(ComputerUseAccessError::PlatformRejected));

    fixture.start(&[ScreenRecording], cx);

    assert_eq!(fixture.current(cx), None);
    assert_eq!(
        fixture.status(ScreenRecording, cx),
        PermissionSetupStatus::Failed(PermissionSetupFailure::SettingsUnavailable)
    );
    assert_eq!(fixture.access.observers(), 0);

    // The next setup of the permission clears the failure.
    fixture.access.open_failure.set(None);
    fixture.start(&[ScreenRecording], cx);
    assert_eq!(
        fixture.status(ScreenRecording, cx),
        PermissionSetupStatus::Running
    );
}

#[gpui::test]
fn system_settings_that_never_comes_forward_ends_the_setup(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture.start(&[Accessibility], cx);

    cx.executor().advance_clock(OPENING_TIMEOUT);
    cx.run_until_parked();

    assert_eq!(fixture.current(cx), None);
    assert_eq!(
        fixture.status(Accessibility, cx),
        PermissionSetupStatus::Failed(PermissionSetupFailure::SettingsNotShown)
    );
}

/// A preparation that cannot verify the permission still opens System Settings, where the guide
/// explains how to turn on an existing entry.
#[gpui::test]
fn a_failed_preparation_still_guides(cx: &mut TestAppContext) {
    let fixture = install(NotGranted, NotGranted, cx);
    fixture
        .access
        .setup_failure
        .set(Some(ComputerUseAccessError::PlatformUnavailable));

    fixture.start(&[ScreenRecording], cx);

    assert_eq!(*fixture.access.opened.borrow(), [ScreenRecording]);
    assert_eq!(
        fixture.current(cx),
        Some((ScreenRecording, SetupStep::Opening))
    );
}
