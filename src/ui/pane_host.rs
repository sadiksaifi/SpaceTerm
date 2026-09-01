use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::pane_action_menu::{
    CloseTarget, PaneActionMenuCommand, pane_action_menu_entries, sf_symbol,
};
use super::terminal_focus::{TerminalFocusBlocker, TerminalProductFocus};
use super::{
    ClosePane, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp,
    PreparedRemotePaneRestart, RemoteChildLaunchUnavailable, RemotePaneLifecycleError, SplitDown,
    SplitRight, TERMINAL_KEY_CONTEXT, TerminalPane, TerminalPaneEvent, TogglePaneZoom,
};

#[derive(Debug, Error)]
/// A typed rejection while coordinating Remote lifecycle across one Window's Pane hierarchy.
pub(crate) enum RemotePaneHostLifecycleError {
    #[error("Pane {pane_id} cannot change remote session lifecycle: {source}")]
    Pane {
        pane_id: PaneId,
        #[source]
        source: RemotePaneLifecycleError,
    },
    #[error("the prepared restart belongs to Window {prepared}, not Window {current}")]
    WindowChanged {
        prepared: WindowId,
        current: WindowId,
    },
    #[error("Pane {0} changed after remote restart preparation")]
    PaneChanged(PaneId),
}

/// Move-only restart reservations for every Pane in one unchanged Window hierarchy.
///
/// The token is valid only while Window, Pane, and session-epoch identities remain unchanged.
pub(crate) struct PreparedPaneHostRemoteRestart {
    window_id: WindowId,
    panes: Vec<(PaneId, Entity<TerminalPane>, PreparedRemotePaneRestart)>,
}
use crate::domain::{
    ClosePaneOutcome, FocusDirection, PaneId, PaneNodeRef, PaneSize, PaneTreeRef, SplitAxis,
    SplitId, TerminalWindow, WindowId, WorkspaceDirectoryIdentity, WorkspaceId, ZoomState,
};
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch, SelectionCopy,
    WorkspaceChildLaunchValidation, WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, DefiniteLength, Entity, EventEmitter, MouseDownEvent, Pixels,
    PromptButton, PromptLevel, Render, Window, deferred, div, px, rgba,
};
use gpui_symbols::{Icon, SymbolWeight};
use spaceterm_ui::{
    ButtonSize, ButtonVariant, IconButton, Menu, MenuAlignment, MenuLifecycleEvent, MenuPlacement,
    MenuPlacementConfig, MenuSize, ResizeAxis, ResizeHandle, ResizeHandleEvent, ResizeInputSource,
    Tooltip,
};

const DIVIDER_SIZE: f32 = super::resize_handle_theme::VISIBLE_THICKNESS;
const PANE_HEADER_HEIGHT: f32 = 32.0;
const PANE_HEADER_HORIZONTAL_PADDING: f32 = 12.0;
const PANE_CONTROL_INSET: f32 = 4.0;
const PANE_CONTROL_TOP: f32 = 2.0;
const PANE_CONTROL_SIZE: f32 = 28.0;
const MINIMUM_PANE_WIDTH: f32 = PANE_HEADER_HORIZONTAL_PADDING * 2.0 + PANE_CONTROL_SIZE;
const MINIMUM_PANE_HEIGHT: f32 = PANE_HEADER_HEIGHT + PANE_CONTROL_INSET;

