//! Workspace sidebar presentation and transient interaction lifecycle.
//! Workspace policy stays with the application; the sidebar emits typed requests.
#[cfg(test)]
mod motion_tests;
mod text;
mod view;

use super::drag_and_drop::{DragPreview, DragSession, ReorderableStrip};
use super::sidebar_motion::{Reveal, SidebarFrame, SidebarMotion};
use super::workspace_chrome::{
    WorkspaceChromeIdentity, WorkspaceChromeLayout, WorkspaceChromeStatus,
};
use super::workspace_creation::{RemoteCreationServer, WorkspaceCreation};
use super::workspace_status::{WorkspaceStatusPaint, resolve as resolve_workspace_status};
use super::{WORKSPACE_SIDEBAR_DEFAULT_WIDTH, WORKSPACE_SIDEBAR_MINIMUM_WIDTH};
use crate::appearance::ChromeColors;
use crate::appearance::Color;
use crate::domain::{RemoteConnectionPhase, WorkspaceId, WorktreeId};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DispatchPhase, Entity, EntityId, EventEmitter, FocusHandle,
    KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle,
    SharedString, WeakEntity, Window, canvas, div, point, px,
};
use spaceterm_ui::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, ButtonSize, ButtonVariant,
    ContextMenu, Icon, IconButton, IconName, Menu, MenuEntry, MenuLifecycleEvent, MenuSize,
    OverlayScrollbar, OverlayScrollbarEvent, ResizeAxis, ResizeFinishReason, ResizeHandle,
    ResizeHandleEvent, ResizeHandleTarget, ResizeInputSource, ScrollMetrics, TextInput,
    TextInputEvent, TextInputVariant, Tooltip, TooltipTargetVisibility, dismiss_active_menu,
};
use std::ops::RangeInclusive;

const CHROME_DIVIDER_SIZE: f32 = super::control_theme::resize_handle::VISIBLE_THICKNESS;
const SIDEBAR_FOOTER_HORIZONTAL_PADDING: f32 = 4.0;

#[derive(Clone)]
pub(super) enum SidebarEvent {
    Activate {
        workspace_id: WorkspaceId,
        focus_pane: bool,
    },
    Command {
        workspace_id: WorkspaceId,
        command: WorkspaceMenuCommand,
    },
    /// Shows a Worktree's Tabs, opening its first Tab when it has none.
    ActivateWorktree {
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        focus_pane: bool,
    },
    SetWorktreesExpanded {
        workspace_id: WorkspaceId,
        expanded: bool,
    },
    WorktreeCommand {
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        command: WorktreeMenuCommand,
    },
    Rename {
        workspace_id: WorkspaceId,
        name: String,
        /// The name version the draft began at, for a Workspace a SpaceTerm Server owns. The
        /// draft keeps it so a rename published while the editor was open conflicts instead of
        /// being silently overwritten.
        version: Option<u64>,
    },
    Move {
        workspace_id: WorkspaceId,
        position: usize,
    },
    Create(WorkspaceCreation),
    FocusChanged,
    LayoutChanged,
    FocusPane,
}
impl EventEmitter<SidebarEvent> for WorkspaceSidebar {}

/// Minimum Workspace row height before semantic text requires more room.
pub(super) const SIDEBAR_ROW_HEIGHT: f32 = 58.0;
pub(super) const SIDEBAR_ROW_TITLE_LINE_PADDING: f32 = 6.0;
pub(super) const SIDEBAR_ROW_DETAIL_LINE_PADDING: f32 = 3.0;
pub(super) const SIDEBAR_ROW_LINE_GAP: f32 = 2.0;
/// The air a row's content keeps inside the chip that carries its selection.
pub(super) const SIDEBAR_ROW_CHIP_PADDING: f32 = 6.0;
/// The vertical inset of the chip that carries a row's selection.
pub(super) const SIDEBAR_ROW_SELECTION_INSET_Y: f32 = 3.0;
pub(super) const NEW_WORKSPACE_BUTTON_HEIGHT: f32 = 40.0;
pub(super) const SIDEBAR_MAXIMUM_WIDTH: f32 = 420.0;
pub(super) const TERMINAL_CONTENT_MINIMUM_WIDTH: f32 = 240.0;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspaceMenuCommand {
    NewTab,
    /// Presents the New Worktree dialog for the Workspace's repository.
    NewWorktree,
    PinDirectory,
    UnpinDirectory,
    Reconnect,
    /// Removes this client's Workspace Attachment, leaving the Workspace and its Terminal
    /// Sessions running on its server. It has no default Shortcut.
    Detach,
    Close,
}

/// A SpaceTerm Server, by the route this client reaches it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ServerTarget {
    Local,
    Direct(crate::direct_connections::DirectRouteId),
}

/// What a Worktree row's menu asks of the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorktreeMenuCommand {
    /// Opens another Tab in the Worktree, or its first.
    NewTab,
    /// Copies the Worktree's root.
    CopyPath,
    /// Closes every Tab of the Worktree and keeps its row.
    CloseTabs,
    /// Asks to delete the Worktree's directory and closes its Tabs. The branch stays.
    Remove,
}

/// Whether a Worktree row offers Remove Worktree, and why not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorktreeRemoval {
    Allowed,
    /// The repository's Main Worktree.
    Main,
    Locked,
    /// A Workspace's Pinned Directory is inside the Worktree.
    HoldsPinnedDirectory,
    /// The owning server is still resolving where its Workspaces' Pinned Directories are.
    CheckingPinnedDirectories,
    /// The owning server couldn't resolve where its Workspaces' Pinned Directories are.
    PinnedDirectoriesUnavailable,
    /// The repository the Workspace reads doesn't list the Worktree, so only its Tabs remain.
    Unlisted,
    /// A SpaceTerm Server owns the Worktree, and removing it there is not available yet.
    Server,
    /// Shared edits wait for a connection or the previous server answer.
    NeedsServer,
}

#[derive(Clone, Copy)]
enum RowMenuCommand {
    Workspace(WorkspaceMenuCommand),
    /// A collapsed Workspace row acts on its Active Worktree.
    Worktree(WorktreeId, WorktreeMenuCommand),
    Rename,
}

struct WorkspaceRenameState {
    workspace_id: WorkspaceId,
    initial_value: String,
    /// The name version the row showed when editing began, kept for a shared Workspace.
    version: Option<u64>,
    input: Entity<TextInput>,
    focus_handle: FocusHandle,
    context_menu_open: bool,
    /// How many window modals borrow the keyboard and return it to the draft when they close.
    modal_holds: usize,
    /// Whether finishing returns focus to the sidebar rather than the Pane.
    return_to_sidebar: bool,
}

#[derive(Clone, PartialEq)]
pub(super) struct WorkspaceRowViewModel {
    pub(super) workspace_id: WorkspaceId,
    pub(super) name: SharedString,
    pub(super) path: SharedString,
    pub(super) machine: Option<SharedString>,
    pub(super) tooltip: SharedString,
    pub(super) pinned: bool,
    pub(super) remote_connection_phase: Option<RemoteConnectionPhase>,
    /// The branch or Pull Request that ends line 2, after the directory.
    pub(super) repository: Option<crate::repository_status::presentation::SidebarBadge>,
    /// The Worktrees a git Workspace discloses under its row.
    pub(super) worktrees: Option<WorktreeSection>,
    /// The Workspace reads a local repository, so it can create Worktrees.
    pub(super) creates_worktrees: bool,
    pub(super) active: bool,
    /// Present when a SpaceTerm Server owns the Workspace, which changes what its row offers.
    pub(super) shared: Option<SharedWorkspaceRow>,
}

