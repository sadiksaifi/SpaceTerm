//! The Client View State a closed Workspace Window leaves while this client keeps running, and
//! the Settings Window's last placement and Settings Section.
//!
//! Closing the Workspace Window remembers its view here, and the next Workspace Window takes it
//! and presents the same view. Exiting the process drops it, so a launch opens from defaults.
//! Nothing here reaches a server, the Settings Document or disk.
//!
//! Each server Pane's Scrollback position, Selection and cached rows stay with that Pane's
//! client-lifetime record in the terminal layer, which reattaches them as it does after a
//! reconnect. This module keeps the rest.
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gpui::{App, Bounds, DisplayId, Pixels, Size, Window, WindowBounds, WindowOptions, px};
use spaceterm_server::hierarchy::Id;
use spaceterm_server::workspace_order::{OwnerId, WorkspaceReference};

use super::settings_window::SettingsSectionId;

#[derive(Default)]
struct Remembered {
    workspace_window: Option<WindowView>,
    settings_window: Option<SettingsWindowView>,
}
impl gpui::Global for Remembered {}

/// Remembers the view of the Workspace Window that is closing, for the next one.
pub(crate) fn remember_workspace_window(view: WindowView, cx: &mut App) {
    cx.default_global::<Remembered>().workspace_window = Some(view);
}

/// The view the last closed Workspace Window left, once. The first window after launch has none.
pub(crate) fn take_workspace_window(cx: &mut App) -> Option<WindowView> {
    cx.default_global::<Remembered>().workspace_window.take()
}

/// Remembers where the closing Settings Window was and the Settings Section it showed.
pub(crate) fn remember_settings_window(section: SettingsSectionId, window: &Window, cx: &mut App) {
    let placement = WindowPlacement::of(window, cx);
    cx.default_global::<Remembered>().settings_window = Some(SettingsWindowView {
        placement,
        section,
    });
}

/// Where the last closed Settings Window was, and its Settings Section.
pub(crate) fn settings_window(cx: &App) -> Option<SettingsWindowView> {
    cx.try_global::<Remembered>()?.settings_window.clone()
}

/// Drops everything remembered, as a real quit does.
pub(crate) fn forget(cx: &mut App) {
    cx.set_global(Remembered::default());
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsWindowView {
    pub(crate) placement: WindowPlacement,
    pub(crate) section: SettingsSectionId,
}

/// An Operating-System Window's size, position, display, and windowed, filled or fullscreen
/// state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowPlacement {
    /// The window's state, with the bounds it returns to when it leaves it.
    bounds: WindowBounds,
    /// The display's persistent identity, when it reports one.
    display: Option<[u8; 16]>,
    /// The window system's own record of the window, where it keeps one. On macOS it holds a
    /// filled or tiled state and the frame the window returns to from it, which the bounds alone
    /// report as an ordinary window. It stays in memory like the rest of this view.
    native: Option<Arc<[u8]>>,
}

/// What a window opened with a [`WindowPlacement`] still restores once it exists.
#[must_use]
pub(crate) struct OpenedPlacement {
    native: Option<Arc<[u8]>>,
}

impl OpenedPlacement {
    /// Restores the window system's own record of the window, such as a filled state.
    pub(crate) fn finish(self, window: &Window) {
        if let Some(native) = self.native {
            window.restore_native_window_state(&native);
        }
    }
}

impl WindowPlacement {
    pub(crate) fn of(window: &Window, cx: &App) -> Self {
        Self {
            // Client-drawn insets are the window's own decoration, which opening adds again.
            bounds: window.inner_window_bounds(),
            display: window
                .display(cx)
                .and_then(|display| display.uuid().ok())
                .map(|uuid| *uuid.as_bytes()),
            native: window.native_window_state().map(Arc::from),
        }
    }

    /// Opens a window with this placement on a connected display, inside that display's visible
    /// area and at least `options.window_min_size`. Finish the returned placement on the new
    /// window.
    pub(crate) fn apply(&self, options: &mut WindowOptions, cx: &App) -> OpenedPlacement {
        let primary = cx.primary_display().map(|display| display.id());
        let mut displays: Vec<_> = cx
            .displays()
            .iter()
            .map(|display| DisplayArea {
                id: display.id(),
                identity: display.uuid().ok().map(|uuid| *uuid.as_bytes()),
                visible: display.visible_bounds(),
            })
            .collect();
        // The primary display stands in for one that is gone.
        if let Some(index) = displays
            .iter()
            .position(|display| Some(display.id) == primary)
        {
            displays.swap(0, index);
        }
        // Wayland compositors place windows themselves.
        let positions = cx.compositor_name() != "Wayland";
        let minimum = options
            .window_min_size
            .unwrap_or_else(|| gpui::size(px(0.0), px(0.0)));
        let Some(placed) = place(self, &displays, positions, minimum) else {
            return OpenedPlacement { native: None };
        };
        options.window_bounds = Some(placed.bounds);
        options.display_id = placed.display;
        self.opened(&placed)
    }

