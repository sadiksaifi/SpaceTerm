use super::chrome_geometry::concentric_outset;
use super::chrome_icons::{IconRole, InteractiveIconRole};
use super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use super::drag_and_drop::{DragPreview, DragSession, drag_release_observer, grab_point};
use super::pane_lifecycle::{PaneConstruction, RemoteHierarchyLifecycle};
use super::terminal_status::{
    StatusColors, StatusGlyph, TerminalProgress, reported_glyph_is_drawable, reported_title,
};
use crate::domain::PinnedDirectory;
use crate::domain::remote_workspace::RemoteRestartBatch;
use crate::terminal::metadata::CurrentDirectory;
use crate::ui::appearance::gpui_color;
use std::collections::BTreeMap;

use thiserror::Error;

use super::terminal_focus::{TerminalFocusBlocker, TerminalFocusCoordinator, TerminalProductFocus};
use super::{
    ClosePane, FocusNextPane, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp,
    FocusPreviousPane, PaneOrigin, PreparedRemotePaneRestart, RemoteChildLaunchUnavailable,
    RemotePaneLifecycleError, SplitDown, SplitRight, TERMINAL_KEY_CONTEXT, TerminalPane,
    TerminalPaneEvent, TogglePaneZoom,
};

#[derive(Debug, Error)]
/// A typed rejection while coordinating Remote lifecycle across one Tab's Pane hierarchy.
pub(crate) enum RemoteTabViewLifecycleError {
    #[error("Pane {pane_id} cannot change remote Terminal Session lifecycle: {source}")]
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
pub(crate) struct PreparedTabViewRemoteRestart {
    tab_id: TabId,
    panes: RemoteRestartBatch<(PaneId, Entity<TerminalPane>, PreparedRemotePaneRestart)>,
}
use crate::domain::{
    ClosePaneOutcome, FocusDirection, PaneEdge, PaneId, PaneNodeRef, PaneSize, PaneTreeRef,
    SplitAxis, SplitId, Tab, TabId, WorkspaceId, ZoomState,
};
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch,
    WorkspaceTerminalSessionFactory,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, DefiniteLength, Entity, EventEmitter, MouseDownEvent, Pixels,
    Point, Render, Window, div, px, relative,
};
use spaceterm_ui::{
    Alert, AlertIntent, ButtonSize, ButtonVariant, HoverFade, Icon, IconButton, IconName,
    ModalAction, ModalActionRole, ModalId, ResizeAxis, ResizeHandle, ResizeHandleEvent,
    ResizeInputSource, Tooltip,
};

/// The empty base surface between Split Panes, which every Pane Layout calculation reserves.
fn pane_gap(cx: &App) -> f32 {
    f32::from(
        super::workspace_frame::WorkspaceFrame::for_appearance(super::appearance::chrome(cx), cx)
            .pane_gap(),
    )
}
/// The Pane Caption's outer height, including its symmetric vertical padding.
#[cfg(test)]
const PANE_CAPTION_HEIGHT: f32 = 32.0;
const PANE_CAPTION_VERTICAL_PADDING: f32 = 4.0;
const PANE_CAPTION_LEFT_PADDING: f32 = 10.0;
const PANE_CAPTION_RIGHT_PADDING: f32 = 5.0;
/// Width reserved by the middle dot that separates a directory from its status.
const PANE_CAPTION_SEPARATOR_WIDTH: f32 = 17.25;
const PANE_STATUS_ICON_GAP: f32 = 6.0;
/// Width reserved by the chevron that separates a Pane's origin from its directory.
const PANE_ORIGIN_SEPARATOR_WIDTH: f32 = 18.4;
#[cfg(test)]
const PANE_CONTROL_SIZE: f32 = 20.0;
const PANE_CONTROL_GAP: f32 = 2.0;
/// How much narrower the gap before Close Pane is than every other Pane control gap, because the
/// open Close Pane glyph reads as wider.
const PANE_CLOSE_OPTICAL_TRIM: f32 = 1.0;
const PANE_CONTROL_LEADING_GAP: f32 = 6.0;
/// The share of the accent color that fills the half of a Pane a dragged Pane would take.
const PANE_DROP_TARGET_FILL_OPACITY: u8 = 56;
/// The status glyph and its trailing air, which every Pane Caption keeps at every width.
#[cfg(test)]
const PANE_STATUS_WIDTH: f32 = 13.0 + PANE_STATUS_ICON_GAP;

#[derive(Clone, Copy)]
enum PaneCaptionAction {
    SplitRight,
    SplitDown,
    ToggleZoom,
    Close,
}

/// Which caption segments this frame's Pane width can hold. The Pane name and status glyph are
/// never dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CaptionLayout {
    show_status_separator: bool,
    show_host: bool,
    show_directory: bool,
    show_user: bool,
    show_label: bool,
    show_splits: bool,
}

/// How many caption segments beyond the Pane name a narrowing caption can give up.
const CAPTION_LADDER: usize = 5;

/// Rendered widths of the caption segments, including separators where present.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CaptionMetrics {
    user: Pixels,
    host: Pixels,
    directory: Pixels,
    name: Pixels,
    status_separator: Pixels,
    label: Pixels,
}

impl CaptionMetrics {
    fn measure(
        text: &PaneCaptionText,
        window: &Window,
        appearance: &super::appearance::ChromeAppearance,
    ) -> Self {
        let host = measure_caption_segment(&text.origin.host, window, appearance);
        Self {
            user: if text.origin.user.is_empty() {
                px(0.0)
            } else {
                measure_caption_segment(&origin_account(&text.origin.user), window, appearance)
            },
            host: if host > px(0.0) {
                host + appearance.spacing(PANE_ORIGIN_SEPARATOR_WIDTH)
            } else {
                host
            },
            directory: measure_caption_segment(&text.directory, window, appearance),
            name: measure_caption_segment(&text.name, window, appearance),
            status_separator: if text.has_directory {
                appearance.spacing(PANE_CAPTION_SEPARATOR_WIDTH)
            } else {
                px(0.0)
            },
            label: measure_caption_segment(&text.label, window, appearance),
        }
    }

    /// The droppable segments in the order a widening caption admits them.
    const fn ladder(self) -> [Pixels; CAPTION_LADDER] {
        [
            self.status_separator,
            self.host,
            self.directory,
            self.user,
            self.label,
        ]
    }
}

impl CaptionLayout {
    fn resolve(
        caption: &PaneCaption,
        width: Pixels,
        window: &Window,
        appearance: &super::appearance::ChromeAppearance,
    ) -> Self {
        Self::from_metrics(
            caption.has_multiple_panes,
            width,
            CaptionMetrics::measure(&caption.text, window, appearance),
            appearance.spacing_scale,
            f32::from(appearance.icons.metrics(IconRole::Status).glyph_size),
            f32::from(
                appearance
                    .icons
                    .interactive_target_size(InteractiveIconRole::Control),
            ),
        )
    }

    fn from_metrics(
        has_multiple_panes: bool,
        width: Pixels,
        metrics: CaptionMetrics,
        spacing_scale: f32,
        status_icon_size: f32,
        control_size: f32,
    ) -> Self {
        let full_control_count = if has_multiple_panes { 4 } else { 2 };
        let fixed_width =
            (PANE_CAPTION_LEFT_PADDING + PANE_CAPTION_RIGHT_PADDING + PANE_STATUS_ICON_GAP)
                * spacing_scale
                + status_icon_size;
        let show_splits = width
            >= px(fixed_width
                + PANE_CONTROL_LEADING_GAP * spacing_scale
                + controls_width(
                    full_control_count,
                    has_multiple_panes,
                    control_size,
                    spacing_scale,
                ));
        let control_count = usize::from(show_splits) * 2 + usize::from(has_multiple_panes) * 2;
        let leading_gap = if control_count == 0 {
            0.0
        } else {
            PANE_CONTROL_LEADING_GAP * spacing_scale
        };
        let available = (width
            - px(fixed_width
                + leading_gap
                + controls_width(
                    control_count,
                    has_multiple_panes,
                    control_size,
                    spacing_scale,
                )))
        .max(px(0.0));
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
            show_status_separator,
            show_host,
            show_directory,
            show_user,
            show_label,
        ] = shown;
        Self {
            show_status_separator,
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

const fn controls_width(count: usize, closes: bool, control_size: f32, spacing_scale: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let close_trim = if closes { PANE_CLOSE_OPTICAL_TRIM } else { 0.0 };
    let gaps = (count - 1) as f32 * PANE_CONTROL_GAP - close_trim;
    count as f32 * control_size + gaps * spacing_scale
}

fn measure_caption_segment(
    text: &gpui::SharedString,
    window: &Window,
    appearance: &super::appearance::ChromeAppearance,
) -> Pixels {
    if text.is_empty() {
        return px(0.0);
    }
    appearance.typography.measure(TextRole::Body, text, window)
}

fn minimum_pane_width(appearance: &super::appearance::ChromeAppearance) -> f32 {
    (PANE_CAPTION_LEFT_PADDING
        + PANE_CAPTION_RIGHT_PADDING
        + PANE_STATUS_ICON_GAP
        + PANE_CONTROL_LEADING_GAP)
        * appearance.spacing_scale
        + f32::from(appearance.icons.metrics(IconRole::Status).glyph_size)
        // A single Pane's two split controls have no Close Pane trim and set the wider minimum.
        + controls_width(
            2,
            false,
            f32::from(
                appearance
                    .icons
                    .interactive_target_size(InteractiveIconRole::Control),
            ),
            appearance.spacing_scale,
        )
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum TabViewEvent {
    UserClosePaneRequested { tab_id: TabId, pane_id: PaneId },
    CloseTabRequested { tab_id: TabId },
    PresentationChanged { tab_id: TabId },
}

impl std::fmt::Debug for TabViewEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UserClosePaneRequested { .. } => "TabViewEvent::UserClosePaneRequested",
            Self::CloseTabRequested { .. } => "TabViewEvent::CloseTabRequested",
            Self::PresentationChanged { .. } => "TabViewEvent::PresentationChanged",
        })
    }
}

/// The value a Pane drag carries, scoped to the Pane Layout that owns the Pane.
struct DraggedPane {
    pane_id: PaneId,
    owner: gpui::EntityId,
}

/// The widest a lifted Pane Caption grows, so a wide Pane lifts a card rather than a bar.
const LIFTED_CAPTION_MAXIMUM_WIDTH: f32 = 320.0;

/// Each Pane's hover and how far it has eased, as read at the start of a frame.
type PaneHovers = BTreeMap<PaneId, (HoverFade, f32)>;

/// A Pane lifted by its caption, the edge of another Pane it takes if released now, and the drag
/// that carries it.
struct PaneDrag {
    pane_id: PaneId,
    drop_target: Option<(PaneId, PaneEdge)>,
    session: DragSession,
}

pub(crate) struct TabView {
    tab: Tab<Entity<TerminalPane>>,
    session_factory: WorkspaceTerminalSessionFactory,
    pane_construction: PaneConstruction,
    pane_bounds: BTreeMap<PaneId, Bounds<Pixels>>,
    pane_layout_size: Option<PaneSize>,
    split_bounds: BTreeMap<SplitId, Bounds<Pixels>>,
    pane_titles: BTreeMap<PaneId, gpui::SharedString>,
    pane_captions: BTreeMap<PaneId, PaneCaptionText>,
    pane_attention: BTreeMap<PaneId, u32>,
    resizing_split_id: Option<SplitId>,
    pane_drag: Option<PaneDrag>,
    active: bool,
    focus_branch_blocker: Option<TerminalFocusBlocker>,
    native_service_hierarchy_generation: u64,
    native_service_focus_signature: Option<(bool, PaneId, Option<TerminalFocusBlocker>)>,
    close_tab_requested: bool,
    remote_lifecycle: RemoteHierarchyLifecycle,
}

