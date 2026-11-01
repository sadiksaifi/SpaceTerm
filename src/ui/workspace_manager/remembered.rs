//! The Workspace Window's part of the view a closed window leaves for the next one.
//!
//! [`crate::ui::remembered_view`] keeps the view while no window is open. This module captures it
//! when the window closes, and presents it as the next window's Workspaces arrive. Every step is
//! passive: nothing selects a Pane, so no Pane control moves and no restored shell starts.
//!
//! The person's keyboard or pointer input in the window settles what is still awaited, so a
//! remembered choice never replaces one the person made. The window learns of that input in one
//! place, [`WorkspaceManager::person_input`], from [`PersonInputScope`], which wraps the window's
//! whole presentation. While a remembered choice is held, it listens in the capture phase, before
//! any element handles the input, for presses, scrolling and keys coming up. A key coming up still
//! arrives after an input method consumed it going down. Keys going down reach a keystroke
//! interceptor first, because a Shortcut's action runs before any element's own key listener.
//! Input handlers report composition start through the same capture scope, before key release.
//! Both listen only while a remembered choice is held. A command chosen from the menu bar reaches the window as an action with
//! no input event, so the menu adapter reports it through [`crate::app::menu_command_chosen`].
//! The window's own changes are not input, so they never settle anything.

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
                remembered.worktrees_expanded = self.worktrees_expanded(entry.id());
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
        self.binding
            .get(self.binding.owner(workspace_id)?)?
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
        self.follow_person_input(window, cx);
    }

    /// Follows the person's input while a remembered choice is held, and stops once nothing is.
    /// Keystrokes need an interceptor, because a Shortcut's action runs before any element's own
    /// key listener; it costs every window, so it lives only as long as the held choice.
    pub(super) fn follow_person_input(&mut self, window: &Window, cx: &mut Context<Self>) {
        let awaits = self.restoration.awaits();
        if awaits == self.person_keys.is_some() {
            return;
        }
        self.person_keys = awaits.then(|| {
            let this_window = window.window_handle().window_id();
            let manager = cx.weak_entity();
            cx.intercept_keystrokes(move |_, window, cx| {
                if window.window_handle().window_id() == this_window {
                    let _ = manager.update(cx, |manager, cx| manager.person_input(cx));
                }
            })
        });
        // The next frame listens for presses, scrolling and key releases, or stops.
        cx.notify();
    }

    /// Wraps the window's whole presentation in the element that tells it about the person's
    /// input while a remembered choice can still move the view. Afterwards it listens for nothing.
    pub(super) fn person_input_scope(
        &self,
        child: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> PersonInputScope {
        PersonInputScope {
            child,
            manager: self.restoration.awaits().then(|| cx.weak_entity()),
        }
    }

    /// The person gave this window input, which is their choice of what it shows. Nothing
    /// remembered moves the view after it.
    pub(crate) fn person_input(&mut self, cx: &mut Context<Self>) {
        if !self.restoration.awaits() {
            return;
        }
        self.restoration.person_input();
        // The keystroke that got here may still be running the interceptor, so it stops after.
        let manager = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = manager.update(cx, |manager, cx| {
                manager.person_keys = None;
                cx.notify();
            });
        });
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
        let Some(binding) = self.binding.get(key) else {
            return;
        };
        let Some(owner) = binding.owner() else {
            return;
        };
        let retained_copy = binding.presents_retained_copy();
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
                self.restore_worktrees_expanded(workspace_id, expanded);
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
        self.follow_person_input(window, cx);
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
        let Some(binding) = self.binding.get(key) else {
            return;
        };
        // A retained copy stands in for an offline server and says nothing about its Workspaces.
        if binding.presents_retained_copy() {
            return;
        }
        let Some(owner) = binding.owner() else {
            return;
        };
        self.restoration
            .published(owner, |target| binding.local(target.workspace).is_some());
        let held: Vec<_> = self
            .restoration
            .take_held(owner)
            .into_iter()
            .filter_map(|(reference, remembered)| {
                let local = binding.local(reference.workspace)?;
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
        self.follow_person_input(window, cx);
    }

    /// Selects the first Workspace in display order when the remembered Active Workspace is gone
    /// and the person has selected nothing since. A saved order still being read decides which
    /// Workspace is first, so the selection waits for it, and each later publication selects the
    /// first Workspace again until the person gives the window input.
    pub(super) fn select_remembered_fallback(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

/// The window's whole presentation, and the person's input to it while a remembered choice is held.
pub(super) struct PersonInputScope {
    child: gpui::AnyElement,
    /// The window to tell, while it holds a remembered choice.
    manager: Option<gpui::WeakEntity<WorkspaceManager>>,
}

impl gpui::IntoElement for PersonInputScope {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for PersonInputScope {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        use gpui::{DispatchPhase, KeyUpEvent, MouseDownEvent, ScrollWheelEvent};

        if let Some(manager) = &self.manager {
            let tell = |manager: &gpui::WeakEntity<WorkspaceManager>| {
                let manager = manager.clone();
                move |phase: DispatchPhase, cx: &mut App| {
                    if phase == DispatchPhase::Capture {
                        let _ = manager.update(cx, |manager, cx| manager.person_input(cx));
                    }
                }
            };
            // This element is an ancestor of everything the window presents, so input to any of it
            // passes here first.
            let key_up = tell(manager);
            window.on_key_event(move |_: &KeyUpEvent, phase, _, cx| key_up(phase, cx));
            let composed = tell(manager);
            window.on_action(
                std::any::TypeId::of::<spaceterm_ui::InputMethodCompositionStarted>(),
                move |_, phase, _, cx| composed(phase, cx),
            );
            let pressed = tell(manager);
            window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| pressed(phase, cx));
            let scrolled = tell(manager);
            window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, cx| scrolled(phase, cx));
        }
        self.child.paint(window, cx);
    }
}
