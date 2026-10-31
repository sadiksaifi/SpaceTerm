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
    cx.try_global::<Remembered>()?.settings_window
}

/// Drops everything remembered, as a real quit does.
pub(crate) fn forget(cx: &mut App) {
    cx.set_global(Remembered::default());
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SettingsWindowView {
    pub(crate) placement: WindowPlacement,
    pub(crate) section: SettingsSectionId,
}

/// An Operating-System Window's size, position, display, and windowed, filled or fullscreen
/// state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowPlacement {
    /// The window's state, with the bounds it returns to when it leaves it.
    bounds: WindowBounds,
    /// The display's persistent identity, when it reports one.
    display: Option<[u8; 16]>,
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
        }
    }

    /// Opens a window with this placement on a connected display, inside that display's visible
    /// area and at least `options.window_min_size`.
    pub(crate) fn apply(&self, options: &mut WindowOptions, cx: &App) {
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
        if let Some((bounds, display)) = place(*self, &displays, positions, minimum) {
            options.window_bounds = Some(bounds);
            options.display_id = display;
        }
    }
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
    placement: WindowPlacement,
    displays: &[DisplayArea],
    positions: bool,
    minimum: Size<Pixels>,
) -> Option<(WindowBounds, Option<DisplayId>)> {
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
    Some((bounds, positions.then_some(display.id)))
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
pub(crate) struct Restoration<M> {
    workspaces: BTreeMap<WorkspaceReference, RememberedWorkspace>,
    selection: Selection<M>,
}

impl<M: Copy + Eq> Restoration<M> {
    /// Restores `view` from `navigation`, where the person's navigation stands in the new window.
    pub(crate) fn new(view: WindowView, navigation: M) -> Self {
        Self {
            workspaces: view.workspaces,
            selection: view
                .active_workspace
                .map_or(Selection::Settled, |target| Selection::Waiting {
                    target,
                    navigation,
                }),
        }
    }

    /// A window with nothing to restore.
    pub(crate) fn none() -> Self {
        Self {
            workspaces: BTreeMap::new(),
            selection: Selection::Settled,
        }
    }

    /// The remembered view of a Workspace the window presents for the first time, once.
    pub(crate) fn take_workspace(
        &mut self,
        workspace: &WorkspaceReference,
    ) -> Option<RememberedWorkspace> {
        self.workspaces.remove(workspace)
    }

    /// Settles the Active Workspace when the person navigated since `navigation` was recorded.
    /// Call before the window changes its own presentation.
    pub(crate) fn observe(&mut self, navigation: M) {
        if let Selection::Waiting {
            navigation: recorded,
            ..
        } = &self.selection
            && *recorded != navigation
        {
            self.selection = Selection::Settled;
        }
    }

    /// Records where navigation stands after the window changed its own presentation.
    pub(crate) fn record(&mut self, navigation: M) {
        if let Selection::Waiting {
            navigation: recorded,
            ..
        } = &mut self.selection
        {
            *recorded = navigation;
        }
    }

    /// Whether the window should select `workspace`, which it just presented. The remembered
    /// Active Workspace returns while the person has selected nothing since the window opened.
    pub(crate) fn selects(&self, workspace: &WorkspaceReference) -> bool {
        matches!(&self.selection, Selection::Waiting { target, .. } if target == workspace)
    }

    /// Settles the Active Workspace once `owner`'s server published its own inventory, which
    /// `presents` reads. Returns whether the window lacks the remembered Workspace after it, so
    /// the window selects its default. A retained copy standing in for an offline server is not
    /// its server's inventory.
    pub(crate) fn published(
        &mut self,
        owner: OwnerId,
        presents: impl FnOnce(&WorkspaceReference) -> bool,
    ) -> bool {
        let Selection::Waiting { target, .. } = &self.selection else {
            return false;
        };
        if target.owner != owner {
            return false;
        }
        let missing = !presents(target);
        self.selection = Selection::Settled;
        missing
    }
}

enum Selection<M> {
    /// The remembered Active Workspace returns when its server presents it, unless the person
    /// navigates first.
    Waiting {
        target: WorkspaceReference,
        navigation: M,
    },
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

    fn view(active: Option<WorkspaceReference>) -> WindowView {
        WindowView {
            placement: None,
            sidebar: SidebarView {
                visible: true,
                width: px(240.0),
                focused: false,
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
        let placement = WindowPlacement {
            bounds: windowed((1700.0, 120.0), (1000.0, 700.0)),
            display: Some([2; 16]),
        };
        let displays = [
            display(1, 1, (0.0, 25.0), (1440.0, 875.0)),
            display(2, 2, (1440.0, 0.0), (2560.0, 1440.0)),
        ];
        assert_eq!(
            place(placement, &displays, true, MINIMUM),
            Some((
                windowed((1700.0, 120.0), (1000.0, 700.0)),
                Some(DisplayId::new(2))
            ))
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
            let placement = WindowPlacement {
                bounds,
                display: Some([1; 16]),
            };
            assert_eq!(
                place(placement, &displays, true, MINIMUM),
                Some((bounds, Some(DisplayId::new(1))))
            );
        }
    }

    #[test]
    fn a_window_whose_display_is_gone_opens_centered_on_the_primary_display() {
        let placement = WindowPlacement {
            bounds: windowed((3000.0, 400.0), (1000.0, 700.0)),
            display: Some([9; 16]),
        };
        let displays = [display(1, 1, (0.0, 25.0), (1440.0, 875.0))];
        assert_eq!(
            place(placement, &displays, true, MINIMUM),
            Some((
                windowed((220.0, 112.5), (1000.0, 700.0)),
                Some(DisplayId::new(1))
            ))
        );
    }

    #[test]
    fn a_window_stays_inside_its_displays_visible_area_at_its_minimum_size() {
        let displays = [display(1, 1, (0.0, 25.0), (1280.0, 775.0))];
        let oversized = WindowPlacement {
            bounds: windowed((-200.0, 0.0), (2000.0, 1200.0)),
            display: Some([1; 16]),
        };
        assert_eq!(
            place(oversized, &displays, true, MINIMUM),
            Some((
                windowed((0.0, 25.0), (1280.0, 775.0)),
                Some(DisplayId::new(1))
            ))
        );
        let offscreen = WindowPlacement {
            bounds: windowed((1200.0, 760.0), (300.0, 100.0)),
            display: Some([1; 16]),
        };
        assert_eq!(
            place(offscreen, &displays, true, MINIMUM),
            Some((
                windowed((800.0, 540.0), (480.0, 260.0)),
                Some(DisplayId::new(1))
            ))
        );
    }

    #[test]
    fn without_window_positions_only_size_and_state_return() {
        let displays = [display(1, 1, (0.0, 0.0), (1920.0, 1080.0))];
        let restore = Bounds {
            origin: point(px(500.0), px(300.0)),
            size: size(px(1000.0), px(700.0)),
        };
        let placement = WindowPlacement {
            bounds: WindowBounds::Maximized(restore),
            display: Some([1; 16]),
        };
        let (bounds, display) = place(placement, &displays, false, MINIMUM).unwrap();
        assert_eq!(display, None);
        assert!(matches!(bounds, WindowBounds::Maximized(_)));
        assert_eq!(bounds.get_bounds().size, restore.size);
    }

    #[test]
    fn a_window_without_displays_keeps_its_default_placement() {
        let placement = WindowPlacement {
            bounds: windowed((0.0, 0.0), (900.0, 580.0)),
            display: None,
        };
        assert_eq!(place(placement, &[], true, MINIMUM), None);
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
        assert!(!restoration.selects(&reference(1, 2)));
        assert!(restoration.selects(&reference(1, 1)));
        assert!(!restoration.published(OwnerId::from_bytes([1; 32]), |_| true));
        // Once settled, a later presentation never moves the selection.
        assert!(!restoration.selects(&reference(1, 1)));
    }

    #[test]
    fn a_killed_or_detached_active_workspace_falls_back_to_the_default() {
        let mut restoration = Restoration::new(view(Some(reference(1, 1))), 0);
        // Another server's inventory says nothing about it.
        assert!(!restoration.published(OwnerId::from_bytes([2; 32]), |_| false));
        assert!(restoration.published(OwnerId::from_bytes([1; 32]), |_| false));
        assert!(!restoration.selects(&reference(1, 1)));
        assert!(!restoration.published(OwnerId::from_bytes([1; 32]), |_| false));
    }

    #[test]
    fn an_offline_servers_workspace_returns_when_the_server_does() {
        let mut restoration = Restoration::new(view(Some(reference(2, 1))), 0);
        // The local server and the window's own presentation change nothing.
        restoration.observe(0);
        assert!(!restoration.published(OwnerId::from_bytes([1; 32]), |_| false));
        restoration.record(1);
        restoration.observe(1);
        assert!(restoration.selects(&reference(2, 1)));
    }

    #[test]
    fn selecting_anything_after_reopening_keeps_the_persons_choice() {
        let mut restoration = Restoration::new(view(Some(reference(2, 1))), 0);
        restoration.record(1);
        restoration.observe(2);
        assert!(!restoration.selects(&reference(2, 1)));
        assert!(!restoration.published(OwnerId::from_bytes([2; 32]), |_| false));
    }

    #[test]
    fn a_window_without_a_remembered_view_selects_nothing() {
        let mut restoration = Restoration::<u8>::none();
        assert!(!restoration.selects(&reference(1, 1)));
        assert!(!restoration.published(OwnerId::from_bytes([1; 32]), |_| false));
        assert_eq!(restoration.take_workspace(&reference(1, 1)), None);
    }
}
