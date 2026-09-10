use super::pane_lifecycle::{PaneConstruction, RemoteHierarchyLifecycle};
use crate::domain::PinnedDirectory;
use crate::domain::remote_workspace::RemoteRestartBatch;
use crate::terminal::metadata::CurrentDirectory;
use std::collections::BTreeMap;

use thiserror::Error;

use super::terminal_focus::{TerminalFocusBlocker, TerminalFocusCoordinator, TerminalProductFocus};
use super::{
    ClosePane, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, PaneOrigin,
    PreparedRemotePaneRestart, RemoteChildLaunchUnavailable, RemotePaneLifecycleError, SplitDown,
    SplitRight, TERMINAL_KEY_CONTEXT, TerminalPane, TerminalPaneEvent, TogglePaneZoom,
};

#[derive(Debug, Error)]
/// A typed rejection while coordinating Remote lifecycle across one Tab's Pane hierarchy.
pub(crate) enum RemotePaneHostLifecycleError {
    #[error("Pane {pane_id} cannot change remote session lifecycle: {source}")]
    Pane {
        pane_id: PaneId,
        #[source]
        source: RemotePaneLifecycleError,
    },
    #[error("the prepared restart belongs to Tab {prepared}, not Tab {current}")]
    TabChanged { prepared: TabId, current: TabId },
    #[error("Pane {0} changed after remote restart preparation")]
    PaneChanged(PaneId),
}

/// Move-only restart reservations for every Pane in one unchanged Tab hierarchy.
///
/// The token is valid only while Tab, Pane, and session-epoch identities remain unchanged.
pub(crate) struct PreparedPaneHostRemoteRestart {
    tab_id: TabId,
    panes: RemoteRestartBatch<(PaneId, Entity<TerminalPane>, PreparedRemotePaneRestart)>,
}
use crate::domain::{
    ClosePaneOutcome, FocusDirection, PaneId, PaneNodeRef, PaneSize, PaneTreeRef, SplitAxis,
    SplitId, TabId, TerminalTab, WorkspaceId, ZoomState,
};
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch,
    WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, DefiniteLength, Entity, EventEmitter, MouseDownEvent, Pixels,
    PromptButton, PromptLevel, Render, Window, div, px, relative, rgba,
};
use spaceterm_ui::{
    ButtonSize, ButtonVariant, Icon, IconButton, IconName, ResizeAxis, ResizeHandle,
    ResizeHandleEvent, ResizeInputSource, Tooltip,
};

const DIVIDER_SIZE: f32 = super::resize_handle_theme::VISIBLE_THICKNESS;
/// The Pane Caption's outer height, which its top padding sits inside.
const PANE_CAPTION_HEIGHT: f32 = 32.0;
/// Space held above the caption row, so the header breathes away from the chrome above it.
const PANE_CAPTION_TOP_PADDING: f32 = 8.0;
const PANE_CAPTION_LEFT_PADDING: f32 = 10.0;
const PANE_CAPTION_RIGHT_PADDING: f32 = 5.0;
const PANE_CAPTION_TEXT_SIZE: f32 = 12.65;
/// Width reserved by the middle dot that separates a directory from its Pane label.
const PANE_CAPTION_SEPARATOR_WIDTH: f32 = 17.25;
const PANE_ORIGIN_ICON_SIZE: f32 = 13.8;
const PANE_ORIGIN_ICON_GAP: f32 = 6.0;
/// Width reserved by the chevron that separates a Pane's origin from its directory.
const PANE_ORIGIN_SEPARATOR_WIDTH: f32 = 18.4;
const PANE_CONTROL_SIZE: f32 = 20.0;
const PANE_CONTROL_GAP: f32 = 2.0;
const PANE_CONTROL_ICON_SIZE: f32 = 13.8;
/// The zoom glyphs' own size, drawn slightly smaller than their neighbours.
///
/// The arrows reach into all four corners of their em box, so they read larger at the size the
/// caption's other controls share. This applies to the Pane Caption's zoom control alone.
const PANE_ZOOM_ICON_SIZE: f32 = 12.45;
/// The close glyph's own size, drawn slightly larger than its neighbours.
///
/// An X occupies less of its em box than the panel and zoom glyphs, so it reads smaller at the
/// size they share. This applies to the Pane Caption's close control alone.
const PANE_CLOSE_ICON_SIZE: f32 = 16.45;
const PANE_CONTROL_LEADING_GAP: f32 = 6.0;
const PANE_ATTENTION_WIDTH: f32 = 13.0;
const MINIMUM_PANE_WIDTH: f32 = PANE_CAPTION_LEFT_PADDING
    + PANE_CAPTION_RIGHT_PADDING
    + PANE_ATTENTION_WIDTH
    + PANE_CONTROL_LEADING_GAP
    + PANE_CONTROL_SIZE * 2.0
    + PANE_CONTROL_GAP;
const MINIMUM_PANE_HEIGHT: f32 = PANE_CAPTION_HEIGHT + 4.0;

#[derive(Clone, Copy)]
enum PaneCaptionAction {
    SplitRight,
    SplitDown,
    ToggleZoom,
    Close,
}

impl PaneCaptionAction {
    /// The glyph size this control paints at.
    const fn icon_size(self) -> f32 {
        match self {
            Self::SplitRight | Self::SplitDown => PANE_CONTROL_ICON_SIZE,
            Self::ToggleZoom => PANE_ZOOM_ICON_SIZE,
            Self::Close => PANE_CLOSE_ICON_SIZE,
        }
    }
}

/// Which caption segments this frame's Pane width can hold.
///
/// The Pane name is never dropped. Segments leave in order of how little they identify the Pane:
/// the running label first, then the account, then the leading directory, then the machine, and
/// last the origin icon, so the narrowest Pane still names the directory it sits in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CaptionLayout {
    show_origin: bool,
    show_host: bool,
    show_directory: bool,
    show_user: bool,
    show_label: bool,
    show_splits: bool,
}

/// How many caption segments beyond the Pane name a narrowing caption can give up.
const CAPTION_LADDER: usize = 5;

/// Rendered widths of the caption segments, each including the separator that precedes it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CaptionMetrics {
    origin_icon: Pixels,
    user: Pixels,
    host: Pixels,
    directory: Pixels,
    name: Pixels,
    label: Pixels,
}

impl CaptionMetrics {
    fn measure(text: &PaneCaptionText, window: &Window) -> Self {
        let host = measure_caption_segment(&text.origin.host, window);
        Self {
            origin_icon: if text.origin.is_empty() {
                px(0.0)
            } else {
                px(PANE_ORIGIN_ICON_SIZE + PANE_ORIGIN_ICON_GAP)
            },
            user: if text.origin.user.is_empty() {
                px(0.0)
            } else {
                measure_caption_segment(&origin_account(&text.origin.user), window)
            },
            host: if host > px(0.0) {
                host + px(PANE_ORIGIN_SEPARATOR_WIDTH)
            } else {
                host
            },
            directory: measure_caption_segment(&text.directory, window),
            name: measure_caption_segment(&text.name, window),
            label: if text.label.is_empty() {
                px(0.0)
            } else {
                measure_caption_segment(&text.label, window) + px(PANE_CAPTION_SEPARATOR_WIDTH)
            },
        }
    }

    /// The droppable segments in the order a narrowing caption gives them up, last one first.
    const fn ladder(self) -> [Pixels; CAPTION_LADDER] {
        [
            self.origin_icon,
            self.host,
            self.directory,
            self.user,
            self.label,
        ]
    }
}

impl CaptionLayout {
    fn resolve(caption: &PaneCaption, width: Pixels, window: &Window) -> Self {
        Self::from_metrics(
            caption.attention,
            caption.has_multiple_panes,
            width,
            CaptionMetrics::measure(&caption.text, window),
        )
    }

    fn from_metrics(
        attention: bool,
        has_multiple_panes: bool,
        width: Pixels,
        metrics: CaptionMetrics,
    ) -> Self {
        let full_control_count = if has_multiple_panes { 4 } else { 2 };
        let fixed_width = PANE_CAPTION_LEFT_PADDING
            + PANE_CAPTION_RIGHT_PADDING
            + if attention { PANE_ATTENTION_WIDTH } else { 0.0 };
        let show_splits = width
            >= px(fixed_width + PANE_CONTROL_LEADING_GAP + controls_width(full_control_count));
        let control_count = usize::from(show_splits) * 2 + usize::from(has_multiple_panes) * 2;
        let leading_gap = if control_count == 0 {
            0.0
        } else {
            PANE_CONTROL_LEADING_GAP
        };
        let available =
            (width - px(fixed_width + leading_gap + controls_width(control_count))).max(px(0.0));
        // The name is always kept. Every other segment is admitted in priority order and the
        // first one that does not fit ends the ladder, so segments never reappear out of order.
        let mut claimed = metrics.name;
        let mut shown = [false; CAPTION_LADDER];
        for (admitted, width) in shown.iter_mut().zip(metrics.ladder()) {
            if claimed + width > available {
                break;
            }
            claimed += width;
            *admitted = true;
        }
        let [
            show_origin,
            show_host,
            show_directory,
            show_user,
            show_label,
        ] = shown;
        Self {
            show_origin,
            show_host,
            show_directory,
            show_user,
            show_label,
            show_splits,
        }
    }
}

/// The account half of an origin, spelled the way a shell prompt spells it.
fn origin_account(user: &gpui::SharedString) -> gpui::SharedString {
    format!("{user}@").into()
}

const fn controls_width(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    count as f32 * PANE_CONTROL_SIZE + (count - 1) as f32 * PANE_CONTROL_GAP
}

fn measure_caption_segment(text: &gpui::SharedString, window: &Window) -> Pixels {
    if text.is_empty() {
        return px(0.0);
    }
    let style = window.text_style();
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::Font {
            family: style.font_family,
            features: style.font_features,
            fallbacks: style.font_fallbacks,
            weight: style.font_weight,
            style: style.font_style,
        },
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.clone(), px(PANE_CAPTION_TEXT_SIZE), &[run], None)
        .width
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum PaneHostEvent {
    UserClosePaneRequested { tab_id: TabId, pane_id: PaneId },
    CloseTabRequested { tab_id: TabId },
    PresentationChanged { tab_id: TabId },
}

impl std::fmt::Debug for PaneHostEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UserClosePaneRequested { .. } => "PaneHostEvent::UserClosePaneRequested",
            Self::CloseTabRequested { .. } => "PaneHostEvent::CloseTabRequested",
            Self::PresentationChanged { .. } => "PaneHostEvent::PresentationChanged",
        })
    }
}

pub(crate) struct PaneHost {
    terminal_tab: TerminalTab<Entity<TerminalPane>>,
    session_factory: WorkspaceTerminalSessionFactory,
    pane_construction: PaneConstruction,
    pane_bounds: BTreeMap<PaneId, Bounds<Pixels>>,
    pane_layout_size: Option<PaneSize>,
    split_bounds: BTreeMap<SplitId, Bounds<Pixels>>,
    pane_titles: BTreeMap<PaneId, gpui::SharedString>,
    pane_captions: BTreeMap<PaneId, PaneCaptionText>,
    pane_attention: BTreeMap<PaneId, u32>,
    resizing_split_id: Option<SplitId>,
    active: bool,
    focus_branch_blocker: Option<TerminalFocusBlocker>,
    native_service_hierarchy_generation: u64,
    native_service_focus_signature: Option<(bool, PaneId, Option<TerminalFocusBlocker>)>,
    close_tab_requested: bool,
    remote_lifecycle: RemoteHierarchyLifecycle,
}

