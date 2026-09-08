use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, Modifiers, MouseButton,
    ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div, point,
    px, rgba,
};

use crate::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, ComboBox, ComboBoxAcceptance,
    ComboBoxActivationSource, ComboBoxCloseReason, ComboBoxCopy, ComboBoxFallback, ComboBoxItem,
    ComboBoxLifecycleEvent, ComboBoxMetrics, ComboBoxPaint, ComboBoxTheme, CommandPalette,
    CommandPaletteItem, CommandPaletteMetrics, CommandPalettePaint, CommandPaletteTheme,
    MenuMetrics, MenuPaint, MenuSizes, MenuTheme, ScrollbarTheme, TextInputKeybindingProfile,
    TextInputMetrics, TextInputPaint, TextInputTheme, TextInputVariants,
    install_text_input_keybindings, window_combo_box_is_open, window_menu_is_open,
};

#[derive(Clone, Debug, Eq, PartialEq)]
enum RecordedEvent {
    Lifecycle(ComboBoxLifecycleEvent),
    Accepted {
        item_id: u8,
        source: ComboBoxActivationSource,
        window_was_open: bool,
    },
}

struct TestRoot {
    selected: Option<u8>,
    items: Vec<ComboBoxItem<u8>>,
    fallback: Option<ComboBoxFallback<u8>>,
    copy: ComboBoxCopy,
    disabled: bool,
    other_focus: FocusHandle,
    events: Rc<RefCell<Vec<RecordedEvent>>>,
    underlay_presses: Rc<Cell<usize>>,
}

impl Render for TestRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let lifecycle_events = Rc::clone(&self.events);
        let acceptance_events = Rc::clone(&self.events);
        let underlay_presses = Rc::clone(&self.underlay_presses);
        let combo_box = ComboBox::new(
            "test-combo-box",
            "Workspace type",
            self.selected,
            "Choose a workspace type",
            self.items.clone(),
        );
        let combo_box = if let Some(fallback) = self.fallback.clone() {
            combo_box.fallback(fallback)
        } else {
            combo_box
        }
        .copy(self.copy.clone())
        .disabled(self.disabled)
        .placement(AnchoredPlacementConfig::new(
            AnchoredPlacement::Bottom,
            AnchoredAlignment::Start,
        ))
        .debug_selector("combo-box-trigger")
        .on_lifecycle(move |event, _| {
            lifecycle_events
                .borrow_mut()
                .push(RecordedEvent::Lifecycle(*event));
        })
        .on_accept(move |acceptance: &ComboBoxAcceptance<u8>, window, cx| {
            acceptance_events
                .borrow_mut()
                .push(RecordedEvent::Accepted {
                    item_id: *acceptance.item_id(),
                    source: acceptance.source(),
                    window_was_open: window_combo_box_is_open(window, cx),
                });
        });

        div()
            .relative()
            .size_full()
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                underlay_presses.set(underlay_presses.get() + 1);
            })
            .flex()
            .flex_col()
            .child(div().h(px(80.0)))
            .child(combo_box)
            .child(
                div()
                    .debug_selector(|| "other-focus".to_owned())
                    .track_focus(&self.other_focus)
                    .child("Other"),
            )
    }
}

struct PaletteReplacementRoot {
    palette: Entity<CommandPalette<u8>>,
    events: Rc<RefCell<Vec<RecordedEvent>>>,
}

impl Render for PaletteReplacementRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let events = Rc::clone(&self.events);
        div()
            .size_full()
            .child(
                ComboBox::new(
                    "palette-replacement-combo-box",
                    "Workspace type",
                    None,
                    "Choose",
                    items(),
                )
                .debug_selector("palette-replacement-trigger")
                .on_lifecycle(move |event, _| {
                    events.borrow_mut().push(RecordedEvent::Lifecycle(*event));
                }),
            )
            .child(self.palette.clone())
    }
}

type ComboBoxWindow<'a> = (
    Entity<TestRoot>,
    Rc<RefCell<Vec<RecordedEvent>>>,
    Rc<Cell<usize>>,
    &'a mut VisualTestContext,
);

fn items() -> Vec<ComboBoxItem<u8>> {
    vec![
        ComboBoxItem::new(1, "Local Workspace")
            .description("Open a directory")
            .debug_selector("combo-row-local"),
        ComboBoxItem::new(2, "Unavailable Workspace")
            .disabled(true)
            .debug_selector("combo-row-disabled"),
        ComboBoxItem::new(3, "Remote Workspace")
            .keywords(["ssh"])
            .debug_selector("combo-row-remote"),
        ComboBoxItem::new(4, "Zellij Session").debug_selector("combo-row-zellij"),
    ]
}

