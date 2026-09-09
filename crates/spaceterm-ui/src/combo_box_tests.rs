use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, Keystroke, Modifiers,
    MouseButton, ParentElement as _, Render, ScrollDelta, ScrollWheelEvent, Styled as _,
    TestAppContext, TouchPhase, VisualTestContext, Window, div, point, px, rgba,
};

use crate::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, ComboBox, ComboBoxAcceptance,
    ComboBoxActivationSource, ComboBoxCloseReason, ComboBoxCopy, ComboBoxFallback, ComboBoxItem,
    ComboBoxLifecycleEvent, ComboBoxMetrics, ComboBoxPaint, ComboBoxTheme, CommandPalette,
    CommandPaletteEvent, CommandPaletteItem, CommandPaletteLifecycleEvent, CommandPaletteMetrics,
    CommandPalettePaint, CommandPaletteTheme, Menu, MenuEntry, MenuLifecycleEvent, MenuMetrics,
    MenuPaint, MenuSizes, MenuTheme, ScrollbarTheme, TextInputKeybindingProfile, TextInputMetrics,
    TextInputPaint, TextInputTheme, TextInputVariants, install_text_input_keybindings,
    window_combo_box_is_open, window_menu_is_open,
};

#[derive(Clone, Debug, Eq, PartialEq)]
enum RecordedEvent {
    Lifecycle(ComboBoxLifecycleEvent),
    PaletteOpened,
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
    before_focus: FocusHandle,
    other_focus: FocusHandle,
    show_combo_box: bool,
    right_to_left: bool,
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
        .right_to_left(self.right_to_left)
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
            .child(
                div()
                    .h(px(80.0))
                    .debug_selector(|| "before-focus".to_owned())
                    .track_focus(&self.before_focus)
                    .child("Before"),
            )
            .children(self.show_combo_box.then_some(combo_box))
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
    reentries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuReplacementEvent {
    Combo(ComboBoxLifecycleEvent),
    Menu(MenuLifecycleEvent),
}

struct MenuReplacementRoot {
    prior_focus: FocusHandle,
    events: Rc<RefCell<Vec<MenuReplacementEvent>>>,
}

struct ModalReplacementRoot {
    prior_focus: FocusHandle,
    events: Rc<RefCell<Vec<RecordedEvent>>>,
    modal: Option<crate::ModalPresentationHandle>,
}

struct ReentrantComboReplacementRoot {
    prior_focus: FocusHandle,
    events: Rc<RefCell<Vec<(u8, ComboBoxLifecycleEvent)>>>,
    reentries: usize,
}

struct WheelContainmentRoot {
    underlay_scrolls: Rc<Cell<usize>>,
    accepted: Rc<RefCell<Vec<u8>>>,
}

impl Render for WheelContainmentRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let underlay_scrolls = Rc::clone(&self.underlay_scrolls);
        let accepted = Rc::clone(&self.accepted);
        div()
            .size_full()
            .on_scroll_wheel(move |_, _, _| {
                underlay_scrolls.set(underlay_scrolls.get() + 1);
            })
            .child(div().h(px(80.0)))
            .child(
                ComboBox::new("wheel-combo", "Workspace", Some(1), "Choose", long_items())
                    .debug_selector("wheel-combo-trigger")
                    .on_accept(move |acceptance, _, _| {
                        accepted.borrow_mut().push(*acceptance.item_id());
                    }),
            )
    }
}

impl Render for ReentrantComboReplacementRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let first_events = Rc::clone(&self.events);
        let first_owner = cx.entity().downgrade();
        let second_events = Rc::clone(&self.events);
        div()
            .size_full()
            .child(div().track_focus(&self.prior_focus))
            .child(
                ComboBox::new("reentrant-first", "First", None, "First", items())
                    .debug_selector("reentrant-first-trigger")
                    .on_lifecycle(move |event, cx| {
                        first_events.borrow_mut().push((1, *event));
                        let _ = first_owner.update(cx, |root, cx| {
                            root.reentries += 1;
                            cx.notify();
                        });
                    }),
            )
            .child(
                ComboBox::new("reentrant-second", "Second", None, "Second", items())
                    .debug_selector("reentrant-second-trigger")
                    .on_lifecycle(move |event, _| {
                        second_events.borrow_mut().push((2, *event));
                    }),
            )
    }
}