impl PaneHost {
    #[cfg(test)]
    pub(crate) fn new(
        tab_id: TabId,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prepared_launch = match session_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(error) => panic!("test PaneHost channel preparation failed: {error}"),
        };
        Self::new_with_prepared_launch(
            tab_id,
            session_factory,
            prepared_launch,
            PaneConstruction::testing(),
            window,
            cx,
        )
    }

    pub(crate) fn new_with_prepared_launch(
        tab_id: TabId,
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        pane_construction: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let minimum_pane_size = match PaneSize::new(MINIMUM_PANE_WIDTH, MINIMUM_PANE_HEIGHT) {
            Ok(size) => size,
            Err(error) => {
                unreachable!("fixed minimum Pane dimensions must be valid: {error}")
            }
        };
        let terminal_tab = TerminalTab::new(tab_id, minimum_pane_size, |pane_id| {
            Self::create_terminal(
                pane_id,
                session_factory.clone(),
                prepared_launch,
                pane_construction.clone(),
                window,
                cx,
            )
        });
        let initial_pane_id = terminal_tab.focused_pane_id();
        let Some(initial_terminal) = terminal_tab.terminal(initial_pane_id) else {
            unreachable!("a new Tab must own its initial Pane terminal")
        };
        let initial_title = initial_terminal.read(cx).title();
        let initial_caption = PaneCaptionText::from_terminal(initial_terminal.read(cx));

        Self {
            terminal_tab,
            session_factory,
            pane_construction,
            pane_bounds: BTreeMap::new(),
            pane_layout_size: None,
            split_bounds: BTreeMap::new(),
            pane_titles: BTreeMap::from([(initial_pane_id, initial_title)]),
            pane_captions: BTreeMap::from([(initial_pane_id, initial_caption)]),
            pane_attention: BTreeMap::from([(initial_pane_id, 0)]),
            resizing_split_id: None,
            active: true,
            focus_branch_blocker: None,
            native_service_hierarchy_generation: 0,
            native_service_focus_signature: None,
            close_tab_requested: false,
            remote_lifecycle: RemoteHierarchyLifecycle::default(),
        }
    }

    fn create_terminal(
        pane_id: PaneId,
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        pane_construction: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let terminal =
            cx.new(|cx| pane_construction.create(session_factory, prepared_launch, window, cx));
        cx.subscribe_in(
            &terminal,
            window,
            move |host, terminal, event: &TerminalPaneEvent, window, cx| match event {
                TerminalPaneEvent::FocusRequested => host.focus_pane(pane_id, cx),
                TerminalPaneEvent::TitleChanged(title) => {
                    host.pane_titles.insert(pane_id, title.clone());
                    cx.emit(PaneHostEvent::PresentationChanged {
                        tab_id: host.terminal_tab.id(),
                    });
                    cx.notify();
                }
                TerminalPaneEvent::CaptionChanged => {
                    let caption = PaneCaptionText::from_terminal(terminal.read(cx));
                    if host.pane_captions.get(&pane_id) != Some(&caption) {
                        host.pane_captions.insert(pane_id, caption);
                        cx.notify();
                    }
                }
                TerminalPaneEvent::AttentionChanged { unread_count } => {
                    host.pane_attention.insert(pane_id, *unread_count);
                    cx.emit(PaneHostEvent::PresentationChanged {
                        tab_id: host.terminal_tab.id(),
                    });
                    cx.notify();
                }
                TerminalPaneEvent::Exited => host.close_pane(pane_id, window, cx),
            },
        )
        .detach();
        terminal
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &App) {
        let Some(terminal) = self
            .terminal_tab
            .terminal(self.terminal_tab.focused_pane_id())
        else {
            return;
        };
        terminal.read(cx).focus(window);
    }

    pub(crate) fn native_service_status(
        &mut self,
        workspace_id: WorkspaceId,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> NativeServiceStatus {
        self.sync_terminal_focus(cx);
        let pane_id = self.terminal_tab.focused_pane_id();
        let tab_id = self.terminal_tab.id();
        let hierarchy_generation = self.native_service_hierarchy_generation;
        let Some(terminal) = self.terminal_tab.terminal(pane_id) else {
            return NativeServiceStatus::default();
        };
        terminal.update(cx, |terminal, cx| {
            terminal.native_service_status(
                workspace_id,
                tab_id,
                pane_id,
                hierarchy_generation,
                window,
                cx,
            )
        })
    }

    pub(crate) fn native_service_target(
        &self,
        origin: NativeServiceOrigin,
    ) -> Option<Entity<TerminalPane>> {
        if self.terminal_tab.id() != origin.tab_id()
            || self.terminal_tab.focused_pane_id() != origin.pane_id()
            || self.native_service_hierarchy_generation != origin.hierarchy_generation()
        {
            return None;
        }
        self.terminal_tab.terminal(origin.pane_id()).cloned()
    }

    pub(crate) const fn tab_id(&self) -> TabId {
        self.terminal_tab.id()
    }

    pub(crate) fn pane_count(&self) -> usize {
        self.terminal_tab.pane_count()
    }

    pub(crate) fn current_directory(&self, pane_id: PaneId, cx: &App) -> Option<CurrentDirectory> {
        self.terminal_tab
            .terminal(pane_id)
            .and_then(|terminal| terminal.read(cx).current_directory())
    }

    pub(crate) fn terminal_panes<'a>(
        &'a self,
        cx: &'a App,
    ) -> impl Iterator<Item = (PaneId, &'a TerminalPane)> {
        self.terminal_tab
            .terminals_with_ids()
            .map(|(id, terminal)| (id, terminal.read(cx)))
    }

    pub(crate) fn set_pinned_directory(&mut self, directory: Option<PinnedDirectory>) {
        self.session_factory.set_pinned_directory(directory);
    }

    pub(crate) fn tab_title(&self) -> gpui::SharedString {
        let attention = self.pane_attention.values().copied().sum::<u32>();
        let pane_count = self.terminal_tab.pane_count();
        let title = self
            .pane_titles
            .get(&self.terminal_tab.focused_pane_id())
            .cloned()
            .unwrap_or_else(|| "Terminal".into());
        let title = if pane_count > 1 {
            format!("{title} · {pane_count} Panes").into()
        } else {
            title
        };
        if attention > 0 {
            format!("• {title}").into()
        } else {
            title
        }
    }

    #[cfg(test)]
    pub(crate) const fn zoom_state(&self) -> ZoomState {
        self.terminal_tab.zoom_state()
    }

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.focus(window, cx);
    }

    pub(crate) fn activate_without_focus(&mut self, cx: &mut Context<Self>) {
        self.set_focus_branch(true, None, cx);
        cx.notify();
    }

    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.set_focus_branch(false, None, cx);
        cx.notify();
    }

    pub(crate) fn set_focus_branch(
        &mut self,
        active: bool,
        blocker: Option<TerminalFocusBlocker>,
        cx: &mut Context<Self>,
    ) {
        self.active = active;
        self.focus_branch_blocker = blocker;
        self.sync_terminal_focus(cx);
    }

    pub(crate) fn close_all(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.sync_terminal_focus(cx);
        for terminal in self.terminal_tab.terminals() {
            terminal.update(cx, |terminal, _| terminal.close());
        }
    }

    /// Atomically marks every Pane in this Tab disconnected for one generation.
    ///
    /// All Panes are prevalidated before any mutation. The Pane tree, focus, zoom, and retained
    /// presentations remain intact, while new child launches and terminal input are blocked.
    pub(crate) fn disconnect_remote(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        self.can_disconnect_remote(generation, cx)?;
        for (_, terminal) in self.terminal_tab.terminals_with_ids() {
            terminal.update(cx, |terminal, cx| {
                terminal
                    .disconnect_remote(generation, cx)
                    .expect("prevalidated remote disconnect must remain legal")
            });
        }
        self.remote_lifecycle.disconnect(generation);
        self.sync_terminal_focus(cx);
        Ok(())
    }

    /// Prevalidates a hierarchy-wide disconnect without mutating any Pane.
    pub(crate) fn can_disconnect_remote(
        &self,
        generation: u64,
        cx: &App,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        for (pane_id, terminal) in self.terminal_tab.terminals_with_ids() {
            terminal
                .read(cx)
                .can_disconnect_remote(generation)
                .map_err(|source| RemotePaneHostLifecycleError::Pane { pane_id, source })?;
        }
        Ok(())
    }

    /// Binds one already-reserved launch to every existing Pane without mutating the hierarchy.
    ///
    /// The launch count and Pane identities must match exactly. Any failure drops the aggregate
    /// token and leaves all Panes disconnected and unchanged.
    pub(crate) fn prepare_remote_restart(
        &self,
        session_factory: WorkspaceTerminalSessionFactory,
        generation: u64,
        prepared_launches: Vec<PreparedWorkspaceTerminalLaunch>,
        cx: &App,
    ) -> Result<PreparedPaneHostRemoteRestart, RemotePaneHostLifecycleError> {
        if self.terminal_tab.pane_count() != prepared_launches.len() {
            return Err(RemotePaneHostLifecycleError::PaneChanged(
                self.terminal_tab.focused_pane_id(),
            ));
        }
        let mut panes = Vec::with_capacity(self.terminal_tab.pane_count());
        for ((pane_id, terminal), prepared_launch) in self
            .terminal_tab
            .terminals_with_ids()
            .zip(prepared_launches)
        {
            let prepared = terminal
                .read(cx)
                .prepare_remote_restart(session_factory.clone(), generation, prepared_launch)
                .map_err(|source| RemotePaneHostLifecycleError::Pane { pane_id, source })?;
            panes.push((pane_id, terminal.clone(), prepared));
        }
        Ok(PreparedPaneHostRemoteRestart {
            tab_id: self.terminal_tab.id(),
            panes: RemoteRestartBatch::new(panes),
        })
    }

    /// Revalidates every prepared Pane restart against the current Tab hierarchy.
    pub(crate) fn can_commit_remote_restart(
        &self,
        prepared: &PreparedPaneHostRemoteRestart,
        cx: &App,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        if self.terminal_tab.id() != prepared.tab_id {
            return Err(RemotePaneHostLifecycleError::TabChanged {
                prepared: prepared.tab_id,
                current: self.terminal_tab.id(),
            });
        }
        prepared.panes.validate(
            self.terminal_tab.pane_count(),
            || RemotePaneHostLifecycleError::PaneChanged(self.terminal_tab.focused_pane_id()),
            |(pane_id, terminal, pane_restart)| {
                self.validate_restart_pane(*pane_id, terminal, pane_restart, cx)
            },
        )
    }

    fn validate_restart_pane(
        &self,
        pane_id: PaneId,
        terminal: &Entity<TerminalPane>,
        pane_restart: &PreparedRemotePaneRestart,
        cx: &App,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        let Some(current) = self.terminal_tab.terminal(pane_id) else {
            return Err(RemotePaneHostLifecycleError::PaneChanged(pane_id));
        };
        if current.entity_id() != terminal.entity_id() {
            return Err(RemotePaneHostLifecycleError::PaneChanged(pane_id));
        }
        terminal
            .read(cx)
            .can_commit_remote_restart(pane_restart)
            .map_err(|source| RemotePaneHostLifecycleError::Pane { pane_id, source })?;
        Ok(())
    }

    /// Commits every prevalidated Pane restart in place after aggregate preparation succeeds.
    ///
    /// Tab, Pane-tree, focus, and zoom identities are preserved. Once commit begins, later
    /// Terminal Session startup failure belongs to its individual Pane rather than rolling back
    /// already committed siblings.
    pub(crate) fn commit_remote_restart(
        &mut self,
        prepared: PreparedPaneHostRemoteRestart,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        if self.terminal_tab.id() != prepared.tab_id {
            return Err(RemotePaneHostLifecycleError::TabChanged {
                prepared: prepared.tab_id,
                current: self.terminal_tab.id(),
            });
        }
        prepared.panes.commit(
            self.terminal_tab.pane_count(),
            || RemotePaneHostLifecycleError::PaneChanged(self.terminal_tab.focused_pane_id()),
            cx,
            |(pane_id, terminal, pane_restart), cx| {
                self.validate_restart_pane(*pane_id, terminal, pane_restart, cx)
            },
            |(pane_id, terminal, pane_restart), cx| {
                terminal.update(cx, |terminal, cx| {
                    terminal
                        .commit_remote_restart(pane_restart, window, cx)
                        .unwrap_or_else(|error| {
                            panic!("prevalidated Pane {pane_id} restart commit failed: {error}")
                        })
                });
            },
        )?;
        self.session_factory = session_factory;
        self.remote_lifecycle.restarted();
        self.sync_terminal_focus(cx);
        cx.emit(PaneHostEvent::PresentationChanged {
            tab_id: self.terminal_tab.id(),
        });
        cx.notify();
        Ok(())
    }

    pub(crate) const fn focused_pane_id(&self) -> PaneId {
        self.terminal_tab.focused_pane_id()
    }

    #[cfg(test)]
    pub(crate) fn pane_entity_ids(&self) -> Vec<(PaneId, gpui::EntityId)> {
        self.terminal_tab
            .terminals_with_ids()
            .map(|(pane_id, terminal)| (pane_id, terminal.entity_id()))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn layout_signature(&self) -> String {
        fn encode(tree: PaneTreeRef<'_>, signature: &mut String) {
            match tree.node() {
                PaneNodeRef::Leaf { pane_id } => {
                    signature.push_str(&format!("pane:{}", pane_id.get()));
                }
                PaneNodeRef::Split {
                    split_id,
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    signature.push_str(&format!("split:{}:{axis:?}:{ratio}(", split_id.get()));
                    encode(first, signature);
                    signature.push(',');
                    encode(second, signature);
                    signature.push(')');
                }
            }
        }

        let mut signature = String::new();
        encode(self.terminal_tab.root(), &mut signature);
        signature
    }

    #[cfg(test)]
    pub(crate) const fn remote_disconnected_generation(&self) -> Option<u64> {
        self.remote_lifecycle.disconnected_generation()
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_remote_state(&self, cx: &App) -> (bool, bool) {
        self.terminal_tab
            .terminal(self.terminal_tab.focused_pane_id())
            .map(|terminal| {
                let terminal = terminal.read(cx);
                terminal.remote_session_state()
            })
            .unwrap_or((false, false))
    }

    #[cfg(test)]
    pub(crate) fn terminal_restart_states(
        &self,
        cx: &App,
    ) -> Vec<(PaneId, bool, Option<&'static str>)> {
        self.terminal_tab
            .terminals_with_ids()
            .map(|(pane_id, terminal)| {
                let terminal = terminal.read(cx);
                let (session_attached, failure_operation) = terminal.restart_state();
                (pane_id, session_attached, failure_operation)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.terminal_tab
            .terminal(self.terminal_tab.focused_pane_id())
            .is_some_and(|terminal| terminal.read(cx).is_focused(window))
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_has_input_focus(&self, window: &Window, cx: &App) -> bool {
        self.terminal_tab
            .terminal(self.terminal_tab.focused_pane_id())
            .is_some_and(|terminal| terminal.read(cx).terminal_input_focused(window, cx))
    }

    #[cfg(test)]
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    fn focus_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        if self.terminal_tab.focused_pane_id() == pane_id {
            return;
        }
        if let Err(error) = self.terminal_tab.focus_pane(pane_id) {
            eprintln!("failed to focus Pane: {error}");
            return;
        }
        self.sync_terminal_focus(cx);
        cx.notify();
    }

    fn focus_pane_in_direction(
        &mut self,
        direction: FocusDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane_id) = self.terminal_tab.focus_pane_in_direction(direction) else {
            return;
        };
        self.sync_terminal_focus(cx);
        cx.notify();
        if let Some(terminal) = self.terminal_tab.terminal(pane_id) {
            terminal.update(cx, |terminal, _| terminal.focus(window));
        }
    }

    pub(crate) fn split_focused(
        &mut self,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused_pane_id = self.terminal_tab.focused_pane_id();
        self.split_pane(focused_pane_id, axis, window, cx);
    }

    fn split_pane(
        &mut self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote_lifecycle.disconnected_generation().is_some() {
            cx.emit(RemoteChildLaunchUnavailable::ConnectionUnavailable);
            return;
        }
        let Some(target_size) = self.split_target_size(target_pane_id) else {
            eprintln!("cannot split Pane {target_pane_id} without valid measured bounds");
            return;
        };
        let session_factory = match self
            .session_factory
            .for_source_directory(self.current_directory(target_pane_id, cx))
        {
            Ok(factory) => factory,
            Err(error) => {
                let detail = format!(
                    "Cannot create a Pane because {error}. Restore the directory or change the pinned directory."
                );
                drop(window.prompt(
                    PromptLevel::Warning,
                    "Starting Directory Unavailable",
                    Some(&detail),
                    &[PromptButton::ok("OK")],
                    cx,
                ));
                return;
            }
        };
        if let Some(revalidation) = session_factory.revalidate_remote_child_launch() {
            let child_launch_generation = self.remote_lifecycle.begin_child_launch();
            cx.spawn_in(window, async move |host, cx| {
                let revalidation = revalidation.await;
                let _ = host.update_in(cx, |host, window, cx| {
                    if host.remote_lifecycle.disconnected_generation().is_some() {
                        cx.emit(RemoteChildLaunchUnavailable::Cancelled);
                        return;
                    }
                    if !host
                        .remote_lifecycle
                        .is_current_child_launch(child_launch_generation)
                    {
                        cx.emit(RemoteChildLaunchUnavailable::Stale);
                        return;
                    }
                    if let Err(error) = revalidation {
                        cx.emit(RemoteChildLaunchUnavailable::from(error));
                        return;
                    }
                    let prepared_launch = match session_factory.prepare_child_launch() {
                        Ok(prepared_launch) => prepared_launch,
                        Err(_) => {
                            cx.emit(RemoteChildLaunchUnavailable::ConnectionUnavailable);
                            return;
                        }
                    };
                    let Some(current_size) = host.split_target_size(target_pane_id) else {
                        return;
                    };
                    if current_size != target_size {
                        return;
                    }
                    host.split_pane_with_prepared_launch(
                        target_pane_id,
                        axis,
                        target_size,
                        prepared_launch,
                        window,
                        cx,
                    );
                });
            })
            .detach();
            return;
        }
        let prepared_launch = match session_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(_) => {
                cx.emit(RemoteChildLaunchUnavailable::ConnectionUnavailable);
                return;
            }
        };
        self.split_pane_with_prepared_launch(
            target_pane_id,
            axis,
            target_size,
            prepared_launch,
            window,
            cx,
        );
    }

    fn split_target_size(&self, pane_id: PaneId) -> Option<PaneSize> {
        match self.terminal_tab.zoom_state() {
            ZoomState::Restored => self
                .pane_bounds
                .get(&pane_id)
                .and_then(|bounds| pane_size(*bounds).ok()),
            ZoomState::Zoomed(_) => {
                restored_leaf_size(self.terminal_tab.root(), pane_id, self.pane_layout_size?)
            }
        }
    }

    fn split_pane_with_prepared_launch(
        &mut self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        target_size: PaneSize,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_factory = self.session_factory.clone();
        let pane_construction = self.pane_construction.clone();
        let result = self.terminal_tab.split_pane(
            target_pane_id,
            axis,
            target_size,
            DIVIDER_SIZE,
            |new_pane_id| {
                Self::create_terminal(
                    new_pane_id,
                    session_factory,
                    prepared_launch,
                    pane_construction,
                    window,
                    cx,
                )
            },
        );

        match result {
            Ok(pane_id) => {
                self.advance_native_service_hierarchy_generation(cx);
                if let Some(terminal) = self.terminal_tab.terminal(pane_id) {
                    self.pane_titles.insert(pane_id, terminal.read(cx).title());
                    self.pane_captions
                        .insert(pane_id, PaneCaptionText::from_terminal(terminal.read(cx)));
                }
                self.pane_attention.insert(pane_id, 0);
                self.split_bounds.clear();
                self.sync_terminal_focus(cx);
                cx.emit(PaneHostEvent::PresentationChanged {
                    tab_id: self.terminal_tab.id(),
                });
                cx.notify();
                if let Some(terminal) = self.terminal_tab.terminal(pane_id) {
                    terminal.update(cx, |terminal, _| terminal.focus(window));
                }
            }
            Err(error) => eprintln!("failed to split Pane: {error}"),
        }
    }

    #[cfg(test)]
    fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_pane(self.terminal_tab.focused_pane_id(), window, cx);
    }

    fn request_close_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        if self.close_tab_requested || self.terminal_tab.terminal(pane_id).is_none() {
            return;
        }
        cx.emit(PaneHostEvent::UserClosePaneRequested {
            tab_id: self.terminal_tab.id(),
            pane_id,
        });
    }

    pub(crate) fn close_pane_authorized(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_pane(pane_id, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn close_focused_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_focused(window, cx);
    }

    fn close_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_tab_requested {
            return;
        }

        match self.terminal_tab.close_pane(pane_id) {
            Ok(ClosePaneOutcome::CloseTab { tab_id }) => {
                self.advance_native_service_hierarchy_generation(cx);
                self.close_tab_requested = true;
                self.active = false;
                self.sync_terminal_focus(cx);
                cx.emit(PaneHostEvent::CloseTabRequested { tab_id });
            }
            Ok(ClosePaneOutcome::PaneClosed {
                focused_pane_id,
                closed_terminal,
                ..
            }) => {
                self.advance_native_service_hierarchy_generation(cx);
                closed_terminal.update(cx, |terminal, _| {
                    terminal.set_accessibility_hierarchy(false, usize::MAX);
                    terminal.close();
                });
                self.pane_bounds.remove(&pane_id);
                self.split_bounds.clear();
                self.pane_titles.remove(&pane_id);
                self.pane_captions.remove(&pane_id);
                self.pane_attention.remove(&pane_id);
                self.sync_terminal_focus(cx);
                cx.emit(PaneHostEvent::PresentationChanged {
                    tab_id: self.terminal_tab.id(),
                });
                cx.notify();
                if self.active
                    && let Some(terminal) = self.terminal_tab.terminal(focused_pane_id)
                {
                    terminal.update(cx, |terminal, _| terminal.focus(window));
                }
            }
            Err(error) => eprintln!("failed to close Pane: {error}"),
        }
    }

    pub(crate) fn toggle_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_tab.toggle_zoom().is_none() {
            return;
        }
        self.advance_native_service_hierarchy_generation(cx);
        self.sync_terminal_focus(cx);
        cx.emit(PaneHostEvent::PresentationChanged {
            tab_id: self.terminal_tab.id(),
        });
        cx.notify();
        self.focus(window, cx);
    }

    fn resize_split(
        &mut self,
        split_id: SplitId,
        axis: SplitAxis,
        requested_offset: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.split_bounds.get(&split_id).copied() else {
            return;
        };
        let Some(requested_ratio) = split_ratio_for_offset(axis, bounds, requested_offset) else {
            return;
        };
        let Ok(available_size) = pane_size(bounds) else {
            return;
        };
        match self.terminal_tab.resize_split(
            split_id,
            available_size,
            DIVIDER_SIZE,
            requested_ratio,
        ) {
            Ok(_) => cx.notify(),
            Err(error) => eprintln!("failed to resize split: {error}"),
        }
    }

    fn reset_split(&mut self, split_id: SplitId, cx: &mut Context<Self>) {
        let Some(bounds) = self.split_bounds.get(&split_id).copied() else {
            return;
        };
        let Ok(available_size) = pane_size(bounds) else {
            return;
        };
        match self
            .terminal_tab
            .resize_split(split_id, available_size, DIVIDER_SIZE, 0.5)
        {
            Ok(_) => cx.notify(),
            Err(error) => eprintln!("failed to reset split: {error}"),
        }
    }

    fn handle_resize_event(
        &mut self,
        split_id: SplitId,
        axis: SplitAxis,
        event: ResizeHandleEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ResizeHandleEvent::InteractionStarted { .. } => {
                self.resizing_split_id = Some(split_id);
                self.sync_terminal_focus(cx);
                cx.notify();
            }
            ResizeHandleEvent::ResizeRequested {
                requested_value, ..
            } => self.resize_split(split_id, axis, requested_value, cx),
            ResizeHandleEvent::ResetRequested { source } => {
                self.reset_split(split_id, cx);
                if source == ResizeInputSource::Pointer && self.active {
                    self.focus(window, cx);
                }
            }
            ResizeHandleEvent::InteractionFinished { source, .. } => {
                if self.resizing_split_id != Some(split_id) {
                    return;
                }
                self.resizing_split_id = None;
                self.sync_terminal_focus(cx);
                cx.notify();
                if source == ResizeInputSource::Pointer && self.active {
                    self.focus(window, cx);
                }
            }
        }
    }

    fn sync_terminal_focus(&mut self, cx: &mut Context<Self>) {
        let focused_terminal_id = self
            .terminal_tab
            .terminal(self.terminal_tab.focused_pane_id())
            .map(Entity::entity_id);
        let visible_terminal_id = match self.terminal_tab.zoom_state() {
            ZoomState::Restored => None,
            ZoomState::Zoomed(pane_id) => {
                self.terminal_tab.terminal(pane_id).map(Entity::entity_id)
            }
        };
        let blocker = TerminalFocusCoordinator::pane_layout_blocker(
            self.focus_branch_blocker,
            self.resizing_split_id.is_some(),
        );
        let signature = (self.active, self.terminal_tab.focused_pane_id(), blocker);
        if self.native_service_focus_signature != Some(signature) {
            self.advance_native_service_hierarchy_generation(cx);
            self.native_service_focus_signature = Some(signature);
        }
        let hierarchy_generation = self.native_service_hierarchy_generation;
        let mut panes = Vec::with_capacity(self.terminal_tab.pane_count());
        collect_pane_order(self.terminal_tab.root(), &mut panes);
        let presented_panes = match self.terminal_tab.zoom_state() {
            ZoomState::Zoomed(pane_id) => vec![pane_id],
            ZoomState::Restored => panes.clone(),
        };
        let presentation_order = presented_panes
            .into_iter()
            .enumerate()
            .map(|(order, pane_id)| (pane_id, order))
            .collect::<BTreeMap<_, _>>();
        for pane_id in panes {
            let Some(terminal) = self.terminal_tab.terminal(pane_id) else {
                continue;
            };
            let product_focus = TerminalProductFocus {
                active_workspace: self.active,
                active_tab: self.active,
                pane_visible: self.active
                    && visible_terminal_id.is_none_or(|visible| visible == terminal.entity_id()),
                focused_pane: Some(terminal.entity_id()) == focused_terminal_id,
                blocker,
            };
            terminal.update(cx, |terminal, cx| {
                let product_focus_changed = terminal.set_product_focus(product_focus, cx);
                terminal.synchronize_native_service_hierarchy_generation(hierarchy_generation);
                terminal.set_accessibility_hierarchy(
                    self.active && presentation_order.contains_key(&pane_id),
                    presentation_order
                        .get(&pane_id)
                        .copied()
                        .unwrap_or(usize::MAX),
                );
                if product_focus_changed {
                    cx.notify();
                }
            });
        }
    }

    fn advance_native_service_hierarchy_generation(&mut self, cx: &mut Context<Self>) {
        self.native_service_hierarchy_generation =
            self.native_service_hierarchy_generation.wrapping_add(1);
        let hierarchy_generation = self.native_service_hierarchy_generation;
        for terminal in self.terminal_tab.terminals() {
            terminal.update(cx, |terminal, _| {
                terminal.synchronize_native_service_hierarchy_generation(hierarchy_generation);
            });
        }
    }

    fn perform_caption_action(
        &mut self,
        action: PaneCaptionAction,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_tab.terminal(pane_id).is_none() {
            return;
        }
        self.focus_pane(pane_id, cx);
        match action {
            PaneCaptionAction::SplitRight => {
                self.split_pane(pane_id, SplitAxis::Horizontal, window, cx)
            }
            PaneCaptionAction::SplitDown => {
                self.split_pane(pane_id, SplitAxis::Vertical, window, cx)
            }
            PaneCaptionAction::ToggleZoom => self.toggle_zoom(window, cx),
            PaneCaptionAction::Close => {
                // Close Confirmation restores the responder it captures when presented.
                if self.active && self.focus_branch_blocker.is_none() {
                    self.focus(window, cx);
                }
                self.request_close_pane(pane_id, cx);
                return;
            }
        }
        if self.active && self.focus_branch_blocker.is_none() {
            self.focus(window, cx);
        }
    }

    fn on_split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitAxis::Horizontal, window, cx);
    }

    fn on_split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split_focused(SplitAxis::Vertical, window, cx);
    }

    fn on_focus_pane_left(
        &mut self,
        _: &FocusPaneLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane_in_direction(FocusDirection::Left, window, cx);
    }

    fn on_focus_pane_right(
        &mut self,
        _: &FocusPaneRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane_in_direction(FocusDirection::Right, window, cx);
    }

    fn on_focus_pane_up(&mut self, _: &FocusPaneUp, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_pane_in_direction(FocusDirection::Up, window, cx);
    }

    fn on_focus_pane_down(
        &mut self,
        _: &FocusPaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane_in_direction(FocusDirection::Down, window, cx);
    }

    fn on_toggle_zoom(&mut self, _: &TogglePaneZoom, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_zoom(window, cx);
    }

    fn on_close_pane(&mut self, _: &ClosePane, _: &mut Window, cx: &mut Context<Self>) {
        self.request_close_pane(self.terminal_tab.focused_pane_id(), cx);
    }

    fn render_tree(&self, tree: PaneTreeRef<'_>, host: gpui::WeakEntity<Self>) -> AnyElement {
        match tree.node() {
            PaneNodeRef::Leaf { pane_id } => self.render_leaf(pane_id, host),
            PaneNodeRef::Split {
                split_id,
                axis,
                ratio,
                first,
                second,
            } => self.render_split(split_id, axis, ratio, (first, second), host),
        }
    }

    fn render_leaf(&self, pane_id: PaneId, host: gpui::WeakEntity<Self>) -> AnyElement {
        let Some(terminal) = self.terminal_tab.terminal(pane_id).cloned() else {
            return div()
                .size_full()
                .bg(gpui_color(ACTIVE_THEME.terminal_background))
                .into_any_element();
        };
        let focused = self.terminal_tab.focused_pane_id() == pane_id;
        let has_multiple_panes = self.terminal_tab.pane_count() > 1;
        let zoomed = matches!(self.terminal_tab.zoom_state(), ZoomState::Zoomed(_));
        let text = self
            .pane_captions
            .get(&pane_id)
            .cloned()
            .unwrap_or_default();
        let pane_group = format!("pane-group-{}", pane_id.get());
        let attention = self.pane_attention.get(&pane_id).copied().unwrap_or(0) > 0;
        let measure_host = host.clone();
        let focus_host = host.clone();

        div()
            .on_children_prepainted(move |children, _, cx| {
                let Some(first) = children.first() else {
                    return;
                };
                let bounds = children
                    .get(1)
                    .map_or(*first, |terminal| first.union(terminal));
                let _ = measure_host.update(cx, |host, _| {
                    host.pane_bounds.insert(pane_id, bounds);
                });
            })
            .id(("pane", pane_id.get()))
            .group(pane_group.clone())
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .capture_any_mouse_down(move |_: &MouseDownEvent, _, cx| {
                let _ = focus_host.update(cx, |host, cx| host.focus_pane(pane_id, cx));
            })
            .child(render_pane_caption(
                PaneCaption {
                    pane_id,
                    text,
                    focused,
                    zoomed,
                    attention,
                    has_multiple_panes,
                },
                &pane_group,
                host.clone(),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(terminal),
            )
            .into_any_element()
    }

    fn render_split(
        &self,
        split_id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        children: (PaneTreeRef<'_>, PaneTreeRef<'_>),
        host: gpui::WeakEntity<Self>,
    ) -> AnyElement {
        let (first, second) = children;
        let first = self.render_tree(first, host.clone());
        let second = self.render_tree(second, host.clone());
        let measure_host = host.clone();
        let mut split = div()
            .relative()
            .on_children_prepainted(move |children, _, cx| {
                let (Some(first), Some(last)) = (children.first(), children.get(2)) else {
                    return;
                };
                let bounds = first.union(last);
                let _ = measure_host.update(cx, |host, cx| {
                    if host.split_bounds.insert(split_id, bounds) != Some(bounds) {
                        cx.notify();
                    }
                });
            })
            .id(("split", split_id.get()))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex();
        split = match axis {
            SplitAxis::Horizontal => split.flex_row(),
            SplitAxis::Vertical => split.flex_col(),
        };

        let current_offset = self
            .split_bounds
            .get(&split_id)
            .and_then(|bounds| split_content_extent(axis, *bounds))
            .map_or(0.0, |extent| extent * ratio);

        // Paint the resize target after both panes, but before sibling popovers. A deferred
        // divider would paint through command palettes, whose own menus are deferred overlays.
        let divider = div()
            .absolute()
            .child(render_divider(split_id, axis, current_offset, host));
        let (spacer, divider) = match axis {
            SplitAxis::Horizontal => (
                div().w(px(DIVIDER_SIZE)).h_full().flex_shrink_0(),
                divider
                    .top_0()
                    .bottom_0()
                    .left(relative(ratio))
                    .ml(px(-ratio * DIVIDER_SIZE)),
            ),
            SplitAxis::Vertical => (
                div().h(px(DIVIDER_SIZE)).w_full().flex_shrink_0(),
                divider
                    .left_0()
                    .right_0()
                    .top(relative(ratio))
                    .mt(px(-ratio * DIVIDER_SIZE)),
            ),
        };
        split
            .child(split_child(first, axis, ratio))
            .child(spacer)
            .child(split_child(second, axis, 1.0 - ratio))
            .child(divider)
            .into_any_element()
    }
}

