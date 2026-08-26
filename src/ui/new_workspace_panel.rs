use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Render, Window, px, rgba};
use gpui_symbols::Icon;
use spaceterm_ui::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteEvent, CommandPaletteHint,
    CommandPaletteItem, CommandPaletteLifecycleEvent,
};

use crate::theme::ACTIVE_THEME;

const SOURCE_ICON_SIZE: f32 = 14.0;

/// One way to bring a Workspace into existence, presented as a single New Workspace Panel row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NewWorkspaceSource {
    LocalProject,
    Scratch,
    RemoteProject,
}

impl NewWorkspaceSource {
    /// Rows name their source rather than repeating the panel's own noun, so no label means one
    /// thing on the panel and another on the control that opened it.
    const fn label(self) -> &'static str {
        match self {
            Self::LocalProject => "Local Project…",
            Self::Scratch => "Scratch Workspace",
            Self::RemoteProject => "Remote over SSH…",
        }
    }

    /// The one behavioural difference between the Workspace Kinds, stated once, where the choice
    /// is actually made.
    const fn description(self) -> &'static str {
        match self {
            Self::LocalProject => "Pinned to a folder on this Mac",
            Self::Scratch => "Starts at ~, follows your shell",
            Self::RemoteProject => "Pinned to a folder on another machine",
        }
    }

    const fn icon(self) -> &'static str {
        match self {
            Self::LocalProject => "folder",
            Self::Scratch => "terminal",
            Self::RemoteProject => "network",
        }
    }

    const fn accessory(self) -> &'static str {
        match self {
            Self::LocalProject => "\u{21e7}\u{2318}O",
            Self::Scratch => "\u{2318}N",
            Self::RemoteProject => "Soon",
        }
    }

    /// Whether the source can create a Workspace today.
    const fn is_available(self) -> bool {
        !matches!(self, Self::RemoteProject)
    }

    const fn debug_selector(self) -> &'static str {
        match self {
            Self::LocalProject => "new-workspace-source-local-project",
            Self::Scratch => "new-workspace-source-scratch",
            Self::RemoteProject => "new-workspace-source-remote-project",
        }
    }

    /// Every source in presentation order.
    const fn ordered() -> [Self; 3] {
        // Local Project leads so that the default selection makes cmd-o enter the Workspace
        // Picker, keeping the most frequent path two keystrokes deep despite the added chooser.
        [Self::LocalProject, Self::Scratch, Self::RemoteProject]
    }

    fn into_palette_item(self) -> CommandPaletteItem<Self> {
        let available = self.is_available();
        let icon_color = rgba(if available {
            ACTIVE_THEME.icon.rgba_hex()
        } else {
            ACTIVE_THEME.text_muted.rgba_hex()
        });
        let icon_name = self.icon();
        let accessory = if available {
            CommandPaletteAccessory::Shortcut(self.accessory().into())
        } else {
            CommandPaletteAccessory::Status(self.accessory().into())
        };
        // An unavailable source stays listed because the three descriptions together are what
        // teach the Workspace Kinds. The palette omits disabled rows from selection and keyboard
        // navigation, so it states the model without becoming a dead stop.
        CommandPaletteItem::new(self, self.label())
            .description(self.description())
            .disabled(!available)
            .leading_icon(move |_| {
                Icon::new(icon_name)
                    .size(px(SOURCE_ICON_SIZE))
                    .color(icon_color)
                    .into_any_element()
            })
            .trailing(accessory)
            .debug_selector(self.debug_selector())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum NewWorkspacePanelEvent {
    StateChanged,
    SourceSelected(NewWorkspaceSource),
}

/// The one surface that presents every way to create a Workspace.
///
/// The panel owns no hierarchy state. Selecting a row emits its source and the Workspace Manager
/// performs the lifecycle action, so the panel stays a chooser rather than a second creation path.
pub(super) struct NewWorkspacePanel {
    palette: Entity<CommandPalette<NewWorkspaceSource>>,
    open: bool,
    pending_open: bool,
}

impl NewWorkspacePanel {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let items = NewWorkspaceSource::ordered()
            .into_iter()
            .map(NewWorkspaceSource::into_palette_item)
            .collect();
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new("New Workspace", items, window, cx);
            palette.set_no_results_text("No matching source", cx);
            palette.set_hints(
                vec![
                    CommandPaletteHint::new("Choose", "\u{21b5}"),
                    CommandPaletteHint::new("Dismiss", "esc"),
                ],
                cx,
            );
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |panel, _, event: &CommandPaletteEvent<NewWorkspaceSource>, _, cx| {
                panel.reduce_palette_event(event, cx);
            },
        )
        .detach();

        Self {
            palette,
            open: false,
            pending_open: false,
        }
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.palette
                .update(cx, |palette, cx| palette.open(window, cx));
            return;
        }

        self.pending_open = true;
        cx.emit(NewWorkspacePanelEvent::StateChanged);
        cx.notify();

        // The palette takes the responder on open, so the deferred open runs after the caller's
        // own focus work has settled. A cancelled request leaves pending_open cleared.
        let panel = cx.entity();
        let palette = self.palette.clone();
        window.defer(cx, move |window, cx| {
            if !panel.read(cx).pending_open {
                return;
            }

            let (opened, is_open) = palette.update(cx, |palette, cx| {
                let opened = palette.open(window, cx);
                (opened, palette.is_open())
            });
            if !opened {
                panel.update(cx, |panel, cx| {
                    panel.settle(is_open, cx);
                });
            }
        });
    }

    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_pending_open(cx);
        self.palette
            .update(cx, |palette, cx| palette.dismiss(window, cx));
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the panel currently owns terminal input, including the window between an accepted
    /// open request and the palette actually opening.
    pub(super) fn blocks_terminal_input(&self) -> bool {
        self.open || self.pending_open
    }

    fn cancel_pending_open(&mut self, cx: &mut Context<Self>) {
        if self.pending_open {
            self.pending_open = false;
            cx.emit(NewWorkspacePanelEvent::StateChanged);
            cx.notify();
        }
    }

    fn settle(&mut self, open: bool, cx: &mut Context<Self>) {
        if !self.pending_open && self.open == open {
            return;
        }
        self.pending_open = false;
        self.open = open;
        cx.emit(NewWorkspacePanelEvent::StateChanged);
        cx.notify();
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<NewWorkspaceSource>,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                self.settle(true, cx);
            }
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(_)) => {
                self.settle(false, cx);
            }
            CommandPaletteEvent::Activated(activation) => {
                cx.emit(NewWorkspacePanelEvent::SourceSelected(
                    *activation.item_id(),
                ));
                cx.notify();
            }
            CommandPaletteEvent::QueryChanged(_)
            | CommandPaletteEvent::HeaderAction(_)
            | CommandPaletteEvent::MenuAction(_)
            | CommandPaletteEvent::Confirmed => {}
        }
    }

    #[cfg(test)]
    fn palette(&self) -> Entity<CommandPalette<NewWorkspaceSource>> {
        self.palette.clone()
    }
}

