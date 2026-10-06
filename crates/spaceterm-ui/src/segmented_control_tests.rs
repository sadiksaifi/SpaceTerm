use std::{cell::RefCell, rc::Rc};

use gpui::{
    Context, Entity, FocusHandle, Modifiers, Render, TestAppContext, VisualTestContext, Window,
};

use super::*;

#[test]
fn density_scales_segment_bounds_but_not_radius() {
    let original = test_theme().resolve(SegmentedSize::Regular).metrics;
    let comfortable = test_theme()
        .scaled_spacing(1.25)
        .resolve(SegmentedSize::Regular)
        .metrics;

    assert!(comfortable.option_height > original.option_height);
    assert_eq!(comfortable.radius, original.radius);
}

#[test]
fn elevation_belongs_to_the_segmented_track_and_selected_chip_border() {
    let shadow = ControlShadow::single(crate::ControlShadowLayer::new(
        rgba(0x11111159).into(),
        px(0.0),
        px(1.0),
        px(2.0),
        px(-1.0),
    ));
    let track_border = rgba(0x00000026);
    let selected_border = rgba(0x00000026);
    let theme = test_theme().track_elevation(shadow, Some(track_border), Some(selected_border));
    let style = theme.resolve(SegmentedSize::Regular);

    assert_eq!(style.track_shadow, shadow);
    assert_eq!(style.track_border, track_border);
    assert_eq!(style.selected_shadow, ControlShadow::none());
    for paints in [
        theme.paints.normal,
        theme.paints.hovered,
        theme.paints.pressed,
        theme.paints.disabled,
    ] {
        assert_eq!(paints.selected.border, selected_border);
        assert_eq!(paints.unselected.border, rgba(0x00000000));
    }
}

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

#[derive(Clone, Copy, Debug)]
enum SizingHost {
    Block,
    Row,
    Column,
}

struct SizingRoot {
    host: SizingHost,
    host_width: Pixels,
    full_width: bool,
    size: SegmentedSize,
    right_to_left: bool,
}

impl Render for SizingRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "sizing-host".to_owned())
            .w(self.host_width)
            .when(matches!(self.host, SizingHost::Row), |host| host.flex())
            .when(matches!(self.host, SizingHost::Column), |host| {
                host.flex().flex_col()
            })
            .child(
                SegmentedControl::new(
                    "sizing-control",
                    "Selection",
                    &true,
                    vec![
                        SegmentedOption::new(false, "Off").debug_selector("sizing-off"),
                        SegmentedOption::new(true, "Comfortable").debug_selector("sizing-on"),
                    ],
                )
                .unwrap()
                .size(self.size)
                .full_width(self.full_width)
                .right_to_left(self.right_to_left)
                .debug_selector("sizing-control")
                .on_change(|_, _, _| {}),
            )
    }
}

#[gpui::test]
fn intrinsic_track_hugs_options_in_every_parent_layout(cx: &mut TestAppContext) {
    for scale in [1.0, 1.25] {
        cx.set_global(test_theme().scaled_spacing(scale));
        for host in [SizingHost::Block, SizingHost::Row, SizingHost::Column] {
            for size in [SegmentedSize::Regular, SegmentedSize::Card] {
                for right_to_left in [false, true] {
                    let (_, cx) = cx.add_window_view(move |_, _| SizingRoot {
                        host,
                        host_width: px(320.0),
                        full_width: false,
                        size,
                        right_to_left,
                    });
                    cx.run_until_parked();
                    let track = cx.debug_bounds("sizing-control").unwrap();
                    let off = cx.debug_bounds("sizing-off").unwrap();
                    let on = cx.debug_bounds("sizing-on").unwrap();
                    let left = off.left().min(on.left()) - track.left();
                    let right = track.right() - off.right().max(on.right());
                    let inset = if size == SegmentedSize::Regular {
                        px(2.0)
                    } else {
                        px(0.0)
                    };
                    assert_eq!(
                        (left, right),
                        (inset, inset),
                        "intrinsic track must not retain parent slack: {host:?}/{size:?}/rtl={right_to_left}/scale={scale}"
                    );
                }
            }
        }
    }
}

