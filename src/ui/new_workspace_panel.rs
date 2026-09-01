use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, Render, Window, px, rgba};
use gpui_symbols::Icon;
use spaceterm_ui::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteActivationPolicy, CommandPaletteEvent,
    CommandPaletteHint, CommandPaletteItem, CommandPaletteLifecycleEvent,
    CommandPaletteReplacementFocus,
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
            Self::LocalProject => "Local Project",
            Self::Scratch => "Scratch Workspace",
            Self::RemoteProject => "Remote over SSH",
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
            Self::RemoteProject => "globe",
        }
    }

    const fn accessory(self) -> Option<&'static str> {
        match self {
            Self::LocalProject => Some("\u{21e7}\u{2318}O"),
            Self::Scratch => Some("\u{2318}N"),
            Self::RemoteProject => None,
        }
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

    fn into_palette_item(
        self,
        remote_unavailable_reason: Option<String>,
    ) -> CommandPaletteItem<Self> {
        let icon_color = rgba(ACTIVE_THEME.icon.rgba_hex());
        let icon_name = self.icon();
        let unavailable = (self == Self::RemoteProject)
            .then_some(remote_unavailable_reason)
            .flatten();
        let item = CommandPaletteItem::new(self, self.label())
            .description(
                unavailable
                    .clone()
                    .unwrap_or_else(|| self.description().to_owned()),
            )
            .disabled(unavailable.is_some())
            .leading_icon(move |_| {
                Icon::new(icon_name)
                    .size(px(SOURCE_ICON_SIZE))
                    .color(icon_color)
                    .into_any_element()
            })
            .debug_selector(self.debug_selector());

        match (self.accessory(), unavailable.as_ref()) {
            (_, Some(_)) => item.trailing(CommandPaletteAccessory::Status("Unavailable".into())),
            (Some(shortcut), None) => {
                item.trailing(CommandPaletteAccessory::Shortcut(shortcut.into()))
            }
            (None, None) => item,
        }
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
        Self::new_with_remote_unavailable_reason(None, window, cx)
    }

    pub(super) fn new_with_remote_unavailable_reason(
        remote_unavailable_reason: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let items = NewWorkspaceSource::ordered()
            .into_iter()
            .map(|source| source.into_palette_item(remote_unavailable_reason.clone()))
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
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |panel, _, event: &CommandPaletteEvent<NewWorkspaceSource>, window, cx| {
                panel.reduce_palette_event(event, window, cx);
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

    pub(super) fn dismiss_for_replacement(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<CommandPaletteReplacementFocus> {
        self.cancel_pending_open(cx);
        let replacement = self.palette.update(cx, |palette, cx| {
            palette.dismiss_for_replacement(window, cx)
        });
        if replacement.is_some() {
            self.settle(false, cx);
        }
        replacement
    }

    pub(super) fn open_replacing(
        &mut self,
        replacement: CommandPaletteReplacementFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.open || self.pending_open {
            return false;
        }
        self.pending_open = true;
        cx.emit(NewWorkspacePanelEvent::StateChanged);
        cx.notify();
        let opened = self.palette.update(cx, |palette, cx| {
            palette.open_replacing(replacement, window, cx)
        });
        if opened || spaceterm_ui::window_modal_is_open(window, cx) {
            return true;
        }
        self.settle(false, cx);
        false
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the panel currently owns terminal input, including the window between an accepted
    /// open request and the palette actually opening.
    pub(super) fn blocks_terminal_input(&self) -> bool {
        self.open || self.pending_open
    }

    pub(super) fn input_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.palette.read(cx).editor_is_focused(window, cx)
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
        window: &mut Window,
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
                let source = *activation.item_id();
                if source != NewWorkspaceSource::RemoteProject {
                    self.palette
                        .update(cx, |palette, cx| palette.dismiss(window, cx));
                }
                cx.emit(NewWorkspacePanelEvent::SourceSelected(source));
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
        selected_source: Option<NewWorkspaceSource>,
    }

    impl NewWorkspacePanelHarness {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let panel = cx.new(|cx| NewWorkspacePanel::new(window, cx));
            cx.subscribe(&panel, |harness, _, event: &NewWorkspacePanelEvent, cx| {
                if let NewWorkspacePanelEvent::SourceSelected(source) = event {
                    harness.selected_source = Some(*source);
                    cx.notify();
                }
            })
            .detach();

            Self {
                panel,
                prior_focus: cx.focus_handle(),
                selected_source: None,
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
    ) -> (
        Entity<NewWorkspacePanelHarness>,
        Entity<NewWorkspacePanel>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let (harness, cx) = cx.add_window_view(NewWorkspacePanelHarness::new);
        let panel = harness.read_with(cx, |harness, _| harness.panel.clone());
        cx.update(|window, cx| harness.read(cx).prior_focus.focus(window));
        cx.run_until_parked();
        (harness, panel, cx)
    }

    fn open(panel: &Entity<NewWorkspacePanel>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| panel.update(cx, |panel, cx| panel.open(window, cx)));
        cx.run_until_parked();
    }

    #[test]
    fn local_project_should_lead_so_the_default_selection_opens_the_picker() {
        assert_eq!(
            NewWorkspaceSource::ordered(),
            [
                NewWorkspaceSource::LocalProject,
                NewWorkspaceSource::Scratch,
                NewWorkspaceSource::RemoteProject,
            ]
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
    fn remote_project_should_be_enabled_without_a_coming_soon_accessory() {
        let remote = NewWorkspaceSource::RemoteProject;

        assert!(!remote.into_palette_item(None).is_disabled());
        assert_eq!(remote.accessory(), None);
        assert_eq!(remote.icon(), "globe");
    }

    #[test]
    fn unavailable_remote_project_should_be_disabled_with_the_actionable_probe_reason() {
        let reason = "OpenSSH 9.0 or later is required";
        let remote = NewWorkspaceSource::RemoteProject.into_palette_item(Some(reason.to_owned()));

        assert!(remote.is_disabled());
        assert_eq!(remote.description_text(), Some(reason));
    }

    #[gpui::test]
    fn opening_should_present_every_source_and_block_terminal_input(cx: &mut TestAppContext) {
        let (_, panel, cx) = new_workspace_panel(cx);

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
        let (_, panel, cx) = new_workspace_panel(cx);

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
    fn remote_project_should_be_keyboard_selectable_and_emit_its_source(cx: &mut TestAppContext) {
        let (harness, panel, cx) = new_workspace_panel(cx);

        open(&panel, cx);
        cx.simulate_keystrokes("down down");
        cx.run_until_parked();

        assert!(
            cx.debug_bounds(NewWorkspaceSource::RemoteProject.debug_selector())
                .is_some(),
            "Remote over SSH was not rendered"
        );
        assert_eq!(
            panel.read_with(cx, |panel, cx| panel
                .palette()
                .read(cx)
                .selected_item_id()
                .copied()),
            Some(NewWorkspaceSource::RemoteProject)
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            harness.read_with(cx, |harness, _| harness.selected_source),
            Some(NewWorkspaceSource::RemoteProject)
        );
        assert!(panel.read_with(cx, |panel, _| panel.is_open()));
    }

    #[gpui::test]
    fn local_and_scratch_should_keep_their_completed_activation_behavior(cx: &mut TestAppContext) {
        let (harness, panel, cx) = new_workspace_panel(cx);

        open(&panel, cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            harness.read_with(cx, |harness, _| harness.selected_source),
            Some(NewWorkspaceSource::LocalProject)
        );
        assert!(!panel.read_with(cx, |panel, _| panel.is_open()));
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));

        open(&panel, cx);
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        assert_eq!(
            harness.read_with(cx, |harness, _| harness.selected_source),
            Some(NewWorkspaceSource::Scratch)
        );
        assert!(!panel.read_with(cx, |panel, _| panel.is_open()));
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));
    }

    #[gpui::test]
    fn open_escape_reopen_should_restore_then_retake_the_first_responder(cx: &mut TestAppContext) {
        let (harness, panel, cx) = new_workspace_panel(cx);

        open(&panel, cx);
        assert!(cx.update(|window, cx| panel.read(cx).input_is_focused(window, cx)));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!panel.read_with(cx, |panel, _| panel.blocks_terminal_input()));
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));

        open(&panel, cx);
        assert!(panel.read_with(cx, |panel, _| panel.blocks_terminal_input()));
        assert!(cx.update(|window, cx| panel.read(cx).input_is_focused(window, cx)));
        assert!(!cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));
    }

    #[gpui::test]
    fn replacement_wrappers_should_be_exact_once_and_preserve_original_focus(
        cx: &mut TestAppContext,
    ) {
        let (harness, panel, cx) = new_workspace_panel(cx);
        open(&panel, cx);

        cx.update(|window, cx| {
            let replacement = panel
                .update(cx, |panel, cx| panel.dismiss_for_replacement(window, cx))
                .expect("an open source panel should transfer its focus chain");
            assert!(
                panel
                    .update(cx, |panel, cx| panel.dismiss_for_replacement(window, cx))
                    .is_none()
            );
            assert!(panel.update(cx, |panel, cx| {
                panel.open_replacing(replacement, window, cx)
            }));
        });
        cx.run_until_parked();

        assert!(cx.update(|window, cx| panel.read(cx).input_is_focused(window, cx)));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));
    }

    #[gpui::test]
    fn dismissing_should_release_terminal_input(cx: &mut TestAppContext) {
        let (_, panel, cx) = new_workspace_panel(cx);

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