impl EventEmitter<NewWorkspacePanelEvent> for NewWorkspacePanel {}

impl Render for NewWorkspacePanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.palette.clone()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{FocusHandle, TestAppContext, VisualTestContext, div};

    use super::*;

    struct NewWorkspacePanelHarness {
        panel: Entity<NewWorkspacePanel>,
        prior_focus: FocusHandle,
    }

    impl NewWorkspacePanelHarness {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            Self {
                panel: cx.new(|cx| NewWorkspacePanel::new(window, cx)),
                prior_focus: cx.focus_handle(),
            }
        }
    }

    impl Render for NewWorkspacePanelHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.prior_focus)
                .child(self.panel.clone())
        }
    }

    fn new_workspace_panel(
        cx: &mut TestAppContext,
    ) -> (Entity<NewWorkspacePanel>, &mut VisualTestContext) {
        cx.update(crate::ui::init);
        let (harness, cx) = cx.add_window_view(NewWorkspacePanelHarness::new);
        let panel = harness.read_with(cx, |harness, _| harness.panel.clone());
        cx.update(|window, cx| harness.read(cx).prior_focus.focus(window));
        cx.run_until_parked();
        (panel, cx)
    }

    fn open(panel: &Entity<NewWorkspacePanel>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| panel.update(cx, |panel, cx| panel.open(window, cx)));
        cx.run_until_parked();
    }

    #[test]
    fn local_project_should_lead_so_the_default_selection_opens_the_picker() {
        assert_eq!(
            NewWorkspaceSource::ordered().first().copied(),
            Some(NewWorkspaceSource::LocalProject)
        );
    }

    #[test]
    fn every_source_should_state_its_workspace_directory_behaviour() {
        for source in NewWorkspaceSource::ordered() {
            assert!(
                !source.description().is_empty(),
                "{source:?} listed no description, so the panel stops teaching the Kinds"
            );
        }
    }

    #[test]
    fn rows_should_not_repeat_the_panel_noun() {
        for source in NewWorkspaceSource::ordered() {
            assert_ne!(
                source.label(),
                "New Workspace",
                "{source:?} reused the panel's own name, which is the ambiguity it removes"
            );
        }
    }

    #[test]
    fn remote_project_should_be_the_only_unavailable_source() {
        let unavailable: Vec<_> = NewWorkspaceSource::ordered()
            .into_iter()
            .filter(|source| !source.is_available())
            .collect();

        assert_eq!(unavailable, vec![NewWorkspaceSource::RemoteProject]);
    }

    #[gpui::test]
    fn opening_should_present_every_source_and_block_terminal_input(cx: &mut TestAppContext) {
        let (panel, cx) = new_workspace_panel(cx);

        open(&panel, cx);

        for source in NewWorkspaceSource::ordered() {
            assert!(
                cx.debug_bounds(source.debug_selector()).is_some(),
                "{source:?} was not rendered"
            );
        }
        assert!(panel.read_with(cx, |panel, _| panel.blocks_terminal_input()));
    }

    #[gpui::test]
    fn opening_should_select_local_project_before_any_query(cx: &mut TestAppContext) {
        let (panel, cx) = new_workspace_panel(cx);

        open(&panel, cx);

        assert_eq!(
            panel.read_with(cx, |panel, cx| panel
                .palette()
                .read(cx)
                .selected_item_id()
                .copied()),
            Some(NewWorkspaceSource::LocalProject)
        );
    }

    #[gpui::test]
    fn an_unavailable_source_should_stay_listed_but_never_be_selected(cx: &mut TestAppContext) {
        let (panel, cx) = new_workspace_panel(cx);

        open(&panel, cx);
        cx.simulate_keystrokes("down down down down");
        cx.run_until_parked();

        assert!(
            cx.debug_bounds(NewWorkspaceSource::RemoteProject.debug_selector())
                .is_some(),
            "the Remote Project row must stay visible to teach the Kinds"
        );
        assert_ne!(
            panel.read_with(cx, |panel, cx| panel
                .palette()
                .read(cx)
                .selected_item_id()
                .copied()),
            Some(NewWorkspaceSource::RemoteProject)
        );
    }

    #[gpui::test]
    fn dismissing_should_release_terminal_input(cx: &mut TestAppContext) {
        let (panel, cx) = new_workspace_panel(cx);

        open(&panel, cx);
        cx.update(|window, cx| panel.update(cx, |panel, cx| panel.dismiss(window, cx)));
        cx.run_until_parked();

        assert_eq!(
            panel.read_with(cx, |panel, _| (
                panel.is_open(),
                panel.blocks_terminal_input()
            )),
            (false, false)
        );
    }
}