impl EventEmitter<PaneHostEvent> for PaneHost {}
impl EventEmitter<RemoteChildLaunchUnavailable> for PaneHost {}

fn collect_pane_order(tree: PaneTreeRef<'_>, panes: &mut Vec<PaneId>) {
    match tree.node() {
        PaneNodeRef::Leaf { pane_id } => panes.push(pane_id),
        PaneNodeRef::Split { first, second, .. } => {
            collect_pane_order(first, panes);
            collect_pane_order(second, panes);
        }
    }
}

impl Render for PaneHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_terminal_focus(cx);
        let host = cx.entity().downgrade();
        let zoom_state = self.terminal_tab.zoom_state();
        let minimum_size = match zoom_state {
            ZoomState::Zoomed(_) => self.terminal_tab.minimum_pane_size(),
            ZoomState::Restored => match self.terminal_tab.minimum_size(DIVIDER_SIZE) {
                Ok(size) => size,
                Err(error) => {
                    eprintln!("failed to calculate minimum Pane layout size: {error}");
                    self.terminal_tab.minimum_pane_size()
                }
            },
        };
        let content = match zoom_state {
            ZoomState::Restored => self.render_tree(self.terminal_tab.root(), host.clone()),
            ZoomState::Zoomed(pane_id) => self.render_leaf(pane_id, host),
        };

        div()
            .on_children_prepainted({
                let host = cx.entity().downgrade();
                move |children, _, cx| {
                    let Some(bounds) = children.first() else {
                        return;
                    };
                    let _ = host.update(cx, |host, _| {
                        host.pane_layout_size = pane_size(*bounds).ok();
                    });
                }
            })
            .id(("pane-host", self.terminal_tab.id().get()))
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w(px(minimum_size.width()))
            .min_h(px(minimum_size.height()))
            .overflow_hidden()
            .bg(gpui_color(ACTIVE_THEME.terminal_background))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_focus_pane_left))
            .on_action(cx.listener(Self::on_focus_pane_right))
            .on_action(cx.listener(Self::on_focus_pane_up))
            .on_action(cx.listener(Self::on_focus_pane_down))
            .on_action(cx.listener(Self::on_toggle_zoom))
            .on_action(cx.listener(Self::on_close_pane))
            .child(content)
    }
}

