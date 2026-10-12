use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Context, Entity, FocusHandle, Modifiers, Pixels, Render, TestAppContext, VisualTestContext,
    Window, div, point, px,
};
use spaceterm_ui::a11y_testing::{A11yTree, perform};

use super::*;

/// What one Pane stand-in saw.
#[derive(Default)]
struct Record {
    actions: RefCell<Vec<PanePresentationAction>>,
    terminal_input: RefCell<Vec<String>>,
    terminal_clicks: Cell<usize>,
    /// Presses anywhere in the Pane, which the Tab View sees first and treats as selecting it.
    selections: Cell<usize>,
    /// Scrolls that reached the Pane itself rather than its presentation.
    scrolls: Cell<usize>,
}

/// A Pane stand-in: a focusable surface with Terminal content and the presentation layer over it.
/// It paints Terminal content only while the presentation allows it, and its input gate drops
/// every key the presentation refuses, as the Terminal Pane does. Like the Terminal Pane, it takes
/// focus on any press inside it, and like the Tab View around it, it treats that press as a
/// selection.
struct PaneHarness {
    index: usize,
    focus: FocusHandle,
    presentation_focus: PresentationFocus,
    presentation: PanePresentation,
    terminal_background: Color,
    width: Pixels,
    height: Pixels,
    record: Rc<Record>,
}

impl Render for PaneHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let record = Rc::clone(&self.record);
        let selections = Rc::clone(&self.record);
        let scrolls = Rc::clone(&self.record);
        let index = self.index;
        div()
            .id(("pane-frame", index))
            .capture_any_mouse_down(move |_, _, _| {
                selections.selections.set(selections.selections.get() + 1);
            })
            .child(
                div()
                    .id(("pane", index))
                    .debug_selector(move || {
                        if index == 0 {
                            "pane".to_owned()
                        } else {
                            format!("pane-{index}")
                        }
                    })
                    .track_focus(&self.focus)
                    .relative()
                    .w(self.width)
                    .h(self.height)
                    .on_key_down(cx.listener(|harness, event: &KeyDownEvent, window, cx| {
                        let traversed = harness.presentation.traverse_actions(
                            event,
                            &harness.presentation_focus,
                            window,
                            cx,
                        );
                        if !traversed && harness.presentation.accepts_input() {
                            harness
                                .record
                                .terminal_input
                                .borrow_mut()
                                .push(event.keystroke.key.clone());
                        }
                        // The Pane keeps every key, whether the Terminal receives it or not.
                        cx.stop_propagation();
                    }))
                    .on_scroll_wheel(move |_, _, _| scrolls.scrolls.set(scrolls.scrolls.get() + 1))
                    .when(self.presentation.content_eligible(), |pane| {
                        let clicks = Rc::clone(&record);
                        pane.child(
                            div()
                                .id("terminal")
                                .debug_selector(|| "terminal".to_owned())
                                .absolute()
                                .inset_0()
                                .on_mouse_down(gpui::MouseButton::Left, move |_, _, _| {
                                    clicks.terminal_clicks.set(clicks.terminal_clicks.get() + 1);
                                })
                                .child("fixture terminal output"),
                        )
                    })
                    .child(
                        PanePresentationLayer::new(
                            ("presentation", index),
                            self.presentation.clone(),
                            self.terminal_background,
                        )
                        .focus(self.presentation_focus.clone())
                        .on_action(move |action, _, _| record.actions.borrow_mut().push(action)),
                    ),
            )
    }
}

/// A window with a control before its Panes, so traversal that leaves a Pane has somewhere to go.
/// Tab from that control moves through the window's tab stops.
struct WindowHarness {
    outside: FocusHandle,
    panes: Vec<Entity<PaneHarness>>,
}

impl Render for WindowHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_start()
            .on_key_down(|event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.stop_propagation();
                }
            })
            .child(
                div()
                    .id("outside")
                    .role(accesskit::Role::Button)
                    .aria_label("Outside")
                    .track_focus(&self.outside)
                    .size(px(24.0)),
            )
            .children(self.panes.iter().cloned())
    }
}

/// Pane stand-ins side by side in one test window. Methods without an index act on the first.
struct Panes<'a> {
    harnesses: Vec<Entity<PaneHarness>>,
    focuses: Vec<FocusHandle>,
    records: Vec<Rc<Record>>,
    outside: FocusHandle,
    cx: &'a mut VisualTestContext,
}

