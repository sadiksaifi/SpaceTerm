#[cfg(all(test, target_os = "linux", feature = "native-tests"))]
#[path = "../platform/linux_adapter_tests/repository_caption.rs"]
mod linux_adapter_tests;

use super::chrome_geometry::concentric_outset;
use super::chrome_icons::{IconRole, InteractiveIconRole};
use super::chrome_typography::{ChromeTextStyleExt as _, ChromeTypography, TextRole};
use super::drag_and_drop::{DragPreview, DragSession, drag_release_observer, grab_point};
use super::pane_lifecycle::PaneConstruction;
use super::pane_motion::{PaneFrames, PaneMotion};
use super::terminal_status::{
    StatusColors, StatusGlyph, TerminalProgress, reported_glyph_is_drawable, reported_title,
};
use crate::repository_status::presentation::{HeadGlyph, RepositoryCaption, RepositoryMark};
use crate::terminal::metadata::CurrentDirectory;
use crate::ui::appearance::gpui_color;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use super::terminal_focus::{TerminalFocusBlocker, TerminalFocusCoordinator, TerminalProductFocus};
use super::{
    ClosePane, FocusNextPane, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp,
    FocusPreviousPane, PaneOrigin, ShowRepositoryStatus, SplitDown, SplitRight,
    TERMINAL_KEY_CONTEXT, TerminalPane, TerminalPaneEvent, TogglePaneZoom,
};

use crate::domain::{
    ClosePaneOutcome, FocusDirection, PaneEdge, PaneId, PaneNodeRef, PaneSize, PaneTreeRef,
    SplitAxis, SplitId, Tab, TabId, WorkspaceId, ZoomState,
};
use crate::terminal::{NativeServiceOrigin, NativeServiceStatus, WorkspaceTerminalSessionFactory};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, DefiniteLength, Entity, EventEmitter, FocusHandle,
    MouseDownEvent, Pixels, Point, Render, Window, div, point, px, relative,
};
use spaceterm_ui::{
    Alert, AlertIntent, ButtonSize, ButtonVariant, HoverFade, Icon, IconButton, IconName,
    ModalAction, ModalActionRole, ModalId, ResizeAxis, ResizeHandle, ResizeHandleEvent,
    ResizeInputSource, Tooltip,
};

/// One Tab's shared server Panes, when a SpaceTerm Server owns the Tab.
type SharedPaneBinding = super::shared_workspaces::SharedPanes;

/// What a shared server Pane presents in place of, and over, its terminal.
#[derive(Default)]
struct SharedPaneSurface {
    /// The screen that replaces the Pane outright, so nothing of a former shell paints.
    screen: Option<AnyElement>,
    /// The interruption notice over the Pane.
    notice: Option<AnyElement>,
}

/// What a shared server Pane's caption controls offer. A split or a close goes to the server, so
/// each waits while this client is offline or an edit is unanswered.
#[derive(Clone, Copy)]
struct SharedCaptionControls {
    edits: bool,
    edits_reason: Option<&'static str>,
}

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
/// Width reserved by the middle dot that separates a directory from its status, including the
/// air on both sides.
const PANE_CAPTION_SEPARATOR_WIDTH: f32 = 17.25;
/// The air before a directory's status, which stays when the middle dot is dropped.
const PANE_STATUS_LEADING_GAP: f32 = 5.0;
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
/// The air between a Pane's identity and its Repository Status.
const PANE_REPOSITORY_LEADING_GAP: f32 = 10.0;
/// The air between the parts of the Repository Status segment.
const PANE_REPOSITORY_PART_GAP: f32 = 4.0;
/// The narrowest truncated branch worth showing.
const PANE_REPOSITORY_MINIMUM_BRANCH_WIDTH: f32 = 36.0;
/// The share of the accent color that fills the half of a Pane a dragged Pane would take.
const PANE_DROP_TARGET_FILL_OPACITY: u8 = 56;
/// The status glyph and its trailing air, which every Pane Caption keeps at every width.
#[cfg(test)]
const PANE_STATUS_WIDTH: f32 = 13.0 + PANE_STATUS_ICON_GAP;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    repository: RepositoryCaptionLayout,
}

/// How many caption segments beyond the Pane name a narrowing caption can give up.
const CAPTION_LADDER: usize = 8;

/// Rendered widths of the caption segments, including separators and leading gaps where present.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CaptionMetrics {
    user: Pixels,
    host: Pixels,
    directory: Pixels,
    name: Pixels,
    /// The air before the status, kept with the name.
    status_gap: Pixels,
    /// The middle dot and the air after it.
    status_separator: Pixels,
    label: Pixels,
    /// Repository Status widths, absent when the Pane shows none.
    repository: Option<RepositoryCaptionMetrics>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RepositoryCaptionMetrics {
    /// The glyph and mark, which a caption with Repository Status always keeps.
    anchor: Pixels,
    branch: Pixels,
    detail: Option<Pixels>,
    divergence: Option<Pixels>,
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
            status_gap: if text.has_directory {
                appearance.spacing(PANE_STATUS_LEADING_GAP)
            } else {
                px(0.0)
            },
            status_separator: if text.has_directory {
                appearance.spacing(PANE_CAPTION_SEPARATOR_WIDTH - PANE_STATUS_LEADING_GAP)
            } else {
                px(0.0)
            },
            label: measure_caption_segment(&text.label, window, appearance),
            repository: text.repository.as_ref().map(|repository| {
                let part = |text: &str| {
                    appearance.spacing(PANE_REPOSITORY_PART_GAP)
                        + measure_caption_segment(&text.to_owned().into(), window, appearance)
                };
                RepositoryCaptionMetrics {
                    anchor: appearance.spacing(PANE_REPOSITORY_LEADING_GAP)
                        + head_glyph_width(repository.glyph, appearance, |text| {
                            measure_caption_segment(&text.to_owned().into(), window, appearance)
                        })
                        + repository.mark.map_or(px(0.0), |mark| part(mark.glyph())),
                    branch: part(&repository.branch),
                    detail: repository.detail.as_ref().map(|detail| part(&detail.text)),
                    divergence: repository.divergence.as_deref().map(part),
                }
            }),
        }
    }

    /// The droppable segments in the order a widening caption admits them. An absent segment is
    /// neither admitted nor ends the ladder.
    fn ladder(self) -> [Option<Pixels>; CAPTION_LADDER] {
        let repository = self.repository;
        [
            repository.map(|repository| repository.branch),
            Some(self.status_separator),
            repository.and_then(|repository| repository.detail),
            Some(self.host),
            Some(self.directory),
            Some(self.user),
            Some(self.label),
            repository.and_then(|repository| repository.divergence),
        ]
    }
}

/// Which parts of the Repository Status segment fit. The glyph and mark are kept whenever the
/// segment shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RepositoryCaptionLayout {
    /// The glyph and mark fit beside the name, the status, and every control the Pane shows.
    show: bool,
    show_branch: bool,
    /// The widest the branch may be when it does not fit whole, in whole pixels.
    branch_limit: Option<u32>,
    show_detail: bool,
    show_divergence: bool,
}

impl CaptionLayout {
    /// `controls_reveal` is how far the Pane controls have eased into view; hidden controls free
    /// their width for the caption's other segments.
    fn resolve(
        caption: &PaneCaption,
        width: Pixels,
        controls_reveal: f32,
        window: &Window,
        appearance: &super::appearance::ChromeAppearance,
    ) -> Self {
        Self::from_metrics(
            caption.has_multiple_panes,
            width,
            controls_reveal,
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
        controls_reveal: f32,
        metrics: CaptionMetrics,
        spacing_scale: f32,
        status_icon_size: f32,
        control_size: f32,
    ) -> Self {
        let full_control_count = if has_multiple_panes { 4 } else { 2 };
        let fixed_width =
            (PANE_CAPTION_LEFT_PADDING + PANE_CAPTION_RIGHT_PADDING + PANE_STATUS_ICON_GAP)
                * spacing_scale
                + status_icon_size
                + metrics
                    .repository
                    .map_or(0.0, |repository| f32::from(repository.anchor));
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
        let controls_extent = leading_gap
            + controls_width(
                control_count,
                has_multiple_panes,
                control_size,
                spacing_scale,
            );
        // The name and status come before Repository Status. The check uses the revealed controls,
        // so hovering a Pane never shows or hides the segment.
        if metrics.repository.is_some()
            && width < px(fixed_width + controls_extent) + metrics.name + metrics.status_gap
        {
            return Self::from_metrics(
                has_multiple_panes,
                width,
                controls_reveal,
                CaptionMetrics {
                    repository: None,
                    ..metrics
                },
                spacing_scale,
                status_icon_size,
                control_size,
            );
        }
        let available = (width
            - px(fixed_width + controls_extent * controls_reveal.clamp(0.0, 1.0)))
        .max(px(0.0));
        // The name is always kept. Every other segment is admitted in priority order and the
        // first one that does not fit ends the ladder, so segments never reappear out of order.
        // A branch that does not fit whole is truncated when its narrowest form fits, and ends the
        // ladder either way.
        let mut claimed = metrics.name + metrics.status_gap;
        let mut shown = [false; CAPTION_LADDER];
        let mut branch_limit = None;
        for (index, (admitted, width)) in shown.iter_mut().zip(metrics.ladder()).enumerate() {
            let Some(width) = width else {
                continue;
            };
            if claimed + width > available {
                // The branch's leading gap sits outside its width limit.
                let remaining =
                    f32::from(available - claimed) - PANE_REPOSITORY_PART_GAP * spacing_scale;
                if index == 0 && remaining >= PANE_REPOSITORY_MINIMUM_BRANCH_WIDTH * spacing_scale {
                    *admitted = true;
                    branch_limit = Some(remaining.floor() as u32);
                }
                break;
            }
            claimed += width;
            *admitted = true;
        }
        let [
            show_repository_branch,
            show_status_separator,
            show_repository_detail,
            show_host,
            show_directory,
            show_user,
            show_label,
            show_repository_divergence,
        ] = shown;
        Self {
            show_status_separator,
            show_host,
            show_directory,
            show_user,
            show_label,
            show_splits,
            repository: RepositoryCaptionLayout {
                show: metrics.repository.is_some(),
                show_branch: show_repository_branch,
                branch_limit,
                show_detail: show_repository_detail,
                show_divergence: show_repository_divergence,
            },
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
    // GPUI rounds intrinsic text bounds up to logical pixels; preserve that reservation when
    // snapping to device pixels so the ladder cannot squeeze the name.
    let width = appearance
        .typography
        .measure(TextRole::Body, text, window)
        .ceil();
    ChromeTypography::round_measurement_up(width, window)
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

/// Each Pane's hover and how far its caption controls have eased into view, as read at the start
/// of a frame.
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
    /// Each Pane's laid-out place.
    pane_bounds: BTreeMap<PaneId, Bounds<Pixels>>,
    /// Where each Pane's surface was drawn, which differs from its place while Panes move.
    pane_surfaces: BTreeMap<PaneId, Bounds<Pixels>>,
    /// Where each Pane's caption Split and Close controls took presses in the last frame.
    pane_unselecting_controls: BTreeMap<(PaneId, PaneCaptionAction), gpui::Hitbox>,
    pane_layout_size: Option<PaneSize>,
    /// A Split that waits for the next Pane measurement, because the retained bounds are stale.
    pending_split: Option<(PaneId, SplitAxis)>,
    split_bounds: BTreeMap<SplitId, Bounds<Pixels>>,
    pane_titles: BTreeMap<PaneId, gpui::SharedString>,
    pane_captions: BTreeMap<PaneId, PaneCaptionText>,
    pane_attention: BTreeMap<PaneId, u32>,
    resizing_split_id: Option<SplitId>,
    pane_drag: Option<PaneDrag>,
    pane_motion: PaneMotion,
    /// Where this frame draws each Pane, from `pane_motion`.
    pane_frames: PaneFrames,
    /// How much wider than the Pane Layout this frame's stage is while it moves with the sidebar.
    stage_stretch: Pixels,
    active: bool,
    focus_branch_blocker: Option<TerminalFocusBlocker>,
    native_service_hierarchy_generation: u64,
    native_service_focus_signature: Option<(bool, PaneId, Option<TerminalFocusBlocker>)>,
    /// Advances when a split, close, move, or zoom changes the Pane layout.
    pane_layout_revision: u64,
    /// The layout revision the caption controls last rendered for.
    revealed_layout_revision: u64,
    close_tab_requested: bool,
    /// The Tab's own focus in the Tab bar, where assistive technology can rest on it.
    tab_focus: FocusHandle,
    /// The server Tab this Tab presents, when a SpaceTerm Server owns it.
    shared: Option<SharedPaneBinding>,
    parked_shared_panes: BTreeMap<PaneId, crate::domain::tab::ParkedPane<Entity<TerminalPane>>>,
    directory_recovery: Option<Entity<super::server_pane::DirectoryRecovery>>,
    /// Reads every shared Pane's connection status while the Tab lives.
    _shared_status: Option<gpui::Task<()>>,
}

impl TabView {
    #[cfg(test)]
    pub(crate) fn new(
        tab_id: TabId,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_pane_construction(
            tab_id,
            session_factory,
            PaneConstruction::testing(),
            window,
            cx,
        )
    }

    pub(crate) fn with_pane_construction(
        tab_id: TabId,
        session_factory: WorkspaceTerminalSessionFactory,
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
            pane_surfaces: BTreeMap::new(),
            pane_unselecting_controls: BTreeMap::new(),
            pane_layout_size: None,
            pending_split: None,
            split_bounds: BTreeMap::new(),
            pane_titles: BTreeMap::from([(initial_pane_id, initial_title)]),
            pane_captions: BTreeMap::from([(initial_pane_id, initial_caption)]),
            pane_attention: BTreeMap::from([(initial_pane_id, 0)]),
            resizing_split_id: None,
            pane_drag: None,
            pane_motion: PaneMotion::default(),
            pane_frames: PaneFrames::default(),
            stage_stretch: px(0.0),
            active: true,
            focus_branch_blocker: None,
            native_service_hierarchy_generation: 0,
            native_service_focus_signature: None,
            pane_layout_revision: 0,
            revealed_layout_revision: 0,
            close_tab_requested: false,
            tab_focus: cx.focus_handle(),
            shared: None,
            parked_shared_panes: BTreeMap::new(),
            directory_recovery: None,
            _shared_status: None,
        }
    }

    fn create_terminal(
        pane_id: PaneId,
        session_factory: WorkspaceTerminalSessionFactory,
        pane_construction: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let terminal = cx.new(|cx| pane_construction.create(session_factory, window, cx));
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
                // A Pane whose Terminal Session the server owns keeps its place: the published
                // inventory owns shared membership, and an exit this client hears first could
                // otherwise close a Tab the server has already refilled. A Pane of this client's
                // own closes with its shell, as it always has.
                TerminalPaneEvent::Exited if view.holds_shared_pane_locally(pane_id) => {
                    view.release_shared_pane(pane_id, window, cx);
                }
                TerminalPaneEvent::Exited => view.close_pane(pane_id, window, cx),
            },
        )
        .detach();
        terminal
    }

    /// Draws the Panes across a stage `stretch` wider than the Pane Layout, which keeps its size
    /// while the stage moves with the sidebar.
    pub(crate) fn present_stage_stretch(&mut self, stretch: Pixels, cx: &mut Context<Self>) {
        if self.stage_stretch != stretch {
            self.stage_stretch = stretch;
            cx.notify();
        }
    }

    /// Moves the keyboard to the Focused Pane. A server Pane presenting a screen in place of its
    /// shell takes it on that screen, so Tab reaches the screen's action and typing meant for a
    /// shell reaches nothing. Focusing selects nothing.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        let pane_id = self.tab.focused_pane_id();
        if let Some(focus) = self.shared.as_ref().and_then(|shared| {
            shared
                .view(pane_id)
                .and_then(crate::ui::server_pane::ServerPaneView::screen)
                .and_then(|_| shared.screen_focus(pane_id))
        }) {
            focus.focus(window, cx);
            return;
        }
        let Some(terminal) = self.tab.pane(pane_id) else {
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

    #[cfg(test)]
    pub(crate) fn pane_count(&self) -> usize {
        self.tab.pane_count()
    }

    /// The Root Pane's machine and Repository Source Directory.
    pub(crate) fn repository_source(
        &self,
        cx: &App,
    ) -> Option<(
        crate::repository_status::RepositoryMachine,
        crate::repository_status::scheduler::SourceDirectory,
    )> {
        self.pane_repository_source(self.tab.root_pane_id(), cx)
    }

    pub(crate) fn pane_repository_source(
        &self,
        pane_id: PaneId,
        cx: &App,
    ) -> Option<(
        crate::repository_status::RepositoryMachine,
        crate::repository_status::scheduler::SourceDirectory,
    )> {
        self.tab
            .pane(pane_id)
            .and_then(|terminal| terminal.read(cx).repository_source())
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

    pub(crate) fn tab_focus(&self) -> &FocusHandle {
        &self.tab_focus
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

    /// Opens this Tab for a person, which is an explicit selection of its Focused Pane.
    ///
    /// Only that Pane is asked for: every other Pane of this Tab keeps the grant it already has,
    /// or none. Restoring an Active Tab while publishing membership uses
    /// [`Self::activate_without_focus`], which asks the server for nothing.
    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.open_focused_pane(window, cx);
    }

    /// Opens this Tab's Focused Pane for a person, leaving the focus branch its Workspace owns
    /// where that Workspace set it.
    ///
    /// A Tab strip whose Workspace opens reaches here instead of [`Self::activate`], because the
    /// Workspace decides the focus branch for every one of its Tabs.
    pub(crate) fn open_focused_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_shared_pane(self.tab.focused_pane_id());
        self.focus(window, cx);
    }

    pub(crate) fn activate_without_focus(&mut self, cx: &mut Context<Self>) {
        self.set_focus_branch(true, None, cx);
        cx.notify();
    }

    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.pending_split = None;
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
        for (_, parked) in std::mem::take(&mut self.parked_shared_panes) {
            parked.payload().update(cx, |terminal, _| terminal.close());
        }
    }