/// One Pane caption split into the segments the header renders and drops independently.
///
/// `origin` is the account and machine the Terminal runs on, `directory` the leading path up to
/// and including its last separator, `name` the directory leaf that identifies the Pane, and
/// `label` the Terminal title or running command. `running` marks a label that is a live command.
#[derive(Clone, Default, Eq, PartialEq)]
struct PaneCaptionText {
    origin: PaneOrigin,
    directory: gpui::SharedString,
    name: gpui::SharedString,
    label: gpui::SharedString,
    running: bool,
}

impl PaneCaptionText {
    fn from_terminal(terminal: &TerminalPane) -> Self {
        let facts = terminal.caption();
        if facts.directory.is_empty() {
            return Self {
                origin: facts.origin,
                directory: gpui::SharedString::default(),
                name: facts.label,
                label: gpui::SharedString::default(),
                running: facts.running,
            };
        }
        let (leading, name) = split_directory_leaf(&facts.directory);
        Self {
            origin: facts.origin,
            directory: leading,
            name,
            label: facts.label,
            running: facts.running,
        }
    }
}

/// Splits one directory into its leading path and its leaf, keeping the separator on the lead.
fn split_directory_leaf(directory: &str) -> (gpui::SharedString, gpui::SharedString) {
    let trimmed = directory.trim_end_matches('/');
    if trimmed.is_empty() {
        return (gpui::SharedString::default(), directory.to_owned().into());
    }
    match trimmed.rfind('/') {
        Some(separator) => (
            trimmed[..=separator].to_owned().into(),
            trimmed[separator + 1..].to_owned().into(),
        ),
        None => (gpui::SharedString::default(), trimmed.to_owned().into()),
    }
}

struct PaneCaption {
    pane_id: PaneId,
    text: PaneCaptionText,
    focused: bool,
    zoomed: bool,
    attention: bool,
    has_multiple_panes: bool,
}

fn render_pane_caption(
    caption: PaneCaption,
    pane_group: &str,
    host: gpui::WeakEntity<PaneHost>,
) -> AnyElement {
    let pane_group = pane_group.to_owned();
    // Resolve controls from this frame's actual width, including during split resizing.
    gpui::canvas(
        move |bounds, window, cx| {
            let mut content = render_pane_caption_content(
                caption,
                &pane_group,
                host,
                crate::desktop_profile::DesktopPresentation::get(cx),
                bounds.size.width,
                window,
            );
            content.layout_as_root(bounds.size.map(gpui::AvailableSpace::Definite), window, cx);
            content.prepaint_at(bounds.origin, window, cx);
            content
        },
        |_, mut content, window, cx| content.paint(window, cx),
    )
    .w_full()
    .h(px(PANE_CAPTION_HEIGHT))
    .flex_shrink_0()
    .into_any_element()
}

fn render_pane_caption_content(
    caption: PaneCaption,
    pane_group: &str,
    host: gpui::WeakEntity<PaneHost>,
    presentation: &crate::desktop_profile::DesktopPresentation,
    width: Pixels,
    window: &Window,
) -> AnyElement {
    let layout = CaptionLayout::resolve(&caption, width, window);
    let PaneCaption {
        pane_id,
        text,
        focused,
        zoomed,
        attention,
        has_multiple_panes,
    } = caption;
    let color = caption_color(focused);
    let focus_host = host.clone();
    let mut controls = div()
        .id(("pane-controls", pane_id.get()))
        .debug_selector(move || {
            format!(
                "pane-controls-{}-{}",
                pane_id.get(),
                if layout.show_splits { "full" } else { "narrow" }
            )
        })
        .flex()
        .items_center()
        .gap(px(PANE_CONTROL_GAP))
        .ml(px(PANE_CONTROL_LEADING_GAP))
        .flex_shrink_0()
        .when(!focused, |controls| {
            controls
                .opacity(0.0)
                .group_hover(pane_group.to_owned(), |controls| controls.opacity(1.0))
        });
    let actions = [
        (
            PaneCaptionAction::SplitRight,
            "split-right",
            "Split Right",
            IconName::Columns2,
            presentation.shortcut(&SplitRight),
            layout.show_splits,
        ),
        (
            PaneCaptionAction::SplitDown,
            "split-down",
            "Split Down",
            IconName::Rows2,
            presentation.shortcut(&SplitDown),
            layout.show_splits,
        ),
        (
            PaneCaptionAction::ToggleZoom,
            "toggle-zoom",
            if zoomed { "Restore Panes" } else { "Zoom Pane" },
            if zoomed {
                IconName::Minimize2
            } else {
                IconName::Maximize2
            },
            presentation.shortcut(&TogglePaneZoom),
            has_multiple_panes,
        ),
        (
            PaneCaptionAction::Close,
            "close",
            "Close Pane",
            IconName::X,
            presentation.shortcut(&ClosePane),
            has_multiple_panes,
        ),
    ];
    for (action, selector, name, icon, shortcut, visible) in actions {
        if !visible {
            continue;
        }
        let host = host.clone();
        let id = format!("pane-{selector}-{}", pane_id.get());
        let icon_size = action.icon_size();
        controls = controls.child(
            IconButton::new(
                gpui::SharedString::from(id.clone()),
                name,
                move |foreground| Icon::new(icon, px(icon_size), foreground).into_any_element(),
            )
            // The caption paints no surface, so its controls must not paint one either.
            .variant(ButtonVariant::Bare)
            .size(ButtonSize::Compact)
            .preserve_ancestor_hover()
            .debug_selector(id.clone())
            .tooltip(
                Tooltip::new(gpui::SharedString::from(format!("{id}-tooltip")), name)
                    .keyboard_equivalent(shortcut),
            )
            .on_activate(move |_, window, cx| {
                let _ = host.update(cx, |host, cx| {
                    host.perform_caption_action(action, pane_id, window, cx);
                });
            }),
        );
    }
    div()
        .id(("pane-caption", pane_id.get()))
        .debug_selector(move || {
            format!(
                "pane-caption-{}-{}",
                pane_id.get(),
                if focused { "focused" } else { "unfocused" }
            )
        })
        .h(px(PANE_CAPTION_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .pl(px(PANE_CAPTION_LEFT_PADDING))
        .pr(px(PANE_CAPTION_RIGHT_PADDING))
        .pt(px(PANE_CAPTION_TOP_PADDING))
        // The caption paints no surface of its own: it reads as identity floating over the Pane.
        .text_size(px(PANE_CAPTION_TEXT_SIZE))
        .text_color(gpui_color(color))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            let _ = focus_host.update(cx, |host, cx| {
                host.focus_pane(pane_id, cx);
                host.focus(window, cx);
            });
            cx.stop_propagation();
        })
        .when(attention, |caption| {
            caption.child(
                div()
                    .debug_selector(move || format!("pane-attention-{}", pane_id.get()))
                    .mr(px(7.0))
                    .size(px(6.0))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(gpui_color(ACTIVE_THEME.warning)),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .overflow_hidden()
                .when(layout.show_origin && !text.origin.is_empty(), |row| {
                    row.child(render_pane_origin(pane_id, &text.origin, layout, color))
                })
                .when(layout.show_directory && !text.directory.is_empty(), |row| {
                    row.child(
                        div()
                            .debug_selector(move || {
                                format!("pane-caption-directory-{}", pane_id.get())
                            })
                            .flex_shrink_0()
                            .text_color(gpui_color(color))
                            .child(text.directory),
                    )
                })
                .child(
                    div()
                        .debug_selector(move || format!("pane-caption-name-{}", pane_id.get()))
                        .min_w_0()
                        .truncate()
                        .child(text.name),
                )
                .when(layout.show_label && !text.label.is_empty(), |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .mx(px(5.0))
                            .text_color(gpui_color(color))
                            .child("·"),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("pane-caption-label-{}", pane_id.get()))
                            .min_w_0()
                            .truncate()
                            .text_color(gpui_color(color))
                            .child(text.label),
                    )
                }),
        )
        .child(controls)
        .into_any_element()
}

