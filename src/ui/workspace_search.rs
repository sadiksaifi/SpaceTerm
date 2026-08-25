use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Render, Window, px, rgba};
use gpui_symbols::Icon;
use spaceterm_ui::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteEvent, CommandPaletteHint,
    CommandPaletteItem, CommandPaletteLifecycleEvent,
};

use crate::domain::WorkspaceId;
use crate::theme::ACTIVE_THEME;

const WORKSPACE_ICON_SIZE: f32 = 14.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WorkspaceSearchEvent {
    StateChanged,
    WorkspaceSelected(WorkspaceId),
}

pub(super) struct WorkspaceSearchItem {
    workspace_id: WorkspaceId,
    name: String,
    path: String,
    local_project: bool,
    available: bool,
    window_count: usize,
    pane_count: usize,
}

impl WorkspaceSearchItem {
    pub(super) fn new(
        workspace_id: WorkspaceId,
        name: String,
        path: String,
        local_project: bool,
        available: bool,
        window_count: usize,
        pane_count: usize,
    ) -> Self {
        Self {
            workspace_id,
            name,
            path,
            local_project,
            available,
            window_count,
            pane_count,
        }
    }

    fn into_palette_item(self) -> CommandPaletteItem<WorkspaceId> {
        let icon_color = rgba(if self.available {
            ACTIVE_THEME.icon.rgba_hex()
        } else {
            ACTIVE_THEME.warning.rgba_hex()
        });
        let icon_name = if self.local_project {
            "folder"
        } else {
            "terminal"
        };
        CommandPaletteItem::new(self.workspace_id, self.name)
            .description(self.path)
            .leading_icon(move |_| {
                Icon::new(icon_name)
                    .size(px(WORKSPACE_ICON_SIZE))
                    .color(icon_color)
                    .into_any_element()
            })
            .trailing(CommandPaletteAccessory::Text(
                format!("{}W · {}P", self.window_count, self.pane_count).into(),
            ))
            .debug_selector(format!(
                "workspace-search-result-{}",
                self.workspace_id.get()
            ))
    }
}

pub(super) struct WorkspaceSearch {
    palette: Entity<CommandPalette<WorkspaceId>>,
    open: bool,
    open_generation: Option<u64>,
    activation_generation: Option<u64>,
    pending_open: Option<u64>,
    next_open_generation: u64,
}