#[gpui::test]
fn full_width_track_distributes_all_available_width_between_options(cx: &mut TestAppContext) {
    for scale in [1.0, 1.25] {
        cx.set_global(test_theme().scaled_spacing(scale));
        for host in [SizingHost::Block, SizingHost::Row, SizingHost::Column] {
            for size in [SegmentedSize::Regular, SegmentedSize::Card] {
                for right_to_left in [false, true] {
                    let (_, cx) = cx.add_window_view(move |_, _| SizingRoot {
                        host,
                        host_width: px(320.0),
                        full_width: true,
                        size,
                        right_to_left,
                    });
                    cx.run_until_parked();
                    let host_bounds = cx.debug_bounds("sizing-host").unwrap();
                    let track = cx.debug_bounds("sizing-control").unwrap();
                    let off = cx.debug_bounds("sizing-off").unwrap();
                    let on = cx.debug_bounds("sizing-on").unwrap();
                    let left = off.left().min(on.left()) - track.left();
                    let right = track.right() - off.right().max(on.right());
                    let inset = if size == SegmentedSize::Regular {
                        px(2.0)
                    } else {
                        px(0.0)
                    };
                    assert_eq!(track.size.width, host_bounds.size.width);
                    let device_pixel = cx.update(|window, _| px(1.0 / window.scale_factor()));
                    assert!(
                        (off.size.width - on.size.width).abs() <= device_pixel,
                        "equal segments may differ only by device-pixel rounding: {off:?}, {on:?}"
                    );
                    assert_eq!(
                        (left, right),
                        (inset, inset),
                        "full-width track must have symmetric insets: {host:?}/{size:?}/rtl={right_to_left}/scale={scale}"
                    );
                    cx.update(|window, cx| {
                        window.activate_window();
                        window.focus_next(cx);
                    });
                    cx.run_until_parked();
                    assert_eq!(cx.debug_bounds("sizing-control").unwrap(), track);
                    assert_eq!(cx.debug_bounds("sizing-on").unwrap(), on);
                    let ring = cx.debug_bounds("sizing-control-keyboard-focus").unwrap();
                    assert_eq!(on.left() - ring.left(), ring.right() - on.right());
                }
            }
        }
    }
}

#[gpui::test]
fn narrow_full_width_track_contains_equal_options_and_their_labels(cx: &mut TestAppContext) {
    cx.set_global(test_theme());
    for size in [SegmentedSize::Regular, SegmentedSize::Card] {
        let (_, cx) = cx.add_window_view(move |_, _| SizingRoot {
            host: SizingHost::Block,
            host_width: px(40.0),
            full_width: true,
            size,
            right_to_left: false,
        });
        cx.run_until_parked();
        let track = cx.debug_bounds("sizing-control").unwrap();
        let off = cx.debug_bounds("sizing-off").unwrap();
        let on = cx.debug_bounds("sizing-on").unwrap();
        let label = cx.debug_bounds("sizing-on-label").unwrap();
        assert_eq!(off.size.width, on.size.width);
        assert!(
            track.right() >= on.right(),
            "track {track:?} must contain both options, including {on:?}"
        );
        assert!(
            on.left() <= label.left() && label.right() <= on.right(),
            "option {on:?} must contain label {label:?}"
        );
    }
}

struct TestRoot {
    current: Mode,
    size: SegmentedSize,
    disabled: bool,
    disable_auto: bool,
    disable_dark: bool,
    omit_auto: bool,
    right_to_left: bool,
    changes: Rc<RefCell<Vec<SegmentedChange<Mode>>>>,
    other_focus: FocusHandle,
    container_focus: FocusHandle,
    preview_font: Rc<RefCell<Option<gpui::Font>>>,
}