/// Names a shared Workspace's origin wherever the sidebar and the Tab strip describe it.
pub(super) const SERVER_WORKSPACE_BADGE: &str = "Server";

/// Introduces the directory of a Worktree's last-used Pane once that Pane has left the Worktree.
pub(super) const OUTSIDE_PANE_LABEL: &str = "Pane outside this Worktree";

/// What a shared server Workspace's row presents and offers.
///
/// A shared edit goes to the server, so Rename, the pin commands, and Kill Workspace wait while
/// this client is offline or an earlier edit is unanswered. Its Repository Status and Worktrees
/// are what the owning server read; Worktree writes use that server too.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SharedWorkspaceRow {
    /// The name version the server published, which a rename draft keeps from the moment it
    /// begins.
    pub(super) name_version: u64,
    /// Whether a shared edit can be asked for now.
    pub(super) edits: bool,
    /// Why a shared edit is unavailable, for a disabled command and its row status.
    pub(super) edits_reason: &'static str,
    /// The short status line 2 shows in place of the directory while an edit that changes this
    /// row waits, or after the server connection was lost.
    pub(super) status: Option<crate::ui::shared_workspaces::SharedRowStatus>,
    /// Whether this client can change its Workspace Attachments now. Detaching needs no server,
    /// so being offline never withholds it; an attachment write SpaceTerm could not confirm does,
    /// until SpaceTerm reopens.
    pub(super) detaches: bool,
}

impl WorkspaceRowViewModel {
    /// Whether the row's own chip carries the selection, rather than its Active Worktree's row.
    fn selected(&self) -> bool {
        self.active
            && !self.worktrees.as_ref().is_some_and(|section| {
                section.expanded && section.rows().any(|worktree| worktree.active)
            })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct WorktreeSection {
    pub(super) expanded: bool,
    /// The listed repository's Main Worktree, as the expanded Workspace row describes it.
    pub(super) repository: Option<SharedString>,
    /// The read repository's Worktrees first, then each former repository's Worktrees that still
    /// have Tabs.
    pub(super) groups: Vec<WorktreeGroup>,
}

impl WorktreeSection {
    pub(super) fn rows(&self) -> impl Iterator<Item = &WorktreeRowViewModel> {
        self.groups.iter().flat_map(|group| &group.rows)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct WorktreeGroup {
    /// The name of a repository the Workspace no longer reads, shown above its Worktrees.
    pub(super) former_repository: Option<SharedString>,
    pub(super) rows: Vec<WorktreeRowViewModel>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct WorktreeRowViewModel {
    pub(super) worktree_id: WorktreeId,
    /// The Worktree directory's name, which titles the row.
    pub(super) name: SharedString,
    /// The branch name, or the detached commit id.
    pub(super) label: SharedString,
    pub(super) detached: bool,
    /// The Worktree's root.
    pub(super) path: SharedString,
    /// The directory line 2 shows: the last-used Pane's while it is inside the Worktree, else the
    /// root, compacted against home.
    pub(super) directory: SharedString,
    /// The full directory line 2 shows.
    pub(super) directory_tooltip: SharedString,
    /// The full directory of the Pane last used in the Worktree, once that Pane has left it.
    /// Line 2 then shows the Worktree's root and Repository Status rather than the Pane's.
    pub(super) outside_pane_directory: Option<SharedString>,
    /// The branch or Pull Request that ends line 2.
    pub(super) repository: Option<crate::repository_status::presentation::SidebarBadge>,
    /// The repository's Main Worktree, which is never removed.
    pub(super) main: bool,
    pub(super) has_tabs: bool,
    /// The owning server is available to create the first Tab, or this is a local Worktree.
    pub(super) opens_first_tab: bool,
    /// The Worktree holds the Workspace's Active Tab.
    pub(super) active: bool,
    pub(super) locked: bool,
    /// Git no longer has the Worktree's directory.
    pub(super) missing: bool,
    pub(super) removal: WorktreeRemoval,
    /// The editable Worktree has Tabs and an existing Tab outside it to keep.
    pub(super) closes_tabs: bool,
}

impl WorktreeRowViewModel {
    /// Whether activating the row shows Tabs: an existing Tab, or a first Tab in its directory.
    pub(super) fn openable(&self) -> bool {
        self.has_tabs || (!self.missing && self.opens_first_tab)
    }

    /// Whether the row offers New Tab: a Tab in this Worktree's directory.
    pub(super) fn opens_tabs(&self) -> bool {
        self.openable() && self.opens_first_tab
    }
}

/// One row the keyboard can stand on.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SidebarRowKey {
    Workspace(WorkspaceId),
    Worktree(WorkspaceId, WorktreeId),
}

impl SidebarRowKey {
    const fn workspace_id(self) -> WorkspaceId {
        match self {
            Self::Workspace(workspace_id) | Self::Worktree(workspace_id, _) => workspace_id,
        }
    }
}

/// One row of a Workspace group, by what it shows.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum GroupRow {
    /// The Workspace row that heads the group.
    Workspace,
    Worktree(WorktreeId),
    /// The label above a former repository's Worktrees, by a digest of its name and how many
    /// labels before it share that name, so it keeps its place while its Worktrees change.
    FormerRepository(u64),
}

/// What one row below a Workspace row shows.
#[derive(Clone, Copy, Debug)]
enum GroupRowContent<'a> {
    /// A Worktree row, with its position among its repository's Worktree rows and their count.
    Worktree(&'a WorktreeRowViewModel, (usize, usize)),
    /// The label above a former repository's Worktrees.
    FormerRepository(&'a SharedString),
}

/// A row that left its Workspace group, kept to draw while its place closes.
#[derive(Clone, Debug)]
enum DepartedRow {
    Worktree(Box<WorktreeRowViewModel>),
    FormerRepository(SharedString),
}

impl DepartedRow {
    fn content(&self) -> GroupRowContent<'_> {
        match self {
            Self::Worktree(row) => GroupRowContent::Worktree(row, (1, 1)),
            Self::FormerRepository(name) => GroupRowContent::FormerRepository(name),
        }
    }
}

/// How one Workspace group moves as its Worktree rows disclose, come, and go.
struct GroupMotion {
    /// How far the group has a Worktree section, which fades its chevron in and out.
    disclosing: Reveal,
    /// How far the Worktree rows are disclosed. One progress moves the Workspace row's height
    /// and line 2, the chevron's turn, the Worktree rows' reveal, and the groups below.
    disclosed: Reveal,
    /// The Worktree section the group last presented, kept while its rows close after the
    /// section leaves.
    section: Option<WorktreeSection>,
    /// The group's rows while its Worktree rows are drawn, as they grow in and close. It starts
    /// afresh each time they are disclosed, so disclosure never reads as rows arriving.
    rows: ReorderableStrip<GroupRow>,
    /// The rows that left the group, which draw in their closing places.
    departed: std::collections::HashMap<GroupRow, DepartedRow>,
    /// The group's rows as last painted.
    items: ScrollHandle,
    /// How the current frame presents the group.
    frame: GroupFrame,
    /// Whether the Worktree rows are disclosing in the current frame, or finished in it.
    opening: bool,
}

/// How one frame presents a Workspace group.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GroupFrame {
    disclosing: SidebarFrame,
    disclosed: SidebarFrame,
}

impl GroupFrame {
    /// A group shown at rest with `section`.
    fn at_rest(section: Option<&WorktreeSection>) -> Self {
        let rest = |shown: bool| SidebarFrame {
            reveal: if shown { 1.0 } else { 0.0 },
            moving: false,
        };
        Self {
            disclosing: rest(section.is_some()),
            disclosed: rest(section.is_some_and(|section| section.expanded)),
        }
    }

    fn moving(self) -> bool {
        self.disclosing.moving || self.disclosed.moving
    }

    /// Whether the frame draws the Worktree rows, which it does from the first frame that
    /// discloses them to the last that closes them.
    fn draws_worktrees(self) -> bool {
        self.disclosed.reveal > 0.0 || self.disclosed.moving
    }
}

impl GroupMotion {
    /// A group first presented with `section`, at rest.
    fn new(section: Option<&WorktreeSection>) -> Self {
        Self {
            disclosing: Reveal::new(section.is_some()),
            disclosed: Reveal::new(section.is_some_and(|section| section.expanded)),
            section: section.cloned(),
            rows: ReorderableStrip::new(gpui::Axis::Vertical),
            departed: std::collections::HashMap::new(),
            items: ScrollHandle::new(),
            frame: GroupFrame::at_rest(section),
            opening: false,
        }
    }

    /// Moves toward presenting `section`, from wherever the group was drawn.
    fn present(
        &mut self,
        section: Option<&WorktreeSection>,
        now: std::time::Instant,
        motion: spaceterm_ui::ControlMotion,
    ) {
        let expanded = section.is_some_and(|section| section.expanded);
        let disclosing = self
            .disclosing
            .present_at(section.is_some(), true, now, motion);
        let disclosed = self.disclosed.present_at(expanded, true, now, motion);
        self.opening = expanded && (disclosed.moving || self.frame.disclosed.moving);
        self.frame = GroupFrame {
            disclosing,
            disclosed,
        };
        match section {
            Some(section) if self.section.as_ref() != Some(section) => {
                if let Some(previous) = self.section.replace(section.clone()) {
                    let current = worktree_group_rows(section)
                        .into_iter()
                        .map(|(row, _)| row)
                        .collect::<Vec<_>>();
                    for (row, content) in worktree_group_rows(&previous) {
                        if current.contains(&row) {
                            continue;
                        }
                        let departed = match content {
                            GroupRowContent::Worktree(worktree, _) => {
                                DepartedRow::Worktree(Box::new(worktree.clone()))
                            }
                            GroupRowContent::FormerRepository(name) => {
                                DepartedRow::FormerRepository(name.clone())
                            }
                        };
                        self.departed.insert(row, departed);
                    }
                }
            }
            Some(_) => {}
            None if !self.frame.draws_worktrees() && !disclosing.moving => self.section = None,
            None => {}
        }
    }

    /// The rows the group draws this frame, in order.
    fn row_ids(&self) -> Vec<GroupRow> {
        let mut ids = vec![GroupRow::Workspace];
        if let Some(section) = self
            .section
            .as_ref()
            .filter(|_| self.frame.draws_worktrees())
        {
            ids.extend(worktree_group_rows(section).into_iter().map(|(row, _)| row));
        }
        ids
    }

    /// The rows that left from beside `row` and still draw in its closing gaps, before it and
    /// after it, each with how much of it shows from its top. A place with no row to draw, as a
    /// row that returned leaves, stays empty.
    #[expect(
        clippy::type_complexity,
        reason = "the rows on either side are the same list of departed rows and their extents"
    )]
    fn departed_beside(
        &self,
        row: GroupRow,
    ) -> (
        Vec<(Option<&DepartedRow>, Pixels)>,
        Vec<(Option<&DepartedRow>, Pixels)>,
    ) {
        let (before, after) = self.rows.leaving_beside(row);
        let departed = |rows: &[super::drag_and_drop::Leaving<GroupRow>]| {
            rows.iter()
                .map(|leaving| {
                    let departed = leaving.id.and_then(|id| self.departed.get(&id));
                    (departed, leaving.extent)
                })
                .collect::<Vec<_>>()
        };
        (departed(before), departed(after))
    }
}