    pub(crate) const fn focused_pane_id(&self) -> PaneId {
        self.tab.focused_pane_id()
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
        // Moving the keyboard to a Pane is an explicit selection of it.
        self.select_shared_pane(pane_id);
        self.sync_terminal_focus(cx);
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });
        cx.notify();
        self.focus(window, cx);
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

    /// Splits the Focused Pane after this Tab next measures its Panes, unless that Pane closes first.
    pub(crate) fn split_focused_after_layout(&mut self, axis: SplitAxis, cx: &mut Context<Self>) {
        self.pending_split = Some((self.tab.focused_pane_id(), axis));
        cx.notify();
    }

    fn split_pane(
        &mut self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target_size) = self.split_target_size(target_pane_id, pane_gap(cx)) else {
            eprintln!("cannot split Pane {target_pane_id} without valid measured bounds");
            return;
        };
        if self.shared.is_some() {
            // This client owns its layout. Validate the restored allocation before asking the
            // server for membership; another client's published membership has no such limit.
            if self
                .tab
                .validate_split(target_pane_id, axis, target_size, pane_gap(cx))
                .is_err()
            {
                return;
            }
            if matches!(self.tab.zoom_state(), ZoomState::Zoomed(_)) {
                self.toggle_zoom(window, cx);
            }
            self.split_shared(target_pane_id, axis, window, cx);
            return;
        }
        self.split_pane_measured(target_pane_id, axis, target_size, window, cx);
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
        let Some(pane) = self.pane_surfaces.get(&pane_id).copied() else {
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
                shared: self.shared_caption_controls(pane_id),
            },
            card_width,
            1.0,
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
                self.pane_layout_revision += 1;
                self.pane_motion.expect_change();
                self.split_bounds.clear();
                self.sync_terminal_focus(cx);
                cx.emit(TabViewEvent::PresentationChanged {
                    tab_id: self.tab.id(),
                });
                self.focus(window, cx);
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
        // A raised Pane paints over the others, and later Panes paint over earlier ones where
        // moving Panes overlap.
        let raised = self.pane_frames.raised.map(|(pane_id, _)| pane_id);
        let (target_pane_id, bounds) = raised
            .into_iter()
            .chain(panes.into_iter().rev())
            .filter_map(|candidate| Some((candidate, *self.pane_surfaces.get(&candidate)?)))
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

    fn split_pane_measured(
        &mut self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        target_size: PaneSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_factory = self.session_factory.clone();
        let pane_construction = self.pane_construction.clone();
        let gap = pane_gap(cx);
        let result = self
            .tab
            .split_pane(target_pane_id, axis, target_size, gap, |new_pane_id| {
                Self::create_terminal(new_pane_id, session_factory, pane_construction, window, cx)
            });

        match result {
            Ok(pane_id) => {
                self.advance_native_service_hierarchy_generation(cx);
                self.pane_layout_revision += 1;
                self.pane_motion.expect_change();
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
        // A shared Pane's close goes up to the Workspace Window, which asks the server after Close
        // Confirmation. Nothing here closes it locally.
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
        self.close_pane_focusing(pane_id, true, window, cx);
    }

    /// Closes a Pane. `focus_remaining` hands the keyboard to the Focused Pane that remains, as a
    /// close a person asked for does; a removal another client caused passes `false` unless the
    /// keyboard was in the Pane that left, so typing elsewhere in this window stays where it is.
    fn close_pane_focusing(
        &mut self,
        pane_id: PaneId,
        focus_remaining: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            Ok(ClosePaneOutcome::PaneClosed { closed_pane, .. }) => {
                self.advance_native_service_hierarchy_generation(cx);
                self.pane_layout_revision += 1;
                self.pane_motion.expect_change();
                closed_pane.update(cx, |terminal, _| {
                    terminal.set_accessibility_hierarchy(false);
                    terminal.close();
                });
                self.pane_bounds.remove(&pane_id);
                self.pane_surfaces.remove(&pane_id);
                self.pane_unselecting_controls
                    .retain(|(control_pane_id, _), _| *control_pane_id != pane_id);
                self.split_bounds.clear();
                self.pane_titles.remove(&pane_id);
                self.pane_captions.remove(&pane_id);
                self.pane_attention.remove(&pane_id);
                self.forget_shared_pane(pane_id);
                self.sync_terminal_focus(cx);
                cx.emit(TabViewEvent::PresentationChanged {
                    tab_id: self.tab.id(),
                });
                cx.notify();
                if self.active && focus_remaining {
                    self.focus(window, cx);
                }
            }
            Err(error) => eprintln!("failed to close Pane: {error}"),
        }
    }

    pub(crate) fn toggle_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let zoomed = match self.tab.zoom_state() {
            ZoomState::Restored => self.tab.focused_pane_id(),
            ZoomState::Zoomed(pane_id) => pane_id,
        };
        if self.tab.toggle_zoom().is_none() {
            return;
        }
        self.advance_native_service_hierarchy_generation(cx);
        self.pane_layout_revision += 1;
        self.pane_motion.expect_raise(zoomed);
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
            Ok(_) => {
                self.pane_motion.expect_change();
                cx.notify();
            }
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
                terminal
                    .set_accessibility_hierarchy(self.active && presented_panes.contains(&pane_id));
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
        // Zoom explicitly selects its Pane. Splits validate the restored allocation before
        // asking for membership or starting a shell; Close captures its exact targets first.
        if matches!(action, PaneCaptionAction::ToggleZoom) {
            self.select_shared_pane(pane_id);
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
            // The raised Pane draws above the layout, so its place in the layout stays empty.
            PaneNodeRef::Leaf { pane_id }
                if self
                    .pane_frames
                    .raised
                    .is_some_and(|(raised, _)| raised == pane_id) =>
            {
                div().size_full().min_w_0().min_h_0().into_any_element()
            }
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

    /// Each Pane's hover and controls reveal, read once per frame.
    ///
    /// The Focused Pane always shows its caption controls. Another Pane shows them under the
    /// pointer while the window is active. A reveal that changes with the Pane layout, such as
    /// focus moving to a new split or a Pane leaving the pointer, takes effect with the layout
    /// instead of easing after it.
    fn pane_hovers(
        &mut self,
        window_active: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> PaneHovers {
        let focused_pane_id = self.tab.focused_pane_id();
        let animate = self.revealed_layout_revision == self.pane_layout_revision;
        self.revealed_layout_revision = self.pane_layout_revision;
        self.tab
            .panes_with_ids()
            .map(|(pane_id, _)| {
                let fade = HoverFade::new(("pane-hover", pane_id.get()), window, cx);
                let revealed = pane_id == focused_pane_id || (window_active && fade.is_hovered(cx));
                let reveal = spaceterm_ui::eased_flag(
                    ("pane-controls-reveal", pane_id.get()),
                    revealed,
                    animate && !fade.moved_by_layout(cx),
                    window,
                    cx,
                );
                (pane_id, (fade, reveal))
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
        let shared_surface = self.shared_pane_surface(pane_id, &view, cx);
        let has_multiple_panes = self.tab.pane_count() > 1;
        let zoomed = matches!(self.tab.zoom_state(), ZoomState::Zoomed(_));
        let text = self
            .pane_captions
            .get(&pane_id)
            .cloned()
            .unwrap_or_default();
        let hover = hovers.get(&pane_id).cloned();
        let controls_reveal = hover
            .as_ref()
            .map_or(if focused { 1.0 } else { 0.0 }, |(_, reveal)| *reveal);
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

        // The Pane is drawn inside its laid-out place, which it fills at rest. While the layout
        // changes it moves on its own, and its Terminal keeps the size of the place, so the
        // Terminal Session resizes once per change rather than once per frame.
        let inset = self.pane_frames.inset(pane_id);
        // A raised Pane casts no shadow: its surface is translucent, so a shadow beneath it would
        // darken all of it.
        let raised = self
            .pane_frames
            .raised
            .is_some_and(|(raised, _)| raised == pane_id);
        let surface = div()
            .id(("pane", pane_id.get()))
            .debug_selector(move || format!("pane-surface-{}", pane_id.get()))
            .absolute()
            .top(inset.top)
            .right(inset.right)
            .bottom(inset.bottom)
            .left(inset.left)
            .flex()
            .flex_col()
            .overflow_hidden()
            // A moving Pane's surface reaches past its Terminal, so a press there also moves input
            // focus, as a press on the caption does.
            .capture_any_mouse_down(move |event: &MouseDownEvent, window, cx| {
                let _ = focus_view.update(cx, |view, cx| {
                    // A press is an explicit Pane selection, so a shared Pane asks the server for
                    // itself. Focus a Pane regains on its own never does. Neither does a press on
                    // Split, which validates the restored allocation first, or on Close, which
                    // only asks for confirmation.
                    let on_unselecting_control = view
                        .pane_unselecting_controls
                        .range(
                            (pane_id, PaneCaptionAction::SplitRight)
                                ..=(pane_id, PaneCaptionAction::Close),
                        )
                        .any(|(_, control)| control.is_hovered_at(event.position, window));
                    if !on_unselecting_control {
                        view.select_shared_pane(pane_id);
                    }
                    if view.tab.focused_pane_id() != pane_id {
                        view.focus_pane(pane_id, cx);
                        view.focus(window, cx);
                    }
                });
            })
            .rounded(frame.pane_radius())
            // A moving Pane can overlap the Panes painted before it, which take no pointer input
            // through it.
            .when(raised || self.pane_frames.moving, |surface| {
                surface.occlude()
            })
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
                    shared: self.shared_caption_controls(pane_id),
                },
                controls_reveal,
                view.clone(),
                appearance.clone(),
            ))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .mr(-(inset.left + inset.right))
                    .mb(-(inset.top + inset.bottom))
                    .overflow_hidden()
                    // A shared Pane's screen replaces its terminal outright, so no Screen, title,
                    // or accessible text of a former shell paints behind it.
                    .map(|body| match shared_surface.screen {
                        Some(screen) => body.child(screen),
                        None => body.child(terminal),
                    })
                    .children(shared_surface.notice),
            )
            .child(render_pane_corner_surface(pane_id, frame, appearance))
            // The hairline paints last so the caption, Terminal and corner mask never cover it.
            .child(pane_rim)
            .when_some(drop_edge, |leaf, edge| {
                leaf.child(render_pane_drop_target(pane_id, edge, radius, appearance))
            })
            .when_some(hover, |leaf, (fade, _)| leaf.child(fade.tracker()));

        div()
            .on_children_prepainted(move |children, _, cx| {
                let [place, surface] = children.as_slice() else {
                    return;
                };
                let _ = measure_view.update(cx, |view, _| {
                    view.pane_bounds.insert(pane_id, *place);
                    view.pane_surfaces.insert(pane_id, *surface);
                });
            })
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(div().absolute().inset_0())
            .child(surface)
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
            .role(gpui::accesskit::Role::Group)
            .a11y_synthetic_children(|builder| {
                // Keep the pointer target above both sides while reading the splitter between
                // them. Retaining the Split's group also confines its platform contents.
                let node = builder.parent_node();
                if let [first, second, splitter] = node.children() {
                    let children = [*first, *splitter, *second];
                    node.set_children(children);
                }
            })
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex();
        split = match axis {
            SplitAxis::Horizontal => split.flex_row(),
            SplitAxis::Vertical => split.flex_col(),
        };

        let gap = pane_gap(cx);
        let bounds = self.split_bounds.get(&split_id).copied();
        let extent = bounds.and_then(|bounds| split_content_extent(axis, bounds, gap));
        let current_offset = extent.map_or(0.0, |extent| extent * ratio);
        // A ratio left outside the bounds by a smaller window stays inside the published range.
        let offset_range = bounds.zip(extent).and_then(|(bounds, extent)| {
            let range = self
                .tab
                .split_ratio_range(split_id, pane_size(bounds).ok()?, gap)
                .ok()?;
            Some(extent * range.start().min(ratio)..=extent * range.end().max(ratio))
        });

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
                offset_range,
                !self.pane_frames.moving,
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
        // Moving Panes reach outside their laid-out places and across the gap between them. A
        // raised Pane moves above the layout instead, which stays as it is unless the stage
        // stretches it.
        let moving = self.stage_stretch != px(0.0)
            || (self.pane_frames.moving && self.pane_frames.raised.is_none());
        split
            .child(split_child("split-first", first, axis, ratio, !moving))
            .child(spacer.when(!moving, |spacer| {
                spacer.bg(gpui_color(appearance.surface(
                    crate::appearance::SurfaceRole::Base,
                    super::workspace_frame::base_surface(&appearance.colors),
                )))
            }))
            .child(split_child(
                "split-second",
                second,
                axis,
                1.0 - ratio,
                !moving,
            ))
            .child(resize_target)
            .into_any_element()
    }
}

/// Shared server Panes: how this Tab presents one server Tab's Panes, submits its edits, and
/// refuses the operations the server path does not support yet.
impl TabView {
    pub(crate) fn canonicalize_shared_ids(&mut self) {
        if let Some(shared) = self.shared.as_mut() {
            shared.canonicalize();
        }
    }

    pub(crate) fn shared_layout(&self) -> Option<spaceterm_server::client_view::Layout> {
        use spaceterm_server::client_view::{Layout, LayoutNode};
        use spaceterm_server::hierarchy::Axis;
        fn collect(
            tree: PaneTreeRef<'_>,
            shared: &super::shared_workspaces::SharedPanes,
            nodes: &mut Vec<LayoutNode>,
        ) -> Option<usize> {
            let index = nodes.len();
            nodes.push(LayoutNode::Pane([0; 32]));
            nodes[index] = match tree.node() {
                PaneNodeRef::Leaf { pane_id } => LayoutNode::Pane(shared.server_pane(pane_id)?),
                PaneNodeRef::Split {
                    axis,
                    ratio,
                    first,
                    second,
                    ..
                } => LayoutNode::Split {
                    axis: match axis {
                        SplitAxis::Horizontal => Axis::Right,
                        SplitAxis::Vertical => Axis::Down,
                    },
                    ratio,
                    first: collect(first, shared, nodes)?,
                    second: collect(second, shared, nodes)?,
                },
            };
            Some(index)
        }
        let mut nodes = Vec::new();
        collect(self.tab.root(), self.shared.as_ref()?, &mut nodes)?;
        Some(Layout { nodes })
    }