impl Render for TestRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let changes = Rc::clone(&self.changes);
        let preview_font = Rc::clone(&self.preview_font);
        let control = SegmentedControl::new(
            "test-segmented",
            "Appearance",
            &self.current,
            vec![
                SegmentedOption::new(Mode::Light, "Light")
                    .debug_selector("test-segmented-light")
                    .preview(move |color, extent| {
                        let preview_font = Rc::clone(&preview_font);
                        div()
                            .debug_selector(|| "test-segmented-light-preview".to_owned())
                            .w(extent)
                            .h(extent)
                            .bg(color)
                            .child(
                                gpui::canvas(
                                    move |_, window, _| {
                                        *preview_font.borrow_mut() =
                                            Some(window.text_style().font());
                                    },
                                    |_, _, _, _| {},
                                )
                                .size_full(),
                            )
                            .into_any_element()
                    }),
                SegmentedOption::new(Mode::Dark, "Dark")
                    .debug_selector("test-segmented-dark")
                    .disabled(self.disable_dark),
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
            .id("test-segmented-container")
            .role(accesskit::Role::Group)
            .aria_label("Container")
            .track_focus(&self.container_focus)
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
        disable_dark: false,
        omit_auto: false,
        right_to_left: false,
        changes: root_changes,
        other_focus: cx.focus_handle().tab_stop(true),
        container_focus: cx.focus_handle(),
        preview_font: Rc::default(),
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (root, changes, cx)
}

#[gpui::test]
fn selection_keeps_the_rendered_segment_font_stable(cx: &mut TestAppContext) {
    let (root, _, cx) = segmented_window(cx);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.size = SegmentedSize::Card;
            cx.notify();
        });
    });
    cx.run_until_parked();
    let before = root.read_with(cx, |root, _| root.preview_font.borrow().clone().unwrap());
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.current = Mode::Light;
            cx.notify();
        });
    });
    cx.run_until_parked();
    let after = root.read_with(cx, |root, _| root.preview_font.borrow().clone().unwrap());
    assert_eq!(
        after, before,
        "selection must not reshape the segment's text"
    );
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
fn selection_keeps_segment_and_label_geometry_stable(cx: &mut TestAppContext) {
    let (root, _, cx) = segmented_window(cx);
    for size in [SegmentedSize::Regular, SegmentedSize::Card] {
        cx.update(|_, cx| {
            root.update(cx, |root, cx| {
                root.size = size;
                root.current = Mode::Dark;
                cx.notify();
            });
        });
        cx.run_until_parked();
        let selectors = [
            "test-segmented",
            "test-segmented-light",
            "test-segmented-light-label",
            "test-segmented-dark",
            "test-segmented-dark-label",
            "test-segmented-auto",
            "test-segmented-auto-label",
        ];
        let before = selectors.map(|selector| cx.debug_bounds(selector).unwrap());
        for mode in [Mode::Light, Mode::Auto, Mode::Dark] {
            cx.update(|_, cx| {
                root.update(cx, |root, cx| {
                    root.current = mode;
                    cx.notify();
                });
            });
            cx.run_until_parked();
            for (selector, expected) in selectors.into_iter().zip(before) {
                assert_eq!(
                    cx.debug_bounds(selector).unwrap(),
                    expected,
                    "{selector} moved or resized after selection changed to {mode:?} ({size:?})"
                );
            }
        }
    }
}

