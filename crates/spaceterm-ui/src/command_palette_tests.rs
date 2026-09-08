use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gpui::{
    Context, Entity, FocusHandle, Modifiers, Render, ScrollDelta, ScrollWheelEvent, TestAppContext,
    TouchPhase, VisualTestContext, Window, point, rgba,
};

use super::*;

fn test_theme() -> CommandPaletteTheme {
    CommandPaletteTheme::new(
        CommandPalettePaint::new(
            rgba(0x141415ff),
            rgba(0x252530ff),
            rgba(0xcdcdcdff),
            rgba(0x878787ff),
            rgba(0x606079ff),
            rgba(0x252530ff),
            rgba(0xffffffff),
            rgba(0x7e98e8ff),
        )
        .separator(rgba(0x252530ff))
        .hover_background(rgba(0x1c1c24ff))
        .section_foreground(rgba(0x878787ff))
        .footer(rgba(0x878787ff), rgba(0x606079ff)),
        CommandPaletteMetrics::new(px(420.0), px(40.0))
            .single_line_row_height(px(28.0))
            .footer_padding(px(8.0))
            .panel_geometry(px(260.0), px(24.0)),
    )
}

#[test]
fn row_icons_should_share_selected_and_disabled_text_foregrounds() {
    let paint = test_theme().paint;

    assert_eq!(row_foreground(paint, false, false), paint.foreground);
    assert_eq!(
        row_foreground(paint, false, true),
        paint.selected_foreground
    );
    assert_eq!(row_foreground(paint, true, true), paint.disabled);
}

fn items() -> Vec<CommandPaletteItem<u8>> {
    vec![
        CommandPaletteItem::new(1, "Open Workspace")
            .description("Choose a directory")
            .keywords(["project"])
            .debug_selector("row-open"),
        CommandPaletteItem::new(2, "Disabled Command")
            .disabled(true)
            .debug_selector("row-disabled"),
        CommandPaletteItem::new(3, "Close Window")
            .keywords(["remove"])
            .debug_selector("row-close"),
    ]
}

fn sectioned_results() -> (PresentedResults, CommandPaletteMetrics) {
    let items = vec![
        CommandPaletteItem::new(1, "Recent One").section("Recent"),
        CommandPaletteItem::new(2, "Recent Two").section("Recent"),
        CommandPaletteItem::new(3, "All One").section("All"),
        CommandPaletteItem::new(4, "All Two").section("All"),
        CommandPaletteItem::new(5, "All Three").section("All"),
    ];
    let matches = match_command_palette_items(&items, "", CommandPaletteMatching::Semantic);
    (
        PresentedResults::new(&items, &matches),
        CommandPaletteMetrics::new(px(420.0), px(40.0)),
    )
}

#[test]
fn presented_results_should_own_section_order_and_match_mapping() {
    let (results, _) = sectioned_results();

    assert_eq!(
        (results.rows(), results.list_index_for_match(2)),
        (
            &[
                PaletteRow::Section("Recent".into()),
                PaletteRow::Item {
                    position: 0,
                    single_line: true
                },
                PaletteRow::Item {
                    position: 1,
                    single_line: true
                },
                PaletteRow::Separator,
                PaletteRow::Section("All".into()),
                PaletteRow::Item {
                    position: 2,
                    single_line: true
                },
                PaletteRow::Item {
                    position: 3,
                    single_line: true
                },
                PaletteRow::Item {
                    position: 4,
                    single_line: true
                },
            ][..],
            Some(5),
        )
    );
}

#[test]
fn presented_results_should_measure_and_hit_test_every_row_kind() {
    let (results, metrics) = sectioned_results();

    assert_eq!(
        (
            results.total_height(metrics),
            results.row_at_y(px(0.0), metrics).map(|(index, _)| index),
            results.item_at_y(px(22.0), metrics),
            results.item_at_y(px(105.0), metrics),
            results.item_at_y(px(133.0), metrics),
            results.row_at_y(px(253.0), metrics),
        ),
        (px(253.0), Some(0), Some(0), None, Some(2), None)
    );
}

#[test]
fn page_target_should_include_section_and_separator_heights() {
    let (results, metrics) = sectioned_results();

    assert_eq!(
        results.page_target(Some(0), &[0, 1, 2, 3, 4], px(120.0), 1, metrics),
        Some(2)
    );
}

#[test]
fn page_target_should_skip_disabled_matches_without_ignoring_their_height() {
    let (results, metrics) = sectioned_results();

    assert_eq!(
        results.page_target(Some(4), &[0, 2, 4], px(120.0), -1, metrics),
        Some(2)
    );
}

#[test]
fn empty_query_should_preserve_provider_order() {
    let matches = match_command_palette_items(&items(), "", CommandPaletteMatching::Semantic);

    assert_eq!(
        matches
            .iter()
            .map(|matched| matched.item_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn duplicate_item_identity_should_keep_only_the_first_semantic_item() {
    let items = unique_items(vec![
        CommandPaletteItem::new(1, "First"),
        CommandPaletteItem::new(1, "Replacement"),
    ]);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label(), "First");
}

#[test]
fn fallback_provider_should_preserve_exact_query_in_typed_identity() {
    let fallback = CommandPaletteFallback::new(|query| {
        CommandPaletteItem::new(query.to_owned(), "Use exact query")
    });

    assert_eq!(fallback.item(" Mixed Case ").id(), " Mixed Case ");
}

#[test]
fn matcher_should_search_description_and_keywords() {
    let items = items();

    assert_eq!(
        match_command_palette_items(&items, "directory", CommandPaletteMatching::Semantic)[0]
            .item_index,
        0
    );
    assert_eq!(
        match_command_palette_items(&items, "remove", CommandPaletteMatching::Semantic)[0]
            .item_index,
        2
    );
}

#[test]
fn direct_label_match_should_rank_above_metadata_match() {
    let items = vec![
        CommandPaletteItem::new(1, "Open").keywords(["window"]),
        CommandPaletteItem::new(2, "Window Settings"),
    ];

    assert_eq!(
        match_command_palette_items(&items, "window", CommandPaletteMatching::Semantic)[0]
            .item_index,
        1
    );
}

#[test]
fn unicode_highlights_should_remain_valid_label_boundaries() {
    let items = vec![CommandPaletteItem::new(1, "Éclair 🔍")];
    let matches = match_command_palette_items(&items, "é🔍", CommandPaletteMatching::Semantic);
    let ranges = &matches[0].label_highlights;

    assert_eq!(ranges, &[0..2, 8..12]);
    assert!(ranges.iter().all(|range| {
        items[0].label().is_char_boundary(range.start)
            && items[0].label().is_char_boundary(range.end)
    }));
}

#[test]
fn multiple_query_tokens_may_match_different_semantic_fields() {
    let items = vec![CommandPaletteItem::new(1, "Open Workspace").keywords(["project"])];

    assert_eq!(
        match_command_palette_items(&items, "open project", CommandPaletteMatching::Semantic).len(),
        1
    );
}

struct CloneCountingId {
    value: u16,
    clones: Rc<Cell<usize>>,
}

impl Clone for CloneCountingId {
    fn clone(&self) -> Self {
        self.clones.set(self.clones.get() + 1);
        Self {
            value: self.value,
            clones: Rc::clone(&self.clones),
        }
    }
}

impl PartialEq for CloneCountingId {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for CloneCountingId {}

struct CloneCountingRoot {
    palette: Entity<CommandPalette<CloneCountingId>>,
}

impl Render for CloneCountingRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.palette.clone())
    }
}

struct TestRoot {
    palette: Entity<CommandPalette<u8>>,
    other_focus: FocusHandle,
    intruder_focus: FocusHandle,
    underlay_presses: Rc<RefCell<usize>>,
}

impl Render for TestRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let underlay_press = self.underlay_presses.clone();
        let underlay_scroll = self.underlay_presses.clone();
        div()
            .relative()
            .size_full()
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                *underlay_press.borrow_mut() += 1;
            })
            .on_scroll_wheel(move |_, _, _| {
                *underlay_scroll.borrow_mut() += 1;
            })
            .child(
                div()
                    .debug_selector(|| "prior-focus".to_owned())
                    .track_focus(&self.other_focus)
                    .child("Prior"),
            )
            .child(div().track_focus(&self.intruder_focus).child("Intruder"))
            .child(self.palette.clone())
            .on_action(cx.listener(|_, _: &MoveDown, _, _| {}))
    }
}

type PaletteWindow<'a> = (
    Entity<TestRoot>,
    Entity<CommandPalette<u8>>,
    Rc<RefCell<Vec<CommandPaletteEvent<u8>>>>,
    Rc<RefCell<usize>>,
    &'a mut VisualTestContext,
);