    pub(crate) fn restore_shared_layout(
        &mut self,
        layout: &spaceterm_server::client_view::Layout,
        cx: &mut Context<Self>,
    ) {
        use crate::domain::tab::PaneArrangement;
        use spaceterm_server::client_view::{Layout, LayoutNode};
        use spaceterm_server::hierarchy::Axis;
        fn convert(
            layout: &Layout,
            index: usize,
            shared: &super::shared_workspaces::SharedPanes,
        ) -> Option<PaneArrangement> {
            match layout.nodes.get(index)? {
                LayoutNode::Pane(id) => Some(PaneArrangement::Pane(shared.local_pane(*id)?)),
                LayoutNode::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => Some(PaneArrangement::Split {
                    axis: match axis {
                        Axis::Right => SplitAxis::Horizontal,
                        Axis::Down => SplitAxis::Vertical,
                    },
                    ratio: *ratio,
                    first: Box::new(convert(layout, *first, shared)?),
                    second: Box::new(convert(layout, *second, shared)?),
                }),
            }
        }
        if layout.validate().is_err() {
            return;
        }
        let Some(arrangement) = self
            .shared
            .as_ref()
            .and_then(|shared| convert(layout, 0, shared))
        else {
            return;
        };
        if self.tab.restore_arrangement(arrangement).is_err() {
            return;
        }
        self.advance_native_service_hierarchy_generation(cx);
        self.pane_layout_revision += 1;
        self.pane_motion.expect_change();
        self.split_bounds.clear();
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });
        cx.notify();
    }

    /// Presents one server Tab in this Tab. The Pane this Tab already owns becomes the server's
    /// Root Pane, and every later Pane arrives with the server's published membership.
    pub(crate) fn adopt_shared(
        &mut self,
        mut shared: super::shared_workspaces::SharedPanes,
        root_pane: spaceterm_server::hierarchy::Id,
        root_state: std::sync::Arc<dyn crate::ui::server_pane::ServerPaneConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        shared.register(
            self.tab.root_pane_id(),
            root_pane,
            root_state,
            cx.focus_handle(),
        );
        self.shared = Some(shared);
        self._shared_status = Some(cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(super::shared_workspaces::POLL_INTERVAL)
                    .await;
                if view
                    .update_in(cx, |view, window, cx| view.sync_shared_panes(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        self.sync_shared_panes(window, cx);
    }

    /// Adds the Panes the server published that this Tab does not present yet.
    ///
    /// Each Pane takes its place beside the Pane it was split from, in this client's own layout.
    /// The arrangement that survives, the Focused Pane, and the Zoomed Pane all stay as they are,
    /// because another client's creation is membership and not a view change here.
    pub(crate) fn reconcile_shared(
        &mut self,
        server_tab: &spaceterm_server::hierarchy::Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use spaceterm_server::hierarchy::Axis;

        let Some(shared) = self.shared.as_mut() else {
            return false;
        };
        shared.canonicalize();
        // Every Pane the server published joins before any Pane it removed leaves. One poll can
        // carry both a creation and the removal of what it was created beside, and a Tab that
        // lost its last known Pane first would lose the Panes that replaced it.
        let mut changed = false;
        for pane in &server_tab.panes {
            let Some(shared) = self.shared.as_ref() else {
                break;
            };
            if let Some(pane_id) = shared.local_pane(pane.id) {
                if self.tab.pane(pane_id).is_none()
                    && let Some(parked) = self.parked_shared_panes.remove(&pane_id)
                {
                    let source = pane
                        .source
                        .and_then(|id| shared.local_pane(id))
                        .filter(|id| self.tab.pane(*id).is_some())
                        .unwrap_or_else(|| self.tab.root_pane_id());
                    let axis = match pane.axis {
                        Axis::Right => SplitAxis::Horizontal,
                        Axis::Down => SplitAxis::Vertical,
                    };
                    if self.tab.restore_parked_pane(parked, source, axis).is_ok() {
                        self.finish_shared_insertion(pane_id, cx);
                        changed = true;
                    }
                }
                let Some(shared) = self.shared.as_ref() else {
                    continue;
                };
                if shared.needs_binding(pane_id, pane.id) {
                    let Some((factory, state)) = shared.sessions().open(pane.id) else {
                        continue;
                    };
                    let held_keyboard = self.tab.pane(pane_id).is_some_and(|terminal| {
                        self.shared_pane_holds_focus(pane_id, terminal, window, cx)
                    });
                    if let Some(terminal) = self.tab.pane(pane_id) {
                        terminal
                            .update(cx, |terminal, cx| terminal.bind_pending_server(factory, cx));
                    }
                    if let Some(shared) = self.shared.as_mut() {
                        let focus = shared
                            .screen_focus(pane_id)
                            .cloned()
                            .unwrap_or_else(|| cx.focus_handle());
                        shared.register(pane_id, pane.id, state, focus);
                    }
                    // Binding supplies membership. Only the focus still on this provisional Pane
                    // asks for first use; sidebar, Find, chooser and later navigation keep focus.
                    if self.active && self.tab.focused_pane_id() == pane_id && held_keyboard {
                        self.select_shared_pane(pane_id);
                        self.focus(window, cx);
                    }
                    changed = true;
                }
                continue;
            }
            // The Pane this one was split from may already be gone, and the server clears the
            // link when it removes a Pane. The arrangement is this client's own, so a Pane with
            // no source it still presents takes its place beside this Tab's Root Pane.
            let source = pane
                .source
                .and_then(|source| shared.local_pane(source))
                .unwrap_or_else(|| self.tab.root_pane_id());
            let Some((session_factory, state)) = shared.sessions().open(pane.id) else {
                continue;
            };
            let pane_construction = shared.sessions().panes().clone();
            let axis = match pane.axis {
                Axis::Right => SplitAxis::Horizontal,
                Axis::Down => SplitAxis::Vertical,
            };
            let inserted = self.tab.insert_shared_pane(source, axis, |pane_id| {
                Self::create_terminal(pane_id, session_factory, pane_construction, window, cx)
            });
            match inserted {
                Ok(pane_id) => {
                    if let Some(shared) = self.shared.as_mut() {
                        shared.register(pane_id, pane.id, state, cx.focus_handle());
                    }
                    self.finish_shared_insertion(pane_id, cx);
                    changed = true;
                }
                Err(error) => eprintln!("failed to present a shared Pane: {error}"),
            }
        }
        changed |= self.remove_absent_shared_panes(server_tab, window, cx);
        let retired = self
            .parked_shared_panes
            .keys()
            .copied()
            .filter(|id| {
                self.shared
                    .as_ref()
                    .is_none_or(|shared| !shared.closing(*id))
            })
            .collect::<Vec<_>>();
        for id in retired {
            if let Some(parked) = self.parked_shared_panes.remove(&id) {
                parked.payload().update(cx, |terminal, _| terminal.close());
                self.forget_shared_pane(id);
            }
        }
        if changed {
            self.sync_shared_panes(window, cx);
        }
        changed
    }

    /// Removes the Panes the server no longer has.
    ///
    /// The server already removed them, so this is not a close a person asked for. An inactive
    /// Pane may never have connected, so membership alone decides this rather than a Terminal
    /// Session reporting its own end.
    fn remove_absent_shared_panes(
        &mut self,
        server_tab: &spaceterm_server::hierarchy::Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(shared) = self.shared.as_ref() else {
            return false;
        };
        let absent: Vec<PaneId> = shared
            .pane_ids()
            .into_iter()
            .filter(|&pane_id| {
                self.tab.pane(pane_id).is_some()
                    && shared.server_pane(pane_id).is_some_and(|server| {
                        !server_tab.panes.iter().any(|pane| pane.id == server)
                    })
            })
            .collect();
        let mut removed = false;
        for pane_id in absent {
            // The keyboard moves to the Pane that remains only when it was in the Pane that left,
            // as it does when a split's shell exits. A person typing in the sidebar, the Tab
            // strip, or another Pane keeps typing there.
            let held_keyboard = self.tab.pane(pane_id).is_some_and(|terminal| {
                self.shared_pane_holds_focus(pane_id, terminal, window, cx)
            });
            if self
                .shared
                .as_ref()
                .is_some_and(|shared| shared.closing(pane_id))
            {
                if let Ok(Some(parked)) = self.tab.park_pane(pane_id) {
                    parked.payload().update(cx, |terminal, cx| {
                        terminal.set_product_focus(
                            TerminalProductFocus {
                                active_workspace: false,
                                active_tab: false,
                                pane_visible: false,
                                focused_pane: false,
                                blocker: None,
                            },
                            cx,
                        );
                        terminal.set_accessibility_hierarchy(false);
                    });
                    self.parked_shared_panes.insert(pane_id, parked);
                    self.advance_native_service_hierarchy_generation(cx);
                    self.pane_layout_revision += 1;
                    self.pane_motion.expect_change();
                    self.split_bounds.clear();
                    self.sync_terminal_focus(cx);
                    if self.active && held_keyboard {
                        self.focus(window, cx);
                    }
                    cx.emit(TabViewEvent::PresentationChanged {
                        tab_id: self.tab.id(),
                    });
                    cx.notify();
                }
            } else {
                self.forget_shared_pane(pane_id);
                self.close_pane_focusing(pane_id, held_keyboard, window, cx);
            }
            removed = true;
        }
        removed
    }

    /// Delivers the Focused Pane's exit, as its Terminal Session does when the server ends it.
    #[cfg(test)]
    pub(crate) fn exit_focused_pane_for_test(&mut self, cx: &mut Context<Self>) {
        let pane_id = self.tab.focused_pane_id();
        if let Some(terminal) = self.tab.pane(pane_id).cloned() {
            terminal.update(cx, |_, cx| cx.emit(TerminalPaneEvent::Exited));
        }
    }

    /// Whether a SpaceTerm Server owns the Pane's Terminal Session, so the published inventory
    /// owns whether the Pane keeps its place.
    fn holds_shared_pane_locally(&self, pane_id: PaneId) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|shared| shared.server_pane(pane_id).is_some())
    }

    /// The server ended a shared Pane's Terminal Session.
    ///
    /// The Pane keeps its place until the server publishes an inventory without it, so a Tab this
    /// client has not caught up with never closes from membership it no longer knows. Its screen
    /// and title belonged to the shell that ended, so nothing of them paints meanwhile.
    fn release_shared_pane(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tab.pane(pane_id).cloned() else {
            return;
        };
        terminal.update(cx, |terminal, cx| {
            terminal.set_server_connection_interrupted(true, window, cx);
            terminal.clear_server_presentation(cx);
            terminal.present_server_metadata(None, cx);
        });
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });
        cx.notify();
    }

    /// The server identity of one shared Pane this Tab presents.
    pub(crate) fn shared_server_pane(
        &self,
        pane_id: PaneId,
    ) -> Option<spaceterm_server::hierarchy::Id> {
        self.shared.as_ref()?.server_pane(pane_id)
    }

    /// Presents the Repository Status the server read for each shared Pane's directory.
    pub(crate) fn present_shared_repositories(
        &mut self,
        view: &dyn Fn(spaceterm_server::hierarchy::Id) -> crate::repository_status::RepositoryView,
        cx: &mut Context<Self>,
    ) {
        let panes: Vec<_> = self
            .tab
            .panes_with_ids()
            .filter_map(|(pane_id, terminal)| {
                Some((self.shared_server_pane(pane_id)?, terminal.clone()))
            })
            .collect();
        for (server, terminal) in panes {
            let presented = view(server);
            terminal.update(cx, |terminal, cx| {
                terminal.present_server_repository(presented, cx);
            });
        }
    }

    /// Presents the Terminal Metadata the server published for each shared Pane, including Panes
    /// this client never painted. `published` names the Session run and its latest metadata, or
    /// `None` while the Pane runs no Session.
    pub(crate) fn present_shared_metadata<'p>(
        &mut self,
        published: &dyn Fn(
            spaceterm_server::hierarchy::Id,
        ) -> Option<(
            crate::ui::terminal_pane::ServerSessionRun,
            &'p spaceterm_server::pane::metadata::Snapshot,
        )>,
        cx: &mut Context<Self>,
    ) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        let panes: Vec<_> = self
            .tab
            .panes_with_ids()
            .filter_map(|(pane_id, terminal)| {
                Some((shared.server_pane(pane_id)?, terminal.clone()))
            })
            .collect();
        for (server, terminal) in panes {
            let presented = match published(server) {
                Some((run, wire)) => {
                    if terminal
                        .read(cx)
                        .presents_server_metadata(run, wire.revision)
                    {
                        continue;
                    }
                    // A publication this route cannot decode changes nothing.
                    let Some(metadata) = shared.sessions().metadata(wire) else {
                        continue;
                    };
                    Some((run, metadata))
                }
                None => None,
            };
            terminal.update(cx, |terminal, cx| {
                terminal.present_server_metadata(presented, cx);
            });
        }
    }

    /// Whether this client's own pending close ends every Pane this Tab presents.
    pub(crate) fn shared_closing(&self) -> bool {
        self.shared.as_ref().is_some_and(|shared| {
            self.tab
                .panes_with_ids()
                .all(|(pane_id, _)| shared.closing(pane_id))
        })
    }

    /// Whether this Tab presents the server Pane.
    pub(crate) fn holds_shared_pane(&self, server: spaceterm_server::hierarchy::Id) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|shared| shared.holds(server))
    }

    pub(crate) fn shared_pane_closes_directly(
        &self,
        server: spaceterm_server::hierarchy::Id,
        live_session: bool,
        cx: &App,
    ) -> Option<bool> {
        use crate::terminal::server_session::DevelopmentPaneStatus;
        let shared = self.shared.as_ref()?;
        let pane = shared.local_pane(server)?;
        let status = shared.view(pane)?.connection().status();
        if matches!(
            status,
            DevelopmentPaneStatus::Takeover
                | DevelopmentPaneStatus::TakeoverReconnecting
                | DevelopmentPaneStatus::TakeoverFailed(_)
                | DevelopmentPaneStatus::TakingOver
        ) {
            return Some(false);
        }
        Some(if live_session {
            status == DevelopmentPaneStatus::Connected
                && !self
                    .tab
                    .pane(pane)?
                    .read(cx)
                    .close_facts()
                    .requires_confirmation()
        } else {
            matches!(
                status,
                DevelopmentPaneStatus::Connected
                    | DevelopmentPaneStatus::Restored
                    | DevelopmentPaneStatus::SessionEnded
                    | DevelopmentPaneStatus::Ended
            )
        })
    }

    /// Whether the keyboard rests anywhere in this Tab: its Tab strip item, one of its Panes, or
    /// the screen replacing one of them.
    pub(crate) fn holds_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.tab_focus.contains_focused(window, cx)
            || self.tab.panes_with_ids().any(|(pane_id, terminal)| {
                self.shared_pane_holds_focus(pane_id, terminal, window, cx)
            })
    }

    /// Focuses the Pane one accepted split of this client's own created, and asks the server for
    /// it. Only the client that asked for the split does this.
    pub(crate) fn focus_shared_pane_explicitly(
        &mut self,
        server: spaceterm_server::hierarchy::Id,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane_id) = self
            .shared
            .as_ref()
            .and_then(|shared| shared.local_pane(server))
        else {
            return;
        };
        self.select_shared_pane(pane_id);
        self.focus_pane(pane_id, cx);
        self.focus(window, cx);
    }

    /// The bookkeeping a Pane needs once it joins the layout, without taking focus or zoom from
    /// the Pane a person is working in.
    fn finish_shared_insertion(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        self.advance_native_service_hierarchy_generation(cx);
        self.pane_layout_revision += 1;
        self.pane_motion.expect_change();
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
    }

    /// Presents a connected split immediately and queues its bounded membership request.
    fn split_shared(
        &mut self,
        source: PaneId,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedSubmission;
        use spaceterm_server::hierarchy::Axis;

        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        let axis = match axis {
            SplitAxis::Horizontal => Axis::Right,
            SplitAxis::Vertical => Axis::Down,
        };
        let edits = shared.edits();
        if let SharedSubmission::Refused(reason) = shared.split(source, axis) {
            self.report_shared_refusal("create a Pane", reason, window, cx);
        } else {
            let presentation = edits.take_presentation();
            let tab = self
                .shared
                .as_ref()
                .and_then(|shared| shared.presentation_tab());
            let target = presentation.map(|event| edits.canonical(event.target));
            if let Some(tab) = tab {
                self.reconcile_shared(&tab, window, cx);
            }
            if let Some(target) = target {
                // The initiating interaction owns this selection. Its acknowledgement owns none.
                self.focus_shared_pane_explicitly(target, window, cx);
            }
        }
        cx.notify();
    }

    /// Explains a shared edit the client could not submit. Nothing was sent, and nothing waits.
    fn report_shared_refusal(
        &self,
        operation: &str,
        reason: super::shared_workspaces::SharedRefusalReason,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detail = format!(
            "SpaceTerm did not {operation} in this shared Workspace. {}",
            reason.explanation()
        );
        let _ = Alert::new(
            ModalId::new("shared-pane-edit-unavailable"),
            "Shared change unavailable",
            "Shared Change Unavailable",
            detail,
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "shared-pane-edit-unavailable-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }

    /// Reads every shared Pane's connection and presents what changed.
    fn sync_shared_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane_ids) = self
            .shared
            .as_ref()
            .map(super::shared_workspaces::SharedPanes::pane_ids)
        else {
            return;
        };
        let mut changed = false;
        for pane_id in pane_ids {
            if self
                .shared
                .as_ref()
                .is_some_and(|shared| shared.closing(pane_id))
            {
                continue;
            }
            let Some(sync) = self
                .shared
                .as_mut()
                .and_then(|shared| shared.view_mut(pane_id))
                .map(crate::ui::server_pane::ServerPaneView::sync)
            else {
                continue;
            };
            if !sync.changed {
                continue;
            }
            changed = true;
            self.apply_shared_sync(pane_id, sync, window, cx);
        }
        if changed {
            cx.notify();
        }
    }

    fn apply_shared_sync(
        &mut self,
        pane_id: PaneId,
        sync: crate::ui::server_pane::ServerPaneSync,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tab.pane(pane_id).cloned() else {
            return;
        };
        // Read the keyboard's owner before the presentation moves, because only the focus this
        // Pane already holds moves with it. A sidebar rename, the Workspace Switcher, and
        // Terminal Find keep the keys a person is typing, whatever the server publishes here.
        let pane_holds_focus = self.shared_pane_holds_focus(pane_id, &terminal, window, cx);
        terminal.update(cx, |terminal, cx| {
            terminal.set_server_connection_interrupted(sync.interrupted, window, cx);
            if sync.clear_presentation {
                terminal.clear_server_presentation(cx);
            }
        });
        if !self.active || self.tab.focused_pane_id() != pane_id || !pane_holds_focus {
            return;
        }
        let screen = self
            .shared
            .as_ref()
            .and_then(|shared| shared.view(pane_id))
            .and_then(crate::ui::server_pane::ServerPaneView::screen);
        match screen {
            // Focus rests on the screen, so typing meant for a former shell reaches nothing and
            // never takes control back by accident. Tab from here reaches the screen's action.
            Some(_) if sync.entering_screen => {
                if let Some(focus) = self
                    .shared
                    .as_ref()
                    .and_then(|shared| shared.screen_focus(pane_id))
                {
                    focus.focus(window, cx);
                }
            }
            // The server publishes this client's screens again, so typing returns to the Pane.
            None if !sync.interrupted => {
                terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
            }
            _ => {}
        }
    }

    /// Asks the server for this Pane, which starts or acquires only this Pane.
    ///
    /// Only an explicit selection reaches here: a press on the Pane, a caption control other than
    /// Close, keyboard Pane navigation, or opening its Tab. Close, focus a Pane regains on its
    /// own, window restoration, and membership publication never do, so none of them starts a
    /// retained shell. An unreachable server is asked for nothing at all.
    fn select_shared_pane(&self, pane_id: PaneId) {
        let Some(shared) = self.shared.as_ref().filter(|shared| shared.reachable()) else {
            return;
        };
        if let Some(view) = shared.view(pane_id) {
            view.select();
        }
    }

    fn forget_shared_pane(&mut self, pane_id: PaneId) {
        if let Some(shared) = self.shared.as_mut() {
            shared.forget(pane_id);
        }
    }

    fn shared_caption_controls(&self, pane_id: PaneId) -> Option<SharedCaptionControls> {
        let shared = self.shared.as_ref()?;
        shared.server_pane(pane_id)?;
        let editability = shared.editability();
        Some(SharedCaptionControls {
            edits: editability.available(),
            edits_reason: Some(editability.explanation()),
        })
    }

    /// What replaces and overlays a shared Pane: the screen for a Pane this client cannot present
    /// authoritatively, and the notice for an interruption over a frozen screen.
    fn shared_pane_surface(
        &self,
        pane_id: PaneId,
        view: &gpui::WeakEntity<Self>,
        cx: &App,
    ) -> SharedPaneSurface {
        use crate::ui::server_pane::{self, ServerPaneScreen};
        use spaceterm_ui::Button;

        let Some(pane) = self.shared.as_ref().and_then(|shared| shared.view(pane_id)) else {
            return SharedPaneSurface::default();
        };
        let selector = |suffix: &str| {
            gpui::SharedString::from(format!("shared-pane-{suffix}-{}", pane_id.get()))
        };
        let screen_focus = self
            .shared
            .as_ref()
            .and_then(|shared| shared.screen_focus(pane_id))
            .cloned();
        let screen = pane.screen().map(|screen| match screen {
            ServerPaneScreen::Takeover {
                taking_over,
                unconfirmed,
                offered,
                ended,
                controller,
                dormant,
            } => {
                let status = if taking_over {
                    Some(server_pane::progress_status(
                        selector("take-over-status"),
                        selector("take-over-progress"),
                        server_pane::TAKING_OVER.into(),
                        cx,
                    ))
                } else if unconfirmed {
                    Some(server_pane::note_status(
                        selector("take-over-status"),
                        server_pane::TAKE_OVER_UNCONFIRMED.into(),
                        cx,
                    ))
                } else {
                    None
                };
                let (title, detail) = if ended {
                    (
                        server_pane::SESSION_ENDED_TITLE.into(),
                        server_pane::SESSION_ENDED_DETAIL,
                    )
                } else if offered || taking_over {
                    server_pane::takeover_copy(controller.as_ref(), dormant)
                } else {
                    // SpaceTerm stopped reconnecting, so the screen claims nothing about the shell
                    // or who controls it.
                    (
                        server_pane::TAKEOVER_TITLE.into(),
                        server_pane::TAKEOVER_FAILED_DETAIL,
                    )
                };
                // The Session already ended, so nothing here can take it over.
                let action = (!ended).then(|| {
                    let owner = view.clone();
                    Button::new(selector("take-over"), "Take Over")
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Regular)
                        .disabled(!offered)
                        .tab_stop(true)
                        .accessibility_description(
                            "Moves typing and this Pane's size to this SpaceTerm.",
                        )
                        .debug_selector(selector("take-over"))
                        .on_activate(move |_, window, cx| {
                            let _ = owner.update(cx, |view, cx| {
                                view.take_over_shared_pane(pane_id, window, cx);
                            });
                        })
                });
                server_pane::render_screen(
                    selector("takeover"),
                    server_pane::ScreenCopy::new(title, detail.into()),
                    // Losing control, a new Controlling Client, and the end of a Session all happen
                    // without any action here, so assistive technology announces them.
                    true,
                    action,
                    status,
                    pane.announcements(),
                    cx,
                )
            }
            ServerPaneScreen::Restored {
                starting,
                unconfirmed,
                offered,
                ended,
            } => {
                let copy = server_pane::DormantCopy::of(ended);
                let status = if starting {
                    Some(server_pane::progress_status(
                        selector(&format!("{}-status", copy.action)),
                        selector(&format!("{}-progress", copy.action)),
                        copy.progress.into(),
                        cx,
                    ))
                } else if unconfirmed {
                    Some(server_pane::note_status(
                        selector(&format!("{}-status", copy.action)),
                        copy.unconfirmed.into(),
                        cx,
                    ))
                } else {
                    None
                };
                let owner = view.clone();
                let action = Button::new(selector(copy.action), copy.label)
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Regular)
                    .disabled(!offered)
                    .tab_stop(true)
                    .accessibility_description(copy.description)
                    .debug_selector(selector(copy.action))
                    .on_activate(move |_, window, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            view.start_shared_shell(pane_id, window, cx);
                        });
                    });
                server_pane::render_screen(
                    selector(copy.screen),
                    server_pane::ScreenCopy::new(copy.title.into(), copy.detail.into()),
                    true,
                    Some(action),
                    status,
                    pane.announcements(),
                    cx,
                )
            }
            ServerPaneScreen::StartUnknown {
                checking,
                unanswered,
            } => {
                // This client restarts this computer's server and an SSH server from Server
                // Settings. A direct network server restarts only on its own computer.
                let sessions = self.shared.as_ref().map(|shared| shared.sessions());
                let restart_target = match sessions {
                    Some(sessions) if sessions.direct_route().is_some() => None,
                    Some(sessions) => Some(sessions.remote_destination().cloned().map_or(
                        crate::ui::MachineTarget::Local,
                        crate::ui::MachineTarget::Ssh,
                    )),
                    None => None,
                };
                let owner = view.clone();
                server_pane::StartUnknownScreen {
                    screen: selector("start-unknown"),
                    retry: selector("check-start"),
                    server_settings: restart_target
                        .as_ref()
                        .map(|_| selector("start-unknown-server-settings")),
                    checking,
                    unanswered,
                    on_retry: move |window: &mut Window, cx: &mut App| {
                        let _ = owner.update(cx, |view, cx| {
                            view.retry_shared_pane(pane_id, window, cx);
                        });
                    },
                    // Settings reads the server again, and Restart Server asks before it ends
                    // anything. Nothing here restarts it.
                    on_server_settings: move |_: &mut Window, cx: &mut App| {
                        if let Some(target) = restart_target.clone() {
                            crate::ui::settings_window::open_server_settings(target, cx);
                        }
                    },
                }
                .render(pane.announcements(), cx)
            }
            ServerPaneScreen::DirectoryUnavailable { starting, offered } => {
                let target = pane.connection().recovery_target();
                let editability = self.shared.as_ref().map(|shared| shared.editability());
                let retry_owner = view.clone();
                let choose_owner = view.clone();
                let actions = server_pane::DirectoryActions {
                    retry: selector("directory-retry"),
                    choose: selector("choose-directory"),
                    enabled: offered && !starting && !self.directory_recovery_is_open(cx),
                    offer: match editability.filter(|_| target.is_some()) {
                        Some(editability) if editability.available() => {
                            server_pane::ChooseDirectory::Offered
                        }
                        Some(editability) => {
                            server_pane::ChooseDirectory::Withheld(editability.explanation())
                        }
                        None => server_pane::ChooseDirectory::Unsupported,
                    },
                    on_retry: move |window: &mut Window, cx: &mut App| {
                        let _ = retry_owner
                            .update(cx, |view, cx| view.start_shared_shell(pane_id, window, cx));
                    },
                    on_choose: move |window: &mut Window, cx: &mut App| {
                        let _ = choose_owner.update(cx, |view, cx| {
                            view.choose_shared_directory(pane_id, window, cx);
                        });
                    },
                };
                let status = server_pane::directory_status(
                    selector("directory-status"),
                    selector("directory-progress"),
                    starting,
                    pane.directory_note(),
                    cx,
                );
                let missing_worktree = target.as_ref().is_some_and(|target| {
                    self.shared.as_ref().is_some_and(|shared| {
                        shared.missing_worktree_for_directory(&target.original)
                    })
                });
                server_pane::render_directory_screen(
                    selector("directory-unavailable"),
                    target.and_then(|target| {
                        target
                            .original
                            .to_str()
                            .map(|path| gpui::SharedString::from(path.to_owned()))
                    }),
                    actions.buttons(),
                    status,
                    missing_worktree,
                    pane.announcements(),
                    cx,
                )
            }
        });
        let has_screen = screen.is_some();
        // Start Server reaches only this computer's server. Another computer's stopped server
        // is started there, so its Pane explains that and offers nothing.
        let remote = self
            .shared
            .as_ref()
            .and_then(|shared| shared.sessions().remote_name());
        let notice = pane.notice().map(|(title, detail, intent)| {
            let detail = match &remote {
                Some(name) if pane.server_stopped() => gpui::SharedString::from(format!(
                    "The SpaceTerm server on \u{201C}{name}\u{201D} was stopped, so the programs \
                     in this Pane ended. It stays stopped until it\u{2019}s started on that \
                     computer."
                )),
                _ => detail,
            };
            let retry = if pane.server_stopped() {
                remote.is_none().then(|| {
                    server_pane::start_server_button(
                        selector("start-server"),
                        crate::ui::workspace_manager::start_local_server,
                    )
                })
            } else {
                pane.notice_offers_retry().then(|| {
                    let owner = view.clone();
                    server_pane::retry_button(selector("retry"), move |window, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            view.retry_shared_pane(pane_id, window, cx);
                        });
                    })
                })
            };
            // Over a screen, the notice follows the screen's content instead of floating over its
            // actions.
            server_pane::render_notice(
                selector("notice"),
                title,
                detail,
                intent,
                retry,
                !has_screen,
                cx,
            )
        });
        let (screen, notice) = match screen {
            Some(screen) => (Some(server_pane::with_notice(screen, notice, cx)), None),
            None => (None, notice),
        };
        // The screen's labeled Group rests on its own focus, so assistive technology announces
        // where the keyboard landed, and plain Tab the terminal never took reaches its action
        // while terminal input is paused.
        let screen = screen.map(|screen| match &screen_focus {
            Some(focus) => {
                server_pane::with_tab_traversal(screen.track_focus(focus)).into_any_element()
            }
            None => screen.into_any_element(),
        });
        SharedPaneSurface { screen, notice }
    }

    fn take_over_shared_pane(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sync) = self
            .shared
            .as_mut()
            .and_then(|shared| shared.view_mut(pane_id))
            .map(crate::ui::server_pane::ServerPaneView::take_over)
        else {
            return;
        };
        // Take Over waits disabled while the server answers, and a disabled control gives up
        // focus. The screen keeps it, so the keyboard stays where the action was.
        self.focus_shared_screen(pane_id, window, cx);
        self.apply_shared_sync(pane_id, sync, window, cx);
        cx.notify();
    }

    fn start_shared_shell(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sync) = self
            .shared
            .as_mut()
            .and_then(|shared| shared.view_mut(pane_id))
            .map(crate::ui::server_pane::ServerPaneView::start_shell)
        else {
            return;
        };
        self.focus_shared_screen(pane_id, window, cx);
        self.apply_shared_sync(pane_id, sync, window, cx);
        cx.notify();
    }

    fn choose_shared_directory(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if spaceterm_ui::window_modal_is_open(window, cx) {
            return;
        }
        if self.directory_recovery_is_open(cx) {
            return;
        }
        let Some(shared) = &self.shared else { return };
        if !shared.editability().available() {
            return;
        }
        let Some(view) = shared.view(pane_id).filter(|view| view.can_start_shell()) else {
            return;
        };
        let connection = view.connection();
        let (Some(server), Some(target), Some(_focus)) = (
            connection.directory_server(),
            connection.recovery_target(),
            shared.screen_focus(pane_id).cloned(),
        ) else {
            return;
        };
        self.focus_shared_screen(pane_id, window, cx);
        let current = std::sync::Arc::clone(&connection);
        let online = std::sync::Arc::clone(&connection);
        let expected = target.clone();
        let current_owner = cx.weak_entity();
        let restore_owner = cx.weak_entity();
        let expected_target = target.clone();
        let actions = super::server_pane::RecoveryActions {
            current: std::rc::Rc::new(move |cx| {
                current_owner.upgrade().is_some_and(|owner| {
                    let view = owner.read(cx);
                    view.active && view.tab.pane(pane_id).is_some()
                }) && current.recovery_target().as_ref() == Some(&expected)
                    && matches!(
                        current.status(),
                        crate::terminal::server_session::DevelopmentPaneStatus::Failed(
                            crate::local_server::ServerError::DirectoryUnavailable
                        ) | crate::terminal::server_session::DevelopmentPaneStatus::Reconnecting
                    )
            }),
            online: std::rc::Rc::new(move || {
                online.status()
                    == crate::terminal::server_session::DevelopmentPaneStatus::Failed(
                        crate::local_server::ServerError::DirectoryUnavailable,
                    )
            }),
            confirm: std::rc::Rc::new(move |replacement| {
                connection.repair_directory(target.clone(), replacement)
            }),
            restore_focus: std::rc::Rc::new(move |window, cx| {
                let _ =
                    restore_owner.update(cx, |view, cx| view.restore_directory_focus(window, cx));
            }),
        };
        let recovery = cx.new(|cx| {
            super::server_pane::DirectoryRecovery::new(server, expected_target, actions, window, cx)
        });
        cx.observe(&recovery, |view, _, cx| {
            cx.emit(TabViewEvent::PresentationChanged {
                tab_id: view.tab.id(),
            });
            cx.notify();
        })
        .detach();
        self.directory_recovery = Some(recovery);
        cx.emit(TabViewEvent::PresentationChanged {
            tab_id: self.tab.id(),
        });

        cx.notify();
    }
    /// The recovery picker holds the keyboard while it is open and restores focus when it closes.
    fn directory_recovery_is_open(&self, cx: &App) -> bool {
        self.directory_recovery
            .as_ref()
            .is_some_and(|picker| picker.read(cx).is_open())
    }

    pub(crate) fn directory_recovery(
        &self,
        cx: &App,
    ) -> Option<Entity<super::server_pane::DirectoryRecovery>> {
        self.directory_recovery
            .clone()
            .filter(|recovery| recovery.read(cx).is_open())
    }
    fn restore_directory_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        let focused = self.tab.focused_pane_id();
        if self
            .shared
            .as_ref()
            .and_then(|shared| shared.view(focused))
            .and_then(|view| view.screen())
            .is_some()
        {
            self.focus_shared_screen(focused, window, cx);
        } else {
            self.focus(window, cx);
        }
    }
    /// Tells each shared Pane that the person stopped its server, as the server's route reported.
    pub(crate) fn server_stopped(&mut self, cx: &mut Context<Self>) {
        let Some(shared) = self.shared.as_mut() else {
            return;
        };
        for pane_id in shared.pane_ids() {
            if let Some(view) = shared.view_mut(pane_id) {
                view.server_stopped_elsewhere();
            }
        }
        cx.notify();
    }
    /// Reconnects each shared Pane the person's stop interrupted, once they started the server.
    pub(crate) fn server_started(&mut self, cx: &mut Context<Self>) {
        let Some(shared) = self.shared.as_mut() else {
            return;
        };
        for pane_id in shared.pane_ids() {
            if let Some(view) = shared.view_mut(pane_id) {
                view.server_started();
            }
        }
        cx.notify();
    }

    fn retry_shared_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        // Retry is the person asking to reach the Pane's server. Another computer's route may
        // need them to sign in again, which only a requested connection may ask for, so Retry
        // asks for one now. An interruption alone never does.
        if let Some(destination) = self
            .shared
            .as_ref()
            .and_then(|shared| shared.sessions().remote_destination().cloned())
            && let Some(connections) = crate::ui::ssh_servers::installed(cx)
            && !matches!(
                connections.status(&destination),
                crate::ssh::server_connection::RemoteConnectionStatus::Connected { .. }
            )
        {
            crate::ui::ssh_servers::present_prompts_in(window, cx);
            connections.connect(destination);
        }
        // A direct network route asks the system for this device's key only now, because the
        // person asked. It never falls back to SSH for that computer.
        if let Some(route) = self
            .shared
            .as_ref()
            .and_then(|shared| shared.sessions().direct_route())
            .map(|direct| direct.route)
            && let Some(connections) = crate::ui::direct_servers::installed(cx)
            && connections.status(&route)
                != Some(crate::direct_connections::DirectRouteStatus::Connected)
        {
            connections.resume(&route);
        }
        if self
            .shared
            .as_mut()
            .and_then(|shared| shared.view_mut(pane_id))
            .map(crate::ui::server_pane::ServerPaneView::retry)
            .is_some()
        {
            // A check waits disabled while the server answers, and a disabled control gives up
            // focus. The screen keeps it, so the keyboard stays where the action was.
            if matches!(
                self.shared
                    .as_ref()
                    .and_then(|shared| shared.view(pane_id))
                    .and_then(crate::ui::server_pane::ServerPaneView::screen),
                Some(crate::ui::server_pane::ServerPaneScreen::StartUnknown { checking: true, .. })
            ) {
                self.focus_shared_screen(pane_id, window, cx);
            }
            cx.notify();
        }
    }

    /// Whether the keyboard rests in this shared Pane: in its terminal, or anywhere in the screen
    /// replacing it, including that screen's action.
    ///
    /// Terminal Find sits inside the Pane and owns the keyboard itself, so it does not count. Only
    /// what the Pane presents in place of a shell may hand focus on, and only to the Pane.
    fn shared_pane_holds_focus(
        &self,
        pane_id: PaneId,
        terminal: &Entity<TerminalPane>,
        window: &Window,
        cx: &App,
    ) -> bool {
        terminal.read(cx).is_focused(window)
            || self
                .shared
                .as_ref()
                .and_then(|shared| shared.screen_focus(pane_id))
                .is_some_and(|focus| focus.contains_focused(window, cx))
    }

    fn focus_shared_screen(&self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self
            .shared
            .as_ref()
            .and_then(|shared| shared.screen_focus(pane_id))
        {
            focus.focus(window, cx);
        }
    }
}

