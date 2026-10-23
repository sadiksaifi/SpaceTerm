//! Transient terminal work for one Workspace Window. Membership ordering stays with shared edits.
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{App, AppContext as _, AsyncApp, Entity, Global, Size, Task, WeakEntity, Window};

use super::{tab_view::TabView, terminal_pane::TerminalPane};
use crate::domain::PaneId;

const START_LIMIT: usize = 4;
const BACKGROUND_START_LIMIT: usize = START_LIMIT - 1;
const SLICE: Duration = Duration::from_millis(2);
const TURN: Duration = Duration::from_millis(8);
const AGE: Duration = Duration::from_secs(2);
const CACHE_PANES: usize = 6;
const CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Readiness(Rc<RefCell<State>>);

#[derive(Default)]
struct Windows(Vec<(gpui::AnyWindowHandle, Readiness)>);
impl Global for Windows {}

struct State {
    entries: Vec<Entry>,
    content_size: Option<Size<gpui::Pixels>>,
    wake: async_channel::Sender<()>,
    _task: Option<Task<()>>,
    _closed: Option<gpui::Subscription>,
    selection: Option<gpui::EntityId>,
    previous: Option<gpui::EntityId>,
    direction: isize,
    clock: u64,
}

#[derive(Clone)]
struct Entry {
    pane: WeakEntity<TerminalPane>,
    tab: WeakEntity<TabView>,
    pane_id: PaneId,
    order: usize,
    workspace: Option<gpui::EntityId>,
    visible: bool,
    focused: bool,
    selected_at: u64,
    queued: Instant,
    dirty: bool,
    starting: bool,
    retained: usize,
}

impl Readiness {
    pub(crate) fn for_window(window: &mut Window, cx: &mut App) -> Self {
        let handle = window.window_handle();
        if let Some(found) = cx
            .try_global::<Windows>()
            .and_then(|windows| windows.0.iter().find(|(id, _)| *id == handle))
        {
            return found.1.clone();
        }
        let (wake, receiver) = async_channel::bounded(1);
        let this = Self(Rc::new(RefCell::new(State {
            entries: Vec::new(),
            content_size: None,
            wake,
            _task: None,
            _closed: None,
            selection: None,
            previous: None,
            direction: 0,
            clock: 0,
        })));
        let worker = Rc::downgrade(&this.0);
        let task = cx.spawn(async move |cx: &mut AsyncApp| {
            while receiver.recv().await.is_ok() {
                loop {
                    while receiver.try_recv().is_ok() {}
                    let Some(worker) = worker.upgrade() else {
                        return;
                    };
                    let more = cx
                        .update_window(handle, |_, window, cx| Readiness(worker).step(window, cx))
                        .unwrap_or(false);
                    if !more {
                        break;
                    }
                    cx.background_executor().timer(TURN).await;
                }
            }
        });
        this.0.borrow_mut()._task = Some(task);
        if cx.try_global::<Windows>().is_none() {
            cx.set_global(Windows::default());
        }
        cx.global_mut::<Windows>().0.push((handle, this.clone()));
        let closed = cx.on_window_closed(move |cx, id| {
            if id == handle.window_id() {
                let windows = &mut cx.global_mut::<Windows>().0;
                if let Some(index) = windows.iter().position(|(window, _)| *window == handle) {
                    let retired = windows.remove(index).1;
                    let mut state = retired.0.borrow_mut();
                    state.wake.close();
                    state._task.take();
                    state._closed.take();
                    state.entries.clear();
                }
            }
        });
        this.0.borrow_mut()._closed = Some(closed);
        this
    }

    pub(crate) fn register(
        &self,
        pane: &Entity<TerminalPane>,
        tab: WeakEntity<TabView>,
        pane_id: PaneId,
    ) {
        self.0.borrow_mut().entries.push(Entry {
            pane: pane.downgrade(),
            tab,
            pane_id,
            order: usize::MAX / 2,
            workspace: None,
            visible: false,
            focused: false,
            selected_at: 0,
            queued: Instant::now(),
            dirty: true,
            starting: false,
            retained: 0,
        });
        self.wake();
    }