/// Each row a Worktree section draws below its Workspace row, in order, with what it shows.
fn worktree_group_rows(section: &WorktreeSection) -> Vec<(GroupRow, GroupRowContent<'_>)> {
    use std::hash::{Hash as _, Hasher as _};
    let mut rows = Vec::new();
    let mut labels = Vec::new();
    for group in &section.groups {
        if let (Some(name), false) = (&group.former_repository, group.rows.is_empty()) {
            let earlier = labels.iter().filter(|label| *label == name).count();
            labels.push(name.clone());
            let mut digest = std::hash::DefaultHasher::new();
            (name.as_ref(), earlier).hash(&mut digest);
            rows.push((
                GroupRow::FormerRepository(digest.finish()),
                GroupRowContent::FormerRepository(name),
            ));
        }
        let count = group.rows.len();
        rows.extend(group.rows.iter().enumerate().map(|(index, row)| {
            (
                GroupRow::Worktree(row.worktree_id),
                GroupRowContent::Worktree(row, (index + 1, count)),
            )
        }));
    }
    rows
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SidebarLayout {
    pub(super) visible: bool,
    pub(super) width: Pixels,
}

/// The value a Workspace drag carries, scoped to the sidebar that lists the Workspace.
struct DraggedWorkspace {
    workspace_id: WorkspaceId,
    owner: EntityId,
}

/// Owns sidebar layout, scrolling, menus, inline rename, reordering, and focus transitions.
pub(super) struct WorkspaceSidebar {
    rows: Vec<WorkspaceRowViewModel>,
    /// The saved paired servers the creation menu offers.
    remote_servers: Vec<RemoteCreationServer>,
    layout: SidebarLayout,
    scroll_handle: ScrollHandle,
    /// Each row's top and bottom within its Workspace group, as last painted. A group is one
    /// scroll child, so revealing a row inside a tall group needs the row's own span.
    row_spans: std::collections::HashMap<SidebarRowKey, (Pixels, Pixels)>,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    focus: FocusHandle,
    /// Whether the keyboard brought focus to the sidebar, so its selection is emphasized. Pointer
    /// paths that focus the sidebar, like the scrollbar and a secondary click, clear it.
    focus_visible: bool,
    menu: Option<SidebarRowKey>,
    /// Whether the sidebar had focus when the open menu was requested, so a rename it starts
    /// returns focus to where it was.
    menu_from_sidebar: bool,
    /// The row the keyboard stands on when it is not the derived selection: a Worktree with no
    /// Tabs yet, or a Workspace whose Active Worktree's row is disclosed.
    cursor: Option<SidebarRowKey>,
    new_workspace_menu_open: bool,
    rename: Option<WorkspaceRenameState>,
    resize_origin: Option<SidebarLayout>,
    suppress_pointer_until_release: bool,
    /// The Workspace groups, whose selection rests on the Workspace row or one of its Worktree
    /// rows.
    workspace_reorder: ReorderableStrip<WorkspaceId, Option<WorktreeId>>,
    /// How each Workspace group moves as its Worktree rows disclose, come, and go.
    groups: std::collections::HashMap<WorkspaceId, GroupMotion>,
    /// The row that carried the selection in the last frame, where its chip was painted along the
    /// list, and how tall the chip was.
    painted_selection: Option<(SidebarRowKey, Pixels, Pixels)>,
    /// The row that carried the selection at the last render.
    selected: Option<SidebarRowKey>,
    /// The row the selection is moving to, and how much taller than that row's chip the chip it
    /// left was, so the moving chip starts at the size it was drawn.
    selection_lengthening: Option<(SidebarRowKey, Pixels)>,
    /// How the sidebar moves as it shows and hides.
    motion: SidebarMotion,
    /// How the current frame presents the sidebar.
    frame: SidebarFrame,
}

impl WorkspaceSidebar {
    pub(super) fn new(visible: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The Workspace Manager draws the sidebar as a cached view.
        super::cached_view::redraw_on_presentation_change(cx);
        let scrollbar = cx.new(|_| {
            OverlayScrollbar::<f32>::new("workspace-scrollbar").accessibility_name("Workspace list")
        });
        cx.subscribe_in(
            &scrollbar,
            window,
            |sidebar, _, event: &OverlayScrollbarEvent<f32>, window, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => {
                    sidebar.focus_visible = false;
                    sidebar.focus.focus(window, cx);
                    cx.emit(SidebarEvent::FocusChanged);
                }
                OverlayScrollbarEvent::OffsetRequested(offset) => {
                    let current = sidebar.scroll_handle.offset();
                    sidebar
                        .scroll_handle
                        .set_offset(point(current.x, px(-*offset)));
                    cx.notify();
                }
            },
        )
        .detach();
        cx.observe_window_bounds(window, |sidebar, window, cx| {
            sidebar.constrain_to_window(window, cx);
        })
        .detach();
        let focus = cx.focus_handle().tab_stop(true);
        cx.on_focus(&focus, window, |_, _, cx| {
            cx.notify();
            cx.emit(SidebarEvent::FocusChanged);
        })
        .detach();
        cx.on_blur(&focus, window, |sidebar: &mut Self, _, cx| {
            // A menu borrows focus and returns it, so the keyboard keeps its row meanwhile.
            if sidebar.menu.is_none() {
                sidebar.cursor = None;
            }
            cx.notify();
            cx.emit(SidebarEvent::FocusChanged);
        })
        .detach();
        Self {
            rows: Vec::new(),
            remote_servers: Vec::new(),
            layout: SidebarLayout {
                visible,
                width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            },
            scroll_handle: ScrollHandle::new(),
            row_spans: std::collections::HashMap::new(),
            scrollbar,
            focus,
            focus_visible: false,
            menu: None,
            menu_from_sidebar: false,
            cursor: None,
            new_workspace_menu_open: false,
            rename: None,
            resize_origin: None,
            suppress_pointer_until_release: false,
            workspace_reorder: ReorderableStrip::new(gpui::Axis::Vertical),
            groups: std::collections::HashMap::new(),
            painted_selection: None,
            selected: None,
            selection_lengthening: None,
            motion: SidebarMotion::new(visible),
            frame: SidebarFrame {
                reveal: if visible { 1.0 } else { 0.0 },
                moving: false,
            },
        }
    }

    /// Presents the sidebar in the current frame, which the Workspace chrome around it follows.
    pub(super) fn present(&mut self, cx: &App) -> SidebarFrame {
        self.frame = self.motion.present(self.layout.visible, cx);
        self.frame
    }

    pub(super) fn dismiss_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_is_focused(window) {
            self.focus.focus(window, cx);
        }
        self.rename = None;
        self.menu = None;
        cx.emit(SidebarEvent::FocusChanged);
        cx.notify();
    }

    fn begin_resize(&mut self, source: ResizeInputSource) {
        self.resize_origin = Some(self.layout);
        if source == ResizeInputSource::Pointer {
            self.suppress_pointer_until_release = false;
        }
    }

    /// Ends a resize when a toggle takes the sidebar's width from it. The handle's remaining events
    /// for that interaction change nothing.
    fn end_resize(&mut self) {
        self.resize_origin = None;
    }

    fn finish_resize(
        &mut self,
        source: ResizeInputSource,
        reason: ResizeFinishReason,
    ) -> (bool, Option<SidebarLayout>) {
        if source == ResizeInputSource::Pointer {
            self.suppress_pointer_until_release = !matches!(
                reason,
                ResizeFinishReason::Completed | ResizeFinishReason::PointerButtonLost
            );
        }
        let origin = self.resize_origin.take();
        (
            origin.is_some(),
            origin.filter(|_| reason == ResizeFinishReason::Escape),
        )
    }

    pub(super) fn rename_is_focused(&self, window: &Window) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|rename| rename.focus_handle.is_focused(window))
    }

    /// Keeps an inline rename open while the window modal about to present borrows its keyboard,
    /// so closing the modal sequence returns typing to the draft. A focused rename, or one an
    /// earlier modal already holds, is held. Returns whether this modal holds it.
    pub(super) fn hold_rename_for_modal(&mut self, window: &Window) -> bool {
        let Some(rename) = self
            .rename
            .as_mut()
            .filter(|rename| rename.modal_holds > 0 || rename.focus_handle.is_focused(window))
        else {
            return false;
        };
        rename.modal_holds += 1;
        true
    }

    /// Ends one modal's hold. Once none holds it, a rename the closed modals did not return the
    /// keyboard to finishes as a blur does.
    pub(super) fn release_rename_hold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.as_mut().filter(|rename| rename.modal_holds > 0) else {
            return;
        };
        rename.modal_holds -= 1;
        if rename.modal_holds > 0 || rename.focus_handle.is_focused(window) {
            return;
        }
        let input_id = rename.input.entity_id();
        let value = rename.input.read(cx).value().to_owned();
        self.finish_rename(input_id, Some(value), false, window, cx);
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<f32>> {
        let track_height_px = f32::from(self.scroll_handle.bounds().size.height);
        let maximum_offset_px = f32::from(self.scroll_handle.max_offset().y);
        let offset_px = -f32::from(self.scroll_handle.offset().y);
        ScrollMetrics::for_pixels(0.0, track_height_px, maximum_offset_px, offset_px)
    }

    fn sync_scrollbar(&self, cx: &mut App) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    pub(super) fn reveal_scrollbar(&self, cx: &mut App) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }
}