impl EventEmitter<TabViewEvent> for TabView {}

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
        let hovers = self.pane_hovers(appearance.active, window, cx);
        let places = self
            .pane_layout_size
            .map(|layout_size| {
                let bounds = Bounds::new(
                    Point::default(),
                    gpui::size(px(layout_size.width()), px(layout_size.height())),
                );
                // The other Panes keep their places while a Pane is zoomed, so they can keep moving
                // beneath it as it zooms.
                let mut places = BTreeMap::new();
                lay_out_panes(self.tab.root(), bounds, pane_gap(cx), &mut places);
                if let ZoomState::Zoomed(pane_id) = zoom_state {
                    places.insert(pane_id, bounds);
                }
                places
            })
            .unwrap_or_default();
        self.pane_frames = self.pane_motion.present(&places, window.scale_factor(), cx);
        if self.pane_frames.moving {
            window.request_animation_frame();
        }
        if let Some(layout_size) = self.pane_layout_size {
            self.pane_frames
                .stretch(px(layout_size.width()), self.stage_stretch);
        }
        // Removed Panes take the material of the Pane that keeps focus.
        let terminal_background = self
            .tab
            .pane(self.tab.focused_pane_id())
            .map_or(appearance.colors.background, |terminal| {
                terminal.read(cx).surface_background()
            });
        let mut closing = self
            .pane_frames
            .closing
            .iter()
            .map(|place| render_closing_pane(*place, terminal_background, &appearance, cx))
            .collect::<Vec<_>>();
        let content = match (zoom_state, self.pane_frames.raised) {
            // A zooming Pane moves above the restored layout, which stays where it is beneath it.
            (_, Some((pane_id, _))) => {
                let place = places.get(&pane_id).copied().unwrap_or_default();
                div()
                    .relative()
                    .size_full()
                    .child(
                        div()
                            .size_full()
                            .opacity(self.pane_frames.beneath_opacity)
                            .child(self.render_tree(
                                self.tab.root(),
                                &hovers,
                                view.clone(),
                                &appearance,
                                cx,
                            ))
                            .children(std::mem::take(&mut closing)),
                    )
                    // The Panes a zoom fades out take no pointer input.
                    .when(matches!(zoom_state, ZoomState::Zoomed(_)), |content| {
                        content.child(div().absolute().top_0().left_0().size_full().occlude())
                    })
                    .child(
                        div()
                            .absolute()
                            .left(place.left())
                            .top(place.top())
                            .w(place.size.width)
                            .h(place.size.height)
                            .child(self.render_leaf(pane_id, &hovers, view, &appearance, cx)),
                    )
                    .into_any_element()
            }
            (ZoomState::Restored, None) => {
                self.render_tree(self.tab.root(), &hovers, view.clone(), &appearance, cx)
            }
            (ZoomState::Zoomed(pane_id), None) => {
                self.render_leaf(pane_id, &hovers, view, &appearance, cx)
            }
        };

        div()
            .on_children_prepainted({
                let view = cx.entity().downgrade();
                move |children, window, cx| {
                    let Some(bounds) = children.first() else {
                        return;
                    };
                    let (resized, pending_split) = view
                        .update(cx, |view, _| {
                            let measured = pane_size(*bounds).ok();
                            let resized = view.pane_layout_size != measured && measured.is_some();
                            view.pane_layout_size = measured;
                            if resized {
                                view.pane_motion.restart_change();
                            }
                            (resized, view.pending_split.take())
                        })
                        .unwrap_or_default();
                    // This frame laid out its Panes for the previous size. Pane motion starts from
                    // where the next frame draws them at the measured size.
                    if resized {
                        let view = view.clone();
                        window.defer(cx, move |_, cx| {
                            let _ = view.update(cx, |_, cx| cx.notify());
                        });
                    }
                    if let Some((pane_id, axis)) = pending_split {
                        let view = view.clone();
                        window.defer(cx, move |window, cx| {
                            let _ = view.update(cx, |view, cx| {
                                if view.tab.pane(pane_id).is_some() {
                                    view.split_pane(pane_id, axis, window, cx);
                                }
                            });
                        });
                    }
                }
            })
            .id(("tab-view", self.tab.id().get()))
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w(px(minimum_size.width()))
            .min_h(px(minimum_size.height()))
            // A stretched stage draws the Panes past the Pane Layout, and clips them itself.
            .when(self.stage_stretch == px(0.0), |view| view.overflow_hidden())
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
            .children(closing)
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
    repository: Option<RepositoryCaption>,
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
                repository: facts.repository,
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
            repository: facts.repository,
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
    /// Present when a SpaceTerm Server owns the Pane, which changes what its controls offer.
    shared: Option<SharedCaptionControls>,
}

