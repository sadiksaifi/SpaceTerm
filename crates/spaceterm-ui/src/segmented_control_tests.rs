use std::{cell::RefCell, rc::Rc};

use gpui::{
    Context, Entity, FocusHandle, Modifiers, Render, TestAppContext, VisualTestContext, Window,
};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Light,
    Dark,
    Auto,
}

fn test_theme() -> SegmentedControlTheme {
    let values = SegmentedValuePaints::new(
        SegmentedPaint::new(rgba(0x00000000), rgba(0xd0d0d0ff), rgba(0x00000000)),
        SegmentedPaint::new(rgba(0x2277ddff), rgba(0xffffffff), rgba(0x2277ddff)),
    );
    let disabled = SegmentedValuePaints::new(
        SegmentedPaint::new(rgba(0x00000000), rgba(0x80808080), rgba(0x00000000)),
        SegmentedPaint::new(rgba(0x2277dd80), rgba(0xffffff80), rgba(0x2277dd80)),
    );
    SegmentedControlTheme::new(
        SegmentedPaints::new(values, values, values, disabled),
        SegmentedSizes::new(
            SegmentedMetrics::new(px(24.0), px(0.0), px(56.0), px(0.0)),
            SegmentedMetrics::new(px(28.0), px(52.0), px(84.0), px(10.0)),
        ),
        rgba(0x18181880),
        rgba(0x303030ff),
        rgba(0x00aaffff),
    )
}

struct TestRoot {
    current: Mode,
    size: SegmentedSize,
    disabled: bool,
    disable_auto: bool,
    omit_auto: bool,
    right_to_left: bool,
    changes: Rc<RefCell<Vec<SegmentedChange<Mode>>>>,
    other_focus: FocusHandle,
}

impl Render for TestRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let changes = Rc::clone(&self.changes);
        let control = SegmentedControl::new(
            "test-segmented",
            "Appearance",
            &self.current,
            vec![
                SegmentedOption::new(Mode::Light, "Light")
                    .debug_selector("test-segmented-light")
                    .preview(|color, extent| {
                        div().w(extent).h(extent).bg(color).into_any_element()
                    }),
                SegmentedOption::new(Mode::Dark, "Dark").debug_selector("test-segmented-dark"),
            ]
            .into_iter()
            .chain((!self.omit_auto).then(|| {
                SegmentedOption::new(Mode::Auto, "Auto")
                    .debug_selector("test-segmented-auto")
                    .disabled(self.disable_auto)
            }))
            .collect(),
        )
        .expect("three options are within the bounded option set")
        .size(self.size)
        .disabled(self.disabled)
        .right_to_left(self.right_to_left)
        .debug_selector("test-segmented")
        .on_change(move |change, _, _| changes.borrow_mut().push(change.clone()));
        div()
            .flex()
            .flex_col()
            .child(div().track_focus(&self.other_focus).child("Other"))
            .child(control)
    }
}

type SegmentedWindow<'a> = (
    Entity<TestRoot>,
    Rc<RefCell<Vec<SegmentedChange<Mode>>>>,
    &'a mut VisualTestContext,
);