fn install_themes(cx: &mut TestAppContext) {
    cx.set_global(ComboBoxTheme::new(
        ComboBoxPaint::new(
            rgba(0x141415ff),
            rgba(0x252530ff),
            rgba(0xcdcdcdff),
            rgba(0x878787ff),
            rgba(0x606079ff),
            rgba(0x252530ff),
            rgba(0xffffffff),
            rgba(0x141415ff),
            rgba(0x1c1c24ff),
            rgba(0x606079ff),
            rgba(0x7e98e8ff),
        ),
        ComboBoxMetrics::new(px(240.0), px(40.0)).geometry(px(260.0), px(36.0), px(30.0), px(46.0)),
    ));
    let input_paint = TextInputPaint::new(
        rgba(0xcdcdcdff),
        rgba(0x878787ff),
        rgba(0x6e94b266),
        rgba(0xcdcdcdff),
        rgba(0x606079ff),
        rgba(0x606079ff),
    );
    cx.set_global(TextInputTheme::new(
        TextInputVariants::new(input_paint, input_paint),
        TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(20.0)),
    ));
    let menu_paint = MenuPaint::new(
        rgba(0x141415ff),
        rgba(0x252530ff),
        rgba(0xcdcdcdff),
        rgba(0x878787ff),
        rgba(0x606079ff),
        rgba(0x252530ff),
        rgba(0xffffffff),
        rgba(0xd8647eff),
        rgba(0x252530ff),
    );
    let menu_metrics = MenuMetrics::new(px(160.0), px(26.0));
    cx.set_global(MenuTheme::new(
        menu_paint,
        MenuSizes::new(menu_metrics, menu_metrics, menu_metrics),
    ));
    cx.set_global(CommandPaletteTheme::new(
        CommandPalettePaint::new(
            rgba(0x141415ff),
            rgba(0x252530ff),
            rgba(0xcdcdcdff),
            rgba(0x878787ff),
            rgba(0x606079ff),
            rgba(0x252530ff),
            rgba(0xffffffff),
            rgba(0x7e98e8ff),
        ),
        CommandPaletteMetrics::new(px(420.0), px(40.0)),
    ));
    cx.set_global(ScrollbarTheme::new(
        rgba(0x33373878),
        rgba(0x60607978),
        rgba(0xcdcdcdff),
    ));
    cx.update(crate::text_input::init);
    cx.update(crate::menu::init);
    cx.update(crate::combo_box::init);
    cx.update(crate::command_palette::init);
    cx.update(|cx| install_text_input_keybindings(cx, TextInputKeybindingProfile::MacOs));
}

fn combo_box_window(
    cx: &mut TestAppContext,
    selected: Option<u8>,
    items: Vec<ComboBoxItem<u8>>,
    disabled: bool,
) -> ComboBoxWindow<'_> {
    install_themes(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let underlay_presses = Rc::new(Cell::new(0));
    let root_events = Rc::clone(&events);
    let root_underlay_presses = Rc::clone(&underlay_presses);
    let (root, cx) = cx.add_window_view(move |_, cx| TestRoot {
        selected,
        items,
        fallback: None,
        copy: ComboBoxCopy::default(),
        disabled,
        other_focus: cx.focus_handle().tab_stop(true),
        events: root_events,
        underlay_presses: root_underlay_presses,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (root, events, underlay_presses, cx)
}

fn fallback_combo_box_window(
    cx: &mut TestAppContext,
    fallback: ComboBoxFallback<u8>,
) -> ComboBoxWindow<'_> {
    install_themes(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let underlay_presses = Rc::new(Cell::new(0));
    let root_events = Rc::clone(&events);
    let root_underlay_presses = Rc::clone(&underlay_presses);
    let (root, cx) = cx.add_window_view(move |_, cx| TestRoot {
        selected: None,
        items: items(),
        fallback: Some(fallback),
        copy: ComboBoxCopy::default(),
        disabled: false,
        other_focus: cx.focus_handle().tab_stop(true),
        events: root_events,
        underlay_presses: root_underlay_presses,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (root, events, underlay_presses, cx)
}

fn trigger_center(cx: &mut VisualTestContext) -> gpui::Point<gpui::Pixels> {
    cx.debug_bounds("combo-box-trigger")
        .expect("the ComboBox trigger should render")
        .center()
}

fn open_by_pointer(cx: &mut VisualTestContext) {
    let trigger = trigger_center(cx);
    cx.simulate_click(trigger, Modifiers::none());
    cx.run_until_parked();
}

fn focus_trigger(cx: &mut VisualTestContext) {
    cx.update(|window, _| window.focus_next());
    cx.run_until_parked();
}

#[gpui::test]
fn pointer_press_should_open_the_popup(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);

    open_by_pointer(cx);

    assert!(cx.debug_bounds("combo-box-panel").is_some());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Opened)]
    );
}