    /// What a window `placed` restores once it exists. The window system's record names a
    /// display, which must still be connected.
    fn opened(&self, placed: &Placed) -> OpenedPlacement {
        OpenedPlacement {
            native: placed
                .on_remembered_display
                .then(|| self.native.clone())
                .flatten(),
        }
    }
}

/// Where [`place`] opens a window.
#[derive(Debug, PartialEq)]
struct Placed {
    bounds: WindowBounds,
    display: Option<DisplayId>,
    /// Whether the window returns to the display it left.
    on_remembered_display: bool,
}

/// One connected display, as placement reads it.
#[derive(Clone, Copy, Debug)]
struct DisplayArea {
    id: DisplayId,
    identity: Option<[u8; 16]>,
    visible: Bounds<Pixels>,
}

/// Where a window with `placement` opens among `displays`, the primary display first. A window
/// whose display is gone is centered on the primary display. Without `positions`, only its size
/// and state return, on the display the window system chooses.
fn place(
    placement: &WindowPlacement,
    displays: &[DisplayArea],
    positions: bool,
    minimum: Size<Pixels>,
) -> Option<Placed> {
    let remembered = displays.iter().find(|display| {
        placement.display.is_some() && display.identity == placement.display
    });
    let display = remembered.or_else(|| displays.first())?;
    let area = display.visible;
    let restore = placement.bounds.get_bounds();
    let size = gpui::size(
        clamp(restore.size.width, minimum.width, area.size.width),
        clamp(restore.size.height, minimum.height, area.size.height),
    );
    let origin = if remembered.is_some() {
        gpui::point(
            clamp(
                restore.origin.x,
                area.origin.x,
                area.origin.x + area.size.width - size.width,
            ),
            clamp(
                restore.origin.y,
                area.origin.y,
                area.origin.y + area.size.height - size.height,
            ),
        )
    } else {
        gpui::point(
            area.origin.x + (area.size.width - size.width) / 2.0,
            area.origin.y + (area.size.height - size.height) / 2.0,
        )
    };
    let bounds = Bounds { origin, size };
    let bounds = match placement.bounds {
        WindowBounds::Windowed(_) => WindowBounds::Windowed(bounds),
        WindowBounds::Maximized(_) => WindowBounds::Maximized(bounds),
        WindowBounds::Fullscreen(_) => WindowBounds::Fullscreen(bounds),
    };
    Some(Placed {
        bounds,
        display: positions.then_some(display.id),
        on_remembered_display: positions && remembered.is_some(),
    })
}

/// `value` within `low..=high`, preferring `low` when the range is empty, so a minimum size wins
/// over a smaller display.
fn clamp(value: Pixels, low: Pixels, high: Pixels) -> Pixels {
    value.min(high).max(low)
}

/// The sidebar as the window left it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SidebarView {
    pub(crate) visible: bool,
    pub(crate) width: Pixels,
    /// Whether the sidebar held the keyboard.
    pub(crate) focused: bool,
    /// Whether the keyboard brought it there, which emphasizes the selected row.
    pub(crate) focus_visible: bool,
}