fn segmented_window(cx: &mut TestAppContext) -> SegmentedWindow<'_> {
    cx.set_global(test_theme());
    let changes = Rc::new(RefCell::new(Vec::new()));
    let root_changes = Rc::clone(&changes);
    let (root, cx) = cx.add_window_view(move |_, cx| TestRoot {
        current: Mode::Dark,
        size: SegmentedSize::Regular,
        disabled: false,
        disable_auto: false,
        omit_auto: false,
        right_to_left: false,
        changes: root_changes,
        other_focus: cx.focus_handle().tab_stop(true),
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (root, changes, cx)
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

fn focus_control(cx: &mut VisualTestContext) {
    // Advance the framework's tab-stop order so the control is reached the way keyboard traversal
    // reaches it, rather than by focusing an internal handle directly. The sibling stop is first.
    cx.update(|window, _| {
        window.focus_next();
        window.focus_next();
    });
    cx.run_until_parked();
}

#[test]
fn an_empty_option_set_is_rejected() {
    let result = SegmentedControl::new("id", "Appearance", &Mode::Light, Vec::new());

    assert_eq!(result.err(), Some(SegmentedBuildError::EmptyOptions));
}

#[test]
fn an_option_set_beyond_the_bound_is_rejected() {
    let options = (0..=MAXIMUM_SEGMENTED_OPTIONS)
        .map(|index| SegmentedOption::new(index, "Option"))
        .collect::<Vec<_>>();

    let result = SegmentedControl::new("id", "Appearance", &0, options);

    assert_eq!(result.err(), Some(SegmentedBuildError::TooManyOptions));
}

#[test]
fn navigation_skips_disabled_options_in_both_directions() {
    let navigable = vec![Some(Mode::Light), None, Some(Mode::Auto)];

    assert_eq!(step(&navigable, Some(0), true), Some(2));
    assert_eq!(step(&navigable, Some(2), false), Some(0));
    assert_eq!(step(&navigable, Some(2), true), None);
    assert_eq!(step(&navigable, Some(0), false), None);
    assert_eq!(boundary(&navigable, true), Some(0));
    assert_eq!(boundary(&navigable, false), Some(2));
}

#[test]
fn navigation_from_an_unmatched_value_enters_the_set_from_its_first_enabled_option() {
    let navigable = vec![None, Some(Mode::Dark), Some(Mode::Auto)];

    assert_eq!(step(&navigable, None, true), Some(1));
    assert_eq!(step(&navigable, None, false), Some(1));
}

#[test]
fn navigation_over_a_fully_disabled_set_reaches_nothing() {
    let navigable: Vec<Option<Mode>> = vec![None, None];

    assert_eq!(step(&navigable, None, true), None);
    assert_eq!(boundary(&navigable, true), None);
    assert_eq!(boundary(&navigable, false), None);
}

#[test]
fn interaction_paint_refines_every_rendered_part_of_a_segment() {
    let paint = SegmentedPaint::new(rgba(0x111111ff), rgba(0x121212ff), rgba(0x131313ff));

    assert_eq!(
        SegmentedPaintRefinement(paint).segment(StyleRefinement::default()),
        StyleRefinement::default()
            .bg(paint.background())
            .border_color(paint.border())
            .text_color(paint.label())
    );
}

#[gpui::test]
fn clicking_an_unselected_option_requests_its_value(cx: &mut TestAppContext) {
    let (_root, changes, cx) = segmented_window(cx);

    click("test-segmented-light", cx);

    let changes = changes.borrow();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].previous(), Some(&Mode::Dark));
    assert_eq!(changes[0].requested(), &Mode::Light);
    assert_eq!(changes[0].source(), SegmentedActivationSource::Pointer);
}

#[gpui::test]
fn clicking_the_selected_option_requests_nothing(cx: &mut TestAppContext) {
    let (_root, changes, cx) = segmented_window(cx);

    click("test-segmented-dark", cx);

    assert!(changes.borrow().is_empty());
}

#[gpui::test]
fn arrow_keys_move_the_selection_and_stop_at_both_ends(cx: &mut TestAppContext) {
    let (root, changes, cx) = segmented_window(cx);
    focus_control(cx);

    cx.simulate_keystrokes("right");
    cx.run_until_parked();
    cx.simulate_keystrokes("left");
    cx.run_until_parked();

    {
        let requests = changes.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].requested(), &Mode::Auto);
        assert_eq!(requests[0].source(), SegmentedActivationSource::Arrow);
        assert_eq!(requests[1].requested(), &Mode::Light);
    }

    // The caller still renders Dark, so Left from the middle reaches Light and Left again is the
    // start of the set.
    changes.borrow_mut().clear();
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.current = Mode::Light;
            cx.notify();
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("left");
    cx.run_until_parked();

    assert!(changes.borrow().is_empty());
}

#[gpui::test]
fn home_and_end_reach_the_first_and_last_enabled_options(cx: &mut TestAppContext) {
    let (_root, changes, cx) = segmented_window(cx);
    focus_control(cx);

    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    cx.simulate_keystrokes("home");
    cx.run_until_parked();

    let requests = changes.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].requested(), &Mode::Auto);
    assert_eq!(requests[0].source(), SegmentedActivationSource::Boundary);
    assert_eq!(requests[1].requested(), &Mode::Light);
    assert_eq!(requests[1].source(), SegmentedActivationSource::Boundary);
}

#[gpui::test]
fn arrow_keys_skip_a_disabled_option(cx: &mut TestAppContext) {
    let (root, changes, cx) = segmented_window(cx);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.disable_auto = true;
            cx.notify();
        })
    });
    cx.run_until_parked();
    focus_control(cx);

    cx.simulate_keystrokes("right");
    cx.run_until_parked();

    assert!(changes.borrow().is_empty());
}