fn install_control_themes(cx: &mut TestAppContext) {
    let variant = crate::button::ButtonVariantStyle::new(
        crate::button::ButtonPaint::new(rgba(0x00000000), rgba(0xcdcdcdff), rgba(0x00000000)),
        crate::button::ButtonPaint::new(rgba(0x252530ff), rgba(0xcdcdcdff), rgba(0x00000000)),
        crate::button::ButtonPaint::new(rgba(0x252530ff), rgba(0xcdcdcdff), rgba(0x00000000)),
        crate::button::ButtonPaint::new(rgba(0x141415ff), rgba(0x606079ff), rgba(0x00000000)),
    );
    let button_metrics = crate::button::ButtonMetrics::new(px(24.0));
    cx.set_global(crate::button::ButtonTheme::new(
        crate::button::ButtonVariants::new(variant, variant, variant, variant, variant, variant),
        crate::button::ButtonSizes::new(
            button_metrics,
            button_metrics,
            button_metrics,
            button_metrics,
        ),
        rgba(0x405065ff),
    ));
    let menu_paint = crate::menu::MenuPaint::new(
        rgba(0x141415ff),
        rgba(0x252530ff),
        rgba(0xcdcdcdff),
        rgba(0x878787ff),
        rgba(0x606079ff),
        rgba(0x252530ff),
        rgba(0xcdcdcdff),
        rgba(0xd8647eff),
        rgba(0x252530ff),
    );
    let menu_metrics = crate::menu::MenuMetrics::new(px(160.0), px(26.0));
    cx.set_global(crate::menu::MenuTheme::new(
        menu_paint,
        crate::menu::MenuSizes::new(menu_metrics, menu_metrics, menu_metrics),
    ));
    cx.update(crate::menu::init);
    cx.set_global(crate::overlay_scrollbar::ScrollbarTheme::new(
        rgba(0x33373878),
        rgba(0x60607978),
        rgba(0xcdcdcdff),
    ));
    let input_paint = crate::text_input::TextInputPaint::new(
        rgba(0xcdcdcdff),
        rgba(0x878787ff),
        rgba(0x6e94b266),
        rgba(0xcdcdcdff),
        rgba(0x606079ff),
        rgba(0x606079ff),
    );
    cx.set_global(crate::text_input::TextInputTheme::new(
        crate::text_input::TextInputVariants::new(input_paint, input_paint),
        crate::text_input::TextInputMetrics::new(
            px(1.0),
            px(2.0),
            std::time::Duration::from_millis(16),
            px(20.0),
        ),
    ));
    cx.update(|cx| {
        crate::text_input::install_text_input_keybindings(
            cx,
            crate::text_input::TextInputKeybindingProfile::MacOs,
        );
    });
}

fn palette_window(cx: &mut TestAppContext) -> PaletteWindow<'_> {
    cx.set_global(test_theme());
    install_control_themes(cx);
    cx.update(crate::text_input::init);
    cx.update(super::init);
    cx.update(|cx| install_command_palette_keybindings(cx, CommandPaletteKeybindingProfile::MacOs));
    let events = Rc::new(RefCell::new(Vec::new()));
    let underlay = Rc::new(RefCell::new(0));
    let root_events = events.clone();
    let root_underlay = underlay.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let palette = cx.new(|cx| CommandPalette::new("Search commands", items(), window, cx));
        cx.subscribe(&palette, move |_, _, event: &CommandPaletteEvent<u8>, _| {
            root_events.borrow_mut().push(event.clone());
        })
        .detach();
        TestRoot {
            palette,
            other_focus: cx.focus_handle().tab_stop(true),
            intruder_focus: cx.focus_handle().tab_stop(true),
            underlay_presses: root_underlay,
        }
    });
    let palette = root.read_with(cx, |root, _| root.palette.clone());
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (root, palette, events, underlay, cx)
}

fn open_palette(
    root: &Entity<TestRoot>,
    palette: &Entity<CommandPalette<u8>>,
    cx: &mut VisualTestContext,
) -> FocusHandle {
    let prior = root.read_with(cx, |root, _| root.other_focus.clone());
    cx.update(|window, _| prior.focus(window));
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.open(window, cx);
        });
    });
    cx.run_until_parked();
    prior
}

#[gpui::test]
fn pinned_fallback_should_receive_exact_query_and_yield_to_ordinary_matches(
    cx: &mut TestAppContext,
) {
    let (root, palette, events, _, cx) = palette_window(cx);
    let queries = Rc::new(RefCell::new(Vec::new()));
    let recorded_queries = Rc::clone(&queries);
    palette.update(cx, |palette, cx| {
        palette.set_fallback(
            Some(CommandPaletteFallback::new(move |query| {
                recorded_queries.borrow_mut().push(query.to_owned());
                CommandPaletteItem::new(9, format!("Create {query}")).debug_selector("row-fallback")
            })),
            cx,
        );
    });
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| palette.set_query(" Mixed Case ", cx));
    cx.run_until_parked();

    assert_eq!(
        queries.borrow().last().map(String::as_str),
        Some(" Mixed Case ")
    );
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(9)
    );
    assert!(cx.debug_bounds("row-fallback").is_some());

    events.borrow_mut().clear();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        events
            .borrow()
            .contains(&CommandPaletteEvent::Activated(CommandPaletteActivation {
                item_id: 9,
                source: CommandPaletteActivationSource::Keyboard,
            }))
    );

    palette.update(cx, |palette, cx| palette.set_query("open", cx));
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
    });
    cx.run_until_parked();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(1)
    );
    let ordinary = cx
        .debug_bounds("row-open")
        .expect("the ordinary match should render");
    let fallback = cx
        .debug_bounds("row-fallback")
        .expect("the pinned fallback should remain visible");
    assert!(fallback.top() >= ordinary.bottom());
}

#[gpui::test]
fn disabled_command_palette_fallback_should_render_without_selection(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| {
        palette.set_fallback(
            Some(CommandPaletteFallback::new(|query| {
                CommandPaletteItem::new(9, format!("Create {query}"))
                    .disabled(true)
                    .debug_selector("row-fallback")
            })),
            cx,
        );
    });
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| {
        palette.set_query("no ordinary result", cx)
    });
    cx.run_until_parked();
    events.borrow_mut().clear();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        None
    );
    assert!(cx.debug_bounds("row-fallback").is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(
        events
            .borrow()
            .iter()
            .all(|event| !matches!(event, CommandPaletteEvent::Activated(_)))
    );
}

#[gpui::test]
fn pointer_should_accept_the_command_palette_fallback(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| {
        palette.set_fallback(
            Some(CommandPaletteFallback::new(|query| {
                CommandPaletteItem::new(9, format!("Create {query}")).debug_selector("row-fallback")
            })),
            cx,
        );
    });
    open_palette(&root, &palette, cx);
    events.borrow_mut().clear();
    let fallback = cx
        .debug_bounds("row-fallback")
        .expect("the fallback row should render");

    cx.simulate_click(fallback.center(), Modifiers::default());
    cx.run_until_parked();

    assert!(
        events
            .borrow()
            .contains(&CommandPaletteEvent::Activated(CommandPaletteActivation {
                item_id: 9,
                source: CommandPaletteActivationSource::Pointer,
            }))
    );
}

#[gpui::test]
fn wheel_scrolling_the_results_should_not_reenter_the_list_state(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    // Overflow the panel so the list actually scrolls and notifies its scroll handler.
    palette.update(cx, |palette, cx| {
        palette.set_items(
            (0..64)
                .map(|index| CommandPaletteItem::new(index, format!("Command {index}")))
                .collect(),
            cx,
        );
    });
    cx.run_until_parked();

    let panel = cx
        .debug_bounds("command-palette-panel")
        .expect("the palette panel was not rendered");
    let selected_before_scroll =
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied());
    cx.simulate_event(ScrollWheelEvent {
        position: panel.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();

    assert!(
        palette.read_with(cx, |palette, _| palette.is_open()),
        "wheel scrolling the results closed the palette"
    );
    assert!(
        cx.debug_bounds("command-palette-panel").is_some(),
        "the palette stopped rendering after a wheel scroll"
    );
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        selected_before_scroll,
        "wheel scrolling changed selection under a stationary pointer"
    );
    assert!(palette.read_with(cx, |palette, _| palette.hover_suppressed));
    assert!(!palette.read_with(cx, |palette, _| palette.pointer_suppressed));

    cx.simulate_mouse_move(panel.center(), None, Modifiers::none());
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| palette.hover_suppressed));

    cx.simulate_mouse_move(
        panel.center() + point(px(1.0), px(0.0)),
        None,
        Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(!palette.read_with(cx, |palette, _| palette.hover_suppressed));
}