impl Panes<'_> {
    fn present(&mut self, presentation: PanePresentation) {
        self.present_in(0, presentation);
    }

    fn present_in(&mut self, index: usize, presentation: PanePresentation) {
        self.harnesses[index].update(self.cx, |harness, cx| {
            harness.presentation = presentation;
            cx.notify();
        });
        self.cx.run_until_parked();
    }

    /// Presses and releases each key, since a button activates on release.
    fn press(&mut self, keys: &str) {
        press(self.cx, keys);
    }

    fn advance(&mut self, duration: Duration) {
        self.cx.executor().advance_clock(duration);
        self.cx.run_until_parked();
    }

    fn shows(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        let point = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} should be on screen"))
            .center();
        self.cx.simulate_click(point, Modifiers::none());
        self.cx.run_until_parked();
    }

    fn pane_focused(&mut self) -> bool {
        self.focused_in(0)
    }

    fn focused_in(&mut self, index: usize) -> bool {
        let focus = self.focuses[index].clone();
        self.cx.update(|window, _| focus.is_focused(window))
    }

    fn focus_pane(&mut self) {
        let focus = self.focuses[0].clone();
        self.cx.update(|window, cx| focus.focus(window, cx));
        self.cx.run_until_parked();
    }

    fn focus_outside(&mut self) {
        let outside = self.outside.clone();
        self.cx.update(|window, cx| outside.focus(window, cx));
        self.cx.run_until_parked();
    }

    /// The accessible name of the focused control.
    fn focused_label(&mut self) -> Option<String> {
        A11yTree::read(self.cx)
            .focused()
            .and_then(|node| node["aria"]["label"].as_str().map(str::to_owned))
    }

    fn record(&self) -> &Record {
        &self.records[0]
    }

    fn take_actions(&self) -> Vec<PanePresentationAction> {
        self.take_actions_in(0)
    }

    fn take_actions_in(&self, index: usize) -> Vec<PanePresentationAction> {
        std::mem::take(&mut *self.records[index].actions.borrow_mut())
    }
}

pub(in crate::ui) fn press(cx: &mut VisualTestContext, keys: &str) {
    for key in keys.split_whitespace() {
        let keystroke = gpui::Keystroke::parse(key).expect("a test keystroke parses");
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            prefer_character_input: false,
            is_held: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
    }
    cx.run_until_parked();
}

fn pane(presentation: PanePresentation, cx: &mut TestAppContext) -> Panes<'_> {
    panes(vec![presentation], px(640.0), cx)
}

/// Panes side by side in one window, the first one focused.
fn panes(
    presentations: Vec<PanePresentation>,
    width: Pixels,
    cx: &mut TestAppContext,
) -> Panes<'_> {
    cx.update(crate::ui::init).unwrap();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let panes = presentations
            .into_iter()
            .enumerate()
            .map(|(index, presentation)| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    PaneHarness {
                        index,
                        presentation_focus: PresentationFocus::new(focus.clone(), cx),
                        focus,
                        presentation,
                        terminal_background: Color::rgb(0x101418),
                        width,
                        height: px(400.0),
                        record: Rc::default(),
                    }
                })
            })
            .collect::<Vec<_>>();
        panes[0].read(cx).focus.clone().focus(window, cx);
        WindowHarness {
            outside: cx.focus_handle().tab_stop(true),
            panes,
        }
    });
    cx.run_until_parked();
    let (harnesses, outside) =
        root.read_with(cx, |root, _| (root.panes.clone(), root.outside.clone()));
    let (focuses, records) = harnesses
        .iter()
        .map(|harness| {
            harness.read_with(cx, |harness, _| {
                (harness.focus.clone(), Rc::clone(&harness.record))
            })
        })
        .unzip();
    Panes {
        harnesses,
        focuses,
        records,
        outside,
        cx,
    }
}