fn render_pane_caption(
    caption: PaneCaption,
    controls_reveal: f32,
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
                if let Some(facts) = super::developer_workbench::caption_fixture(cx) {
                    let repository = caption.text.repository.take();
                    caption.text = PaneCaptionText::from_facts(facts);
                    caption.text.repository = repository;
                }
                if let Some(view) = super::developer_workbench::repository_view_fixture(cx) {
                    caption.text.repository =
                        RepositoryCaption::from_view(&view, std::time::Instant::now());
                }
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
            // repeating the Pane background here would leave opaque squares on the glass.
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
            let layout = CaptionLayout::resolve(
                &caption,
                bounds.size.width,
                controls_reveal,
                window,
                &appearance,
            );
            let content = render_pane_caption_content(
                caption,
                controls_reveal,
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
    controls_reveal: f32,
    view: gpui::WeakEntity<TabView>,
    presentation: &crate::desktop_profile::DesktopPresentation,
    layout: CaptionLayout,
    appearance: &super::appearance::ChromeAppearance,
    paint: crate::appearance::CaptionPaint,
) -> AnyElement {
    let PaneCaption {
        pane_id,
        terminal,
        text,
        focused,
        zoomed,
        attention,
        has_multiple_panes,
        shared,
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
    // a hidden unfocused Pane control is disabled before pointer-down to avoid its stale hitbox.
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
        .flex_shrink_0()
        // An unfocused Pane shows its controls only under the pointer; they slide in with the
        // clip that reveals them.
        // Until fully revealed, the controls overhang the clip that slides them in, so assistive
        // technology must not find their frames there. Only a node with a role publishes that it
        // is hidden.
        .when(controls_reveal < 1.0, |controls| {
            controls
                .role(gpui::accesskit::Role::Group)
                .aria_hidden(true)
        });
    let control_count = usize::from(layout.show_splits) * 2 + usize::from(has_multiple_panes) * 2;
    // Hidden controls give up their width, so the segments before them slide to the trailing edge
    // and back as the controls slide out and in.
    let controls_extent = if control_count == 0 {
        0.0
    } else {
        let control_size = f32::from(
            appearance
                .icons
                .interactive_target_size(InteractiveIconRole::Control),
        );
        appearance.spacing_scale * PANE_CONTROL_LEADING_GAP
            + controls_width(
                control_count,
                has_multiple_panes,
                control_size,
                appearance.spacing_scale,
            )
    };
    // The leading gap opens first and stays outside the clip, so a partly revealed control never
    // touches the segment before it.
    let slot_width = controls_reveal * controls_extent;
    let leading_gap = slot_width.min(appearance.spacing_scale * PANE_CONTROL_LEADING_GAP);
    let controls_clip = div()
        .id(("pane-controls-clip", pane_id.get()))
        .debug_selector(move || format!("pane-controls-clip-{}", pane_id.get()))
        .flex_shrink_0()
        .w(px(slot_width - leading_gap))
        .h_full()
        .flex()
        .items_center()
        // The controls slide in from the trailing edge, and a partly hidden control takes no
        // clicks.
        .justify_start()
        .overflow_hidden();
    let controls_slot = div()
        .flex_shrink_0()
        .w(px(slot_width))
        .h_full()
        .flex()
        .items_center()
        .justify_end();
    // A shared Pane's split and close ask the server, so they wait while this client is offline
    // or an earlier edit is unanswered.
    let split_unavailable =
        shared.and_then(|shared| (!shared.edits).then_some(shared.edits_reason));
    let close_unavailable = split_unavailable;
    let actions = [
        (
            PaneCaptionAction::SplitRight,
            "split-right",
            "Split Right",
            IconName::Columns2,
            presentation.shortcut(&SplitRight),
            layout.show_splits,
            split_unavailable,
        ),
        (
            PaneCaptionAction::SplitDown,
            "split-down",
            "Split Down",
            IconName::Rows2,
            presentation.shortcut(&SplitDown),
            layout.show_splits,
            split_unavailable,
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
            None,
        ),
        (
            PaneCaptionAction::Close,
            "close",
            "Close Pane",
            IconName::X,
            presentation.shortcut(&ClosePane),
            has_multiple_panes,
            close_unavailable,
        ),
    ];
    for (action, selector, name, icon, shortcut, visible, unavailable) in actions {
        if !visible {
            continue;
        }
        let caption_action_available = caption_action_available && unavailable.is_none();
        let control_view = view.clone();
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
        .when_some(unavailable.flatten(), |button, reason| {
            button.accessibility_description(reason)
        })
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
        controls = if matches!(action, PaneCaptionAction::ToggleZoom) {
            controls.child(button)
        } else {
            // The Pane's surface reads where Split and Close take presses, so a press there
            // selects nothing.
            let unselecting_control = gpui::canvas(
                move |bounds, window, cx| {
                    let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
                    let _ = control_view.update(cx, |view, _| {
                        view.pane_unselecting_controls
                            .insert((pane_id, action), hitbox);
                    });
                },
                |_, _, _, _| (),
            )
            .absolute()
            .inset_0();
            controls.child(
                div()
                    .relative()
                    .when(matches!(action, PaneCaptionAction::Close), |control| {
                        control.ml(-appearance.spacing(PANE_CLOSE_OPTICAL_TRIM))
                    })
                    .child(unselecting_control)
                    .child(button),
            )
        };
    }
    let caption_name = format!("Pane Caption, {}", text.name);
    let repository = text.repository.clone();
    let caption_content =
        render_caption_identity(pane_id, text, attention, layout, appearance, &paint);
    let focus_pane = move |window: &mut Window, cx: &mut App| {
        let _ = focus_view.update(cx, |view, cx| {
            view.focus_pane(pane_id, cx);
            view.focus(window, cx);
        });
    };
    let accessibility_focus = focus_pane.clone();
    caption_row(appearance, color)
        .id(("pane-caption", pane_id.get()))
        .role(gpui::accesskit::Role::Group)
        .aria_label(caption_name)
        .when(zoomed, |row| row.aria_description("Zoomed Pane"))
        .on_a11y_action(gpui::accesskit::Action::Click, move |_, window, cx| {
            accessibility_focus(window, cx);
        })
        .debug_selector(move || {
            format!(
                "pane-caption-{}-{}",
                pane_id.get(),
                if focused { "focused" } else { "unfocused" }
            )
        })
        // The press focuses the Pane at once, since a press that starts a drag never clicks.
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
            focus_pane(window, cx);
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
        .when_some(
            repository.filter(|_| layout.repository.show),
            |row, repository| {
                row.child(render_repository_segment(
                    pane_id,
                    &repository,
                    (terminal.downgrade(), view.clone()),
                    layout.repository,
                    appearance,
                    &paint,
                ))
            },
        )
        .child(controls_slot.child(controls_clip.child(controls)))
        .into_any_element()
}

/// The mark before a branch name or detached commit id, in the inherited text color.
pub(super) fn render_head_glyph(
    glyph: HeadGlyph,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    let mark = div().flex_shrink_0().flex().items_center();
    match glyph.text() {
        Some(text) => mark.child(text),
        None => mark.child(Icon::inherited(
            IconName::GitBranch,
            appearance.icons.metrics(IconRole::Caption).glyph_size,
        )),
    }
    .into_any_element()
}

/// The width [`render_head_glyph`] takes, measuring a text mark with `measure`.
pub(super) fn head_glyph_width(
    glyph: HeadGlyph,
    appearance: &super::appearance::ChromeAppearance,
    measure: impl FnOnce(&str) -> Pixels,
) -> Pixels {
    glyph.text().map_or_else(
        || appearance.icons.metrics(IconRole::Caption).glyph_size,
        measure,
    )
}

/// The Pane's Repository Status: glyph, branch, one mark, the detail, and the divergence.
/// Color is never the only signal; the glyph, words, and the spoken label carry every state.
fn render_repository_segment(
    pane_id: PaneId,
    repository: &RepositoryCaption,
    (terminal, view): (gpui::WeakEntity<TerminalPane>, gpui::WeakEntity<TabView>),
    layout: RepositoryCaptionLayout,
    appearance: &super::appearance::ChromeAppearance,
    paint: &crate::appearance::CaptionPaint,
) -> AnyElement {
    let text_color = if repository.dimmed {
        paint.secondary
    } else {
        paint.foreground
    };
    let part = || {
        div()
            .flex_shrink_0()
            .ml(appearance.spacing(PANE_REPOSITORY_PART_GAP))
    };
    // The segment opens the popover on click, so a press neither focuses the Pane nor starts a
    // Pane drag; the click focuses the Pane first.
    let show = move |window: &mut Window, cx: &mut App| {
        let _ = view.update(cx, |view, cx| view.focus_pane(pane_id, cx));
        let _ = terminal.update(cx, |terminal, cx| {
            terminal.show_repository_status(&ShowRepositoryStatus, window, cx);
        });
    };
    let accessible_show = show.clone();
    let mut segment = div()
        .id(("pane-repository", pane_id.get()))
        .debug_selector(move || format!("pane-repository-{}", pane_id.get()))
        .role(gpui::accesskit::Role::Button)
        .aria_label(gpui::SharedString::from(
            repository.accessible_label.clone(),
        ))
        .cursor_pointer()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| show(window, cx))
        .on_a11y_action(gpui::accesskit::Action::Click, move |_, window, cx| {
            accessible_show(window, cx);
        })
        .flex()
        .items_center()
        .min_w_0()
        .flex_shrink_0()
        .ml(appearance.spacing(PANE_REPOSITORY_LEADING_GAP))
        .text_color(gpui_color(text_color))
        .child(render_head_glyph(repository.glyph, appearance));
    if layout.show_branch {
        segment = segment.child(
            part()
                .debug_selector(move || format!("pane-repository-branch-{}", pane_id.get()))
                .min_w_0()
                .truncate()
                .when_some(layout.branch_limit, |branch, limit| {
                    branch.max_w(px(limit as f32))
                })
                .child(repository.branch.clone()),
        );
    }
    if let Some(mark) = repository.mark {
        let color = match mark {
            RepositoryMark::Operation => paint.repository_operation,
            RepositoryMark::Changes => paint.repository_changes,
        };
        segment = segment.child(
            part()
                .debug_selector(move || format!("pane-repository-mark-{}", pane_id.get()))
                .text_color(gpui_color(if repository.dimmed {
                    text_color
                } else {
                    color
                }))
                .child(mark.glyph()),
        );
    }
    if layout.show_detail
        && let Some(detail) = &repository.detail
    {
        segment = segment.child(
            part()
                .debug_selector(move || format!("pane-repository-detail-{}", pane_id.get()))
                .when(detail.dimmed, |detail| {
                    detail.text_color(gpui_color(paint.secondary))
                })
                .child(detail.text.clone()),
        );
    }
    if layout.show_divergence
        && let Some(divergence) = &repository.divergence
    {
        segment = segment.child(
            part()
                .debug_selector(move || format!("pane-repository-divergence-{}", pane_id.get()))
                .child(divergence.clone()),
        );
    }
    segment.into_any_element()
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
                        .ml(appearance.spacing(5.0))
                        .text_color(gpui_color(color))
                        .child("·"),
                )
            })
            .child(
                div()
                    .debug_selector(move || format!("pane-caption-status-{}", pane_id.get()))
                    .flex_shrink_0()
                    .ml(appearance.spacing(PANE_STATUS_LEADING_GAP))
                    .child(render_pane_status(
                        pane_id,
                        (text.progress, text.glyph, attention),
                        paint,
                        appearance,
                    )),
            )
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

