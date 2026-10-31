//! The Workspace Window's part of the view a closed window leaves for the next one.
//!
//! [`crate::ui::remembered_view`] keeps the view while no window is open. This module captures it
//! when the window closes, and presents it as the next window's Workspaces arrive. Every step is
//! passive: nothing selects a Pane, so no Pane control moves and no restored shell starts.
//!
//! The person's keyboard or pointer input in the window settles what is still awaited, so a
//! remembered choice never replaces one the person made. The window learns of that input in one
//! place, [`WorkspaceManager::person_input`], from two listeners that see it before any element
//! handles it: a keystroke interceptor for this window, and a window-wide capture-phase listener
//! for presses and scrolling. The window's own changes are not input, so they never settle
//! anything.

use gpui::{App, Context, Window};
use spaceterm_server::workspace_order::WorkspaceReference;

use super::{ServerKey, WorkspaceManager};
use crate::domain::WorkspaceId;
use crate::ui::remembered_view::{Restoration, SidebarView, WindowPlacement, WindowView};

impl WorkspaceManager {
    /// Closes the window, leaving its view for the next Workspace Window.
    pub(super) fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_view(window, cx);
        window.remove_window();
    }

    /// Leaves this window's view for the next Workspace Window, as the window closes.
    pub(super) fn remember_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.remembered_view(window, cx);
        crate::ui::remembered_view::remember_workspace_window(view, cx);
    }

    fn remembered_view(&self, window: &Window, cx: &App) -> WindowView {
        let sidebar = self.sidebar.read(cx);
        let layout = sidebar.layout();
        let workspaces = self
            .workspaces
            .iter()
            .filter_map(|entry| {
                let reference = self.workspace_reference(entry.id())?;
                let mut remembered = entry.payload().read(cx).remember(cx)?;
                remembered.worktrees_expanded = self.shared_worktrees_expanded(entry.id());
                Some((reference, remembered))
            })
            .collect();
        let mut view = WindowView {
            placement: Some(WindowPlacement::of(window, cx)),
            sidebar: SidebarView {
                visible: layout.visible,
                width: layout.width,
                focused: sidebar.is_focused(window),
                focus_visible: sidebar.has_visible_focus(window),
            },
            active_workspace: self
                .workspaces
                .active_workspace_id()
                .and_then(|workspace_id| self.workspace_reference(workspace_id)),
            workspaces,
        };
        self.restoration.carry_into(&mut view);
        view
    }

    /// Names a server Workspace across windows.
    fn workspace_reference(&self, workspace_id: WorkspaceId) -> Option<WorkspaceReference> {
        self.shared
            .get(self.shared.owner(workspace_id)?)?
            .workspace_reference(workspace_id)
    }

    /// Presents the view the last closed Workspace Window left: the sidebar now, and each
    /// Workspace's view as its server presents it.
    pub(crate) fn restore_remembered(
        &mut self,
        view: WindowView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sidebar = view.sidebar;
        self.sidebar.update(cx, |workspace_sidebar, cx| {
            workspace_sidebar.set_layout(sidebar.visible, sidebar.width, window, cx);
            if sidebar.focused {
                workspace_sidebar.take_keyboard(sidebar.focus_visible, window, cx);
            }
        });
        self.restoration = Restoration::new(view);
    }

    /// Follows the person's keystrokes in this window, before anything in it handles them, so
    /// every key counts, whatever it does.
    pub(super) fn follow_person_input(&mut self, window: &Window, cx: &mut Context<Self>) {
        let this_window = window.window_handle().window_id();
        let manager = cx.weak_entity();
        self._person_input = Some(cx.intercept_keystrokes(move |_, window, cx| {
            if window.window_handle().window_id() == this_window {
                let _ = manager.update(cx, |manager, _| manager.person_input());
            }
        }));
    }

    /// Listens for the person's presses and scrolling anywhere in this window, before any element
    /// handles or blocks them. It draws nothing.
    pub(super) fn person_pointer_input(cx: &mut Context<Self>) -> impl gpui::IntoElement {
        use gpui::{DispatchPhase, MouseDownEvent, ScrollWheelEvent, Styled as _};

        let manager = cx.weak_entity();
        gpui::canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let pressed = manager.clone();
                window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = pressed.update(cx, |manager, _| manager.person_input());
                    }
                });
                let scrolled = manager.clone();
                window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = scrolled.update(cx, |manager, _| manager.person_input());
                    }
                });
            },
        )
        .absolute()
        .size_full()
    }

    /// The person gave this window keyboard or pointer input, which is their choice of what it
    /// shows. Nothing remembered moves the view after it.
    pub(super) fn person_input(&mut self) {
        self.restoration.person_input();
    }

    /// Whether the window opened from a remembered view, which never opens a Workspace for a
    /// person on its own.
    pub(super) const fn restores_remembered_view(&self) -> bool {
        self.restoration.remembered()
    }

    /// Presents the remembered view of a Workspace its server just presented, and selects it
    /// when it was the Active Workspace and the person has given the window no input since.
    pub(super) fn restore_remembered_workspace(
        &mut self,
        key: ServerKey,
        workspace_id: WorkspaceId,
        server: spaceterm_server::hierarchy::Id,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shared) = self.shared.get(key) else {
            return;
        };
        let Some(owner) = shared.owner() else {
            return;
        };
        let retained_copy = shared.presents_retained_copy();
        let reference = WorkspaceReference {
            owner,
            workspace: server,
        };
        let Some(manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|entry| entry.payload().clone())
        else {
            return;
        };
        let keyboard = self.content_holds_keyboard(window, cx);
        if let Some(remembered) = self.restoration.take_workspace(&reference) {
            if let Some(expanded) = remembered.worktrees_expanded {
                self.restore_shared_worktrees_expanded(workspace_id, expanded);
            }
            manager.update(cx, |manager, cx| {
                manager.restore_remembered(&remembered, window, cx);
            });
            // A retained copy may still list a Tab or Pane another client closed. The server's
            // own membership decides the selection again.
            if retained_copy {
                self.restoration.hold(reference, remembered);
            }
        }
        let active = self.workspaces.active_workspace_id() == Some(workspace_id);
        if self.restoration.selects(&reference) && !active {
            self.activate_workspace_focusing(workspace_id, false, window, cx);
        }
        // The keyboard follows a Tab or Workspace the restoration changed, unless it rests
        // outside the terminal content, as in the sidebar.
        if keyboard && self.workspaces.active_workspace_id() == Some(workspace_id) {
            self.focus(window, cx);
        }
    }

    /// Settles what the remembered view left open once a server published its own inventory,
    /// which follows that inventory's application and the window's own presentation changes.
    ///
    /// Each Workspace a retained copy presented takes its remembered selection again, so a Tab
    /// or Pane that is gone leaves the Root Tab or Root Pane. A remembered Active Workspace the
    /// inventory lacks was killed or detached, so the first Workspace in display order is
    /// selected instead.
    pub(super) fn settle_remembered_view(
        &mut self,
        key: ServerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shared) = self.shared.get(key) else {
            return;
        };
        // A retained copy stands in for an offline server and says nothing about its Workspaces.
        if shared.presents_retained_copy() {
            return;
        }
        let Some(owner) = shared.owner() else {
            return;
        };
        self.restoration.published(owner, |target| {
            shared.local(target.workspace).is_some()
        });
        let held: Vec<_> = self
            .restoration
            .take_held(owner)
            .into_iter()
            .filter_map(|(reference, remembered)| {
                let local = shared.local(reference.workspace)?;
                let manager = self.workspaces.workspace(local)?.payload().clone();
                Some((local, manager, remembered))
            })
            .collect();
        for (local, manager, remembered) in held {
            let keyboard = self.workspaces.active_workspace_id() == Some(local)
                && self.content_holds_keyboard(window, cx);
            manager.update(cx, |manager, cx| {
                manager.restore_selection(&remembered, window, cx);
            });
            if keyboard {
                self.focus(window, cx);
            }
        }
        self.select_remembered_fallback(window, cx);
    }

    /// Selects the first Workspace in display order when the remembered Active Workspace is gone
    /// and the person has selected nothing since. A saved order still being read decides which
    /// Workspace is first, so the selection waits for it, and each later publication selects the
    /// first Workspace again until the person gives the window input.
    pub(super) fn select_remembered_fallback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.display_order_ready() || !self.restoration.falls_back() {
            return;
        }
        let Some(first) = self.workspaces.iter().next().map(|entry| entry.id()) else {
            return;
        };
        if self.workspaces.active_workspace_id() != Some(first) {
            let keyboard = self.content_holds_keyboard(window, cx);
            self.activate_workspace_focusing(first, false, window, cx);
            if keyboard {
                self.focus(window, cx);
            }
        }
    }

    /// Whether the keyboard rests on the empty view or the Active Workspace's content, where a
    /// restored selection takes it along.
    fn content_holds_keyboard(&self, window: &Window, cx: &Context<Self>) -> bool {
        window.focused(cx).is_none()
            || self.empty_focus.is_focused(window)
            || self
                .active_tab_manager()
                .is_some_and(|manager| manager.read(cx).holds_keyboard(window, cx))
    }
}