fn every_presentation() -> Vec<PanePresentation> {
    let device = DisplayName::reported("Studio Mac");
    vec![
        PanePresentation::Live,
        PanePresentation::AwaitingMeasurement,
        PanePresentation::Starting,
        PanePresentation::Takeover(device.clone()),
        PanePresentation::Ended {
            reason: SessionEndReason::EndAllSessions,
        },
        PanePresentation::Ended {
            reason: SessionEndReason::Restored,
        },
        PanePresentation::Failed {
            class: PaneFailureClass::Launch,
        },
        PanePresentation::Failed {
            class: PaneFailureClass::Interrupted,
        },
        PanePresentation::Failed {
            class: PaneFailureClass::ResourceLimit,
        },
        PanePresentation::Failed {
            class: PaneFailureClass::Unavailable,
        },
        PanePresentation::DirectoryUnavailable,
        PanePresentation::Offline {
            prior: OfflinePrior::Frozen,
        },
        PanePresentation::Offline {
            prior: OfflinePrior::Takeover(device.clone()),
        },
        PanePresentation::Reconciling {
            prior: OfflinePrior::Frozen,
        },
        PanePresentation::Reconciling {
            prior: OfflinePrior::Takeover(device),
        },
    ]
}

#[test]
fn only_a_live_pane_accepts_input_and_only_a_live_or_frozen_screen_shows_content() {
    for presentation in every_presentation() {
        let live = presentation == PanePresentation::Live;
        let frozen = matches!(
            presentation,
            PanePresentation::Offline {
                prior: OfflinePrior::Frozen
            } | PanePresentation::Reconciling {
                prior: OfflinePrior::Frozen
            }
        );
        assert_eq!(presentation.accepts_input(), live, "{presentation:?}");
        assert_eq!(
            presentation.content_eligible(),
            live || frozen,
            "{presentation:?}"
        );
        assert_eq!(presentation.freezes_content(), frozen, "{presentation:?}");
        // A content-free presentation draws its own screen; a live or frozen one draws none.
        assert_eq!(
            presentation.screen().is_some(),
            !presentation.content_eligible()
                && !matches!(
                    presentation,
                    PanePresentation::AwaitingMeasurement | PanePresentation::Starting
                ),
            "{presentation:?}"
        );
    }
}

#[test]
fn only_a_takeover_screen_offers_the_take_over_command() {
    for presentation in every_presentation() {
        let screen_offers_take_over = presentation.screen().is_some_and(|screen| {
            screen
                .actions
                .iter()
                .any(|action| action.action == PanePresentationAction::TakeOver && action.available)
        });
        assert_eq!(
            presentation.offers_take_over(),
            screen_offers_take_over,
            "{presentation:?}"
        );
    }
}

#[test]
fn reported_names_are_bounded_and_cannot_reorder_surrounding_text() {
    assert_eq!(
        DisplayName::reported("  Studio Mac  ").get().unwrap(),
        "Studio Mac"
    );
    assert_eq!(DisplayName::reported(" \t ").get(), None);
    assert_eq!(DisplayName::unnamed().get(), None);

    let spoofed = DisplayName::reported("Mac\u{202e}cipe\u{7}");
    let spoofed = spoofed.get().unwrap();
    assert!(!spoofed.contains('\u{202e}') && !spoofed.contains('\u{7}'));

    let long = DisplayName::reported(&"m".repeat(500));
    assert_eq!(
        long.get().unwrap().chars().count(),
        MAXIMUM_DISPLAY_NAME_CHARS
    );
}

#[test]
fn takeover_copy_names_the_controlling_device_or_says_another_device() {
    let named = PanePresentation::Takeover(DisplayName::reported("Studio Mac"))
        .screen()
        .unwrap();
    assert_eq!(named.title, "In use on Studio Mac");
    assert!(
        named
            .detail
            .contains("Studio Mac will show this screen instead.")
    );

    let unnamed = PanePresentation::Takeover(DisplayName::unnamed())
        .screen()
        .unwrap();
    assert_eq!(unnamed.title, "In use on another device");

    // An interrupted Takeover Screen keeps naming the device, and never offers Take Over until
    // the server can grant it.
    for presentation in [
        PanePresentation::Offline {
            prior: OfflinePrior::Takeover(DisplayName::reported("Studio Mac")),
        },
        PanePresentation::Reconciling {
            prior: OfflinePrior::Takeover(DisplayName::reported("Studio Mac")),
        },
    ] {
        let screen = presentation.screen().unwrap();
        assert_eq!(screen.title, "In use on Studio Mac");
        assert!(
            screen.actions.iter().any(
                |action| action.action == PanePresentationAction::TakeOver && !action.available
            ),
            "{presentation:?}"
        );
    }
}