impl WorkspaceSidebar {
    fn request_menu(
        &mut self,
        key: SidebarRowKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .visible_keys()
            .iter()
            .any(|(visible, _)| *visible == key)
        {
            return false;
        }
        // Like a source list, a secondary click leaves the selection and focus where they were and
        // outlines the row its menu acts on, so closing the menu or its alert returns typing to
        // the Pane. The keyboard opens the menu only on the selected row of the focused sidebar.
        self.menu_from_sidebar = self.focus.is_focused(window);
        self.rename = None;
        self.menu = Some(key);
        cx.notify();
        true
    }

    fn handle_menu_lifecycle(
        &mut self,
        key: SidebarRowKey,
        event: MenuLifecycleEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            MenuLifecycleEvent::Opened => self.menu = Some(key),
            MenuLifecycleEvent::Closed(_) if self.menu == Some(key) => {
                self.menu = None;
                // The keyboard's row outlives the menu only in the sidebar that kept focus.
                if !self.focus.is_focused(window) {
                    self.cursor = None;
                }
            }
            MenuLifecycleEvent::Closed(_) => return,
        }
        cx.emit(SidebarEvent::FocusChanged);
        cx.notify();
    }

    pub(super) fn handle_new_workspace_menu_lifecycle(
        &mut self,
        event: MenuLifecycleEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            MenuLifecycleEvent::Opened => self.new_workspace_menu_open = true,
            MenuLifecycleEvent::Closed(_) => {
                if !self.new_workspace_menu_open {
                    return;
                }
                self.new_workspace_menu_open = false;
            }
        }
        cx.emit(SidebarEvent::FocusChanged);
        cx.notify();
    }

    fn perform_menu_command(
        &mut self,
        workspace_id: WorkspaceId,
        command: RowMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            RowMenuCommand::Rename => {
                let Some(workspace) = self
                    .rows
                    .iter()
                    .find(|row| row.workspace_id == workspace_id)
                else {
                    cx.emit(SidebarEvent::FocusChanged);
                    return;
                };
                let input = cx.new(|cx| {
                    TextInput::new(
                        "workspace-rename-input",
                        "Workspace name",
                        workspace.name.clone(),
                        window,
                        cx,
                    )
                    .variant(TextInputVariant::Bare)
                    .debug_selector("workspace-rename-input")
                });
                let input_id = input.entity_id();
                cx.subscribe_in(
                    &input,
                    window,
                    move |sidebar, input, event: &TextInputEvent, window, cx| match event {
                        TextInputEvent::Submitted => {
                            let value = input.read(cx).value().to_owned();
                            cx.defer_in(window, move |sidebar, window, cx| {
                                sidebar.finish_rename(input_id, Some(value), true, window, cx);
                            });
                        }
                        TextInputEvent::Cancelled => {
                            cx.defer_in(window, move |sidebar, window, cx| {
                                sidebar.finish_rename(input_id, None, true, window, cx);
                            });
                        }
                        TextInputEvent::FocusLost => {
                            let held = sidebar
                                .rename
                                .as_ref()
                                .filter(|rename| rename.input.entity_id() == input_id)
                                .is_some_and(|rename| {
                                    rename.context_menu_open || rename.modal_holds > 0
                                });
                            if !held {
                                let value = input.read(cx).value().to_owned();
                                cx.defer_in(window, move |sidebar, window, cx| {
                                    sidebar.finish_rename(input_id, Some(value), false, window, cx);
                                });
                            }
                        }
                        TextInputEvent::ContextMenuOpened => {
                            if let Some(rename) = &mut sidebar.rename
                                && rename.input.entity_id() == input_id
                            {
                                rename.context_menu_open = true;
                            }
                        }
                        TextInputEvent::ContextMenuClosed => {
                            let should_finish = sidebar
                                .rename
                                .as_mut()
                                .filter(|rename| rename.input.entity_id() == input_id)
                                .is_some_and(|rename| {
                                    rename.context_menu_open = false;
                                    !rename.focus_handle.is_focused(window)
                                });
                            if should_finish {
                                let value = input.read(cx).value().to_owned();
                                cx.defer_in(window, move |sidebar, window, cx| {
                                    sidebar.finish_rename(input_id, Some(value), false, window, cx);
                                });
                            }
                        }
                        _ => {}
                    },
                )
                .detach();
                self.rename = Some(WorkspaceRenameState {
                    workspace_id,
                    initial_value: input.read(cx).value().to_owned(),
                    // The draft is fenced to the value it opened on, so a rename another client
                    // publishes meanwhile is a conflict rather than something to overwrite.
                    version: workspace.shared.as_ref().map(|shared| shared.name_version),
                    focus_handle: input.read(cx).focus_handle(),
                    input,
                    context_menu_open: false,
                    modal_holds: 0,
                    return_to_sidebar: self.menu_from_sidebar,
                });
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
                cx.defer_in(window, |sidebar, window, cx| {
                    let Some(rename) = &sidebar.rename else {
                        return;
                    };
                    let input = rename.input.clone();
                    let focus_handle = rename.focus_handle.clone();
                    input.update(cx, |input, cx| input.select_all(cx));
                    focus_handle.focus(window, cx);
                    cx.emit(SidebarEvent::FocusChanged);
                });
            }
            RowMenuCommand::Workspace(command) => cx.emit(SidebarEvent::Command {
                workspace_id,
                command,
            }),
            RowMenuCommand::Worktree(worktree_id, command) => {
                cx.emit(SidebarEvent::WorktreeCommand {
                    workspace_id,
                    worktree_id,
                    command,
                });
            }
        }
    }
    fn finish_rename(
        &mut self,
        input_id: EntityId,
        value: Option<String>,
        restore_sidebar_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self.rename.as_ref() else {
            return;
        };
        if rename.input.entity_id() != input_id {
            return;
        }
        let workspace_id = rename.workspace_id;
        let version = rename.version;
        if let Some(value) = value
            && value.trim() != rename.initial_value
        {
            cx.emit(SidebarEvent::Rename {
                workspace_id,
                name: value,
                version,
            });
        }
        let return_to_sidebar = rename.return_to_sidebar;
        self.rename = None;
        if restore_sidebar_focus {
            if return_to_sidebar {
                self.focus.focus(window, cx);
            } else {
                cx.emit(SidebarEvent::FocusPane);
            }
        }
        cx.emit(SidebarEvent::FocusChanged);

        cx.notify();
    }
}
pub(super) fn remote_connection_status(phase: RemoteConnectionPhase) -> Option<&'static str> {
    match phase {
        RemoteConnectionPhase::Connected => None,
        RemoteConnectionPhase::Reconnecting => Some("Reconnecting…"),
        RemoteConnectionPhase::Disconnected => Some("Not connected"),
        RemoteConnectionPhase::Failed => Some("Connection failed"),
    }
}