#[gpui::test]
fn wheel_scroll_should_not_clone_offscreen_items(cx: &mut TestAppContext) {
    cx.set_global(test_theme());
    install_control_themes(cx);
    cx.update(crate::text_input::init);
    cx.update(super::init);
    cx.update(|cx| install_command_palette_keybindings(cx, CommandPaletteKeybindingProfile::MacOs));
    let offscreen_clones = Rc::new(Cell::new(0));
    let tracked_clones = Rc::clone(&offscreen_clones);
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let items = (0..64)
            .map(|value| {
                let clones = if value == 63 {
                    Rc::clone(&tracked_clones)
                } else {
                    Rc::new(Cell::new(0))
                };
                CommandPaletteItem::new(
                    CloneCountingId { value, clones },
                    format!("Command {value}"),
                )
            })
            .collect();
        let palette = cx.new(|cx| CommandPalette::new("Search", items, window, cx));
        CloneCountingRoot { palette }
    });
    let palette = root.read_with(cx, |root, _| root.palette.clone());
    cx.update(|window, _| window.activate_window());
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.open(window, cx);
        });
    });
    cx.run_until_parked();
    offscreen_clones.set(0);

    let panel = cx
        .debug_bounds("command-palette-panel")
        .expect("the palette panel was not rendered");
    cx.simulate_event(ScrollWheelEvent {
        position: panel.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-24.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();

    assert_eq!(offscreen_clones.get(), 0);
}

#[gpui::test]
fn exact_fit_panel_height_should_include_its_outer_borders(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    let metrics = test_theme().metrics;
    let content_height = palette.read_with(cx, |palette, _| {
        palette.presented_results.total_height(metrics)
    });
    let panel = cx
        .debug_bounds("command-palette-panel")
        .expect("the palette panel was not rendered");

    assert_eq!(
        panel.size.height,
        content_height
            + metrics.input_height
            + metrics.panel_padding * 2.0
            + metrics.border_width * 3.0,
    );
}

#[gpui::test]
fn selected_row_highlight_should_span_the_panel_inset(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    let metrics = test_theme().metrics;
    let panel = cx
        .debug_bounds("command-palette-panel")
        .expect("the palette panel was not rendered");
    let row = cx
        .debug_bounds("row-open")
        .expect("the selected row was not rendered");

    assert_eq!(
        row.size.width,
        panel.size.width - metrics.panel_padding * 2.0 - metrics.border_width * 2.0,
        "the selected row did not span the panel inset: {row:?} in {panel:?}"
    );
}

#[gpui::test]
fn editor_and_row_content_should_share_one_leading_edge(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    let metrics = test_theme().metrics;
    let editor = cx
        .debug_bounds("command-palette-editor")
        .expect("the palette editor was not rendered");
    let row = cx
        .debug_bounds("row-open")
        .expect("the first row was not rendered");

    assert_eq!(
        editor.left() + metrics.content_leading_inset(),
        row.left() + metrics.horizontal_padding,
        "the editor text and row content did not share a leading edge: {editor:?} {row:?}"
    );
}

#[test]
fn caller_matching_should_present_every_item_in_caller_order() {
    let items = items();
    // A path-shaped query fuzzy-matches nothing here, so semantic matching would empty the list.
    let semantic = match_command_palette_items(&items, "~/Doc", CommandPaletteMatching::Semantic);
    assert!(
        semantic.is_empty(),
        "the semantic matcher unexpectedly matched a path query"
    );

    let caller = match_command_palette_items(&items, "~/Doc", CommandPaletteMatching::Caller);
    assert_eq!(
        caller
            .iter()
            .map(|matched| matched.item_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "caller matching did not present every item in caller order"
    );
    assert!(
        caller
            .iter()
            .all(|matched| matched.label_highlights.is_empty()
                && matched.description_highlights.is_empty()),
        "caller matching produced highlights for a query that is not a search term"
    );
}

#[gpui::test]
fn caller_matching_should_survive_a_query_that_matches_nothing(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_matching(CommandPaletteMatching::Caller, cx);
        palette.set_query("~/Doc", cx);
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("row-open").is_some(),
        "caller matching filtered an item the caller supplied"
    );
}

#[gpui::test]
fn a_lone_footer_action_should_be_offered_without_a_disclosure(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_actions_menu(
            vec![
                MenuEntry::action("Choose with Finder", "finder".into())
                    .debug_selector("footer-finder"),
            ],
            cx,
        );
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("command-palette-actions-menu").is_none(),
        "one action still rendered a disclosure"
    );
    let button = cx
        .debug_bounds("footer-finder")
        .expect("the lone action was not offered directly");

    cx.simulate_click(button.center(), Modifiers::none());
    cx.run_until_parked();

    assert!(
        events
            .borrow()
            .contains(&CommandPaletteEvent::MenuAction("finder".into())),
        "activating the lone action did not report its caller identity"
    );
}

#[gpui::test]
fn lone_decorated_footer_entries_should_stay_behind_a_disclosure(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let decorated = vec![
        (
            "checkbox",
            MenuEntry::checkbox("Show hidden", false, "checkbox".into()),
        ),
        (
            "destructive",
            MenuEntry::action("Delete", "destructive".into()).destructive(true),
        ),
        (
            "shortcut",
            MenuEntry::action("Retry", "shortcut".into()).shortcut("⌘R"),
        ),
        (
            "icon",
            MenuEntry::action("Finder", "icon".into()).icon(|_| div().into_any_element()),
        ),
    ];

    for (decoration, entry) in decorated {
        palette.update(cx, |palette, cx| {
            palette.set_actions_menu(vec![entry], cx);
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("command-palette-actions-menu").is_some(),
            "a lone {decoration} entry lost its menu presentation"
        );
    }
}

#[gpui::test]
fn a_row_without_a_description_should_take_the_single_line_height(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_items(
            vec![
                CommandPaletteItem::new(1, "Projects/").debug_selector("row-single"),
                CommandPaletteItem::new(2, "Open Workspace")
                    .description("Choose a directory")
                    .debug_selector("row-described"),
            ],
            cx,
        );
    });
    cx.run_until_parked();

    let metrics = test_theme().metrics;
    let single = cx
        .debug_bounds("row-single")
        .expect("the single-line row was not rendered");
    let described = cx
        .debug_bounds("row-described")
        .expect("the described row was not rendered");

    assert_eq!(single.size.height, metrics.single_line_row_height);
    assert_eq!(described.size.height, metrics.row_height);
}

#[gpui::test]
fn footer_control_labels_should_share_the_content_edges(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_actions_menu(
            vec![
                MenuEntry::action("Choose with Finder", "finder".into())
                    .debug_selector("footer-finder"),
            ],
            cx,
        );
        palette.set_confirm(
            Some(
                CommandPaletteConfirm::new("Add", "Primary+Enter").debug_selector("footer-confirm"),
            ),
            cx,
        );
    });
    cx.run_until_parked();

    let metrics = test_theme().metrics;
    // The padding install_control_themes gives a Small text button around its label.
    let label_inset = px(8.0);
    let row = cx
        .debug_bounds("row-open")
        .expect("the first row was not rendered");
    let action = cx
        .debug_bounds("footer-finder")
        .expect("the footer action was not rendered");
    let confirm = cx
        .debug_bounds("footer-confirm")
        .expect("the confirm was not rendered");

    // The controls hang outward by their own label padding, so their text, not their boxes,
    // lines up with the row content above.
    assert_eq!(
        action.left() + label_inset,
        row.left() + metrics.horizontal_padding,
        "the footer action's label did not share the content leading edge: {action:?} {row:?}"
    );
    assert_eq!(
        confirm.right() - label_inset,
        row.right() - metrics.horizontal_padding,
        "the confirm's label did not share the content trailing edge: {confirm:?} {row:?}"
    );
}

#[gpui::test]
fn the_footer_should_anchor_actions_and_the_confirm_to_opposite_edges(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_actions_menu(
            vec![
                MenuEntry::action("Choose with Finder", "finder".into())
                    .debug_selector("footer-finder"),
            ],
            cx,
        );
        palette.set_confirm(
            Some(
                CommandPaletteConfirm::new("Add", "Primary+Enter").debug_selector("footer-confirm"),
            ),
            cx,
        );
    });
    cx.run_until_parked();

    let footer = cx
        .debug_bounds("command-palette-footer")
        .expect("the footer was not rendered");
    let action = cx
        .debug_bounds("footer-finder")
        .expect("the footer action was not rendered");
    let confirm = cx
        .debug_bounds("footer-confirm")
        .expect("the confirm was not rendered");

    assert!(
        action.right() < confirm.left(),
        "the action and the confirm were not separated: {action:?} {confirm:?}"
    );
    let leading_gap = action.left() - footer.left();
    let trailing_gap = footer.right() - confirm.right();
    assert!(
        leading_gap < footer.size.width / 4.0 && trailing_gap < footer.size.width / 4.0,
        "the footer clustered its controls instead of anchoring both edges: \
             leading {leading_gap:?} trailing {trailing_gap:?} in {footer:?}"
    );
}

#[gpui::test]
fn several_footer_actions_should_stay_behind_a_disclosure(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_actions_menu(
            vec![
                MenuEntry::action("Choose with Finder", "finder".into())
                    .debug_selector("footer-finder"),
                MenuEntry::action("Retry", "retry".into()),
            ],
            cx,
        );
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("command-palette-actions-menu").is_some(),
        "several actions did not render a disclosure"
    );
    assert!(
        cx.debug_bounds("footer-finder").is_none(),
        "a menu entry rendered outside its closed menu"
    );
}