#[gpui::test]
fn command_palette_should_replace_an_open_combo_box_in_the_same_window(cx: &mut TestAppContext) {
    install_themes(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let root_events = Rc::clone(&events);
    let (root, cx) = cx.add_window_view(move |window, cx| PaletteReplacementRoot {
        palette: cx.new(|cx| {
            CommandPalette::new(
                "Replacement palette",
                vec![CommandPaletteItem::new(1, "Command")],
                window,
                cx,
            )
        }),
        events: root_events,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let trigger = cx
        .debug_bounds("palette-replacement-trigger")
        .expect("the ComboBox trigger should render")
        .center();
    cx.simulate_click(trigger, Modifiers::none());
    cx.run_until_parked();
    events.borrow_mut().clear();
    let palette = root.read_with(cx, |root, _| root.palette.clone());

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
    });
    cx.run_until_parked();

    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::Replaced,
        ))]
    );
}

#[gpui::test]
fn down_key_on_the_focused_trigger_should_open_the_popup(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    focus_trigger(cx);

    cx.simulate_keystrokes("down");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-box-panel").is_some());
    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Opened)]
    );
}

#[gpui::test]
fn printable_input_on_the_trigger_should_open_with_that_query(cx: &mut TestAppContext) {
    let (_, _, _, cx) = combo_box_window(cx, Some(1), items(), false);
    focus_trigger(cx);

    cx.simulate_keystrokes("z");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-row-zellij").is_some());
    assert!(cx.debug_bounds("combo-row-local").is_none());
}

#[gpui::test]
fn pinned_fallback_should_persist_after_filtering_and_receive_the_exact_query(
    cx: &mut TestAppContext,
) {
    let queries = Rc::new(RefCell::new(Vec::new()));
    let recorded_queries = Rc::clone(&queries);
    let fallback = ComboBoxFallback::new(move |query| {
        recorded_queries.borrow_mut().push(query.to_owned());
        ComboBoxItem::new(9, format!("Create {query}")).debug_selector("combo-row-fallback")
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    focus_trigger(cx);

    cx.simulate_keystrokes("x");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-row-local").is_none());
    assert!(cx.debug_bounds("combo-row-fallback").is_some());
    assert_eq!(queries.borrow().last().map(String::as_str), Some("x"));

    events.borrow_mut().clear();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 9,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn ordinary_match_should_take_precedence_while_fallback_stays_pinned_last(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::new(|query| {
        ComboBoxItem::new(9, format!("Create {query}")).debug_selector("combo-row-fallback")
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);

    let ordinary = cx
        .debug_bounds("combo-row-zellij")
        .expect("the last ordinary row should render");
    let fallback = cx
        .debug_bounds("combo-row-fallback")
        .expect("the pinned fallback should render");
    assert!(fallback.top() >= ordinary.bottom());

    events.borrow_mut().clear();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 1,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn pointer_should_accept_the_pinned_fallback(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::new(|query| {
        ComboBoxItem::new(9, format!("Create {query}")).debug_selector("combo-row-fallback")
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let fallback = cx
        .debug_bounds("combo-row-fallback")
        .expect("the fallback row should render");

    cx.simulate_click(fallback.center(), Modifiers::none());
    cx.run_until_parked();

    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 9,
        source: ComboBoxActivationSource::Pointer,
        window_was_open: false,
    }));
}

#[gpui::test]
fn disabled_pinned_fallback_should_render_without_becoming_provisional(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::new(|query| {
        ComboBoxItem::new(9, format!("Create {query}"))
            .disabled(true)
            .debug_selector("combo-row-fallback")
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    focus_trigger(cx);

    cx.simulate_keystrokes("x enter");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-row-fallback").is_some());
    assert!(
        events
            .borrow()
            .iter()
            .all(|event| !matches!(event, RecordedEvent::Accepted { .. }))
    );
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn navigation_should_remain_provisional_until_acceptance(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("down");
    cx.run_until_parked();

    assert_eq!(root.read_with(cx, |root, _| root.selected), Some(1));
    assert!(events.borrow().is_empty());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn home_and_end_should_navigate_options_while_the_editor_is_focused(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("end enter");
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
                ComboBoxCloseReason::Accepted,
            )),
            RecordedEvent::Accepted {
                item_id: 4,
                source: ComboBoxActivationSource::Keyboard,
                window_was_open: false,
            },
        ]
    );
}

#[gpui::test]
fn acceptance_should_close_before_notifying_the_caller(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    cx.simulate_keystrokes("down");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
                ComboBoxCloseReason::Accepted,
            )),
            RecordedEvent::Accepted {
                item_id: 3,
                source: ComboBoxActivationSource::Keyboard,
                window_was_open: false,
            },
        ]
    );
}