#[test]
fn ended_screens_distinguish_end_all_sessions_from_a_restored_pane() {
    let ended = |reason| {
        PanePresentation::Ended { reason }
            .screen()
            .expect("an ended session has a screen")
    };
    let ended_all = ended(SessionEndReason::EndAllSessions);
    let restored = ended(SessionEndReason::Restored);
    assert_eq!(ended_all.title, "Session ended");
    assert_eq!(restored.title, "Session ended");
    assert_ne!(ended_all.detail, restored.detail);
    for screen in [ended_all, restored] {
        assert_eq!(
            screen.actions,
            [ScreenAction {
                action: PanePresentationAction::StartSession,
                prominence: Prominence::Primary,
                available: true,
            }]
        );
    }
}

/// Every replacement and its text stay readable when a program or theme picks the background.
#[test]
fn presentation_text_stays_readable_on_any_terminal_background_in_light_and_dark() {
    for appearance in [
        crate::appearance::Appearance::Light,
        crate::appearance::Appearance::Dark,
    ] {
        let chrome = prepared_appearance(appearance);
        for background in [
            Color::BLACK,
            Color::WHITE,
            Color::rgb(0x002b36),
            Color::rgb(0xfdf6e3),
            Color::rgb(0x38658a),
            Color::rgba(0x10141880),
        ] {
            let paint = PresentationPaint::resolve(&chrome, background);
            assert!(paint.surface.is_opaque(), "{appearance:?} {background:?}");
            for (text, minimum) in [
                (paint.title, 4.5),
                (paint.detail, 4.5),
                (paint.neutral, 3.0),
                (paint.warning, 3.0),
                (paint.error, 3.0),
            ] {
                let ratio = text
                    .source_over(paint.surface)
                    .contrast_ratio(paint.surface);
                assert!(
                    ratio >= minimum,
                    "{appearance:?} on {background:?}: {text:?} has {ratio:.2}"
                );
            }
        }
    }
}

pub(in crate::ui) fn prepared_appearance(
    appearance: crate::appearance::Appearance,
) -> ChromeAppearance {
    let preferences = crate::appearance::AppearancePreferences {
        mode: match appearance {
            crate::appearance::Appearance::Light => crate::appearance::AppearanceMode::Light,
            crate::appearance::Appearance::Dark => crate::appearance::AppearanceMode::Dark,
        },
        ..crate::appearance::AppearancePreferences::default()
    };
    let resolved = crate::appearance::ThemeCatalog::default()
        .resolve(
            crate::appearance::AppearanceGeneration::INITIAL,
            &preferences,
            crate::appearance::SystemAppearance::available(appearance)
                .with_composition(crate::appearance::CompositionCapabilities::new(true, true)),
            &crate::appearance::AvailableFonts::default(),
        )
        .expect("built-in appearance should resolve");
    ChromeAppearance::prepare(&resolved.chrome)
}

#[gpui::test]
fn startup_waits_before_showing_progress_and_measuring_then_starting_reveals_it_once(
    cx: &mut TestAppContext,
) {
    let mut pane = pane(PanePresentation::AwaitingMeasurement, cx);
    assert!(pane.shows("pane-presentation-pending"));
    assert!(!pane.shows("terminal"));
    assert!(!pane.shows("pane-presentation-progress"));
    assert!(A11yTree::read(pane.cx).find("Session status").is_none());

    pane.advance(Duration::from_millis(200));
    pane.present(PanePresentation::Starting);
    assert!(!pane.shows("pane-presentation-progress"));

    // Measuring and starting share one delay, so progress appears 300 ms after the Pane did.
    pane.advance(Duration::from_millis(100));
    assert!(pane.shows("pane-presentation-progress"));
    let tree = A11yTree::read(pane.cx);
    let status = tree.node("Session status");
    assert_eq!(status["aria"]["role"], "Status");
    assert_eq!(status["aria"]["value"], "Starting");

    pane.press("a enter");
    assert!(pane.record().terminal_input.borrow().is_empty());

    pane.present(PanePresentation::Live);
    assert!(!pane.shows("pane-presentation-pending"));
    assert!(pane.shows("terminal"));
    pane.press("a tab");
    assert_eq!(*pane.record().terminal_input.borrow(), ["a", "tab"]);
}