impl ModalReplacementRoot {
    fn present_modal(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.modal = Some(
            crate::Alert::new(
                crate::ModalId::new("combo-replacement-alert"),
                "Replacement alert",
                "Replacement",
                "Modal replacement",
                vec![
                    crate::ModalAction::new(
                        (),
                        "OK",
                        crate::ModalActionRole::Affirmative,
                        "combo-replacement-alert-ok",
                    )
                    .default_action(true),
                ],
            )
            .present(window, cx, |_, _| {})
            .expect("the replacement Alert should present"),
        );
    }
}

impl Render for ModalReplacementRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let events = Rc::clone(&self.events);
        crate::ModalLayer::new(crate::TooltipLayer::new(
            div()
                .size_full()
                .child(div().track_focus(&self.prior_focus))
                .child(
                    ComboBox::new(
                        "modal-replacement-combo",
                        "Workspace",
                        None,
                        "Choose",
                        items(),
                    )
                    .debug_selector("modal-replacement-combo-trigger")
                    .on_lifecycle(move |event, _| {
                        events.borrow_mut().push(RecordedEvent::Lifecycle(*event));
                    }),
                ),
        ))
    }
}

impl Render for MenuReplacementRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let combo_events = Rc::clone(&self.events);
        let menu_events = Rc::clone(&self.events);
        div()
            .size_full()
            .child(
                div()
                    .track_focus(&self.prior_focus)
                    .debug_selector(|| "menu-replacement-prior".to_owned()),
            )
            .child(
                ComboBox::new(
                    "menu-replacement-combo",
                    "Workspace",
                    None,
                    "Choose",
                    items(),
                )
                .debug_selector("menu-replacement-combo-trigger")
                .on_lifecycle(move |event, _| {
                    combo_events
                        .borrow_mut()
                        .push(MenuReplacementEvent::Combo(*event));
                }),
            )
            .child(
                Menu::new(
                    "menu-replacement-menu",
                    "Replacement menu",
                    vec![MenuEntry::action("Action", 1u8)],
                )
                .debug_selector("menu-replacement-menu-trigger")
                .on_activate(|_, _, _| {})
                .on_lifecycle(move |event, _| {
                    menu_events
                        .borrow_mut()
                        .push(MenuReplacementEvent::Menu(*event));
                }),
            )
    }
}

impl Render for PaletteReplacementRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let events = Rc::clone(&self.events);
        let owner = cx.entity().downgrade();
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
                .on_lifecycle(move |event, cx| {
                    events.borrow_mut().push(RecordedEvent::Lifecycle(*event));
                    let _ = owner.update(cx, |root, cx| {
                        root.reentries += 1;
                        cx.notify();
                    });
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

fn install_modal_test_support(cx: &mut TestAppContext) {
    let variant = crate::ButtonVariantStyle::new(
        crate::ButtonPaint::new(rgba(0x00000000), rgba(0xcdcdcdff), rgba(0x00000000)),
        crate::ButtonPaint::new(rgba(0x252530ff), rgba(0xcdcdcdff), rgba(0x00000000)),
        crate::ButtonPaint::new(rgba(0x252530ff), rgba(0xcdcdcdff), rgba(0x00000000)),
        crate::ButtonPaint::new(rgba(0x141415ff), rgba(0x606079ff), rgba(0x00000000)),
    );
    let button_metrics = crate::ButtonMetrics::new(px(24.0));
    cx.set_global(crate::ButtonTheme::new(
        crate::ButtonVariants::new(variant, variant, variant, variant, variant, variant),
        crate::ButtonSizes::new(
            button_metrics,
            button_metrics,
            button_metrics,
            button_metrics,
        ),
        rgba(0x405065ff),
    ));
    cx.update(crate::tooltip::init);
    cx.update(crate::modal::init);
    cx.update(|cx| {
        crate::install_modal_policy(cx, crate::ModalDesktopPolicy::mac_os());
        crate::install_modal_theme(
            cx,
            crate::ModalTheme::new(
                crate::ModalPaint::new(
                    rgba(0x00000099),
                    rgba(0x202024ff),
                    rgba(0x606068ff),
                    rgba(0xffffffff),
                    rgba(0xb0b0b8ff),
                    rgba(0x505058ff),
                    rgba(0x404048ff),
                    rgba(0x55aaffff),
                    rgba(0x5599ffff),
                    rgba(0x5599ff22),
                    rgba(0xffbb55ff),
                    rgba(0xffbb5522),
                    rgba(0xff6677ff),
                    rgba(0xff667722),
                ),
                crate::ModalMetrics::new(px(360.0), px(480.0), px(640.0)),
            ),
        );
    });
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
        before_focus: cx.focus_handle().tab_stop(true),
        other_focus: cx.focus_handle().tab_stop(true),
        show_combo_box: true,
        right_to_left: false,
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
        before_focus: cx.focus_handle().tab_stop(true),
        other_focus: cx.focus_handle().tab_stop(true),
        show_combo_box: true,
        right_to_left: false,
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
fn combo_box_ownership_should_be_isolated_per_window(cx: &mut TestAppContext) {
    let (_, first_events, _, first) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(first);
    let first_window = first.update(|window, _| window.window_handle());
    let mut shared_app = first.cx.clone();

    let (_, second_events, _, second) = combo_box_window(&mut shared_app, Some(3), items(), false);
    open_by_pointer(second);
    assert!(second.update(|window, cx| window_combo_box_is_open(window, cx)));

    let mut first = VisualTestContext::from_window(first_window, &second.cx);
    assert!(!first.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert_eq!(
        first_events.borrow().as_slice(),
        [
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Opened),
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
                ComboBoxCloseReason::Deactivated,
            )),
        ]
    );
    assert_eq!(
        second_events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Opened)]
    );

    let first_event_count = first_events.borrow().len();
    second.simulate_keystrokes("escape");
    second.run_until_parked();
    assert_eq!(first_events.borrow().len(), first_event_count);
    assert!(!second.update(|window, cx| window_combo_box_is_open(window, cx)));
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
        reentries: 0,
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
    root.update(cx, |root, _| root.reentries = 0);
    let palette = root.read_with(cx, |root, _| root.palette.clone());
    let palette_events = Rc::clone(&events);
    root.update(cx, |_, cx| {
        cx.subscribe(&palette, move |_, _, event, _| {
            if matches!(
                event,
                CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened)
            ) {
                palette_events
                    .borrow_mut()
                    .push(RecordedEvent::PaletteOpened);
            }
        })
        .detach();
    });

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
    });
    cx.run_until_parked();

    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert_eq!(
        events.borrow().as_slice(),
        [
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
                ComboBoxCloseReason::Replaced,
            )),
            RecordedEvent::PaletteOpened,
        ]
    );
    assert_eq!(root.read_with(cx, |root, _| root.reentries), 1);
}