fn focus_control(cx: &mut VisualTestContext) {
    // Advance the framework's tab-stop order so the control is reached the way keyboard traversal
    // reaches it, rather than by focusing an internal handle directly. The sibling stop is first.
    cx.update(|window, cx| {
        window.focus_next(cx);
        window.focus_next(cx);
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
    let font = gpui::font("Test Font");
    let refinement = SegmentedPaintRefinement {
        paint,
        font: font.clone(),
        font_size: px(12.0),
        line_height: 1.2,
    };

    // The refinement restates the segment's typography: GPUI replaces a text style rather than
    // merging it, so a refinement carrying only a color would reflow the segment under the pointer.
    assert_eq!(
        refinement.segment(StyleRefinement::default()),
        StyleRefinement::default()
            .bg(paint.background())
            .border_color(paint.border())
            .font(font)
            .text_size(px(12.0))
            .line_height(gpui::relative(1.2))
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
    root.update(cx, |root, cx| {
        root.current = Mode::Auto;
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("right");
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
    root.update(cx, |root, cx| {
        root.disable_auto = false;
        root.disable_dark = true;
        root.current = Mode::Light;
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("right");
    cx.run_until_parked();
    root.update(cx, |root, cx| {
        root.current = Mode::Auto;
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    let requests = changes.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].requested(), &Mode::Auto);
    assert_eq!(requests[0].source(), SegmentedActivationSource::Arrow);
    assert_eq!(requests[1].requested(), &Mode::Light);
    assert_eq!(requests[1].source(), SegmentedActivationSource::Arrow);
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
    focus_control(cx);
    assert!(cx.debug_bounds("test-segmented-keyboard-focus").is_some());
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert!(cx.update(|window, cx| window.focused(cx).is_none()));
    let other = root.read_with(cx, |root, _| root.other_focus.clone());
    for _ in 0..3 {
        cx.update(|window, cx| window.focus_next(cx));
        assert!(cx.update(|window, _| other.is_focused(window)));
    }

    click("test-segmented-light", cx);
    cx.simulate_keystrokes("right");
    cx.run_until_parked();

    assert!(changes.borrow().is_empty());
}

#[gpui::test]
fn keyboard_focus_rings_the_selected_segment(cx: &mut TestAppContext) {
    let (_root, _changes, cx) = segmented_window(cx);

    focus_control(cx);

    let selected = cx
        .debug_bounds("test-segmented-dark")
        .expect("the selected segment should render");
    let ring = cx
        .debug_bounds("test-segmented-keyboard-focus")
        .expect("keyboard focus should draw a ring");
    assert_eq!(ring, selected.dilate(px(2.0)));
}

#[gpui::test]
fn the_ring_follows_arrow_selection_at_rest_above_every_segment(cx: &mut TestAppContext) {
    let (root, changes, cx) = segmented_window(cx);
    focus_control(cx);
    crate::focus_ring::settle(cx);

    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    assert_eq!(
        changes.borrow().last().map(|change| *change.requested()),
        Some(Mode::Light)
    );
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.current = Mode::Light;
            cx.notify();
        })
    });
    cx.run_until_parked();

    let scale = cx.update(|window, _| window.scale_factor());
    let light = cx.debug_bounds("test-segmented-light").unwrap();
    let track = cx.debug_bounds("test-segmented").unwrap().scale(scale);
    let ring = rgba(0x00aaffff);
    let (bands, others): (Vec<_>, Vec<_>) = cx.update(|window, _| {
        window
            .painted_quads()
            .into_iter()
            .filter(|quad| quad.bounds.intersects(&track))
            .partition(|quad| {
                let color = gpui::Rgba::from(quad.border_color);
                (color.b - ring.b).abs() < 0.01 && (color.g - ring.g).abs() < 0.01
            })
    });
    assert!(!bands.is_empty(), "the focused control keeps its ring");
    for band in &bands {
        assert_eq!(
            band.bounds,
            light.dilate(px(2.0)).scale(scale),
            "a selection change moves the band at rest instead of replaying its entrance"
        );
    }
    // GPUI gives quads that do not overlap the same draw order, so only a later order paints over.
    let band = bands.iter().map(|band| band.order).min().unwrap();
    assert!(
        others.iter().all(|quad| quad.order <= band),
        "later segments and the track border must not paint over the band"
    );
}

#[gpui::test]
fn pointer_activation_does_not_draw_the_keyboard_focus_ring(cx: &mut TestAppContext) {
    let (_root, _changes, cx) = segmented_window(cx);

    click("test-segmented-light", cx);

    assert_eq!(cx.debug_bounds("test-segmented-keyboard-focus"), None);
}

#[gpui::test]
fn card_presentation_draws_every_option_and_its_preview(cx: &mut TestAppContext) {
    let (root, _changes, cx) = segmented_window(cx);
    assert!(cx.debug_bounds("test-segmented-light-preview").is_none());
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
    let preview = cx
        .debug_bounds("test-segmented-light-preview")
        .expect("the Card preview renders");
    assert!(light.contains(&preview.center()));
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
        disable_dark: false,
        omit_auto: true,
        right_to_left: false,
        changes: root_changes,
        other_focus: cx.focus_handle().tab_stop(true),
        container_focus: cx.focus_handle(),
        preview_font: Rc::default(),
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
fn spacing_scale_grows_option_height_but_not_text() {
    let theme = test_theme();

    let scaled = theme.scaled_spacing(2.0);

    let base = theme.sizes.resolve(SegmentedSize::Card);
    let grown = scaled.sizes.resolve(SegmentedSize::Card);
    assert_eq!(grown.font_size, base.font_size);
    assert!(grown.option_height > base.option_height);
    assert_eq!(grown.border_width, base.border_width);
}

#[gpui::test]
fn segmented_controls_publish_a_radio_group_that_follows_focus_and_press(cx: &mut TestAppContext) {
    use crate::a11y_testing::{A11yTree, perform, supports};
    use gpui::accesskit::Action;

    let (root, changes, cx) = segmented_window(cx);
    root.update(cx, |root, cx| {
        root.disable_auto = true;
        cx.notify();
    });
    let tree = A11yTree::read(cx);
    let group = tree.node("Appearance");
    assert_eq!(group["aria"]["role"], "RadioGroup");
    let options = tree.children(group);
    let labels = options
        .iter()
        .map(|option| option["aria"]["label"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["Light", "Dark", "Auto"]);
    for (option, (selected, position)) in options.iter().zip([(false, 1), (true, 2), (false, 3)]) {
        assert_eq!(option["aria"]["role"], "RadioButton");
        assert_eq!(
            option["aria"]["toggled"],
            if selected { "True" } else { "False" }
        );
        assert_eq!(option["aria"]["position_in_set"], position);
        assert_eq!(option["aria"]["size_of_set"], 3);
    }
    assert_eq!(tree.node("Auto")["aria"]["disabled"], true);
    assert!(!supports(tree.node("Auto"), Action::Click));

    focus_control(cx);
    let tree = A11yTree::read(cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Dark");

    perform(cx, tree.node("Light"), Action::Click);
    let changes = changes.borrow();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].requested(), &Mode::Light);
    assert_eq!(
        changes[0].source(),
        SegmentedActivationSource::Accessibility
    );
}

#[gpui::test]
fn only_a_focused_segmented_control_claims_its_selected_option(cx: &mut TestAppContext) {
    use crate::a11y_testing::A11yTree;

    let (root, _, cx) = segmented_window(cx);
    cx.update(|window, cx| {
        root.read(cx).container_focus.clone().focus(window, cx);
    });
    let tree = A11yTree::read(cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Container");

    focus_control(cx);
    let tree = A11yTree::read(cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Dark");
}