pub(super) fn remote_connection_color(
    phase: RemoteConnectionPhase,
    colors: &ChromeColors,
) -> Color {
    match phase {
        RemoteConnectionPhase::Reconnecting => colors.info,
        RemoteConnectionPhase::Connected => colors.success,
        RemoteConnectionPhase::Disconnected => colors.icon_muted,
        RemoteConnectionPhase::Failed => colors.error,
    }
}

fn workspace_row_status_paint(
    proposed: Color,
    selected: bool,
    minimum_contrast: f64,
    colors: &ChromeColors,
) -> WorkspaceStatusPaint {
    let (normal_background, hovered_background) = if selected {
        (
            colors.row_selected_background,
            colors.row_selected_hover_background,
        )
    } else {
        (colors.row_background, colors.row_hover_background)
    };
    resolve_workspace_status(
        proposed,
        normal_background,
        hovered_background,
        minimum_contrast,
    )
}

impl WorkspaceSidebar {
    /// Presents `rows` as one list in the order given, whichever servers own their Workspaces.
    /// Shows these rows. Returns whether they changed.
    pub(super) fn set_rows(
        &mut self,
        rows: Vec<WorkspaceRowViewModel>,
        remote_servers: Vec<RemoteCreationServer>,
        cx: &mut Context<Self>,
    ) -> bool {
        // The Workspace Manager sets the rows on every render, so only a change redraws the
        // sidebar.
        if self.rows == rows && self.remote_servers == remote_servers {
            return false;
        }
        self.rows = rows;
        self.remote_servers = remote_servers;
        if self
            .cursor
            .is_some_and(|cursor| !self.visible_keys().iter().any(|(key, _)| *key == cursor))
        {
            self.cursor = None;
        }
        cx.notify();
        true
    }
    fn collapsed_top_chrome_width(&self, window: &Window, cx: &App) -> Pixels {
        let active = self.rows.iter().find(|row| row.active);
        let identity = WorkspaceChromeIdentity {
            name: active.map_or_else(String::new, |row| row.name.to_string()),
            worktree: active
                .and_then(|row| row.worktrees.as_ref())
                .and_then(|section| section.rows().find(|row| row.active))
                .map(|row| row.name.to_string()),
            pinned: active.is_some_and(|row| row.pinned),
            status: active
                .and_then(|row| WorkspaceChromeStatus::resolve(row.remote_connection_phase)),
        };
        WorkspaceChromeLayout::collapsed_width(&identity, window, cx)
    }
}
impl WorkspaceSidebar {
    /// Lifts a Workspace row to move it in the list. A press that becomes a drag never activates
    /// it.
    fn begin_workspace_drag(
        &mut self,
        workspace_id: WorkspaceId,
        cursor_offset: gpui::Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> DragPreview {
        let size = self.row_position(workspace_id).and_then(|position| {
            self.workspace_reorder
                .item_size(&self.scroll_handle, workspace_id, position)
        });
        let session = DragSession::begin(cx, |sidebar: &mut Self, _, cx| {
            let ids = sidebar.row_ids();
            sidebar
                .workspace_reorder
                .cancel(&sidebar.scroll_handle, &ids, cx);
            cx.notify();
        });
        let ids = self.row_ids();
        // The lifted copy shows the Workspace row alone, without its group's Worktree rows.
        let length = self
            .row_spans
            .get(&SidebarRowKey::Workspace(workspace_id))
            .map(|(top, bottom)| *bottom - *top);
        self.workspace_reorder.begin(
            workspace_id,
            length,
            session,
            &self.scroll_handle,
            &ids,
            window,
            cx,
        );
        self.drag_workspace_to(workspace_id, window.mouse_position(), cx);
        cx.notify();
        let sidebar = cx.entity().downgrade();
        let placement = sidebar.clone();
        DragPreview::new(window, cx, move |window, cx| {
            sidebar
                .upgrade()
                .map(|owner| {
                    owner.read(cx).render_lifted_row(
                        workspace_id,
                        size,
                        sidebar.clone(),
                        window,
                        cx,
                    )
                })
                .unwrap_or_else(|| div().into_any_element())
        })
        .placed(cursor_offset, move |cx| {
            placement
                .upgrade()
                .and_then(|owner| owner.read(cx).workspace_reorder.lifted_origin())
        })
    }

    /// The copy of a dragged row that follows the pointer, at the size its row was painted.
    fn render_lifted_row(
        &self,
        workspace_id: WorkspaceId,
        size: Option<gpui::Size<Pixels>>,
        sidebar: WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let Some(row) = self
            .row_position(workspace_id)
            .map(|position| self.rows[position].clone())
        else {
            return div().into_any_element();
        };
        let appearance = super::appearance::chrome(cx);
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let disclosure = view::RowDisclosure::at_rest(row.worktrees.as_ref());
        div()
            .when_some(size, |row, size| row.w(size.width))
            .child(self.render_workspace_row(
                row,
                view::RowRole::Lifted,
                view::RowHover {
                    level: 1.0,
                    tracker: None,
                },
                disclosure,
                sidebar,
                presentation,
                window,
                appearance,
                cx,
            ))
            .into_any_element()
    }

    fn drag_workspace_to(
        &mut self,
        workspace_id: WorkspaceId,
        pointer: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_reorder.dragged() != Some(workspace_id) {
            return;
        }
        let ids = self.row_ids();
        if self
            .workspace_reorder
            .track(&self.scroll_handle, &ids, pointer, cx)
        {
            cx.notify();
        }
    }

    /// Lands the dragged row in the slot marked at the release point and asks the application to
    /// move its Workspace. The next rows the application supplies confirm or replace this order.
    fn finish_workspace_drag(&mut self, pointer: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.workspace_reorder.dragged() else {
            return;
        };
        self.drag_workspace_to(workspace_id, pointer, cx);
        let ids = self.row_ids();
        let landing = self
            .workspace_reorder
            .finish(&self.scroll_handle, &ids, cx)
            .and_then(|(workspace_id, position)| {
                self.row_position(workspace_id)
                    .map(|current| (workspace_id, current, position))
            });
        if let Some((workspace_id, current, position)) = landing {
            self.move_row(workspace_id, current, position, cx);
        }
        cx.notify();
    }

    fn move_row(
        &mut self,
        workspace_id: WorkspaceId,
        current: usize,
        position: usize,
        cx: &mut Context<Self>,
    ) {
        let row = self.rows.remove(current);
        self.rows.insert(position, row);
        cx.emit(SidebarEvent::Move {
            workspace_id,
            position,
        });
        cx.notify();
    }

    fn row_ids(&self) -> Vec<WorkspaceId> {
        self.rows.iter().map(|row| row.workspace_id).collect()
    }

    fn row_position(&self, workspace_id: WorkspaceId) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.workspace_id == workspace_id)
    }
}