#[gpui::test]
fn continuing_activation_should_report_the_item_without_closing(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(
        events.borrow().iter().any(|event| matches!(
            event,
            CommandPaletteEvent::Activated(activation) if *activation.item_id() == 1
        )),
        "a continuing activation did not report its item"
    );
    assert!(
        palette.read_with(cx, |palette, _| palette.is_open()),
        "a continuing activation closed the palette"
    );
    assert!(
        !events.borrow().iter().any(|event| matches!(
            event,
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(_))
        )),
        "a continuing activation published a close lifecycle event"
    );
}

#[gpui::test]
fn confirm_should_render_once_and_report_its_keyboard_equivalent(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    assert!(
        cx.debug_bounds("command-palette-footer").is_none(),
        "a palette without a confirm rendered a footer"
    );

    palette.update(cx, |palette, cx| {
        palette.set_confirm(
            Some(
                CommandPaletteConfirm::new("Add", "Primary+Enter")
                    .debug_selector("palette-confirm"),
            ),
            cx,
        );
    });
    cx.run_until_parked();

    let footer = cx
        .debug_bounds("command-palette-footer")
        .expect("an installed confirm did not render a footer");
    let confirm = cx
        .debug_bounds("palette-confirm")
        .expect("the confirm control was not rendered");
    assert!(
        footer.contains(&confirm.center()),
        "the confirm control rendered outside the footer: {footer:?} {confirm:?}"
    );

    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();

    assert_eq!(
        events
            .borrow()
            .iter()
            .filter(|event| **event == CommandPaletteEvent::Confirmed)
            .count(),
        1,
        "the confirm key did not emit exactly one confirmation"
    );
}

#[gpui::test]
fn a_disabled_confirm_should_ignore_its_keyboard_equivalent(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_confirm(
            Some(CommandPaletteConfirm::new("Add", "Primary+Enter").disabled(true)),
            cx,
        );
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();

    assert!(
        !events.borrow().contains(&CommandPaletteEvent::Confirmed),
        "a disabled confirm reported a confirmation"
    );
}

#[gpui::test]
fn the_confirm_key_should_stay_unclaimed_without_a_confirm(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();

    assert!(
        !events.borrow().contains(&CommandPaletteEvent::Confirmed),
        "a palette without a confirm claimed the confirm key"
    );
    assert!(
        palette.read_with(cx, |palette, _| palette.is_open()),
        "the confirm key closed a palette that had no confirm"
    );
}

#[gpui::test]
fn footer_should_render_only_with_hints_or_an_actions_menu(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    assert!(
        cx.debug_bounds("command-palette-footer").is_none(),
        "a palette without hints or an actions menu rendered a footer"
    );

    palette.update(cx, |palette, cx| {
        palette.set_hints(vec![CommandPaletteHint::new("Open", "\u{21b5}")], cx);
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("command-palette-footer").is_some(),
        "a palette with hints did not render a footer"
    );
}

#[gpui::test]
fn header_action_press_should_emit_its_caller_identity(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_header_actions(
            vec![
                CommandPaletteAction::new("toggle-ignored", "Toggle ignored", |_| {
                    div().into_any_element()
                })
                .debug_selector("header-toggle-ignored"),
            ],
            cx,
        );
    });
    cx.run_until_parked();

    let button = cx
        .debug_bounds("header-toggle-ignored")
        .expect("the search-line control was not rendered");
    cx.simulate_click(button.center(), Modifiers::default());
    cx.run_until_parked();

    assert!(
        events
            .borrow()
            .contains(&CommandPaletteEvent::HeaderAction("toggle-ignored".into())),
        "the search-line control did not emit its caller identity: {:?}",
        events.borrow()
    );
    assert!(
        palette.read_with(cx, |palette, _| palette.is_open()),
        "pressing a search-line control closed the palette"
    );
}

#[gpui::test]
fn actions_menu_should_take_focus_without_closing_the_palette(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_actions_menu(
            vec![
                MenuEntry::action("Copy path", SharedString::from("copy-path")),
                MenuEntry::action("Reveal", SharedString::from("reveal")),
            ],
            cx,
        );
    });
    cx.run_until_parked();

    let trigger = cx
        .debug_bounds("command-palette-actions-menu")
        .expect("the actions menu trigger was not rendered");
    cx.simulate_click(trigger.center(), Modifiers::default());
    cx.run_until_parked();

    assert!(
        palette.read_with(cx, |palette, _| palette.is_open()),
        "opening the actions menu closed the palette"
    );
}

#[gpui::test]
fn actions_menu_external_focus_should_close_the_palette(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let intruder = root.read_with(cx, |root, _| root.intruder_focus.clone());
    cx.update(|window, cx| {
        root.update(cx, |_, cx| {
            let intruder = intruder.clone();
            cx.subscribe_in(
                &palette,
                window,
                move |_, _, event: &CommandPaletteEvent<u8>, window, _| {
                    if matches!(event, CommandPaletteEvent::MenuAction(_)) {
                        intruder.focus(window);
                    }
                },
            )
            .detach();
        });
        palette.update(cx, |palette, cx| {
            palette.set_actions_menu(
                vec![
                    MenuEntry::action(
                        "Focus external control",
                        SharedString::from("focus-external"),
                    )
                    .debug_selector("command-palette-focus-external"),
                    MenuEntry::action("Second action", SharedString::from("second")),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();

    let trigger = cx
        .debug_bounds("command-palette-actions-menu")
        .expect("the actions menu trigger was not rendered");
    cx.simulate_click(trigger.center(), Modifiers::default());
    cx.run_until_parked();
    let entry = cx
        .debug_bounds("command-palette-focus-external")
        .expect("the actions menu entry was not rendered");
    cx.simulate_click(entry.center(), Modifiers::default());
    cx.run_until_parked();

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(cx.update(|window, _| intruder.is_focused(window)));
    assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
        CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::FocusLost)
    )));
}

#[gpui::test]
fn section_boundaries_should_emit_one_heading_each(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    palette.update(cx, |palette, cx| {
        palette.set_items(
            vec![
                CommandPaletteItem::new(1, "Recent One").section("Recent"),
                CommandPaletteItem::new(2, "Recent Two").section("Recent"),
                CommandPaletteItem::new(3, "All One").section("All"),
            ],
            cx,
        );
    });
    cx.run_until_parked();

    let rows = palette.read_with(cx, |palette, _| palette.presented_results.rows().to_vec());
    assert_eq!(
        rows,
        vec![
            PaletteRow::Section("Recent".into()),
            PaletteRow::Item {
                position: 0,
                single_line: true
            },
            PaletteRow::Item {
                position: 1,
                single_line: true
            },
            PaletteRow::Separator,
            PaletteRow::Section("All".into()),
            PaletteRow::Item {
                position: 2,
                single_line: true
            },
        ]
    );
}

#[test]
fn scored_queries_should_preserve_contiguous_section_order() {
    let items = vec![
        CommandPaletteItem::new(1, "Open Alpha Workspace").section("Recent"),
        CommandPaletteItem::new(2, "Another Alpha Workspace").section("Recent"),
        CommandPaletteItem::new(3, "Alpha").section("All"),
        CommandPaletteItem::new(4, "Alpha Command").section("All"),
    ];
    let matches = match_command_palette_items(&items, "alpha", CommandPaletteMatching::Semantic);

    assert_eq!(
        matches
            .iter()
            .map(|matched| items[matched.item_index].section_text().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["Recent", "Recent", "All", "All"],
    );
}

#[test]
fn description_matches_should_report_their_own_highlight_ranges() {
    let items = vec![CommandPaletteItem::new(1, "Open").description("Choose a directory")];
    let matches =
        match_command_palette_items(&items, "directory", CommandPaletteMatching::Semantic);

    assert!(matches[0].label_highlights.is_empty());
    assert_eq!(matches[0].description_highlights, vec![9..18]);
}

#[gpui::test]
fn escape_should_close_and_restore_exact_prior_focus(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    let prior = open_palette(&root, &palette, cx);
    assert!(cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) }));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(!cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) }));
    assert!(cx.update(|window, _| prior.is_focused(window)));
    assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
        CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Escape)
    )));
}

#[gpui::test]
fn escape_should_close_after_focus_moves_to_a_header_action(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| {
        palette.set_header_actions(
            vec![CommandPaletteAction::new("refresh", "Refresh", |_| {
                div().into_any_element()
            })],
            cx,
        );
    });
    cx.run_until_parked();
    open_palette(&root, &palette, cx);

    cx.update(|window, _| window.focus_next());
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| {
        palette
            .read(cx)
            .input
            .read(cx)
            .focus_handle()
            .is_focused(window)
    }));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
}