    pub(crate) fn registered(&self, pane: gpui::EntityId) -> bool {
        self.0
            .borrow()
            .entries
            .iter()
            .any(|entry| entry.pane.entity_id() == pane)
    }

    pub(crate) fn wake(&self) {
        let _ = self.0.borrow().wake.try_send(());
    }

    pub(crate) fn invalidate_layout(&self) {
        self.0.borrow_mut().content_size = None;
    }

    pub(crate) fn measured(&self, size: Size<gpui::Pixels>) {
        let mut state = self.0.borrow_mut();
        if state.content_size != Some(size) {
            state.content_size = Some(size);
            for entry in &mut state.entries {
                entry.dirty = true;
            }
        }
        drop(state);
        self.wake();
    }

    pub(crate) fn dirty(&self, pane: gpui::EntityId) {
        let mut state = self.0.borrow_mut();
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| entry.pane.entity_id() == pane)
        {
            if !entry.dirty {
                entry.queued = Instant::now();
            }
            entry.dirty = true;
        }
        drop(state);
        self.wake();
    }

    pub(crate) fn navigation(
        &self,
        workspace: gpui::EntityId,
        tab: gpui::EntityId,
        order: usize,
        active: bool,
    ) {
        let mut state = self.0.borrow_mut();
        let selected = active && state.selection != Some(tab);
        if selected {
            let old_order = state
                .entries
                .iter()
                .find(|entry| {
                    Some(entry.tab.entity_id()) == state.selection
                        && entry.workspace == Some(workspace)
                })
                .map(|entry| entry.order);
            state.direction = old_order.map_or(0, |old| (order as isize - old as isize).signum());
            state.previous = state.selection;
            state.selection = Some(tab);
            state.clock += 1;
        }
        let clock = state.clock;
        for entry in &mut state.entries {
            if entry.tab.entity_id() == tab {
                entry.dirty |= entry.order != order || selected;
                entry.order = order;
                entry.workspace = Some(workspace);
                if selected {
                    entry.selected_at = clock;
                }
            }
        }
        drop(state);
        self.wake();
    }

    fn step(&self, window: &mut Window, cx: &mut App) -> bool {
        let began = Instant::now();
        let now = began;
        let Some(size) = self.0.borrow().content_size else {
            return false;
        };
        let mut entries = self.0.borrow().entries.clone();
        entries.retain(|entry| entry.pane.upgrade().is_some() && entry.tab.upgrade().is_some());
        for entry in &mut entries {
            if let Some(pane) = entry.pane.upgrade() {
                let pane = pane.read(cx);
                (entry.visible, entry.focused) = pane.readiness_visibility();
            }
            if entry.starting {
                entry.starting = entry
                    .tab
                    .upgrade()
                    .is_some_and(|tab| tab.read(cx).readiness_starting(entry.pane_id));
            }
        }
        let (selection, previous, direction) = {
            let state = self.0.borrow();
            (state.selection, state.previous, state.direction)
        };
        let selected_entry = entries
            .iter()
            .find(|entry| Some(entry.tab.entity_id()) == selection);
        let selected_order = selected_entry.map_or(0, |entry| entry.order);
        let selected_workspace = selected_entry.and_then(|entry| entry.workspace);
        entries.sort_by_key(|entry| {
            Priority {
                visible: entry.visible,
                focused: entry.focused,
                order: entry.order,
                same_workspace: entry.workspace == selected_workspace,
                selected_at: entry.selected_at,
                previous: Some(entry.tab.entity_id()) == previous,
                queued_for: now.saturating_duration_since(entry.queued),
            }
            .rank(selected_order, direction)
        });
        let mut starting = entries.iter().filter(|entry| entry.starting).count();
        let mut worked = false;
        let mut more = starting > 0;
        for entry in &mut entries {
            if worked && began.elapsed() >= SLICE {
                more |= entry.dirty;
                continue;
            }
            if !entry.dirty {
                continue;
            }
            let (Some(tab), Some(pane)) = (entry.tab.upgrade(), entry.pane.upgrade()) else {
                continue;
            };
            let allocation = tab.read(cx).readiness_allocation(entry.pane_id, size, cx);
            let allowed = starting
                < if entry.visible {
                    START_LIMIT
                } else {
                    BACKGROUND_START_LIMIT
                };
            worked = true;
            let (started, done) = pane.update(cx, |pane, cx| {
                pane.prepare_readiness(allocation, allowed, window, cx)
            });
            if started {
                entry.starting = tab.read(cx).readiness_starting(entry.pane_id);
                starting += usize::from(entry.starting);
            }
            entry.dirty = !done;
            if allowed || done {
                entry.queued = Instant::now();
            }
            entry.retained = pane.read(cx).readiness_cache_cost(cx);
            more |= !done;
        }
        // CPU caches have a window-wide budget. GPU resources are retired separately on hide.
        let mut retained = 0usize;
        let mut panes = 0;
        for entry in &mut entries {
            if entry.visible || entry.retained == 0 {
                continue;
            }
            retained = retained.saturating_add(entry.retained);
            panes += 1;
            if panes > CACHE_PANES || retained > CACHE_BYTES {
                if let Some(pane) = entry.pane.upgrade() {
                    pane.update(cx, |pane, cx| pane.evict_readiness_cache(cx));
                }
                entry.retained = 0;
                entry.dirty = false;
            }
        }
        // Preserve wakeups raised by a Pane while this turn was processing its newest screen.
        let mut state = self.0.borrow_mut();
        for entry in &mut entries {
            if let Some(current) = state
                .entries
                .iter()
                .find(|current| current.pane == entry.pane)
                && current.queued > now
            {
                entry.dirty = true;
            }
        }
        state.entries = entries;
        more
    }
}