impl Render for WorkspaceSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ids = self.row_ids();
        self.workspace_reorder
            .refresh(&self.scroll_handle, &ids, window, cx);
        if let Some(active) = self.rows.iter().find(|row| row.active) {
            self.workspace_reorder.reveal_while_entering(
                active.workspace_id,
                &self.scroll_handle,
                &ids,
            );
        }
        self.present_groups(window, cx);
        let appearance = super::appearance::chrome(cx);
        // A selection that moves starts from the rect its chip was painted at, whatever moved
        // under it since.
        let selected_key = self.selected_key();
        if selected_key != self.selected {
            self.selection_lengthening = None;
            if let (Some(previous), Some(next)) = (self.selected, selected_key)
                && let Some((painted, start, height)) = self.painted_selection
                && painted == previous
            {
                self.workspace_reorder.drew_selection_at(start);
                self.selection_lengthening =
                    Some((next, height - self.chip_height(next, appearance)));
            }
            self.selected = selected_key;
        }
        let selected = selected_key.map(|key| {
            let row = match key {
                SidebarRowKey::Workspace(_) => None,
                SidebarRowKey::Worktree(_, worktree_id) => Some(worktree_id),
            };
            (key.workspace_id(), row, self.row_inset(key, appearance, cx))
        });
        self.workspace_reorder
            .follow_row_selection(selected, window, cx);
        // A selected Worktree row growing in or disclosing stays in view, as a new active
        // Workspace does, once it has been painted.
        if let Some(key @ SidebarRowKey::Worktree(workspace_id, worktree_id)) = self.selected_key()
            && self.row_spans.contains_key(&key)
            && self.groups.get(&workspace_id).is_some_and(|group| {
                group.opening || group.rows.is_entering(GroupRow::Worktree(worktree_id))
            })
        {
            self.reveal_key(key);
        }
        self.sync_scrollbar(cx);
        let hovers = self.row_hovers(window, cx);
        self.render_body(hovers, cx.entity().downgrade(), window, cx)
    }
}

impl WorkspaceSidebar {
    /// Moves each Workspace group toward its rows, and keeps frames coming while any moves. A
    /// group presented for the first time starts at rest.
    fn present_groups(&mut self, window: &mut Window, cx: &App) {
        let now = super::drag_and_drop::now(cx);
        let motion = super::drag_and_drop::presence_motion(cx);
        let rows = &self.rows;
        self.groups
            .retain(|workspace_id, _| rows.iter().any(|row| row.workspace_id == *workspace_id));
        let mut moving = false;
        for row in rows {
            let section = row.worktrees.as_ref();
            let group = self
                .groups
                .entry(row.workspace_id)
                .and_modify(|group| group.present(section, now, motion))
                .or_insert_with(|| GroupMotion::new(section));
            moving |= group.frame.moving();
            if group.frame.draws_worktrees() {
                let ids = group.row_ids();
                group.rows.refresh(&group.items, &ids, window, cx);
                let leaving = group.rows.leaving().collect::<Vec<_>>();
                group.departed.retain(|row, _| leaving.contains(row));
            } else {
                group.rows = ReorderableStrip::new(gpui::Axis::Vertical);
                group.departed.clear();
            }
        }
        if moving {
            window.request_animation_frame();
        }
    }