#[gpui::test]
fn command_period_should_close_and_restore_exact_prior_focus(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    let prior = open_palette(&root, &palette, cx);

    cx.simulate_keystrokes("cmd-.");
    cx.run_until_parked();

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(cx.update(|window, _| prior.is_focused(window)));
}

#[gpui::test]
fn navigation_should_wrap_and_skip_disabled_items(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);

    cx.simulate_keystrokes("ctrl-n ctrl-n ctrl-p");
    cx.run_until_parked();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(3)
    );
}

#[gpui::test]
fn pointer_hover_should_stay_suppressed_until_the_pointer_moves(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let close_row = cx.debug_bounds("row-close").unwrap_or_default().center();

    assert!(palette.read_with(cx, |palette, _| palette.pointer_suppressed));

    cx.simulate_mouse_move(close_row, None, Modifiers::default());
    cx.simulate_keystrokes("down");
    cx.run_until_parked();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(1)
    );
    assert!(palette.read_with(cx, |palette, _| palette.pointer_suppressed));

    cx.simulate_mouse_move(close_row, None, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(1)
    );
    assert!(palette.read_with(cx, |palette, _| palette.pointer_suppressed));

    cx.simulate_mouse_move(
        close_row + gpui::point(px(1.0), px(0.0)),
        None,
        Modifiers::default(),
    );
    cx.run_until_parked();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(3)
    );
    assert!(!palette.read_with(cx, |palette, _| palette.pointer_suppressed));
}

#[gpui::test]
fn page_navigation_should_move_by_the_visible_result_count(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| {
        palette.set_items(
            (0u8..32)
                .map(|id| CommandPaletteItem::new(id, format!("Command {id}")))
                .collect(),
            cx,
        );
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("pagedown");
    cx.run_until_parked();

    assert!(
        palette
            .read_with(cx, |palette, _| palette.selected_item_id().copied())
            .is_some_and(|selected| selected > 0)
    );
}

#[gpui::test]
fn keyboard_navigation_should_reveal_results_beyond_the_initial_viewport(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| {
        palette.set_items(
            (0u8..32)
                .map(|id| {
                    CommandPaletteItem::new(id, format!("Command {id}"))
                        .debug_selector(format!("row-{id}"))
                })
                .collect(),
            cx,
        );
    });
    open_palette(&root, &palette, cx);

    cx.simulate_keystrokes(&["down"; 12].join(" "));
    cx.run_until_parked();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(12)
    );
    assert!(
        cx.debug_bounds("row-12").is_some(),
        "keyboard navigation selected an offscreen result without revealing it"
    );
}

#[gpui::test]
fn tab_navigation_should_reach_header_controls_and_return_to_the_query(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| {
        palette.set_header_actions(
            vec![CommandPaletteAction::new("refresh", "Refresh", |_| {
                div().into_any_element()
            })],
            cx,
        );
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| {
        palette
            .read(cx)
            .input
            .read(cx)
            .focus_handle()
            .is_focused(window)
    }));

    cx.simulate_keystrokes("shift-tab x");
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(cx.update(|window, cx| {
        palette
            .read(cx)
            .input
            .read(cx)
            .focus_handle()
            .is_focused(window)
    }));
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.query().to_owned()),
        "x"
    );
}

#[gpui::test]
fn shift_tab_from_query_should_reach_a_confirm_only_footer(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| {
        palette.set_confirm(Some(CommandPaletteConfirm::new("Add", "Primary+Enter")), cx);
    });
    open_palette(&root, &palette, cx);

    cx.simulate_keystrokes("shift-tab");
    cx.run_until_parked();

    assert!(!cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));

    cx.simulate_keystrokes("tab x");
    cx.run_until_parked();

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.query().to_owned()),
        "x"
    );
}

#[gpui::test]
fn preferred_item_should_seed_each_open_transition(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| {
        palette.set_preferred_item(Some(3), cx);
    });

    open_palette(&root, &palette, cx);

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(3)
    );
}

#[gpui::test]
fn opening_should_reveal_a_preferred_item_below_the_initial_viewport(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| {
        palette.set_items(
            (0u8..32)
                .map(|id| {
                    CommandPaletteItem::new(id, format!("Command {id}"))
                        .debug_selector(format!("row-{id}"))
                })
                .collect(),
            cx,
        );
        palette.set_preferred_item(Some(31), cx);
    });

    open_palette(&root, &palette, cx);

    assert!(cx.debug_bounds("row-31").is_some());
}

#[gpui::test]
fn loading_state_should_not_activate_a_hidden_stale_selection(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| palette.set_loading(true, cx));

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, CommandPaletteEvent::Activated(_)))
    );
}

#[gpui::test]
fn activation_should_restore_focus_then_emit_activation_before_final_close(
    cx: &mut TestAppContext,
) {
    let (root, palette, events, _, cx) = palette_window(cx);
    let prior = open_palette(&root, &palette, cx);
    events.borrow_mut().clear();

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(cx.update(|window, _| prior.is_focused(window)));
    assert_eq!(
        events.borrow().as_slice(),
        [
            CommandPaletteEvent::Activated(CommandPaletteActivation {
                item_id: 1,
                source: CommandPaletteActivationSource::Keyboard,
            }),
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(
                CommandPaletteCloseReason::Activated,
            )),
        ]
    );
}

#[gpui::test]
fn pointer_press_and_release_must_belong_to_the_same_row(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let first = cx.debug_bounds("row-open").unwrap_or_default().center();
    let second = cx.debug_bounds("row-close").unwrap_or_default().center();

    cx.simulate_mouse_down(first, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(second, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(second, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, CommandPaletteEvent::Activated(_)))
    );
}

#[gpui::test]
fn programmatic_query_uses_the_inputs_normalized_bounded_value(cx: &mut TestAppContext) {
    let (_, palette, events, _, cx) = palette_window(cx);
    palette.update(cx, |palette, cx| palette.set_query("open\r\nwindow", cx));
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.query().to_owned()),
        "open window"
    );
    events.borrow_mut().clear();

    palette.update(cx, |palette, cx| {
        palette.set_query("x".repeat(65 * 1024), cx)
    });

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.query().to_owned()),
        "open window"
    );
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn pointer_release_should_not_activate_a_row_removed_by_a_query_change(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.pointer_down(3, cx);
            palette.set_query("open", cx);
            if palette.pointer_up(&3, true) {
                palette.selected = Some(3);
                palette.activate_selected(CommandPaletteActivationSource::Pointer, window, cx);
            }
        });
    });

    assert!(palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(
        !events
            .borrow()
            .iter()
            .any(|event| matches!(event, CommandPaletteEvent::Activated(_)))
    );
}

#[gpui::test]
fn pointer_click_should_emit_typed_pointer_activation_for_any_visible_row(cx: &mut TestAppContext) {
    let (root, _, events, _, cx) = palette_window(cx);
    let palette = root.read_with(cx, |root, _| root.palette.clone());
    open_palette(&root, &palette, cx);
    let last = cx.debug_bounds("row-close").unwrap_or_default().center();

    cx.simulate_click(last, Modifiers::default());
    cx.run_until_parked();

    assert!(
        events
            .borrow()
            .contains(&CommandPaletteEvent::Activated(CommandPaletteActivation {
                item_id: 3,
                source: CommandPaletteActivationSource::Pointer,
            }))
    );
}

#[gpui::test]
fn implicit_dismissal_lock_should_retain_palette_until_explicit_completion(
    cx: &mut TestAppContext,
) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| palette.set_dismissible(false, cx));

    cx.simulate_keystrokes("escape");
    let panel = cx.debug_bounds("command-palette-panel").unwrap_or_default();
    let outside = point(panel.left() - px(8.0), panel.bottom() + px(8.0));
    cx.simulate_click(outside, Modifiers::default());
    let intruder = root.read_with(cx, |root, _| root.intruder_focus.clone());
    cx.update(|window, _| intruder.focus(window));
    cx.deactivate_window();
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(!events.borrow().iter().any(|event| matches!(
        event,
        CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(_))
    )));

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx);
        });
    });

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
}

#[gpui::test]
fn read_only_query_should_reject_user_edits_but_accept_owner_updates(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| {
        palette.set_query("locked", cx);
        palette.set_query_editable(false, cx);
    });

    cx.simulate_keystrokes("cmd-a x");
    cx.run_until_parked();
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.query().to_owned()),
        "locked"
    );

    palette.update(cx, |palette, cx| palette.set_query("owner", cx));
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.query().to_owned()),
        "owner"
    );
}

#[gpui::test]
fn focus_loss_should_dismiss_without_stealing_focus_from_the_new_owner(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let intruder = root.read_with(cx, |root, _| root.intruder_focus.clone());

    cx.update(|window, _| intruder.focus(window));
    cx.run_until_parked();

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(cx.update(|window, _| intruder.is_focused(window)));
    assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
        CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::FocusLost)
    )));
}