const _: () = assert!(
    MINIMUM_PANE_WIDTH >= PANE_CONTROL_SIZE + PANE_CONTROL_INSET * 2.0
        && MINIMUM_PANE_HEIGHT >= PANE_CONTROL_TOP + PANE_CONTROL_SIZE + PANE_CONTROL_INSET
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PaneHostEvent {
    CloseWindowRequested {
        window_id: WindowId,
    },
    PresentationChanged {
        window_id: WindowId,
    },
    ReportedWorkingDirectoryChanged {
        window_id: WindowId,
        pane_id: PaneId,
        path: PathBuf,
    },
    PaneClosed {
        window_id: WindowId,
        pane_id: PaneId,
        promoted_pane_id: PaneId,
        promoted_directory: Option<PathBuf>,
    },
    DirectoryAvailable {
        identity: crate::domain::WorkspaceDirectoryIdentity,
    },
    DirectoryUnavailable {
        reason: String,
    },
}

pub(crate) struct PaneHost {
    terminal_window: TerminalWindow<Entity<TerminalPane>>,
    session_factory: WorkspaceTerminalSessionFactory,
    pane_bounds: BTreeMap<PaneId, Bounds<Pixels>>,
    split_bounds: BTreeMap<SplitId, Bounds<Pixels>>,
    pane_titles: BTreeMap<PaneId, gpui::SharedString>,
    pane_attention: BTreeMap<PaneId, u32>,
    menu_pane_id: Option<PaneId>,
    resizing_split_id: Option<SplitId>,
    active: bool,
    focus_branch_blocker: Option<TerminalFocusBlocker>,
    native_service_hierarchy_generation: u64,
    native_service_focus_signature: Option<(bool, PaneId, Option<TerminalFocusBlocker>)>,
    close_window_requested: bool,
    remote_disconnected_generation: Option<u64>,
    child_launch_generation: u64,
}

impl PaneHost {
    #[cfg(test)]
    pub(crate) fn new(
        window_id: WindowId,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prepared_launch = match session_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(error) => panic!("test PaneHost channel preparation failed: {error}"),
        };
        Self::new_with_prepared_launch(window_id, session_factory, prepared_launch, window, cx)
    }

    pub(crate) fn new_with_prepared_launch(
        window_id: WindowId,
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let minimum_pane_size = match PaneSize::new(MINIMUM_PANE_WIDTH, MINIMUM_PANE_HEIGHT) {
            Ok(size) => size,
            Err(error) => {
                unreachable!("fixed minimum Pane dimensions must be valid: {error}")
            }
        };
        let terminal_window = TerminalWindow::new(window_id, minimum_pane_size, |pane_id| {
            Self::create_terminal(
                pane_id,
                session_factory.clone(),
                prepared_launch,
                window,
                cx,
            )
        });
        let initial_pane_id = terminal_window.focused_pane_id();
        let Some(initial_terminal) = terminal_window.terminal(initial_pane_id) else {
            unreachable!("a new Window must own its initial Pane terminal")
        };
        let initial_title = initial_terminal.read(cx).title();

        Self {
            terminal_window,
            session_factory,
            pane_bounds: BTreeMap::new(),
            split_bounds: BTreeMap::new(),
            pane_titles: BTreeMap::from([(initial_pane_id, initial_title)]),
            pane_attention: BTreeMap::from([(initial_pane_id, 0)]),
            menu_pane_id: None,
            resizing_split_id: None,
            active: true,
            focus_branch_blocker: None,
            native_service_hierarchy_generation: 0,
            native_service_focus_signature: None,
            close_window_requested: false,
            remote_disconnected_generation: None,
            child_launch_generation: 0,
        }
    }

    fn create_terminal(
        pane_id: PaneId,
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let terminal = cx.new(|cx| {
            TerminalPane::new_with_prepared_launch(session_factory, prepared_launch, window, cx)
        });
        cx.subscribe_in(
            &terminal,
            window,
            move |host, _terminal, event: &TerminalPaneEvent, window, cx| match event {
                TerminalPaneEvent::FocusRequested => host.focus_pane(pane_id, cx),
                TerminalPaneEvent::TitleChanged(title) => {
                    host.pane_titles.insert(pane_id, title.clone());
                    cx.emit(PaneHostEvent::PresentationChanged {
                        window_id: host.terminal_window.id(),
                    });
                    cx.notify();
                }
                TerminalPaneEvent::ReportedWorkingDirectoryChanged(path) => {
                    cx.emit(PaneHostEvent::ReportedWorkingDirectoryChanged {
                        window_id: host.terminal_window.id(),
                        pane_id,
                        path: path.clone(),
                    });
                }
                TerminalPaneEvent::AttentionChanged { unread_count } => {
                    host.pane_attention.insert(pane_id, *unread_count);
                    cx.emit(PaneHostEvent::PresentationChanged {
                        window_id: host.terminal_window.id(),
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
            .terminal_window
            .terminal(self.terminal_window.focused_pane_id())
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
        let pane_id = self.terminal_window.focused_pane_id();
        let window_id = self.terminal_window.id();
        let hierarchy_generation = self.native_service_hierarchy_generation;
        let Some(terminal) = self.terminal_window.terminal(pane_id) else {
            return NativeServiceStatus::default();
        };
        terminal.update(cx, |terminal, cx| {
            terminal.native_service_status(
                workspace_id,
                window_id,
                pane_id,
                hierarchy_generation,
                window,
                cx,
            )
        })
    }

    pub(crate) fn native_service_selection(
        &self,
        origin: NativeServiceOrigin,
        window: &Window,
        cx: &mut App,
    ) -> Option<SelectionCopy> {
        if self.terminal_window.id() != origin.window_id()
            || self.terminal_window.focused_pane_id() != origin.pane_id()
            || self.native_service_hierarchy_generation != origin.hierarchy_generation()
        {
            return None;
        }
        self.terminal_window
            .terminal(origin.pane_id())?
            .update(cx, |terminal, cx| {
                terminal.native_service_selection(origin, window, cx)
            })
    }

    pub(crate) fn insert_native_service_text(
        &self,
        origin: NativeServiceOrigin,
        text: String,
        window: &Window,
        cx: &mut App,
    ) -> bool {
        if self.terminal_window.id() != origin.window_id()
            || self.terminal_window.focused_pane_id() != origin.pane_id()
            || self.native_service_hierarchy_generation != origin.hierarchy_generation()
        {
            return false;
        }
        let Some(terminal) = self.terminal_window.terminal(origin.pane_id()) else {
            return false;
        };
        terminal.update(cx, |terminal, cx| {
            terminal.insert_native_service_text(origin, text, window, cx)
        })
    }

    pub(crate) const fn window_id(&self) -> WindowId {
        self.terminal_window.id()
    }

    pub(crate) fn pane_count(&self) -> usize {
        self.terminal_window.pane_count()
    }

    pub(crate) fn root_pane_id(&self) -> PaneId {
        self.terminal_window.root_pane_id()
    }

    pub(crate) fn reported_working_directory(&self, pane_id: PaneId, cx: &App) -> Option<PathBuf> {
        self.terminal_window
            .terminal(pane_id)
            .and_then(|terminal| terminal.read(cx).reported_working_directory())
    }

    pub(crate) fn set_workspace_directory(
        &mut self,
        path: &Path,
        identity: WorkspaceDirectoryIdentity,
    ) {
        self.session_factory
            .set_working_directory(path.to_path_buf(), identity);
    }

    pub(crate) fn window_title(&self) -> gpui::SharedString {
        let attention = self.pane_attention.values().copied().sum::<u32>();
        let pane_count = self.terminal_window.pane_count();
        if pane_count > 1 {
            return if attention > 0 {
                format!("• {pane_count} Panes").into()
            } else {
                format!("{pane_count} Panes").into()
            };
        }

        let title = self
            .pane_titles
            .get(&self.terminal_window.focused_pane_id())
            .cloned()
            .unwrap_or_else(|| "Terminal".into());
        if attention > 0 {
            format!("• {title}").into()
        } else {
            title
        }
    }

    pub(crate) const fn zoom_state(&self) -> ZoomState {
        self.terminal_window.zoom_state()
    }

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.focus(window, cx);
    }

    pub(crate) fn activate_without_focus(&mut self, cx: &mut Context<Self>) {
        self.menu_pane_id = None;
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
        if !active {
            self.menu_pane_id = None;
        }
        self.sync_terminal_focus(cx);
    }

    pub(crate) fn close_all(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.menu_pane_id = None;
        self.sync_terminal_focus(cx);
        for terminal in self.terminal_window.terminals() {
            terminal.update(cx, |terminal, _| terminal.close());
        }
    }

    /// Atomically marks every Pane in this Window disconnected for one generation.
    ///
    /// All Panes are prevalidated before any mutation. The Pane tree, focus, zoom, and retained
    /// presentations remain intact, while new child launches and terminal input are blocked.
    pub(crate) fn disconnect_remote(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        self.can_disconnect_remote(generation, cx)?;
        for (_, terminal) in self.terminal_window.terminals_with_ids() {
            terminal.update(cx, |terminal, cx| {
                terminal
                    .disconnect_remote(generation, cx)
                    .expect("prevalidated remote disconnect must remain legal")
            });
        }
        self.remote_disconnected_generation = Some(generation);
        self.child_launch_generation = self.child_launch_generation.wrapping_add(1);
        self.sync_terminal_focus(cx);
        Ok(())
    }

    /// Prevalidates a hierarchy-wide disconnect without mutating any Pane.
    pub(crate) fn can_disconnect_remote(
        &self,
        generation: u64,
        cx: &App,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        for (pane_id, terminal) in self.terminal_window.terminals_with_ids() {
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
        if self.terminal_window.pane_count() != prepared_launches.len() {
            return Err(RemotePaneHostLifecycleError::PaneChanged(
                self.terminal_window.focused_pane_id(),
            ));
        }
        let mut panes = Vec::with_capacity(self.terminal_window.pane_count());
        for ((pane_id, terminal), prepared_launch) in self
            .terminal_window
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
            window_id: self.terminal_window.id(),
            panes,
        })
    }

    /// Revalidates every prepared Pane restart against the current Window hierarchy.
    pub(crate) fn can_commit_remote_restart(
        &self,
        prepared: &PreparedPaneHostRemoteRestart,
        cx: &App,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        if self.terminal_window.id() != prepared.window_id {
            return Err(RemotePaneHostLifecycleError::WindowChanged {
                prepared: prepared.window_id,
                current: self.terminal_window.id(),
            });
        }
        if self.terminal_window.pane_count() != prepared.panes.len() {
            return Err(RemotePaneHostLifecycleError::PaneChanged(
                self.terminal_window.focused_pane_id(),
            ));
        }
        for (pane_id, terminal, pane_restart) in &prepared.panes {
            let Some(current) = self.terminal_window.terminal(*pane_id) else {
                return Err(RemotePaneHostLifecycleError::PaneChanged(*pane_id));
            };
            if current.entity_id() != terminal.entity_id() {
                return Err(RemotePaneHostLifecycleError::PaneChanged(*pane_id));
            }
            terminal
                .read(cx)
                .can_commit_remote_restart(pane_restart)
                .map_err(|source| RemotePaneHostLifecycleError::Pane {
                    pane_id: *pane_id,
                    source,
                })?;
        }
        Ok(())
    }

    /// Commits every prevalidated Pane restart in place after aggregate preparation succeeds.
    ///
    /// Window, Pane-tree, focus, and zoom identities are preserved. Once commit begins, later
    /// Terminal Session startup failure belongs to its individual Pane rather than rolling back
    /// already committed siblings.
    pub(crate) fn commit_remote_restart(
        &mut self,
        prepared: PreparedPaneHostRemoteRestart,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneHostLifecycleError> {
        self.can_commit_remote_restart(&prepared, cx)?;
        for (pane_id, terminal, pane_restart) in prepared.panes {
            terminal.update(cx, |terminal, cx| {
                terminal
                    .commit_remote_restart(pane_restart, window, cx)
                    .unwrap_or_else(|error| {
                        panic!("prevalidated Pane {pane_id} restart commit failed: {error}")
                    })
            });
        }
        self.session_factory = session_factory;
        self.remote_disconnected_generation = None;
        self.child_launch_generation = self.child_launch_generation.wrapping_add(1);
        self.sync_terminal_focus(cx);
        cx.emit(PaneHostEvent::PresentationChanged {
            window_id: self.terminal_window.id(),
        });
        cx.notify();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) const fn focused_pane_id(&self) -> PaneId {
        self.terminal_window.focused_pane_id()
    }

    #[cfg(test)]
    pub(crate) fn pane_entity_ids(&self) -> Vec<(PaneId, gpui::EntityId)> {
        self.terminal_window
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
        encode(self.terminal_window.root(), &mut signature);
        signature
    }

    #[cfg(test)]
    pub(crate) const fn remote_disconnected_generation(&self) -> Option<u64> {
        self.remote_disconnected_generation
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_remote_state(&self, cx: &App) -> (bool, bool) {
        self.terminal_window
            .terminal(self.terminal_window.focused_pane_id())
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
        self.terminal_window
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
        self.terminal_window
            .terminal(self.terminal_window.focused_pane_id())
            .is_some_and(|terminal| terminal.read(cx).is_focused(window))
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_has_input_focus(&self, window: &Window, cx: &App) -> bool {
        self.terminal_window
            .terminal(self.terminal_window.focused_pane_id())
            .is_some_and(|terminal| terminal.read(cx).terminal_input_focused(window, cx))
    }

    #[cfg(test)]
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    fn focus_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        if self.terminal_window.focused_pane_id() == pane_id {
            return;
        }
        if let Err(error) = self.terminal_window.focus_pane(pane_id) {
            eprintln!("failed to focus Pane: {error}");
            return;
        }
        self.menu_pane_id = None;
        self.sync_terminal_focus(cx);
        cx.notify();
    }

    fn focus_pane_in_direction(
        &mut self,
        direction: FocusDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane_id) = self.terminal_window.focus_pane_in_direction(direction) else {
            return;
        };
        self.menu_pane_id = None;
        self.sync_terminal_focus(cx);
        cx.notify();
        if let Some(terminal) = self.terminal_window.terminal(pane_id) {
            terminal.update(cx, |terminal, _| terminal.focus(window));
        }
    }

    pub(crate) fn split_focused(
        &mut self,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused_pane_id = self.terminal_window.focused_pane_id();
        self.split_pane(focused_pane_id, axis, window, cx);
    }

    fn split_pane(
        &mut self,
        target_pane_id: PaneId,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote_disconnected_generation.is_some() {
            cx.emit(RemoteChildLaunchUnavailable::ConnectionUnavailable);
            return;
        }
        let Some(target_bounds) = self.pane_bounds.get(&target_pane_id).copied() else {
            eprintln!("cannot split Pane {target_pane_id} before its bounds are measured");
            return;
        };
        let Ok(target_size) = pane_size(target_bounds) else {
            eprintln!("cannot split Pane {target_pane_id} with invalid measured bounds");
            return;
        };
        match self.session_factory.validate_child_launch() {
            Ok(WorkspaceChildLaunchValidation::Local(directory)) => {
                cx.emit(PaneHostEvent::DirectoryAvailable {
                    identity: directory.identity(),
                });
            }
            Ok(WorkspaceChildLaunchValidation::Remote) => {}
            Err(error) => {
                let reason = error.to_string();
                cx.emit(PaneHostEvent::DirectoryUnavailable {
                    reason: reason.clone(),
                });
                let directory = self.session_factory.local_working_directory().map_or_else(
                    || "the local Workspace Directory".to_owned(),
                    |path| path.display().to_string(),
                );
                let detail = format!(
                    "Cannot create a Pane at {directory} because {reason}. Restore the directory or use another Workspace."
                );
                drop(window.prompt(
                    PromptLevel::Warning,
                    "Workspace Directory Unavailable",
                    Some(&detail),
                    &[PromptButton::ok("OK")],
                    cx,
                ));
                return;
            }
        }
        if let Some(revalidation) = self.session_factory.revalidate_remote_child_launch() {
            self.child_launch_generation = self.child_launch_generation.wrapping_add(1);
            let child_launch_generation = self.child_launch_generation;
            let session_factory = self.session_factory.clone();
            cx.spawn_in(window, async move |host, cx| {
                let revalidation = revalidation.await;
                let _ = host.update_in(cx, |host, window, cx| {
                    if host.remote_disconnected_generation.is_some() {
                        cx.emit(RemoteChildLaunchUnavailable::Cancelled);
                        return;
                    }
                    if host.child_launch_generation != child_launch_generation {
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
                    let Some(current_bounds) = host.pane_bounds.get(&target_pane_id).copied()
                    else {
                        return;
                    };
                    let Ok(current_size) = pane_size(current_bounds) else {
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
        let prepared_launch = match self.session_factory.prepare_child_launch() {
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
        let result = self.terminal_window.split_pane(
            target_pane_id,
            axis,
            target_size,
            DIVIDER_SIZE,
            |new_pane_id| {
                Self::create_terminal(new_pane_id, session_factory, prepared_launch, window, cx)
            },
        );

        match result {
            Ok(pane_id) => {
                self.advance_native_service_hierarchy_generation(cx);
                if let Some(terminal) = self.terminal_window.terminal(pane_id) {
                    self.pane_titles.insert(pane_id, terminal.read(cx).title());
                }
                self.pane_attention.insert(pane_id, 0);
                self.menu_pane_id = None;
                self.split_bounds.clear();
                self.sync_terminal_focus(cx);
                cx.emit(PaneHostEvent::PresentationChanged {
                    window_id: self.terminal_window.id(),
                });
                cx.notify();
                if let Some(terminal) = self.terminal_window.terminal(pane_id) {
                    terminal.update(cx, |terminal, _| terminal.focus(window));
                }
            }
            Err(error) => eprintln!("failed to split Pane: {error}"),
        }
    }

    fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_pane(self.terminal_window.focused_pane_id(), window, cx);
    }

    fn close_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_window_requested {
            return;
        }

        match self.terminal_window.close_pane(pane_id) {
            Ok(ClosePaneOutcome::CloseWindow { window_id }) => {
                self.advance_native_service_hierarchy_generation(cx);
                self.close_window_requested = true;
                self.active = false;
                self.menu_pane_id = None;
                self.sync_terminal_focus(cx);
                cx.emit(PaneHostEvent::CloseWindowRequested { window_id });
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
                self.pane_attention.remove(&pane_id);
                let promoted_pane_id = self.terminal_window.root_pane_id();
                let promoted_directory = self
                    .terminal_window
                    .terminal(promoted_pane_id)
                    .and_then(|terminal| terminal.read(cx).reported_working_directory());
                self.menu_pane_id = None;
                self.sync_terminal_focus(cx);
                cx.emit(PaneHostEvent::PresentationChanged {
                    window_id: self.terminal_window.id(),
                });
                cx.emit(PaneHostEvent::PaneClosed {
                    window_id: self.terminal_window.id(),
                    pane_id,
                    promoted_pane_id,
                    promoted_directory,
                });
                cx.notify();
                if self.active
                    && let Some(terminal) = self.terminal_window.terminal(focused_pane_id)
                {
                    terminal.update(cx, |terminal, _| terminal.focus(window));
                }
            }
            Err(error) => eprintln!("failed to close Pane: {error}"),
        }
    }

    pub(crate) fn toggle_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_window.toggle_zoom().is_none() {
            return;
        }
        self.advance_native_service_hierarchy_generation(cx);
        self.menu_pane_id = None;
        self.sync_terminal_focus(cx);
        cx.emit(PaneHostEvent::PresentationChanged {
            window_id: self.terminal_window.id(),
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
        match self.terminal_window.resize_split(
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
            .terminal_window
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

    fn handle_menu_lifecycle(
        &mut self,
        pane_id: PaneId,
        event: MenuLifecycleEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            MenuLifecycleEvent::Opened => {
                if let Err(error) = self.terminal_window.focus_pane(pane_id) {
                    eprintln!("failed to focus Pane: {error}");
                    return;
                }
                self.menu_pane_id = Some(pane_id);
                self.sync_terminal_focus(cx);
            }
            MenuLifecycleEvent::Closed(_) => {
                if self.menu_pane_id != Some(pane_id) {
                    return;
                }
                self.menu_pane_id = None;
                self.sync_terminal_focus(cx);
            }
        }
        cx.notify();
    }

    fn sync_terminal_focus(&mut self, cx: &mut Context<Self>) {
        self.sync_terminal_focus_with_menu_blocker(self.menu_pane_id.is_some(), cx);
    }

    fn sync_terminal_focus_with_menu_blocker(
        &mut self,
        menu_blocked: bool,
        cx: &mut Context<Self>,
    ) {
        let focused_terminal_id = self
            .terminal_window
            .terminal(self.terminal_window.focused_pane_id())
            .map(Entity::entity_id);
        let visible_terminal_id = match self.terminal_window.zoom_state() {
            ZoomState::Restored => None,
            ZoomState::Zoomed(pane_id) => self
                .terminal_window
                .terminal(pane_id)
                .map(Entity::entity_id),
        };
        let blocker = menu_blocked
            .then_some(TerminalFocusBlocker::PaneMenu)
            .or(self
                .resizing_split_id
                .is_some()
                .then_some(TerminalFocusBlocker::PaneResize))
            .or(self.focus_branch_blocker);
        let signature = (self.active, self.terminal_window.focused_pane_id(), blocker);
        if self.native_service_focus_signature != Some(signature) {
            self.advance_native_service_hierarchy_generation(cx);
            self.native_service_focus_signature = Some(signature);
        }
        let hierarchy_generation = self.native_service_hierarchy_generation;
        let mut panes = Vec::with_capacity(self.terminal_window.pane_count());
        collect_pane_order(self.terminal_window.root(), &mut panes);
        let presented_panes = match self.terminal_window.zoom_state() {
            ZoomState::Zoomed(pane_id) => vec![pane_id],
            ZoomState::Restored => panes.clone(),
        };
        let presentation_order = presented_panes
            .into_iter()
            .enumerate()
            .map(|(order, pane_id)| (pane_id, order))
            .collect::<BTreeMap<_, _>>();
        for pane_id in panes {
            let Some(terminal) = self.terminal_window.terminal(pane_id) else {
                continue;
            };
            let product_focus = TerminalProductFocus {
                active_workspace: self.active,
                active_window: self.active,
                pane_visible: self.active
                    && visible_terminal_id.is_none_or(|visible| visible == terminal.entity_id()),
                focused_pane: Some(terminal.entity_id()) == focused_terminal_id,
                blocker,
            };
            terminal.update(cx, |terminal, cx| {
                let product_focus_changed = terminal.set_product_focus(product_focus);
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
        for terminal in self.terminal_window.terminals() {
            terminal.update(cx, |terminal, _| {
                terminal.synchronize_native_service_hierarchy_generation(hierarchy_generation);
            });
        }
    }

    fn perform_menu_command(
        &mut self,
        command: PaneActionMenuCommand,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu_pane_id = Some(pane_id);
        self.sync_terminal_focus(cx);

        match command {
            PaneActionMenuCommand::SplitRight => {
                self.split_pane(pane_id, SplitAxis::Horizontal, window, cx)
            }
            PaneActionMenuCommand::SplitDown => {
                self.split_pane(pane_id, SplitAxis::Vertical, window, cx)
            }
            PaneActionMenuCommand::ToggleZoom => self.toggle_zoom(window, cx),
            PaneActionMenuCommand::Close => self.close_pane(pane_id, window, cx),
        }
        if self.menu_pane_id.take().is_some() {
            self.sync_terminal_focus(cx);
            cx.notify();
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

    fn on_close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        self.close_focused(window, cx);
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
        let Some(terminal) = self.terminal_window.terminal(pane_id).cloned() else {
            return div()
                .size_full()
                .bg(gpui_color(ACTIVE_THEME.terminal_background))
                .into_any_element();
        };
        let focused = self.terminal_window.focused_pane_id() == pane_id;
        let has_multiple_panes = self.terminal_window.pane_count() > 1;
        let zoomed = matches!(self.terminal_window.zoom_state(), ZoomState::Zoomed(_));
        let title = self
            .pane_titles
            .get(&pane_id)
            .cloned()
            .unwrap_or_else(|| "Terminal".into());
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
            .when(has_multiple_panes, |pane| {
                pane.child(render_pane_header(
                    pane_id,
                    title,
                    focused,
                    zoomed,
                    attention,
                    host.clone(),
                ))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(terminal),
            )
            .when(has_multiple_panes, |pane| {
                pane.child(render_pane_controls(
                    pane_id,
                    focused,
                    zoomed,
                    &pane_group,
                    host.clone(),
                ))
            })
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
            .on_children_prepainted(move |children, _, cx| {
                let (Some(first), Some(last)) = (children.first(), children.last()) else {
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

        split
            .child(split_child(first, axis, ratio))
            .child(deferred(render_divider(
                split_id,
                axis,
                current_offset,
                host,
            )))
            .child(split_child(second, axis, 1.0 - ratio))
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
        let zoom_state = self.terminal_window.zoom_state();
        let minimum_size = match zoom_state {
            ZoomState::Zoomed(_) => self.terminal_window.minimum_pane_size(),
            ZoomState::Restored => match self.terminal_window.minimum_size(DIVIDER_SIZE) {
                Ok(size) => size,
                Err(error) => {
                    eprintln!("failed to calculate minimum Pane layout size: {error}");
                    self.terminal_window.minimum_pane_size()
                }
            },
        };
        let content = match zoom_state {
            ZoomState::Restored => self.render_tree(self.terminal_window.root(), host.clone()),
            ZoomState::Zoomed(pane_id) => self.render_leaf(pane_id, host),
        };

        div()
            .id(("pane-host", self.terminal_window.id().get()))
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

fn render_pane_header(
    pane_id: PaneId,
    title: gpui::SharedString,
    focused: bool,
    zoomed: bool,
    attention: bool,
    host: gpui::WeakEntity<PaneHost>,
) -> AnyElement {
    let divider_color = if focused {
        ACTIVE_THEME.panel_focused_border
    } else {
        ACTIVE_THEME.border
    };
    let title = if zoomed {
        div()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.0))
            .child(
                IconButton::new(
                    ("pane-zoom-restore", pane_id.get()),
                    "Restore Panes",
                    |foreground| {
                        Icon::new("arrow.down.right.and.arrow.up.left")
                            .weight(SymbolWeight::Medium)
                            .size(px(13.0))
                            .color(foreground)
                            .into_any_element()
                    },
                )
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .debug_selector(format!("pane-zoom-restore-{}", pane_id.get()))
                .tooltip(
                    Tooltip::new(
                        ("pane-zoom-restore-tooltip", pane_id.get()),
                        "Restore Panes",
                    )
                    .debug_selector(format!("pane-zoom-restore-tooltip-{}", pane_id.get())),
                )
                .on_activate(move |_, window, cx| {
                    let _ = host.update(cx, |host, cx| host.toggle_zoom(window, cx));
                }),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(format!("{title} · Zoomed")),
            )
            .into_any_element()
    } else {
        div().min_w_0().truncate().child(title).into_any_element()
    };

    div()
        .id(("pane-header", pane_id.get()))
        .debug_selector(move || {
            format!(
                "pane-header-{}-{}",
                pane_id.get(),
                if focused { "focused" } else { "unfocused" }
            )
        })
        .relative()
        .h(px(PANE_HEADER_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .min_w_0()
        .pl(px(PANE_HEADER_HORIZONTAL_PADDING))
        .pr(px(PANE_CONTROL_INSET + PANE_CONTROL_SIZE + 4.0))
        .border_b(px(1.0))
        .border_color(gpui_color(divider_color))
        .bg(gpui_color(ACTIVE_THEME.terminal_background))
        .text_size(px(12.0))
        .text_color(gpui_color(ACTIVE_THEME.text_muted))
        .when(attention, |header| {
            header.child(
                div()
                    .debug_selector(move || format!("pane-attention-{}", pane_id.get()))
                    .mr(px(7.0))
                    .size(px(7.0))
                    .rounded_full()
                    .bg(gpui_color(ACTIVE_THEME.warning)),
            )
        })
        .child(title)
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

fn render_pane_controls(
    pane_id: PaneId,
    focused: bool,
    zoomed: bool,
    pane_group: &str,
    host: gpui::WeakEntity<PaneHost>,
) -> AnyElement {
    let activation_host = host.clone();
    let lifecycle_host = host;

    div()
        .id(("pane-controls", pane_id.get()))
        .absolute()
        .top(px(PANE_CONTROL_TOP))
        .right(px(PANE_CONTROL_INSET))
        .when(!focused, |controls| {
            controls
                .opacity(0.0)
                .group_hover(pane_group.to_owned(), |controls| controls.opacity(1.0))
        })
        .child(
            Menu::new(
                ("pane-menu", pane_id.get()),
                "Pane Actions",
                pane_action_menu_entries("pane-menu", zoomed, true, CloseTarget::Pane),
            )
            .icon_trigger(sf_symbol("ellipsis"))
            .size(MenuSize::Wide)
            .placement(
                MenuPlacementConfig::new(MenuPlacement::Bottom, MenuAlignment::End).offset(px(0.0)),
            )
            .debug_selector(format!("pane-menu-button-{}", pane_id.get()))
            .on_activate(move |activation, window, cx| {
                let command = *activation.action();
                let _ = activation_host.update(cx, |host, cx| {
                    host.perform_menu_command(command, pane_id, window, cx);
                });
            })
            .on_lifecycle(move |event, cx| {
                let event = *event;
                let _ = lifecycle_host.update(cx, |host, cx| {
                    host.handle_menu_lifecycle(pane_id, event, cx);
                });
            }),
        )
        .into_any_element()
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

        fn revalidate(&self) -> gpui::Task<Result<(), RemoteChannelRevalidationError>> {
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
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        )
    }

    fn remote_test_session_factory(
        records: TestTerminalSessionRecords,
    ) -> WorkspaceTerminalSessionFactory {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let command_context = Arc::new(
            SshCommandContext::new(
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
            crate::domain::ValidatedWorkspaceDirectory::new(
                PathBuf::from("/missing/local/home-is-not-a-workspace"),
                WorkspaceDirectoryIdentity::new(71, 73),
            ),
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                destination,
                crate::domain::RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
            ),
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
        cx.update(crate::ui::init);
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let host = cx.new(|cx| PaneHost::new(WindowId::new(1), session_factory, window, cx));
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
                host.child_launch_generation = host.child_launch_generation.wrapping_add(1);
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory = remote_test_session_factory(records.clone());
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(RevalidatingRemoteChannelProvider::new(destination.clone()));
        let session_factory = remote_test_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::clone(&provider) as Arc<dyn RemoteTerminalChannelProvider>,
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let command_context = Arc::new(
            SshCommandContext::new(
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
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
        cx.update(crate::ui::init);
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
    fn attention_remains_scoped_to_its_owning_pane_and_window_title(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), test_session_factory(), window, cx)
        });

        host.update(cx, |host, _| {
            host.pane_attention.insert(PaneId::new(1), 2);
        });

        assert_eq!(
            host.read_with(cx, |host, _| host.window_title()),
            "• Terminal"
        );
        assert_eq!(
            host.read_with(cx, |host, _| host.pane_attention.clone()),
            BTreeMap::from([(PaneId::new(1), 2)])
        );
    }

    fn test_workspace_root() -> PathBuf {
        PathBuf::from("/tmp/spaceterm-test-workspace")
    }

    fn focused_panes_after_shortcuts<const N: usize>(
        host: &Entity<PaneHost>,
        cx: &mut VisualTestContext,
        shortcuts: [&str; N],
    ) -> [PaneId; N] {
        shortcuts.map(|shortcut| {
            cx.simulate_keystrokes(shortcut);
            host.read_with(cx, |host, _| host.terminal_window.focused_pane_id())
        })
    }

    #[gpui::test]
    fn single_pane_should_not_render_a_pane_header(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let session_factory = test_session_factory();
        let (_host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        assert!(cx.debug_bounds("pane-header-1-focused").is_none());
    }

    #[gpui::test]
    fn pane_menu_restores_its_trigger_before_terminal_input_can_be_refocused(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });
        cx.update(|window, app| {
            window.activate_window();
            host.update(app, |host, app| {
                host.focus(window, app);
                host.split_focused(SplitAxis::Horizontal, window, app);
            });
        });
        cx.run_until_parked();

        let initial =
            cx.update(|window, app| host.read(app).focused_terminal_has_input_focus(window, app));
        assert!(initial);

        let menu_button = cx
            .debug_bounds("pane-menu-button-2")
            .expect("focused Pane menu button must be rendered")
            .center();
        cx.simulate_click(menu_button, Modifiers::none());
        cx.run_until_parked();
        let menu_open = cx.update(|window, app| {
            (
                host.read(app).focused_pane_id(),
                host.read(app).menu_pane_id,
                host.read(app).focused_terminal_has_input_focus(window, app),
            )
        });
        assert_eq!(menu_open, (PaneId::new(2), Some(PaneId::new(2)), false));

        cx.simulate_click(menu_button, Modifiers::none());
        cx.run_until_parked();
        assert!(!cx.update(|window, app| {
            host.read(app).focused_terminal_has_input_focus(window, app)
        }));

        cx.update(|window, app| host.read(app).focus(window, app));
        assert!(cx.update(|window, app| {
            host.read(app).focused_terminal_has_input_focus(window, app)
        }));

        cx.deactivate_window();
        assert!(!cx.update(|window, app| {
            host.read(app).focused_terminal_has_input_focus(window, app)
        }));
    }

    #[gpui::test]
    fn pane_menu_activation_should_not_restore_terminal_before_command_completion(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        let command_count = records.commands().len();

        let menu_button = cx
            .debug_bounds("pane-menu-button-1")
            .expect("Pane menu button must be rendered")
            .center();
        cx.simulate_mouse_down(menu_button, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(menu_button, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let menu_row = cx
            .debug_bounds("pane-menu-row-split-right")
            .expect("Pane menu row must be rendered")
            .center();
        cx.simulate_mouse_down(menu_row, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let focus_edges_before_activation = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(focus_edges_before_activation, [false]);

        cx.simulate_mouse_up(menu_row, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let focus_edges = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            focus_edges.iter().position(|focused| *focused),
            focus_edges.len().checked_sub(1)
        );
        assert_eq!(host.read_with(cx, |host, _| host.pane_count()), 3);
    }

    #[gpui::test]
    fn initial_and_split_panes_should_start_in_the_workspace_root(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let workspace_root = PathBuf::from("/tmp/spaceterm-explicit-workspace-root");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(workspace_root.clone()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
            vec![workspace_root.clone(), workspace_root]
        );
    }

    #[gpui::test]
    fn split_panes_should_render_compact_focused_and_unfocused_headers(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let unfocused_height = cx
            .debug_bounds("pane-header-1-unfocused")
            .map(|bounds| bounds.size.height);
        let focused_height = cx
            .debug_bounds("pane-header-2-focused")
            .map(|bounds| bounds.size.height);

        assert_eq!(
            (unfocused_height, focused_height),
            (Some(px(PANE_HEADER_HEIGHT)), Some(px(PANE_HEADER_HEIGHT)))
        );
    }

    #[gpui::test]
    fn focusing_another_pane_should_move_the_focused_header_state(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.focus_pane(PaneId::new(1), cx);
            });
        });
        cx.run_until_parked();

        let header_state = (
            cx.debug_bounds("pane-header-1-focused").is_some(),
            cx.debug_bounds("pane-header-2-unfocused").is_some(),
        );
        assert_eq!(header_state, (true, true));
    }

    #[gpui::test]
    fn native_service_return_is_rejected_after_pane_changes_away_and_back(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
                host.insert_native_service_text(origin, "stale return".to_owned(), window, cx)
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
                host.insert_native_service_text(origin, "stale return".to_owned(), window, cx)
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
            host.terminal_window
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
    fn terminal_title_event_should_update_the_pane_header_snapshot(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
                host.terminal_window.pane_count(),
                host.terminal_window.focused_pane_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, PaneId::new(1), vec![2]));
    }

    #[gpui::test]
    fn exited_last_terminal_session_should_request_window_close(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let close_requests = Rc::new(Cell::new(0));
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
        cx.update(crate::ui::init);
        let presentation_changes = Rc::new(Cell::new(0));
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
        cx.update(crate::ui::init);
        let presentation_changes = Rc::new(Cell::new(0));
        let session_factory = test_session_factory();
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
                host.toggle_zoom(window, cx);
            });
        });
        cx.run_until_parked();

        let restore_button = cx
            .debug_bounds("pane-zoom-restore-2")
            .map(|bounds| bounds.center())
            .expect("the zoom restore button was not rendered");
        cx.simulate_mouse_move(restore_button, None, Modifiers::none());
        cx.simulate_click(restore_button, Modifiers::none());
        cx.run_until_parked();

        let state = host.read_with(cx, |host, _| {
            (host.terminal_window.zoom_state(), records.pointer_count())
        });
        assert_eq!(state, (ZoomState::Restored, 0));
    }

    #[gpui::test]
    fn menu_click_should_execute_command_without_sending_terminal_pointer_input(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let menu_button = cx
            .debug_bounds("pane-menu-button-2")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "focused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
        }
        cx.run_until_parked();

        let split_down = cx
            .debug_bounds("pane-menu-row-split-down")
            .map(|bounds| bounds.center());
        assert!(split_down.is_some(), "Split Down menu row was not rendered");
        if let Some(split_down) = split_down {
            cx.simulate_mouse_move(split_down, None, Modifiers::none());
            cx.simulate_click(split_down, Modifiers::none());
        }
        cx.run_until_parked();

        let state = host.read_with(cx, |host, _| {
            (
                host.terminal_window.pane_count(),
                host.menu_pane_id,
                records.pointer_count(),
            )
        });
        assert_eq!(state, (3, None, 0));
    }

    #[gpui::test]
    fn ellipsis_click_should_toggle_menu_without_sending_terminal_pointer_input(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let menu_button = cx
            .debug_bounds("pane-menu-button-2")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "focused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
            cx.run_until_parked();
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
            cx.run_until_parked();
        }

        let state = host.read_with(cx, |host, _| (host.menu_pane_id, records.pointer_count()));
        assert_eq!(state, (None, 0));
    }

    #[gpui::test]
    fn opening_nonfocused_pane_menu_should_focus_and_zoom_the_target_pane(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let first_pane_id = PaneId::new(1);
        let menu_button = cx
            .debug_bounds("pane-menu-button-1")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "nonfocused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
        }
        cx.run_until_parked();

        let zoom_row = cx
            .debug_bounds("pane-menu-row-toggle-zoom")
            .map(|bounds| bounds.center());
        assert!(zoom_row.is_some(), "Zoom Pane menu row was not rendered");
        if let Some(zoom_row) = zoom_row {
            cx.simulate_mouse_move(zoom_row, None, Modifiers::none());
            cx.simulate_click(zoom_row, Modifiers::none());
        }
        cx.run_until_parked();

        let terminal = host.read_with(cx, |host, _| {
            host.terminal_window.terminal(first_pane_id).cloned()
        });
        let terminal_is_focused = cx.update(|window, cx| {
            terminal
                .as_ref()
                .is_some_and(|terminal| terminal.read(cx).is_focused(window))
        });
        let state = host.read_with(cx, |host, _| {
            (
                host.terminal_window.focused_pane_id(),
                host.terminal_window.zoom_state(),
                terminal_is_focused,
                records.pointer_count(),
            )
        });

        assert_eq!(
            state,
            (first_pane_id, ZoomState::Zoomed(first_pane_id), true, 0)
        );
    }

    #[gpui::test]
    fn pane_menu_should_render_wide_compact_menu(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();

        let menu_button = cx
            .debug_bounds("pane-menu-button-2")
            .map(|bounds| bounds.center());
        assert!(
            menu_button.is_some(),
            "focused Pane menu button was not rendered"
        );
        if let Some(menu_button) = menu_button {
            cx.simulate_mouse_move(menu_button, None, Modifiers::none());
            cx.simulate_click(menu_button, Modifiers::none());
        }
        cx.run_until_parked();

        let first_row_height = cx
            .debug_bounds("pane-menu-row-split-right")
            .map(|bounds| bounds.size.height);
        let last_row_height = cx
            .debug_bounds("pane-menu-row-close-pane")
            .map(|bounds| bounds.size.height);
        let menu_size = cx.debug_bounds("menu-panel-0").map(|bounds| bounds.size);

        assert_eq!(
            (first_row_height, last_row_height, menu_size),
            (
                Some(px(26.0)),
                Some(px(26.0)),
                Some(size(px(240.0), px(121.0)))
            )
        );
    }

    #[gpui::test]
    fn shared_resize_handle_should_resize_split_without_leaking_terminal_input(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
            let ratio = match host.terminal_window.root().node() {
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
    fn integrated_split_resize_handle_should_own_both_outer_hitbox_edges(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(test_workspace_root()),
        );
        let (host, cx) = cx.add_window_view(|window, cx| {
            PaneHost::new(WindowId::new(1), session_factory, window, cx)
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
}