    pub(super) fn set_layout(
        &mut self,
        visible: bool,
        width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let width = width.clamp(
            px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH),
            Self::maximum_width(window),
        );
        if self.layout == (SidebarLayout { visible, width }) {
            return;
        }
        self.layout = SidebarLayout { visible, width };
        if !visible {
            self.scrollbar
                .update(cx, |scrollbar, cx| scrollbar.reset(cx));
        }
        cx.emit(SidebarEvent::LayoutChanged);
        cx.notify();
    }
    /// The widest sidebar the window leaves room for beside the terminal content.
    fn maximum_width(window: &Window) -> Pixels {
        (spaceterm_ui::content_viewport(window).size.width - px(TERMINAL_CONTENT_MINIMUM_WIDTH))
            .min(px(SIDEBAR_MAXIMUM_WIDTH))
            .max(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH))
    }
    /// The widths a resize may request, including the divider's presented position.
    ///
    /// Every request below the minimum width collapses the sidebar, so the range reaches zero.
    pub(super) fn resize_range(divider_position: Pixels, window: &Window) -> RangeInclusive<f32> {
        0.0..=f32::from(Self::maximum_width(window).max(divider_position))
    }
    fn constrain_to_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.layout.visible {
            self.set_layout(true, self.layout.width, window, cx);
        }
    }
    pub(super) fn resize(
        &mut self,
        requested_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let minimum_width = px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH);
        if requested_width < minimum_width {
            let was_sidebar_focused =
                self.focus.is_focused(window) || self.rename_is_focused(window);
            self.rename = None;
            self.set_layout(false, minimum_width, window, cx);
            if was_sidebar_focused {
                cx.emit(SidebarEvent::FocusPane);
            }
            cx.emit(SidebarEvent::FocusChanged);
            return;
        }

        self.set_layout(true, requested_width, window, cx);
    }

    fn handle_resize_event(
        &mut self,
        event: ResizeHandleEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<f32> {
        let resize_request = matches!(event, ResizeHandleEvent::ResizeRequested { .. });
        match event {
            ResizeHandleEvent::InteractionStarted { source, .. } => {
                self.begin_resize(source);
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
            }
            ResizeHandleEvent::ResizeRequested {
                source,
                requested_value,
                ..
            } if source != ResizeInputSource::Pointer || self.is_resizing() => {
                let reveal_width = self
                    .collapsed_top_chrome_width(window, cx)
                    .max(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH));
                if self.layout.visible || px(requested_value) >= reveal_width {
                    self.resize(px(requested_value), window, cx);
                } else if source == ResizeInputSource::Accessibility && requested_value > 0.0 {
                    // The hidden sidebar publishes zero width, so assistive technology steps up
                    // from zero. Any width above it shows the sidebar at its last width.
                    self.resize(self.layout.width.max(px(requested_value)), window, cx);
                }
            }
            ResizeHandleEvent::ResizeRequested { .. } => {}
            ResizeHandleEvent::ResetRequested { source } => {
                self.resize(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), window, cx);
                if source == ResizeInputSource::Pointer {
                    cx.emit(SidebarEvent::FocusPane);
                }
            }
            ResizeHandleEvent::InteractionFinished { source, reason, .. } => {
                let (finished, restore) = self.finish_resize(source, reason);
                if let Some(origin) = restore {
                    self.set_layout(origin.visible, origin.width, window, cx);
                }
                if !finished {
                    return None;
                }
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
                if source == ResizeInputSource::Pointer {
                    cx.emit(SidebarEvent::FocusPane);
                }
            }
        }
        resize_request.then(|| f32::from(self.layout.width))
    }
}

impl WorkspaceSidebar {
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) || event.keystroke.modifiers.modified() {
            return;
        }
        let keys = self.visible_keys();
        let Some(current) = self.keyboard_position(&keys) else {
            return;
        };
        let key = keys[current].0;
        let section = self
            .rows
            .iter()
            .find(|row| row.workspace_id == key.workspace_id())
            .and_then(|row| row.worktrees.as_ref());
        let next = match event.keystroke.key.as_str() {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(keys.len() - 1),
            "home" => 0,
            "end" => keys.len() - 1,
            "right" => match (key, section) {
                (SidebarRowKey::Workspace(workspace_id), Some(section)) if !section.expanded => {
                    self.cursor = Some(key);
                    self.set_expanded(workspace_id, true, window, cx);
                    return;
                }
                (SidebarRowKey::Workspace(_), Some(_)) => (current + 1).min(keys.len() - 1),
                _ => current,
            },
            "left" => match (key, section) {
                (SidebarRowKey::Workspace(workspace_id), Some(section)) if section.expanded => {
                    self.cursor = Some(key);
                    self.set_expanded(workspace_id, false, window, cx);
                    return;
                }
                (SidebarRowKey::Worktree(workspace_id, _), _) => keys
                    .iter()
                    .position(|(key, _)| *key == SidebarRowKey::Workspace(workspace_id))
                    .unwrap_or(current),
                _ => current,
            },
            "enter" | "space" => {
                if let SidebarRowKey::Worktree(workspace_id, worktree_id) = key {
                    self.cursor = None;
                    cx.emit(SidebarEvent::ActivateWorktree {
                        workspace_id,
                        worktree_id,
                        focus_pane: true,
                    });
                } else if let Some(section) = section {
                    // A git Workspace row is a tree parent, so Return and Space toggle it.
                    self.cursor = Some(key);
                    self.set_expanded(key.workspace_id(), !section.expanded, window, cx);
                    return;
                } else if event.keystroke.key == "enter" {
                    cx.emit(SidebarEvent::FocusPane);
                } else {
                    return;
                }
                window.prevent_default();
                cx.stop_propagation();
                return;
            }
            "escape" => {
                cx.emit(SidebarEvent::FocusPane);
                window.prevent_default();
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        self.focus_visible = true;
        self.land_on(keys[next], next != current, cx);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    /// Moves the keyboard to a row. A Workspace or a Worktree with Tabs becomes active; a
    /// Worktree with no Tabs only holds the keyboard until Return or Space opens it. A Workspace
    /// keeps the keyboard even when its Active Worktree's row is disclosed below it.
    fn land_on(
        &mut self,
        (key, opens): (SidebarRowKey, bool),
        moved: bool,
        cx: &mut Context<Self>,
    ) {
        self.reveal_key(key);
        if !moved {
            return;
        }
        self.cursor = None;
        match key {
            SidebarRowKey::Workspace(workspace_id) => {
                self.cursor = Some(key);
                cx.emit(SidebarEvent::Activate {
                    workspace_id,
                    focus_pane: false,
                });
            }
            SidebarRowKey::Worktree(workspace_id, worktree_id) if opens => {
                cx.emit(SidebarEvent::ActivateWorktree {
                    workspace_id,
                    worktree_id,
                    focus_pane: false,
                });
            }
            SidebarRowKey::Worktree(..) => self.cursor = Some(key),
        }
    }

    fn set_expanded(
        &mut self,
        workspace_id: WorkspaceId,
        expanded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !expanded
            && matches!(self.cursor, Some(SidebarRowKey::Worktree(cursor, _)) if cursor == workspace_id)
        {
            self.cursor = None;
        }
        cx.emit(SidebarEvent::SetWorktreesExpanded {
            workspace_id,
            expanded,
        });
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    /// Every row the keyboard can stand on, in order, and whether landing there opens Tabs.
    fn visible_keys(&self) -> Vec<(SidebarRowKey, bool)> {
        let mut keys = Vec::new();
        for row in &self.rows {
            keys.push((SidebarRowKey::Workspace(row.workspace_id), true));
            let Some(section) = row.worktrees.as_ref().filter(|section| section.expanded) else {
                continue;
            };
            keys.extend(section.rows().map(|worktree| {
                (
                    SidebarRowKey::Worktree(row.workspace_id, worktree.worktree_id),
                    worktree.has_tabs,
                )
            }));
        }
        keys
    }

    /// The row that carries the selection: the keyboard's row, else the Active Worktree's row
    /// when its Workspace is expanded, else the active Workspace's row.
    fn selected_key(&self) -> Option<SidebarRowKey> {
        if self.cursor.is_some() {
            return self.cursor;
        }
        let row = self.rows.iter().find(|row| row.active)?;
        if row.selected() {
            return Some(SidebarRowKey::Workspace(row.workspace_id));
        }
        let worktree = row
            .worktrees
            .as_ref()?
            .rows()
            .find(|worktree| worktree.active)?;
        Some(SidebarRowKey::Worktree(
            row.workspace_id,
            worktree.worktree_id,
        ))
    }

    fn keyboard_position(&self, keys: &[(SidebarRowKey, bool)]) -> Option<usize> {
        let selected = self.selected_key()?;
        keys.iter().position(|(key, _)| *key == selected)
    }
}

impl WorkspaceSidebar {
    pub(super) fn layout(&self) -> SidebarLayout {
        self.layout
    }
    pub(super) fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }
    /// Gives the sidebar the keyboard, as a restored window does when the sidebar held it.
    pub(super) fn take_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.visible {
            self.focus.focus(window, cx);
        }
    }
    /// Whether the sidebar has focus that the keyboard brought to it.
    pub(super) fn has_visible_focus(&self, window: &Window) -> bool {
        self.focus_visible && self.is_focused(window)
    }
    pub(super) fn is_resizing(&self) -> bool {
        self.resize_origin.is_some()
    }
    pub(super) fn is_renaming(&self) -> bool {
        self.rename.is_some()
    }
    pub(super) fn menu_open(&self) -> bool {
        self.menu.is_some() || self.new_workspace_menu_open
    }
    pub(super) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            cx.emit(SidebarEvent::FocusChanged);
            cx.notify();
        }
    }
    pub(super) fn cancel_rename_for(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        if self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.workspace_id == workspace_id)
        {
            self.cancel_rename(cx);
        }
    }
    /// Scrolls the least distance that shows a row whole, or the top of a row taller than the
    /// list. Before the row is painted, the list shows its Workspace group instead.
    fn reveal_key(&self, key: SidebarRowKey) {
        let Some(index) = self.row_position(key.workspace_id()) else {
            return;
        };
        let (Some(group), Some(&(top, bottom))) = (
            self.scroll_handle.bounds_for_item(index),
            self.row_spans.get(&key),
        ) else {
            self.scroll_handle.scroll_to_item(index);
            return;
        };
        let viewport = self.scroll_handle.bounds();
        let mut offset = self.scroll_handle.offset();
        let (top, bottom) = (group.top() + top, group.top() + bottom);
        if top + offset.y < viewport.top() || bottom - top > viewport.size.height {
            offset.y = viewport.top() - top;
        } else if bottom + offset.y > viewport.bottom() {
            offset.y = viewport.bottom() - bottom;
        } else {
            return;
        }
        self.scroll_handle.set_offset(offset);
    }

    /// Records where a Workspace group painted its rows, as its scroll handle tracked them.
    /// `keys` names each child of the group in order; a label has no key.
    fn record_row_spans(&mut self, workspace_id: WorkspaceId, keys: &[Option<SidebarRowKey>]) {
        let children = self
            .groups
            .get(&workspace_id)
            .map_or_else(Vec::new, |group| {
                (0..group.items.children_count())
                    .filter_map(|position| group.items.bounds_for_item(position))
                    .collect::<Vec<_>>()
            });
        self.row_spans
            .retain(|key, _| key.workspace_id() != workspace_id);
        let Some(origin) = children.first().map(|bounds| bounds.top()) else {
            return;
        };
        for (key, bounds) in keys.iter().zip(children) {
            if let Some(key) = key {
                self.row_spans
                    .insert(*key, (bounds.top() - origin, bounds.bottom() - origin));
            }
        }
    }

    pub(super) fn reveal_workspace(&self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        if let Some(index) = self.row_position(workspace_id) {
            self.scroll_handle.scroll_to_item(index);
        }
        cx.notify();
    }
    pub(super) fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore = self.is_focused(window) || self.rename_is_focused(window) || self.menu_open();
        if self.layout.visible && self.menu_open() {
            self.menu.take();
            self.new_workspace_menu_open = false;
            dismiss_active_menu(window, cx);
        }
        self.cancel_rename(cx);
        self.motion.expect_change();
        self.end_resize();
        self.set_layout(!self.layout.visible, self.layout.width, window, cx);
        if !self.layout.visible && restore {
            cx.emit(SidebarEvent::FocusPane);
        }
    }
    pub(super) fn toggle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_focused(window) || self.rename_is_focused(window) {
            self.cancel_rename(cx);
            cx.emit(SidebarEvent::FocusPane);
        } else {
            if !self.layout.visible {
                self.motion.expect_change();
                self.end_resize();
            }
            self.set_layout(true, self.layout.width, window, cx);
            if let Some(active) = self.rows.iter().find(|row| row.active) {
                self.reveal_workspace(active.workspace_id, cx);
            }
            self.cursor = None;
            cx.defer_in(window, |sidebar, window, cx| {
                sidebar.focus_visible = true;
                sidebar.focus.focus(window, cx);
                cx.emit(SidebarEvent::FocusChanged);
                cx.notify();
            });
        }
    }
}