#[gpui::test]
fn replacement_should_close_without_restoring_the_displaced_owner(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    let prior = open_palette(&root, &palette, cx);

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            let _ = palette.dismiss_for_replacement(window, cx);
        });
    });
    cx.run_until_parked();

    assert!(!cx.update(|window, _| prior.is_focused(window)));
    assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
        CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Replaced)
    )));
}

#[gpui::test]
fn replacement_chain_should_restore_the_original_focus_owner(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    let prior = open_palette(&root, &palette, cx);

    let replacement = cx
        .update(|window, cx| {
            palette.update(cx, |palette, cx| {
                palette.dismiss_for_replacement(window, cx)
            })
        })
        .expect("an open palette should transfer its restoration focus");
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.open_replacing(replacement, window, cx);
            palette.dismiss(window, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.update(|window, _| prior.is_focused(window)));
}

#[gpui::test]
fn outside_press_should_close_without_reaching_underlay(cx: &mut TestAppContext) {
    let (root, palette, _, underlay, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let panel = cx.debug_bounds("command-palette-panel").unwrap_or_default();
    let outside = point(panel.left() - px(8.0), panel.bottom() + px(8.0));

    cx.simulate_click(outside, Modifiers::default());
    cx.run_until_parked();

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
    assert_eq!(*underlay.borrow(), 0);
}

#[gpui::test]
fn palette_wheel_events_should_not_reach_the_underlay(cx: &mut TestAppContext) {
    let (root, palette, _, underlay, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let panel = cx.debug_bounds("command-palette-panel").unwrap_or_default();
    let outside = point(panel.left() - px(8.0), panel.bottom() + px(8.0));

    cx.simulate_event(ScrollWheelEvent {
        position: outside,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-20.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
    assert_eq!(*underlay.borrow(), 0);

    palette.update(cx, |palette, cx| palette.set_loading(true, cx));
    cx.run_until_parked();
    let status = cx
        .debug_bounds("command-palette-loading")
        .unwrap_or_default();
    cx.simulate_event(ScrollWheelEvent {
        position: status.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-20.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();

    assert_eq!(*underlay.borrow(), 0);
}

#[gpui::test]
fn stable_selection_should_survive_query_and_item_refresh(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    palette.update(cx, |palette, cx| {
        palette.set_query("window", cx);
        palette.set_items(items(), cx);
    });

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
        Some(3)
    );
}

#[gpui::test]
fn stale_generation_results_should_be_ignored(cx: &mut TestAppContext) {
    let (_, palette, _, _, cx) = palette_window(cx);
    let first = palette.update(cx, |palette, cx| palette.refresh(cx));
    let second = palette.update(cx, |palette, cx| palette.refresh(cx));

    let applied = palette.update(cx, |palette, cx| {
        palette.apply_items(first, vec![CommandPaletteItem::new(9, "Stale")], cx)
    });

    assert!(!applied);
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.generation()),
        second
    );
}

#[gpui::test]
fn closed_palette_should_reject_results_from_the_dismissed_generation(cx: &mut TestAppContext) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let dismissed_generation = palette.read_with(cx, |palette, _| palette.generation());
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.dismiss(window, cx);
        });
    });

    let applied = palette.update(cx, |palette, cx| {
        palette.apply_items(
            dismissed_generation,
            vec![CommandPaletteItem::new(9, "Stale")],
            cx,
        )
    });

    assert!(!applied);
}

#[derive(Debug, Eq, PartialEq)]
struct PaletteStateSnapshot {
    item_ids: Vec<u8>,
    item_labels: Vec<String>,
    match_indexes: Vec<usize>,
    presented_results: PresentedResults,
    matching: CommandPaletteMatching,
    activation: CommandPaletteActivationPolicy,
    selected: Option<u8>,
    preferred: Option<u8>,
    query: String,
    input_value: String,
    generation: CommandPaletteGeneration,
    loading: bool,
    dismissible: bool,
    open: bool,
    restore_focus: bool,
    restore_on_activation: bool,
    pointer_suppressed: bool,
    hover_suppressed: bool,
    registration: Option<CommandPaletteRegistration>,
}

fn palette_state_snapshot(
    palette: &Entity<CommandPalette<u8>>,
    cx: &VisualTestContext,
) -> PaletteStateSnapshot {
    palette.read_with(cx, |palette, cx| PaletteStateSnapshot {
        item_ids: palette.items.iter().map(|item| item.id).collect(),
        item_labels: palette
            .items
            .iter()
            .map(|item| item.label.to_string())
            .collect(),
        match_indexes: palette
            .matches
            .iter()
            .map(|matched| matched.item_index)
            .collect(),
        presented_results: (*palette.presented_results).clone(),
        matching: palette.matching,
        activation: palette.activation,
        selected: palette.selected,
        preferred: palette.preferred,
        query: palette.query.clone(),
        input_value: palette.input.read(cx).value().to_owned(),
        generation: palette.generation,
        loading: palette.loading,
        dismissible: palette.dismissible,
        open: palette.open,
        restore_focus: palette.restore_focus.is_some(),
        restore_on_activation: palette.restore_on_activation.is_some(),
        pointer_suppressed: palette.pointer_suppressed,
        hover_suppressed: palette.hover_suppressed,
        registration: palette.coordinator_registration,
    })
}

struct ModalPaletteMenuBody;

impl Render for ModalPaletteMenuBody {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        crate::Menu::new(
            "palette-modal-menu",
            "Modal options",
            vec![crate::MenuEntry::action("Option", ())],
        )
        .debug_selector("palette-modal-menu-trigger")
        .on_activate(|_, _, _| {})
    }
}

struct ModalPaletteRoot {
    palette: Entity<CommandPalette<u8>>,
    dialog_body: Entity<ModalPaletteMenuBody>,
    prior_focus: FocusHandle,
    modal: Option<crate::ModalPresentationHandle>,
    dialog: Option<crate::DialogCompletion>,
    progress: Option<crate::ProgressDialogHandle>,
    palette_open_events: usize,
}

impl ModalPaletteRoot {
    fn present_alert(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.modal = Some(
            crate::Alert::new(
                crate::ModalId::new("palette-suspension-alert"),
                "Palette suspension",
                "Continue?",
                "The command palette should resume unchanged.",
                vec![
                    crate::ModalAction::new(
                        "ok",
                        "OK",
                        crate::ModalActionRole::Affirmative,
                        "palette-alert-ok",
                    )
                    .default_action(true),
                    crate::ModalAction::new(
                        "cancel",
                        "Cancel",
                        crate::ModalActionRole::Cancel,
                        "palette-alert-cancel",
                    ),
                ],
            )
            .present(window, cx, |_, _| {})
            .expect("alert should present"),
        );
    }

    fn present_dialog_with_menu(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.dialog = Some(
            crate::Dialog::new(
                crate::ModalId::new("palette-dialog-menu"),
                "Palette dialog menu",
                "Dialog",
                vec![
                    crate::ModalAction::new(
                        "save",
                        "Save",
                        crate::ModalActionRole::Affirmative,
                        "palette-dialog-save",
                    ),
                    crate::ModalAction::new(
                        "cancel",
                        "Cancel",
                        crate::ModalActionRole::Cancel,
                        "palette-dialog-cancel",
                    ),
                ],
                crate::DialogInitialFocus::Action("save"),
            )
            .body(self.dialog_body.clone())
            .present(
                window,
                cx,
                |_, _, _| crate::DialogCloseDecision::Pending,
                |_, _| {},
            )
            .expect("Dialog should present"),
        );
    }

    fn present_programmatic_progress(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.progress = Some(
            crate::ProgressDialog::<()>::new(
                crate::ModalId::new("palette-suspension-progress"),
                "Palette suspension progress",
                "Working",
                "Waiting",
                crate::ProgressState::Indeterminate,
                crate::ProgressCancellation::programmatic_only(Duration::from_secs(30)),
            )
            .present(
                window,
                cx,
                |_, _, _| crate::ProgressCancelDecision::Deny,
                |_, _| {},
            )
            .expect("programmatic ProgressDialog should present"),
        );
    }
}

impl Render for ModalPaletteRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        crate::ModalLayer::new(crate::TooltipLayer::new(
            div()
                .size_full()
                .track_focus(&self.prior_focus)
                .child(self.palette.clone()),
        ))
    }
}