#[gpui::test]
fn clicking_a_disabled_option_requests_nothing(cx: &mut TestAppContext) {
    let (root, changes, cx) = segmented_window(cx);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.disable_auto = true;
            cx.notify();
        })
    });
    cx.run_until_parked();

    click("test-segmented-auto", cx);

    assert!(changes.borrow().is_empty());
}

#[gpui::test]
fn a_right_to_left_layout_mirrors_arrow_direction(cx: &mut TestAppContext) {
    let (root, changes, cx) = segmented_window(cx);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.right_to_left = true;
            cx.notify();
        })
    });
    cx.run_until_parked();
    focus_control(cx);

    cx.simulate_keystrokes("right");
    cx.run_until_parked();

    let requests = changes.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].requested(), &Mode::Light);
}

#[gpui::test]
fn a_disabled_control_refuses_pointer_and_keyboard_activation(cx: &mut TestAppContext) {
    let (root, changes, cx) = segmented_window(cx);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        })
    });
    cx.run_until_parked();

    click("test-segmented-light", cx);
    cx.simulate_keystrokes("right");
    cx.run_until_parked();

    assert!(changes.borrow().is_empty());
}

#[gpui::test]
fn keyboard_focus_draws_one_outset_ring_around_the_whole_control(cx: &mut TestAppContext) {
    let (_root, _changes, cx) = segmented_window(cx);

    focus_control(cx);

    let track = cx
        .debug_bounds("test-segmented")
        .expect("the control should render");
    let ring = cx
        .debug_bounds("test-segmented-keyboard-focus")
        .expect("keyboard focus should draw a ring");
    assert!(
        ring.left() < track.left()
            && ring.right() > track.right()
            && ring.top() < track.top()
            && ring.bottom() > track.bottom(),
        "the ring should enclose the track"
    );
}

#[gpui::test]
fn pointer_activation_does_not_draw_the_keyboard_focus_ring(cx: &mut TestAppContext) {
    let (_root, _changes, cx) = segmented_window(cx);

    click("test-segmented-light", cx);

    // GPUI retains debug bounds across frames, so this can only assert that the ring was never
    // painted. A pointer press therefore must not paint it even once.
    assert_eq!(cx.debug_bounds("test-segmented-keyboard-focus"), None);
}

#[gpui::test]
fn card_presentation_draws_every_option_and_its_preview(cx: &mut TestAppContext) {
    let (root, _changes, cx) = segmented_window(cx);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.size = SegmentedSize::Card;
            cx.notify();
        })
    });
    cx.run_until_parked();

    for selector in [
        "test-segmented-light",
        "test-segmented-dark",
        "test-segmented-auto",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} was not rendered"
        );
    }
    let light = cx.debug_bounds("test-segmented-light").unwrap();
    let dark = cx.debug_bounds("test-segmented-dark").unwrap();
    assert!(
        light.size.height > px(52.0),
        "a card option should reserve space for its preview"
    );
    assert!(
        dark.origin.x > light.origin.x,
        "card options should be laid out in order"
    );
}

#[gpui::test]
fn a_value_matching_no_option_requests_the_chosen_value_without_a_previous(
    cx: &mut TestAppContext,
) {
    cx.set_global(test_theme());
    let changes = Rc::new(RefCell::new(Vec::new()));
    let root_changes = Rc::clone(&changes);
    let (_root, cx) = cx.add_window_view(move |_, cx| TestRoot {
        // Auto is omitted below, so the current value matches no option.
        current: Mode::Auto,
        size: SegmentedSize::Regular,
        disabled: false,
        disable_auto: false,
        omit_auto: true,
        right_to_left: false,
        changes: root_changes,
        other_focus: cx.focus_handle().tab_stop(true),
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    click("test-segmented-light", cx);

    let requests = changes.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].previous(), None);
    assert_eq!(requests[0].requested(), &Mode::Light);
}

#[test]
fn scaling_metrics_grows_text_and_spacing_independently() {
    let theme = test_theme();

    let scaled = theme.scaled_metrics(2.0, 1.0);

    let base = theme.sizes.resolve(SegmentedSize::Card);
    let grown = scaled.sizes.resolve(SegmentedSize::Card);
    assert_eq!(grown.font_size, base.font_size * 2.0);
    assert_eq!(grown.preview_height, base.preview_height);
    assert!(grown.option_height > base.option_height);
    assert_eq!(grown.border_width, base.border_width);
}