#[gpui::test]
fn takeover_screen_is_content_free_and_never_takes_over_from_terminal_typing(
    cx: &mut TestAppContext,
) {
    let mut pane = pane(
        PanePresentation::Takeover(DisplayName::reported("Studio Mac")),
        cx,
    );
    assert!(!pane.shows("terminal"));
    let pane_bounds = pane.cx.debug_bounds("pane").unwrap();
    assert_eq!(
        pane.cx.debug_bounds("pane-presentation-replacement"),
        Some(pane_bounds)
    );
    let tree = A11yTree::read(pane.cx);
    let screen = tree.node("In use on Studio Mac");
    assert_eq!(screen["aria"]["role"], "Group");
    assert_eq!(
        screen["aria"]["description"],
        "Take Over to use this Pane here. Studio Mac will show this screen instead."
    );
    assert_eq!(tree.node("Take Over")["aria"]["disabled"], true);

    // A click landing as the screen appears does nothing until the screen arms.
    pane.click("pane-presentation-take-over");
    assert!(pane.take_actions().is_empty());
    pane.advance(TAKEOVER_ARMING_DELAY);
    assert_ne!(
        A11yTree::read(pane.cx).node("Take Over")["aria"]["disabled"],
        true
    );

    // Typing meant for the Terminal, Return included, operates nothing and reaches nothing.
    pane.press("l s enter space");
    pane.cx.run_until_parked();
    assert!(pane.take_actions().is_empty());
    assert!(pane.record().terminal_input.borrow().is_empty());
    assert!(pane.pane_focused());

    // Tab completion followed by Return or Space, typed for the shell, never reaches Take Over,
    // and Tab never leaves the Pane for another control.
    for keys in [
        "tab enter",
        "tab space",
        "shift-tab enter",
        "shift-tab space",
        "tab tab enter",
    ] {
        pane.press(keys);
        assert!(pane.take_actions().is_empty(), "{keys}");
        assert!(pane.pane_focused(), "{keys}");
    }
    assert!(pane.record().terminal_input.borrow().is_empty());

    // Take Over is no tab stop for traversal that starts elsewhere in the window either.
    pane.focus_outside();
    pane.press("tab");
    assert_ne!(pane.focused_label().as_deref(), Some("Take Over"));
    pane.focus_pane();

    // A pointer or assistive technology takes over, and focus stays with the Pane.
    pane.click("pane-presentation-take-over");
    assert!(pane.pane_focused());
    let tree = A11yTree::read(pane.cx);
    perform(pane.cx, tree.node("Take Over"), accesskit::Action::Click);
    pane.cx.run_until_parked();
    assert_eq!(
        pane.take_actions(),
        [
            PanePresentationAction::TakeOver,
            PanePresentationAction::TakeOver
        ]
    );
    assert!(pane.pane_focused());
}

#[gpui::test]
fn an_interrupted_takeover_screen_keeps_tab_and_return_from_its_actions(cx: &mut TestAppContext) {
    let device = DisplayName::reported("Studio Mac");
    let mut pane = pane(PanePresentation::Live, cx);
    for presentation in [
        PanePresentation::Offline {
            prior: OfflinePrior::Takeover(device.clone()),
        },
        PanePresentation::Reconciling {
            prior: OfflinePrior::Takeover(device.clone()),
        },
    ] {
        pane.present(presentation.clone());
        pane.advance(TAKEOVER_ARMING_DELAY);
        for keys in ["tab enter", "tab space", "shift-tab enter"] {
            pane.press(keys);
            assert!(pane.take_actions().is_empty(), "{presentation:?} {keys}");
            assert!(pane.pane_focused(), "{presentation:?} {keys}");
        }
    }
    assert!(pane.record().terminal_input.borrow().is_empty());
}

#[gpui::test]
fn takeover_rearms_when_the_screen_returns(cx: &mut TestAppContext) {
    let device = PanePresentation::Takeover(DisplayName::unnamed());
    let mut pane = pane(device.clone(), cx);
    pane.advance(TAKEOVER_ARMING_DELAY);
    pane.present(PanePresentation::Live);
    pane.present(device);
    assert_eq!(
        A11yTree::read(pane.cx).node("Take Over")["aria"]["disabled"],
        true
    );
    pane.click("pane-presentation-take-over");
    assert!(pane.take_actions().is_empty());
}

