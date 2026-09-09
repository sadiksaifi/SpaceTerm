use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement as _,
    Keystroke, Modifiers, MouseButton, ParentElement as _, Render, ScrollDelta, ScrollWheelEvent,
    Styled as _, TestAppContext, TouchPhase, VisualTestContext, Window, div, point, px, rgba,
};

use crate::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, ComboBox, ComboBoxAcceptance,
    ComboBoxActivationSource, ComboBoxCloseReason, ComboBoxCopy, ComboBoxFallback, ComboBoxHandle,
    ComboBoxItem, ComboBoxKeybindingProfile, ComboBoxLifecycleEvent, ComboBoxMetrics,
    ComboBoxPaint, ComboBoxTheme, CommandPalette, CommandPaletteEvent, CommandPaletteItem,
    CommandPaletteLifecycleEvent, CommandPaletteMetrics, CommandPalettePaint, CommandPaletteTheme,
    Menu, MenuEntry, MenuLifecycleEvent, MenuMetrics, MenuPaint, MenuSizes, MenuTheme,
    ScrollbarTheme, TextInputKeybindingProfile, TextInputMetrics, TextInputPaint, TextInputTheme,
    TextInputVariants, install_combo_box_keybindings, install_portable_combo_box_keybindings,
    install_text_input_keybindings, window_combo_box_is_open, window_menu_is_open,
};

#[derive(Clone, Debug, Eq, PartialEq)]
enum RecordedEvent {
    Lifecycle(ComboBoxLifecycleEvent),
    PaletteOpened,
    ModalOpened,
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
    handle: ComboBoxHandle<u8>,
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
        )
        .handle(self.handle.clone());
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
    reentries: usize,
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

struct IconTriggerRoot {
    disabled: bool,
    before_focus: FocusHandle,
    after_focus: FocusHandle,
    events: Rc<RefCell<Vec<ComboBoxLifecycleEvent>>>,
}

impl Render for IconTriggerRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let events = Rc::clone(&self.events);
        crate::TooltipLayer::new(
            div()
                .relative()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(40.0)).track_focus(&self.before_focus))
                .child(
                    ComboBox::new(
                        "icon-combo",
                        "Choose Workspace",
                        None,
                        "Choose Workspace",
                        items(),
                    )
                    .icon_trigger(|foreground| {
                        div()
                            .debug_selector(|| "combo-box-trigger-icon".to_owned())
                            .size(px(12.0))
                            .bg(foreground)
                            .into_any_element()
                    })
                    .full_width(true)
                    .disabled(self.disabled)
                    .debug_selector("combo-box-trigger")
                    .tooltip(
                        crate::Tooltip::new("icon-combo-tooltip", "Choose Workspace")
                            .debug_selector("icon-combo-help"),
                    )
                    .on_lifecycle(move |event, _| events.borrow_mut().push(*event)),
                )
                .child(div().h(px(40.0)).track_focus(&self.after_focus)),
        )
    }
}

struct PlacementRoot {
    origin: gpui::Point<gpui::Pixels>,
    placement: AnchoredPlacementConfig,
    items: Vec<ComboBoxItem<u8>>,
    accepted: Rc<RefCell<Vec<u8>>>,
}

impl Render for PlacementRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let accepted = Rc::clone(&self.accepted);
        div().relative().size_full().child(
            div()
                .absolute()
                .left(self.origin.x)
                .top(self.origin.y)
                .w(px(100.0))
                .child(
                    ComboBox::new(
                        "placement-combo",
                        "Workspace",
                        None,
                        "Choose",
                        self.items.clone(),
                    )
                    .full_width(true)
                    .placement(self.placement)
                    .debug_selector("combo-box-trigger")
                    .on_accept(move |acceptance, _, _| {
                        accepted.borrow_mut().push(*acceptance.item_id());
                    }),
                ),
        )
    }
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
        let events = Rc::clone(&self.events);
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
            .present_with_lifecycle(
                window,
                cx,
                |_, _| {},
                move |event, _| {
                    if matches!(event, crate::ModalLifecycleEvent::Opened(_)) {
                        events.borrow_mut().push(RecordedEvent::ModalOpened);
                    }
                },
            )
            .expect("the replacement Alert should present"),
        );
    }
}