/// One closed Workspace Window's Client View State, by server identity, because the next window
/// numbers its Workspaces, Tabs and Panes anew.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowView {
    pub(crate) placement: Option<WindowPlacement>,
    pub(crate) sidebar: SidebarView,
    pub(crate) active_workspace: Option<WorkspaceReference>,
    pub(crate) workspaces: BTreeMap<WorkspaceReference, RememberedWorkspace>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RememberedWorkspace {
    pub(crate) active_tab: Option<Id>,
    pub(crate) tabs: HashMap<Id, RememberedTab>,
    /// The person's choice for the Workspace's Worktree group, or `None` for the default.
    pub(crate) worktrees_expanded: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RememberedTab {
    /// The Pane focus history, oldest first.
    pub(crate) history: Vec<Id>,
    pub(crate) focused: Id,
    pub(crate) zoomed: bool,
    pub(crate) panes: HashMap<Id, RememberedPane>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RememberedPane {
    /// The font-size zoom, in points from the Settings size.
    pub(crate) font_zoom: f32,
    /// The Terminal Find text while Find is open.
    pub(crate) find: Option<String>,
}

/// What a window takes from a remembered view as its Workspaces arrive.
///
/// Every remembered choice yields to the person: once navigation moves from where the window
/// last left it, nothing remembered changes the view again.
pub(crate) struct Restoration<M> {
    /// Whether the window opened from a remembered view, which it presents passively.
    remembered: bool,
    workspaces: BTreeMap<WorkspaceReference, RememberedWorkspace>,
    /// Views presented from a retained copy, which apply again once their server publishes its
    /// own membership, so a Tab or Pane removed meanwhile falls back to the Root Tab or Pane.
    held: BTreeMap<WorkspaceReference, RememberedWorkspace>,
    selection: Selection,
    /// Where navigation stood when the window last changed its own presentation, until the
    /// person navigates.
    navigation: Option<M>,
}

impl<M: Copy + Eq> Restoration<M> {
    /// Restores `view` from `navigation`, where the person's navigation stands in the new window.
    pub(crate) fn new(view: WindowView, navigation: M) -> Self {
        Self {
            remembered: true,
            workspaces: view.workspaces,
            held: BTreeMap::new(),
            selection: view
                .active_workspace
                .map_or(Selection::Settled, Selection::Waiting),
            navigation: Some(navigation),
        }
    }

    /// A window with nothing to restore.
    pub(crate) fn none() -> Self {
        Self {
            remembered: false,
            workspaces: BTreeMap::new(),
            held: BTreeMap::new(),
            selection: Selection::Settled,
            navigation: None,
        }
    }

    /// Whether the window opened from a remembered view. Such a window never opens a Workspace
    /// for a person on its own, as a new client's first view does.
    pub(crate) const fn remembered(&self) -> bool {
        self.remembered
    }

    /// The remembered view of a Workspace the window presents for the first time, once.
    pub(crate) fn take_workspace(
        &mut self,
        workspace: &WorkspaceReference,
    ) -> Option<RememberedWorkspace> {
        self.workspaces.remove(workspace)
    }

    /// Carries what this window never presented into `view`, the view it leaves as it closes:
    /// remembered Workspaces that never arrived, and the remembered Active Workspace while it is
    /// still awaited. A window closed before its server returned leaves the next one the same
    /// view.
    pub(crate) fn carry_into(&self, view: &mut WindowView) {
        for (workspace, remembered) in &self.workspaces {
            view.workspaces
                .entry(*workspace)
                .or_insert_with(|| remembered.clone());
        }
        if let Selection::Waiting(target) = &self.selection {
            view.active_workspace = Some(*target);
        }
    }

    /// Keeps the view of a Workspace a retained copy presented, until its server publishes.
    pub(crate) fn hold(&mut self, workspace: WorkspaceReference, view: RememberedWorkspace) {
        if self.navigation.is_some() {
            self.held.insert(workspace, view);
        }
    }

    /// The held views of `owner`'s Workspaces, once its server published its own membership.
    pub(crate) fn take_held(&mut self, owner: OwnerId) -> Vec<(WorkspaceReference, RememberedWorkspace)> {
        let owned: Vec<_> = self
            .held
            .keys()
            .filter(|workspace| workspace.owner == owner)
            .copied()
            .collect();
        owned
            .into_iter()
            .filter_map(|workspace| Some((workspace, self.held.remove(&workspace)?)))
            .collect()
    }

    /// Settles everything when the person navigated since navigation was last recorded. Call
    /// before the window changes its own presentation.
    pub(crate) fn observe(&mut self, navigation: M) {
        if self.navigation.is_some_and(|recorded| recorded != navigation) {
            self.navigation = None;
            self.selection = Selection::Settled;
            self.held.clear();
        }
    }

    /// Records where navigation stands after the window changed its own presentation.
    pub(crate) fn record(&mut self, navigation: M) {
        if self.navigation.is_some() {
            self.navigation = Some(navigation);
        }
    }

    /// Whether the window should select `workspace`, which it just presented. The remembered
    /// Active Workspace returns while the person has selected nothing since the window opened.
    pub(crate) fn selects(&self, workspace: &WorkspaceReference) -> bool {
        matches!(&self.selection, Selection::Waiting(target) if target == workspace)
    }

    /// Settles the Active Workspace once `owner`'s server published its own inventory, which
    /// `presents` reads. When the window lacks the remembered Workspace after it, the window's
    /// default comes next: see [`Self::take_fallback`]. A retained copy standing in for an
    /// offline server is not its server's inventory.
    pub(crate) fn published(
        &mut self,
        owner: OwnerId,
        presents: impl FnOnce(&WorkspaceReference) -> bool,
    ) {
        let Selection::Waiting(target) = &self.selection else {
            return;
        };
        if target.owner != owner {
            return;
        }
        self.selection = if presents(target) {
            Selection::Settled
        } else {
            Selection::Fallback
        };
    }

    /// Whether the window selects its default Workspace now, because the remembered one is gone
    /// and the person has selected nothing since. The window asks once its Workspace order is
    /// ready, so the default is the first Workspace in the order the person keeps.
    pub(crate) fn take_fallback(&mut self) -> bool {
        if matches!(self.selection, Selection::Fallback) {
            self.selection = Selection::Settled;
            return true;
        }
        false
    }
}

enum Selection {
    /// The remembered Active Workspace returns when its server presents it.
    Waiting(WorkspaceReference),
    /// The remembered Active Workspace is gone, so the window selects its default.
    Fallback,
    Settled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn reference(owner: u8, workspace: u8) -> WorkspaceReference {
        WorkspaceReference {
            owner: OwnerId::from_bytes([owner; 32]),
            workspace: [workspace; 32],
        }
    }

    fn owner(owner: u8) -> OwnerId {
        OwnerId::from_bytes([owner; 32])
    }

    fn view(active: Option<WorkspaceReference>) -> WindowView {
        WindowView {
            placement: None,
            sidebar: SidebarView {
                visible: true,
                width: px(240.0),
                focused: false,
                focus_visible: false,
            },
            active_workspace: active,
            workspaces: BTreeMap::from([(
                reference(1, 1),
                RememberedWorkspace {
                    active_tab: Some([2; 32]),
                    tabs: HashMap::new(),
                    worktrees_expanded: Some(false),
                },
            )]),
        }
    }

    fn display(id: u64, identity: u8, origin: (f32, f32), extent: (f32, f32)) -> DisplayArea {
        DisplayArea {
            id: DisplayId::new(id),
            identity: Some([identity; 16]),
            visible: Bounds {
                origin: point(px(origin.0), px(origin.1)),
                size: size(px(extent.0), px(extent.1)),
            },
        }
    }

    fn windowed(origin: (f32, f32), extent: (f32, f32)) -> WindowBounds {
        WindowBounds::Windowed(Bounds {
            origin: point(px(origin.0), px(origin.1)),
            size: size(px(extent.0), px(extent.1)),
        })
    }

    fn placement(bounds: WindowBounds, display: u8) -> WindowPlacement {
        WindowPlacement {
            bounds,
            display: Some([display; 16]),
            native: None,
        }
    }

    fn placed(bounds: WindowBounds, display: u64, remembered: bool) -> Option<Placed> {
        Some(Placed {
            bounds,
            display: Some(DisplayId::new(display)),
            on_remembered_display: remembered,
        })
    }

    const MINIMUM: Size<Pixels> = Size {
        width: px(480.0),
        height: px(260.0),
    };

    #[gpui::test]
    fn a_closed_window_view_returns_once_and_a_real_quit_drops_it(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(take_workspace_window(cx), None);
            remember_workspace_window(view(None), cx);
            assert_eq!(take_workspace_window(cx), Some(view(None)));
            assert_eq!(take_workspace_window(cx), None);

            remember_workspace_window(view(None), cx);
            forget(cx);
            assert_eq!(take_workspace_window(cx), None);
            assert_eq!(settings_window(cx), None);
        });
    }

    #[test]
    fn a_window_returns_where_it_was_on_its_display() {
        let displays = [
            display(1, 1, (0.0, 25.0), (1440.0, 875.0)),
            display(2, 2, (1440.0, 0.0), (2560.0, 1440.0)),
        ];
        let bounds = windowed((1700.0, 120.0), (1000.0, 700.0));
        assert_eq!(
            place(&placement(bounds, 2), &displays, true, MINIMUM),
            placed(bounds, 2, true)
        );
    }

    #[test]
    fn filled_and_fullscreen_windows_return_filled_and_fullscreen() {
        let displays = [display(1, 1, (0.0, 0.0), (1440.0, 900.0))];
        let restore = Bounds {
            origin: point(px(100.0), px(80.0)),
            size: size(px(900.0), px(580.0)),
        };
        for bounds in [
            WindowBounds::Maximized(restore),
            WindowBounds::Fullscreen(restore),
        ] {
            assert_eq!(
                place(&placement(bounds, 1), &displays, true, MINIMUM),
                placed(bounds, 1, true)
            );
        }
    }

    #[test]
    fn the_window_systems_record_returns_only_on_its_own_display() {
        let native: Arc<[u8]> = Arc::from(&b"filled, from 900x580"[..]);
        let remembered = WindowPlacement {
            native: Some(Arc::clone(&native)),
            ..placement(windowed((0.0, 25.0), (1440.0, 875.0)), 1)
        };
        let finish = |displays: &[DisplayArea], positions: bool| {
            let placed = place(&remembered, displays, positions, MINIMUM).unwrap();
            remembered.opened(&placed).native
        };
        // macOS reports a filled window as ordinary bounds; its record restores the fill and
        // the frame the window returns to from it.
        assert_eq!(
            finish(&[display(1, 1, (0.0, 25.0), (1440.0, 875.0))], true),
            Some(native)
        );
        // The record would place the window on a display that is gone.
        assert_eq!(
            finish(&[display(2, 2, (0.0, 25.0), (1440.0, 875.0))], true),
            None
        );
        assert_eq!(
            finish(&[display(1, 1, (0.0, 25.0), (1440.0, 875.0))], false),
            None
        );
    }

    #[test]
    fn a_window_whose_display_is_gone_opens_centered_on_the_primary_display() {
        let displays = [display(1, 1, (0.0, 25.0), (1440.0, 875.0))];
        assert_eq!(
            place(
                &placement(windowed((3000.0, 400.0), (1000.0, 700.0)), 9),
                &displays,
                true,
                MINIMUM
            ),
            placed(windowed((220.0, 112.5), (1000.0, 700.0)), 1, false)
        );
    }

    #[test]
    fn a_window_stays_inside_its_displays_visible_area_at_its_minimum_size() {
        let displays = [display(1, 1, (0.0, 25.0), (1280.0, 775.0))];
        assert_eq!(
            place(
                &placement(windowed((-200.0, 0.0), (2000.0, 1200.0)), 1),
                &displays,
                true,
                MINIMUM
            ),
            placed(windowed((0.0, 25.0), (1280.0, 775.0)), 1, true)
        );
        assert_eq!(
            place(
                &placement(windowed((1200.0, 760.0), (300.0, 100.0)), 1),
                &displays,
                true,
                MINIMUM
            ),
            placed(windowed((800.0, 540.0), (480.0, 260.0)), 1, true)
        );
    }

    #[test]
    fn without_window_positions_only_size_and_state_return() {
        let displays = [display(1, 1, (0.0, 0.0), (1920.0, 1080.0))];
        let restore = Bounds {
            origin: point(px(500.0), px(300.0)),
            size: size(px(1000.0), px(700.0)),
        };
        let placed = place(
            &placement(WindowBounds::Maximized(restore), 1),
            &displays,
            false,
            MINIMUM,
        )
        .unwrap();
        assert_eq!(placed.display, None);
        assert!(!placed.on_remembered_display);
        assert!(matches!(placed.bounds, WindowBounds::Maximized(_)));
        assert_eq!(placed.bounds.get_bounds().size, restore.size);
    }

    #[test]
    fn a_window_without_displays_keeps_its_default_placement() {
        let placement = WindowPlacement {
            bounds: windowed((0.0, 0.0), (900.0, 580.0)),
            display: None,
            native: None,
        };
        assert_eq!(place(&placement, &[], true, MINIMUM), None);
    }

    #[test]
    fn each_workspace_view_returns_once() {
        let mut restoration = Restoration::new(view(None), 0);
        assert_eq!(
            restoration
                .take_workspace(&reference(1, 1))
                .and_then(|workspace| workspace.worktrees_expanded),
            Some(false)
        );
        assert_eq!(restoration.take_workspace(&reference(1, 1)), None);
        assert_eq!(restoration.take_workspace(&reference(1, 2)), None);
    }

    #[test]
    fn the_remembered_active_workspace_returns_when_its_server_presents_it() {
        let mut restoration = Restoration::new(view(Some(reference(1, 1))), 0);
        assert!(restoration.remembered());
        assert!(!restoration.selects(&reference(1, 2)));
        assert!(restoration.selects(&reference(1, 1)));
        restoration.published(owner(1), |_| true);
        assert!(!restoration.take_fallback());
        // Once settled, a later presentation never moves the selection.
        assert!(!restoration.selects(&reference(1, 1)));
    }

    #[test]
    fn a_killed_or_detached_active_workspace_falls_back_to_the_default_once() {
        let mut restoration = Restoration::new(view(Some(reference(1, 1))), 0);
        // Another server's inventory says nothing about it.
        restoration.published(owner(2), |_| false);
        assert!(!restoration.take_fallback());
        restoration.published(owner(1), |_| false);
        assert!(!restoration.selects(&reference(1, 1)));
        assert!(restoration.take_fallback());
        assert!(!restoration.take_fallback());
    }

    #[test]
    fn a_fallback_waiting_for_the_workspace_order_yields_to_navigation() {
        let mut restoration = Restoration::new(view(Some(reference(1, 1))), 0);
        restoration.published(owner(1), |_| false);
        // The window's own presentation changes nothing while the order is read.
        restoration.record(1);
        restoration.observe(1);
        assert!(restoration.take_fallback());

        let mut restoration = Restoration::new(view(Some(reference(1, 1))), 0);
        restoration.published(owner(1), |_| false);
        restoration.observe(2);
        assert!(!restoration.take_fallback());
    }

    #[test]
    fn an_offline_servers_workspace_returns_when_the_server_does() {
        let mut restoration = Restoration::new(view(Some(reference(2, 1))), 0);
        // The local server and the window's own presentation change nothing.
        restoration.observe(0);
        restoration.published(owner(1), |_| false);
        restoration.record(1);
        restoration.observe(1);
        assert!(!restoration.take_fallback());
        assert!(restoration.selects(&reference(2, 1)));
    }

    #[test]
    fn selecting_anything_after_reopening_keeps_the_persons_choice() {
        let mut restoration = Restoration::new(view(Some(reference(2, 1))), 0);
        restoration.hold(reference(2, 1), RememberedWorkspace::default());
        restoration.record(1);
        restoration.observe(2);
        assert!(!restoration.selects(&reference(2, 1)));
        restoration.published(owner(2), |_| false);
        assert!(!restoration.take_fallback());
        assert_eq!(restoration.take_held(owner(2)), Vec::new());
    }

    #[test]
    fn a_view_a_retained_copy_presented_applies_again_when_its_server_publishes() {
        let mut restoration = Restoration::new(view(None), 0);
        let remembered = restoration
            .take_workspace(&reference(1, 1))
            .expect("the view is remembered");
        restoration.hold(reference(1, 1), remembered.clone());
        assert_eq!(restoration.take_held(owner(2)), Vec::new());
        assert_eq!(
            restoration.take_held(owner(1)),
            vec![(reference(1, 1), remembered)]
        );
        assert_eq!(restoration.take_held(owner(1)), Vec::new());
    }

    #[test]
    fn a_window_closed_before_its_view_arrived_carries_it_into_the_next() {
        let mut restoration = Restoration::new(view(Some(reference(1, 1))), 0);
        let mut next = view(None);
        next.workspaces.clear();
        restoration.carry_into(&mut next);
        assert_eq!(next.active_workspace, Some(reference(1, 1)));
        assert!(next.workspaces.contains_key(&reference(1, 1)));

        // Once presented or chosen against, nothing is carried.
        let _ = restoration.take_workspace(&reference(1, 1));
        restoration.observe(1);
        let mut later = view(None);
        later.workspaces.clear();
        restoration.carry_into(&mut later);
        assert_eq!(later.active_workspace, None);
        assert!(later.workspaces.is_empty());
    }

    #[test]
    fn a_window_without_a_remembered_view_restores_nothing() {
        let mut restoration = Restoration::<u8>::none();
        assert!(!restoration.remembered());
        assert!(!restoration.selects(&reference(1, 1)));
        restoration.published(owner(1), |_| false);
        assert!(!restoration.take_fallback());
        restoration.hold(reference(1, 1), RememberedWorkspace::default());
        assert_eq!(restoration.take_held(owner(1)), Vec::new());
        assert_eq!(restoration.take_workspace(&reference(1, 1)), None);
    }
}