#[gpui::test]
fn combo_box_replacement_should_release_borrows_before_reentrant_lifecycle_delivery(
    cx: &mut TestAppContext,
) {
    install_themes(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let root_events = Rc::clone(&events);
    let (root, cx) = cx.add_window_view(move |_, cx| ReentrantComboReplacementRoot {
        prior_focus: cx.focus_handle().tab_stop(true),
        events: root_events,
        reentries: 0,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let trigger = cx
        .debug_bounds("reentrant-first-trigger")
        .expect("the first ComboBox trigger should render")
        .center();
    cx.simulate_click(trigger, Modifiers::none());
    cx.run_until_parked();
    events.borrow_mut().clear();
    root.update(cx, |root, _| root.reentries = 0);

    cx.update(|window, cx| {
        window.focus_next();
        window.focus_next();
        window.focus_next();
        window.dispatch_keystroke(Keystroke::parse("enter").expect("valid keystroke"), cx);
    });
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [
            (
                1,
                ComboBoxLifecycleEvent::Closed(ComboBoxCloseReason::Replaced),
            ),
            (2, ComboBoxLifecycleEvent::Opened),
        ]
    );
    assert_eq!(root.read_with(cx, |root, _| root.reentries), 1);
}

#[gpui::test]
fn menu_replacement_should_close_combo_first_and_restore_its_predecessor(cx: &mut TestAppContext) {
    install_themes(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let root_events = Rc::clone(&events);
    let (root, cx) = cx.add_window_view(move |_, cx| MenuReplacementRoot {
        prior_focus: cx.focus_handle().tab_stop(true),
        events: root_events,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let prior_focus = root.read_with(cx, |root, _| root.prior_focus.clone());
    cx.update(|window, _| prior_focus.focus(window));
    let combo_trigger = cx
        .debug_bounds("menu-replacement-combo-trigger")
        .expect("the ComboBox trigger should render")
        .center();
    cx.simulate_click(combo_trigger, Modifiers::none());
    cx.run_until_parked();
    events.borrow_mut().clear();
    cx.update(|window, cx| {
        window.focus_next();
        window.focus_next();
        window.focus_next();
        window.dispatch_keystroke(Keystroke::parse("enter").expect("valid keystroke"), cx);
    });
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [
            MenuReplacementEvent::Combo(ComboBoxLifecycleEvent::Closed(
                ComboBoxCloseReason::Replaced,
            )),
            MenuReplacementEvent::Menu(MenuLifecycleEvent::Opened),
        ]
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, cx| window_menu_is_open(window, cx)));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|window, _| prior_focus.is_focused(window)));
}

#[gpui::test]
fn modal_should_replace_an_open_combo_box_and_inherit_its_predecessor(cx: &mut TestAppContext) {
    install_themes(cx);
    install_modal_test_support(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let root_events = Rc::clone(&events);
    let (root, cx) = cx.add_window_view(move |_, cx| ModalReplacementRoot {
        prior_focus: cx.focus_handle().tab_stop(true),
        events: root_events,
        modal: None,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let prior_focus = root.read_with(cx, |root, _| root.prior_focus.clone());
    cx.update(|window, _| prior_focus.focus(window));
    let trigger = cx
        .debug_bounds("modal-replacement-combo-trigger")
        .expect("the ComboBox trigger should render")
        .center();
    cx.simulate_click(trigger, Modifiers::none());
    cx.run_until_parked();
    events.borrow_mut().clear();

    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.present_modal(window, cx));
    });
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::Replaced,
        ))]
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, cx| crate::window_modal_is_open(window, cx)));

    let modal = root.read_with(cx, |root, _| {
        root.modal
            .clone()
            .expect("the Alert handle should be retained")
    });
    cx.update(|window, cx| modal.dismiss(window, cx).expect("the Alert should dismiss"));
    cx.run_until_parked();
    assert!(cx.update(|window, _| prior_focus.is_focused(window)));
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
fn second_trigger_press_should_toggle_closed_with_the_trigger_reason(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    open_by_pointer(cx);

    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::Trigger,
        ))]
    );
}