/// The single colour one caption paints every part of itself with.
///
/// The caption is one statement of identity, so its icon, account, machine, directory, label and
/// controls all read at the same weight. Only focus changes the tier.
fn caption_color(focused: bool) -> Color {
    if focused {
        ACTIVE_THEME.text_muted
    } else {
        ACTIVE_THEME.text_placeholder
    }
}

/// Renders the account and machine a Pane runs on, ahead of the directory it sits in.
///
/// The icon states Local or Remote from the Terminal's own classification, so a Remote Pane stays
/// distinguishable by shape at the width where its account and machine text no longer fit.
fn render_pane_origin(
    pane_id: PaneId,
    origin: &PaneOrigin,
    layout: CaptionLayout,
    color: Color,
) -> AnyElement {
    let (icon, location) = if origin.remote {
        (IconName::Globe, "remote")
    } else {
        (IconName::Terminal, "local")
    };
    let icon_tint = gpui_color(color);
    div()
        .debug_selector(move || format!("pane-caption-origin-{}-{location}", pane_id.get()))
        .flex()
        .items_center()
        .flex_shrink_0()
        .child(
            div()
                .mr(px(PANE_ORIGIN_ICON_GAP))
                .flex()
                .items_center()
                .child(Icon::new(icon, px(PANE_ORIGIN_ICON_SIZE), icon_tint)),
        )
        .when(layout.show_user && !origin.user.is_empty(), |row| {
            row.child(
                div()
                    .debug_selector(move || format!("pane-caption-account-{}", pane_id.get()))
                    .text_color(gpui_color(color))
                    .child(origin_account(&origin.user)),
            )
        })
        .when(layout.show_host && !origin.host.is_empty(), |row| {
            row.child(
                div()
                    .debug_selector(move || format!("pane-caption-host-{}", pane_id.get()))
                    .text_color(gpui_color(color))
                    .child(origin.host.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .mx(px(5.0))
                    .text_color(gpui_color(color))
                    .child("›"),
            )
        })
        .into_any_element()
}

fn split_child(child: AnyElement, axis: SplitAxis, ratio: f32) -> impl IntoElement {
    let child = div()
        .flex_basis(DefiniteLength::Fraction(ratio))
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .child(child);

    match axis {
        SplitAxis::Horizontal => child.h_full(),
        SplitAxis::Vertical => child.w_full(),
    }
}

fn render_divider(
    split_id: SplitId,
    axis: SplitAxis,
    current_offset: f32,
    host: gpui::WeakEntity<PaneHost>,
) -> AnyElement {
    ResizeHandle::new(
        ("split-divider", split_id.get()),
        "Resize Pane split",
        match axis {
            SplitAxis::Horizontal => ResizeAxis::Horizontal,
            SplitAxis::Vertical => ResizeAxis::Vertical,
        },
        current_offset,
    )
    .tab_stop(true)
    .reset_on_double_click(true)
    .debug_selector(format!("split-divider-{}", split_id.get()))
    .on_event(move |event, window, cx| {
        let event = *event;
        let _ = host.update(cx, |host, cx| {
            host.handle_resize_event(split_id, axis, event, window, cx);
        });
    })
    .into_any_element()
}

// Splitting restores the grid, so a zoomed Pane must use its allocation in that grid.
fn restored_leaf_size(
    tree: PaneTreeRef<'_>,
    target: PaneId,
    available: PaneSize,
) -> Option<PaneSize> {
    match tree.node() {
        PaneNodeRef::Leaf { pane_id } => (pane_id == target).then_some(available),
        PaneNodeRef::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let child_size = |fraction| {
                match axis {
                    SplitAxis::Horizontal => PaneSize::new(
                        (available.width() - DIVIDER_SIZE) * fraction,
                        available.height(),
                    ),
                    SplitAxis::Vertical => PaneSize::new(
                        available.width(),
                        (available.height() - DIVIDER_SIZE) * fraction,
                    ),
                }
                .ok()
            };
            restored_leaf_size(first, target, child_size(ratio)?)
                .or_else(|| restored_leaf_size(second, target, child_size(1.0 - ratio)?))
        }
    }
}

fn pane_size(bounds: Bounds<Pixels>) -> Result<PaneSize, crate::domain::PaneSizeError> {
    PaneSize::new(f32::from(bounds.size.width), f32::from(bounds.size.height))
}

fn split_content_extent(axis: SplitAxis, bounds: Bounds<Pixels>) -> Option<f32> {
    let extent = match axis {
        SplitAxis::Horizontal => f32::from(bounds.size.width),
        SplitAxis::Vertical => f32::from(bounds.size.height),
    } - DIVIDER_SIZE;
    (extent > 0.0).then_some(extent)
}