#[gpui::test]
fn each_screen_offers_and_reports_its_own_actions(cx: &mut TestAppContext) {
    let mut pane = pane(PanePresentation::Live, cx);
    for (presentation, title, enabled, disabled) in [
        (
            PanePresentation::Ended {
                reason: SessionEndReason::EndAllSessions,
            },
            "Session ended",
            vec![PanePresentationAction::StartSession],
            vec![],
        ),
        (
            PanePresentation::Ended {
                reason: SessionEndReason::Restored,
            },
            "Session ended",
            vec![PanePresentationAction::StartSession],
            vec![],
        ),
        (
            PanePresentation::Failed {
                class: PaneFailureClass::Launch,
            },
            "The shell couldn't start",
            vec![PanePresentationAction::Retry],
            vec![],
        ),
        (
            PanePresentation::Failed {
                class: PaneFailureClass::Interrupted,
            },
            "Session stopped unexpectedly",
            vec![PanePresentationAction::Retry],
            vec![],
        ),
        (
            PanePresentation::Failed {
                class: PaneFailureClass::ResourceLimit,
            },
            "Server limit reached",
            vec![PanePresentationAction::Retry],
            vec![],
        ),
        (
            PanePresentation::Failed {
                class: PaneFailureClass::Unavailable,
            },
            "Session unavailable",
            vec![PanePresentationAction::Retry],
            vec![],
        ),
        (
            PanePresentation::DirectoryUnavailable,
            "Directory unavailable",
            vec![
                PanePresentationAction::Retry,
                PanePresentationAction::ChooseDirectory,
            ],
            vec![],
        ),
        (
            PanePresentation::Offline {
                prior: OfflinePrior::Takeover(DisplayName::reported("Studio Mac")),
            },
            "In use on Studio Mac",
            vec![PanePresentationAction::Retry],
            vec![PanePresentationAction::TakeOver],
        ),
        (
            PanePresentation::Reconciling {
                prior: OfflinePrior::Takeover(DisplayName::reported("Studio Mac")),
            },
            "In use on Studio Mac",
            vec![],
            vec![PanePresentationAction::TakeOver],
        ),
    ] {
        pane.present(presentation.clone());
        assert!(!pane.shows("terminal"), "{presentation:?}");
        let tree = A11yTree::read(pane.cx);
        assert_eq!(
            tree.node(title)["aria"]["role"],
            "Group",
            "{presentation:?}"
        );
        for action in &disabled {
            assert_eq!(
                tree.node(action.label())["aria"]["disabled"],
                true,
                "{presentation:?}"
            );
            pane.click(action.selector());
        }
        assert!(pane.take_actions().is_empty(), "{presentation:?}");
        for action in &enabled {
            assert_ne!(
                tree.node(action.label())["aria"]["disabled"],
                true,
                "{presentation:?}"
            );
            pane.click(action.selector());
        }
        assert_eq!(pane.take_actions(), enabled, "{presentation:?}");
        pane.press("enter");
        assert!(pane.take_actions().is_empty(), "{presentation:?}");
        assert!(
            pane.record().terminal_input.borrow().is_empty(),
            "{presentation:?}"
        );
    }
}

#[gpui::test]
fn an_offline_pane_freezes_its_screen_under_a_non_modal_retry(cx: &mut TestAppContext) {
    let mut pane = pane(
        PanePresentation::Offline {
            prior: OfflinePrior::Frozen,
        },
        cx,
    );
    // The last screen stays and stays selectable under the scrim.
    assert!(pane.shows("terminal"));
    assert!(pane.shows("pane-presentation-scrim"));
    assert!(!pane.shows("pane-presentation-replacement"));
    let pane_bounds = pane.cx.debug_bounds("pane").unwrap();
    pane.cx.simulate_click(
        pane_bounds.origin + gpui::point(px(40.0), px(40.0)),
        Modifiers::none(),
    );
    assert_eq!(pane.record().terminal_clicks.get(), 1);

    let tree = A11yTree::read(pane.cx);
    let status = tree.node("Connection status");
    assert_eq!(status["aria"]["role"], "Status");
    assert_eq!(status["aria"]["value"], "Offline");

    pane.press("a enter");
    assert!(pane.record().terminal_input.borrow().is_empty());
    assert!(pane.take_actions().is_empty());
    pane.click("pane-presentation-retry");
    assert_eq!(pane.take_actions(), [PanePresentationAction::Retry]);
    pane.press("tab enter");
    assert_eq!(pane.take_actions(), [PanePresentationAction::Retry]);
    assert!(pane.pane_focused());

    // Reconciling keeps the screen frozen and offers nothing until the Pane is live again.
    pane.present(PanePresentation::Reconciling {
        prior: OfflinePrior::Frozen,
    });
    assert!(pane.shows("terminal"));
    assert!(pane.shows("pane-presentation-reconciling"));
    assert!(!pane.shows("pane-presentation-retry"));
    assert_eq!(
        A11yTree::read(pane.cx).node("Connection status")["aria"]["value"],
        "Reconnecting"
    );
    pane.press("tab");
    assert!(pane.pane_focused());
    assert!(pane.record().terminal_input.borrow().is_empty());
}