#[gpui::test]
fn tab_should_close_and_continue_focus_traversal(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
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
fn shift_tab_should_traverse_to_the_control_before_the_trigger_after_pointer_open(
    cx: &mut TestAppContext,
) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let before_focus = root.read_with(cx, |root, _| root.before_focus.clone());

    cx.simulate_keystrokes("shift-tab");
    cx.run_until_parked();

    assert!(cx.update(|window, _| before_focus.is_focused(window)));
    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::TabTraversal,
        ))]
    );
}

#[gpui::test]
fn moving_focus_out_should_close_with_the_focus_lost_reason(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let other_focus = root.read_with(cx, |root, _| root.other_focus.clone());

    cx.update(|window, _| other_focus.focus(window));
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::FocusLost,
        ))]
    );
}

#[gpui::test]
fn deactivation_should_close_then_restore_the_predecessor_on_reactivation(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    let before_focus = root.read_with(cx, |root, _| root.before_focus.clone());
    cx.update(|window, _| before_focus.focus(window));
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.deactivate_window();
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::Deactivated,
        ))]
    );
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert!(cx.update(|window, _| before_focus.is_focused(window)));
}

#[gpui::test]
fn removing_the_trigger_should_close_and_restore_focus(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    let before_focus = root.read_with(cx, |root, _| root.before_focus.clone());
    cx.update(|window, _| before_focus.focus(window));
    open_by_pointer(cx);
    events.borrow_mut().clear();

    root.update(cx, |root, cx| {
        root.show_combo_box = false;
        cx.notify();
    });
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
            ComboBoxCloseReason::TargetDisappeared,
        ))]
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, _| before_focus.is_focused(window)));
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
fn nested_editor_context_menu_should_preserve_standard_editing_actions_and_combo_ownership(
    cx: &mut TestAppContext,
) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    cx.simulate_input("local");
    cx.run_until_parked();
    events.borrow_mut().clear();
    let input = cx
        .debug_bounds("combo-box-input")
        .expect("the ComboBox editor should render")
        .center();

    cx.simulate_mouse_down(input, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(input, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();

    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, cx| window_menu_is_open(window, cx)));
    for action in ["Undo", "Cut", "Copy", "Paste", "Select All"] {
        assert!(
            cx.debug_bounds(action).is_some(),
            "the {action} action should be published by the editor menu"
        );
    }
    assert!(events.borrow().is_empty());

    let select_all = cx
        .debug_bounds("Select All")
        .expect("Select All should render")
        .center();
    cx.simulate_click(select_all, Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| window_combo_box_is_open(window, cx)),
        "the context menu action closed its owning ComboBox: {:?}",
        events.borrow().as_slice()
    );

    cx.simulate_mouse_down(input, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(input, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    let cut = cx.debug_bounds("Cut").expect("Cut should render").center();
    cx.simulate_click(cut, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("combo-row-local").is_some());
    assert!(cx.debug_bounds("combo-row-remote").is_some());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
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

fn long_items() -> Vec<ComboBoxItem<u8>> {
    (1..=64)
        .map(|id| {
            ComboBoxItem::new(id, format!("Workspace {id:02}"))
                .debug_selector(format!("combo-row-{id}"))
        })
        .collect()
}

#[gpui::test]
fn wheel_should_scroll_long_results_without_reaching_the_underlay(cx: &mut TestAppContext) {
    install_themes(cx);
    let underlay_scrolls = Rc::new(Cell::new(0));
    let root_scrolls = Rc::clone(&underlay_scrolls);
    let accepted = Rc::new(RefCell::new(Vec::new()));
    let root_accepted = Rc::clone(&accepted);
    let (_, cx) = cx.add_window_view(move |_, _| WheelContainmentRoot {
        underlay_scrolls: root_scrolls,
        accepted: root_accepted,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let trigger = cx
        .debug_bounds("wheel-combo-trigger")
        .expect("the ComboBox trigger should render")
        .center();
    cx.simulate_click(trigger, Modifiers::none());
    cx.run_until_parked();
    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("the ComboBox panel should render");
    assert!(cx.debug_bounds("combo-row-1").is_some());
    assert!(cx.debug_bounds("combo-row-20").is_none());

    cx.simulate_event(ScrollWheelEvent {
        position: panel.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-640.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let first_visible_row = point(panel.center().x, panel.top() + px(64.0));
    cx.simulate_click(first_visible_row, Modifiers::none());
    cx.run_until_parked();
    assert!(accepted.borrow().first().is_some_and(|id| *id > 1));
    assert_eq!(underlay_scrolls.get(), 0);
}

#[gpui::test]
fn page_navigation_should_move_by_a_viewport_in_both_directions(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(30), long_items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("pageup enter");
    cx.run_until_parked();
    let page_up_id = events
        .borrow()
        .iter()
        .find_map(|event| match event {
            RecordedEvent::Accepted { item_id, .. } => Some(*item_id),
            RecordedEvent::Lifecycle(_) | RecordedEvent::PaletteOpened => None,
        })
        .expect("Page Up should leave an acceptible provisional item");
    assert!(page_up_id < 30);

    open_by_pointer(cx);
    events.borrow_mut().clear();
    cx.simulate_keystrokes("pagedown enter");
    cx.run_until_parked();
    let page_down_id = events
        .borrow()
        .iter()
        .find_map(|event| match event {
            RecordedEvent::Accepted { item_id, .. } => Some(*item_id),
            RecordedEvent::Lifecycle(_) | RecordedEvent::PaletteOpened => None,
        })
        .expect("Page Down should leave an acceptible provisional item");
    assert!(page_down_id > 30);
}

#[gpui::test]
fn repaired_provisional_item_should_be_revealed_after_a_long_model_update(cx: &mut TestAppContext) {
    let (root, _, _, cx) = combo_box_window(cx, Some(1), long_items(), false);
    open_by_pointer(cx);
    assert!(cx.debug_bounds("combo-row-63").is_none());

    root.update(cx, |root, cx| {
        root.selected = Some(63);
        root.items.retain(|item| item.id() != &1);
        cx.notify();
    });
    for _ in 0..3 {
        cx.update(|window, _| window.refresh());
        let _ = cx.debug_bounds("combo-box-panel");
        cx.run_until_parked();
    }

    assert!(
        cx.debug_bounds("combo-row-63").is_some(),
        "the repaired provisional row should be scrolled into the virtualized viewport"
    );
}

#[gpui::test]
fn start_alignment_should_follow_right_to_left_layout_direction(cx: &mut TestAppContext) {
    let (root, _, _, cx) = combo_box_window(cx, Some(1), items(), false);
    root.update(cx, |root, cx| {
        root.right_to_left = true;
        cx.notify();
    });
    cx.run_until_parked();
    open_by_pointer(cx);

    let trigger = cx
        .debug_bounds("combo-box-trigger")
        .expect("the trigger should render");
    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("the panel should render");
    assert_eq!(trigger.right() - panel.right(), px(12.0));
}

#[gpui::test]
fn committed_input_method_text_should_filter_without_closing_the_popup(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_input("ssh");
    cx.run_until_parked();

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 3,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}