/// One side of a Split, which clips its Panes to their places when `clip` is set.
fn split_child(
    id: &'static str,
    child: AnyElement,
    axis: SplitAxis,
    ratio: f32,
    clip: bool,
) -> impl IntoElement {
    let child = div()
        .id(id)
        .role(gpui::accesskit::Role::Group)
        .a11y_synthetic_children(|builder| {
            // Preserve one child link per side for reordering, then let the platform present
            // that side's Pane Captions and terminals in place.
            builder
                .parent_node()
                .set_role(gpui::accesskit::Role::GenericContainer);
        })
        .flex_basis(DefiniteLength::Fraction(ratio))
        .min_w_0()
        .min_h_0()
        .when(clip, |child| child.overflow_hidden())
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
    offset_range: Option<RangeInclusive<f32>>,
    pointer_input: bool,
    view: gpui::WeakEntity<TabView>,
) -> AnyElement {
    let handle = ResizeHandle::new(
        ("split-resize", split_id.get()),
        "Resize Pane split",
        match axis {
            SplitAxis::Horizontal => ResizeAxis::Horizontal,
            SplitAxis::Vertical => ResizeAxis::Vertical,
        },
        current_offset,
    );
    match offset_range {
        Some(range) => handle.range(range),
        None => handle,
    }
    .tab_stop(true)
    .reset_on_double_click(true)
    // Moving Panes cover the gap, so the pointer reaches them instead.
    .pointer_input(pointer_input)
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

/// The place a removed Pane leaves while it closes: a Pane's material over `terminal_background`,
/// without contents.
fn render_closing_pane(
    place: Bounds<Pixels>,
    terminal_background: crate::appearance::Color,
    appearance: &super::appearance::ChromeAppearance,
    cx: &App,
) -> AnyElement {
    let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
    div()
        .debug_selector(|| "closing-pane".to_owned())
        .absolute()
        .left(place.left())
        .top(place.top())
        .w(place.size.width)
        .h(place.size.height)
        .rounded(frame.pane_radius())
        .bg(gpui_color(appearance.pane_surface(terminal_background)))
        .border(frame.pane_rim_width())
        .border_color(gpui_color(appearance.pane_rim_on(terminal_background)))
        .into_any_element()
}

/// Inserts each Pane's place in `bounds` into `places`, as the Splits of `tree` divide it.
fn lay_out_panes(
    tree: PaneTreeRef<'_>,
    bounds: Bounds<Pixels>,
    gap: f32,
    places: &mut BTreeMap<PaneId, Bounds<Pixels>>,
) {
    match tree.node() {
        PaneNodeRef::Leaf { pane_id } => {
            places.insert(pane_id, bounds);
        }
        PaneNodeRef::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let (first_bounds, second_bounds) = match axis {
                SplitAxis::Horizontal => {
                    let extent = px((f32::from(bounds.size.width) - gap).max(0.0) * ratio);
                    (
                        Bounds::from_corners(
                            bounds.origin,
                            point(bounds.left() + extent, bounds.bottom()),
                        ),
                        Bounds::from_corners(
                            point(bounds.left() + extent + px(gap), bounds.top()),
                            bounds.bottom_right(),
                        ),
                    )
                }
                SplitAxis::Vertical => {
                    let extent = px((f32::from(bounds.size.height) - gap).max(0.0) * ratio);
                    (
                        Bounds::from_corners(
                            bounds.origin,
                            point(bounds.right(), bounds.top() + extent),
                        ),
                        Bounds::from_corners(
                            point(bounds.left(), bounds.top() + extent + px(gap)),
                            bounds.bottom_right(),
                        ),
                    )
                }
            };
            lay_out_panes(first, first_bounds, gap, places);
            lay_out_panes(second, second_bounds, gap, places);
        }
    }
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

    use gpui::{
        Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
        VisualTestContext, bounds, point, px, size,
    };

    use super::*;
    use crate::appearance::Color;
    use crate::terminal::testing::{
        RecordedCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{
        ScreenSnapshot, ScrollbarSnapshot, TerminalSessionEvent, TerminalSessionExit,
        TerminalSessionFactory,
    };

    struct CaptionSegmentFixture {
        text: &'static str,
    }

    impl Render for CaptionSegmentFixture {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let appearance = super::super::appearance::ChromeAppearance::default();
            div()
                .debug_selector(|| "fractional-caption-segment".to_owned())
                .w(measure_caption_segment(
                    &self.text.into(),
                    window,
                    &appearance,
                ))
                .chrome_text(appearance.typography.style(TextRole::Body))
                .truncate()
                .child(self.text)
        }
    }

    #[gpui::test]
    fn caption_segment_keeps_its_full_width_at_fractional_scales(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| CaptionSegmentFixture { text: "A aa" });
        let appearance = super::super::appearance::ChromeAppearance::default();
        let mut failures = Vec::new();
        for (scale, text) in [(1.25, "A aa"), (1.5, "A aa"), (1.75, "A aaaaa")] {
            cx.simulate_scale_factor_change(scale);
            view.update(cx, |view, cx| {
                view.text = text;
                cx.notify();
            });
            cx.run_until_parked();
            let intrinsic = cx.update(|window, _| {
                appearance
                    .typography
                    .measure(TextRole::Body, text, window)
                    .ceil()
            });
            // The caption ladder must reserve all 29 or 51 intrinsic logical pixels after snapping.
            let intrinsic_device = f32::from(intrinsic) * scale;
            assert_eq!(f32::from(intrinsic) as u32 % 2, 1);
            assert!((intrinsic_device - 0.5).ceil() < intrinsic_device);
            let segment = cx
                .debug_bounds("fractional-caption-segment")
                .expect("caption segment");
            if segment.size.width < intrinsic {
                failures.push((scale, text, segment, intrinsic));
            }
        }
        assert!(
            failures.is_empty(),
            "caption ladder must reserve the full segment: {failures:?}"
        );
    }

    fn prepared_appearance(
        appearance: crate::appearance::Appearance,
        opacity: f32,
    ) -> super::super::appearance::ChromeAppearance {
        let mut preferences = crate::appearance::AppearancePreferences {
            mode: match appearance {
                crate::appearance::Appearance::Light => crate::appearance::AppearanceMode::Light,
                crate::appearance::Appearance::Dark => crate::appearance::AppearanceMode::Dark,
            },
            ..crate::appearance::AppearancePreferences::default()
        };
        preferences.window.opacity = opacity;
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
        for opacity in [1.0, 0.65, 0.0] {
            let appearance = prepared_appearance(crate::appearance::Appearance::Light, opacity);
            let window = appearance.control_host_background(spaceterm_ui::ControlHost::Window);
            for terminal in [Color::BLACK, Color::rgb(0xfafafa), Color::rgb(0x38658a)] {
                let host = appearance.pane_surface(terminal).source_over(window);
                let edge = appearance.pane_rim_on(terminal).source_over(host);
                let contrast = edge.contrast_ratio(host);
                assert!(
                    (1.20..=1.30).contains(&contrast),
                    "Light Pane edge {edge:?} must remain in its surface band on {host:?} at opacity {opacity}"
                );
            }
        }
    }

    #[test]
    fn dark_pane_rim_uses_the_accepted_terminal_surface_as_its_host() {
        for opacity in [1.0, 0.65, 0.0] {
            let appearance = prepared_appearance(crate::appearance::Appearance::Dark, opacity);
            for terminal in [Color::BLACK, Color::rgb(0xfafafa), Color::rgb(0x38658a)] {
                let window = appearance.control_host_background(spaceterm_ui::ControlHost::Window);
                let host = appearance.pane_surface(terminal).source_over(window);
                let edge = appearance.pane_rim_on(terminal).source_over(host);
                assert!((1.25..=1.50).contains(&edge.contrast_ratio(host)));
            }
        }
    }

    /// A desktop-owned Tab's Panes over a scripted terminal.
    fn test_session_factory() -> WorkspaceTerminalSessionFactory {
        desktop_session_factory(Rc::new(
            TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                .with_selection_copy_response(Ok(None)),
        ))
    }

    /// Desktop-owned Panes over `terminal`.
    fn desktop_session_factory(
        terminal: Rc<dyn TerminalSessionFactory>,
    ) -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::new_local(
            terminal,
            crate::domain::ValidatedLocalDirectory::new(
                PathBuf::from("/missing/local/home-is-not-a-workspace"),
                crate::domain::LocalDirectoryIdentity::for_test(71074),
            ),
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
    fn a_split_should_open_the_new_pane_while_each_terminal_resizes_only_once(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::{AnimatePresence, finish_slides};

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::new(
                TestTerminalSessionFactory::new(records.clone())
                    .with_selection_copy_response(Ok(None)),
            ),
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        let whole = cx.debug_bounds("pane-surface-1").unwrap();
        let before = records.commands().len();
        // How many times each Terminal Session has resized since the split.
        let resizes = || {
            let mut resizes = BTreeMap::<usize, usize>::new();
            for call in &records.commands()[before..] {
                if matches!(call.command, RecordedCommand::Resize(_)) {
                    *resizes.entry(call.session_id).or_default() += 1;
                }
            }
            resizes
        };

        cx.update(|_, cx| cx.set_global(AnimatePresence));
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let moving = (
            cx.debug_bounds("pane-surface-1").unwrap(),
            cx.debug_bounds("pane-surface-2").unwrap(),
        );
        let resized_while_moving = resizes();
        finish_slides(cx);
        let rested = (
            cx.debug_bounds("pane-surface-1").unwrap(),
            cx.debug_bounds("pane-surface-2").unwrap(),
        );

        assert!(
            moving.0.right() > rested.0.right() && moving.0.right() < whole.right(),
            "the split Pane must still be making room: {moving:?} toward {rested:?}"
        );
        assert!(
            moving.1.left() > rested.1.left() && moving.1.right() == whole.right(),
            "the new Pane must open from its far edge: {moving:?} toward {rested:?}"
        );
        assert_eq!(
            (rested.0.right() < rested.1.left(), &resized_while_moving),
            (true, &resizes()),
            "each Terminal must take its final size when the motion starts"
        );
        assert!(
            resized_while_moving.get(&1) == Some(&1)
                && resized_while_moving.values().all(|count| *count == 1),
            "each Terminal must resize once: {resized_while_moving:?}"
        );
    }

    #[gpui::test]
    fn a_closed_pane_should_close_its_place_as_its_neighbor_takes_it(cx: &mut TestAppContext) {
        use crate::ui::drag_and_drop::{AnimatePresence, finish_slides};

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        let whole = cx.debug_bounds("pane-surface-1").unwrap();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        let split = cx.debug_bounds("pane-surface-1").unwrap();

        cx.update(|_, cx| cx.set_global(AnimatePresence));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_pane(PaneId::new(2), window, cx));
        });
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let moving = (
            cx.debug_bounds("pane-surface-1").unwrap(),
            cx.debug_bounds("closing-pane").unwrap(),
        );
        finish_slides(cx);

        assert!(
            moving.0.right() > split.right() && moving.0.right() < whole.right(),
            "the remaining Pane must be taking the closed Pane's place: {moving:?}"
        );
        assert!(
            moving.1.left() > moving.0.right() && moving.1.right() == whole.right(),
            "the closed Pane's place must close toward its far edge: {moving:?}"
        );
        assert_eq!(
            (
                cx.debug_bounds("pane-surface-1"),
                cx.debug_bounds("closing-pane")
            ),
            (Some(whole), None)
        );
    }

    #[gpui::test]
    fn a_zoom_at_a_fractional_size_should_resize_the_terminal_once_as_its_pane_grows(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::AnimatePresence;

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::new(
                TestTerminalSessionFactory::new(records.clone())
                    .with_selection_copy_response(Ok(None)),
            ),
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.simulate_resize(gpui::size(px(1001.0), px(701.0)));
        cx.run_until_parked();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        let resizes = || {
            records
                .commands()
                .iter()
                .filter(|call| matches!(call.command, RecordedCommand::Resize(_)))
                .count()
        };
        let before = resizes();

        cx.update(|_, cx| cx.set_global(AnimatePresence));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.focus_pane(PaneId::new(3), cx);
                view.toggle_zoom(window, cx);
            });
        });
        cx.run_until_parked();
        let mut counts = vec![resizes() - before];
        for _ in 0..30 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(16));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            counts.push(resizes() - before);
        }

        assert_eq!(
            counts,
            vec![1; 31],
            "the zoomed Terminal must resize once, when the zoom starts"
        );
    }

    #[gpui::test]
    fn a_split_after_a_window_resize_should_start_from_where_the_pane_is_drawn(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::AnimatePresence;

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        cx.simulate_resize(gpui::size(px(1001.0), px(701.0)));
        cx.run_until_parked();
        let whole = cx.debug_bounds("pane-surface-1").unwrap();

        cx.update(|_, cx| cx.set_global(AnimatePresence));
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let moving = cx.debug_bounds("pane-surface-1").unwrap();

        assert!(
            moving.right() < whole.right() && moving.size.height == whole.size.height,
            "the split Pane must make room from its resized place: {moving:?} in {whole:?}"
        );
    }

    #[gpui::test]
    fn a_pane_dropped_while_panes_move_should_land_on_the_pane_drawn_under_the_pointer(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::AnimatePresence;

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();

        cx.update(|_, cx| cx.set_global(AnimatePresence));
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let drawn = cx.debug_bounds("pane-surface-1").unwrap();
        let place = view.read_with(cx, |view, _| view.pane_bounds[&PaneId::new(2)]);
        let pointer = point((place.left() + drawn.right()) / 2.0, drawn.center().y);
        assert!(drawn.contains(&pointer) && place.contains(&pointer));

        let target = view.read_with(cx, |view, cx| {
            view.drop_target(PaneId::new(2), pointer, pane_gap(cx))
        });

        assert_eq!(
            target.map(|(pane_id, _)| pane_id),
            Some(PaneId::new(1)),
            "the pointer is over the drawn Pane 1, inside the place Pane 2 opens toward"
        );
    }

    /// A Tab with Pane 1 at the top left, Pane 3 below it, and Pane 2 on the right, each surface at
    /// rest, and the Tab's whole Pane Layout.
    fn three_pane_zoom_view(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TabView>,
        [Bounds<Pixels>; 3],
        Bounds<Pixels>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        let whole = cx.debug_bounds("pane-surface-1").unwrap();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        split_test_pane(&view, PaneId::new(1), SplitAxis::Vertical, cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.focus_pane(PaneId::new(1), cx);
                view.focus(window, cx);
            });
        });
        cx.run_until_parked();
        let rest = ["pane-surface-1", "pane-surface-2", "pane-surface-3"]
            .map(|surface| cx.debug_bounds(surface).unwrap());
        (view, rest, whole, cx)
    }

    /// Presses at `position` and reports the Focused Pane and how many times the press changed it.
    fn press_counting_focus_changes(
        view: &Entity<TabView>,
        position: Point<Pixels>,
        cx: &mut VisualTestContext,
    ) -> (PaneId, usize) {
        let changes = Rc::new(RefCell::new(0));
        let counted = changes.clone();
        view.update(cx, |_, cx| {
            cx.subscribe(view, move |_, _, event: &TabViewEvent, _| {
                if matches!(event, TabViewEvent::PresentationChanged { .. }) {
                    *counted.borrow_mut() += 1;
                }
            })
            .detach();
        });
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
        let changes = *changes.borrow();
        (
            view.read_with(cx, |view, _| view.focused_pane_id()),
            changes,
        )
    }

    /// Draws the frame 60ms into a Pane motion.
    fn draw_motion_frame(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_zoom_should_grow_the_pane_over_the_others_as_they_stay_in_place(cx: &mut TestAppContext) {
        use crate::ui::drag_and_drop::{AnimatePresence, finish_slides};

        let (view, [first, second, third], whole, cx) = three_pane_zoom_view(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        draw_motion_frame(cx);
        let growing = cx.debug_bounds("pane-surface-1").unwrap();
        let others = (
            cx.debug_bounds("pane-surface-2"),
            cx.debug_bounds("pane-surface-3"),
            cx.debug_bounds("closing-pane"),
        );
        let over_second = point(growing.right() - px(4.0), second.center().y);
        let focused = press_counting_focus_changes(&view, over_second, cx);
        finish_slides(cx);

        assert!(
            growing.right() > first.right()
                && growing.right() < whole.right()
                && growing.bottom() > first.bottom(),
            "the zoomed Pane must be growing from its place: {growing:?}"
        );
        assert!(second.contains(&over_second));
        assert_eq!(
            (others, focused),
            ((Some(second), Some(third), None), (PaneId::new(1), 0)),
            "the other Panes must stay in place beneath the zoomed Pane, which takes the pointer"
        );
        assert_eq!(
            (
                cx.debug_bounds("pane-surface-1"),
                cx.debug_bounds("pane-surface-2")
            ),
            (Some(whole), None)
        );
    }

    #[gpui::test]
    fn an_unzoom_should_return_the_pane_over_the_others_in_their_places(cx: &mut TestAppContext) {
        use crate::ui::drag_and_drop::{AnimatePresence, finish_slides};

        let (view, [first, second, third], whole, cx) = three_pane_zoom_view(cx);
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        cx.run_until_parked();
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        draw_motion_frame(cx);
        let shrinking = cx.debug_bounds("pane-surface-1").unwrap();
        let others = (
            cx.debug_bounds("pane-surface-2"),
            cx.debug_bounds("pane-surface-3"),
        );
        let over_third = point(third.center().x, shrinking.bottom() - px(4.0));
        let focused = press_counting_focus_changes(&view, over_third, cx);
        finish_slides(cx);

        assert!(
            shrinking.right() > first.right() && shrinking.right() < whole.right(),
            "the zoomed Pane must be returning to its place: {shrinking:?}"
        );
        assert!(third.contains(&over_third));
        assert_eq!(
            (others, focused),
            ((Some(second), Some(third)), (PaneId::new(1), 0)),
            "the other Panes must wait in place beneath the returning Pane, which takes the pointer"
        );
        assert_eq!(cx.debug_bounds("pane-surface-1"), Some(first));
    }

    #[gpui::test]
    fn an_unzoom_that_widens_the_pane_layout_to_its_minimum_should_still_move_the_panes(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::AnimatePresence;

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        for _ in 0..4 {
            split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        }
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.focus_pane(PaneId::new(1), cx);
                view.toggle_zoom(window, cx);
            });
        });
        cx.run_until_parked();
        let minimum = cx.update(|_, cx| {
            let gap = pane_gap(cx);
            view.read(cx).tab.minimum_size(gap).unwrap()
        });
        cx.simulate_resize(gpui::size(
            px(minimum.width() - 30.0),
            px(minimum.height() + 200.0),
        ));
        cx.run_until_parked();

        cx.update(|_, cx| cx.set_global(AnimatePresence));
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |view, _| (
                view.tab.pane_count(),
                view.pane_frames.moving
            )),
            (5, true),
            "the Panes must keep moving once the layout widens to its minimum"
        );
    }

    #[gpui::test]
    fn a_pane_dropped_where_moving_panes_overlap_should_land_on_the_one_drawn_on_top(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        split_test_pane(&view, PaneId::new(2), SplitAxis::Horizontal, cx);
        let first = cx.debug_bounds("pane-surface-1").unwrap();

        // Pane 3 paints after Pane 2, so where their surfaces overlap mid-motion it is on top.
        let target = cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.pane_surfaces.insert(PaneId::new(2), first);
                view.pane_surfaces.insert(PaneId::new(3), first);
                view.drop_target(PaneId::new(1), first.center(), pane_gap(cx))
            })
        });

        assert_eq!(target.map(|(pane_id, _)| pane_id), Some(PaneId::new(3)));
    }

    #[gpui::test]
    fn a_press_on_a_split_gap_that_moving_panes_cover_should_not_resize_the_split(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::{AnimatePresence, finish_slides};

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();

        cx.update(|_, cx| cx.set_global(AnimatePresence));
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| view.focus(window, cx));
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let gap = cx.debug_bounds("split-gap-1").unwrap().center();
        assert!(
            cx.debug_bounds("pane-surface-1").unwrap().contains(&gap),
            "Pane 1 must still cover the gap it moves toward"
        );
        cx.simulate_mouse_down(gap, MouseButton::Left, Modifiers::none());
        let while_moving = cx.update(|window, cx| {
            let view = view.read(cx);
            (
                view.resizing_split_id,
                view.focused_terminal_has_input_focus(window, cx),
            )
        });
        cx.simulate_mouse_up(gap, MouseButton::Left, Modifiers::none());
        finish_slides(cx);
        cx.simulate_mouse_down(gap, MouseButton::Left, Modifiers::none());
        let at_rest = view.read_with(cx, |view, _| view.resizing_split_id);
        cx.simulate_mouse_up(gap, MouseButton::Left, Modifiers::none());

        assert_eq!(
            (while_moving, at_rest),
            ((None, true), Some(SplitId::new(1))),
            "the split must take the press only once its gap shows"
        );
    }

    #[gpui::test]
    fn a_scroll_where_moving_panes_overlap_should_not_reach_the_one_beneath(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::AnimatePresence;

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::new(
                TestTerminalSessionFactory::new(records.clone())
                    .with_selection_copy_response(Ok(None)),
            ),
            crate::terminal::testing::test_local_directory(test_home_directory()),
        );
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let size = view.pane_layout_size.unwrap();
                view.tab
                    .resize_split(SplitId::new(1), size, pane_gap(cx), 0.7)
                    .unwrap();
                cx.notify();
            })
        });
        cx.run_until_parked();

        // Pane 1 moves to the right of Pane 2 and paints after it.
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let second = cx.debug_bounds("pane-surface-2").unwrap();
        let caption = cx
            .debug_bounds("pane-caption-1-unfocused")
            .or_else(|| cx.debug_bounds("pane-caption-1-focused"))
            .unwrap();
        let right_edge = point(second.right() - px(8.0), second.center().y);
        drag_pane_caption(
            caption,
            &[right_edge, right_edge + point(px(-1.0), px(0.0))],
            cx,
        );
        cx.simulate_mouse_up(right_edge, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(30));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let top = cx.debug_bounds("pane-surface-1").unwrap();
        let beneath = cx.debug_bounds("pane-surface-2").unwrap();
        // Pane 1's Terminal keeps the size of its place, so only Pane 2's Terminal is under the
        // right of Pane 1's surface.
        let pointer = point(top.right() - px(8.0), beneath.top() + px(80.0));
        assert!(top.contains(&pointer) && beneath.contains(&pointer));
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| view.focus(window, cx));
        });
        cx.run_until_parked();
        let before = records.commands().len();
        cx.simulate_event(ScrollWheelEvent {
            position: pointer,
            delta: ScrollDelta::Lines(point(0.0, -3.0)),
            touch_phase: TouchPhase::Moved,
            modifiers: Modifiers::none(),
        });

        let scrolled = records.commands()[before..]
            .iter()
            .filter(|call| matches!(call.command, RecordedCommand::Wheel(_)))
            .map(|call| call.session_id)
            .collect::<Vec<_>>();
        assert!(
            !scrolled.contains(&2),
            "Pane 2's Terminal Session is beneath Pane 1: {scrolled:?}"
        );
    }

    #[gpui::test]
    fn a_press_beside_a_zooming_pane_should_not_focus_a_faded_pane(cx: &mut TestAppContext) {
        use crate::ui::drag_and_drop::AnimatePresence;

        let (view, [_, second, _], _, cx) = three_pane_zoom_view(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        draw_motion_frame(cx);
        let growing = cx.debug_bounds("pane-surface-1").unwrap();
        let beside = point(second.right() - px(4.0), second.center().y);
        let faded = view.read_with(cx, |view, _| view.pane_frames.beneath_opacity);
        assert!(!growing.contains(&beside));

        let focused = press_counting_focus_changes(&view, beside, cx);

        assert_eq!(
            (faded, focused),
            (0.0, (PaneId::new(1), 0)),
            "Pane 2 has faded out beneath the zoom and must not take the press"
        );
    }

    #[gpui::test]
    fn a_zoom_during_a_split_should_keep_the_other_panes_moving_beneath_it(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::drag_and_drop::AnimatePresence;

        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let session_factory = test_session_factory();
        let (view, cx) = cx
            .add_window_view(|window, cx| TabView::new(TabId::new(1), session_factory, window, cx));
        cx.run_until_parked();
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        draw_motion_frame(cx);
        let drawn = cx.debug_bounds("pane-surface-1").unwrap();

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.focus_pane(PaneId::new(2), cx);
                view.toggle_zoom(window, cx);
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let zooming = cx.debug_bounds("pane-surface-1").unwrap();

        assert!(
            (zooming.right() - drawn.right()).abs() < px(1.0),
            "Pane 1 must continue from where it was drawn: {zooming:?} after {drawn:?}"
        );
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

        assert_eq!(view.read_with(cx, |view, _| view.tab_title()), "• Terminal");
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
            repository: None,
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
        let fonts = cx.update(|cx| crate::host_fonts::HostFonts::get(cx));
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| {
            // UI initialization installs a default profile; retain the fixture's font families.
            cx.set_global(fonts);
            let resolved = super::super::appearance_runtime::current(cx);
            let active = Arc::new(super::super::appearance::ChromeAppearance {
                typography: ChromeTypography::prepare(
                    &resolved.chrome.typography,
                    resolved.chrome.density,
                ),
                ..Default::default()
            });
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

    #[gpui::test]
    fn pane_captions_publish_groups_that_focus_and_operate_their_pane(cx: &mut TestAppContext) {
        use gpui::accesskit::Action;
        use spaceterm_ui::a11y_testing::{A11yTree, perform};

        let (_, view, records, cx) = caption_view(cx);
        view.update(cx, |view, cx| {
            view.pane_captions.get_mut(&PaneId::new(1)).unwrap().name = "Primary shell".into();
            cx.notify();
        });
        let closes = Rc::new(RefCell::new(Vec::new()));
        let requested_closes = closes.clone();
        view.update(cx, |_, cx| {
            cx.subscribe(&view, move |_, _, event: &TabViewEvent, _| {
                if let TabViewEvent::UserClosePaneRequested { pane_id, .. } = event {
                    requested_closes.borrow_mut().push(*pane_id);
                }
            })
            .detach();
        });
        let tree = A11yTree::read(cx);
        assert_eq!(
            tree.node("Pane Caption, Primary shell")["aria"]["role"],
            "Group"
        );
        perform(cx, tree.node("Split Right"), Action::Click);
        assert_eq!(view.read_with(cx, |view, _| view.pane_count()), 2);

        view.update(cx, |view, cx| {
            view.pane_captions.get_mut(&PaneId::new(2)).unwrap().name = "Second shell".into();
            cx.notify();
        });
        let tree = A11yTree::read(cx);
        perform(cx, tree.node("Pane Caption, Primary shell"), Action::Click);
        assert_eq!(
            view.read_with(cx, |view, _| view.focused_pane_id()),
            PaneId::new(1)
        );
        assert!(cx.update(|window, cx| view.read(cx).focused_terminal_has_input_focus(window, cx)));
        // The controls are exposed once their reveal settles.
        crate::ui::settle_hover(cx);
        let tree = A11yTree::read(cx);
        let controls = tree.children(tree.node("Pane Caption, Primary shell"));
        let control = |name| {
            controls
                .iter()
                .find(|node| node["aria"]["label"] == name)
                .unwrap()
        };
        for name in ["Split Right", "Split Down", "Zoom Pane", "Close Pane"] {
            assert_eq!(control(name)["aria"]["role"], "Button");
        }
        perform(cx, control("Zoom Pane"), Action::Click);
        let tree = A11yTree::read(cx);
        assert!(tree.find("Pane Caption, Second shell").is_none());
        perform(cx, tree.node("Restore Panes"), Action::Click);
        let tree = A11yTree::read(cx);
        let close = tree
            .children(tree.node("Pane Caption, Primary shell"))
            .into_iter()
            .find(|node| node["aria"]["label"] == "Close Pane")
            .unwrap();
        perform(cx, close, Action::Click);
        assert_eq!(*closes.borrow(), [PaneId::new(1)]);
        assert_eq!(
            view.read_with(cx, |view, _| view.pane_count()),
            2,
            "Close Confirmation owns authorization to remove the Pane"
        );
        assert_eq!(records.pointer_count(), 0);
    }

    fn first_mouse_click_caption_control(selector: &'static str, cx: &mut VisualTestContext) {
        let control = cx
            .debug_bounds(selector)
            .expect("caption control must exist")
            .center();
        first_mouse_click(control, cx);
    }

    fn first_mouse_click(control: Point<Pixels>, cx: &mut VisualTestContext) {
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
    fn split_panes_publish_terminal_nodes_in_layout_order_and_follow_input_focus(
        cx: &mut TestAppContext,
    ) {
        use gpui::accesskit::Action;
        use spaceterm_ui::a11y_testing::{A11yTree, node_id, perform};

        cx.update(crate::ui::init).unwrap();
        let factory = test_session_factory();
        let construction = PaneConstruction::new(
            Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
            Rc::new(crate::platform::accesskit_terminal_accessibility::AccessKitTerminalAccessibilityAdapterFactory),
            crate::terminal::native_services::testing::adapters(),
            crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
        );
        let (view, cx) = cx.add_window_view(|window, cx| {
            TabView::with_pane_construction(TabId::new(1), factory, construction, window, cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| view.activate(window, cx));
        });
        cx.run_until_parked();
        split_test_pane(&view, PaneId::new(1), SplitAxis::Horizontal, cx);
        split_test_pane(&view, PaneId::new(1), SplitAxis::Vertical, cx);

        view.update(cx, |view, cx| {
            for (pane, title) in [(1, "Left upper"), (2, "Right"), (3, "Left lower")] {
                view.pane_captions.get_mut(&PaneId::new(pane)).unwrap().name = title.into();
            }
            cx.notify();
        });
        let tree = A11yTree::read(cx);
        let panes = tree
            .in_order()
            .into_iter()
            .filter(|node| node["aria"]["role"] == "Terminal")
            .collect::<Vec<_>>();
        assert_eq!(panes.len(), 3);
        assert_eq!(
            panes
                .iter()
                .map(|pane| node_id(pane))
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        for pane in &panes {
            let children = tree.children(pane);
            assert_eq!(
                children
                    .iter()
                    .map(|child| node_id(child))
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                children.len()
            );
        }
        let captions = tree
            .in_order()
            .into_iter()
            .filter(|node| {
                node["aria"]["role"] == "Group"
                    && node["aria"]["label"]
                        .as_str()
                        .is_some_and(|label| label.starts_with("Pane Caption, "))
            })
            .map(|node| node["aria"]["label"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            captions,
            [
                "Pane Caption, Left upper",
                "Pane Caption, Left lower",
                "Pane Caption, Right"
            ]
        );
        assert_eq!(node_id(tree.focused().unwrap()), node_id(panes[1]));

        perform(cx, panes[2], Action::Focus);
        let tree = A11yTree::read(cx);
        assert_eq!(node_id(tree.focused().unwrap()), node_id(panes[2]));
        assert_eq!(
            view.read_with(cx, |view, _| view.focused_pane_id()),
            PaneId::new(2)
        );
        assert!(cx.update(|window, cx| view.read(cx).focused_terminal_has_input_focus(window, cx)));

        cx.simulate_keystrokes("cmd-alt-left");
        let tree = A11yTree::read(cx);
        let focused = view.read_with(cx, |view, _| view.focused_pane_id());
        assert_ne!(focused, PaneId::new(2));
        assert!(cx.update(|window, cx| view.read(cx).focused_terminal_has_input_focus(window, cx)));
        assert_ne!(node_id(tree.focused().unwrap()), node_id(panes[2]));

        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        assert_eq!(A11yTree::read(cx).with_role("Terminal").len(), 1);
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_zoom(window, cx)));
        assert_eq!(A11yTree::read(cx).with_role("Terminal").len(), 3);
        let removed_caption = view.read_with(cx, |view, _| {
            format!("Pane Caption, {}", view.pane_captions[&focused].name)
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.close_pane_authorized(focused, window, cx)
            })
        });
        let tree = A11yTree::read(cx);
        assert_eq!(tree.with_role("Terminal").len(), 2);
        assert!(tree.find(&removed_caption).is_none());
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

        // Hidden controls wait past their caption's trailing edge, so the click aims where Split
        // Down slides in.
        let hidden = cx.debug_bounds("pane-split-down-1").unwrap().center();
        let controls = cx.debug_bounds("pane-controls-1-full").unwrap();
        first_mouse_click(gpui::point(hidden.x - controls.size.width, hidden.y), cx);
        assert_eq!(
            view.read_with(cx, |view, _| (view.pane_count(), view.focused_pane_id())),
            (2, PaneId::new(1)),
            "the activation click may focus its Pane but must not invoke its hidden action",
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
    fn caption_and_tab_should_show_the_host_shell_again_after_a_command_finishes(
        cx: &mut TestAppContext,
    ) {
        use crate::terminal::metadata::{CommandMetadata, CommandState, TitleProvenance};
        let (_, view, records, cx) = caption_view(cx);
        let mut screen =
            ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "zsh", 1);
        let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
        metadata.title.provenance = TitleProvenance::Fallback;
        // The client factory's placeholder says zsh; the Pane host actually launched fish.
        metadata.shell_name = Arc::from("fish");
        metadata.directory.path = Arc::from("/srv/new-place");
        records
            .event_sender(1)
            .unwrap()
            .try_send(TerminalSessionEvent::Screen(screen.clone()))
            .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.pane_captions[&PaneId::new(1)].label, "fish");
            assert_eq!(view.tab_title(), "fish · new-place");
        });

        let snapshot = Arc::make_mut(&mut screen);
        snapshot.generation = crate::terminal::PresentationGeneration::test(2);
        let metadata = Arc::make_mut(&mut snapshot.metadata);
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
        assert_eq!(
            view.read_with(cx, |view, _| view.tab_title()),
            "π build · new-place"
        );
        let snapshot = Arc::make_mut(&mut screen);
        snapshot.generation = crate::terminal::PresentationGeneration::test(3);
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
            ("/srv/".into(), "new-place".into(), "fish".into(), None)
        );
        assert_eq!(
            view.read_with(cx, |view, _| view.tab_title()),
            "fish · new-place"
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
    fn a_narrowing_caption_with_a_branch_should_keep_its_name_whole_and_apart_from_its_status(
        cx: &mut TestAppContext,
    ) {
        let (root, view, records, cx) = caption_view(cx);
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
        metadata.directory.path = Arc::from("/Users/tester");
        records
            .event_sender(1)
            .unwrap()
            .try_send(TerminalSessionEvent::Screen(screen))
            .unwrap();
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.pane_captions
                .entry(PaneId::new(1))
                .or_default()
                .repository = Some(RepositoryCaption {
                glyph: HeadGlyph::Branch,
                branch: "feat/workbench-fixture".into(),
                mark: Some(RepositoryMark::Changes),
                detail: None,
                divergence: None,
                dimmed: false,
                accessible_label: "Repository Status, feat/workbench-fixture".into(),
            });
            cx.notify();
        });
        crate::ui::settle_hover(cx);
        let name_width = cx.debug_bounds("pane-caption-name-1").unwrap().size.width;

        let mut truncated = 0;
        for width in (130..420).step_by(2) {
            root.update(cx, |root, cx| {
                root.width = px(width as f32);
                cx.notify();
            });
            cx.run_until_parked();
            let name = cx.debug_bounds("pane-caption-name-1").unwrap();
            let status = cx.debug_bounds("pane-caption-status-1").unwrap();
            let Some(branch) = cx.debug_bounds("pane-repository-branch-1") else {
                continue;
            };
            assert!(
                name.size.width >= name_width - px(0.5),
                "at {width}: the branch {branch:?} squeezed the name {name:?}"
            );
            assert!(
                status.left() - name.right() >= px(PANE_STATUS_LEADING_GAP - 0.5),
                "at {width}: {name:?} {status:?}"
            );
            if cx.debug_bounds("pane-caption-status-separator-1").is_none() {
                truncated += 1;
            }
        }
        assert!(
            truncated > 0,
            "some widths show the branch without the middle dot"
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
            status_gap: px(PANE_STATUS_LEADING_GAP),
            status_separator: px(PANE_CAPTION_SEPARATOR_WIDTH - PANE_STATUS_LEADING_GAP),
            label: px(30.0),
            repository: None,
        };
        let resolve = |width: f32| {
            CaptionLayout::from_metrics(
                false,
                px(width),
                1.0,
                metrics,
                1.0,
                13.0,
                PANE_CONTROL_SIZE,
            )
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
            repository: RepositoryCaptionLayout::default(),
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

    #[gpui::test]
    fn a_long_repository_branch_should_keep_its_segment_inside_the_caption_before_controls(
        cx: &mut TestAppContext,
    ) {
        assert_long_repository_branch_bounds(cx, Some(RepositoryMark::Changes));
    }

    pub(super) fn assert_long_repository_branch_bounds(
        cx: &mut TestAppContext,
        mark: Option<RepositoryMark>,
    ) {
        let (root, view, records, cx) = caption_view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.pane_count()), 2);
        let mut screen =
            ScreenSnapshot::from_test_parts_at(Arc::from([]), Default::default(), "zsh", 1);
        let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
        metadata.context = crate::terminal::metadata::TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            "/home/tester",
            crate::terminal::metadata::LocalMachine::new(
                Some("tester"),
                Some("workstation"),
                Some("/home/tester"),
            ),
        );
        metadata.directory.path = Arc::from("/home/tester/api");
        records
            .event_sender(1)
            .unwrap()
            .try_send(TerminalSessionEvent::Screen(screen))
            .unwrap();
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.pane_captions
                .entry(PaneId::new(1))
                .or_default()
                .repository = Some(RepositoryCaption {
                glyph: HeadGlyph::Branch,
                branch: "fix/linux-terminal-accessibility-child-elements".into(),
                mark,
                detail: mark.map(|_| crate::repository_status::presentation::CaptionDetail {
                    text: "3 changed".into(),
                    dimmed: false,
                }),
                divergence: Some("\u{2191}2 \u{2193}1".into()),
                dimmed: false,
                accessible_label: "Repository Status, long branch, 3 changed".into(),
            });
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| view.focus_pane(PaneId::new(1), cx));
        crate::ui::settle_hover(cx);
        let mut branches = 0;
        let mut segments = 0;
        let mut overlaps = Vec::new();
        for width in [
            2000.0, 1280.0, 840.0, 654.0, 520.0, 400.0, 320.0, 260.0, 200.0,
        ] {
            root.update(cx, |root, cx| {
                root.width = px(width);
                cx.notify();
            });
            cx.run_until_parked();
            let other_pane = cx.debug_bounds("pane-surface-2").unwrap();
            cx.simulate_mouse_move(other_pane.center(), None, gpui::Modifiers::none());
            view.update(cx, |view, cx| view.focus_pane(PaneId::new(2), cx));
            crate::ui::settle_hover(cx);
            if let Some(segment) = cx.debug_bounds("pane-repository-1") {
                let caption = cx.debug_bounds("pane-caption-1-unfocused").unwrap();
                if segment.right() > caption.right() {
                    overlaps.push(format!(
                        "at {width}: unfocused segment {segment:?} exceeds caption {caption:?}"
                    ));
                }
            }
            view.update(cx, |view, cx| view.focus_pane(PaneId::new(1), cx));
            cx.run_until_parked();
            for _ in 0..12 {
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(16));
                cx.update(|window, cx| window.simulate_next_frame(cx));
                cx.run_until_parked();
                if let Some(segment) = cx.debug_bounds("pane-repository-1") {
                    let clip = cx.debug_bounds("pane-controls-clip-1").unwrap();
                    if clip.size.width > px(0.0) {
                        assert!(
                            segment.right() <= clip.left(),
                            "at {width}: {segment:?} overlaps revealing controls {clip:?}"
                        );
                    }
                }
            }
            crate::ui::settle_hover(cx);
            let caption = cx.debug_bounds("pane-caption-1-focused").unwrap();
            let Some(segment) = cx.debug_bounds("pane-repository-1") else {
                continue;
            };
            segments += 1;
            assert!(
                segment.left() >= caption.left() && segment.right() <= caption.right(),
                "at {width}: segment {segment:?} exceeds caption {caption:?}"
            );
            let control = cx
                .debug_bounds("pane-split-right-1")
                .or_else(|| cx.debug_bounds("pane-toggle-zoom-1"))
                .unwrap();
            assert!(
                segment.right() <= control.left(),
                "at {width}: segment {segment:?} overlaps first control {control:?}"
            );
            if let Some(branch) = cx.debug_bounds("pane-repository-branch-1") {
                branches += 1;
                assert!(
                    branch.right() <= segment.right(),
                    "at {width}: {branch:?} {segment:?}"
                );
            }
        }
        assert!(
            segments >= 5,
            "several narrow and wide Panes show Repository Status"
        );
        assert!(branches >= 3, "several widths exercise branch truncation");
        assert!(overlaps.is_empty(), "{}", overlaps.join("\n"));
    }

    #[gpui::test]
    fn repository_status_should_take_the_place_of_an_unfocused_panes_hidden_controls(
        cx: &mut TestAppContext,
    ) {
        use spaceterm_ui::a11y_testing::A11yTree;

        let (_, view, _, cx) = caption_view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.pane_captions
                    .entry(PaneId::new(1))
                    .or_default()
                    .repository = Some(RepositoryCaption {
                    glyph: HeadGlyph::Branch,
                    branch: "main".into(),
                    mark: None,
                    detail: None,
                    divergence: None,
                    dimmed: false,
                    accessible_label: "Repository Status, main".into(),
                });
                cx.notify();
            });
        });
        crate::ui::settle_hover(cx);
        // The pointer rests over the Focused Pane, so the other Pane is not hovered.
        let focused_pane = cx.debug_bounds("pane-surface-2").unwrap();
        cx.simulate_mouse_move(focused_pane.center(), None, gpui::Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(
            view.read_with(cx, |view, _| view.focused_pane_id()),
            PaneId::new(2)
        );
        let segment_right =
            |cx: &mut VisualTestContext| cx.debug_bounds("pane-repository-1").unwrap().right();
        // Hidden controls publish no frame that assistive technology could find under the
        // Repository Status, so only the Focused Pane's controls are exposed.
        let exposed_closes = |cx: &mut VisualTestContext| {
            let tree = A11yTree::read(cx);
            tree.with_role("Button")
                .into_iter()
                .filter(|node| node["aria"]["label"] == "Close Pane" && tree.exposed(node))
                .count()
        };
        let trailing = cx.debug_bounds("pane-controls-clip-1").unwrap().right();
        assert!(
            (segment_right(cx) - trailing).abs() < px(0.5),
            "an unfocused Pane's hidden controls leave their place to the Repository Status",
        );
        assert_eq!(exposed_closes(cx), 1);

        view.update(cx, |view, cx| view.focus_pane(PaneId::new(1), cx));
        crate::ui::settle_hover(cx);
        let controls = cx.debug_bounds("pane-controls-1-full").unwrap();
        assert!(
            segment_right(cx) <= controls.left(),
            "the Focused Pane's controls push the Repository Status before them",
        );

        view.update(cx, |view, cx| view.focus_pane(PaneId::new(2), cx));
        crate::ui::settle_hover(cx);
        assert!((segment_right(cx) - trailing).abs() < px(0.5));
    }

    #[gpui::test]
    fn controls_growing_in_should_keep_their_gap_from_the_repository_status(
        cx: &mut TestAppContext,
    ) {
        use spaceterm_ui::a11y_testing::A11yTree;

        let (_, view, _, cx) = caption_view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
                view.pane_captions
                    .entry(PaneId::new(1))
                    .or_default()
                    .repository = Some(RepositoryCaption {
                    glyph: HeadGlyph::Branch,
                    branch: "main".into(),
                    mark: None,
                    detail: None,
                    divergence: Some("\u{2191}1".into()),
                    dimmed: false,
                    accessible_label: "Repository Status, main".into(),
                });
                cx.notify();
            });
        });
        crate::ui::settle_hover(cx);
        let focused_pane = cx.debug_bounds("pane-surface-2").unwrap();
        cx.simulate_mouse_move(focused_pane.center(), None, gpui::Modifiers::none());
        crate::ui::settle_hover(cx);

        view.update(cx, |view, cx| view.focus_pane(PaneId::new(1), cx));
        cx.run_until_parked();
        let mut partly_revealed = false;
        for _ in 0..12 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(16));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            let segment = cx.debug_bounds("pane-repository-1").unwrap();
            let clip = cx.debug_bounds("pane-controls-clip-1").unwrap();
            let controls = cx.debug_bounds("pane-controls-1-full").unwrap();
            if clip.size.width > px(0.0) && clip.size.width < controls.size.width {
                partly_revealed = true;
                // The revealing controls slide in with their clip's leading edge.
                assert!(
                    (controls.left() - clip.left()).abs() < px(0.5),
                    "{controls:?} {clip:?}"
                );
                // Only the hovered Pane's settled controls are exposed; the revealing ones overhang
                // their clip.
                let tree = A11yTree::read(cx);
                let exposed = tree
                    .with_role("Button")
                    .into_iter()
                    .filter(|node| node["aria"]["label"] == "Close Pane" && tree.exposed(node))
                    .count();
                assert_eq!(exposed, 1);
            }
            if clip.size.width > px(0.0) {
                assert!(
                    clip.left() - segment.right() >= px(PANE_CONTROL_LEADING_GAP - 0.5),
                    "{segment:?} {clip:?}"
                );
            }
        }
        assert!(partly_revealed, "the controls ease in over several frames");
    }

    #[gpui::test]
    fn a_split_should_move_the_repository_status_with_its_layout_instead_of_after_it(
        cx: &mut TestAppContext,
    ) {
        let (_, view, _, cx) = caption_view(cx);
        view.update(cx, |view, cx| {
            view.pane_captions
                .entry(PaneId::new(1))
                .or_default()
                .repository = Some(RepositoryCaption {
                glyph: HeadGlyph::Branch,
                branch: "main".into(),
                mark: None,
                detail: None,
                divergence: None,
                dimmed: false,
                accessible_label: "Repository Status, main".into(),
            });
            cx.notify();
        });
        cx.simulate_event(gpui::MouseExitEvent {
            position: gpui::point(px(0.0), px(0.0)),
            pressed_button: None,
            modifiers: gpui::Modifiers::none(),
        });
        crate::ui::settle_hover(cx);

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        // No time passes, so only a change made with the layout shows.
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |view, _| view.focused_pane_id()),
            PaneId::new(2)
        );
        let segment = cx.debug_bounds("pane-repository-1").unwrap();
        let trailing = cx.debug_bounds("pane-controls-clip-1").unwrap().right();
        assert!((segment.right() - trailing).abs() < px(0.5));
    }

    #[gpui::test]
    fn a_caption_button_split_should_place_the_controls_it_moves_from_under_the_pointer_at_once(
        cx: &mut TestAppContext,
    ) {
        let (_, view, _, cx) = caption_view(cx);
        view.update(cx, |view, cx| {
            view.pane_captions
                .entry(PaneId::new(1))
                .or_default()
                .repository = Some(RepositoryCaption {
                glyph: HeadGlyph::Branch,
                branch: "main".into(),
                mark: None,
                detail: None,
                divergence: None,
                dimmed: false,
                accessible_label: "Repository Status, main".into(),
            });
            cx.notify();
        });
        crate::ui::settle_hover(cx);
        // The pointer rests on the Pane's trailing caption control, as after clicking Split.
        let controls = cx.debug_bounds("pane-controls-1-full").unwrap();
        let pointer = gpui::point(controls.right() - px(4.0), controls.center().y);
        cx.simulate_mouse_move(pointer, None, gpui::Modifiers::none());
        crate::ui::settle_hover(cx);

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        // The narrower Pane leaves the still pointer; its hover catches up one frame later.
        cx.run_until_parked();
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let placed = (
            cx.debug_bounds("pane-repository-1").unwrap(),
            cx.debug_bounds("pane-controls-1-full").unwrap(),
        );

        crate::ui::settle_hover(cx);
        let settled = (
            cx.debug_bounds("pane-repository-1").unwrap(),
            cx.debug_bounds("pane-controls-1-full").unwrap(),
        );
        assert_eq!(placed, settled);
    }

    #[test]
    fn narrowing_a_caption_should_keep_the_repository_branch_and_drop_its_counts_first() {
        let repository = RepositoryCaptionMetrics {
            anchor: px(20.0),
            branch: px(50.0),
            detail: Some(px(70.0)),
            divergence: Some(px(30.0)),
        };
        let metrics = CaptionMetrics {
            user: px(40.0),
            host: px(60.0),
            directory: px(120.0),
            name: px(40.0),
            status_gap: px(PANE_STATUS_LEADING_GAP),
            status_separator: px(PANE_CAPTION_SEPARATOR_WIDTH - PANE_STATUS_LEADING_GAP),
            label: px(30.0),
            repository: Some(repository),
        };
        let controls = PANE_CAPTION_LEFT_PADDING
            + PANE_CAPTION_RIGHT_PADDING
            + PANE_STATUS_WIDTH
            + PANE_CONTROL_LEADING_GAP
            + controls_width(2, false, PANE_CONTROL_SIZE, 1.0)
            + 20.0;
        let resolve = |metrics, width: f32| {
            CaptionLayout::from_metrics(
                false,
                px(width),
                1.0,
                metrics,
                1.0,
                13.0,
                PANE_CONTROL_SIZE,
            )
        };
        let shown = |layout: CaptionLayout| {
            (
                layout.repository.show_branch,
                layout.show_status_separator,
                layout.repository.show_detail,
                layout.show_host,
                layout.show_label,
                layout.repository.show_divergence,
            )
        };

        let full = resolve(metrics, controls + 457.25);
        assert_eq!(shown(full), (true, true, true, true, true, true));
        assert_eq!(full.repository.branch_limit, None);
        assert_eq!(
            shown(resolve(metrics, controls + 457.0)),
            (true, true, true, true, true, false)
        );
        assert_eq!(
            shown(resolve(metrics, controls + 177.25)),
            (true, true, true, false, false, false)
        );
        assert_eq!(
            shown(resolve(metrics, controls + 177.0)),
            (true, true, false, false, false, false)
        );
        assert_eq!(
            shown(resolve(metrics, controls + 90.0)),
            (true, false, false, false, false, false)
        );

        let truncated = resolve(metrics, controls + 89.0);
        assert!(truncated.repository.show_branch);
        assert_eq!(truncated.repository.branch_limit, Some(40));
        assert!(!truncated.show_status_separator);
        assert!(!resolve(metrics, controls + 75.0).repository.show_branch);

        let without_counts = CaptionMetrics {
            repository: Some(RepositoryCaptionMetrics {
                detail: None,
                divergence: None,
                ..repository
            }),
            ..metrics
        };
        assert_eq!(
            shown(resolve(without_counts, controls + 167.25)),
            (true, true, false, true, false, false)
        );
    }

    #[test]
    fn a_pane_too_narrow_for_its_name_beside_the_repository_glyph_should_hide_repository_status() {
        let metrics = CaptionMetrics {
            name: px(40.0),
            status_gap: px(PANE_STATUS_LEADING_GAP),
            status_separator: px(PANE_CAPTION_SEPARATOR_WIDTH - PANE_STATUS_LEADING_GAP),
            repository: Some(RepositoryCaptionMetrics {
                anchor: px(20.0),
                branch: px(50.0),
                detail: None,
                divergence: None,
            }),
            ..CaptionMetrics::default()
        };
        let needed = PANE_CAPTION_LEFT_PADDING
            + PANE_CAPTION_RIGHT_PADDING
            + PANE_STATUS_WIDTH
            + 20.0
            + PANE_CONTROL_LEADING_GAP
            + controls_width(4, true, PANE_CONTROL_SIZE, 1.0)
            + 40.0
            + PANE_STATUS_LEADING_GAP;
        let resolve = |width: f32, reveal| {
            CaptionLayout::from_metrics(
                true,
                px(width),
                reveal,
                metrics,
                1.0,
                13.0,
                PANE_CONTROL_SIZE,
            )
        };

        let roomy = resolve(needed + 0.5, 1.0);
        assert!(roomy.repository.show && roomy.show_splits);
        assert!(!roomy.repository.show_branch);
        for reveal in [0.0, 1.0] {
            assert!(
                !resolve(needed - 0.5, reveal).repository.show,
                "at reveal {reveal}"
            );
        }
    }

    #[test]
    fn hidden_pane_controls_should_free_their_width_for_the_caption() {
        let metrics = CaptionMetrics {
            user: px(40.0),
            host: px(60.0),
            directory: px(120.0),
            name: px(40.0),
            status_gap: px(PANE_STATUS_LEADING_GAP),
            status_separator: px(PANE_CAPTION_SEPARATOR_WIDTH - PANE_STATUS_LEADING_GAP),
            label: px(30.0),
            repository: Some(RepositoryCaptionMetrics {
                anchor: px(20.0),
                branch: px(50.0),
                detail: Some(px(70.0)),
                divergence: Some(px(30.0)),
            }),
        };
        let controls = PANE_CONTROL_LEADING_GAP + controls_width(2, false, PANE_CONTROL_SIZE, 1.0);
        let width = PANE_CAPTION_LEFT_PADDING
            + PANE_CAPTION_RIGHT_PADDING
            + PANE_STATUS_WIDTH
            + 20.0
            + controls
            + 89.0;
        let resolve = |metrics, reveal| {
            CaptionLayout::from_metrics(
                false,
                px(width),
                reveal,
                metrics,
                1.0,
                13.0,
                PANE_CONTROL_SIZE,
            )
        };

        assert_eq!(resolve(metrics, 1.0).repository.branch_limit, Some(40));
        let hidden = resolve(metrics, 0.0);
        assert!(hidden.repository.show_branch);
        assert_eq!(hidden.repository.branch_limit, None);
        assert!(hidden.show_splits, "hidden controls keep their set");

        let without_repository = CaptionMetrics {
            repository: None,
            ..metrics
        };
        assert!(!resolve(without_repository, 1.0).show_host);
        assert!(resolve(without_repository, 0.0).show_host);
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
            CaptionLayout::from_metrics(
                false,
                px(width),
                1.0,
                metrics,
                1.0,
                13.0,
                PANE_CONTROL_SIZE,
            )
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
    fn a_split_after_layout_should_skip_a_pane_closed_before_the_layout(cx: &mut TestAppContext) {
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

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_focused_after_layout(SplitAxis::Vertical, cx);
                view.close_pane_authorized(PaneId::new(2), window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |view, _| view.layout_signature()),
            "pane:1"
        );
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
            });
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| view.focus_pane(PaneId::new(1), cx));
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
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(records.clone()).with_client_filesystem(
                crate::platform::local_filesystem::LocalFilesystemAuthority::testing(),
            ),
        );
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
    #[gpui::test]
    fn split_resize_handles_move_to_the_offset_assistive_technology_sets(cx: &mut TestAppContext) {
        use gpui::accesskit::{Action, ActionData};
        use spaceterm_ui::a11y_testing::{A11yTree, perform_with};

        let (view, cx) = split_gap_view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.split_pane(PaneId::new(1), SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let extent = view.read_with(cx, |view, cx| {
            let bounds = view.split_bounds.values().next().copied().unwrap();
            split_content_extent(SplitAxis::Horizontal, bounds, pane_gap(cx)).unwrap()
        });
        let set_offset = |offset: f64, cx: &mut VisualTestContext| {
            let tree = A11yTree::read(cx);
            perform_with(
                cx,
                tree.node("Resize Pane split"),
                Action::SetValue,
                Some(ActionData::NumericValue(offset)),
            );
            cx.run_until_parked();
        };
        let published = |cx: &mut VisualTestContext| {
            A11yTree::read(cx).node("Resize Pane split")["aria"]["numeric_value"]
                .as_f64()
                .unwrap()
        };

        set_offset(f64::from(extent * 0.25), cx);
        assert_eq!(split_ratio(&view, cx), 0.25);
        assert_eq!(published(cx), f64::from(extent * 0.25));

        // The Tab keeps the second Pane at its minimum size, as it does for a drag.
        set_offset(f64::from(extent * 10.0), cx);
        let ratio = split_ratio(&view, cx);
        assert!(ratio > 0.25 && ratio < 1.0, "{ratio}");
        assert_eq!(published(cx), f64::from(extent * ratio));
        assert_eq!(view.read_with(cx, |view, _| view.resizing_split_id), None);
    }

    /// Each Split's published value, minimum, and maximum, from the Tab's own bounds.
    fn expected_split_ranges(
        view: &Entity<TabView>,
        cx: &mut VisualTestContext,
    ) -> Vec<(&'static str, (f64, f64, f64))> {
        fn collect(tree: PaneTreeRef<'_>, splits: &mut Vec<(SplitId, SplitAxis, f32)>) {
            if let PaneNodeRef::Split {
                split_id,
                axis,
                ratio,
                first,
                second,
            } = tree.node()
            {
                splits.push((split_id, axis, ratio));
                collect(first, splits);
                collect(second, splits);
            }
        }

        view.read_with(cx, |view, cx| {
            let gap = pane_gap(cx);
            let mut splits = Vec::new();
            collect(view.tab.root(), &mut splits);
            splits
                .into_iter()
                .map(|(split_id, axis, ratio)| {
                    let bounds = view.split_bounds[&split_id];
                    let extent = split_content_extent(axis, bounds, gap).unwrap();
                    let range = view
                        .tab
                        .split_ratio_range(split_id, pane_size(bounds).unwrap(), gap)
                        .unwrap();
                    let orientation = match axis {
                        SplitAxis::Horizontal => "Vertical",
                        SplitAxis::Vertical => "Horizontal",
                    };
                    let offset = |ratio: f32| f64::from(extent * ratio);
                    (
                        orientation,
                        (offset(ratio), offset(*range.start()), offset(*range.end())),
                    )
                })
                .collect()
        })
    }

    fn pane_split_handle<'a>(
        tree: &'a spaceterm_ui::a11y_testing::A11yTree,
        orientation: &str,
    ) -> &'a serde_json::Value {
        tree.with_role("Splitter")
            .into_iter()
            .find(|node| {
                node["aria"]["label"] == "Resize Pane split"
                    && node["aria"]["orientation"] == orientation
            })
            .unwrap_or_else(|| panic!("no {orientation} Pane split handle"))
    }

    /// The published value, minimum, and maximum of the Pane split handle with `orientation`.
    fn published_split_range(orientation: &str, cx: &mut VisualTestContext) -> (f64, f64, f64) {
        let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
        let splitter = pane_split_handle(&tree, orientation);
        let number = |key: &str| {
            splitter["aria"][key]
                .as_f64()
                .unwrap_or_else(|| panic!("the Pane split handle publishes no {key}"))
        };
        (
            number("numeric_value"),
            number("min_numeric_value"),
            number("max_numeric_value"),
        )
    }

    #[gpui::test]
    fn split_resize_handles_publish_the_range_their_tab_resizes_within(cx: &mut TestAppContext) {
        use gpui::accesskit::{Action, ActionData};
        use spaceterm_ui::a11y_testing::{A11yTree, perform_with};

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

        let expected = expected_split_ranges(&view, cx);
        assert_eq!(expected.len(), 2);
        for (orientation, range) in &expected {
            assert!(
                range.1 > 0.0 && range.1 < range.0 && range.0 < range.2,
                "{range:?}"
            );
            assert_eq!(published_split_range(orientation, cx), *range);
        }

        for (orientation, _) in expected {
            for requested in [-10_000.0, 10_000.0] {
                let tree = A11yTree::read(cx);
                perform_with(
                    cx,
                    pane_split_handle(&tree, orientation),
                    Action::SetValue,
                    Some(ActionData::NumericValue(requested)),
                );
                cx.run_until_parked();

                let (value, minimum, maximum) = published_split_range(orientation, cx);
                let bound = if requested < 0.0 { minimum } else { maximum };
                assert_eq!(value, bound, "{orientation} handle after {requested}");
                let current = expected_split_ranges(&view, cx)
                    .into_iter()
                    .find(|(candidate, _)| *candidate == orientation)
                    .unwrap()
                    .1;
                assert_eq!((value, minimum, maximum), current);
            }
        }
    }
}