#[gpui::test]
fn disabled_control_should_ignore_pointer_and_keyboard_open_requests(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), true);

    open_by_pointer(cx);
    focus_trigger(cx);
    cx.simulate_keystrokes("down z");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-box-panel").is_none());
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn disabling_an_open_control_should_close_it_with_the_disabled_reason(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    root.update(cx, |root, cx| {
        root.disabled = true;
        cx.notify();
    });
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::Disabled,
        ))]
    );
}

#[gpui::test]
fn all_disabled_items_should_render_but_reject_acceptance(cx: &mut TestAppContext) {
    let all_disabled = vec![
        ComboBoxItem::new(1, "Local")
            .disabled(true)
            .debug_selector("combo-row-local"),
        ComboBoxItem::new(2, "Remote")
            .disabled(true)
            .debug_selector("combo-row-remote"),
    ];
    let (_, events, _, cx) = combo_box_window(cx, None, all_disabled, false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-row-local").is_some());
    assert!(events.borrow().is_empty());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn empty_items_should_render_a_non_accepting_empty_state(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, None, Vec::new(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-box-empty").is_some());
    assert!(events.borrow().is_empty());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn caller_owned_status_copy_should_update_an_open_popup(cx: &mut TestAppContext) {
    let (root, _, _, cx) = combo_box_window(cx, None, Vec::new(), false);
    open_by_pointer(cx);

    root.update(cx, |root, cx| {
        root.copy = ComboBoxCopy::new(
            "Filter workspaces",
            "Find a workspace",
            "Refreshing",
            "Nothing available",
        );
        cx.notify();
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-box-empty").is_some());
}

#[gpui::test]
fn escape_should_close_with_the_escape_reason(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::Escape,
        ))]
    );
}

#[gpui::test]
fn tab_should_close_and_continue_focus_traversal(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    focus_trigger(cx);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let other_focus = root.read_with(cx, |root, _| root.other_focus.clone());

    cx.simulate_keystrokes("tab");
    cx.run_until_parked();

    assert!(cx.update(|window, _| other_focus.is_focused(window)));
    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::TabTraversal,
        ))]
    );
}

#[gpui::test]
fn outside_press_should_close_without_reaching_the_underlay(cx: &mut TestAppContext) {
    let (_, events, underlay_presses, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    underlay_presses.set(0);
    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("the ComboBox panel should render");
    let outside = point(panel.right() + px(8.0), panel.bottom() + px(8.0));

    cx.simulate_click(outside, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(underlay_presses.get(), 0);
    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::Outside,
        ))]
    );
}

#[gpui::test]
fn nested_editor_secondary_press_should_remain_inert_without_closing_the_combo_box(
    cx: &mut TestAppContext,
) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let input = cx
        .debug_bounds("combo-box-input")
        .expect("the ComboBox editor should render")
        .center();

    cx.simulate_mouse_down(input, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(input, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();

    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(!cx.update(|window, cx| window_menu_is_open(window, cx)));
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn pointer_press_and_release_must_belong_to_the_same_row(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let first = cx
        .debug_bounds("combo-row-local")
        .expect("the first row should render")
        .center();
    let second = cx
        .debug_bounds("combo-row-remote")
        .expect("the second enabled row should render")
        .center();

    cx.simulate_mouse_down(first, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(second, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(second, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert!(events.borrow().is_empty());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn pointer_press_and_release_on_one_enabled_row_should_accept(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let remote = cx
        .debug_bounds("combo-row-remote")
        .expect("the remote row should render")
        .center();

    cx.simulate_mouse_down(remote, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(remote, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
                ComboBoxCloseReason::Accepted,
            )),
            RecordedEvent::Accepted {
                item_id: 3,
                source: ComboBoxActivationSource::Pointer,
                window_was_open: false,
            },
        ]
    );
}

#[gpui::test]
fn query_change_should_invalidate_an_in_progress_pointer_gesture(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let remote = cx
        .debug_bounds("combo-row-remote")
        .expect("the remote row should render")
        .center();

    cx.simulate_mouse_down(remote, MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes("z");
    cx.simulate_mouse_up(remote, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert!(events.borrow().is_empty());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn open_popup_should_render_live_item_updates(cx: &mut TestAppContext) {
    let (root, _, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);

    root.update(cx, |root, cx| {
        root.items.push(
            ComboBoxItem::new(5, "Container Workspace").debug_selector("combo-row-container"),
        );
        cx.notify();
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-box-panel").is_some());
    assert!(cx.debug_bounds("combo-row-container").is_some());
}