fn modal_palette_window(
    cx: &mut TestAppContext,
) -> (
    Entity<ModalPaletteRoot>,
    Entity<CommandPalette<u8>>,
    &'_ mut VisualTestContext,
) {
    cx.set_global(test_theme());
    install_control_themes(cx);
    cx.update(crate::text_input::init);
    cx.update(crate::menu::init);
    cx.update(super::init);
    cx.update(|cx| install_command_palette_keybindings(cx, CommandPaletteKeybindingProfile::MacOs));
    cx.update(crate::tooltip::init);
    cx.update(crate::modal::init);
    cx.update(|cx| {
        crate::install_modal_policy(cx, crate::ModalDesktopPolicy::mac_os());
        let color = rgba(0x202024ff);
        crate::install_modal_theme(
            cx,
            crate::ModalTheme::new(
                crate::ModalPaint::new(
                    rgba(0x00000099),
                    color,
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
    let (root, cx) = cx.add_window_view(|window, cx| ModalPaletteRoot {
        palette: cx.new(|cx| CommandPalette::new("Search commands", items(), window, cx)),
        dialog_body: cx.new(|_| ModalPaletteMenuBody),
        prior_focus: cx.focus_handle().tab_stop(true),
        modal: None,
        dialog: None,
        progress: None,
        palette_open_events: 0,
    });
    let palette = root.read_with(cx, |root, _| root.palette.clone());
    root.update(cx, |_, cx| {
        cx.subscribe(&palette, |root, _, event, _| {
            if matches!(
                event,
                CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened)
            ) {
                root.palette_open_events += 1;
            }
        })
        .detach();
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (root, palette, cx)
}

#[gpui::test]
fn palette_open_requested_after_modal_presentation_should_wait_without_mutating_state(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    palette.update(cx, |palette, cx| palette.set_query("window", cx));
    let before = palette.read_with(cx, |palette, cx| {
        (
            palette.query.clone(),
            palette.input.read(cx).value().to_owned(),
            palette.selected,
            palette.generation,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();
    let replacement_focus = root.read_with(cx, |root, _| CommandPaletteReplacementFocus {
        restore_focus: Some(root.prior_focus.downgrade()),
    });

    let open_results = cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            let first = palette.open(window, cx);
            let replacing = palette.open_replacing(replacement_focus, window, cx);
            palette.focus_editor(window, cx);
            palette.focus_editor(window, cx);
            (first, replacing)
        })
    });
    cx.run_until_parked();
    let blocked = palette.read_with(cx, |palette, cx| {
        (
            palette.open,
            palette.pending_open.is_some(),
            palette.suspended_by_modal.is_some(),
            palette.query.clone(),
            palette.input.read(cx).value().to_owned(),
            palette.selected,
            palette.generation,
        )
    });
    let modal_retained_focus = cx.update(|window, cx| {
        crate::modal::focused_modal_parent(window, cx).is_some()
            && !palette.read(cx).editor_is_focused(window, cx)
    });

    assert_eq!(open_results, (false, false));
    assert_eq!(
        blocked,
        (false, true, true, before.0, before.1, before.2, before.3)
    );
    assert!(modal_retained_focus);
    assert!(cx.debug_bounds("command-palette-panel").is_none());
    assert_eq!(root.read_with(cx, |root, _| root.palette_open_events), 0);

    let modal = root
        .read_with(cx, |root, _| root.modal.clone())
        .expect("modal handle should be retained");
    cx.update(|window, cx| modal.dismiss(window, cx).expect("modal should close"));
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open
            && palette.pending_open.is_none()
            && palette.suspended_by_modal.is_none()
            && palette.query.is_empty()
    }));
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
    assert!(cx.debug_bounds("command-palette-panel").is_some());
    assert_eq!(root.read_with(cx, |root, _| root.palette_open_events), 1);
}

#[gpui::test]
fn suspended_palette_focus_entries_should_not_steal_focus_from_active_modal(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            assert!(palette.open(window, cx));
            palette.set_query("window", cx);
        });
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();
    let replacement_focus = root.read_with(cx, |root, _| CommandPaletteReplacementFocus {
        restore_focus: Some(root.prior_focus.downgrade()),
    });

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            assert!(!palette.open(window, cx));
            assert!(!palette.open_replacing(replacement_focus, window, cx));
            palette.focus_editor(window, cx);
            palette.focus_editor(window, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.update(|window, cx| {
        crate::modal::focused_modal_parent(window, cx).is_some()
            && !palette.read(cx).editor_is_focused(window, cx)
    }));
    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_some() && palette.query == "window"
    }));
    assert!(cx.debug_bounds("command-palette-panel").is_none());
}

#[gpui::test]
fn suspended_palette_focus_entries_should_not_steal_focus_from_active_dialog(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            assert!(palette.open(window, cx));
        });
        root.update(cx, |root, cx| root.present_dialog_with_menu(window, cx));
    });
    cx.run_until_parked();
    let replacement_focus = root.read_with(cx, |root, _| CommandPaletteReplacementFocus {
        restore_focus: Some(root.prior_focus.downgrade()),
    });

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            assert!(!palette.open(window, cx));
            assert!(!palette.open_replacing(replacement_focus, window, cx));
            palette.focus_editor(window, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.update(|window, cx| {
        crate::modal::focused_modal_parent(window, cx).is_some()
            && !palette.read(cx).editor_is_focused(window, cx)
    }));
    assert!(cx.debug_bounds("modal-surface-1").is_some());
    assert!(cx.debug_bounds("command-palette-panel").is_none());
}

#[gpui::test]
fn modal_focus_scope_should_repair_direct_unauthorized_focus_theft(cx: &mut TestAppContext) {
    let (root, _, cx) = modal_palette_window(cx);
    let unauthorized = root.read_with(cx, |root, _| root.prior_focus.clone());
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();

    cx.update(|window, _| unauthorized.focus(window));
    cx.run_until_parked();

    assert!(cx.update(|window, cx| {
        crate::modal::focused_modal_parent(window, cx).is_some() && !unauthorized.is_focused(window)
    }));
}

#[gpui::test]
fn modal_close_without_registered_palette_should_not_suspend_later_palette(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();
    let modal = root
        .read_with(cx, |root, _| root.modal.clone())
        .expect("modal handle should be retained");

    cx.update(|window, cx| modal.dismiss(window, cx).expect("modal should close"));
    cx.run_until_parked();
    let window_id = cx.update(|window, _| window.window_handle().window_id());
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("command-palette-panel").is_some(),
        "the palette did not render after an unrelated modal closed"
    );
    assert!(!cx.update(|_, cx| {
        cx.global::<CommandPaletteCoordinator>()
            .modal_suspensions
            .contains_key(&window_id)
    }));
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
}

#[gpui::test]
fn closing_suspended_palette_should_not_suspend_replacement_after_modal_close(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();
    let modal = root
        .read_with(cx, |root, _| root.modal.clone())
        .expect("modal handle should be retained");

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            assert!(
                palette.dismiss(window, cx),
                "suspended palette should close"
            );
        });
        modal.dismiss(window, cx).expect("modal should close");
    });
    cx.run_until_parked();
    let replacement = cx.update(|window, cx| {
        let replacement = cx.new(|cx| {
            CommandPalette::new(
                "Replacement",
                vec![CommandPaletteItem::new(9, "New")],
                window,
                cx,
            )
        });
        root.update(cx, |root, cx| {
            root.palette = replacement.clone();
            cx.notify();
        });
        replacement.update(cx, |palette, cx| palette.open(window, cx));
        replacement
    });
    cx.run_until_parked();
    let window_id = cx.update(|window, _| window.window_handle().window_id());

    assert!(
        cx.debug_bounds("command-palette-panel").is_some(),
        "the replacement palette did not render"
    );
    assert!(!cx.update(|_, cx| {
        cx.global::<CommandPaletteCoordinator>()
            .modal_suspensions
            .contains_key(&window_id)
    }));
    assert!(cx.update(|window, cx| { replacement.read(cx).editor_is_focused(window, cx) }));
}

#[gpui::test]
fn modal_suspension_should_preserve_complete_palette_state_without_closing(
    cx: &mut TestAppContext,
) {
    let (root, palette, events, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    palette.update(cx, |palette, cx| {
        palette.set_query("window", cx);
        palette.set_loading(true, cx);
        palette.set_dismissible(false, cx);
        palette.set_preferred_item(Some(3), cx);
    });
    cx.run_until_parked();
    events.borrow_mut().clear();
    let before = palette_state_snapshot(&palette, cx);
    let window_id = cx.update(|window, _| window.window_handle().window_id());

    let suspension = cx.update(|_, cx| suspend_window_command_palette(window_id, cx));
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| {
        palette.suspended_by_modal.is_some() && palette.open
    }));
    assert!(events.borrow().is_empty());

    let resumed = cx.update(|_, cx| resume_window_command_palette(suspension.token, cx));
    cx.run_until_parked();

    assert!(resumed);
    assert_eq!(palette_state_snapshot(&palette, cx), before);
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn modal_palette_suspension_should_be_isolated_by_operating_system_window(cx: &mut TestAppContext) {
    cx.update(init);
    let first_suspended = Rc::new(Cell::new(false));
    let second_suspended = Rc::new(Cell::new(false));
    let first_flag = first_suspended.clone();
    let second_flag = second_suspended.clone();
    let first_window = WindowId::from(101);
    let second_window = WindowId::from(202);
    cx.update(|cx| {
        cx.update_global::<CommandPaletteCoordinator, _>(|coordinator, _| {
            coordinator.registrations.insert(
                first_window,
                ErasedPaletteRegistration {
                    token: CommandPaletteRegistration(1),
                    suspend: Rc::new(move |_, _| {
                        first_flag.set(true);
                        None
                    }),
                    resume: Rc::new(|_, _, _| {}),
                    replace: Rc::new(|_| {}),
                    replace_now: Rc::new(|_, _| None),
                },
            );
            coordinator.registrations.insert(
                second_window,
                ErasedPaletteRegistration {
                    token: CommandPaletteRegistration(2),
                    suspend: Rc::new(move |_, _| {
                        second_flag.set(true);
                        None
                    }),
                    resume: Rc::new(|_, _, _| {}),
                    replace: Rc::new(|_| {}),
                    replace_now: Rc::new(|_, _| None),
                },
            );
        });
        let _ = suspend_window_command_palette(first_window, cx);
    });

    assert!(first_suspended.get());
    assert!(!second_suspended.get());
    assert_eq!(
        cx.update(|cx| {
            let coordinator = cx.global::<CommandPaletteCoordinator>();
            (
                coordinator.modal_suspensions.contains_key(&first_window),
                coordinator.modal_suspensions.contains_key(&second_window),
            )
        }),
        (true, false)
    );
}