impl TabView {
    #[cfg(test)]
    pub(crate) fn new(
        tab_id: TabId,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prepared_launch = match session_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(error) => panic!("test TabView channel preparation failed: {error}"),
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
        let appearance = super::appearance::shared_chrome(cx);
        let radius =
            super::workspace_frame::WorkspaceFrame::for_appearance(&appearance, cx).pane_radius();
        let minimum_pane_size = match PaneSize::new(
            minimum_pane_width(&appearance),
            f32::from(appearance.caption_height() + radius) + 4.0,
        ) {
            Ok(size) => size,
            Err(error) => {
                unreachable!("fixed minimum Pane dimensions must be valid: {error}")
            }
        };
        let tab = Tab::new(tab_id, minimum_pane_size, |pane_id| {
            Self::create_terminal(
                pane_id,
                session_factory.clone(),
                prepared_launch,
                pane_construction.clone(),
                window,
                cx,
            )
        });
        let initial_pane_id = tab.focused_pane_id();
        let Some(initial_terminal) = tab.pane(initial_pane_id) else {
            unreachable!("a new Tab must own its initial Pane terminal")
        };
        let initial_title = initial_terminal.read(cx).title();
        let initial_caption = PaneCaptionText::from_terminal(initial_terminal.read(cx));

        Self {
            tab,
            session_factory,
            pane_construction,
            pane_bounds: BTreeMap::new(),
            pane_layout_size: None,
            split_bounds: BTreeMap::new(),
            pane_titles: BTreeMap::from([(initial_pane_id, initial_title)]),
            pane_captions: BTreeMap::from([(initial_pane_id, initial_caption)]),
            pane_attention: BTreeMap::from([(initial_pane_id, 0)]),
            resizing_split_id: None,
            pane_drag: None,
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
            move |view, terminal, event: &TerminalPaneEvent, window, cx| match event {
                TerminalPaneEvent::FocusRequested => view.focus_pane(pane_id, cx),
                TerminalPaneEvent::SurfaceBackgroundChanged => cx.notify(),
                TerminalPaneEvent::TitleChanged(title) => {
                    view.pane_titles.insert(pane_id, title.clone());
                    cx.emit(TabViewEvent::PresentationChanged {
                        tab_id: view.tab.id(),
                    });
                    cx.notify();
                }
                TerminalPaneEvent::CaptionChanged => {
                    let caption = PaneCaptionText::from_terminal(terminal.read(cx));
                    if view.pane_captions.get(&pane_id) != Some(&caption) {
                        view.pane_captions.insert(pane_id, caption);
                        if pane_id == view.tab.root_pane_id()
                            || pane_id == view.tab.focused_pane_id()
                        {
                            cx.emit(TabViewEvent::PresentationChanged {
                                tab_id: view.tab.id(),
                            });
                        }
                        cx.notify();
                    }
                }
                TerminalPaneEvent::AttentionChanged { unread_count } => {
                    view.pane_attention.insert(pane_id, *unread_count);
                    cx.emit(TabViewEvent::PresentationChanged {
                        tab_id: view.tab.id(),
                    });
                    cx.notify();
                }
                TerminalPaneEvent::Exited => view.close_pane(pane_id, window, cx),
            },
        )
        .detach();
        terminal
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        let Some(terminal) = self.tab.pane(self.tab.focused_pane_id()) else {
            return;
        };
        terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a desktop Services Adapter queries Services state"
        )
    )]
    pub(crate) fn native_service_status(
        &mut self,
        workspace_id: WorkspaceId,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> NativeServiceStatus {
        self.sync_terminal_focus(cx);
        let pane_id = self.tab.focused_pane_id();
        let tab_id = self.tab.id();
        let hierarchy_generation = self.native_service_hierarchy_generation;
        let Some(terminal) = self.tab.pane(pane_id) else {
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

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a desktop Services Adapter queries Services state"
        )
    )]
    pub(crate) fn native_service_target(
        &self,
        origin: NativeServiceOrigin,
    ) -> Option<Entity<TerminalPane>> {
        if self.tab.id() != origin.tab_id()
            || self.tab.focused_pane_id() != origin.pane_id()
            || self.native_service_hierarchy_generation != origin.hierarchy_generation()
        {
            return None;
        }
        self.tab.pane(origin.pane_id()).cloned()
    }

    pub(crate) const fn tab_id(&self) -> TabId {
        self.tab.id()
    }

    pub(crate) fn pane_count(&self) -> usize {
        self.tab.pane_count()
    }

    pub(crate) fn automatic_directory(&self, cx: &App) -> Option<CurrentDirectory> {
        self.current_directory(self.tab.root_pane_id(), cx)
    }

    pub(crate) fn current_directory(&self, pane_id: PaneId, cx: &App) -> Option<CurrentDirectory> {
        self.tab
            .pane(pane_id)
            .and_then(|terminal| terminal.read(cx).current_directory())
    }

    pub(crate) fn terminal_panes<'a>(
        &'a self,
        cx: &'a App,
    ) -> impl Iterator<Item = (PaneId, &'a TerminalPane)> {
        self.tab
            .panes_with_ids()
            .map(|(id, terminal)| (id, terminal.read(cx)))
    }

    pub(crate) fn set_pinned_directory(&mut self, directory: Option<PinnedDirectory>) {
        self.session_factory.set_pinned_directory(directory);
    }

    /// What this Tab presents about the Terminal Session its Focused Pane runs, read from that
    /// Pane's caption facts.
    pub(crate) fn tab_identity(&self) -> TabIdentity {
        let caption = self
            .pane_captions
            .get(&self.tab.focused_pane_id())
            .cloned()
            .unwrap_or_default();
        let title = self
            .pane_titles
            .get(&self.tab.focused_pane_id())
            .map(|title| gpui::SharedString::from(reported_title(title).words.to_owned()))
            .unwrap_or_else(|| "Terminal".into());
        TabIdentity::resolve(
            caption,
            title,
            self.pane_attention.values().copied().sum::<u32>() > 0,
        )
    }

    #[cfg(test)]
    pub(crate) fn cached_focused_progress(&self) -> TerminalProgress {
        self.pane_captions
            .get(&self.tab.focused_pane_id())
            .map_or(TerminalProgress::None, |caption| caption.progress)
    }

    /// Records unread attention for one Pane as its Terminal Session would report it.
    #[cfg(test)]
    pub(crate) fn set_test_attention(
        &mut self,
        pane_id: PaneId,
        unread_count: u32,
        cx: &mut Context<Self>,
    ) {
        self.pane_attention.insert(pane_id, unread_count);
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn tab_title(&self) -> gpui::SharedString {
        self.tab_identity().one_line()
    }

    #[cfg(test)]
    pub(crate) const fn zoom_state(&self) -> ZoomState {
        self.tab.zoom_state()
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
        for terminal in self.tab.panes() {
            terminal.update(cx, |terminal, _| terminal.close());
        }
    }

    /// Atomically marks every Pane in this Tab disconnected for one generation. All Panes are
    /// prevalidated before any mutation.
    pub(crate) fn disconnect_remote(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), RemoteTabViewLifecycleError> {
        self.can_disconnect_remote(generation, cx)?;
        for (_, terminal) in self.tab.panes_with_ids() {
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
    ) -> Result<(), RemoteTabViewLifecycleError> {
        for (pane_id, terminal) in self.tab.panes_with_ids() {
            terminal
                .read(cx)
                .can_disconnect_remote(generation)
                .map_err(|source| RemoteTabViewLifecycleError::Pane { pane_id, source })?;
        }
        Ok(())
    }

    /// Binds one already-reserved launch to every existing Pane without mutating the hierarchy. Any
    /// failure drops the aggregate token and leaves all Panes disconnected.
    pub(crate) fn prepare_remote_restart(
        &self,
        session_factory: WorkspaceTerminalSessionFactory,
        generation: u64,
        prepared_launches: Vec<PreparedWorkspaceTerminalLaunch>,
        cx: &App,
    ) -> Result<PreparedTabViewRemoteRestart, RemoteTabViewLifecycleError> {
        if self.tab.pane_count() != prepared_launches.len() {
            return Err(RemoteTabViewLifecycleError::PaneChanged(
                self.tab.focused_pane_id(),
            ));
        }
        let mut panes = Vec::with_capacity(self.tab.pane_count());
        for ((pane_id, terminal), prepared_launch) in
            self.tab.panes_with_ids().zip(prepared_launches)
        {
            let prepared = terminal
                .read(cx)
                .prepare_remote_restart(session_factory.clone(), generation, prepared_launch)
                .map_err(|source| RemoteTabViewLifecycleError::Pane { pane_id, source })?;
            panes.push((pane_id, terminal.clone(), prepared));
        }
        Ok(PreparedTabViewRemoteRestart {
            tab_id: self.tab.id(),
            panes: RemoteRestartBatch::new(panes),
        })
    }

    /// Revalidates every prepared Pane restart against the current Tab hierarchy.
    pub(crate) fn can_commit_remote_restart(
        &self,
        prepared: &PreparedTabViewRemoteRestart,
        cx: &App,
    ) -> Result<(), RemoteTabViewLifecycleError> {
        if self.tab.id() != prepared.tab_id {
            return Err(RemoteTabViewLifecycleError::TabChanged {
                prepared: prepared.tab_id,
                current: self.tab.id(),
            });
        }
        prepared.panes.validate(
            self.tab.pane_count(),
            || RemoteTabViewLifecycleError::PaneChanged(self.tab.focused_pane_id()),
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
    ) -> Result<(), RemoteTabViewLifecycleError> {
        let Some(current) = self.tab.pane(pane_id) else {
            return Err(RemoteTabViewLifecycleError::PaneChanged(pane_id));
        };
        if current.entity_id() != terminal.entity_id() {
            return Err(RemoteTabViewLifecycleError::PaneChanged(pane_id));
        }
        terminal
            .read(cx)
            .can_commit_remote_restart(pane_restart)
            .map_err(|source| RemoteTabViewLifecycleError::Pane { pane_id, source })?;
        Ok(())
    }

    /// Commits every prevalidated Pane restart in place after aggregate preparation succeeds. A
    /// later startup failure belongs to its Pane rather than rolling back committed siblings.
    pub(crate) fn commit_remote_restart(
        &mut self,
        prepared: PreparedTabViewRemoteRestart,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RemoteTabViewLifecycleError> {
        if self.tab.id() != prepared.tab_id {
            return Err(RemoteTabViewLifecycleError::TabChanged {
                prepared: prepared.tab_id,
                current: self.tab.id(),
            });
        }
        prepared.panes.commit(
            self.tab.pane_count(),
            || RemoteTabViewLifecycleError::PaneChanged(self.tab.focused_pane_id()),
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
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });
        cx.notify();
        Ok(())
    }

    pub(crate) const fn focused_pane_id(&self) -> PaneId {
        self.tab.focused_pane_id()
    }

    #[cfg(test)]
    pub(crate) fn pane_entity_ids(&self) -> Vec<(PaneId, gpui::EntityId)> {
        self.tab
            .panes_with_ids()
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
        encode(self.tab.root(), &mut signature);
        signature
    }

    #[cfg(test)]
    pub(crate) const fn remote_disconnected_generation(&self) -> Option<u64> {
        self.remote_lifecycle.disconnected_generation()
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_remote_state(&self, cx: &App) -> (bool, bool) {
        self.tab
            .pane(self.tab.focused_pane_id())
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
        self.tab
            .panes_with_ids()
            .map(|(pane_id, terminal)| {
                let terminal = terminal.read(cx);
                let (session_attached, failure_operation) = terminal.restart_state();
                (pane_id, session_attached, failure_operation)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.tab
            .pane(self.tab.focused_pane_id())
            .is_some_and(|terminal| terminal.read(cx).is_focused(window))
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_has_input_focus(&self, window: &Window, cx: &App) -> bool {
        self.tab
            .pane(self.tab.focused_pane_id())
            .is_some_and(|terminal| terminal.read(cx).terminal_input_focused(window, cx))
    }

    #[cfg(test)]
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    fn focus_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        if self.tab.focused_pane_id() == pane_id {
            return;
        }
        if let Err(error) = self.tab.focus_pane(pane_id) {
            eprintln!("failed to focus Pane: {error}");
            return;
        }
        self.sync_terminal_focus(cx);
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });
        cx.notify();
    }

    fn focus_pane_in_direction(
        &mut self,
        direction: FocusDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane_id) = self.tab.focus_pane_in_direction(direction) else {
            return;
        };
        self.finish_keyboard_focus_change(pane_id, window, cx);
    }

    fn finish_keyboard_focus_change(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_terminal_focus(cx);
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });
        cx.notify();
        if let Some(terminal) = self.tab.pane(pane_id) {
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
        }
    }

    pub(crate) fn split_focused(
        &mut self,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused_pane_id = self.tab.focused_pane_id();
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
        let Some(target_size) = self.split_target_size(target_pane_id, pane_gap(cx)) else {
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
                let view = cx.weak_entity();
                let window_handle = window.window_handle();
                let _ = Alert::new(
                    ModalId::new("pane-starting-directory-unavailable"),
                    "Starting directory unavailable",
                    "Starting Directory Unavailable",
                    detail,
                    vec![ModalAction::new(
                        (),
                        "OK",
                        ModalActionRole::Cancel,
                        "pane-start-error-ok",
                    )],
                )
                .intent(AlertIntent::Warning)
                .present(window, cx, move |_, cx| {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        let _ = view.update(cx, |view, cx| view.focus(window, cx));
                    });
                });
                return;
            }
        };
        if let Some(revalidation) = session_factory.revalidate_remote_child_launch() {
            let child_launch_generation = self.remote_lifecycle.begin_child_launch();
            cx.spawn_in(window, async move |view, cx| {
                let revalidation = revalidation.await;
                let _ = view.update_in(cx, |view, window, cx| {
                    if view.remote_lifecycle.disconnected_generation().is_some() {
                        cx.emit(RemoteChildLaunchUnavailable::Cancelled);
                        return;
                    }
                    if !view
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
                    let Some(current_size) = view.split_target_size(target_pane_id, pane_gap(cx))
                    else {
                        return;
                    };
                    if current_size != target_size {
                        return;
                    }
                    view.split_pane_with_prepared_launch(
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

    fn split_target_size(&self, pane_id: PaneId, gap: f32) -> Option<PaneSize> {
        match self.tab.zoom_state() {
            ZoomState::Restored => self
                .pane_bounds
                .get(&pane_id)
                .and_then(|bounds| pane_size(*bounds).ok()),
            ZoomState::Zoomed(_) => {
                restored_leaf_size(self.tab.root(), pane_id, self.pane_layout_size?, gap)
            }
        }
    }

    /// Lifts a Pane by its caption so it can split another Pane in this Tab.
    fn begin_pane_drag(
        &mut self,
        pane_id: PaneId,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> DragPreview {
        let session = DragSession::begin(cx, |view: &mut Self, _, cx| {
            if view.pane_drag.take().is_some() {
                cx.notify();
            }
        });
        self.pane_drag = Some(PaneDrag {
            pane_id,
            drop_target: None,
            session,
        });
        let pointer = window.mouse_position();
        self.drag_pane_to(pane_id, pointer, cx);
        cx.notify();
        let Some(pane) = self.pane_bounds.get(&pane_id).copied() else {
            return DragPreview::empty();
        };
        let caption = Bounds::new(
            pane.origin,
            gpui::size(
                pane.size.width,
                super::appearance::chrome(cx).caption_height(),
            ),
        );
        let grab = grab_point(window, cx);
        let view = cx.entity().downgrade();
        DragPreview::new(window, cx, move |window, cx| {
            view.upgrade()
                .map(|view| {
                    view.read(cx)
                        .render_lifted_caption(pane_id, caption, grab, window, cx)
                })
                .unwrap_or_else(|| div().into_any_element())
        })
    }

    /// The copy of a dragged Pane's caption that follows the pointer, without the controls.
    /// `caption` is where the caption was painted and `grab` where the pointer took it, in window
    /// coordinates.
    fn render_lifted_caption(
        &self,
        pane_id: PaneId,
        caption: Bounds<Pixels>,
        grab: Point<Pixels>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let Some(terminal) = self.tab.pane(pane_id).cloned() else {
            return div().into_any_element();
        };
        let appearance = super::appearance::chrome(cx);
        let mut text = self
            .pane_captions
            .get(&pane_id)
            .cloned()
            .unwrap_or_default();
        text.glyph = drawable_reported_glyph(text.glyph.as_ref(), |glyph| {
            let caption_style = appearance.typography.style(TextRole::Body);
            reported_glyph_is_drawable(glyph, &caption_style.font, caption_style.size, window)
        });
        let focused = self.tab.focused_pane_id() == pane_id;
        let attention = self.pane_attention.get(&pane_id).copied().unwrap_or(0) > 0;
        let background = terminal.read(cx).surface_background();
        let paint = appearance.colors.caption(background, focused);
        let card_width = caption
            .size
            .width
            .min(appearance.spacing(LIFTED_CAPTION_MAXIMUM_WIDTH));
        let grab_x = (grab.x - caption.origin.x).clamp(px(0.0), caption.size.width);
        let card_left = grab_x * (1.0 - card_width / caption.size.width.max(card_width));
        let layout = CaptionLayout::resolve(
            &PaneCaption {
                pane_id,
                terminal,
                text: text.clone(),
                focused,
                zoomed: false,
                attention,
                has_multiple_panes: true,
            },
            card_width,
            window,
            appearance,
        );
        // The Pane's surface as the window shows it, so the card reads the same over any Pane.
        let surface = appearance
            .pane_surface(background)
            .source_over(appearance.control_host_background(spaceterm_ui::ControlHost::Window));
        let radius =
            super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx).pane_radius();
        let shell =
            spaceterm_ui::floating_surface_theme(cx).shell(spaceterm_ui::FloatingRole::Popover);
        let card = shell
            .frame(
                caption_row(appearance, paint.foreground).child(render_caption_identity(
                    pane_id, text, attention, layout, appearance, &paint,
                )),
            )
            .debug_selector(move || format!("pane-caption-preview-{}", pane_id.get()))
            .absolute()
            .top_0()
            .left(card_left)
            .w(card_width)
            .h(caption.size.height)
            .rounded(radius)
            .bg(gpui_color(surface))
            .border_color(gpui_color(appearance.pane_rim_on(background)));
        div()
            .relative()
            .w(caption.size.width)
            .h(caption.size.height)
            .child(card)
            .into_any_element()
    }

    fn drag_pane_to(&mut self, pane_id: PaneId, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let drop_target = self.drop_target(pane_id, pointer, pane_gap(cx));
        let Some(drag) = self
            .pane_drag
            .as_mut()
            .filter(|drag| drag.pane_id == pane_id)
        else {
            return;
        };
        if drag.drop_target != drop_target {
            drag.drop_target = drop_target;
            cx.notify();
        }
    }

    /// Ends a Pane drag, moving the Pane onto the edge of the Pane under `pointer`, if any.
    fn finish_pane_drag(
        &mut self,
        pointer: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.pane_drag.take() else {
            return;
        };
        cx.notify();
        if !drag.session.is_active(cx) {
            return;
        }
        let gap = pane_gap(cx);
        let Some((target_pane_id, edge)) = self.drop_target(drag.pane_id, pointer, gap) else {
            return;
        };
        let Some(target_size) = self.split_target_size(target_pane_id, gap) else {
            return;
        };
        match self
            .tab
            .move_pane(drag.pane_id, target_pane_id, edge, target_size, gap)
        {
            Ok(true) => {
                self.advance_native_service_hierarchy_generation(cx);
                self.split_bounds.clear();
                self.sync_terminal_focus(cx);
                cx.emit(TabViewEvent::PresentationChanged {
                    tab_id: self.tab.id(),
                });
                if let Some(terminal) = self.tab.pane(drag.pane_id) {
                    terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
                }
            }
            Ok(false) => {}
            Err(error) => eprintln!("failed to move Pane: {error}"),
        }
    }

    /// The Pane under `pointer` that can take the dragged Pane, and the edge it would take.
    fn drop_target(
        &self,
        pane_id: PaneId,
        pointer: Point<Pixels>,
        gap: f32,
    ) -> Option<(PaneId, PaneEdge)> {
        if matches!(self.tab.zoom_state(), ZoomState::Zoomed(_)) {
            return None;
        }
        let mut panes = Vec::new();
        collect_pane_order(self.tab.root(), &mut panes);
        let (target_pane_id, bounds) = panes
            .into_iter()
            .filter_map(|candidate| Some((candidate, *self.pane_bounds.get(&candidate)?)))
            .find(|(_, bounds)| bounds.contains(&pointer))?;
        if target_pane_id == pane_id {
            return None;
        }
        let edge = drop_edge(bounds, pointer);
        let target_size = self.split_target_size(target_pane_id, gap)?;
        self.tab
            .can_receive_pane(target_pane_id, edge, target_size, gap)
            .then_some((target_pane_id, edge))
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
        let gap = pane_gap(cx);
        let result = self
            .tab
            .split_pane(target_pane_id, axis, target_size, gap, |new_pane_id| {
                Self::create_terminal(
                    new_pane_id,
                    session_factory,
                    prepared_launch,
                    pane_construction,
                    window,
                    cx,
                )
            });

        match result {
            Ok(pane_id) => {
                self.advance_native_service_hierarchy_generation(cx);
                if let Some(terminal) = self.tab.pane(pane_id) {
                    self.pane_titles.insert(pane_id, terminal.read(cx).title());
                    self.pane_captions
                        .insert(pane_id, PaneCaptionText::from_terminal(terminal.read(cx)));
                }
                self.pane_attention.insert(pane_id, 0);
                self.split_bounds.clear();
                self.sync_terminal_focus(cx);
                cx.emit(TabViewEvent::PresentationChanged {
                    tab_id: self.tab.id(),
                });
                cx.notify();
                if let Some(terminal) = self.tab.pane(pane_id) {
                    terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
                }
            }
            Err(error) => eprintln!("failed to split Pane: {error}"),
        }
    }

    fn request_close_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        if self.close_tab_requested || self.tab.pane(pane_id).is_none() {
            return;
        }
        cx.emit(TabViewEvent::UserClosePaneRequested {
            tab_id: self.tab.id(),
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

    fn close_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_tab_requested {
            return;
        }

        match self.tab.close_pane(pane_id) {
            Ok(ClosePaneOutcome::CloseTab { tab_id }) => {
                self.advance_native_service_hierarchy_generation(cx);
                self.close_tab_requested = true;
                self.active = false;
                self.sync_terminal_focus(cx);
                cx.emit(TabViewEvent::CloseTabRequested { tab_id });
            }
            Ok(ClosePaneOutcome::PaneClosed {
                focused_pane_id,
                closed_pane,
                ..
            }) => {
                self.advance_native_service_hierarchy_generation(cx);
                closed_pane.update(cx, |terminal, _| {
                    terminal.set_accessibility_hierarchy(false, usize::MAX);
                    terminal.close();
                });
                self.pane_bounds.remove(&pane_id);
                self.split_bounds.clear();
                self.pane_titles.remove(&pane_id);
                self.pane_captions.remove(&pane_id);
                self.pane_attention.remove(&pane_id);
                self.sync_terminal_focus(cx);
                cx.emit(TabViewEvent::PresentationChanged {
                    tab_id: self.tab.id(),
                });
                cx.notify();
                if self.active
                    && let Some(terminal) = self.tab.pane(focused_pane_id)
                {
                    terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
                }
            }
            Err(error) => eprintln!("failed to close Pane: {error}"),
        }
    }

    pub(crate) fn toggle_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab.toggle_zoom().is_none() {
            return;
        }
        self.advance_native_service_hierarchy_generation(cx);
        self.sync_terminal_focus(cx);
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
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
    ) -> Option<f32> {
        let bounds = self.split_bounds.get(&split_id).copied()?;
        let gap = pane_gap(cx);
        let requested_ratio = split_ratio_for_offset(axis, bounds, requested_offset, gap)?;
        let Ok(available_size) = pane_size(bounds) else {
            return None;
        };
        match self
            .tab
            .resize_split(split_id, available_size, gap, requested_ratio)
        {
            Ok(accepted_ratio) => {
                cx.notify();
                split_content_extent(axis, bounds, gap).map(|extent| extent * accepted_ratio)
            }
            Err(error) => {
                eprintln!("failed to resize split: {error}");
                None
            }
        }
    }

    fn reset_split(&mut self, split_id: SplitId, cx: &mut Context<Self>) {
        let Some(bounds) = self.split_bounds.get(&split_id).copied() else {
            return;
        };
        let Ok(available_size) = pane_size(bounds) else {
            return;
        };
        let gap = pane_gap(cx);
        match self.tab.resize_split(split_id, available_size, gap, 0.5) {
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
    ) -> Option<f32> {
        match event {
            ResizeHandleEvent::InteractionStarted { .. } => {
                self.resizing_split_id = Some(split_id);
                self.sync_terminal_focus(cx);
                cx.notify();
                None
            }
            ResizeHandleEvent::ResizeRequested {
                requested_value, ..
            } => self.resize_split(split_id, axis, requested_value, cx),
            ResizeHandleEvent::ResetRequested { source } => {
                self.reset_split(split_id, cx);
                if source == ResizeInputSource::Pointer && self.active {
                    self.focus(window, cx);
                }
                None
            }
            ResizeHandleEvent::InteractionFinished { source, .. } => {
                if self.resizing_split_id != Some(split_id) {
                    return None;
                }
                self.resizing_split_id = None;
                self.sync_terminal_focus(cx);
                cx.notify();
                if source == ResizeInputSource::Pointer && self.active {
                    self.focus(window, cx);
                }
                None
            }
        }
    }

    fn sync_terminal_focus(&mut self, cx: &mut Context<Self>) {
        let focused_terminal_id = self
            .tab
            .pane(self.tab.focused_pane_id())
            .map(Entity::entity_id);
        let visible_terminal_id = match self.tab.zoom_state() {
            ZoomState::Restored => None,
            ZoomState::Zoomed(pane_id) => self.tab.pane(pane_id).map(Entity::entity_id),
        };
        let blocker = TerminalFocusCoordinator::pane_layout_blocker(
            self.focus_branch_blocker,
            self.resizing_split_id.is_some(),
        );
        let signature = (self.active, self.tab.focused_pane_id(), blocker);
        if self.native_service_focus_signature != Some(signature) {
            self.advance_native_service_hierarchy_generation(cx);
            self.native_service_focus_signature = Some(signature);
        }
        let hierarchy_generation = self.native_service_hierarchy_generation;
        let mut panes = Vec::with_capacity(self.tab.pane_count());
        collect_pane_order(self.tab.root(), &mut panes);
        let presented_panes = match self.tab.zoom_state() {
            ZoomState::Zoomed(pane_id) => vec![pane_id],
            ZoomState::Restored => panes.clone(),
        };
        let presentation_order = presented_panes
            .into_iter()
            .enumerate()
            .map(|(order, pane_id)| (pane_id, order))
            .collect::<BTreeMap<_, _>>();
        for pane_id in panes {
            let Some(terminal) = self.tab.pane(pane_id) else {
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
        for terminal in self.tab.panes() {
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
        if self.tab.pane(pane_id).is_none() {
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

    fn on_focus_previous_pane(
        &mut self,
        _: &FocusPreviousPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane_id) = self.tab.focus_previous_pane() else {
            return;
        };
        self.finish_keyboard_focus_change(pane_id, window, cx);
    }

    fn on_focus_next_pane(
        &mut self,
        _: &FocusNextPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane_id) = self.tab.focus_next_pane() else {
            return;
        };
        self.finish_keyboard_focus_change(pane_id, window, cx);
    }

    fn on_toggle_zoom(&mut self, _: &TogglePaneZoom, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_zoom(window, cx);
    }

    fn on_close_pane(&mut self, _: &ClosePane, _: &mut Window, cx: &mut Context<Self>) {
        self.request_close_pane(self.tab.focused_pane_id(), cx);
    }

    fn render_tree(
        &self,
        tree: PaneTreeRef<'_>,
        hovers: &PaneHovers,
        view: gpui::WeakEntity<Self>,
        appearance: &std::sync::Arc<super::appearance::ChromeAppearance>,
        cx: &App,
    ) -> AnyElement {
        match tree.node() {
            PaneNodeRef::Leaf { pane_id } => {
                self.render_leaf(pane_id, hovers, view, appearance, cx)
            }
            PaneNodeRef::Split {
                split_id,
                axis,
                ratio,
                first,
                second,
            } => self.render_split(
                split_id,
                axis,
                ratio,
                (first, second),
                hovers,
                view,
                appearance,
                cx,
            ),
        }
    }

    /// Each Pane's hover, read once per frame.
    fn pane_hovers(&self, window: &mut Window, cx: &mut App) -> PaneHovers {
        self.tab
            .panes_with_ids()
            .map(|(pane_id, _)| {
                let fade = HoverFade::new(("pane-hover", pane_id.get()), window, cx);
                let level = fade.level(window, cx);
                (pane_id, (fade, level))
            })
            .collect()
    }

    fn render_leaf(
        &self,
        pane_id: PaneId,
        hovers: &PaneHovers,
        view: gpui::WeakEntity<Self>,
        appearance: &std::sync::Arc<super::appearance::ChromeAppearance>,
        cx: &App,
    ) -> AnyElement {
        let Some(terminal) = self.tab.pane(pane_id).cloned() else {
            // A Pane without its Terminal Session still occupies a Pane's place, so it keeps a
            // Pane's material rather than punching an opaque block through a translucent window.
            return div()
                .size_full()
                .bg(gpui_color(appearance.surface(
                    crate::appearance::SurfaceRole::Surface,
                    appearance.colors.background,
                )))
                .into_any_element();
        };
        let focused = self.tab.focused_pane_id() == pane_id;
        let has_multiple_panes = self.tab.pane_count() > 1;
        let zoomed = matches!(self.tab.zoom_state(), ZoomState::Zoomed(_));
        let text = self
            .pane_captions
            .get(&pane_id)
            .cloned()
            .unwrap_or_default();
        let hover = hovers.get(&pane_id).cloned();
        let hover_level = hover.as_ref().map_or(0.0, |(_, level)| *level);
        let attention = self.pane_attention.get(&pane_id).copied().unwrap_or(0) > 0;
        let drop_edge = self
            .pane_drag
            .as_ref()
            .and_then(|drag| drag.drop_target)
            .and_then(|(target_pane_id, edge)| (target_pane_id == pane_id).then_some(edge));
        let measure_view = view.clone();
        let focus_view = view.clone();
        let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
        // The Pane interior stays Terminal-owned; only the surface shape comes from the frame.
        let surface_terminal = terminal.clone();
        let surface_appearance = appearance.clone();
        let radius = frame.pane_radius();
        let rim_width = frame.pane_rim_width();
        let pane_rim = {
            let rim_terminal = terminal.clone();
            let rim_appearance = appearance.clone();
            gpui::canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    let rim =
                        rim_appearance.pane_rim_on(rim_terminal.read(cx).surface_background());
                    window.paint_quad(
                        gpui::outline(bounds, gpui_color(rim), gpui::BorderStyle::Solid)
                            .border_widths(rim_width)
                            .corner_radii(radius),
                    );
                },
            )
            .absolute()
            .inset_0()
            .into_any_element()
        };

        div()
            .on_children_prepainted(move |children, _, cx| {
                let Some(first) = children.get(1) else {
                    return;
                };
                let bounds = children
                    .get(2)
                    .map_or(*first, |terminal| first.union(terminal));
                let _ = measure_view.update(cx, |view, _| {
                    view.pane_bounds.insert(pane_id, bounds);
                });
            })
            .id(("pane", pane_id.get()))
            .debug_selector(move || format!("pane-surface-{}", pane_id.get()))
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .capture_any_mouse_down(move |_: &MouseDownEvent, _, cx| {
                let _ = focus_view.update(cx, |view, cx| view.focus_pane(pane_id, cx));
            })
            .rounded(frame.pane_radius())
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        // Read the surface at paint time because the terminal chooses its
                        // presentation during prepaint.
                        let color = surface_appearance
                            .pane_surface(surface_terminal.read(cx).surface_background());
                        window
                            .paint_quad(gpui::fill(bounds, gpui_color(color)).corner_radii(radius));
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(render_pane_caption(
                PaneCaption {
                    pane_id,
                    terminal: terminal.clone(),
                    text,
                    focused,
                    zoomed,
                    attention,
                    has_multiple_panes,
                },
                hover_level,
                view.clone(),
                appearance.clone(),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(terminal),
            )
            .child(render_pane_corner_surface(pane_id, frame, appearance))
            // The hairline paints last so the caption, Terminal and corner mask never cover it.
            .child(pane_rim)
            .when_some(drop_edge, |leaf, edge| {
                leaf.child(render_pane_drop_target(pane_id, edge, radius, appearance))
            })
            .when_some(hover, |leaf, (fade, _)| leaf.child(fade.tracker()))
            .into_any_element()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one recursive render step over the Split node, its TabView, and the frame inputs"
    )]
    fn render_split(
        &self,
        split_id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        children: (PaneTreeRef<'_>, PaneTreeRef<'_>),
        hovers: &PaneHovers,
        view: gpui::WeakEntity<Self>,
        appearance: &std::sync::Arc<super::appearance::ChromeAppearance>,
        cx: &App,
    ) -> AnyElement {
        let (first, second) = children;
        let first = self.render_tree(first, hovers, view.clone(), appearance, cx);
        let second = self.render_tree(second, hovers, view.clone(), appearance, cx);
        let measure_view = view.clone();
        let mut split = div()
            .relative()
            .on_children_prepainted(move |children, _, cx| {
                let (Some(first), Some(last)) = (children.first(), children.get(2)) else {
                    return;
                };
                let bounds = first.union(last);
                let _ = measure_view.update(cx, |view, cx| {
                    if view.split_bounds.insert(split_id, bounds) != Some(bounds) {
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

        let gap = pane_gap(cx);
        let current_offset = self
            .split_bounds
            .get(&split_id)
            .and_then(|bounds| split_content_extent(axis, *bounds, gap))
            .map_or(0.0, |extent| extent * ratio);

        // The resize target paints after both Panes but before sibling popovers; a deferred target
        // would paint through command palettes.
        let resize_target = div()
            .id(("split-gap", split_id.get()))
            .debug_selector(move || format!("split-gap-{}", split_id.get()))
            .absolute()
            .flex()
            .justify_center()
            .child(render_split_resize_handle(
                split_id,
                axis,
                current_offset,
                view,
            ));
        let (spacer, resize_target) = match axis {
            SplitAxis::Horizontal => (
                div().w(px(gap)).h_full().flex_shrink_0(),
                resize_target
                    .flex_row()
                    .top_0()
                    .bottom_0()
                    .w(px(gap))
                    .left(relative(ratio))
                    .ml(px(-ratio * gap)),
            ),
            SplitAxis::Vertical => (
                div().h(px(gap)).w_full().flex_shrink_0(),
                resize_target
                    .flex_col()
                    .left_0()
                    .right_0()
                    .h(px(gap))
                    .top(relative(ratio))
                    .mt(px(-ratio * gap)),
            ),
        };
        split
            .child(split_child(first, axis, ratio))
            .child(spacer.bg(gpui_color(appearance.surface(
                crate::appearance::SurfaceRole::Base,
                super::workspace_frame::base_surface(&appearance.colors),
            ))))
            .child(split_child(second, axis, 1.0 - ratio))
            .child(resize_target)
            .into_any_element()
    }
}

impl EventEmitter<TabViewEvent> for TabView {}
impl EventEmitter<RemoteChildLaunchUnavailable> for TabView {}

fn collect_pane_order(tree: PaneTreeRef<'_>, panes: &mut Vec<PaneId>) {
    match tree.node() {
        PaneNodeRef::Leaf { pane_id } => panes.push(pane_id),
        PaneNodeRef::Split { first, second, .. } => {
            collect_pane_order(first, panes);
            collect_pane_order(second, panes);
        }
    }
}

impl Render for TabView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A drag GPUI no longer carries ended without this Tab seeing the release, such as while
        // the Tab was hidden, so it moves nothing.
        if self
            .pane_drag
            .as_ref()
            .is_some_and(|drag| !drag.session.is_active(cx))
        {
            self.pane_drag = None;
        }
        let appearance = super::appearance::shared_chrome(cx);
        let radius =
            super::workspace_frame::WorkspaceFrame::for_appearance(&appearance, cx).pane_radius();
        if let Ok(minimum) = PaneSize::new(
            minimum_pane_width(&appearance),
            f32::from(appearance.caption_height() + radius) + 4.0,
        ) {
            self.tab.set_minimum_pane_size(minimum);
        }
        self.sync_terminal_focus(cx);
        let view = cx.entity().downgrade();
        let zoom_state = self.tab.zoom_state();
        let minimum_size = match zoom_state {
            ZoomState::Zoomed(_) => self.tab.minimum_pane_size(),
            ZoomState::Restored => match self.tab.minimum_size(pane_gap(cx)) {
                Ok(size) => size,
                Err(error) => {
                    eprintln!("failed to calculate minimum Pane layout size: {error}");
                    self.tab.minimum_pane_size()
                }
            },
        };
        let hovers = self.pane_hovers(window, cx);
        let content = match zoom_state {
            ZoomState::Restored => {
                self.render_tree(self.tab.root(), &hovers, view.clone(), &appearance, cx)
            }
            ZoomState::Zoomed(pane_id) => self.render_leaf(pane_id, &hovers, view, &appearance, cx),
        };

        div()
            .on_children_prepainted({
                let view = cx.entity().downgrade();
                move |children, _, cx| {
                    let Some(bounds) = children.first() else {
                        return;
                    };
                    let _ = view.update(cx, |view, _| {
                        view.pane_layout_size = pane_size(*bounds).ok();
                    });
                }
            })
            .id(("tab-view", self.tab.id().get()))
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w(px(minimum_size.width()))
            .min_h(px(minimum_size.height()))
            .overflow_hidden()
            .font(appearance.typography.style(TextRole::Body).font.clone())
            // Leaves, corner fillets and Split spacers each own their single surface fill.
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_focus_pane_left))
            .on_action(cx.listener(Self::on_focus_pane_right))
            .on_action(cx.listener(Self::on_focus_pane_up))
            .on_action(cx.listener(Self::on_focus_pane_down))
            .on_action(cx.listener(Self::on_focus_previous_pane))
            .on_action(cx.listener(Self::on_focus_next_pane))
            .on_action(cx.listener(Self::on_toggle_zoom))
            .on_action(cx.listener(Self::on_close_pane))
            .on_drag_move::<DraggedPane>({
                let view = cx.entity().downgrade();
                let owner = cx.entity_id();
                move |event, _, cx| {
                    let dragged = event.drag(cx);
                    if dragged.owner != owner {
                        return;
                    }
                    let pane_id = dragged.pane_id;
                    let pointer = event.event.position;
                    let _ = view.update(cx, |view, cx| view.drag_pane_to(pane_id, pointer, cx));
                }
            })
            .child(content)
            .child({
                let view = cx.entity().downgrade();
                drag_release_observer(move |window, cx| {
                    let pointer = window.mouse_position();
                    let _ = view.update(cx, |view, cx| view.finish_pane_drag(pointer, window, cx));
                })
            })
    }
}

/// What one Tab presents about the Terminal Session its Focused Pane runs. A Tab never composes a
/// path or interprets a title, and leaves a segment empty rather than inventing one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TabIdentity {
    /// Local or Remote, as the Terminal Session classified itself.
    pub(crate) remote: bool,
    /// The OSC 9;4 status the Terminal Session last reported, independent of its title.
    pub(crate) progress: TerminalProgress,
    /// What the Terminal Session is doing: its Terminal title or its running command.
    pub(crate) activity: gpui::SharedString,
    /// The Current Directory leaf that places the Terminal Session, such as `~` or its project.
    pub(crate) place: gpui::SharedString,
    /// The glyph the program reported for itself, replacing the Terminal Session's own.
    pub(crate) glyph: Option<gpui::SharedString>,
    /// Whether any Pane in the Tab is asking for attention.
    pub(crate) attention: bool,
}

impl TabIdentity {
    fn resolve(caption: PaneCaptionText, title: gpui::SharedString, attention: bool) -> Self {
        // A caption without a label has no directory, and its `name` is already what the Terminal
        // Session is doing. Otherwise `name` is the directory leaf and `label` the activity.
        let (activity, place) = if caption.label.is_empty() {
            (caption.name, gpui::SharedString::default())
        } else {
            (caption.label, caption.name)
        };
        let activity = if activity.is_empty() { title } else { activity };
        // Saying the same word twice adds nothing, so the activity keeps it.
        let place = if place == activity {
            gpui::SharedString::default()
        } else {
            place
        };
        Self {
            remote: caption.origin.remote,
            progress: caption.progress,
            activity,
            place,
            glyph: caption.glyph,
            attention,
        }
    }

    /// The identity as one line, in the order the Tab paints it.
    #[cfg(test)]
    pub(crate) fn one_line(&self) -> gpui::SharedString {
        let mut line = String::new();
        if self.attention {
            line.push_str("• ");
        }
        if let TerminalProgress::Normal(percent) = self.progress {
            line.push_str(&format!("[{percent}%] "));
        } else if self.progress != TerminalProgress::None {
            line.push_str(&format!("[{:?}] ", self.progress));
        }
        line.push_str(if self.activity.is_empty() {
            "Terminal"
        } else {
            &self.activity
        });
        if !self.place.is_empty() {
            line.push_str(" · ");
            line.push_str(&self.place);
        }
        line.into()
    }
}

/// One Pane caption split into the segments the header renders and drops independently.
#[derive(Clone, Default, Eq, PartialEq)]
struct PaneCaptionText {
    origin: PaneOrigin,
    has_directory: bool,
    directory: gpui::SharedString,
    name: gpui::SharedString,
    label: gpui::SharedString,
    glyph: Option<gpui::SharedString>,
    running: bool,
    progress: TerminalProgress,
}

impl PaneCaptionText {
    fn from_terminal(terminal: &TerminalPane) -> Self {
        Self::from_facts(terminal.caption())
    }

    fn from_facts(facts: super::terminal_pane::PaneCaptionFacts) -> Self {
        if facts.directory.is_empty() {
            return Self {
                origin: facts.origin,
                has_directory: false,
                directory: gpui::SharedString::default(),
                name: facts.label,
                label: gpui::SharedString::default(),
                glyph: facts.glyph,
                running: facts.running,
                progress: facts.progress,
            };
        }
        let (leading, name) = split_directory_leaf(&facts.directory);
        Self {
            origin: facts.origin,
            has_directory: true,
            directory: leading,
            name,
            label: facts.label,
            glyph: facts.glyph,
            running: facts.running,
            progress: facts.progress,
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
    terminal: Entity<TerminalPane>,
    text: PaneCaptionText,
    focused: bool,
    zoomed: bool,
    attention: bool,
    has_multiple_panes: bool,
}

fn render_pane_caption(
    caption: PaneCaption,
    hover: f32,
    view: gpui::WeakEntity<TabView>,
    appearance: std::sync::Arc<super::appearance::ChromeAppearance>,
) -> AnyElement {
    let caption_height = appearance.caption_height();
    // Resolve controls from this frame's actual width, including during split resizing.
    gpui::canvas(
        move |bounds, window, cx| {
            let mut caption = caption;
            #[cfg(feature = "developer-tools")]
            {
                caption.text = super::developer_workbench::caption_fixture(cx)
                    .map(PaneCaptionText::from_facts)
                    .unwrap_or(caption.text);
            }
            caption.text.glyph = drawable_reported_glyph(caption.text.glyph.as_ref(), |glyph| {
                let caption_style = appearance.typography.style(TextRole::Body);
                reported_glyph_is_drawable(glyph, &caption_style.font, caption_style.size, window)
            });
            let background = caption.terminal.read(cx).surface_background();
            let pane_radius =
                super::workspace_frame::WorkspaceFrame::for_appearance(&appearance, cx)
                    .pane_radius();
            let pane_id = caption.pane_id;
            let mut paint = appearance.colors.caption(background, caption.focused);
            // Caption buttons already sit on the Pane. Add only their state color difference;
            // repeating the Terminal background here would leave opaque squares on the glass.
            for control in [
                &mut paint.control,
                &mut paint.control_hover,
                &mut paint.control_pressed,
                &mut paint.control_disabled,
            ] {
                control.background = appearance.materials.paint(
                    crate::appearance::SurfaceRole::Surface,
                    background,
                    control.background,
                );
            }
            let layout = CaptionLayout::resolve(&caption, bounds.size.width, window, &appearance);
            let content = render_pane_caption_content(
                caption,
                hover,
                view,
                crate::desktop_profile::DesktopPresentation::get(cx),
                layout,
                &appearance,
                paint,
            );
            // The caption shares the terminal surface, but its contents remain Chrome-owned.
            let mut content = div()
                .debug_selector(move || {
                    format!(
                        "pane-caption-surface-{}-{:08x}",
                        pane_id.get(),
                        background.rgba_hex()
                    )
                })
                .size_full()
                // The caption keeps the full height of its strip as a hit target, and its contents
                // ride the middle of that strip whatever the Pane's own height turns out to be.
                .flex()
                .items_center()
                // The caption is the top of the floating Pane, so its surface carries the Pane's
                // own top corners rather than painting a square edge over them.
                .rounded_tl(pane_radius)
                .rounded_tr(pane_radius)
                .child(content)
                .into_any_element();
            content.layout_as_root(bounds.size.map(gpui::AvailableSpace::Definite), window, cx);
            content.prepaint_at(bounds.origin, window, cx);
            content
        },
        |_, mut content, window, cx| content.paint(window, cx),
    )
    .w_full()
    .h(caption_height)
    .flex_shrink_0()
    .into_any_element()
}

pub(super) fn drawable_reported_glyph(
    glyph: Option<&gpui::SharedString>,
    supports: impl FnOnce(&str) -> bool,
) -> Option<gpui::SharedString> {
    glyph.filter(|glyph| supports(glyph)).cloned()
}

fn render_pane_caption_content(
    caption: PaneCaption,
    hover: f32,
    view: gpui::WeakEntity<TabView>,
    presentation: &crate::desktop_profile::DesktopPresentation,
    layout: CaptionLayout,
    appearance: &super::appearance::ChromeAppearance,
    paint: crate::appearance::CaptionPaint,
) -> AnyElement {
    let PaneCaption {
        pane_id,
        terminal: _,
        text,
        focused,
        zoomed,
        attention,
        has_multiple_panes,
    } = caption;
    let color = paint.foreground;
    let button_paint = |value: crate::appearance::SemanticPaint| {
        spaceterm_ui::ButtonPaint::new(
            gpui_color(value.background),
            gpui_color(value.foreground),
            gpui_color(value.border),
        )
        .icon_foreground(gpui_color(value.icon))
    };
    let control_style = spaceterm_ui::ButtonVariantStyle::new(
        button_paint(paint.control),
        button_paint(paint.control_hover),
        button_paint(paint.control_pressed),
        button_paint(paint.control_disabled),
    );
    // The native Window may become active before GPUI dispatches its accepts-first-mouse event, so
    // an opacity-zero unfocused Pane control is disabled before pointer-down to avoid its stale
    // hitbox.
    let caption_action_available = focused || appearance.active;
    let focus_view = view.clone();
    let drag_view = view.clone();
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
        .gap(appearance.spacing(PANE_CONTROL_GAP))
        .ml(appearance.spacing(PANE_CONTROL_LEADING_GAP))
        .flex_shrink_0()
        // An unfocused Pane shows its controls only under the pointer.
        .when(!focused, |controls| {
            controls.opacity(if appearance.active { hover } else { 0.0 })
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
        let view = view.clone();
        let id = format!("pane-{selector}-{}", pane_id.get());
        let icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        let button = IconButton::new(
            gpui::SharedString::from(id.clone()),
            name,
            move |foreground| Icon::new(icon, icon_size, foreground).into_any_element(),
        )
        .variant(ButtonVariant::Bare)
        .disabled(!caption_action_available)
        .contextual_style(control_style, gpui_color(paint.focus))
        .size(ButtonSize::Compact)
        .preserve_ancestor_hover()
        .debug_selector(id.clone())
        .tooltip(
            Tooltip::new(gpui::SharedString::from(format!("{id}-tooltip")), name)
                .shortcut(shortcut.unwrap_or_default()),
        )
        .on_activate(move |_, window, cx| {
            if !caption_action_available {
                return;
            }
            let _ = view.update(cx, |view, cx| {
                view.perform_caption_action(action, pane_id, window, cx);
            });
        });
        controls = if matches!(action, PaneCaptionAction::Close) {
            controls.child(
                div()
                    .ml(-appearance.spacing(PANE_CLOSE_OPTICAL_TRIM))
                    .child(button),
            )
        } else {
            controls.child(button)
        };
    }
    let caption_content =
        render_caption_identity(pane_id, text, attention, layout, appearance, &paint);
    caption_row(appearance, color)
        .id(("pane-caption", pane_id.get()))
        .debug_selector(move || {
            format!(
                "pane-caption-{}-{}",
                pane_id.get(),
                if focused { "focused" } else { "unfocused" }
            )
        })
        // The press focuses the Pane at once, since a press that starts a drag never clicks.
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
            let _ = focus_view.update(cx, |view, cx| {
                view.focus_pane(pane_id, cx);
                view.focus(window, cx);
            });
            cx.stop_propagation();
        })
        // The caption carries its Pane to another Pane's edge whenever another Pane is visible.
        .when(has_multiple_panes && !zoomed, |row| {
            let owner = drag_view.entity_id();
            row.on_drag(DraggedPane { pane_id, owner }, move |_, _, window, cx| {
                let preview = drag_view
                    .update(cx, |view, cx| view.begin_pane_drag(pane_id, window, cx))
                    .unwrap_or_else(|_| DragPreview::empty());
                cx.new(|_| preview)
            })
        })
        .child(caption_content)
        .child(controls)
        .into_any_element()
}

/// The Pane's identity as its caption shows it: origin, directory, name, status, and label.
fn render_caption_identity(
    pane_id: PaneId,
    text: PaneCaptionText,
    attention: bool,
    layout: CaptionLayout,
    appearance: &super::appearance::ChromeAppearance,
    paint: &crate::appearance::CaptionPaint,
) -> gpui::Div {
    let color = paint.foreground;
    let mut caption_content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .child(render_pane_origin(
            pane_id,
            &text.origin,
            layout,
            color,
            appearance,
        ));
    if text.has_directory {
        caption_content = caption_content
            .when(layout.show_directory && !text.directory.is_empty(), |row| {
                row.child(
                    div()
                        .debug_selector(move || format!("pane-caption-directory-{}", pane_id.get()))
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
            .when(layout.show_status_separator, |row| {
                row.child(
                    div()
                        .debug_selector(move || {
                            format!("pane-caption-status-separator-{}", pane_id.get())
                        })
                        .flex_shrink_0()
                        .mx(appearance.spacing(5.0))
                        .text_color(gpui_color(color))
                        .child("·"),
                )
            })
            .child(render_pane_status(
                pane_id,
                (text.progress, text.glyph, attention),
                paint,
                appearance,
            ))
            .when(layout.show_label && !text.label.is_empty(), |row| {
                row.child(
                    div()
                        .debug_selector(move || format!("pane-caption-label-{}", pane_id.get()))
                        .min_w_0()
                        .truncate()
                        .text_color(gpui_color(color))
                        .child(text.label),
                )
            });
    } else {
        caption_content = caption_content
            .child(render_pane_status(
                pane_id,
                (text.progress, text.glyph, attention),
                paint,
                appearance,
            ))
            .child(
                div()
                    .debug_selector(move || format!("pane-caption-name-{}", pane_id.get()))
                    .min_w_0()
                    .truncate()
                    .child(text.name),
            );
    }
    caption_content
}

/// The caption strip's row: full height, padded, and set in the caption's text style.
fn caption_row(
    appearance: &super::appearance::ChromeAppearance,
    color: crate::appearance::Color,
) -> gpui::Div {
    div()
        // The row fills the caption strip rather than restating its height, so the contents centre
        // on the strip's own middle and the whole strip stays one hit target.
        .h_full()
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .pl(appearance.spacing(PANE_CAPTION_LEFT_PADDING))
        .pr(appearance.spacing(PANE_CAPTION_RIGHT_PADDING))
        .py(appearance.spacing(PANE_CAPTION_VERTICAL_PADDING))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(color))
}

/// Renders the account and machine a Pane runs on, ahead of the directory it sits in.
fn render_pane_origin(
    pane_id: PaneId,
    origin: &PaneOrigin,
    layout: CaptionLayout,
    color: crate::appearance::Color,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    let location = if origin.remote { "remote" } else { "local" };
    div()
        .debug_selector(move || format!("pane-caption-origin-{}-{location}", pane_id.get()))
        .flex()
        .items_center()
        .flex_shrink_0()
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
                    .mx(appearance.spacing(5.0))
                    .text_color(gpui_color(color))
                    .child("›"),
            )
        })
        .into_any_element()
}

/// Renders the status after the Pane's directory and before its activity.
fn render_pane_status(
    pane_id: PaneId,
    (progress, reported, attention): (TerminalProgress, Option<gpui::SharedString>, bool),
    paint: &crate::appearance::CaptionPaint,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    div()
        .mr(appearance.spacing(PANE_STATUS_ICON_GAP))
        .flex()
        .items_center()
        .flex_shrink_0()
        .text_color(gpui_color(paint.foreground))
        .child(
            StatusGlyph {
                icon: IconName::Terminal,
                reported,
                size: appearance.icons.metrics(IconRole::Status).glyph_size,
                progress,
                attention,
                id: ("pane-status", pane_id.get()).into(),
                selector_prefix: format!("pane-status-{}", pane_id.get()),
                colors: StatusColors {
                    host: gpui_color(paint.background),
                    attention: gpui_color(paint.attention),
                    busy: gpui_color(paint.busy),
                    error: gpui_color(paint.error),
                    paused: gpui_color(paint.secondary),
                },
                differentiate_without_color: appearance.capabilities.differentiate_without_color,
            }
            .render(),
        )
        .into_any_element()
}

/// Masks the Pane's corner fillets with Chrome while leaving its rounded interior visible.
fn render_pane_corner_surface(
    pane_id: PaneId,
    frame: super::workspace_frame::WorkspaceFrame,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    let radius = frame.pane_radius();
    // Any width reaching past the corner fillet works; the radius itself always does.
    let width = radius;
    let base = appearance.surface(
        crate::appearance::SurfaceRole::Base,
        super::workspace_frame::base_surface(&appearance.colors),
    );
    div()
        .debug_selector(move || {
            format!(
                "pane-corner-mask-{}-{}-{:08x}",
                pane_id.get(),
                f32::from(radius),
                base.rgba_hex()
            )
        })
        .absolute()
        .top(-width)
        .left(-width)
        .right(-width)
        .bottom(-width)
        .border(width)
        .border_color(gpui_color(base))
        .rounded(px(concentric_outset(f32::from(radius), f32::from(width))))
        .into_any_element()
}

/// Shows the half of a target Pane that a dragged Pane takes when it is released.
fn render_pane_drop_target(
    pane_id: PaneId,
    edge: PaneEdge,
    radius: Pixels,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    let accent = appearance.colors.primary_background;
    let half = div()
        .debug_selector(move || {
            format!("pane-drop-target-{}-{edge:?}", pane_id.get()).to_lowercase()
        })
        .absolute()
        .bg(gpui_color(
            accent.multiply_opacity(PANE_DROP_TARGET_FILL_OPACITY),
        ))
        .border_2()
        .border_color(gpui_color(accent))
        .rounded(radius);
    match edge {
        PaneEdge::Left => half.top_0().bottom_0().left_0().w(relative(0.5)),
        PaneEdge::Right => half.top_0().bottom_0().right_0().w(relative(0.5)),
        PaneEdge::Top => half.left_0().right_0().top_0().h(relative(0.5)),
        PaneEdge::Bottom => half.left_0().right_0().bottom_0().h(relative(0.5)),
    }
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

/// The Split's resize interaction, owned by the empty gap between its Panes.
fn render_split_resize_handle(
    split_id: SplitId,
    axis: SplitAxis,
    current_offset: f32,
    view: gpui::WeakEntity<TabView>,
) -> AnyElement {
    ResizeHandle::new(
        ("split-resize", split_id.get()),
        "Resize Pane split",
        match axis {
            SplitAxis::Horizontal => ResizeAxis::Horizontal,
            SplitAxis::Vertical => ResizeAxis::Vertical,
        },
        current_offset,
    )
    .tab_stop(true)
    .reset_on_double_click(true)
    .paint_divider(false)
    .debug_selector(format!("split-resize-{}", split_id.get()))
    .on_event_with_accepted_value(move |event, window, cx| {
        let event = *event;
        view.update(cx, |view, cx| {
            view.handle_resize_event(split_id, axis, event, window, cx)
        })
        .ok()
        .flatten()
    })
    .into_any_element()
}

// Splitting restores the grid, so a zoomed Pane must use its allocation in that grid.
fn restored_leaf_size(
    tree: PaneTreeRef<'_>,
    target: PaneId,
    available: PaneSize,
    gap: f32,
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
                    SplitAxis::Horizontal => {
                        PaneSize::new((available.width() - gap) * fraction, available.height())
                    }
                    SplitAxis::Vertical => {
                        PaneSize::new(available.width(), (available.height() - gap) * fraction)
                    }
                }
                .ok()
            };
            restored_leaf_size(first, target, child_size(ratio)?, gap)
                .or_else(|| restored_leaf_size(second, target, child_size(1.0 - ratio)?, gap))
        }
    }
}

/// The edge of a Pane nearest `point`, by the four triangles its diagonals divide it into.
fn drop_edge(bounds: Bounds<Pixels>, point: Point<Pixels>) -> PaneEdge {
    let center = bounds.center();
    let horizontal = f32::from(point.x - center.x) / f32::from(bounds.size.width).max(f32::EPSILON);
    let vertical = f32::from(point.y - center.y) / f32::from(bounds.size.height).max(f32::EPSILON);
    match (
        horizontal.abs() > vertical.abs(),
        horizontal < 0.0,
        vertical < 0.0,
    ) {
        (true, true, _) => PaneEdge::Left,
        (true, false, _) => PaneEdge::Right,
        (false, _, true) => PaneEdge::Top,
        (false, _, false) => PaneEdge::Bottom,
    }
}

fn pane_size(bounds: Bounds<Pixels>) -> Result<PaneSize, crate::domain::PaneSizeError> {
    PaneSize::new(f32::from(bounds.size.width), f32::from(bounds.size.height))
}

/// The extent a Split shares between its two children once its gap is reserved.
fn split_content_extent(axis: SplitAxis, bounds: Bounds<Pixels>, gap: f32) -> Option<f32> {
    let extent = match axis {
        SplitAxis::Horizontal => f32::from(bounds.size.width),
        SplitAxis::Vertical => f32::from(bounds.size.height),
    } - gap;
    (extent > 0.0).then_some(extent)
}

fn split_ratio_for_offset(
    axis: SplitAxis,
    bounds: Bounds<Pixels>,
    requested_offset: f32,
    gap: f32,
) -> Option<f32> {
    let content_extent = split_content_extent(axis, bounds, gap)?;
    requested_offset
        .is_finite()
        .then_some(requested_offset / content_extent)
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
    use crate::appearance::Color;
    use crate::ssh::command::ValidatedRemoteShellCommand;
    use crate::terminal::testing::{
        RecordedCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{
        ScreenSnapshot, ScrollbarSnapshot, TerminalSessionChannelProvider,
        TerminalSessionChannelRevalidationError, TerminalSessionChannelUnavailable,
        TerminalSessionEvent, TerminalSessionExit, TerminalSessionFactory,
    };
    use crate::ui::RemoteChildLaunchUnavailable;

    fn prepared_appearance(
        appearance: crate::appearance::Appearance,
        transparency: f32,
    ) -> super::super::appearance::ChromeAppearance {
        let mut preferences = crate::appearance::AppearancePreferences {
            mode: match appearance {
                crate::appearance::Appearance::Light => crate::appearance::AppearanceMode::Light,
                crate::appearance::Appearance::Dark => crate::appearance::AppearanceMode::Dark,
            },
            ..crate::appearance::AppearancePreferences::default()
        };
        preferences.window.transparency = transparency;
        let resolved = crate::appearance::ThemeCatalog::default()
            .resolve(
                crate::appearance::AppearanceGeneration::INITIAL,
                &preferences,
                crate::appearance::SystemAppearance::available(appearance)
                    .with_composition(crate::appearance::CompositionCapabilities::new(true, true)),
                &crate::appearance::AvailableFonts::default(),
            )
            .expect("built-in appearance should resolve");
        super::super::appearance::ChromeAppearance::prepare(&resolved.chrome)
    }

    #[test]
    fn light_pane_rim_uses_the_accepted_terminal_surface_as_its_host() {
        for transparency in [0.0, 0.35, 1.0] {
            let appearance =
                prepared_appearance(crate::appearance::Appearance::Light, transparency);
            let window = appearance.control_host_background(spaceterm_ui::ControlHost::Window);
            for terminal in [Color::BLACK, Color::rgb(0xfafafa), Color::rgb(0x38658a)] {
                let host = appearance.pane_surface(terminal).source_over(window);
                let edge = appearance.pane_rim_on(terminal).source_over(host);
                let contrast = edge.contrast_ratio(host);
                assert!(
                    (1.20..=1.30).contains(&contrast),
                    "Light Pane edge {edge:?} must remain in its surface band on {host:?} at transparency {transparency}"
                );
            }
        }
    }

    #[test]
    fn dark_pane_rim_uses_the_accepted_terminal_surface_as_its_host() {
        for transparency in [0.0, 0.35, 1.0] {
            let appearance = prepared_appearance(crate::appearance::Appearance::Dark, transparency);
            for terminal in [Color::BLACK, Color::rgb(0xfafafa), Color::rgb(0x38658a)] {
                let window = appearance.control_host_background(spaceterm_ui::ControlHost::Window);
                let host = appearance.pane_surface(terminal).source_over(window);
                let edge = appearance.pane_rim_on(terminal).source_over(host);
                assert!((1.25..=1.50).contains(&edge.contrast_ratio(host)));
            }
        }
    }

    struct RemoteLaunchEventHarness {
        view: Entity<TabView>,
        events: Rc<RefCell<Vec<RemoteChildLaunchUnavailable>>>,
    }

    impl Render for RemoteLaunchEventHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    struct RevalidatingTerminalSessionChannelProvider {
        grant: AtomicBool,
        revalidations: AtomicUsize,
        preparations: AtomicUsize,
        revalidation_error: Mutex<Option<TerminalSessionChannelRevalidationError>>,
        command_context: crate::ssh::testing::SshConnectionFixture,
    }

    impl RevalidatingTerminalSessionChannelProvider {
        fn new(destination: crate::domain::SshDestination) -> Self {
            Self {
                grant: AtomicBool::new(true),
                revalidations: AtomicUsize::new(0),
                preparations: AtomicUsize::new(0),
                revalidation_error: Mutex::new(None),
                command_context: crate::ssh::testing::SshConnectionFixture::new(destination),
            }
        }

        fn fail_revalidation_with(&self, error: Option<TerminalSessionChannelRevalidationError>) {
            *self.revalidation_error.lock().unwrap() = error;
        }
    }

    impl TerminalSessionChannelProvider for RevalidatingTerminalSessionChannelProvider {
        fn is_ready(&self) -> bool {
            true
        }

        fn revalidate(
            &self,
            _directory: crate::domain::RemoteDirectory,
            _expected_identity: Option<crate::domain::RemoteDirectoryIdentity>,
        ) -> gpui::Task<Result<(), TerminalSessionChannelRevalidationError>> {
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
        ) -> Result<
            crate::ssh::command::PreparedSshTerminalSessionChannelCommand,
            TerminalSessionChannelUnavailable,
        > {
            if !self.grant.swap(false, Ordering::AcqRel) {
                return Err(TerminalSessionChannelUnavailable);
            }
            self.preparations.fetch_add(1, Ordering::AcqRel);
            Ok(self.command_context.prepare_terminal_session_channel(
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
        let command_context = Arc::new(crate::ssh::testing::SshConnectionFixture::new(
            destination.clone(),
        ));
        remote_test_session_factory_with_provider(
            records,
            destination,
            Arc::new(move || {
                Ok(command_context.prepare_terminal_session_channel(
                    ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
                ))
            }),
        )
    }

    fn remote_test_session_factory_with_provider(
        records: TestTerminalSessionRecords,
        destination: crate::domain::SshDestination,
        provider: Arc<dyn TerminalSessionChannelProvider>,
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
        view: &Entity<TabView>,
        pane_id: PaneId,
        axis: SplitAxis,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_pane(pane_id, axis, window, cx);
            });
        });
        cx.run_until_parked();
    }

    fn remote_view_with_events(
        session_factory: WorkspaceTerminalSessionFactory,
        cx: &mut TestAppContext,
    ) -> (
        Entity<TabView>,
        Rc<RefCell<Vec<RemoteChildLaunchUnavailable>>>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TabView::new(TabId::new(1), session_factory, window, cx));
            cx.subscribe(
                &view,
                move |_, _, event: &RemoteChildLaunchUnavailable, _| {
                    recorded_events.borrow_mut().push(*event);
                },
            )
            .detach();
            RemoteLaunchEventHarness { view, events }
        });
        let (view, events) = harness.read_with(cx, |harness, _| {
            (harness.view.clone(), Rc::clone(&harness.events))
        });
        cx.run_until_parked();
        (view, events, cx)
    }

    #[gpui::test]
    fn remote_split_should_emit_each_typed_revalidation_failure_without_mutation(
        cx: &mut TestAppContext,
    ) {
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(RevalidatingTerminalSessionChannelProvider::new(
            destination.clone(),
        ));
        let session_factory = remote_test_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::clone(&provider) as Arc<dyn TerminalSessionChannelProvider>,
        );
        let (view, events, cx) = remote_view_with_events(session_factory, cx);
        let before = view.read_with(cx, |view, _| {
            (
                view.pane_count(),
                view.focused_pane_id(),
                view.layout_signature(),
            )
        });

        for error in [
            TerminalSessionChannelRevalidationError::ConnectionUnavailable,
            TerminalSessionChannelRevalidationError::DirectoryUnavailable,
            TerminalSessionChannelRevalidationError::IdentityChanged,
        ] {
            provider.fail_revalidation_with(Some(error));
            split_test_pane(&view, before.1, SplitAxis::Horizontal, cx);
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
            view.read_with(cx, |view, _| {
                (
                    view.pane_count(),
                    view.focused_pane_id(),
                    view.layout_signature(),
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
        let provider = Arc::new(RevalidatingTerminalSessionChannelProvider::new(
            destination.clone(),
        ));
        let session_factory = remote_test_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::clone(&provider) as Arc<dyn TerminalSessionChannelProvider>,
        );
        let (view, events, cx) = remote_view_with_events(session_factory, cx);
        let before = view.read_with(cx, |view, _| {
            (
                view.pane_count(),
                view.focused_pane_id(),
                view.layout_signature(),
            )
        });

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_pane(before.1, SplitAxis::Horizontal, window, cx);
                view.remote_lifecycle.begin_child_launch();
            });
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_pane(before.1, SplitAxis::Horizontal, window, cx);
                view.disconnect_remote(1, cx).unwrap();
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
            view.read_with(cx, |view, _| {
                (
                    view.pane_count(),
                    view.focused_pane_id(),
                    view.layout_signature(),
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();

        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);

        assert_eq!(view.read_with(cx, |view, _| view.pane_count()), 2);
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
        let provider = Arc::new(RevalidatingTerminalSessionChannelProvider::new(
            destination.clone(),
        ));
        let session_factory = remote_test_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::clone(&provider) as Arc<dyn TerminalSessionChannelProvider>,
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        let focused = view.read_with(cx, |view, _| view.focused_pane_id());

        provider.fail_revalidation_with(Some(
            TerminalSessionChannelRevalidationError::IdentityChanged,
        ));
        split_test_pane(&view, focused, SplitAxis::Horizontal, cx);

        assert_eq!(view.read_with(cx, |view, _| view.pane_count()), 1);
        assert_eq!(
            view.read_with(cx, |view, _| view.focused_pane_id()),
            focused
        );
        assert_eq!(records.starts().len(), 1);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 1);
        assert_eq!(provider.revalidations.load(Ordering::Acquire), 1);

        provider.fail_revalidation_with(None);
        split_test_pane(&view, focused, SplitAxis::Horizontal, cx);

        assert_eq!(view.read_with(cx, |view, _| view.pane_count()), 2);
        assert_eq!(records.starts().len(), 2);
        assert_eq!(provider.preparations.load(Ordering::Acquire), 2);
        assert_eq!(provider.revalidations.load(Ordering::Acquire), 2);
    }

    #[gpui::test]
    fn remote_split_should_leave_hierarchy_unchanged_when_terminal_session_channel_reservation_fails(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let command_context = Arc::new(crate::ssh::testing::SshConnectionFixture::new(
            destination.clone(),
        ));
        let preparations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = {
            let preparations = Arc::clone(&preparations);
            Arc::new(move || {
                if preparations.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                    Ok(command_context.prepare_terminal_session_channel(
                        ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
                    ))
                } else {
                    Err(TerminalSessionChannelUnavailable)
                }
            })
        };
        let session_factory =
            remote_test_session_factory_with_provider(records.clone(), destination, provider);
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();

        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);

        assert_eq!(
            view.read_with(cx, |view, _| (view.pane_count(), view.focused_pane_id())),
            (1, PaneId::new(1))
        );
        assert_eq!(records.starts().len(), 1);
        assert_eq!(preparations.load(std::sync::atomic::Ordering::Acquire), 2);
    }

    fn four_pane_view(cx: &mut TestAppContext) -> (Entity<TabView>, &mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));

        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        split_test_pane(&view, PaneId::new(1), SplitAxis::Vertical, cx);
        split_test_pane(&view, PaneId::new(2), SplitAxis::Vertical, cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.focus_pane(PaneId::new(1), cx);
                view.focus(window, cx);
            });
        });
        cx.run_until_parked();

        (view, cx)
    }

    /// Presses a Pane's caption beside its name and moves the pointer along `path`.
    fn drag_pane_caption(
        caption: Bounds<Pixels>,
        path: &[Point<Pixels>],
        cx: &mut VisualTestContext,
    ) {
        let from = point(caption.left() + px(24.0), caption.center().y);
        cx.simulate_mouse_move(from, None, Modifiers::none());
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        for position in path {
            cx.simulate_mouse_move(*position, Some(MouseButton::Left), Modifiers::none());
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn dropping_a_pane_on_an_edge_of_another_should_split_it_there(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);
        let caption = cx.debug_bounds("pane-caption-4-unfocused").unwrap();
        let target = cx.debug_bounds("pane-surface-1").unwrap();
        let near_left_edge = point(target.left() + target.size.width * 0.1, target.center().y);

        drag_pane_caption(
            caption,
            &[near_left_edge, near_left_edge + point(px(1.0), px(0.0))],
            cx,
        );
        let during = (
            cx.debug_bounds("pane-drop-target-1-left")
                .map(|overlay| (overlay.origin, f32::from(overlay.size.width).round())),
            cx.debug_bounds("drag-preview").is_some(),
        );
        cx.simulate_mouse_up(near_left_edge, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            during,
            (
                Some((target.origin, (f32::from(target.size.width) / 2.0).round())),
                true
            )
        );
        assert_eq!(
            view.read_with(cx, |view, _| (
                view.layout_signature(),
                view.focused_pane_id(),
                view.pane_count(),
                view.pane_drag.is_some(),
            )),
            (
                "split:1:Horizontal:0.5(split:2:Vertical:0.5(split:4:Horizontal:0.5(pane:4,pane:1),pane:3),pane:2)"
                    .to_owned(),
                PaneId::new(4),
                4,
                false,
            )
        );
        assert!(cx.debug_bounds("pane-drop-target-1-left").is_none());
    }

    #[gpui::test]
    fn a_pane_drag_should_start_over_a_terminal_painted_after_its_caption(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);
        let caption = cx.debug_bounds("pane-caption-1-focused").unwrap();
        let target = cx.debug_bounds("pane-surface-4").unwrap();
        let near_right_edge = point(target.right() - target.size.width * 0.1, target.center().y);

        drag_pane_caption(
            caption,
            &[near_right_edge, near_right_edge - point(px(1.0), px(0.0))],
            cx,
        );
        cx.simulate_mouse_up(near_right_edge, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |view, _| view.layout_signature()),
            "split:1:Horizontal:0.5(pane:3,split:3:Vertical:0.5(pane:2,split:4:Horizontal:0.5(pane:4,pane:1)))"
        );
    }

    #[gpui::test]
    fn escape_should_cancel_a_pane_drag_without_moving_the_pane(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);
        let before = view.read_with(cx, |view, _| view.layout_signature());
        let caption = cx.debug_bounds("pane-caption-4-unfocused").unwrap();
        let target = cx.debug_bounds("pane-surface-1").unwrap();
        let near_left_edge = point(target.left() + target.size.width * 0.1, target.center().y);

        drag_pane_caption(
            caption,
            &[near_left_edge, near_left_edge + point(px(1.0), px(0.0))],
            cx,
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let cancelled = (
            view.read_with(cx, |view, _| view.pane_drag.is_some()),
            cx.debug_bounds("pane-drop-target-1-left").is_some(),
            cx.debug_bounds("drag-preview").is_some(),
        );
        cx.simulate_mouse_up(near_left_edge, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            (
                cancelled,
                view.read_with(cx, |view, _| view.layout_signature())
            ),
            ((false, false, false), before)
        );
    }

    #[gpui::test]
    fn a_pane_drag_that_ended_unseen_should_move_nothing(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);
        let before = view.read_with(cx, |view, _| view.layout_signature());
        let caption = cx.debug_bounds("pane-caption-4-unfocused").unwrap();
        let target = cx.debug_bounds("pane-surface-1").unwrap();
        let near_left_edge = point(target.left() + target.size.width * 0.1, target.center().y);

        drag_pane_caption(
            caption,
            &[near_left_edge, near_left_edge + point(px(1.0), px(0.0))],
            cx,
        );
        // GPUI ends the drag while this Tab is hidden, so the Tab never sees the release.
        cx.update(|window, cx| {
            cx.stop_active_drag(window);
        });
        cx.run_until_parked();
        let ended = view.read_with(cx, |view, _| view.pane_drag.is_some());
        cx.simulate_click(near_left_edge, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            (ended, view.read_with(cx, |view, _| view.layout_signature())),
            (false, before)
        );
    }

    #[gpui::test]
    fn a_pane_drag_that_moves_nothing_should_leave_its_pane_focused(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);
        let caption = cx.debug_bounds("pane-caption-4-unfocused").unwrap();
        let own_pane = cx.debug_bounds("pane-surface-4").unwrap();

        drag_pane_caption(caption, &[own_pane.center()], cx);
        cx.simulate_mouse_up(own_pane.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let released = cx.update(|window, cx| {
            let view = view.read(cx);
            (
                view.focused_pane_id(),
                view.focused_terminal_is_focused(window, cx),
            )
        });

        let caption = cx.debug_bounds("pane-caption-1-unfocused").unwrap();
        let target = cx.debug_bounds("pane-surface-4").unwrap();
        drag_pane_caption(caption, &[target.center()], cx);
        cx.simulate_keystrokes("escape");
        cx.simulate_mouse_up(target.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let cancelled = cx.update(|window, cx| {
            let view = view.read(cx);
            (
                view.focused_pane_id(),
                view.focused_terminal_is_focused(window, cx),
            )
        });

        assert_eq!(
            (released, cancelled),
            ((PaneId::new(4), true), (PaneId::new(1), true)),
            "the pressed Pane must take keyboard focus even though the drag never clicked"
        );
    }

    #[gpui::test]
    fn releasing_a_pane_over_itself_should_leave_the_layout_unchanged(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);
        let before = view.read_with(cx, |view, _| view.layout_signature());
        let caption = cx.debug_bounds("pane-caption-4-unfocused").unwrap();
        let own_pane = cx.debug_bounds("pane-surface-4").unwrap();

        drag_pane_caption(caption, &[own_pane.center()], cx);
        let offered_target = view.read_with(cx, |view, _| {
            view.pane_drag.as_ref().and_then(|drag| drag.drop_target)
        });
        cx.simulate_mouse_up(own_pane.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            (
                offered_target,
                view.read_with(cx, |view, _| (
                    view.layout_signature(),
                    view.pane_drag.is_some()
                )),
            ),
            (None, (before, false))
        );
    }

    #[test]
    fn drop_edge_should_divide_a_pane_along_its_diagonals() {
        let pane = bounds(point(px(0.0), px(0.0)), size(px(400.0), px(100.0)));

        assert_eq!(
            [
                point(px(60.0), px(50.0)),
                point(px(340.0), px(50.0)),
                point(px(200.0), px(10.0)),
                point(px(200.0), px(90.0)),
                point(px(120.0), px(20.0)),
            ]
            .map(|pointer| drop_edge(pane, pointer)),
            [
                PaneEdge::Left,
                PaneEdge::Right,
                PaneEdge::Top,
                PaneEdge::Bottom,
                PaneEdge::Top,
            ]
        );
    }

    #[gpui::test]
    fn pane_attention_count_should_decorate_its_tab_title(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let (view, cx) = cx.add_window_view(|window, cx| {
            TabView::new(TabId::new(1), test_session_factory(), window, cx)
        });

        view.update(cx, |view, _| {
            view.pane_attention.insert(PaneId::new(1), 2);
        });

        assert_eq!(
            view.read_with(cx, |view, _| view.tab_title()),
            "• Terminal · spaceterm-test-workspace"
        );
        assert_eq!(
            view.read_with(cx, |view, _| view.pane_attention.clone()),
            BTreeMap::from([(PaneId::new(1), 2)])
        );
    }

    fn test_home_directory() -> PathBuf {
        PathBuf::from("/tmp/spaceterm-test-workspace")
    }

    fn caption_text(origin: PaneOrigin, directory: &str, label: &str) -> PaneCaptionText {
        PaneCaptionText::from_facts(super::super::terminal_pane::PaneCaptionFacts {
            origin,
            directory: directory.to_owned().into(),
            label: label.to_owned().into(),
            glyph: None,
            running: false,
            progress: TerminalProgress::None,
        })
    }

    fn local_origin() -> PaneOrigin {
        PaneOrigin {
            user: "tester".into(),
            host: "workstation".into(),
            remote: false,
        }
    }

    fn remote_origin() -> PaneOrigin {
        PaneOrigin {
            user: "tester".into(),
            host: "build-box".into(),
            remote: true,
        }
    }

    /// A Tab restates what its Focused Pane is doing and where, and nothing about who runs it: the
    /// account, the machine, and the Pane count stay in the Pane Caption and the Workspace chrome.
    #[test]
    fn tab_identity_should_present_activity_then_directory_leaf() {
        for (origin, remote) in [(local_origin(), false), (remote_origin(), true)] {
            let mut caption = caption_text(origin, "~/Projects/api", "cargo test");
            caption.progress = TerminalProgress::Normal(40);
            let identity = TabIdentity::resolve(caption, "cargo test".into(), false);
            assert_eq!(
                (
                    identity.remote,
                    identity.progress,
                    identity.activity.as_ref(),
                    identity.place.as_ref(),
                ),
                (remote, TerminalProgress::Normal(40), "cargo test", "api")
            );
            assert_eq!(identity.one_line().as_ref(), "[40%] cargo test · api");
        }
    }

    /// The Tab never says the same word twice and never invents a segment it was not given.
    #[test]
    fn tab_identity_should_drop_empty_and_repeated_segments() {
        // A Pane with no directory carries its label as the thing it is doing, with no place.
        let untitled =
            TabIdentity::resolve(caption_text(local_origin(), "", "zsh"), "zsh".into(), false);
        assert_eq!(
            (untitled.activity.as_ref(), untitled.place.as_ref()),
            ("zsh", "")
        );

        // A title that repeats the directory leaf is presented once.
        let repeated = TabIdentity::resolve(
            caption_text(remote_origin(), "~/services", "services"),
            "services".into(),
            true,
        );
        assert_eq!(repeated.one_line().as_ref(), "• services");

        // Nothing resolved at all still names the Tab rather than leaving it blank.
        assert_eq!(TabIdentity::default().one_line().as_ref(), "Terminal");
    }

    fn focused_panes_after_shortcuts<const N: usize>(
        view: &Entity<TabView>,
        cx: &mut VisualTestContext,
        shortcuts: [&str; N],
    ) -> [PaneId; N] {
        shortcuts.map(|shortcut| {
            cx.simulate_keystrokes(shortcut);
            view.read_with(cx, |view, _| view.tab.focused_pane_id())
        })
    }

    struct CaptionTestView {
        view: Entity<TabView>,
        width: Pixels,
        activity: spaceterm_ui::ControlWindowActivity,
    }

    impl Render for CaptionTestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.activity
                .mount(div().w(self.width).h(px(600.0)).child(self.view.clone()))
        }
    }

    fn caption_view(
        cx: &mut TestAppContext,
    ) -> (
        Entity<CaptionTestView>,
        Entity<TabView>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        caption_view_with_activity(cx, spaceterm_ui::ControlWindowActivity::Active)
    }

    fn caption_view_with_activity(
        cx: &mut TestAppContext,
        activity: spaceterm_ui::ControlWindowActivity,
    ) -> (
        Entity<CaptionTestView>,
        Entity<TabView>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| {
            let active = Arc::new(super::super::appearance::ChromeAppearance::default());
            let mut inactive = (*active).clone();
            inactive.active = false;
            cx.set_global(super::super::appearance::InstalledChrome {
                active,
                inactive: Arc::new(inactive),
            });
        });
        let records = TestTerminalSessionRecords::default();
        let factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let factory = WorkspaceTerminalSessionFactory::new_local(
            factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (root, cx) = cx.add_window_view(|window, cx| CaptionTestView {
            view: cx.new(|cx| TabView::new(TabId::new(1), factory, window, cx)),
            width: px(1000.0),
            activity,
        });
        let view = root.read_with(cx, |root, _| root.view.clone());
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| view.activate(window, cx));
        });
        cx.run_until_parked();
        (root, view, records, cx)
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

    fn first_mouse_click_caption_control(selector: &'static str, cx: &mut VisualTestContext) {
        let control = cx
            .debug_bounds(selector)
            .expect("caption control must exist")
            .center();
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: control,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: true,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position: control,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn inactive_first_mouse_ignores_hidden_caption_actions_but_keeps_focused_actions_available(
        cx: &mut TestAppContext,
    ) {
        let (_, view, _, cx) =
            caption_view_with_activity(cx, spaceterm_ui::ControlWindowActivity::Inactive);

        first_mouse_click_caption_control("pane-split-right-1", cx);
        assert_eq!(
            view.read_with(cx, |view, _| (view.pane_count(), view.focused_pane_id())),
            (2, PaneId::new(2)),
            "the visible focused-Pane action remains available on the activation click",
        );

        first_mouse_click_caption_control("pane-split-down-1", cx);
        assert_eq!(
            view.read_with(cx, |view, _| (view.pane_count(), view.focused_pane_id())),
            (2, PaneId::new(1)),
            "the activation click may focus its Pane but must not invoke its opacity-zero action",
        );
    }

    #[gpui::test]
    fn caption_split_buttons_should_split_their_owner_and_restore_terminal_input(
        cx: &mut TestAppContext,
    ) {
        let (_, view, records, cx) = caption_view(cx);
        assert!(cx.debug_bounds("pane-toggle-zoom-1").is_none());
        assert!(cx.debug_bounds("pane-close-1").is_none());
        click_caption_control("pane-split-right-1", cx);
        click_caption_control("pane-split-down-1", cx);
        assert_eq!(
            view.read_with(cx, |view, _| (view.pane_count(), view.focused_pane_id())),
            (3, PaneId::new(3))
        );
        assert_eq!(records.pointer_count(), 0);
        assert!(
            cx.update(|window, cx| { view.read(cx).focused_terminal_has_input_focus(window, cx) })
        );
        cx.simulate_keystrokes("a");
        assert!(
            records.commands().iter().any(
                |call| call.session_id == 3 && matches!(&call.command, RecordedCommand::Key(_))
            )
        );
    }

    #[gpui::test]
    fn caption_zoom_should_target_hovered_pane_and_split_should_restore_the_layout(
        cx: &mut TestAppContext,
    ) {
        let (_, view, records, cx) = caption_view(cx);
        click_caption_control("pane-split-right-1", cx);
        click_caption_control("pane-toggle-zoom-1", cx);
        assert_eq!(
            view.read_with(cx, |view, _| view.zoom_state()),
            ZoomState::Zoomed(PaneId::new(1))
        );
        assert!(cx.debug_bounds("pane-caption-2-unfocused").is_none());
        click_caption_control("pane-split-down-1", cx);
        assert_eq!(
            view.read_with(cx, |view, _| (view.zoom_state(), view.pane_count())),
            (ZoomState::Restored, 3)
        );
        assert!(cx.debug_bounds("pane-caption-2-unfocused").is_some());
        assert_eq!(records.pointer_count(), 0);
    }

    #[gpui::test]
    fn zoom_then_split_before_repaint_should_use_the_retained_layout_size(cx: &mut TestAppContext) {
        let (root, view, _, cx) = caption_view(cx);
        click_caption_control("pane-split-right-1", cx);
        root.update(cx, |root, cx| {
            root.width = px(400.0);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.toggle_zoom(window, cx);
                view.split_focused(SplitAxis::Horizontal, window, cx);
            })
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, _| (view.pane_count(), view.zoom_state())),
            (3, ZoomState::Restored)
        );
    }

    #[gpui::test]
    fn zoomed_split_should_respect_the_restored_pane_allocation(cx: &mut TestAppContext) {
        let (root, view, records, cx) = caption_view(cx);
        click_caption_control("pane-split-right-1", cx);
        root.update(cx, |root, cx| {
            root.width = px(200.0);
            cx.notify();
        });
        cx.run_until_parked();
        click_caption_control("pane-toggle-zoom-1", cx);
        click_caption_control("pane-split-right-1", cx);
        assert_eq!(
            view.read_with(cx, |view, _| (view.pane_count(), view.zoom_state())),
            (2, ZoomState::Zoomed(PaneId::new(1)))
        );
        assert_eq!(records.starts().len(), 2);
    }

    #[gpui::test]
    fn minimum_width_single_pane_should_keep_both_split_controls(cx: &mut TestAppContext) {
        let (root, view, _, cx) = caption_view(cx);
        let minimum_width =
            cx.update(|_, cx| minimum_pane_width(crate::ui::appearance::chrome(cx)));
        root.update(cx, |root, cx| {
            root.width = px(minimum_width);
            cx.notify();
        });
        view.update(cx, |view, cx| {
            view.pane_attention.insert(PaneId::new(1), 1);
            cx.notify();
        });
        cx.run_until_parked();
        let caption = cx.debug_bounds("pane-caption-1-focused").unwrap();
        for selector in ["pane-split-right-1", "pane-split-down-1"] {
            let button = cx.debug_bounds(selector).unwrap();
            assert!(
                caption.contains(&button.origin) && caption.contains(&button.bottom_right()),
                "caption must contain {selector}, got {caption:?} and {button:?}"
            );
        }
        let status = cx.debug_bounds("pane-status-1-attention").unwrap();
        let first_control = cx.debug_bounds("pane-split-right-1").unwrap();
        assert!(
            status.right() <= first_control.left(),
            "status must remain visible before controls, got {status:?} and {first_control:?}"
        );
        assert!(cx.debug_bounds("pane-toggle-zoom-1").is_none());
        assert!(cx.debug_bounds("pane-close-1").is_none());
    }

    #[gpui::test]
    fn minimum_width_attention_captions_should_keep_zoom_and_close_inside_their_pane(
        cx: &mut TestAppContext,
    ) {
        let (root, view, _, cx) = caption_view(cx);
        click_caption_control("pane-split-down-1", cx);
        let minimum_width =
            cx.update(|_, cx| minimum_pane_width(crate::ui::appearance::chrome(cx)));
        root.update(cx, |root, cx| {
            root.width = px(minimum_width);
            cx.notify();
        });
        view.update(cx, |view, cx| {
            view.pane_attention.insert(PaneId::new(2), 1);
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
        let zoom = cx.debug_bounds("pane-toggle-zoom-2").unwrap();
        let close = cx.debug_bounds("pane-close-2").unwrap();
        assert_eq!(
            close.left() - zoom.right(),
            px(PANE_CONTROL_GAP - PANE_CLOSE_OPTICAL_TRIM)
        );
        click_caption_control("pane-toggle-zoom-2", cx);
        assert_eq!(
            view.read_with(cx, |view, _| view.zoom_state()),
            ZoomState::Zoomed(PaneId::new(2))
        );
        root.update(cx, |root, cx| {
            root.width = px(1000.0);
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
        let (_, view, records, cx) = caption_view(cx);
        let mut screen =
            ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "zsh", 1);
        let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
        metadata.title.provenance = TitleProvenance::Fallback;
        metadata.directory.path = Arc::from("/srv/new-place");
        metadata.command = Some(CommandMetadata {
            line: Arc::from("π build"),
            state: CommandState::Running,
        });
        records
            .event_sender(1)
            .unwrap()
            .try_send(TerminalSessionEvent::Screen(screen.clone()))
            .unwrap();
        cx.run_until_parked();
        let caption = view.read_with(cx, |view, _| {
            let caption = view.pane_captions.get(&PaneId::new(1)).unwrap();
            (
                caption.directory.clone(),
                caption.name.clone(),
                caption.label.clone(),
                caption.glyph.clone(),
            )
        });
        assert_eq!(
            caption,
            ("/srv/".into(), "new-place".into(), "π build".into(), None,)
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
            .try_send(TerminalSessionEvent::Screen(screen))
            .unwrap();
        cx.run_until_parked();
        let caption = view.read_with(cx, |view, _| {
            let caption = view.pane_captions.get(&PaneId::new(1)).unwrap();
            (
                caption.directory.clone(),
                caption.name.clone(),
                caption.label.clone(),
                caption.glyph.clone(),
            )
        });
        assert_eq!(
            caption,
            ("/srv/".into(), "new-place".into(), "zsh".into(), None)
        );
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
            let (_, view, records, cx) = caption_view(cx);
            let mut screen =
                ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "zsh", 1);
            let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
            metadata.context = context;
            metadata.directory.path = Arc::from("/Users/tester/Projects/app");
            records
                .event_sender(1)
                .unwrap()
                .try_send(TerminalSessionEvent::Screen(screen))
                .unwrap();
            cx.run_until_parked();

            let caption = view.read_with(cx, |view, _| {
                let caption = view.pane_captions.get(&PaneId::new(1)).unwrap();
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
    fn caption_should_present_origin_directory_status_then_activity(cx: &mut TestAppContext) {
        let (_, _, records, cx) = caption_view(cx);
        let mut screen =
            ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "zsh", 1);
        let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
        metadata.context = crate::terminal::metadata::TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            "/Users/tester",
            crate::terminal::metadata::LocalMachine::new(
                Some("tester"),
                Some("workstation"),
                Some("/Users/tester"),
            ),
        );
        metadata.directory.path = Arc::from("/Users/tester/Projects/app");
        metadata.title.value = Arc::from("build");
        metadata.title.provenance = crate::terminal::metadata::TitleProvenance::TerminalControl;
        metadata.progress = crate::terminal::metadata::ProgressMetadata::Normal(40);
        records
            .event_sender(1)
            .unwrap()
            .try_send(TerminalSessionEvent::Screen(screen))
            .unwrap();
        cx.run_until_parked();

        let centers = [
            "pane-caption-account-1",
            "pane-caption-host-1",
            "pane-caption-directory-1",
            "pane-caption-name-1",
            "pane-caption-status-separator-1",
            "pane-status-1-normal",
            "pane-caption-label-1",
        ]
        .map(|selector| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("caption must render {selector}"))
                .center()
                .x
        });

        assert!(
            centers.windows(2).all(|pair| pair[0] < pair[1]),
            "caption segments must render left-to-right, got {centers:?}"
        );
    }

    #[gpui::test]
    fn caption_vertical_spacing_should_be_symmetric_in_both_densities(cx: &mut TestAppContext) {
        let (_, _, _, cx) = caption_view(cx);
        for spacing_scale in [1.0, 1.25] {
            let appearance = super::super::appearance::ChromeAppearance {
                spacing_scale,
                ..Default::default()
            };
            let expected_height = appearance.caption_height();
            let padding = appearance.spacing(PANE_CAPTION_VERTICAL_PADDING);
            cx.update(|window, cx| {
                cx.set_global(super::super::appearance::InstalledChrome::single(Arc::new(
                    appearance,
                )));
                window.refresh();
            });
            cx.run_until_parked();
            let caption = cx.debug_bounds("pane-caption-1-focused").unwrap();
            let pane = cx.debug_bounds("pane-surface-1").unwrap();
            let controls = cx.debug_bounds("pane-split-right-1").unwrap();
            let name = cx.debug_bounds("pane-caption-name-1").unwrap();
            let top = controls.origin.y - caption.origin.y;
            let bottom = caption.bottom_right().y - controls.bottom_right().y;
            // GPUI rounds layout edges to device pixels.
            assert!((caption.size.height - expected_height).abs() <= px(0.5));
            assert!((top - bottom).abs() <= px(0.5), "top and bottom must match");
            assert!(top >= padding && bottom >= padding);
            // The caption row is the strip: it starts at the Pane's own top edge and its contents
            // ride the strip's middle rather than a box of their own.
            assert_eq!(caption.top(), pane.top());
            assert_eq!(caption.left(), pane.left());
            assert_eq!(caption.size.width, pane.size.width);
            for content in [controls, name] {
                assert!(
                    (content.center().y - caption.center().y).abs() <= px(0.5),
                    "caption content should centre on the strip at {spacing_scale}, got \
                     {content:?} in {caption:?}"
                );
            }
        }
    }

    #[gpui::test]
    fn caption_background_should_follow_its_own_terminal_surface(cx: &mut TestAppContext) {
        let (_, _, records, cx) = caption_view(cx);
        click_caption_control("pane-split-right-1", cx);
        let chrome_before = cx.update(|_, cx| super::super::appearance::chrome(cx).clone());
        for (generation, first, second, selectors) in [
            (
                1,
                Color::rgb(0xfafafa),
                Color::rgb(0x111111),
                [
                    "pane-caption-surface-1-fafafaff",
                    "pane-caption-surface-2-111111ff",
                ],
            ),
            (
                2,
                Color::rgb(0x223344),
                Color::rgb(0xeeddcc),
                [
                    "pane-caption-surface-1-223344ff",
                    "pane-caption-surface-2-eeddccff",
                ],
            ),
        ] {
            for (pane_id, background) in [(1, first), (2, second)] {
                let mut screen = ScreenSnapshot::from_test_parts_at(
                    Arc::from([]),
                    Default::default(),
                    "zsh",
                    generation,
                );
                Arc::make_mut(&mut screen).background = background;
                records
                    .event_sender(pane_id)
                    .unwrap()
                    .try_send(TerminalSessionEvent::Screen(screen))
                    .unwrap();
            }
            cx.run_until_parked();
            for selector in selectors {
                assert!(cx.debug_bounds(selector).is_some(), "{selector}");
            }
            cx.update(|_, cx| assert_eq!(super::super::appearance::chrome(cx), &chrome_before));
        }
    }

    #[gpui::test]
    fn single_pane_should_float_as_one_rounded_surface_holding_its_caption(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();

        let root = gpui::Bounds::new(
            point(px(0.0), px(0.0)),
            cx.update(|window, _| window.viewport_size()),
        );
        let surface = cx
            .debug_bounds("pane-surface-1")
            .expect("the floating Pane surface was rendered");
        let caption = cx
            .debug_bounds("pane-caption-1-focused")
            .expect("the Pane Caption was rendered");
        let terminal = cx
            .debug_bounds("terminal-pane")
            .expect("the Terminal was rendered");
        assert_eq!(surface, root, "a single Pane should fill its Tab");
        assert!(
            surface.contains(&caption.origin) && caption.right() <= surface.right(),
            "the Pane Caption should sit inside the Pane surface"
        );
        assert_eq!(caption.top(), surface.top());
        assert_eq!(
            terminal.bottom(),
            surface.bottom(),
            "the Terminal should reach the rounded Pane edge"
        );

        let (frame, base, measured) = cx.update(|_, cx| {
            let appearance = super::super::appearance::chrome(cx);
            (
                super::super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx),
                super::super::workspace_frame::base_surface(&appearance.colors),
                view.read(cx).pane_bounds.get(&PaneId::new(1)).copied(),
            )
        });
        assert_eq!(
            measured,
            Some(surface),
            "Pane geometry should still measure exactly the caption and terminal region"
        );
        let mask: &'static str = format!(
            "pane-corner-mask-1-{}-{:08x}",
            f32::from(frame.pane_radius()),
            base.rgba_hex()
        )
        .leak();
        assert!(
            cx.debug_bounds(mask).is_some(),
            "the Pane corners should be masked at the frame radius with the base surface"
        );
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
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
    fn cached_reported_glyph_should_follow_each_current_font_check() {
        let cached = Some(gpui::SharedString::from("π"));

        assert_eq!(drawable_reported_glyph(cached.as_ref(), |_| false), None);
        assert_eq!(
            drawable_reported_glyph(cached.as_ref(), |_| true),
            Some(gpui::SharedString::from("π"))
        );
        assert_eq!(cached, Some(gpui::SharedString::from("π")));
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
            user: px(40.0),
            host: px(60.0),
            directory: px(120.0),
            name: px(40.0),
            status_separator: px(PANE_CAPTION_SEPARATOR_WIDTH),
            label: px(30.0),
        };
        let resolve = |width: f32| {
            CaptionLayout::from_metrics(false, px(width), metrics, 1.0, 13.0, PANE_CONTROL_SIZE)
        };
        let controls = PANE_CAPTION_LEFT_PADDING
            + PANE_CAPTION_RIGHT_PADDING
            + PANE_STATUS_WIDTH
            + PANE_CONTROL_LEADING_GAP
            + controls_width(2, false, PANE_CONTROL_SIZE, 1.0);
        let layout = |separator, host, directory, user, label| CaptionLayout {
            show_status_separator: separator,
            show_host: host,
            show_directory: directory,
            show_user: user,
            show_label: label,
            show_splits: true,
        };

        assert_eq!(
            resolve(controls + 308.0),
            layout(true, true, true, true, true)
        );
        assert_eq!(
            resolve(controls + 278.0),
            layout(true, true, true, true, false)
        );
        assert_eq!(
            resolve(controls + 238.0),
            layout(true, true, true, false, false)
        );
        assert_eq!(
            resolve(controls + 118.0),
            layout(true, true, false, false, false)
        );
        assert_eq!(
            resolve(controls + 58.0),
            layout(true, false, false, false, false)
        );
        assert_eq!(
            resolve(controls + 40.0),
            layout(false, false, false, false, false)
        );
        assert!(!resolve(controls - 1.0).show_splits);
    }

    /// A narrow Pane Caption without status retains Split controls above their width threshold.
    #[test]
    fn narrow_caption_without_status_should_keep_split_controls_above_the_control_threshold() {
        let metrics = CaptionMetrics {
            name: px(40.0),
            ..CaptionMetrics::default()
        };
        let controls = PANE_CAPTION_LEFT_PADDING
            + PANE_CAPTION_RIGHT_PADDING
            + PANE_STATUS_WIDTH
            + PANE_CONTROL_LEADING_GAP
            + controls_width(2, false, PANE_CONTROL_SIZE, 1.0);
        let resolve = |width: f32| {
            CaptionLayout::from_metrics(false, px(width), metrics, 1.0, 13.0, PANE_CONTROL_SIZE)
        };

        let narrow = resolve(controls + 10.0);
        assert!(narrow.show_splits);
    }

    #[gpui::test]
    fn every_split_pane_caption_should_render_its_own_name_segment(cx: &mut TestAppContext) {
        let (_, view, _, cx) = caption_view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.focus_pane(PaneId::new(1), cx);
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| {
                view.focus(window, cx);
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.focus_pane(PaneId::new(1), cx);
                view.focus(window, cx);
            });
        });
        cx.run_until_parked();

        let origin = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.native_service_status(WorkspaceId::new(1), window, cx)
                    .origin
                    .expect("the focused terminal must expose a Service origin")
            })
        });
        let accepted = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.focus_pane(PaneId::new(2), cx);
                view.focus_pane(PaneId::new(1), cx);
                view.focus(window, cx);
                view.native_service_target(origin).is_some_and(|terminal| {
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
                crate::terminal::testing::RecordedCommand::RequestPaste(_)
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| {
                view.focus(window, cx);
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.focus_pane(PaneId::new(1), cx);
                view.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let origin = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.native_service_status(WorkspaceId::new(1), window, cx)
                    .origin
                    .unwrap()
            })
        });

        let accepted = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.close_pane(PaneId::new(2), window, cx);
                view.native_service_target(origin).is_some_and(|terminal| {
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
                crate::terminal::testing::RecordedCommand::RequestPaste(_)
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| {
                view.focus(window, cx);
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let _first_pane = view.read_with(cx, |view, _| {
            view.tab
                .pane(PaneId::new(1))
                .cloned()
                .expect("the original Pane should still exist")
        });

        let position = view.read_with(cx, |view, _| {
            view.pane_bounds
                .get(&PaneId::new(1))
                .expect("the target Pane has measured bounds")
                .center()
        });
        cx.simulate_event(gpui::FileDropEvent::Entered {
            position,
            paths: gpui::ExternalPaths(vec![PathBuf::from("/tmp/first pane")].into()),
        });
        cx.run_until_parked();
        cx.simulate_event(gpui::FileDropEvent::Submit { position });
        cx.run_until_parked();

        let paste_requests = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                crate::terminal::testing::RecordedCommand::RequestPaste(text) => {
                    Some((call.session_id, text))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            (
                view.read_with(cx, |view, _| view.focused_pane_id()),
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let first_sender = records
            .event_sender(1)
            .expect("the first Pane's Terminal Session was not started");
        first_sender
            .try_send(TerminalSessionEvent::Screen(
                ScreenSnapshot::from_test_parts(
                    Arc::from([]),
                    ScrollbarSnapshot {
                        total_rows: 100,
                        visible_rows: 20,
                        ..Default::default()
                    },
                    "",
                ),
            ))
            .unwrap();
        cx.run_until_parked();

        let first_pane = view.read_with(cx, |view, _| {
            view.pane_bounds
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
            view.read_with(cx, |view, cx| {
                (
                    view.focused_pane_id(),
                    view.focused_terminal_is_focused(window, cx),
                )
            })
        });
        assert_eq!(state, (PaneId::new(1), true));
    }

    #[gpui::test]
    fn command_shift_vim_shortcuts_should_not_move_pane_focus(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);

        let focused_panes = focused_panes_after_shortcuts(
            &view,
            cx,
            ["cmd-shift-l", "cmd-shift-j", "cmd-shift-h", "cmd-shift-k"],
        );

        assert_eq!(
            focused_panes,
            [
                PaneId::new(1),
                PaneId::new(1),
                PaneId::new(1),
                PaneId::new(1),
            ]
        );
    }

    #[gpui::test]
    fn command_option_arrow_shortcuts_should_focus_panes_in_each_direction(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = four_pane_view(cx);

        let focused_panes = focused_panes_after_shortcuts(
            &view,
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
    fn command_bracket_shortcuts_should_cycle_panes_in_recency_order(cx: &mut TestAppContext) {
        let (view, cx) = four_pane_view(cx);

        let previous =
            focused_panes_after_shortcuts(&view, cx, ["cmd-[", "cmd-[", "cmd-[", "cmd-["]);
        let next = focused_panes_after_shortcuts(&view, cx, ["cmd-]", "cmd-]", "cmd-]", "cmd-]"]);

        assert_eq!(
            (previous, next),
            (
                [
                    PaneId::new(4),
                    PaneId::new(3),
                    PaneId::new(2),
                    PaneId::new(1),
                ],
                [
                    PaneId::new(2),
                    PaneId::new(3),
                    PaneId::new(4),
                    PaneId::new(1),
                ],
            )
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let sender = records
            .last_event_sender()
            .expect("the split Pane's Terminal Session was not started");
        sender
            .try_send(TerminalSessionEvent::Screen(
                ScreenSnapshot::from_test_parts(Arc::from([]), Default::default(), "Claude Code"),
            ))
            .unwrap();
        cx.run_until_parked();

        let title = view.read_with(cx, |view, _| view.pane_titles.get(&PaneId::new(2)).cloned());
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let sender = records
            .event_sender(2)
            .expect("the split Pane's Terminal Session was not started");
        sender
            .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let state = view.read_with(cx, |view, _| {
            (
                view.tab.pane_count(),
                view.tab.focused_pane_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, PaneId::new(1), vec![2]));
    }

    #[gpui::test]
    fn exited_last_terminal_session_should_request_tab_close(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let close_requests = Rc::new(RefCell::new(Vec::new()));
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        let close_requests_for_subscription = Rc::clone(&close_requests);
        view.update(cx, |_, cx| {
            cx.subscribe(&view, move |_, _, event: &TabViewEvent, _| {
                close_requests_for_subscription
                    .borrow_mut()
                    .push(event.clone());
            })
            .detach();
        });

        let sender = records
            .event_sender(1)
            .expect("the initial Pane's Terminal Session was not started");
        sender
            .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
            .unwrap();
        sender
            .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::ExitCode(
                1,
            )))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            (
                close_requests.borrow().clone(),
                records.dropped_session_ids()
            ),
            (
                vec![TabViewEvent::CloseTabRequested {
                    tab_id: TabId::new(1)
                }],
                Vec::new()
            )
        );
    }

    #[gpui::test]
    fn single_pane_toggle_zoom_should_not_emit_presentation_changed(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let presentation_changes = Rc::new(Cell::new(0));
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        let presentation_changes_for_subscription = Rc::clone(&presentation_changes);
        view.update(cx, |_, cx| {
            cx.subscribe(&view, move |_, _, event: &TabViewEvent, _| {
                if matches!(event, TabViewEvent::PresentationChanged { .. }) {
                    presentation_changes_for_subscription.update(|count| count + 1);
                }
            })
            .detach();
        });

        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.toggle_zoom(window, cx));
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        let presentation_changes_for_subscription = Rc::clone(&presentation_changes);
        view.update(cx, |_, cx| {
            cx.subscribe(&view, move |_, _, event: &TabViewEvent, _| {
                if matches!(event, TabViewEvent::PresentationChanged { .. }) {
                    presentation_changes_for_subscription.update(|count| count + 1);
                }
            })
            .detach();
        });

        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.toggle_zoom(window, cx));
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.toggle_zoom(window, cx);
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

        let state = view.read_with(cx, |view, _| {
            (view.tab.zoom_state(), records.pointer_count())
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let handle = cx
            .debug_bounds("split-resize-1-hitbox")
            .expect("the shared split ResizeHandle was rendered");
        let start = handle.center();
        let destination = point(start.x + px(60.0), start.y + px(40.0));

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(destination, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(destination, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let view = view.read(cx);
            let ratio = match view.tab.root().node() {
                PaneNodeRef::Split { ratio, .. } => ratio,
                PaneNodeRef::Leaf { .. } => 0.0,
            };
            (
                ratio,
                view.resizing_split_id,
                view.focused_terminal_has_input_focus(window, cx),
            )
        });
        assert!(
            state.0 > 0.5,
            "the shared handle did not grow the first Pane"
        );
        assert_eq!((state.1, state.2, records.pointer_count()), (None, true, 0));
    }

    #[gpui::test]
    fn pane_split_resize_states_should_keep_the_handle_inside_its_gap(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let hitbox = cx
            .debug_bounds("split-resize-1-hitbox")
            .expect("the split ResizeHandle was not rendered");
        let gap = cx
            .debug_bounds("split-gap-1")
            .expect("the split gap was not rendered");
        let target_width = hitbox.size.width;
        let center = hitbox.center();
        // The handle paints nothing; its unpainted layout mark stays centred inside the gap in
        // every interaction state instead of growing into a capsule.
        let mark = |cx: &mut VisualTestContext| {
            cx.debug_bounds("split-resize-1-divider")
                .expect("the split handle layout was not rendered")
        };
        let resting = mark(cx);

        cx.simulate_mouse_move(center, None, Modifiers::none());
        cx.run_until_parked();
        let hovered = mark(cx);
        cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let active = mark(cx);
        cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::none());

        assert_eq!(target_width, px(8.0));
        for state in [resting, hovered, active] {
            assert_eq!(state.size.width, resting.size.width);
            assert!(
                state.left() >= gap.left() && state.right() <= gap.right(),
                "the handle should stay inside the gap, got {state:?} in {gap:?}"
            );
        }
    }

    #[gpui::test]
    fn pane_split_resize_should_reveal_its_paintless_handle_to_keyboard_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("split-resize-1-keyboard-focus-indicator")
                .is_none(),
            "the idle Pane gap should stay paintless"
        );

        let mut indicator = None;
        for _ in 0..32 {
            cx.update(|window, cx| window.focus_next(cx));
            cx.run_until_parked();
            indicator = cx.debug_bounds("split-resize-1-keyboard-focus-indicator");
            if indicator.is_some() {
                break;
            }
        }
        let indicator = indicator.expect("the split resize tab stop should reveal its indicator");
        let gap = cx
            .debug_bounds("split-gap-1")
            .expect("the split gap was rendered");
        assert!(
            indicator.left() >= gap.left() && indicator.right() <= gap.right(),
            "the keyboard focus indicator escaped the locked Pane gap"
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
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let handle = cx
            .debug_bounds("split-resize-1-hitbox")
            .expect("the integrated split ResizeHandle was rendered");
        let edges = [
            point(handle.left() + px(0.5), handle.center().y),
            point(handle.right() - px(0.5), handle.center().y),
        ];

        for edge in edges {
            cx.simulate_mouse_move(edge, None, Modifiers::none());
            cx.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
            assert_eq!(
                view.read_with(cx, |view, _| view.resizing_split_id),
                Some(SplitId::new(1)),
                "the split handle did not own outer hitbox edge {edge:?}"
            );
            cx.simulate_mouse_up(edge, MouseButton::Left, Modifiers::none());
            assert_eq!(
                view.read_with(cx, |view, _| view.resizing_split_id),
                None,
                "the split handle did not release outer hitbox edge {edge:?}"
            );
        }

        assert_eq!(records.pointer_count(), 0);
    }

    #[test]
    fn split_ratio_should_follow_horizontal_requested_offset() {
        let split_bounds = bounds(point(px(10.0), px(20.0)), size(px(406.0), px(200.0)));

        assert_eq!(
            split_ratio_for_offset(SplitAxis::Horizontal, split_bounds, 100.0, 6.0),
            Some(0.25)
        );
    }

    #[test]
    fn split_ratio_should_follow_vertical_requested_offset() {
        let split_bounds = bounds(point(px(10.0), px(20.0)), size(px(400.0), px(208.0)));

        assert_eq!(
            split_ratio_for_offset(SplitAxis::Vertical, split_bounds, 50.0, 8.0),
            Some(0.25)
        );
    }

    #[test]
    fn split_content_extent_should_reserve_the_gap_and_reject_empty_splits() {
        let split_bounds = bounds(point(px(0.0), px(0.0)), size(px(6.0), px(100.0)));

        assert_eq!(
            split_content_extent(SplitAxis::Vertical, split_bounds, 6.0),
            Some(94.0)
        );
        assert_eq!(
            split_content_extent(SplitAxis::Horizontal, split_bounds, 6.0),
            None
        );
    }

    fn split_gap_view(cx: &mut TestAppContext) -> (Entity<TabView>, &mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (view, cx)
    }

    fn install_spacing_scale(spacing_scale: f32, cx: &mut VisualTestContext) -> f32 {
        let appearance = super::super::appearance::ChromeAppearance {
            spacing_scale,
            ..Default::default()
        };
        let gap = cx.update(|_, cx| {
            f32::from(
                super::super::workspace_frame::WorkspaceFrame::for_appearance(&appearance, cx)
                    .pane_gap(),
            )
        });
        cx.update(|window, cx| {
            cx.set_global(super::super::appearance::InstalledChrome::single(Arc::new(
                appearance,
            )));
            window.refresh();
        });
        cx.run_until_parked();
        gap
    }

    fn split_ratio(view: &Entity<TabView>, cx: &mut VisualTestContext) -> f32 {
        view.read_with(cx, |view, _| match view.tab.root().node() {
            PaneNodeRef::Split { ratio, .. } => ratio,
            PaneNodeRef::Leaf { .. } => f32::NAN,
        })
    }

    /// Nested Splits keep the frame's gap rhythm at both densities, and each gap is empty base
    /// surface owned by an unpainted resize target centred over it.
    #[gpui::test]
    fn split_gaps_should_follow_the_frame_rhythm_in_nested_splits_at_both_densities(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = split_gap_view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_pane(PaneId::new(1), SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_pane(PaneId::new(2), SplitAxis::Vertical, window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.pane_count()), 3);

        let mut gaps = Vec::new();
        for spacing_scale in [1.0, 1.25] {
            let gap = install_spacing_scale(spacing_scale, cx);
            gaps.push(gap);
            let first = cx.debug_bounds("pane-surface-1").expect("Pane 1 surface");
            let second = cx.debug_bounds("pane-surface-2").expect("Pane 2 surface");
            let third = cx.debug_bounds("pane-surface-3").expect("Pane 3 surface");
            // GPUI rounds each laid-out edge to device pixels independently.
            let close = |actual: Pixels, expected: f32| (f32::from(actual) - expected).abs() <= 1.0;
            assert!(
                close(second.left() - first.right(), gap),
                "horizontal gap at {spacing_scale}: {first:?} {second:?}"
            );
            assert!(
                close(third.top() - second.bottom(), gap),
                "nested vertical gap at {spacing_scale}: {second:?} {third:?}"
            );

            for (split, handle_axis_extent) in [("1", gap), ("2", gap)] {
                let gap_bounds = cx
                    .debug_bounds(leaked(format!("split-gap-{split}")))
                    .expect("the Split gap target was rendered");
                let hitbox = cx
                    .debug_bounds(leaked(format!("split-resize-{split}-hitbox")))
                    .expect("the Split resize hitbox was rendered");
                let (gap_extent, gap_center, hit_center) = if split == "1" {
                    (
                        gap_bounds.size.width,
                        gap_bounds.center().x,
                        hitbox.center().x,
                    )
                } else {
                    (
                        gap_bounds.size.height,
                        gap_bounds.center().y,
                        hitbox.center().y,
                    )
                };
                assert!(close(gap_extent, handle_axis_extent), "Split {split} gap");
                assert!(
                    (gap_center - hit_center).abs() <= px(0.5),
                    "Split {split} resize target should be centred over its gap"
                );
            }

            // Every Pane wears the same rounded treatment, in a Split exactly as when alone.
            let (radius, base) = cx.update(|_, cx| {
                let appearance = super::super::appearance::chrome(cx);
                (
                    super::super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                        .pane_radius(),
                    super::super::workspace_frame::base_surface(&appearance.colors),
                )
            });
            for pane in 1..=3 {
                let mask = leaked(format!(
                    "pane-corner-mask-{pane}-{}-{:08x}",
                    f32::from(radius),
                    base.rgba_hex()
                ));
                assert!(
                    cx.debug_bounds(mask).is_some(),
                    "Pane {pane} should carry the frame's corner treatment at {spacing_scale}"
                );
            }

            let (minimum, expected) = view.read_with(cx, |view, _| {
                let leaf = view.tab.minimum_pane_size();
                (
                    view.tab.minimum_size(gap).unwrap(),
                    (leaf.width() * 2.0 + gap, leaf.height() * 2.0 + gap),
                )
            });
            assert_eq!(
                (minimum.width(), minimum.height()),
                expected,
                "minimum Pane Layout size should reserve one gap per Split axis"
            );
        }
        assert_eq!(gaps, [3.0, 4.0]);
    }

    #[gpui::test]
    fn unpainted_split_gap_should_resize_and_reset_without_terminal_input(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::new(TestTerminalSessionFactory::new(records.clone())),
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let hitbox = cx
            .debug_bounds("split-resize-1-hitbox")
            .expect("the Split resize hitbox was rendered");
        let gap = cx
            .debug_bounds("split-gap-1")
            .expect("the Split gap was rendered");
        assert!(
            hitbox.size.width >= gap.size.width,
            "the resize target should cover the whole gap"
        );

        let start = hitbox.center();
        let destination = point(start.x + px(40.0), start.y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(destination, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(destination, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert!(
            split_ratio(&view, cx) > 0.5,
            "dragging the gap should resize"
        );

        assert_eq!(records.pointer_count(), 0);

        let center = cx
            .debug_bounds("split-resize-1-hitbox")
            .expect("the moved Split resize hitbox was rendered")
            .center();
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: center,
            modifiers: Modifiers::none(),
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position: center,
            modifiers: Modifiers::none(),
            click_count: 2,
        });
        cx.run_until_parked();

        assert_eq!(
            split_ratio(&view, cx),
            0.5,
            "double-clicking the gap resets"
        );
        assert_eq!(records.pointer_count(), 0);
        let focused =
            cx.update(|window, cx| view.read(cx).focused_terminal_has_input_focus(window, cx));
        assert!(
            focused,
            "a pointer interaction should return input to the terminal"
        );
    }

    #[gpui::test]
    fn zoomed_pane_should_preserve_its_gapped_restored_split_allocation(cx: &mut TestAppContext) {
        let (view, cx) = split_gap_view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let restored = cx.debug_bounds("pane-surface-2").expect("Pane 2 surface");
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |view, _| view.tab.zoom_state()),
            ZoomState::Zoomed(PaneId::new(2)),
            "the focused Pane should be zoomed before its rendered geometry is inspected"
        );

        let zoomed = cx
            .debug_bounds("pane-surface-2")
            .expect("zoomed Pane surface");
        let viewport = cx.update(|window, _| window.viewport_size());
        assert_eq!(zoomed.size, viewport, "the Zoomed Pane fills its Tab");
        let (radius, base) = cx.update(|_, cx| {
            let appearance = super::super::appearance::chrome(cx);
            (
                super::super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                    .pane_radius(),
                super::super::workspace_frame::base_surface(&appearance.colors),
            )
        });
        assert!(
            cx.debug_bounds(leaked(format!(
                "pane-corner-mask-2-{}-{:08x}",
                f32::from(radius),
                base.rgba_hex()
            )))
            .is_some(),
            "a Zoomed Pane keeps the same rounded treatment as a Split Pane"
        );

        let target = cx.update(|_, cx| {
            let gap = pane_gap(cx);
            view.read(cx).split_target_size(PaneId::new(2), gap)
        });
        let target = target.expect("a Zoomed Pane keeps its restored allocation");
        assert!(
            (target.width() - f32::from(restored.size.width)).abs() <= 1.0
                && (target.height() - f32::from(restored.size.height)).abs() <= 1.0,
            "restored allocation {target:?} should match the gapped layout {restored:?}"
        );
    }

    fn leaked(selector: String) -> &'static str {
        selector.leak()
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
            .try_send(TerminalSessionEvent::Screen(screen))
            .unwrap();
    }

    #[gpui::test]
    fn split_should_inherit_target_directory_and_capture_it_before_remote_wait(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        let records = TestTerminalSessionRecords::default();
        let factory = remote_test_session_factory(records.clone());
        let (view, cx) =
            cx.add_window_view(|window, cx| TabView::new(TabId::new(1), factory, window, cx));
        cx.run_until_parked();
        report_current_directory(&records, 1, 1, "/srv/frontend", true);
        cx.run_until_parked();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        report_current_directory(&records, 2, 1, "/srv/backend", true);
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_pane(PaneId::new(1), SplitAxis::Vertical, window, cx)
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
            view.read_with(cx, |view, cx| view.current_directory(PaneId::new(2), cx)),
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
        let (view, cx) =
            cx.add_window_view(|window, cx| TabView::new(TabId::new(1), factory, window, cx));
        cx.run_until_parked();
        report_current_directory(&records, 1, 1, first.to_str().unwrap(), false);
        cx.run_until_parked();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        report_current_directory(&records, 2, 1, second.to_str().unwrap(), false);
        cx.run_until_parked();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Vertical, cx);
        view.update(cx, |view, _| {
            view.set_pinned_directory(Some(PinnedDirectory::Local(
                authority.validate_directory(&second).unwrap(),
            )))
        });
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        view.update(cx, |view, _| view.set_pinned_directory(None));
        report_current_directory(
            &records,
            1,
            2,
            home.join("missing").to_str().unwrap(),
            false,
        );
        cx.run_until_parked();
        let count = view.read_with(cx, |view, _| view.pane_count());
        split_test_pane(&view, PaneId::new(1), SplitAxis::Vertical, cx);
        assert_eq!(view.read_with(cx, |view, _| view.pane_count()), count);
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