#[derive(Clone, Copy, Default)]
struct Priority {
    visible: bool,
    focused: bool,
    order: usize,
    selected_at: u64,
    same_workspace: bool,
    previous: bool,
    queued_for: Duration,
}

impl Priority {
    fn rank(self, selected_order: usize, direction: isize) -> (u8, u64, usize) {
        if self.visible {
            return (u8::from(!self.focused), 0, self.order);
        }
        if self.queued_for >= AGE {
            return (
                2,
                u64::MAX - self.queued_for.as_millis().min(u128::from(u64::MAX)) as u64,
                self.order,
            );
        }
        let distance = self.order.abs_diff(selected_order);
        let ahead = (self.order as isize - selected_order as isize).signum() == direction;
        let likely = if self.same_workspace && distance == 1 && ahead {
            3
        } else if self.previous {
            4
        } else if self.same_workspace && distance == 1 {
            5
        } else if self.same_workspace {
            6
        } else {
            7
        };
        (likely, u64::MAX - self.selected_at, distance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_promotes_a_jump_target_and_visible_splits_ahead_of_aged_work() {
        let focused = Priority {
            visible: true,
            focused: true,
            order: 2,
            ..Default::default()
        };
        let visible = Priority {
            visible: true,
            order: 2,
            ..Default::default()
        };
        let queued = Priority {
            order: 15,
            selected_at: 99,
            queued_for: AGE * 10,
            ..Default::default()
        };
        assert!(focused.rank(2, -1) < visible.rank(2, -1));
        assert!(visible.rank(2, -1) < queued.rank(2, -1));
    }

    #[test]
    fn likely_return_priority_follows_display_direction_previous_selection_and_recency() {
        let candidate = |order| Priority {
            order,
            same_workspace: true,
            ..Default::default()
        };
        let previous = Priority {
            previous: true,
            order: 14,
            ..Default::default()
        };
        assert!(candidate(3).rank(2, 1) < previous.rank(2, 1));
        assert!(previous.rank(2, 1) < candidate(1).rank(2, 1));
        assert!(candidate(1).rank(2, -1) < previous.rank(2, -1));
        let recent = Priority {
            selected_at: 4,
            same_workspace: true,
            order: 12,
            ..Default::default()
        };
        assert!(recent.rank(2, -1) < candidate(5).rank(2, -1));
    }

    #[test]
    fn waiting_background_work_ages_ahead_of_speculation_in_oldest_first_order() {
        let likely = Priority {
            order: 3,
            selected_at: 10,
            ..Default::default()
        };
        let waiting = Priority {
            order: 9,
            queued_for: AGE,
            ..Default::default()
        };
        let oldest = Priority {
            order: 15,
            queued_for: AGE * 2,
            ..Default::default()
        };
        assert!(waiting.rank(2, 1) < likely.rank(2, 1));
        assert!(oldest.rank(2, 1) < waiting.rank(2, 1));
    }

    #[gpui::test]
    fn hidden_text_preparation_retains_only_six_panes_under_the_window_budget(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        use crate::terminal::testing::{
            TestTerminalSessionFactory, TestTerminalSessionRecords, test_local_directory,
        };
        use crate::terminal::{TerminalSessionEvent, WorkspaceTerminalSessionFactory};
        cx.update(crate::ui::init).unwrap();
        let records = TestTerminalSessionRecords::default();
        let factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::new(TestTerminalSessionFactory::new(records.clone())),
            test_local_directory(std::path::PathBuf::from("/tmp/spaceterm-readiness-test")),
        );
        let (manager, cx) = cx.add_window_view(|window, cx| {
            super::super::tab_manager::TabManager::new(factory, window, cx)
        });
        cx.run_until_parked();
        for _ in 0..7 {
            cx.update(|window, cx| {
                manager.update(cx, |manager, cx| manager.create_tab(window, cx))
            });
            cx.run_until_parked();
        }
        assert_eq!(records.starts().len(), 8);
        manager.update(cx, |manager, cx| manager.deactivate(cx));
        let mut emulator =
            crate::terminal::emulator::TerminalEmulator::new(TerminalGeometry::from_grid(
                CellGridSize::new(8, 2),
                LogicalCellSize::new(8.0, 16.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.feed(b"ready");
        let screen = emulator.snapshot().unwrap().unwrap();
        for start in records.starts() {
            records
                .event_sender(start.session_id)
                .unwrap()
                .try_send(TerminalSessionEvent::Screen(screen.clone()))
                .unwrap();
        }
        cx.run_until_parked();
        for _ in 0..20 {
            cx.executor().advance_clock(TURN);
            cx.run_until_parked();
        }
        cx.update(|_, cx| {
            let readiness = &cx.global::<Windows>().0[0].1;
            let costs = readiness
                .0
                .borrow()
                .entries
                .iter()
                .filter_map(|entry| entry.pane.upgrade())
                .map(|pane| pane.read(cx).readiness_cache_cost(cx))
                .collect::<Vec<_>>();
            assert_eq!(
                costs.iter().filter(|bytes| **bytes > 0).count(),
                6,
                "the window must evict excess hidden preparation"
            );
            assert!(costs.iter().sum::<usize>() <= 16 * 1024 * 1024);
        });
    }

    #[gpui::test]
    fn closing_window_retires_readiness_worker_and_window_registration(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = cx.add_window(|_, _| gpui::EmptyView);
        let (weak, retained_pane_handle) = window
            .update(cx, |_, window, cx| {
                let readiness = Readiness::for_window(window, cx);
                (Rc::downgrade(&readiness.0), readiness)
            })
            .unwrap();
        cx.run_until_parked();
        assert!(weak.upgrade().is_some());
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert!(
            retained_pane_handle.0.borrow()._task.is_none(),
            "closing the window must retire the worker even while a Pane handle survives"
        );
        assert!(retained_pane_handle.0.borrow().entries.is_empty());
        cx.update(|cx| assert!(cx.global::<Windows>().0.is_empty()));
        drop(retained_pane_handle);
        assert!(
            weak.upgrade().is_none(),
            "the retired worker must hold no cycle"
        );
    }
}