impl Render for ModalReplacementRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let events = Rc::clone(&self.events);
        let owner = cx.entity().downgrade();
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
                    .on_lifecycle(move |event, cx| {
                        events.borrow_mut().push(RecordedEvent::Lifecycle(*event));
                        let _ = owner.update(cx, |root, cx| {
                            root.reentries += 1;
                            cx.notify();
                        });
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
    cx.update(install_portable_combo_box_keybindings);
    cx.update(|cx| install_text_input_keybindings(cx, TextInputKeybindingProfile::MacOs));
}

fn install_macos_combo_box_profile(cx: &mut VisualTestContext) {
    cx.update(|_, cx| {
        install_combo_box_keybindings(cx, ComboBoxKeybindingProfile::MacOs);
    });
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
        handle: ComboBoxHandle::default(),
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
        handle: ComboBoxHandle::default(),
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

type IconTriggerWindow<'a> = (
    Entity<IconTriggerRoot>,
    Rc<RefCell<Vec<ComboBoxLifecycleEvent>>>,
    &'a mut VisualTestContext,
);

fn icon_trigger_window(cx: &mut TestAppContext, disabled: bool) -> IconTriggerWindow<'_> {
    install_themes(cx);
    cx.set_global(crate::TooltipTheme::new(
        crate::TooltipPaint::new(
            rgba(0x141415ff),
            rgba(0x252530ff),
            rgba(0xcdcdcdff),
            rgba(0x878787ff),
            rgba(0x878787ff),
        ),
        crate::TooltipMetrics::new(px(240.0)),
    ));
    cx.update(crate::tooltip::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let root_events = Rc::clone(&events);
    let (root, cx) = cx.add_window_view(move |_, cx| IconTriggerRoot {
        disabled,
        before_focus: cx.focus_handle().tab_stop(true),
        after_focus: cx.focus_handle().tab_stop(true),
        events: root_events,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (root, events, cx)
}

fn placement_window(
    cx: &mut TestAppContext,
    origin: gpui::Point<gpui::Pixels>,
    placement: AnchoredPlacementConfig,
    viewport: gpui::Size<gpui::Pixels>,
    items: Vec<ComboBoxItem<u8>>,
) -> (Rc<RefCell<Vec<u8>>>, &mut VisualTestContext) {
    install_themes(cx);
    let accepted = Rc::new(RefCell::new(Vec::new()));
    let root_accepted = Rc::clone(&accepted);
    let (_, cx) = cx.add_window_view(move |_, _| PlacementRoot {
        origin,
        placement,
        items,
        accepted: root_accepted,
    });
    cx.simulate_resize(viewport);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (accepted, cx)
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
fn single_result_should_have_equal_insets_on_all_four_sides(cx: &mut TestAppContext) {
    let (_, _, _, cx) = combo_box_window(
        cx,
        None,
        vec![ComboBoxItem::new(1, "One option").debug_selector("single-option")],
        false,
    );
    open_by_pointer(cx);

    let panel = cx.debug_bounds("combo-box-panel").unwrap();
    let input = cx.debug_bounds("combo-box-input-row").unwrap();
    let row = cx.debug_bounds("single-option").unwrap();
    let border = px(1.0);
    assert_eq!(
        [
            row.left() - panel.left() - border,
            panel.right() - row.right() - border,
            row.top() - input.bottom() - border,
            panel.bottom() - row.bottom() - border,
        ],
        [px(4.0); 4],
        "the result viewport should own one uniform inset around its contents"
    );
}

#[gpui::test]
fn filter_editor_should_use_compact_text_and_caret_geometry(cx: &mut TestAppContext) {
    let (_, _, _, cx) = combo_box_window(cx, None, items(), false);
    open_by_pointer(cx);

    let row = cx.debug_bounds("combo-box-input-row").unwrap();
    let editor = cx.debug_bounds("combo-box-input").unwrap();
    assert_eq!(editor.size.height, px(16.0));
    assert_eq!(editor.center().y, row.center().y);

    cx.simulate_input("remote");
    cx.run_until_parked();
    assert_eq!(cx.debug_bounds("combo-box-input").unwrap(), editor);
}

#[gpui::test]
fn secondary_click_and_modified_release_should_not_accept_an_option(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, None, items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();
    let row = cx.debug_bounds("combo-row-local").unwrap().center();
    let control = Modifiers {
        control: true,
        ..Modifiers::none()
    };

    for (button, down, up) in [
        (MouseButton::Right, Modifiers::none(), Modifiers::none()),
        (MouseButton::Left, control, control),
        (MouseButton::Left, Modifiers::none(), control),
    ] {
        cx.simulate_mouse_down(row, button, down);
        cx.simulate_mouse_up(row, button, up);
        cx.run_until_parked();
        assert!(events.borrow().is_empty());
        assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    }

    cx.simulate_mouse_up(row, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert!(
        events.borrow().is_empty(),
        "a cancelled press must not survive a later release"
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
        reentries: 0,
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
    root.update(cx, |root, _| root.reentries = 0);

    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.present_modal(window, cx));
    });
    cx.run_until_parked();

    assert_eq!(
        events.borrow().as_slice(),
        [
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(
                ComboBoxCloseReason::Replaced,
            )),
            RecordedEvent::ModalOpened,
        ]
    );
    assert_eq!(root.read_with(cx, |root, _| root.reentries), 1);
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
fn macos_control_navigation_should_not_open_a_closed_combo_box(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    install_macos_combo_box_profile(cx);
    focus_trigger(cx);

    cx.simulate_keystrokes("ctrl-n ctrl-p");
    cx.run_until_parked();

    assert!(cx.debug_bounds("combo-box-panel").is_none());
    assert!(events.borrow().is_empty());
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
fn portable_arrow_navigation_should_wrap_in_both_directions(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, Some(4), items(), false);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 1,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));

    root.update(cx, |root, cx| {
        root.selected = Some(1);
        cx.notify();
    });
    cx.run_until_parked();
    open_by_pointer(cx);
    events.borrow_mut().clear();
    cx.simulate_keystrokes("up enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 4,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn macos_control_navigation_should_wrap_in_both_directions_while_input_is_focused(
    cx: &mut TestAppContext,
) {
    let (root, events, _, cx) = combo_box_window(cx, Some(4), items(), false);
    install_macos_combo_box_profile(cx);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("ctrl-n enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 1,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));

    root.update(cx, |root, cx| {
        root.selected = Some(1);
        cx.notify();
    });
    cx.run_until_parked();
    open_by_pointer(cx);
    events.borrow_mut().clear();
    cx.simulate_keystrokes("ctrl-p enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 4,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn macos_control_navigation_should_skip_disabled_items(cx: &mut TestAppContext) {
    let (_, events, _, cx) = combo_box_window(cx, Some(1), items(), false);
    install_macos_combo_box_profile(cx);
    open_by_pointer(cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("ctrl-n enter");
    cx.run_until_parked();

    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 3,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn macos_control_navigation_should_leave_an_empty_list_stable(cx: &mut TestAppContext) {
    let (_, empty_events, _, cx) = combo_box_window(cx, None, Vec::new(), false);
    install_macos_combo_box_profile(cx);
    open_by_pointer(cx);
    empty_events.borrow_mut().clear();
    cx.simulate_keystrokes("ctrl-n ctrl-p enter");
    cx.run_until_parked();
    assert!(empty_events.borrow().is_empty());
}

#[gpui::test]
fn macos_control_navigation_should_leave_a_single_item_selected(cx: &mut TestAppContext) {
    let (_, single_events, _, cx) = combo_box_window(
        cx,
        None,
        vec![ComboBoxItem::new(7, "Only Workspace")],
        false,
    );
    install_macos_combo_box_profile(cx);
    open_by_pointer(cx);
    cx.simulate_keystrokes("ctrl-n ctrl-p enter");
    cx.run_until_parked();
    assert!(single_events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 7,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
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
            RecordedEvent::Lifecycle(_)
            | RecordedEvent::PaletteOpened
            | RecordedEvent::ModalOpened => None,
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
            RecordedEvent::Lifecycle(_)
            | RecordedEvent::PaletteOpened
            | RecordedEvent::ModalOpened => None,
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

#[gpui::test]
fn rendered_combo_box_placement_should_flip_left_to_right_at_the_left_edge(
    cx: &mut TestAppContext,
) {
    let (_, cx) = placement_window(
        cx,
        point(px(12.0), px(120.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Left, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(420.0)),
        items(),
    );
    open_by_pointer(cx);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.left(), trigger.right() + px(4.0));
    assert_eq!(panel.top(), trigger.top());
}

#[gpui::test]
fn rendered_combo_box_placement_should_flip_right_to_left_at_the_right_edge(
    cx: &mut TestAppContext,
) {
    let (_, cx) = placement_window(
        cx,
        point(px(488.0), px(120.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Right, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(420.0)),
        items(),
    );
    open_by_pointer(cx);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.right(), trigger.left() - px(4.0));
    assert_eq!(panel.top(), trigger.top());
}

#[gpui::test]
fn rendered_combo_box_placement_should_flip_top_to_bottom_at_the_top_edge(cx: &mut TestAppContext) {
    let (_, cx) = placement_window(
        cx,
        point(px(180.0), px(12.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Top, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(420.0)),
        items(),
    );
    open_by_pointer(cx);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.top(), trigger.bottom() + px(4.0));
    assert_eq!(panel.left(), trigger.left());
}

#[gpui::test]
fn rendered_combo_box_placement_should_flip_bottom_to_top_at_the_bottom_edge(
    cx: &mut TestAppContext,
) {
    let (_, cx) = placement_window(
        cx,
        point(px(180.0), px(368.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(420.0)),
        items(),
    );
    open_by_pointer(cx);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.bottom(), trigger.top() - px(4.0));
    assert_eq!(panel.left(), trigger.left());
}

#[gpui::test]
fn rendered_combo_box_placement_should_flip_alignment_at_the_viewport_corner(
    cx: &mut TestAppContext,
) {
    let (_, cx) = placement_window(
        cx,
        point(px(488.0), px(12.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Top, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(420.0)),
        items(),
    );
    open_by_pointer(cx);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.top(), trigger.bottom() + px(4.0));
    assert_eq!(panel.right(), trigger.right());
    assert!(panel.left() >= px(12.0) && panel.bottom() <= px(408.0));
}

#[gpui::test]
fn rendered_combo_box_placement_should_keep_logical_start_when_flipped_in_rtl(
    cx: &mut TestAppContext,
) {
    let (_, cx) = placement_window(
        cx,
        point(px(300.0), px(368.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, AnchoredAlignment::Start)
            .direction(crate::AnchoredTextDirection::RightToLeft),
        gpui::size(px(600.0), px(420.0)),
        items(),
    );
    open_by_pointer(cx);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.bottom(), trigger.top() - px(4.0));
    assert_eq!(panel.right(), trigger.right());
}

#[gpui::test]
fn rendered_combo_box_placement_should_flip_after_live_viewport_resize(cx: &mut TestAppContext) {
    let (_, cx) = placement_window(
        cx,
        point(px(180.0), px(250.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(700.0)),
        items(),
    );
    open_by_pointer(cx);
    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let initial_panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(initial_panel.top(), trigger.bottom() + px(4.0));

    cx.simulate_resize(gpui::size(px(600.0), px(400.0)));
    cx.run_until_parked();

    let panel = cx.debug_bounds("combo-box-panel").expect("resized popup");
    assert_eq!(panel.bottom(), trigger.top() - px(4.0));
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn rendered_combo_box_placement_should_keep_last_result_reachable_when_neither_side_fits(
    cx: &mut TestAppContext,
) {
    let (accepted, cx) = placement_window(
        cx,
        point(px(180.0), px(180.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(420.0)),
        long_items(),
    );
    open_by_pointer(cx);
    let trigger = cx.debug_bounds("combo-box-trigger").expect("trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.top(), trigger.bottom() + px(4.0));
    assert_eq!(panel.bottom(), px(408.0));
    assert!(cx.debug_bounds("combo-row-64").is_none());

    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    let last_row = cx.debug_bounds("combo-row-64").expect("last result");
    assert!(last_row.top() >= panel.top() && last_row.bottom() <= panel.bottom());

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(accepted.borrow().as_slice(), [64]);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn wrapped_control_navigation_should_reveal_the_first_offscreen_result(cx: &mut TestAppContext) {
    let (accepted, cx) = placement_window(
        cx,
        point(px(180.0), px(180.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(420.0)),
        long_items(),
    );
    install_macos_combo_box_profile(cx);
    open_by_pointer(cx);
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert!(cx.debug_bounds("combo-row-64").is_some());

    cx.simulate_keystrokes("ctrl-n");
    cx.run_until_parked();

    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    let first_row = cx
        .debug_bounds("combo-row-1")
        .expect("wrapped first result");
    assert!(first_row.top() >= panel.top() && first_row.bottom() <= panel.bottom());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(accepted.borrow().as_slice(), [1]);
}

#[gpui::test]
fn rendered_combo_box_placement_should_keep_active_last_result_visible_after_viewport_shrinks(
    cx: &mut TestAppContext,
) {
    let (accepted, cx) = placement_window(
        cx,
        point(px(180.0), px(180.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(700.0)),
        long_items(),
    );
    open_by_pointer(cx);
    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    let initial_panel = cx.debug_bounds("combo-box-panel").expect("popup");
    let initial_last_row = cx.debug_bounds("combo-row-64").expect("active last result");
    assert!(initial_last_row.bottom() <= initial_panel.bottom());

    cx.simulate_resize(gpui::size(px(600.0), px(420.0)));
    cx.run_until_parked();

    let panel = cx.debug_bounds("combo-box-panel").expect("resized popup");
    assert!(panel.size.height < initial_panel.size.height);
    let last_row = cx
        .debug_bounds("combo-row-64")
        .expect("the active last result should remain visible after resizing");
    assert!(last_row.top() >= panel.top() && last_row.bottom() <= panel.bottom());

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(accepted.borrow().as_slice(), [64]);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn repeated_end_should_reveal_the_active_last_result_after_manual_scrolling(
    cx: &mut TestAppContext,
) {
    let (accepted, cx) = placement_window(
        cx,
        point(px(180.0), px(180.0)),
        AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, AnchoredAlignment::Start),
        gpui::size(px(600.0), px(700.0)),
        long_items(),
    );
    open_by_pointer(cx);
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    cx.simulate_mouse_move(panel.center(), None, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    assert!(cx.debug_bounds("combo-row-64").is_some());
    assert!(cx.debug_bounds("combo-row-40").is_none());

    cx.simulate_event(ScrollWheelEvent {
        position: panel.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(640.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("combo-row-40").is_some());

    cx.simulate_keystrokes("end");
    cx.run_until_parked();

    // Click the actual bottom row because GPUI retains stale debug bounds for virtualized rows.
    cx.simulate_click(
        point(panel.center().x, panel.bottom() - px(20.0)),
        Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(accepted.borrow().as_slice(), [64]);
}

#[gpui::test]
fn icon_trigger_should_remain_a_centered_square_without_the_prompt(cx: &mut TestAppContext) {
    let (_, _, cx) = icon_trigger_window(cx, false);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("icon trigger");
    let icon = cx
        .debug_bounds("combo-box-trigger-icon")
        .expect("trigger icon");
    assert_eq!(trigger.size, gpui::size(px(28.0), px(28.0)));
    assert_eq!(icon.center(), trigger.center());
    assert!(cx.debug_bounds("combo-box-trigger-label").is_none());
}

#[gpui::test]
fn icon_trigger_should_open_the_same_popup_on_pointer_press(cx: &mut TestAppContext) {
    let (_, events, cx) = icon_trigger_window(cx, false);
    open_by_pointer(cx);

    let trigger = cx.debug_bounds("combo-box-trigger").expect("icon trigger");
    let panel = cx.debug_bounds("combo-box-panel").expect("popup");
    assert_eq!(panel.size.width, px(240.0));
    assert_eq!(panel.top(), trigger.bottom() + px(4.0));
    assert_eq!(events.borrow().as_slice(), [ComboBoxLifecycleEvent::Opened]);
}

#[gpui::test]
fn icon_trigger_should_open_with_keyboard_and_restore_focus_on_escape(cx: &mut TestAppContext) {
    let (_, events, cx) = icon_trigger_window(cx, false);
    focus_trigger(cx);
    let trigger_focus = cx
        .update(|window, cx| window.focused(cx))
        .expect("trigger focus");

    cx.simulate_keystrokes("space");
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    cx.simulate_keystrokes("escape");

    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, _| trigger_focus.is_focused(window)));
    assert_eq!(
        events.borrow().as_slice(),
        [
            ComboBoxLifecycleEvent::Opened,
            ComboBoxLifecycleEvent::Closed(ComboBoxCloseReason::Escape),
        ]
    );
}

#[gpui::test]
fn disabled_icon_trigger_should_ignore_pointer_and_be_skipped_by_keyboard(cx: &mut TestAppContext) {
    let (root, events, cx) = icon_trigger_window(cx, true);
    let before_focus = root.read_with(cx, |root, _| root.before_focus.clone());
    cx.update(|window, _| before_focus.focus(window));
    open_by_pointer(cx);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, _| before_focus.is_focused(window)));

    cx.update(|window, _| window.focus_next());
    let after_focus = root.read_with(cx, |root, _| root.after_focus.clone());
    assert!(cx.update(|window, _| after_focus.is_focused(window)));
    cx.simulate_keystrokes("space down");
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn icon_trigger_tooltip_should_show_help_when_closed(cx: &mut TestAppContext) {
    let (_, _, cx) = icon_trigger_window(cx, false);
    let center = trigger_center(cx);
    cx.simulate_mouse_move(center, None, Modifiers::none());
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();

    assert!(cx.debug_bounds("icon-combo-help").is_some());
}

#[gpui::test]
fn icon_trigger_tooltip_should_cancel_pending_help_when_popup_opens(cx: &mut TestAppContext) {
    let (_, _, cx) = icon_trigger_window(cx, false);
    let center = trigger_center(cx);
    cx.simulate_mouse_move(center, None, Modifiers::none());
    open_by_pointer(cx);
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();

    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.debug_bounds("icon-combo-help").is_none());
}

#[gpui::test]
fn disabled_icon_trigger_should_not_present_tooltip_help(cx: &mut TestAppContext) {
    let (_, _, cx) = icon_trigger_window(cx, true);
    let center = trigger_center(cx);
    cx.simulate_mouse_move(center, None, Modifiers::none());
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();

    assert!(cx.debug_bounds("icon-combo-help").is_none());
}

#[gpui::test]
fn no_match_fallbacks_should_hide_until_an_unmatched_nonblank_query(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::when_no_matches(|_| {
        vec![
            ComboBoxItem::new(8, "Local Workspace").debug_selector("create-local"),
            ComboBoxItem::new(9, "Remote Workspace").debug_selector("create-remote"),
        ]
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);
    assert!(cx.debug_bounds("create-local").is_none());
    cx.simulate_keystrokes("l o c a l");
    cx.run_until_parked();
    assert!(cx.debug_bounds("create-local").is_none());
    cx.simulate_keystrokes("cmd-a x");
    cx.run_until_parked();
    assert!(cx.debug_bounds("create-local").is_some());
    assert!(cx.debug_bounds("create-remote").is_some());
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 9,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn no_match_fallbacks_should_hide_for_whitespace_when_there_are_no_items(cx: &mut TestAppContext) {
    let (root, _, _, cx) = combo_box_window(cx, None, Vec::new(), false);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.fallback = Some(ComboBoxFallback::when_no_matches(|_| {
                vec![ComboBoxItem::new(9, "Create").debug_selector("create-workspace")]
            }));
            cx.notify();
        })
    });
    cx.run_until_parked();
    open_by_pointer(cx);
    cx.simulate_keystrokes("space space");
    cx.run_until_parked();
    assert!(cx.debug_bounds("create-workspace").is_none());
    assert!(cx.debug_bounds("combo-box-empty").is_some());
}

#[gpui::test]
fn no_match_fallbacks_should_reject_a_pointer_release_after_query_changes(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::when_no_matches(|_| {
        vec![ComboBoxItem::new(9, "Create").debug_selector("create-workspace")]
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    let row = cx.debug_bounds("create-workspace").unwrap();
    cx.simulate_mouse_down(row.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes("y");
    cx.run_until_parked();
    cx.simulate_mouse_up(row.center(), MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, RecordedEvent::Accepted { .. }))
    );
}

#[gpui::test]
fn handle_should_open_the_rendered_popup_and_restore_prior_focus(cx: &mut TestAppContext) {
    let (root, events, _, cx) = combo_box_window(cx, None, items(), false);
    let handle = cx.update(|_, cx| root.read(cx).handle.clone());
    cx.update(|window, cx| root.read(cx).before_focus.focus(window));
    assert!(cx.update(|window, cx| handle.open(window, cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("combo-box-panel").is_some());
    assert!(!cx.update(|window, cx| handle.open(window, cx)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| root.read(cx).before_focus.is_focused(window)));
    assert_eq!(
        *events.borrow(),
        vec![
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Opened),
            RecordedEvent::Lifecycle(ComboBoxLifecycleEvent::Closed(ComboBoxCloseReason::Escape)),
        ]
    );
}

#[gpui::test]
fn handle_should_not_open_a_removed_trigger(cx: &mut TestAppContext) {
    let (root, _, _, cx) = combo_box_window(cx, None, items(), false);
    let handle = cx.update(|_, cx| root.read(cx).handle.clone());
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.show_combo_box = false;
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| handle.open(window, cx)));
}

#[gpui::test]
fn handle_should_not_open_a_disabled_trigger(cx: &mut TestAppContext) {
    let (root, _, _, cx) = combo_box_window(cx, None, items(), true);
    let handle = cx.update(|_, cx| root.read(cx).handle.clone());
    assert!(!cx.update(|window, cx| handle.open(window, cx)));
}

#[gpui::test]
fn no_match_fallbacks_should_yield_to_a_new_ordinary_match(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::when_no_matches(|_| {
        vec![ComboBoxItem::new(9, "Create").debug_selector("create-workspace")]
    });
    let (root, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert!(cx.debug_bounds("create-workspace").is_some());
    root.update(cx, |root, cx| {
        root.items.push(ComboBoxItem::new(7, "Existing x"));
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("Existing x").is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 7,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn handle_should_not_open_its_control_in_another_window(cx: &mut TestAppContext) {
    let (root, _, _, first) = combo_box_window(cx, None, items(), false);
    let handle = first.update(|_, cx| root.read(cx).handle.clone());
    let mut shared_app = first.cx.clone();
    let (_, _, _, second) = combo_box_window(&mut shared_app, None, items(), false);
    assert!(!second.update(|window, cx| handle.open(window, cx)));
    assert!(!first.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn pinned_rows_should_follow_ordinary_matches_when_query_is_empty(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::pinned_rows(|_| {
        vec![
            ComboBoxItem::new(8, "Local Workspace").debug_selector("create-local"),
            ComboBoxItem::new(9, "Remote Workspace").debug_selector("create-remote"),
        ]
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);
    let ordinary = cx.debug_bounds("combo-row-zellij").unwrap();
    let local = cx.debug_bounds("create-local").unwrap();
    let remote = cx.debug_bounds("create-remote").unwrap();
    assert!(local.top() >= ordinary.bottom());
    assert!(remote.top() >= local.bottom());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 1,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn pinned_rows_should_allow_keyboard_creation_with_a_matching_existing_name(
    cx: &mut TestAppContext,
) {
    let queries = Rc::new(RefCell::new(Vec::new()));
    let captured_queries = queries.clone();
    let fallback = ComboBoxFallback::pinned_rows(move |query| {
        captured_queries.borrow_mut().push(query.to_owned());
        vec![
            ComboBoxItem::new(8, "Local Workspace").debug_selector("create-local"),
            ComboBoxItem::new(9, "Remote Workspace").debug_selector("create-remote"),
        ]
    });
    let (_, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);
    cx.simulate_keystrokes("L o c a l space W o r k s p a c e");
    cx.run_until_parked();
    let existing = cx.debug_bounds("combo-row-local").unwrap();
    let local = cx.debug_bounds("create-local").unwrap();
    let remote = cx.debug_bounds("create-remote").unwrap();
    assert!(local.top() >= existing.bottom());
    assert!(remote.top() >= local.bottom());
    assert_eq!(
        queries.borrow().last().map(String::as_str),
        Some("Local Workspace")
    );
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 8,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn macos_control_navigation_should_wrap_across_filtered_results_and_pinned_rows(
    cx: &mut TestAppContext,
) {
    let queries = Rc::new(RefCell::new(Vec::new()));
    let captured_queries = Rc::clone(&queries);
    let fallback = ComboBoxFallback::pinned_rows(move |query| {
        captured_queries.borrow_mut().push(query.to_owned());
        vec![
            ComboBoxItem::new(8, "Local Workspace").debug_selector("create-local"),
            ComboBoxItem::new(9, "Remote Workspace").debug_selector("create-remote"),
        ]
    });
    let (root, events, _, cx) = fallback_combo_box_window(cx, fallback);
    install_macos_combo_box_profile(cx);
    open_by_pointer(cx);
    cx.simulate_keystrokes("L o c a l space W o r k s p a c e ctrl-p");
    cx.run_until_parked();

    assert_eq!(
        queries.borrow().last().map(String::as_str),
        Some("Local Workspace")
    );
    assert_eq!(root.read_with(cx, |root, _| root.selected), None);
    assert!(cx.debug_bounds("combo-row-local").is_some());
    assert!(cx.debug_bounds("create-local").is_some());
    assert!(cx.debug_bounds("create-remote").is_some());

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 9,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn pinned_selection_should_survive_ordinary_item_metadata_refresh(cx: &mut TestAppContext) {
    let fallback = ComboBoxFallback::pinned_rows(|_| {
        vec![
            ComboBoxItem::new(8, "Local Workspace"),
            ComboBoxItem::new(9, "Remote Workspace"),
        ]
    });
    let (root, events, _, cx) = fallback_combo_box_window(cx, fallback);
    open_by_pointer(cx);
    cx.simulate_keystrokes("L o c a l down");
    cx.run_until_parked();
    root.update(cx, |root, cx| {
        root.items[0] = ComboBoxItem::new(1, "Local Workspace")
            .description("Directory changed while the selector was open");
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 8,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}

#[gpui::test]
fn shortcut_should_render_after_the_existing_trailing_accessory(cx: &mut TestAppContext) {
    let items = vec![
        ComboBoxItem::new(1, "Local Workspace")
            .trailing(crate::ComboBoxAccessory::Text("2T · 3P".into()))
            .shortcut("⌘1"),
    ];
    let (_, _, _, cx) = combo_box_window(cx, None, items, false);
    open_by_pointer(cx);
    let accessory = cx.debug_bounds("combo-box-row-0-accessory").unwrap();
    let shortcut = cx.debug_bounds("combo-box-row-0-shortcut").unwrap();
    assert!(shortcut.left() > accessory.right());
    assert_eq!(shortcut.top(), accessory.top());
}

#[gpui::test]
fn shortcut_refresh_should_preserve_keyboard_selection_and_cancel_a_stale_pointer_press(
    cx: &mut TestAppContext,
) {
    let (root, events, _, cx) = combo_box_window(cx, None, items(), false);
    open_by_pointer(cx);
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    let remote = cx.debug_bounds("combo-row-remote").unwrap().center();
    cx.simulate_mouse_down(remote, MouseButton::Left, Modifiers::none());
    root.update(cx, |root, cx| {
        root.items[2] = root.items[2].clone().shortcut("⌘3");
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("combo-box-row-2-shortcut").is_some());
    cx.simulate_mouse_up(remote, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, RecordedEvent::Accepted { .. }))
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(events.borrow().contains(&RecordedEvent::Accepted {
        item_id: 3,
        source: ComboBoxActivationSource::Keyboard,
        window_was_open: false,
    }));
}