#[gpui::test]
fn a_short_pane_scrolls_its_screen_from_the_top_instead_of_clipping_its_actions(
    cx: &mut TestAppContext,
) {
    let mut pane = pane(PanePresentation::Takeover(DisplayName::unnamed()), cx);
    pane.harnesses[0].update(pane.cx, |harness, cx| {
        harness.height = px(96.0);
        cx.notify();
    });
    pane.advance(TAKEOVER_ARMING_DELAY);
    let replacement = pane
        .cx
        .debug_bounds("pane-presentation-replacement")
        .unwrap();
    assert_eq!(replacement.size.height, px(96.0));
    let screen = pane.cx.debug_bounds("pane-presentation-screen").unwrap();
    assert!(
        screen.top() >= replacement.top(),
        "{screen:?} {replacement:?}"
    );
    let take_over = pane.cx.debug_bounds("pane-presentation-take-over").unwrap();
    assert!(take_over.bottom() > replacement.bottom());

    pane.cx.simulate_event(gpui::ScrollWheelEvent {
        position: replacement.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-400.0))),
        ..Default::default()
    });
    pane.cx.run_until_parked();
    let take_over = pane.cx.debug_bounds("pane-presentation-take-over").unwrap();
    assert!(
        take_over.bottom() <= replacement.bottom(),
        "{take_over:?} {replacement:?}"
    );
    assert!(
        take_over.top() >= replacement.top(),
        "{take_over:?} {replacement:?}"
    );
    // Scrolling the screen never reaches the Terminal.
    assert!(pane.record().terminal_input.borrow().is_empty());
    assert_eq!(pane.record().scrolls.get(), 0);
}

#[gpui::test]
fn a_press_on_a_screen_selects_and_focuses_its_pane(cx: &mut TestAppContext) {
    let device = DisplayName::reported("Studio Mac");
    let mut pane = pane(PanePresentation::Live, cx);
    for presentation in [
        PanePresentation::AwaitingMeasurement,
        PanePresentation::Starting,
        PanePresentation::Takeover(device.clone()),
        PanePresentation::Ended {
            reason: SessionEndReason::Restored,
        },
        PanePresentation::Failed {
            class: PaneFailureClass::Launch,
        },
        PanePresentation::DirectoryUnavailable,
        PanePresentation::Offline {
            prior: OfflinePrior::Takeover(device.clone()),
        },
        PanePresentation::Reconciling {
            prior: OfflinePrior::Takeover(device.clone()),
        },
    ] {
        pane.present(presentation.clone());
        pane.advance(TAKEOVER_ARMING_DELAY);
        pane.focus_outside();
        assert!(!pane.pane_focused(), "{presentation:?}");
        let selections = pane.record().selections.get();
        // The screen's background, clear of its symbol, text, and actions.
        let background = pane.cx.debug_bounds("pane").unwrap().origin + point(px(6.0), px(6.0));
        pane.cx.simulate_click(background, Modifiers::none());
        pane.cx.run_until_parked();
        assert!(pane.pane_focused(), "{presentation:?}");
        assert_eq!(
            pane.record().selections.get(),
            selections + 1,
            "{presentation:?}"
        );
        assert!(pane.take_actions().is_empty(), "{presentation:?}");
        assert_eq!(pane.record().terminal_clicks.get(), 0, "{presentation:?}");

        // Scrolling a screen never scrolls the Terminal behind it.
        pane.cx.simulate_event(gpui::ScrollWheelEvent {
            position: background,
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-40.0))),
            ..Default::default()
        });
        pane.cx.run_until_parked();
        assert_eq!(pane.record().scrolls.get(), 0, "{presentation:?}");
    }
}