impl WorkspaceSearch {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new("Search Workspaces", Vec::new(), window, cx);
            palette.set_no_results_text("No matching Workspaces", cx);
            palette.set_hints(
                vec![
                    CommandPaletteHint::new("Open", "\u{21b5}"),
                    CommandPaletteHint::new("Dismiss", "esc"),
                ],
                cx,
            );
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |search, _, event: &CommandPaletteEvent<WorkspaceId>, window, cx| {
                search.reduce_palette_event(event, window, cx);
            },
        )
        .detach();
        cx.observe_window_activation(window, |search, window, cx| {
            if !window.is_window_active() {
                search.cancel_pending_open(cx);
            }
        })
        .detach();

        Self {
            palette,
            open: false,
            open_generation: None,
            activation_generation: None,
            pending_open: None,
            next_open_generation: 0,
        }
    }

    pub(super) fn open(
        &mut self,
        items: Vec<WorkspaceSearchItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = items
            .into_iter()
            .map(WorkspaceSearchItem::into_palette_item)
            .collect();
        if self.open {
            self.palette.update(cx, |palette, cx| {
                palette.set_items(items, cx);
                palette.open(window, cx);
            });
            return;
        }

        self.next_open_generation = self.next_open_generation.wrapping_add(1);
        let generation = self.next_open_generation;
        self.pending_open = Some(generation);
        cx.emit(WorkspaceSearchEvent::StateChanged);
        cx.notify();

        let search = cx.entity();
        let palette = self.palette.clone();
        window.defer(cx, move |window, cx| {
            if search.read(cx).pending_open != Some(generation) {
                return;
            }

            let (opened, is_open) = palette.update(cx, |palette, cx| {
                palette.set_items(items, cx);
                let opened = palette.open(window, cx);
                (opened, palette.is_open())
            });
            if !opened {
                search.update(cx, |search, cx| {
                    if search.pending_open == Some(generation) {
                        search.pending_open = None;
                        search.open = is_open;
                        cx.emit(WorkspaceSearchEvent::StateChanged);
                        cx.notify();
                    }
                });
            }
        });
    }

    pub(super) fn blocks_terminal_input(&self) -> bool {
        self.open || self.pending_open.is_some()
    }

    fn cancel_pending_open(&mut self, cx: &mut Context<Self>) {
        if self.pending_open.take().is_some() {
            cx.emit(WorkspaceSearchEvent::StateChanged);
            cx.notify();
        }
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<WorkspaceId>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                let generation = self.pending_open.take().unwrap_or_else(|| {
                    self.next_open_generation = self.next_open_generation.wrapping_add(1);
                    self.next_open_generation
                });
                self.open = true;
                self.open_generation = Some(generation);
                cx.emit(WorkspaceSearchEvent::StateChanged);
                cx.notify();
            }
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(_)) => {
                let closing_generation = self.activation_generation.take().or(self.open_generation);
                cx.defer_in(window, move |search, _, cx| {
                    if search.open_generation != closing_generation {
                        return;
                    }
                    search.open = false;
                    search.open_generation = None;
                    cx.emit(WorkspaceSearchEvent::StateChanged);
                    cx.notify();
                });
            }
            CommandPaletteEvent::Activated(activation) => {
                self.activation_generation = self.open_generation;
                cx.emit(WorkspaceSearchEvent::WorkspaceSelected(
                    *activation.item_id(),
                ));
                cx.notify();
            }
            CommandPaletteEvent::QueryChanged(_)
            | CommandPaletteEvent::HeaderAction(_)
            | CommandPaletteEvent::MenuAction(_) => {}
        }
    }

    #[cfg(test)]
    fn palette(&self) -> Entity<CommandPalette<WorkspaceId>> {
        self.palette.clone()
    }

    #[cfg(test)]
    fn pending_open(&self) -> Option<u64> {
        self.pending_open
    }
}

impl EventEmitter<WorkspaceSearchEvent> for WorkspaceSearch {}

impl Render for WorkspaceSearch {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.palette.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{FocusHandle, Modifiers, TestAppContext, VisualTestContext, div};

    use super::*;

    struct WorkspaceSearchHarness {
        search: Entity<WorkspaceSearch>,
        prior_focus: FocusHandle,
        events: Rc<RefCell<Vec<(WorkspaceSearchEvent, bool)>>>,
        reopen_on_selection: bool,
    }