#[cfg(test)]
impl WorkspaceSidebar {
    /// How far a Workspace's Worktree rows are disclosed this frame, which turns its chevron.
    pub(super) fn worktrees_disclosed(&self, workspace_id: WorkspaceId) -> Option<f32> {
        self.groups
            .get(&workspace_id)
            .map(|group| group.frame.disclosed.reveal)
    }

    pub(super) fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll_handle
    }
    pub(super) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }
    pub(super) fn rename_input(&self) -> Option<&Entity<TextInput>> {
        self.rename.as_ref().map(|rename| &rename.input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_text_should_be_readable_on_every_row_background() {
        let colors = ChromeColors::default();
        for (foreground, background) in [
            (colors.row_secondary, colors.row_background),
            (colors.row_hover_secondary, colors.row_hover_background),
            (
                colors.row_selected_secondary,
                colors.row_selected_background,
            ),
            (
                colors.row_selected_hover_secondary,
                colors.row_selected_hover_background,
            ),
        ] {
            assert!(foreground.contrast_ratio(background) >= 4.5);
        }
    }
    #[test]
    fn connection_states_should_use_semantic_colors() {
        assert_eq!(
            remote_connection_color(
                RemoteConnectionPhase::Reconnecting,
                &ChromeColors::default()
            ),
            ChromeColors::default().info
        );
        assert_eq!(
            remote_connection_color(RemoteConnectionPhase::Failed, &ChromeColors::default()),
            ChromeColors::default().error
        );
        assert_eq!(
            remote_connection_color(
                RemoteConnectionPhase::Disconnected,
                &ChromeColors::default()
            ),
            ChromeColors::default().icon_muted
        );
    }

    #[test]
    fn semantic_status_paints_should_remain_readable_in_every_row_state() {
        for colors in [
            crate::appearance::builtin_chrome_base(crate::appearance::Appearance::Dark),
            crate::appearance::builtin_chrome_base(crate::appearance::Appearance::Light),
        ] {
            for selected in [false, true] {
                let (normal_background, hovered_background) = if selected {
                    (
                        colors.row_selected_background,
                        colors.row_selected_hover_background,
                    )
                } else {
                    (colors.row_background, colors.row_hover_background)
                };
                for semantic in [colors.info, colors.success, colors.warning, colors.error] {
                    let paint = workspace_row_status_paint(semantic, selected, 4.5, &colors);
                    assert!(paint.normal.contrast_ratio(normal_background) >= 4.5);
                    assert!(paint.hovered.contrast_ratio(hovered_background) >= 4.5);
                }
            }
        }
    }

    #[test]
    fn remote_status_paints_should_preserve_distinct_semantic_states() {
        let colors = ChromeColors::default();
        for selected in [false, true] {
            let reconnecting = workspace_row_status_paint(colors.info, selected, 4.5, &colors);
            let failed = workspace_row_status_paint(colors.error, selected, 4.5, &colors);
            assert_ne!(reconnecting, failed);
        }
    }
}