fn split_ratio_for_offset(
    axis: SplitAxis,
    bounds: Bounds<Pixels>,
    requested_offset: f32,
) -> Option<f32> {
    let content_extent = split_content_extent(axis, bounds)?;
    requested_offset
        .is_finite()
        .then_some(requested_offset / content_extent)
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use gpui::{
        Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
        VisualTestContext, bounds, point, px, size,
    };

    use super::*;
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{
        RemoteChannelRevalidationError, RemoteChannelUnavailable, RemoteTerminalChannelProvider,
        ScreenSnapshot, ScrollbarSnapshot, SessionEvent, SessionExit, TerminalSessionFactory,
    };
    use crate::ui::RemoteChildLaunchUnavailable;

    struct RemoteLaunchEventHarness {
        host: Entity<PaneHost>,
        events: Rc<RefCell<Vec<RemoteChildLaunchUnavailable>>>,
    }

    impl Render for RemoteLaunchEventHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.host.clone()
        }
    }

    struct RevalidatingRemoteChannelProvider {
        grant: AtomicBool,
        revalidations: AtomicUsize,
        preparations: AtomicUsize,
        revalidation_error: Mutex<Option<RemoteChannelRevalidationError>>,
        command_context: SshCommandContext,
    }

    impl RevalidatingRemoteChannelProvider {
        fn new(destination: crate::domain::SshDestination) -> Self {
            Self {
                grant: AtomicBool::new(true),
                revalidations: AtomicUsize::new(0),
                preparations: AtomicUsize::new(0),
                revalidation_error: Mutex::new(None),
                command_context: SshCommandContext::new(
                    crate::ssh::command::OpenSshExecutable::for_test(),
                    PathBuf::from("/private/config/spaceterm/ssh_config"),
                    destination,
                    PathBuf::from("/private/runtime/spaceterm/master.sock"),
                )
                .unwrap(),
            }
        }

        fn fail_revalidation_with(&self, error: Option<RemoteChannelRevalidationError>) {
            *self.revalidation_error.lock().unwrap() = error;
        }
    }

    impl RemoteTerminalChannelProvider for RevalidatingRemoteChannelProvider {
        fn is_ready(&self) -> bool {
            true
        }

        fn revalidate(
            &self,
            _directory: crate::domain::RemoteDirectory,
            _expected_identity: Option<crate::domain::RemoteDirectoryIdentity>,
        ) -> gpui::Task<Result<(), RemoteChannelRevalidationError>> {
            self.revalidations.fetch_add(1, Ordering::AcqRel);
            self.grant.store(false, Ordering::Release);
            let error = *self.revalidation_error.lock().unwrap();
            if error.is_none() {
                self.grant.store(true, Ordering::Release);
            }
            gpui::Task::ready(error.map_or(Ok(()), Err))
        }

        fn prepare(
            &self,
            _directory: &crate::domain::RemoteDirectory,
        ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable>
        {
            if !self.grant.swap(false, Ordering::AcqRel) {
                return Err(RemoteChannelUnavailable);
            }
            self.preparations.fetch_add(1, Ordering::AcqRel);
            Ok(self.command_context.prepare_pane_channel(
                ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
            ))
        }
    }

    fn test_session_factory() -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::new_local(
            Rc::new(
                TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                    .with_selection_copy_response(Ok(None)),
            ),
            crate::terminal::testing::test_local_directory(test_home_directory()),
        )
    }

    fn remote_test_session_factory(
        records: TestTerminalSessionRecords,
    ) -> WorkspaceTerminalSessionFactory {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let command_context = Arc::new(
            SshCommandContext::new(
                crate::ssh::command::OpenSshExecutable::for_test(),
                PathBuf::from("/private/config/spaceterm/ssh_config"),
                destination.clone(),
                PathBuf::from("/private/runtime/spaceterm/master.sock"),
            )
            .unwrap(),
        );
        remote_test_session_factory_with_provider(
            records,
            destination,
            Arc::new(move || {
                Ok(command_context.prepare_pane_channel(
                    ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
                ))
            }),
        )
    }

    fn remote_test_session_factory_with_provider(
        records: TestTerminalSessionRecords,
        destination: crate::domain::SshDestination,
        provider: Arc<dyn RemoteTerminalChannelProvider>,
    ) -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::new_remote(
            Rc::new(TestTerminalSessionFactory::new(records)),
            crate::domain::ValidatedLocalDirectory::new(
                PathBuf::from("/missing/local/home-is-not-a-workspace"),
                crate::domain::LocalDirectoryIdentity::for_test(71073),
            ),
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                destination,
                crate::domain::RemoteDirectory::new("~/project".to_owned()).unwrap(),
            ),
            crate::domain::RemoteDirectoryIdentity::new("/home/tester/project".to_owned()).unwrap(),
            "project on remote".to_owned(),
            provider,
        )
    }

    fn split_test_pane(
        host: &Entity<PaneHost>,
        pane_id: PaneId,
        axis: SplitAxis,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_pane(pane_id, axis, window, cx);
            });
        });
        cx.run_until_parked();
    }

    fn remote_pane_host_with_events(
        session_factory: WorkspaceTerminalSessionFactory,
        cx: &mut TestAppContext,
    ) -> (
        Entity<PaneHost>,
        Rc<RefCell<Vec<RemoteChildLaunchUnavailable>>>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let host = cx.new(|cx| PaneHost::new(TabId::new(1), session_factory, window, cx));
            cx.subscribe(
                &host,
                move |_, _, event: &RemoteChildLaunchUnavailable, _| {
                    recorded_events.borrow_mut().push(*event);
                },
            )
            .detach();
            RemoteLaunchEventHarness { host, events }
        });
        let (host, events) = harness.read_with(cx, |harness, _| {
            (harness.host.clone(), Rc::clone(&harness.events))
        });
        cx.run_until_parked();
        (host, events, cx)
    }

    #[gpui::test]
    fn remote_split_should_emit_each_typed_revalidation_failure_without_mutation(
        cx: &mut TestAppContext,
    ) {
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(RevalidatingRemoteChannelProvider::new(destination.clone()));
        let session_factory = remote_test_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::clone(&provider) as Arc<dyn RemoteTerminalChannelProvider>,
        );
        let (host, events, cx) = remote_pane_host_with_events(session_factory, cx);
        let before = host.read_with(cx, |host, _| {
            (
                host.pane_count(),
                host.focused_pane_id(),
                host.layout_signature(),
            )
        });

        for error in [
            RemoteChannelRevalidationError::ConnectionUnavailable,
            RemoteChannelRevalidationError::DirectoryUnavailable,
            RemoteChannelRevalidationError::IdentityChanged,
        ] {
            provider.fail_revalidation_with(Some(error));
            split_test_pane(&host, before.1, SplitAxis::Horizontal, cx);
        }

        assert_eq!(
            events.borrow().as_slice(),
            [
                RemoteChildLaunchUnavailable::ConnectionUnavailable,
                RemoteChildLaunchUnavailable::DirectoryUnavailable,
                RemoteChildLaunchUnavailable::IdentityChanged,
            ]
        );
        assert_eq!(
            host.read_with(cx, |host, _| {
                (
                    host.pane_count(),
                    host.focused_pane_id(),
                    host.layout_signature(),
                )
            }),
            before
        );
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn superseded_and_cancelled_remote_splits_should_emit_once_without_mutation(
        cx: &mut TestAppContext,
    ) {
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(RevalidatingRemoteChannelProvider::new(destination.clone()));
        let session_factory = remote_test_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::clone(&provider) as Arc<dyn RemoteTerminalChannelProvider>,
        );
        let (host, events, cx) = remote_pane_host_with_events(session_factory, cx);
        let before = host.read_with(cx, |host, _| {
            (
                host.pane_count(),
                host.focused_pane_id(),
                host.layout_signature(),
            )
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_pane(before.1, SplitAxis::Horizontal, window, cx);
                host.remote_lifecycle.begin_child_launch();
            });
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_pane(before.1, SplitAxis::Horizontal, window, cx);
                host.disconnect_remote(1, cx).unwrap();
            });
        });
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            [
                RemoteChildLaunchUnavailable::Stale,
                RemoteChildLaunchUnavailable::Cancelled,
            ]
        );
        assert_eq!(
            host.read_with(cx, |host, _| {
                (
                    host.pane_count(),
                    host.focused_pane_id(),
                    host.layout_signature(),
                )
            }),
            before
        );
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn remote_split_skips_local_workspace_validation_and_preserves_launch_context(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory = remote_test_session_factory(records.clone());
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.run_until_parked();

        split_test_pane(&host, PaneId::new(1), SplitAxis::Horizontal, cx);

        assert_eq!(host.read_with(cx, |host, _| host.pane_count()), 2);
        assert_eq!(records.starts().len(), 2);
        assert!(records.starts().iter().all(|start| {
            start.remote_launch_plan().is_some_and(|plan| {
                plan.destination().as_str() == "tester@remote"
                    && plan.remote_directory().as_str() == "~/project"
            })
        }));
    }

    #[gpui::test]
    fn remote_split_should_revalidate_before_mutating_the_pane_tree(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(RevalidatingRemoteChannelProvider::new(destination.clone()));
        let session_factory = remote_test_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::clone(&provider) as Arc<dyn RemoteTerminalChannelProvider>,
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.run_until_parked();
        let focused = host.read_with(cx, |host, _| host.focused_pane_id());

        provider.fail_revalidation_with(Some(RemoteChannelRevalidationError::IdentityChanged));
        split_test_pane(&host, focused, SplitAxis::Horizontal, cx);

        assert_eq!(host.read_with(cx, |host, _| host.pane_count()), 1);
        assert_eq!(
            host.read_with(cx, |host, _| host.focused_pane_id()),
            focused
        );
        assert_eq!(records.starts().len(), 1);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 1);
        assert_eq!(provider.revalidations.load(Ordering::Acquire), 1);

        provider.fail_revalidation_with(None);
        split_test_pane(&host, focused, SplitAxis::Horizontal, cx);

        assert_eq!(host.read_with(cx, |host, _| host.pane_count()), 2);
        assert_eq!(records.starts().len(), 2);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 2);
        assert_eq!(provider.revalidations.load(Ordering::Acquire), 2);
    }

    #[gpui::test]
    fn remote_split_should_leave_hierarchy_unchanged_when_channel_reservation_races_master_death(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let command_context = Arc::new(
            SshCommandContext::new(
                crate::ssh::command::OpenSshExecutable::for_test(),
                PathBuf::from("/private/config/spaceterm/ssh_config"),
                destination.clone(),
                PathBuf::from("/private/runtime/spaceterm/master.sock"),
            )
            .unwrap(),
        );
        let preparations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = {
            let preparations = Arc::clone(&preparations);
            Arc::new(move || {
                if preparations.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                    Ok(command_context.prepare_pane_channel(
                        ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
                    ))
                } else {
                    Err(RemoteChannelUnavailable)
                }
            })
        };
        let session_factory =
            remote_test_session_factory_with_provider(records.clone(), destination, provider);
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.run_until_parked();

        split_test_pane(&host, PaneId::new(1), SplitAxis::Horizontal, cx);

        assert_eq!(
            host.read_with(cx, |host, _| (host.pane_count(), host.focused_pane_id())),
            (1, PaneId::new(1))
        );
        assert_eq!(records.starts().len(), 1);
        assert_eq!(preparations.load(std::sync::atomic::Ordering::Acquire), 2);
    }

    fn four_pane_host(cx: &mut TestAppContext) -> (Entity<PaneHost>, &mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        split_test_pane(&host, PaneId::new(1), SplitAxis::Horizontal, cx);
        split_test_pane(&host, PaneId::new(1), SplitAxis::Vertical, cx);
        split_test_pane(&host, PaneId::new(2), SplitAxis::Vertical, cx);
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.focus_pane(PaneId::new(1), cx);
                host.focus(window, cx);
            });
        });
        cx.run_until_parked();

        (host, cx)
    }

    #[gpui::test]
    fn attention_remains_scoped_to_its_owning_pane_and_tab_title(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), test_session_factory(), window, cx)
        });

        host.update(cx, |host, _| {
            host.pane_attention.insert(PaneId::new(1), 2);
        });

        assert_eq!(host.read_with(cx, |host, _| host.tab_title()), "• Terminal");
        assert_eq!(
            host.read_with(cx, |host, _| host.pane_attention.clone()),
            BTreeMap::from([(PaneId::new(1), 2)])
        );
    }

    fn test_home_directory() -> PathBuf {
        PathBuf::from("/tmp/spaceterm-test-workspace")
    }

    fn focused_panes_after_shortcuts<const N: usize>(
        host: &Entity<PaneHost>,
        cx: &mut VisualTestContext,
        shortcuts: [&str; N],
    ) -> [PaneId; N] {
        shortcuts.map(|shortcut| {
            cx.simulate_keystrokes(shortcut);
            host.read_with(cx, |host, _| host.terminal_tab.focused_pane_id())
        })
    }

    struct CaptionTestView {
        host: Entity<PaneHost>,
        width: Pixels,
    }

    impl Render for CaptionTestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(self.width).h(px(600.0)).child(self.host.clone())
        }
    }

    fn caption_host(
        cx: &mut TestAppContext,
    ) -> (
        Entity<CaptionTestView>,
        Entity<PaneHost>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        let records = TestTerminalSessionRecords::default();
        let factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let factory = WorkspaceTerminalSessionFactory::new_local(
            factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx.add_window_view(|window, cx| CaptionTestView {
            host: cx.new(|cx| PaneHost::new(TabId::new(1), factory, window, cx)),
            width: px(1000.0),
        });
        let host = view.read_with(cx, |view, _| view.host.clone());
        cx.update(|window, cx| {
            window.activate_window();
            host.update(cx, |host, cx| host.activate(window, cx));
        });
        cx.run_until_parked();
        (view, host, records, cx)
    }

    fn click_caption_control(selector: &'static str, cx: &mut VisualTestContext) {
        let control = cx
            .debug_bounds(selector)
            .expect("caption control must exist")
            .center();
        cx.simulate_mouse_move(control, None, Modifiers::none());
        cx.simulate_click(control, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn caption_split_buttons_should_split_their_owner_and_restore_terminal_input(
        cx: &mut TestAppContext,
    ) {
        let (_, host, records, cx) = caption_host(cx);
        assert!(cx.debug_bounds("pane-toggle-zoom-1").is_none());
        assert!(cx.debug_bounds("pane-close-1").is_none());
        click_caption_control("pane-split-right-1", cx);
        click_caption_control("pane-split-down-1", cx);
        assert_eq!(
            host.read_with(cx, |host, _| (host.pane_count(), host.focused_pane_id())),
            (3, PaneId::new(3))
        );
        assert_eq!(records.pointer_count(), 0);
        assert!(cx.update(|window, cx| host.read(cx).focused_terminal_has_input_focus(window, cx)));
        cx.simulate_keystrokes("a");
        assert!(
            records.commands().iter().any(|call| call.session_id == 3
                && matches!(&call.command, RecordedSessionCommand::Key(_)))
        );
    }

    #[gpui::test]
    fn caption_zoom_should_target_hovered_pane_and_split_should_restore_the_layout(
        cx: &mut TestAppContext,
    ) {
        let (_, host, records, cx) = caption_host(cx);
        click_caption_control("pane-split-right-1", cx);
        click_caption_control("pane-toggle-zoom-1", cx);
        assert_eq!(
            host.read_with(cx, |host, _| host.zoom_state()),
            ZoomState::Zoomed(PaneId::new(1))
        );
        assert!(cx.debug_bounds("pane-caption-2-unfocused").is_none());
        click_caption_control("pane-split-down-1", cx);
        assert_eq!(
            host.read_with(cx, |host, _| (host.zoom_state(), host.pane_count())),
            (ZoomState::Restored, 3)
        );
        assert!(cx.debug_bounds("pane-caption-2-unfocused").is_some());
        assert_eq!(records.pointer_count(), 0);
    }

    #[gpui::test]
    fn zoom_then_split_before_repaint_should_use_the_retained_layout_size(cx: &mut TestAppContext) {
        let (view, host, _, cx) = caption_host(cx);
        click_caption_control("pane-split-right-1", cx);
        view.update(cx, |view, cx| {
            view.width = px(400.0);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.toggle_zoom(window, cx);
                host.split_focused(SplitAxis::Horizontal, window, cx);
            })
        });
        cx.run_until_parked();
        assert_eq!(
            host.read_with(cx, |host, _| (host.pane_count(), host.zoom_state())),
            (3, ZoomState::Restored)
        );
    }

    #[gpui::test]
    fn zoomed_split_should_respect_the_restored_pane_allocation(cx: &mut TestAppContext) {
        let (view, host, records, cx) = caption_host(cx);
        click_caption_control("pane-split-right-1", cx);
        view.update(cx, |view, cx| {
            view.width = px(200.0);
            cx.notify();
        });
        cx.run_until_parked();
        click_caption_control("pane-toggle-zoom-1", cx);
        click_caption_control("pane-split-right-1", cx);
        assert_eq!(
            host.read_with(cx, |host, _| (host.pane_count(), host.zoom_state())),
            (2, ZoomState::Zoomed(PaneId::new(1)))
        );
        assert_eq!(records.starts().len(), 2);
    }

    #[gpui::test]
    fn minimum_width_single_pane_should_keep_both_split_controls(cx: &mut TestAppContext) {
        let (view, host, _, cx) = caption_host(cx);
        view.update(cx, |view, cx| {
            view.width = px(MINIMUM_PANE_WIDTH);
            cx.notify();
        });
        host.update(cx, |host, cx| {
            host.pane_attention.insert(PaneId::new(1), 1);
            cx.notify();
        });
        cx.run_until_parked();
        let caption = cx.debug_bounds("pane-caption-1-focused").unwrap();
        for selector in ["pane-split-right-1", "pane-split-down-1"] {
            let button = cx.debug_bounds(selector).unwrap();
            assert!(caption.contains(&button.origin) && caption.contains(&button.bottom_right()));
        }
        assert!(cx.debug_bounds("pane-toggle-zoom-1").is_none());
        assert!(cx.debug_bounds("pane-close-1").is_none());
    }

    #[gpui::test]
    fn minimum_width_attention_captions_should_keep_zoom_and_close_inside_their_pane(
        cx: &mut TestAppContext,
    ) {
        let (view, host, _, cx) = caption_host(cx);
        click_caption_control("pane-split-down-1", cx);
        view.update(cx, |view, cx| {
            view.width = px(MINIMUM_PANE_WIDTH);
            cx.notify();
        });
        host.update(cx, |host, cx| {
            host.pane_attention.insert(PaneId::new(2), 1);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pane-controls-2-narrow").is_some());
        let caption = cx.debug_bounds("pane-caption-2-focused").unwrap();
        for selector in ["pane-toggle-zoom-2", "pane-close-2"] {
            let button = cx.debug_bounds(selector).unwrap();
            assert_eq!(
                button.size,
                size(px(PANE_CONTROL_SIZE), px(PANE_CONTROL_SIZE))
            );
            assert!(caption.contains(&button.origin) && caption.contains(&button.bottom_right()));
        }
        click_caption_control("pane-toggle-zoom-2", cx);
        assert_eq!(
            host.read_with(cx, |host, _| host.zoom_state()),
            ZoomState::Zoomed(PaneId::new(2))
        );
        view.update(cx, |view, cx| {
            view.width = px(1000.0);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pane-split-right-2").is_some());
    }

    #[gpui::test]
    fn caption_directory_and_running_command_should_update_without_a_title_change(
        cx: &mut TestAppContext,
    ) {
        use crate::terminal::metadata::{CommandMetadata, CommandState, TitleProvenance};
        let (_, host, records, cx) = caption_host(cx);
        let mut screen =
            ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "zsh", 1);
        let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
        metadata.title.provenance = TitleProvenance::Fallback;
        metadata.directory.path = Arc::from("/srv/new-place");
        metadata.command = Some(CommandMetadata {
            line: Arc::from("cargo build --release"),
            state: CommandState::Running,
        });
        records
            .event_sender(1)
            .unwrap()
            .try_send(SessionEvent::Screen(screen.clone()))
            .unwrap();
        cx.run_until_parked();
        let caption = host.read_with(cx, |host, _| {
            let caption = host.pane_captions.get(&PaneId::new(1)).unwrap();
            (
                caption.directory.clone(),
                caption.name.clone(),
                caption.label.clone(),
            )
        });
        assert_eq!(
            caption,
            (
                "/srv/".into(),
                "new-place".into(),
                "cargo build --release".into()
            )
        );
        let snapshot = Arc::make_mut(&mut screen);
        snapshot.generation = crate::terminal::PresentationGeneration::test(2);
        let metadata = Arc::make_mut(&mut snapshot.metadata);
        metadata.command.as_mut().unwrap().state = CommandState::Finished {
            exit_status: Some(0),
            duration: std::time::Duration::ZERO,
        };
        records
            .event_sender(1)
            .unwrap()
            .try_send(SessionEvent::Screen(screen))
            .unwrap();
        cx.run_until_parked();
        let caption = host.read_with(cx, |host, _| {
            let caption = host.pane_captions.get(&PaneId::new(1)).unwrap();
            (
                caption.directory.clone(),
                caption.name.clone(),
                caption.label.clone(),
            )
        });
        assert_eq!(caption, ("/srv/".into(), "new-place".into(), "zsh".into()));
    }

    #[gpui::test]
    fn captions_should_present_their_account_machine_and_directory_by_location(
        cx: &mut TestAppContext,
    ) {
        for (context, location, host) in [
            (
                crate::terminal::metadata::TerminalMetadataContext::local(
                    crate::local_path::LocalPathSemantics::Posix,
                    "/Users/tester",
                    crate::terminal::metadata::LocalMachine::new(
                        Some("tester"),
                        Some("Testers-Mac.local"),
                        Some("/Users/tester"),
                    ),
                ),
                "local",
                "Testers-Mac",
            ),
            (
                crate::terminal::metadata::TerminalMetadataContext::Remote(
                    crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                        crate::domain::SshDestination::new("tester@build.example".to_owned())
                            .unwrap(),
                        crate::domain::RemoteDirectory::new("~/app".to_owned()).unwrap(),
                    )
                    .with_machine(
                        crate::terminal::metadata::RemoteMachine::new(
                            Some("tester"),
                            Some("/Users/tester"),
                        ),
                    ),
                ),
                "remote",
                "build.example",
            ),
            // A host alias names no account, so the discovered account is what names the user.
            (
                crate::terminal::metadata::TerminalMetadataContext::Remote(
                    crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                        crate::domain::SshDestination::new("build-box".to_owned()).unwrap(),
                        crate::domain::RemoteDirectory::new("~/app".to_owned()).unwrap(),
                    )
                    .with_machine(
                        crate::terminal::metadata::RemoteMachine::new(
                            Some("tester"),
                            Some("/Users/tester"),
                        ),
                    ),
                ),
                "remote",
                "build-box",
            ),
        ] {
            let (_, host_entity, records, cx) = caption_host(cx);
            let mut screen =
                ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "zsh", 1);
            let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
            metadata.context = context;
            metadata.directory.path = Arc::from("/Users/tester/Projects/app");
            records
                .event_sender(1)
                .unwrap()
                .try_send(SessionEvent::Screen(screen))
                .unwrap();
            cx.run_until_parked();

            let caption = host_entity.read_with(cx, |host, _| {
                let caption = host.pane_captions.get(&PaneId::new(1)).unwrap();
                (
                    caption.origin.user.clone(),
                    caption.origin.host.clone(),
                    caption.directory.clone(),
                    caption.name.clone(),
                )
            });
            assert_eq!(caption.0.as_ref(), "tester", "{location}");
            assert_eq!(caption.1.as_ref(), host, "{location}");
            assert_eq!(caption.3.as_ref(), "app", "{location}");
            // Each side abbreviates against its own home, so Local and Remote read identically.
            assert_eq!(caption.2.as_ref(), "~/Projects/", "{location}");
            let origin_selector = if location == "local" {
                "pane-caption-origin-1-local"
            } else {
                "pane-caption-origin-1-remote"
            };
            for selector in [
                origin_selector,
                "pane-caption-account-1",
                "pane-caption-host-1",
            ] {
                assert!(
                    cx.debug_bounds(selector).is_some(),
                    "{location} caption must render {selector}"
                );
            }
        }
    }

    #[gpui::test]
    fn caption_top_padding_should_sit_inside_the_declared_caption_height(cx: &mut TestAppContext) {
        let (_, _, _, cx) = caption_host(cx);

        let caption = cx.debug_bounds("pane-caption-1-focused").unwrap();

        // The header holds its own top space, so reserving it never pushes the terminal down.
        assert_eq!(caption.size.height, px(PANE_CAPTION_HEIGHT));
        let controls = cx.debug_bounds("pane-split-right-1").unwrap();
        assert!(
            controls.origin.y >= caption.origin.y + px(PANE_CAPTION_TOP_PADDING),
            "caption content must start below its top padding"
        );
    }

    #[gpui::test]
    fn single_pane_should_render_a_caption(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (_host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        cx.run_until_parked();
        assert!(cx.debug_bounds("pane-caption-1-focused").is_some());
    }

    #[gpui::test]
    fn initial_and_unknown_source_panes_should_start_in_home_directory(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let home_directory = PathBuf::from("/tmp/spaceterm-home-directory");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(home_directory.clone()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(
            records
                .starts()
                .into_iter()
                .map(|start| {
                    start
                        .local_working_directory()
                        .expect("Pane starts must remain local")
                        .path()
                        .to_path_buf()
                })
                .collect::<Vec<_>>(),
            vec![home_directory.clone(), home_directory]
        );
    }

    #[test]
    fn directory_captions_should_separate_their_leaf_from_the_leading_path() {
        for (directory, expected) in [
            (
                "/Users/tester/Projects/micro",
                ("/Users/tester/Projects/", "micro"),
            ),
            (
                "/Users/tester/Projects/micro/",
                ("/Users/tester/Projects/", "micro"),
            ),
            ("/srv", ("/", "srv")),
            ("/", ("", "/")),
            ("~", ("", "~")),
            ("", ("", "")),
        ] {
            let (leading, name) = split_directory_leaf(directory);
            assert_eq!((leading.as_ref(), name.as_ref()), expected, "{directory}");
        }
    }

    #[test]
    fn narrowing_a_caption_should_give_up_its_segments_in_identity_order() {
        let metrics = CaptionMetrics {
            origin_icon: px(18.0),
            user: px(40.0),
            host: px(60.0),
            directory: px(120.0),
            name: px(40.0),
            label: px(30.0),
        };
        let resolve = |width: f32| CaptionLayout::from_metrics(false, false, px(width), metrics);
        let controls = PANE_CAPTION_LEFT_PADDING
            + PANE_CAPTION_RIGHT_PADDING
            + PANE_CONTROL_LEADING_GAP
            + controls_width(2);
        let layout = |origin, host, directory, user, label| CaptionLayout {
            show_origin: origin,
            show_host: host,
            show_directory: directory,
            show_user: user,
            show_label: label,
            show_splits: true,
        };

        assert_eq!(
            resolve(controls + 310.0),
            layout(true, true, true, true, true)
        );
        assert_eq!(
            resolve(controls + 280.0),
            layout(true, true, true, true, false)
        );
        assert_eq!(
            resolve(controls + 240.0),
            layout(true, true, true, false, false)
        );
        assert_eq!(
            resolve(controls + 120.0),
            layout(true, true, false, false, false)
        );
        assert_eq!(
            resolve(controls + 60.0),
            layout(true, false, false, false, false)
        );
        assert_eq!(
            resolve(controls + 50.0),
            layout(false, false, false, false, false)
        );
        assert!(!resolve(controls - 1.0).show_splits);
    }

    #[gpui::test]
    fn every_split_pane_caption_should_render_its_own_name_segment(cx: &mut TestAppContext) {
        let (_, host, _, cx) = caption_host(cx);
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pane-caption-name-1").is_some());
        assert!(cx.debug_bounds("pane-caption-name-2").is_some());
    }

    #[gpui::test]
    fn split_panes_should_render_compact_focused_and_unfocused_captions(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let unfocused_height = cx
            .debug_bounds("pane-caption-1-unfocused")
            .map(|bounds| bounds.size.height);
        let focused_height = cx
            .debug_bounds("pane-caption-2-focused")
            .map(|bounds| bounds.size.height);

        assert_eq!(
            (unfocused_height, focused_height),
            (Some(px(PANE_CAPTION_HEIGHT)), Some(px(PANE_CAPTION_HEIGHT)))
        );
    }

    #[gpui::test]
    fn focusing_another_pane_should_move_the_focused_caption_state(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.focus_pane(PaneId::new(1), cx);
            });
        });
        cx.run_until_parked();

        let caption_state = (
            cx.debug_bounds("pane-caption-1-focused").is_some(),
            cx.debug_bounds("pane-caption-2-unfocused").is_some(),
        );
        assert_eq!(caption_state, (true, true));
    }

    #[gpui::test]
    fn native_service_return_is_rejected_after_pane_changes_away_and_back(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            host.update(cx, |host, cx| {
                host.focus(window, cx);
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.focus_pane(PaneId::new(1), cx);
                host.focus(window, cx);
            });
        });
        cx.run_until_parked();

        let origin = cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.native_service_status(WorkspaceId::new(1), window, cx)
                    .origin
                    .expect("the focused terminal must expose a Service origin")
            })
        });
        let accepted = cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.focus_pane(PaneId::new(2), cx);
                host.focus_pane(PaneId::new(1), cx);
                host.focus(window, cx);
                host.native_service_target(origin).is_some_and(|terminal| {
                    terminal.update(cx, |terminal, cx| {
                        terminal.insert_native_service_text(
                            origin,
                            "stale return".to_owned(),
                            window,
                            cx,
                        )
                    })
                })
            })
        });

        assert!(!accepted);
        assert!(!records.commands().iter().any(|call| {
            matches!(
                call.command,
                crate::terminal::testing::RecordedSessionCommand::RequestPaste(_)
            )
        }));
    }

    #[gpui::test]
    fn closing_a_nonfocused_pane_invalidates_the_service_hierarchy(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            host.update(cx, |host, cx| {
                host.focus(window, cx);
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.focus_pane(PaneId::new(1), cx);
                host.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let origin = cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.native_service_status(WorkspaceId::new(1), window, cx)
                    .origin
                    .unwrap()
            })
        });

        let accepted = cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.close_pane(PaneId::new(2), window, cx);
                host.native_service_target(origin).is_some_and(|terminal| {
                    terminal.update(cx, |terminal, cx| {
                        terminal.insert_native_service_text(
                            origin,
                            "stale return".to_owned(),
                            window,
                            cx,
                        )
                    })
                })
            })
        });

        assert!(!accepted);
        assert!(!records.commands().iter().any(|call| {
            matches!(
                call.command,
                crate::terminal::testing::RecordedSessionCommand::RequestPaste(_)
            )
        }));
    }

    #[gpui::test]
    fn file_drop_should_focus_the_target_pane_before_requesting_its_paste(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            host.update(cx, |host, cx| {
                host.focus(window, cx);
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let first_pane = host.read_with(cx, |host, _| {
            host.terminal_tab
                .terminal(PaneId::new(1))
                .cloned()
                .expect("the original Pane should still exist")
        });

        cx.update(|window, cx| {
            first_pane.update(cx, |pane, cx| {
                pane.insert_dropped_file_paths_for_test(
                    &[PathBuf::from("/tmp/first pane")],
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        let paste_requests = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                crate::terminal::testing::RecordedSessionCommand::RequestPaste(text) => {
                    Some((call.session_id, text))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            (
                host.read_with(cx, |host, _| host.focused_pane_id()),
                paste_requests,
            ),
            (PaneId::new(1), vec![(1, "'/tmp/first pane'".to_owned())],)
        );
    }

    #[gpui::test]
    fn terminal_scrollbar_interaction_should_focus_its_owning_pane(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let first_sender = records
            .event_sender(1)
            .expect("the first Pane session was not started");
        first_sender
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts(
                Arc::from([]),
                ScrollbarSnapshot {
                    total_rows: 100,
                    visible_rows: 20,
                    ..Default::default()
                },
                "",
            )))
            .unwrap();
        cx.run_until_parked();

        let first_pane = host.read_with(cx, |host, _| {
            host.pane_bounds
                .get(&PaneId::new(1))
                .copied()
                .expect("the first Pane bounds were not measured")
        });
        cx.simulate_event(ScrollWheelEvent {
            position: first_pane.center(),
            delta: ScrollDelta::Lines(point(0.0, -1.0)),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
        let scrollbar = cx
            .debug_bounds("terminal-scrollbar-thumb-hitbox")
            .expect("the first Pane scrollbar was not revealed");

        cx.simulate_mouse_down(scrollbar.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(scrollbar.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            host.read_with(cx, |host, cx| {
                (
                    host.focused_pane_id(),
                    host.focused_terminal_is_focused(window, cx),
                )
            })
        });
        assert_eq!(state, (PaneId::new(1), true));
    }

    #[gpui::test]
    fn command_shift_vim_shortcuts_should_focus_panes_in_each_direction(cx: &mut TestAppContext) {
        let (host, cx) = four_pane_host(cx);

        let focused_panes = focused_panes_after_shortcuts(
            &host,
            cx,
            ["cmd-shift-l", "cmd-shift-j", "cmd-shift-h", "cmd-shift-k"],
        );

        assert_eq!(
            focused_panes,
            [
                PaneId::new(2),
                PaneId::new(4),
                PaneId::new(3),
                PaneId::new(1),
            ]
        );
    }

    #[gpui::test]
    fn command_option_arrow_shortcuts_should_focus_panes_in_each_direction(
        cx: &mut TestAppContext,
    ) {
        let (host, cx) = four_pane_host(cx);

        let focused_panes = focused_panes_after_shortcuts(
            &host,
            cx,
            [
                "cmd-alt-right",
                "cmd-alt-down",
                "cmd-alt-left",
                "cmd-alt-up",
            ],
        );

        assert_eq!(
            focused_panes,
            [
                PaneId::new(2),
                PaneId::new(4),
                PaneId::new(3),
                PaneId::new(1),
            ]
        );
    }

    #[gpui::test]
    fn terminal_title_event_should_update_the_tab_title_snapshot(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let sender = records
            .last_event_sender()
            .expect("the split Pane session was not started");
        sender
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts(
                Arc::from([]),
                Default::default(),
                "Claude Code",
            )))
            .unwrap();
        cx.run_until_parked();

        let title = host.read_with(cx, |host, _| host.pane_titles.get(&PaneId::new(2)).cloned());
        assert_eq!(
            title.as_ref().map(|title| title.as_ref()),
            Some("Claude Code")
        );
    }

    #[gpui::test]
    fn exited_terminal_session_should_close_its_pane_and_focus_the_neighbor(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let sender = records
            .event_sender(2)
            .expect("the split Pane session was not started");
        sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let state = host.read_with(cx, |host, _| {
            (
                host.terminal_tab.pane_count(),
                host.terminal_tab.focused_pane_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, PaneId::new(1), vec![2]));
    }

    #[gpui::test]
    fn exited_last_terminal_session_should_request_tab_close(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let close_requests = Rc::new(Cell::new(0));
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        let close_requests_for_subscription = Rc::clone(&close_requests);
        host.update(cx, |_, cx| {
            cx.subscribe(&host, move |_, _, _: &PaneHostEvent, _| {
                close_requests_for_subscription.update(|count| count + 1);
            })
            .detach();
        });

        let sender = records
            .event_sender(1)
            .expect("the initial Pane session was not started");
        sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .unwrap();
        sender
            .try_send(SessionEvent::Exited(SessionExit::ExitCode(1)))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            (close_requests.get(), records.dropped_session_ids()),
            (1, Vec::new())
        );
    }

    #[gpui::test]
    fn single_pane_toggle_zoom_should_not_emit_presentation_changed(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let presentation_changes = Rc::new(Cell::new(0));
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        let presentation_changes_for_subscription = Rc::clone(&presentation_changes);
        host.update(cx, |_, cx| {
            cx.subscribe(&host, move |_, _, event: &PaneHostEvent, _| {
                if matches!(event, PaneHostEvent::PresentationChanged { .. }) {
                    presentation_changes_for_subscription.update(|count| count + 1);
                }
            })
            .detach();
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| host.toggle_zoom(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(presentation_changes.get(), 0);
    }

    #[gpui::test]
    fn successful_toggle_zoom_should_emit_one_presentation_changed(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let presentation_changes = Rc::new(Cell::new(0));
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        split_test_pane(&host, PaneId::new(1), SplitAxis::Horizontal, cx);
        let presentation_changes_for_subscription = Rc::clone(&presentation_changes);
        host.update(cx, |_, cx| {
            cx.subscribe(&host, move |_, _, event: &PaneHostEvent, _| {
                if matches!(event, PaneHostEvent::PresentationChanged { .. }) {
                    presentation_changes_for_subscription.update(|count| count + 1);
                }
            })
            .detach();
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| host.toggle_zoom(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(presentation_changes.get(), 1);
    }

    #[gpui::test]
    fn zoom_restore_button_should_restore_panes_without_sending_terminal_pointer_input(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.toggle_zoom(window, cx);
            });
        });
        cx.run_until_parked();

        let restore_button = cx
            .debug_bounds("pane-toggle-zoom-2")
            .map(|bounds| bounds.center())
            .expect("the zoom restore button was not rendered");
        cx.simulate_mouse_move(restore_button, None, Modifiers::none());
        cx.simulate_click(restore_button, Modifiers::none());
        cx.run_until_parked();

        let state = host.read_with(cx, |host, _| {
            (host.terminal_tab.zoom_state(), records.pointer_count())
        });
        assert_eq!(state, (ZoomState::Restored, 0));
    }

    #[gpui::test]
    fn shared_resize_handle_should_resize_split_without_leaking_terminal_input(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let handle = cx
            .debug_bounds("split-divider-1-hitbox")
            .expect("the shared split ResizeHandle was rendered");
        let start = handle.center();
        let destination = point(start.x + px(60.0), start.y + px(40.0));

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(destination, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(destination, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let host = host.read(cx);
            let ratio = match host.terminal_tab.root().node() {
                PaneNodeRef::Split { ratio, .. } => ratio,
                PaneNodeRef::Leaf { .. } => 0.0,
            };
            (
                ratio,
                host.resizing_split_id,
                host.focused_terminal_has_input_focus(window, cx),
            )
        });
        assert!(
            state.0 > 0.5,
            "the shared handle did not grow the first Pane"
        );
        assert_eq!((state.1, state.2, records.pointer_count()), (None, true, 0));
    }

    #[gpui::test]
    fn pane_split_resize_states_should_preserve_hairline_thickness(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let hitbox = cx
            .debug_bounds("split-divider-1-hitbox")
            .expect("the split ResizeHandle was not rendered");
        let target_width = hitbox.size.width;
        let center = hitbox.center();
        let resting = cx
            .debug_bounds("split-divider-1-divider")
            .expect("the split divider was not rendered")
            .size
            .width;

        cx.simulate_mouse_move(center, None, Modifiers::none());
        cx.run_until_parked();
        let hovered = cx
            .debug_bounds("split-divider-1-divider")
            .expect("the hovered split divider was not rendered")
            .size
            .width;
        cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let active = cx
            .debug_bounds("split-divider-1-divider")
            .expect("the active split divider was not rendered")
            .size
            .width;
        cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::none());

        assert_eq!(
            (target_width, resting, hovered, active),
            (px(8.0), px(1.0), px(1.0), px(1.0))
        );
    }

    #[gpui::test]
    fn integrated_split_resize_handle_should_own_both_outer_hitbox_edges(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(TabId::new(1), session_factory, window, cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let handle = cx
            .debug_bounds("split-divider-1-hitbox")
            .expect("the integrated split ResizeHandle was rendered");
        let edges = [
            point(handle.left() + px(0.5), handle.center().y),
            point(handle.right() - px(0.5), handle.center().y),
        ];

        for edge in edges {
            cx.simulate_mouse_move(edge, None, Modifiers::none());
            cx.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
            assert_eq!(
                host.read_with(cx, |host, _| host.resizing_split_id),
                Some(SplitId::new(1)),
                "the split handle did not own outer hitbox edge {edge:?}"
            );
            cx.simulate_mouse_up(edge, MouseButton::Left, Modifiers::none());
            assert_eq!(
                host.read_with(cx, |host, _| host.resizing_split_id),
                None,
                "the split handle did not release outer hitbox edge {edge:?}"
            );
        }

        assert_eq!(records.pointer_count(), 0);
    }

    #[test]
    fn split_ratio_should_follow_horizontal_requested_offset() {
        let split_bounds = bounds(point(px(10.0), px(20.0)), size(px(401.0), px(200.0)));

        assert_eq!(
            split_ratio_for_offset(SplitAxis::Horizontal, split_bounds, 100.0),
            Some(0.25)
        );
    }

    #[test]
    fn split_ratio_should_follow_vertical_requested_offset() {
        let split_bounds = bounds(point(px(10.0), px(20.0)), size(px(400.0), px(201.0)));

        assert_eq!(
            split_ratio_for_offset(SplitAxis::Vertical, split_bounds, 50.0),
            Some(0.25)
        );
    }
    fn report_current_directory(
        records: &TestTerminalSessionRecords,
        session: usize,
        generation: u64,
        directory: &str,
        remote: bool,
    ) {
        let mut screen = crate::terminal::ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            crate::terminal::ScrollbarSnapshot::default(),
            "terminal",
            generation,
        );
        let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
        metadata.directory.path = Arc::from(directory);
        if remote {
            metadata.context = crate::terminal::metadata::TerminalMetadataContext::Remote(
                crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                    crate::domain::SshDestination::new("tester@remote".into()).unwrap(),
                    crate::domain::RemoteDirectory::new(directory.into()).unwrap(),
                ),
            );
        }
        records
            .event_sender(session)
            .unwrap()
            .try_send(SessionEvent::Screen(screen))
            .unwrap();
    }

    #[gpui::test]
    fn split_should_inherit_target_directory_and_capture_it_before_remote_wait(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        let records = TestTerminalSessionRecords::default();
        let factory = remote_test_session_factory(records.clone());
        let (host, cx) =
            cx.add_window_view(|window, cx| PaneHost::new(TabId::new(1), factory, window, cx));
        cx.run_until_parked();
        report_current_directory(&records, 1, 1, "/srv/frontend", true);
        cx.run_until_parked();
        split_test_pane(&host, PaneId::new(1), SplitAxis::Horizontal, cx);
        report_current_directory(&records, 2, 1, "/srv/backend", true);
        cx.run_until_parked();
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_pane(PaneId::new(1), SplitAxis::Vertical, window, cx)
            })
        });
        report_current_directory(&records, 1, 2, "/srv/later", true);
        cx.run_until_parked();
        let starts = records.starts();
        assert_eq!(
            starts[1]
                .remote_launch_plan()
                .unwrap()
                .remote_directory()
                .as_str(),
            "/srv/frontend"
        );
        assert_eq!(
            starts[2]
                .remote_launch_plan()
                .unwrap()
                .remote_directory()
                .as_str(),
            "/srv/frontend"
        );
        assert_eq!(
            host.read_with(cx, |host, cx| host.current_directory(PaneId::new(2), cx)),
            Some(CurrentDirectory::Remote(
                crate::domain::RemoteDirectory::new("/srv/backend".into()).unwrap()
            ))
        );
    }

    #[gpui::test]
    fn local_split_should_inherit_target_apply_pins_and_reject_unavailable_directories(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let authority = crate::platform::local_filesystem::LocalFilesystemAuthority::testing();
        let home = fixture.path().to_owned();
        let first = home.join("shell-integration/bash");
        let second = home.join("shell-integration/zsh");
        let records = TestTerminalSessionRecords::default();
        let factory = WorkspaceTerminalSessionFactory::new_local_with_authority(
            Rc::new(TestTerminalSessionFactory::new(records.clone())),
            authority.validate_directory(&home).unwrap(),
            authority.clone(),
        );
        let (host, cx) =
            cx.add_window_view(|window, cx| PaneHost::new(TabId::new(1), factory, window, cx));
        cx.run_until_parked();
        report_current_directory(&records, 1, 1, first.to_str().unwrap(), false);
        cx.run_until_parked();
        split_test_pane(&host, PaneId::new(1), SplitAxis::Horizontal, cx);
        report_current_directory(&records, 2, 1, second.to_str().unwrap(), false);
        cx.run_until_parked();
        split_test_pane(&host, PaneId::new(1), SplitAxis::Vertical, cx);
        host.update(cx, |host, _| {
            host.set_pinned_directory(Some(PinnedDirectory::Local(
                authority.validate_directory(&second).unwrap(),
            )))
        });
        split_test_pane(&host, PaneId::new(1), SplitAxis::Horizontal, cx);
        host.update(cx, |host, _| host.set_pinned_directory(None));
        report_current_directory(
            &records,
            1,
            2,
            home.join("missing").to_str().unwrap(),
            false,
        );
        cx.run_until_parked();
        let count = host.read_with(cx, |host, _| host.pane_count());
        split_test_pane(&host, PaneId::new(1), SplitAxis::Vertical, cx);
        assert_eq!(host.read_with(cx, |host, _| host.pane_count()), count);
        assert_eq!(
            records
                .starts()
                .iter()
                .map(|start| start.local_working_directory().unwrap().path().to_owned())
                .collect::<Vec<_>>(),
            vec![home, first.clone(), first, second]
        );
        assert!(records.dropped_session_ids().is_empty());
    }
}