    impl WorkspaceSearchHarness {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let search = cx.new(|cx| WorkspaceSearch::new(window, cx));
            let events = Rc::new(RefCell::new(Vec::new()));
            let received_events = Rc::clone(&events);
            cx.subscribe_in(
                &search,
                window,
                move |harness, search, event: &WorkspaceSearchEvent, window, cx| {
                    received_events
                        .borrow_mut()
                        .push((event.clone(), search.read(cx).blocks_terminal_input()));
                    if matches!(event, WorkspaceSearchEvent::WorkspaceSelected(_))
                        && harness.reopen_on_selection
                    {
                        harness.reopen_on_selection = false;
                        search.update(cx, |search, cx| {
                            search.open(
                                vec![WorkspaceSearchItem::new(
                                    WorkspaceId::new(9),
                                    "Reopened".to_owned(),
                                    "/reopened".to_owned(),
                                    false,
                                    true,
                                    1,
                                    1,
                                )],
                                window,
                                cx,
                            );
                        });
                    }
                },
            )
            .detach();
            Self {
                search,
                prior_focus: cx.focus_handle(),
                events,
                reopen_on_selection: false,
            }
        }
    }

    impl Render for WorkspaceSearchHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.prior_focus)
                .child(self.search.clone())
        }
    }

    fn workspace_search(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceSearchHarness>,
        Entity<WorkspaceSearch>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let (harness, cx) = cx.add_window_view(WorkspaceSearchHarness::new);
        let search = harness.read_with(cx, |harness, _| harness.search.clone());
        cx.update(|window, cx| harness.read(cx).prior_focus.focus(window));
        cx.run_until_parked();
        (harness, search, cx)
    }

    fn item(workspace_id: u64, name: &str, path: &str) -> WorkspaceSearchItem {
        WorkspaceSearchItem::new(
            WorkspaceId::new(workspace_id),
            name.to_owned(),
            path.to_owned(),
            false,
            true,
            workspace_id as usize,
            workspace_id as usize + 1,
        )
    }

    fn open(
        search: &Entity<WorkspaceSearch>,
        items: Vec<WorkspaceSearchItem>,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            search.update(cx, |search, cx| search.open(items, window, cx));
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn open_should_block_input_before_and_during_palette_lifecycle(cx: &mut TestAppContext) {
        let (_, search, cx) = workspace_search(cx);

        cx.update(|window, cx| {
            search.update(cx, |search, cx| {
                search.open(vec![item(1, "Default", "~")], window, cx);
                assert!(search.blocks_terminal_input());
            });
        });
        cx.run_until_parked();

        assert!(search.read_with(cx, |search, _| search.blocks_terminal_input()));
        assert!(cx.debug_bounds("command-palette-panel").is_some());
    }

    #[gpui::test]
    fn repeated_open_should_run_only_the_latest_deferred_request(cx: &mut TestAppContext) {
        let (_, search, cx) = workspace_search(cx);

        cx.update(|window, cx| {
            search.update(cx, |search, cx| {
                search.open(vec![item(1, "Stale", "/stale")], window, cx);
                search.open(vec![item(2, "Current", "/current")], window, cx);
            });
        });
        cx.run_until_parked();

        let state = search.read_with(cx, |search, cx| {
            (
                search.open,
                search.pending_open(),
                search.palette().read(cx).generation().value(),
                search.palette().read(cx).selected_item_id().copied(),
            )
        });
        assert_eq!(state, (true, None, 1, Some(WorkspaceId::new(2))));
    }

    #[gpui::test]
    fn deactivation_before_deferred_open_should_cancel_the_request(cx: &mut TestAppContext) {
        let (_, search, cx) = workspace_search(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        cx.update(|window, cx| {
            search.update(cx, |search, cx| {
                search.open(vec![item(1, "Default", "~")], window, cx);
            });
        });
        cx.deactivate_window();
        cx.run_until_parked();

        let state = search.read_with(cx, |search, cx| {
            (
                search.open,
                search.pending_open(),
                search.palette().read(cx).is_open(),
                search.blocks_terminal_input(),
            )
        });
        assert_eq!(state, (false, None, false, false));
    }

    #[gpui::test]
    fn repeated_open_while_visible_should_preserve_the_query(cx: &mut TestAppContext) {
        let (_, search, cx) = workspace_search(cx);
        open(&search, vec![item(1, "Alpha", "/alpha")], cx);
        cx.simulate_keystrokes("a");
        cx.run_until_parked();

        open(&search, vec![item(1, "Alpha", "/alpha")], cx);

        assert_eq!(
            search.read_with(cx, |search, cx| search
                .palette()
                .read(cx)
                .query()
                .to_owned()),
            "a"
        );
    }

    #[gpui::test]
    fn repeated_open_while_visible_should_refresh_items_and_preserve_the_query(
        cx: &mut TestAppContext,
    ) {
        let (_, search, cx) = workspace_search(cx);
        open(&search, vec![item(1, "Stale", "/stale")], cx);
        cx.simulate_keystrokes("c");
        cx.run_until_parked();

        open(&search, vec![item(2, "Current", "/current")], cx);

        assert_eq!(
            search.read_with(cx, |search, cx| {
                let palette = search.palette();
                (
                    palette.read(cx).query().to_owned(),
                    palette.read(cx).selected_item_id().copied(),
                )
            }),
            ("c".to_owned(), Some(WorkspaceId::new(2)))
        );
    }

    #[gpui::test]
    fn rows_should_present_workspace_path_counts_and_stable_identity(cx: &mut TestAppContext) {
        let (_, search, cx) = workspace_search(cx);
        let adapted = item(2, "Project", "~/Project").into_palette_item();
        assert_eq!(
            (*adapted.id(), adapted.description_text()),
            (WorkspaceId::new(2), Some("~/Project"))
        );

        open(&search, vec![item(2, "Project", "~/Project")], cx);

        assert!(cx.debug_bounds("workspace-search-result-2").is_some());
        assert_eq!(
            search.read_with(cx, |search, cx| search
                .palette()
                .read(cx)
                .selected_item_id()
                .copied()),
            Some(WorkspaceId::new(2))
        );
    }

    #[gpui::test]
    fn query_should_filter_workspace_names_case_insensitively(cx: &mut TestAppContext) {
        let (_, search, cx) = workspace_search(cx);
        open(
            &search,
            vec![
                item(1, "ALPHA WORKSPACE", "/one"),
                item(2, "Beta Workspace", "/two"),
                item(3, "Gamma Workspace", "/three"),
            ],
            cx,
        );

        cx.simulate_keystrokes("a l p h a");
        cx.run_until_parked();

        assert_eq!(
            search.read_with(cx, |search, cx| {
                let palette = search.palette();
                (
                    palette.read(cx).query().to_owned(),
                    palette.read(cx).selected_item_id().copied(),
                )
            }),
            ("alpha".to_owned(), Some(WorkspaceId::new(1)))
        );
    }

    #[gpui::test]
    fn unmatched_query_should_present_no_selection(cx: &mut TestAppContext) {
        let (_, search, cx) = workspace_search(cx);
        open(&search, vec![item(1, "Default", "/users/test")], cx);

        cx.simulate_keystrokes("z z z z");
        cx.run_until_parked();

        assert_eq!(
            search.read_with(cx, |search, cx| search
                .palette()
                .read(cx)
                .selected_item_id()
                .copied()),
            None
        );
    }

    #[gpui::test]
    fn activation_should_block_during_selection_and_unblock_after_close(cx: &mut TestAppContext) {
        let (harness, search, cx) = workspace_search(cx);
        open(&search, vec![item(7, "Target", "/target")], cx);
        harness.read_with(cx, |harness, _| harness.events.borrow_mut().clear());

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let events = harness.read_with(cx, |harness, _| harness.events.borrow().clone());
        assert_eq!(
            events.as_slice(),
            [
                (
                    WorkspaceSearchEvent::WorkspaceSelected(WorkspaceId::new(7)),
                    true,
                ),
                (WorkspaceSearchEvent::StateChanged, false),
            ]
        );
    }

    #[gpui::test]
    fn activation_reopen_should_ignore_the_old_close_and_keep_blocking(cx: &mut TestAppContext) {
        let (harness, search, cx) = workspace_search(cx);
        open(&search, vec![item(7, "Target", "/target")], cx);
        harness.update(cx, |harness, _| harness.reopen_on_selection = true);

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let state = search.read_with(cx, |search, cx| {
            (
                search.open,
                search.pending_open(),
                search.palette().read(cx).is_open(),
                search.blocks_terminal_input(),
            )
        });
        assert_eq!(state, (true, None, true, true));
    }

    #[gpui::test]
    fn escape_should_restore_the_exact_prior_focus_owner(cx: &mut TestAppContext) {
        let (harness, search, cx) = workspace_search(cx);
        open(&search, vec![item(1, "Default", "~")], cx);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(cx.update(|window, cx| {
            harness.read(cx).prior_focus.is_focused(window)
                && !search.read(cx).blocks_terminal_input()
        }));
    }

    #[gpui::test]
    fn pointer_activation_should_report_the_workspace_id(cx: &mut TestAppContext) {
        let (harness, search, cx) = workspace_search(cx);
        open(&search, vec![item(4, "Clicked", "/clicked")], cx);
        let position = cx
            .debug_bounds("workspace-search-result-4")
            .expect("Workspace Search row was not rendered")
            .center();

        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();

        assert!(harness.read_with(cx, |harness, _| {
            harness.events.borrow().iter().any(|(event, _)| {
                *event == WorkspaceSearchEvent::WorkspaceSelected(WorkspaceId::new(4))
            })
        }));
    }
}