#[gpui::test]
fn blocked_modal_resume_retains_generation_until_matching_palette_actually_focuses(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    let newer_focus = root.read_with(cx, |root, _| root.prior_focus.clone());
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();
    let window_id = cx.update(|window, _| window.window_handle().window_id());
    let modal = root
        .read_with(cx, |root, _| root.modal.clone())
        .expect("modal handle should be retained");
    let generation = palette
        .read_with(cx, |palette, _| palette.suspended_by_modal)
        .expect("palette should be suspended");

    cx.update(|window, cx| {
        modal.dismiss(window, cx).expect("modal should close");
        newer_focus.focus(window);
    });
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal == Some(generation)
    }));
    assert!(cx.update(|window, _| newer_focus.is_focused(window)));
    assert!(cx.update(|_, cx| {
        cx.global::<CommandPaletteCoordinator>()
            .modal_suspensions
            .get(&window_id)
            .is_some_and(|current| current.generation == generation)
    }));

    let editor = palette.read_with(cx, |palette, cx| {
        palette.input.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| editor.focus(window));
    cx.update(|_, cx| retry_window_command_palette_modal_resume(window_id, cx));
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_none()
    }));
    assert!(!cx.update(|_, cx| {
        cx.global::<CommandPaletteCoordinator>()
            .modal_suspensions
            .contains_key(&window_id)
    }));
}

#[gpui::test]
fn replacing_a_palette_during_modal_suspension_should_resume_only_the_replacement(
    cx: &mut TestAppContext,
) {
    let (root, palette, _, _, cx) = palette_window(cx);
    open_palette(&root, &palette, cx);
    let window_id = cx.update(|window, _| window.window_handle().window_id());
    let suspension = cx.update(|_, cx| suspend_window_command_palette(window_id, cx));
    let replacement = cx.update(|window, cx| {
        let replacement = cx.new(|cx| {
            CommandPalette::new(
                "Replacement",
                vec![CommandPaletteItem::new(9, "New")],
                window,
                cx,
            )
        });
        replacement.update(cx, |palette, cx| {
            palette.open(window, cx);
        });
        replacement
    });

    let resumed = cx.update(|_, cx| resume_window_command_palette(suspension.token, cx));
    cx.run_until_parked();

    assert!(resumed);
    assert!(palette.read_with(cx, |palette, _| {
        !palette.open && palette.suspended_by_modal.is_none()
    }));
    assert!(replacement.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_none()
    }));
}

#[gpui::test]
fn modal_open_and_close_should_suspend_then_resume_the_registered_palette(cx: &mut TestAppContext) {
    let (root, palette, cx) = modal_palette_window(cx);
    let prior = root.read_with(cx, |root, _| root.prior_focus.clone());
    cx.update(|window, _| prior.focus(window));
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.open(window, cx);
            palette.set_query("window", cx);
            palette.set_loading(true, cx);
            palette.set_dismissible(false, cx);
        });
    });
    cx.run_until_parked();
    let before = palette_state_snapshot(&palette, cx);

    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_some()
    }));

    let modal = root
        .read_with(cx, |root, _| root.modal.clone())
        .expect("modal handle should be retained");
    cx.update(|window, cx| modal.dismiss(window, cx).expect("modal should close"));
    cx.run_until_parked();

    assert_eq!(palette_state_snapshot(&palette, cx), before);
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
}

#[gpui::test]
fn programmatic_progress_close_should_resume_the_registered_palette(cx: &mut TestAppContext) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
        root.update(cx, |root, cx| {
            root.present_programmatic_progress(window, cx)
        });
    });
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_some()
    }));

    let progress = root
        .read_with(cx, |root, _| root.progress.clone())
        .expect("progress handle should be retained");
    cx.update(|window, cx| {
        progress
            .complete(window, cx)
            .expect("progress should complete");
    });
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_none()
    }));
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
}

#[gpui::test]
fn parent_completion_closes_owned_menu_before_suspended_palette_resumes(cx: &mut TestAppContext) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
        root.update(cx, |root, cx| root.present_dialog_with_menu(window, cx));
    });
    cx.run_until_parked();
    let trigger = cx
        .debug_bounds("palette-modal-menu-trigger")
        .expect("modal-owned Menu trigger should render");
    cx.simulate_click(trigger.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| { crate::menu::window_menu_is_owned_by_current_modal(window, cx) })
    );
    let completion = root
        .read_with(cx, |root, _| root.dialog.clone())
        .expect("Dialog completion should be retained");

    cx.update(|window, cx| {
        completion
            .complete(window, None, cx)
            .expect("Dialog should complete");
    });
    cx.run_until_parked();

    assert!(!cx.update(|window, cx| crate::window_menu_is_open(window, cx)));
    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_none()
    }));
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
}

#[gpui::test]
fn programmatic_progress_closed_while_deactivated_should_resume_palette_on_reactivation(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| palette.open(window, cx));
        root.update(cx, |root, cx| {
            root.present_programmatic_progress(window, cx)
        });
    });
    cx.run_until_parked();
    let progress = root
        .read_with(cx, |root, _| root.progress.clone())
        .expect("progress handle should be retained");

    cx.deactivate_window();
    cx.update(|window, cx| {
        progress
            .complete(window, cx)
            .expect("progress should complete while inactive");
    });
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_some()
    }));

    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_none()
    }));
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
}

#[gpui::test]
fn first_deferred_palette_request_resumes_after_window_reactivation(cx: &mut TestAppContext) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| root.update(cx, |root, cx| root.present_alert(window, cx)));
    cx.run_until_parked();
    let modal = root
        .read_with(cx, |root, _| root.modal.clone())
        .expect("modal handle should be retained");

    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            assert!(!palette.open(window, cx));
        });
    });
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| {
        !palette.open && palette.pending_open.is_some() && palette.suspended_by_modal.is_some()
    }));

    cx.deactivate_window();
    cx.update(|window, cx| modal.dismiss(window, cx).expect("modal should close"));
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| {
        !palette.open && palette.pending_open.is_some() && palette.suspended_by_modal.is_some()
    }));

    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.pending_open.is_none() && palette.suspended_by_modal.is_none()
    }));
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
}

#[gpui::test]
fn suspended_palette_should_remain_open_across_window_deactivation_and_reactivation(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.open(window, cx);
        });
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();

    cx.deactivate_window();
    cx.run_until_parked();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_some()
    }));
    assert!(cx.update(|window, cx| crate::window_modal_is_open(window, cx)));
}

#[gpui::test]
fn closing_a_modal_while_deactivated_should_resume_its_palette_after_reactivation(
    cx: &mut TestAppContext,
) {
    let (root, palette, cx) = modal_palette_window(cx);
    cx.update(|window, cx| {
        palette.update(cx, |palette, cx| {
            palette.open(window, cx);
        });
        root.update(cx, |root, cx| root.present_alert(window, cx));
    });
    cx.run_until_parked();
    let modal = root
        .read_with(cx, |root, _| root.modal.clone())
        .expect("modal handle should be retained");

    cx.deactivate_window();
    cx.update(|window, cx| modal.dismiss(window, cx).expect("modal should close"));
    cx.run_until_parked();
    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_some()
    }));

    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert!(palette.read_with(cx, |palette, _| {
        palette.open && palette.suspended_by_modal.is_none()
    }));
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
}

#[gpui::test]
fn deactivation_should_dismiss_without_restoring_prior_focus(cx: &mut TestAppContext) {
    let (root, palette, events, _, cx) = palette_window(cx);
    let prior = open_palette(&root, &palette, cx);

    cx.deactivate_window();
    cx.run_until_parked();

    assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
    assert!(!cx.update(|window, _| prior.is_focused(window)));
    assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
        CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Deactivated)
    )));

    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert!(cx.update(|window, _| prior.is_focused(window)));
}