#[gpui::test]
fn tab_cycles_within_its_own_pane_and_never_reaches_a_neighbor(cx: &mut TestAppContext) {
    let mut panes = panes(
        vec![
            PanePresentation::Failed {
                class: PaneFailureClass::Launch,
            },
            PanePresentation::Ended {
                reason: SessionEndReason::EndAllSessions,
            },
        ],
        px(320.0),
        cx,
    );
    assert!(panes.focused_in(0));
    panes.press("tab");
    assert_eq!(panes.focused_label().as_deref(), Some("Retry"));
    panes.press("tab");
    assert!(panes.focused_in(0));
    panes.press("shift-tab");
    assert_eq!(panes.focused_label().as_deref(), Some("Retry"));
    panes.press("shift-tab");
    assert!(panes.focused_in(0));
    panes.press("tab enter");
    assert_eq!(panes.take_actions_in(0), [PanePresentationAction::Retry]);
    assert!(panes.take_actions_in(1).is_empty());
    assert!(panes.focused_in(0));

    // While a neighbor's action is available and this Pane's Take Over is not yet, Tab stays put.
    panes.present_in(0, PanePresentation::Takeover(DisplayName::unnamed()));
    for keys in ["tab enter", "shift-tab enter"] {
        panes.press(keys);
        assert!(panes.focused_in(0), "{keys}");
        assert!(panes.take_actions_in(0).is_empty(), "{keys}");
        assert!(panes.take_actions_in(1).is_empty(), "{keys}");
    }

    // A Pane with two actions visits both, in presentation order, then returns to itself.
    panes.present_in(0, PanePresentation::DirectoryUnavailable);
    panes.press("tab");
    assert_eq!(panes.focused_label().as_deref(), Some("Retry"));
    panes.press("tab");
    assert_eq!(panes.focused_label().as_deref(), Some("Choose Directory…"));
    panes.press("tab");
    assert!(panes.focused_in(0));
    panes.press("shift-tab");
    assert_eq!(panes.focused_label().as_deref(), Some("Choose Directory…"));
    panes.press("escape");
    assert!(panes.focused_in(0));
}

#[gpui::test]
fn the_pane_takes_focus_back_when_its_focused_action_goes_away(cx: &mut TestAppContext) {
    let mut pane = pane(
        PanePresentation::Offline {
            prior: OfflinePrior::Frozen,
        },
        cx,
    );
    pane.press("tab");
    assert_eq!(pane.focused_label().as_deref(), Some("Retry"));
    // Reconnecting takes Retry away; the Pane keeps keyboard focus through reconciling to live.
    pane.present(PanePresentation::Reconciling {
        prior: OfflinePrior::Frozen,
    });
    assert!(pane.pane_focused());
    pane.present(PanePresentation::Live);
    assert!(pane.pane_focused());
    pane.press("a");
    assert_eq!(*pane.record().terminal_input.borrow(), ["a"]);

    // An action the next screen also offers keeps its focus.
    pane.present(PanePresentation::Failed {
        class: PaneFailureClass::Interrupted,
    });
    pane.press("tab");
    assert_eq!(pane.focused_label().as_deref(), Some("Retry"));
    pane.present(PanePresentation::DirectoryUnavailable);
    assert_eq!(pane.focused_label().as_deref(), Some("Retry"));

    // A screen going live under a focused action gives the Pane its keys again.
    pane.press("tab");
    assert_eq!(pane.focused_label().as_deref(), Some("Choose Directory…"));
    pane.present(PanePresentation::Live);
    assert!(pane.pane_focused());
    pane.press("b");
    assert_eq!(*pane.record().terminal_input.borrow(), ["a", "b"]);

    // Assistive technology can focus Take Over. Tab from it returns to the Pane, and the Pane takes
    // focus back when the server goes offline and Take Over becomes unavailable.
    pane.present(PanePresentation::Takeover(DisplayName::unnamed()));
    pane.advance(TAKEOVER_ARMING_DELAY);
    let focus_take_over = |pane: &mut Panes| {
        let tree = A11yTree::read(pane.cx);
        perform(pane.cx, tree.node("Take Over"), accesskit::Action::Focus);
        pane.cx.run_until_parked();
        assert_eq!(pane.focused_label().as_deref(), Some("Take Over"));
    };
    focus_take_over(&mut pane);
    pane.press("tab");
    assert!(pane.pane_focused());
    focus_take_over(&mut pane);
    pane.present(PanePresentation::Offline {
        prior: OfflinePrior::Takeover(DisplayName::unnamed()),
    });
    assert!(pane.pane_focused());
    assert!(pane.take_actions().is_empty());
}
