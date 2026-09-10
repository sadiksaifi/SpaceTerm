use super::pane_lifecycle::PaneLifecycleDependencies;
#[cfg(test)]
use super::terminal_focus::TerminalFocusBlocker;
pub(crate) use crate::domain::remote_workspace::RemotePaneLifecycleError;
use crate::domain::remote_workspace::{RemotePaneFacts, RemoteRestartAuthority};
#[cfg(test)]
use crate::terminal::RemoteChannelUnavailable;
use std::cell::Cell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::render_lifecycle::{RenderLifecycle, ScaleChange, SurfaceVisibility};
use super::terminal_context_menu::{TerminalContextMenuCommand, terminal_context_menu_entries};
#[cfg(test)]
use super::terminal_element::PaintPreflightFault;
use super::terminal_element::{
    TerminalGridCache, TerminalGridConfiguration, TerminalGridPresentation,
    terminal_grid_content_bounds,
};
use super::terminal_focus::{TerminalFocusCoordinator, TerminalFocusFacts, TerminalProductFocus};
use super::terminal_graphics::{GraphicsAttemptToken, TerminalGraphicsCache};
use super::terminal_ime::{PreeditLayout, PreeditPosition, TerminalIme, layout_preedit};
use super::{
    CancelUnsafePaste, CloseTerminalFind, ConfirmUnsafePaste, CopySelection,
    DecreaseTerminalFontSize, ExportTerminalDiagnostics, FindNext, FindPrevious,
    FocusNextTerminalFindControl, FocusPreviousTerminalFindControl, IncreaseTerminalFontSize,
    OpenTerminalFind, PasteClipboard, ResetTerminalFontSize, TERMINAL_FIND_KEY_CONTEXT,
    TERMINAL_KEY_CONTEXT, TERMINAL_PASTE_CONFIRMATION_KEY_CONTEXT,
};
use crate::close_confirmation::PaneCloseFacts;
use crate::domain::{PaneId, TabId, WorkspaceId};
use crate::platform::terminal_accessibility::{
    TerminalAccessibilityAdapter, TerminalAccessibilityAdapterFactory, TerminalAccessibilityUpdate,
};
use crate::platform::window_visibility::{WindowVisibility, WindowVisibilitySource};
#[cfg(test)]
use crate::terminal::UnhandledKeyEvent;
use crate::terminal::attention::AttentionState;
use crate::terminal::attention_runtime::AttentionPaneId;
use crate::terminal::geometry::{
    BackingPosition, BackingScale, CellGridPosition, CellGridSize, LogicalCellSize,
    LogicalPosition, LogicalSize, TerminalGeometry,
};
use crate::terminal::native_services::clipboard::{FileClipboard, SelectionPublication};
use crate::terminal::native_services::file_preview::{FilePreviewPanel, FilePreviewPresenter};
use crate::terminal::native_services::{
    NativeServiceAdapters, TerminalContextMenuState, activated_link, revalidated_context_link,
};
use crate::terminal::secure_input::SecureInputPane;
use crate::terminal::wheel_phase::resolve_wheel_phase;
use crate::terminal::{
    AccessibilityGeometry, AccessibilityNotification, AccessibilityNotifications, AttentionFacts,
    DiagnosticBundle, DiagnosticKeyEventKind, FilePreviewTarget, FindDirection,
    FindQueryGeneration, InputModifiers, KeyAction, KeyInput, KeyTranslation, NativeContextActions,
    NativeServiceCapabilities, NativeServiceOrigin, NativeServiceStatus, PaneTerminalState,
    PasteConfirmation, PasteDecision, PastePayload, PasteRequestOutcome, PasteResolution,
    PhysicalKey, PointerButton, PointerInput, PointerPhase, PreparedWorkspaceTerminalLaunch,
    ScreenSnapshot, SelectionCopy, SelectionCopyError, SessionEvent, ShiftSelectionPolicy,
    SurfacePosition, TerminalAccessibilityModel, TerminalFailure, TerminalKeyInputAdapter,
    TerminalKeyInputEventKind, TerminalLocalFileCapabilities, TerminalSessionHandle,
    UnhandledKeyDiagnostic, WheelInput, WheelPhase, WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};
#[cfg(test)]
use gpui::ClipboardItem;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, EntityInputHandler, EventEmitter, ExternalPaths,
    FocusHandle, IntoElement, KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, Task, TextRun, UTF16Selection, Window, div, font, point, px, relative, rgba,
    size,
};
use spaceterm_ui::{
    Button, ButtonRole, ButtonSize, ButtonVariant, ContextMenu, EditCopy, EditPaste, Icon,
    IconButton, IconName, MenuLifecycleEvent, MenuSize, OverlayScrollbar, OverlayScrollbarEvent,
    ScrollMetrics, TextInput, TextInputEvent, TextInputTabBehavior, TextInputVariant, Tooltip,
    window_modal_is_open,
};

const DEFAULT_FONT_SIZE: f32 = 18.0;
const DEFAULT_LINE_HEIGHT: f32 = 20.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 32.0;
const FONT_SIZE_STEP: f32 = 1.0;
const HORIZONTAL_PADDING: f32 = 4.0;
/// The gap above the first terminal row, kept tight so the Pane Caption reads as its header.
const TOP_PADDING: f32 = 2.0;
/// The gap below the last terminal row, which has no neighbouring chrome to close up against.
const BOTTOM_PADDING: f32 = 8.0;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
const MAX_PANE_TITLE_CHARACTERS: usize = 256;
const PRESENTATION_BLINK_INTERVAL: Duration = Duration::from_millis(600);
const VISUAL_BELL_DURATION: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurfaceActivity {
    application_active: bool,
    operating_system_window_key: bool,
}

fn terminal_surface_active(product_focus: TerminalProductFocus, activity: SurfaceActivity) -> bool {
    product_focus.active_workspace
        && product_focus.active_tab
        && activity.operating_system_window_key
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum TerminalPaneEvent {
    FocusRequested,
    TitleChanged(SharedString),
    CaptionChanged,
    AttentionChanged { unread_count: u32 },
    Exited,
}

impl std::fmt::Debug for TerminalPaneEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FocusRequested => "TerminalPaneEvent::FocusRequested",
            Self::TitleChanged(_) => "TerminalPaneEvent::TitleChanged",
            Self::CaptionChanged => "TerminalPaneEvent::CaptionChanged",
            Self::AttentionChanged { .. } => "TerminalPaneEvent::AttentionChanged",
            Self::Exited => "TerminalPaneEvent::Exited",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PasteRequestGuard {
    session_identity: u64,
    focus_epoch: u64,
    hierarchy_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HoveredTerminalLink {
    generation: crate::terminal::PresentationGeneration,
    cell: CellGridPosition,
    target: crate::terminal::HyperlinkTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreeditLayoutKey {
    marked_revision: u64,
    start_row: usize,
    start_column: usize,
    columns: usize,
    caret_utf16: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryAction {
    Presentation,
    RendererResources,
    StartSession,
    CopySelection,
    ExportDiagnostics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RecoveryToken {
    revision: u64,
    action: RecoveryAction,
    generation: crate::terminal::PresentationGeneration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct OperationToken {
    id: u64,
    state_revision: u64,
    generation: crate::terminal::PresentationGeneration,
    recovery: Option<RecoveryToken>,
}

/// A move-only restart token bound to one Pane session epoch and successor generation.
///
/// Preparation reserves a fresh channel but does not replace the current Terminal Session.
pub(crate) struct PreparedRemotePaneRestart {
    session_factory: WorkspaceTerminalSessionFactory,
    prepared_launch: PreparedWorkspaceTerminalLaunch,
    authority: RemoteRestartAuthority,
}

#[derive(Clone, Copy)]
enum PaneSessionStartFailure {
    Preparation,
    Worker,
    RemoteRestart,
}

impl PaneSessionStartFailure {
    fn terminal_failure(self) -> TerminalFailure {
        TerminalFailure::platform(match self {
            Self::Preparation => "prepare-session-channel",
            Self::Worker => "start-session-worker",
            Self::RemoteRestart => "restart-remote-session",
        })
    }

    fn preserves_frame(self, remote: bool) -> bool {
        remote && !matches!(self, Self::Preparation)
    }
}

/// Owns a Pane's Terminal Session, launch authority and event task retirement.
struct PaneSessionLifecycle {
    session_factory: WorkspaceTerminalSessionFactory,
    prepared_launch: Option<PreparedWorkspaceTerminalLaunch>,
    current_directory: Option<crate::terminal::metadata::CurrentDirectory>,
    local_file_capabilities: TerminalLocalFileCapabilities,
    session: Option<Box<dyn TerminalSessionHandle>>,
    session_start_attempted: bool,
    session_epoch: u64,
    accepted_screen_generation: Option<crate::terminal::PresentationGeneration>,
    accepted_directory_revision: Option<u64>,
    remote_connection_generation: Option<u64>,
    remote_input_blocked: bool,
    remote_restart_start_pending: bool,
    native_service_session_identity: u64,
    _event_task: Option<Task<()>>,
    _accessibility_task: Option<Task<()>>,
}

impl PaneSessionLifecycle {
    fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: Option<PreparedWorkspaceTerminalLaunch>,
    ) -> Self {
        Self {
            current_directory: prepared_launch
                .as_ref()
                .map(PreparedWorkspaceTerminalLaunch::starting_directory),
            local_file_capabilities: session_factory.local_file_capabilities(),
            session_factory,
            prepared_launch,
            session: None,
            session_start_attempted: false,
            session_epoch: 0,
            accepted_screen_generation: None,
            accepted_directory_revision: None,
            remote_connection_generation: None,
            remote_input_blocked: false,
            remote_restart_start_pending: false,
            native_service_session_identity: 0,
            _event_task: None,
            _accessibility_task: None,
        }
    }

    fn remote_facts(&self) -> RemotePaneFacts {
        RemotePaneFacts {
            remote: self.session_factory.is_remote(),
            generation: self.remote_connection_generation,
            disconnected: self.remote_input_blocked,
            epoch: self.session_epoch,
        }
    }

    fn retire_events(&mut self) {
        self.session_epoch = self.session_epoch.wrapping_add(1);
        self._event_task.take();
        self._accessibility_task.take();
    }

    fn suspend(&mut self) {
        self.remote_input_blocked = true;
        self.retire_events();
    }

    fn close(&mut self) {
        self.retire_events();
        if self.session.take().is_some() {
            self.native_service_session_identity =
                self.native_service_session_identity.wrapping_add(1);
        }
    }

    fn commit_restart(&mut self, prepared: PreparedRemotePaneRestart) {
        self.retire_events();
        self.session.take();
        self.native_service_session_identity = self.native_service_session_identity.wrapping_add(1);
        self.session_factory = prepared.session_factory;
        self.current_directory = Some(prepared.prepared_launch.starting_directory());
        self.prepared_launch = Some(prepared.prepared_launch);
        self.local_file_capabilities = self.session_factory.local_file_capabilities();
        self.session_start_attempted = false;
        self.remote_connection_generation = Some(prepared.authority.generation());
        self.remote_input_blocked = false;
        self.remote_restart_start_pending = true;
        self.accepted_screen_generation = None;
        self.accepted_directory_revision = None;
    }

    fn attach(
        &mut self,
        started: crate::terminal::StartedTerminalSession,
        cx: &mut Context<TerminalPane>,
    ) {
        self.native_service_session_identity = self.native_service_session_identity.wrapping_add(1);
        self.session = Some(started.handle);
        let receiver = started.events;
        let accessibility_receiver = started.accessibility;
        let session_epoch = self.session_epoch;
        self._event_task = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let mut events = vec![event];
                while let Ok(event) = receiver.try_recv() {
                    events.push(event);
                }
                if this
                    .update(cx, |this, cx| {
                        // Capture retained metadata before a final event can retire this epoch.
                        let previous_directory = this.current_directory();
                        let mut changed = this.sync_directory_metadata(session_epoch);
                        for event in events {
                            changed |= this.handle_session_event(session_epoch, event, cx);
                        }
                        changed |= this.sync_directory_metadata(session_epoch);
                        if previous_directory != this.current_directory() {
                            cx.emit(TerminalPaneEvent::CaptionChanged);
                        }
                        if changed && this.render_lifecycle.can_present() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        self._accessibility_task = Some(cx.spawn(async move |this, cx| {
            while let Ok(mut accessibility) = accessibility_receiver.recv().await {
                while let Ok(newer) = accessibility_receiver.try_recv() {
                    accessibility = newer;
                }
                if this
                    .update(cx, |this, cx| {
                        this.handle_session_accessibility(session_epoch, accessibility);
                        if this.render_lifecycle.can_present() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn start(
        &mut self,
        geometry: TerminalGeometry,
    ) -> Option<Result<crate::terminal::StartedTerminalSession, PaneSessionStartFailure>> {
        if self.session_start_attempted {
            return None;
        }
        self.session_start_attempted = true;
        let prepared = self.prepared_launch.take().or_else(|| {
            (!self.session_factory.is_remote())
                .then(|| self.session_factory.prepare_child_launch().ok())
                .flatten()
        });
        let Some(prepared) = prepared else {
            return Some(Err(PaneSessionStartFailure::Preparation));
        };
        let operation = if self.remote_restart_start_pending {
            PaneSessionStartFailure::RemoteRestart
        } else {
            PaneSessionStartFailure::Worker
        };
        let result = self
            .session_factory
            .start(geometry, prepared)
            .map_err(|_| operation);
        self.remote_restart_start_pending = false;
        Some(result)
    }
}

impl Drop for PaneSessionLifecycle {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) struct TerminalPane {
    terminal_session: PaneSessionLifecycle,
    native_service_focus_epoch: Cell<u64>,
    native_service_hierarchy_generation: u64,
    screen: Arc<ScreenSnapshot>,
    screen_session_epoch: u64,
    last_valid_screen: Arc<ScreenSnapshot>,
    last_valid_screen_session_epoch: u64,
    accessibility: Arc<TerminalAccessibilityModel>,
    pending_accessibility: Option<(u64, Arc<TerminalAccessibilityModel>)>,
    accessibility_element: Box<dyn TerminalAccessibilityAdapter>,
    pending_accessibility_notifications: AccessibilityNotifications,
    accessibility_needs_presentation: bool,
    render_lifecycle: RenderLifecycle,
    pane_state: PaneTerminalState,
    pending_recovery: Option<RecoveryToken>,
    recovery_retry_requested: Option<RecoveryToken>,
    state_revision: u64,
    next_operation_id: u64,
    latest_presentation_operation: Option<u64>,
    latest_export_operation: Option<u64>,
    #[cfg(test)]
    scene_submission_attempts: Vec<crate::terminal::PresentationGeneration>,
    diagnostics: DiagnosticBundle,
    status: Option<String>,
    fallback_title: SharedString,
    title: SharedString,
    focus_handle: FocusHandle,
    find_input: Option<Entity<TextInput>>,
    find_generation: FindQueryGeneration,
    product_focus: TerminalProductFocus,
    focus_coordinator: TerminalFocusCoordinator,
    terminal_input_focus: bool,
    surface_active: bool,
    application_active: bool,
    attention: AttentionState,
    attention_visual: bool,
    attention_generation: u64,
    native_attention_pane: Option<AttentionPaneId>,
    hidden_input: bool,
    secure_input_pane: SecureInputPane,
    lifecycle_dependencies: PaneLifecycleDependencies,
    operating_system_window_key: bool,
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    cell_width: Pixels,
    backing_scale: BackingScale,
    last_geometry: Option<TerminalGeometry>,
    grid_bounds: Option<Bounds<Pixels>>,
    pressed_button: Option<PointerButton>,
    selection_copy_pending: bool,
    pointer_modifiers: InputModifiers,
    shift_selection: ShiftSelectionPolicy,
    wheel_accumulator: WheelAccumulator,
    scrollbar: Entity<OverlayScrollbar<u64>>,
    render_cache: Entity<TerminalGridCache>,
    fallback_render_cache: Entity<TerminalGridCache>,
    grid_presentation: TerminalGridPresentation,
    #[cfg(test)]
    paint_fault: Option<PaintPreflightFault>,
    graphics_cache: Entity<TerminalGraphicsCache>,
    selection_pasteboard: SelectionPublication,
    file_insertion: crate::terminal::native_services::file_insertion::FileInsertionPolicy,
    file_clipboard: Rc<dyn FileClipboard>,
    key_input_adapter: Box<dyn TerminalKeyInputAdapter>,
    ime: TerminalIme,
    preedit_layout: Option<PreeditLayout>,
    preedit_layout_key: Option<PreeditLayoutKey>,
    marked_revision: u64,
    ime_suppressed_keys: Vec<PhysicalKey>,
    pending_file_insertion: Option<PastePayload>,
    pending_paste: Option<PasteConfirmation>,
    hovered_link: Option<HoveredTerminalLink>,
    pressed_link: Option<(
        crate::terminal::PresentationGeneration,
        crate::terminal::HyperlinkTarget,
    )>,
    file_preview: FilePreviewPresenter<Box<dyn FilePreviewPanel>>,
    context_menu: Option<TerminalContextMenuState>,
    blink_phase_visible: bool,
    blink_generation: u64,
    _blink_task: Option<Task<()>>,
    _attention_task: Option<Task<()>>,
    visibility_source: Option<Box<dyn WindowVisibilitySource>>,
    _visibility_task: Option<Task<()>>,
}

impl TerminalPane {
    #[cfg(test)]
    pub(crate) fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prepared_launch = session_factory.prepare_child_launch().ok();
        Self::new_with_services(
            session_factory,
            prepared_launch,
            crate::terminal::testing::test_terminal_key_input_adapter(),
            &crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
            crate::terminal::native_services::testing::adapters(),
            PaneLifecycleDependencies::testing(),
            window,
            cx,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit capability injection follows hierarchy ownership"
    )]
    pub(crate) fn new_with_prepared_launch(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        key_input_adapter: Box<dyn TerminalKeyInputAdapter>,
        accessibility_adapter_factory: &dyn TerminalAccessibilityAdapterFactory,
        native_service_adapters: NativeServiceAdapters,
        lifecycle_dependencies: PaneLifecycleDependencies,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_services(
            session_factory,
            Some(prepared_launch),
            key_input_adapter,
            accessibility_adapter_factory,
            native_service_adapters,
            lifecycle_dependencies,
            window,
            cx,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit capability injection follows hierarchy ownership"
    )]
    fn new_with_services(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: Option<PreparedWorkspaceTerminalLaunch>,
        key_input_adapter: Box<dyn TerminalKeyInputAdapter>,
        accessibility_adapter_factory: &dyn TerminalAccessibilityAdapterFactory,
        native_service_adapters: NativeServiceAdapters,
        lifecycle_dependencies: PaneLifecycleDependencies,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (visibility_sender, visibility_receiver) = async_channel::bounded(1);
        let visibility_source = lifecycle_dependencies.visibility.capture(
            window,
            Box::new(move || {
                let _ = visibility_sender.try_send(());
            }),
        );
        let visibility_task = cx.spawn_in(window, async move |this, cx| {
            while visibility_receiver.recv().await.is_ok() {
                while visibility_receiver.try_recv().is_ok() {}
                if this
                    .update_in(cx, |pane, window, cx| {
                        pane.refresh_surface(window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let focus_handle = cx.focus_handle();
        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family, DEFAULT_FONT_SIZE);
        let backing_scale = BackingScale::new(window.scale_factor()).unwrap_or(BackingScale::ONE);
        let fallback_title: SharedString =
            normalized_pane_title("", &session_factory.fallback_title()).into();
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("terminal-scrollbar"));
        let render_cache = cx.new(|_| TerminalGridCache::new());
        let fallback_render_cache = cx.new(|_| TerminalGridCache::new());
        let graphics_cache = cx.new(|_| TerminalGraphicsCache::default());
        cx.on_release(|pane, cx| {
            pane.close();
            pane.graphics_cache.update(cx, |cache, cx| cache.clear(cx));
        })
        .detach();
        let screen = ScreenSnapshot::empty(native_service_adapters.file_insertion.paths);
        let accessibility = Arc::new(TerminalAccessibilityModel::from_screen(&screen));
        let accessibility_element = accessibility_adapter_factory.create(
            window,
            accessibility.as_ref().clone(),
            font_family.as_ref(),
            px(DEFAULT_FONT_SIZE),
        );
        let mut render_lifecycle = RenderLifecycle::new(SurfaceVisibility {
            application_active: false,
            key_window: false,
            minimized: false,
            occluded: true,
            live_resize: false,
            workspace_visible: false,
            pane_visible: false,
        });
        let _ = render_lifecycle.update_scale(window.scale_factor());
        cx.subscribe_in(
            &scrollbar,
            window,
            |pane, _, event: &OverlayScrollbarEvent<u64>, window, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => {
                    pane.focus(window);
                    cx.emit(TerminalPaneEvent::FocusRequested);
                }
                OverlayScrollbarEvent::OffsetRequested(rows) => {
                    if let Some(session) = &pane.terminal_session.session {
                        session.scroll_to(*rows, pane.screen.generation);
                    }
                }
            },
        )
        .detach();
        cx.observe_window_bounds(window, |pane, window, cx| {
            pane.update_backing_scale(window.scale_factor(), window, cx);
        })
        .detach();
        cx.observe_window_activation(window, |pane, window, cx| {
            pane.refresh_surface(window, cx);
            cx.notify();
        })
        .detach();
        cx.on_focus(&focus_handle, window, |pane, window, cx| {
            pane.refresh_surface(window, cx);
            cx.notify();
        })
        .detach();
        cx.on_blur(&focus_handle, window, |pane, window, cx| {
            pane.refresh_surface(window, cx);
            cx.notify();
        })
        .detach();

        Self {
            terminal_session: PaneSessionLifecycle::new(session_factory, prepared_launch),
            native_service_focus_epoch: Cell::new(0),
            native_service_hierarchy_generation: 0,
            screen_session_epoch: 0,
            last_valid_screen: Arc::clone(&screen),
            last_valid_screen_session_epoch: 0,
            screen,
            accessibility,
            pending_accessibility: None,
            accessibility_element,
            pending_accessibility_notifications: AccessibilityNotifications::default(),
            accessibility_needs_presentation: false,
            render_lifecycle,
            pane_state: PaneTerminalState::default(),
            pending_recovery: None,
            recovery_retry_requested: None,
            state_revision: 0,
            next_operation_id: 0,
            latest_presentation_operation: None,
            latest_export_operation: None,
            #[cfg(test)]
            scene_submission_attempts: Vec::new(),
            diagnostics: DiagnosticBundle::default(),
            status: None,
            title: fallback_title.clone(),
            fallback_title,
            focus_handle,
            find_input: None,
            find_generation: FindQueryGeneration::default(),
            product_focus: TerminalProductFocus::default(),
            focus_coordinator: TerminalFocusCoordinator::default(),
            terminal_input_focus: false,
            surface_active: false,
            application_active: false,
            attention: AttentionState::default(),
            attention_visual: false,
            attention_generation: 0,
            native_attention_pane: Some(lifecycle_dependencies.attention.register_pane()),
            hidden_input: false,
            secure_input_pane: lifecycle_dependencies.secure_input.register_pane(),
            lifecycle_dependencies,
            operating_system_window_key: window.is_window_active(),
            font_family,
            font_size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
            cell_width,
            backing_scale,
            last_geometry: None,
            grid_bounds: None,
            pressed_button: None,
            selection_copy_pending: false,
            pointer_modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
            wheel_accumulator: WheelAccumulator::default(),
            scrollbar,
            render_cache,
            fallback_render_cache,
            grid_presentation: TerminalGridPresentation::new(),
            #[cfg(test)]
            paint_fault: None,
            graphics_cache,
            selection_pasteboard: SelectionPublication::new(
                native_service_adapters.selection_clipboard,
            ),
            file_insertion: native_service_adapters.file_insertion,
            file_clipboard: native_service_adapters.file_clipboard,
            key_input_adapter,
            ime: TerminalIme::default(),
            preedit_layout: None,
            preedit_layout_key: None,
            marked_revision: 0,
            ime_suppressed_keys: Vec::new(),
            pending_file_insertion: None,
            pending_paste: None,
            hovered_link: None,
            pressed_link: None,
            file_preview: FilePreviewPresenter::new(native_service_adapters.file_preview.create()),
            context_menu: None,
            blink_phase_visible: true,
            blink_generation: 0,
            _blink_task: None,
            _attention_task: None,
            visibility_source,
            _visibility_task: Some(visibility_task),
        }
    }

    pub(crate) fn focus(&self, window: &mut Window) {
        self.advance_native_service_focus_epoch();
        self.focus_handle.focus(window);
    }

    fn focus_find(&mut self, window: &mut Window, cx: &App) {
        let Some(input) = &self.find_input else {
            return;
        };
        input.read(cx).focus_handle().focus(window);
    }

    fn advance_native_service_focus_epoch(&self) {
        self.native_service_focus_epoch
            .set(self.native_service_focus_epoch.get().wrapping_add(1));
    }

    pub(crate) fn set_product_focus(
        &mut self,
        product_focus: TerminalProductFocus,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.product_focus == product_focus {
            return false;
        }
        self.native_service_hierarchy_generation =
            self.native_service_hierarchy_generation.wrapping_add(1);
        if self.product_focus.focused_pane && !product_focus.focused_pane {
            self.end_find_state();
        }
        let native_service_blocked = !product_focus.active_workspace
            || !product_focus.active_tab
            || !product_focus.focused_pane
            || product_focus.blocker.is_some();
        let pane_inactive = !product_focus.active_workspace
            || !product_focus.active_tab
            || !product_focus.focused_pane;
        if pane_inactive {
            self.file_preview.dismiss();
        }
        if native_service_blocked {
            self.pending_file_insertion = None;
            self.context_menu = None;
        }
        if native_service_blocked
            && let Some(confirmation) = self.pending_paste.take()
            && let Some(session) = &self.terminal_session.session
        {
            let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
        }
        let was_presentable = self.render_lifecycle.can_present();
        self.product_focus = product_focus;
        let pane_visible = product_focus.active_tab && product_focus.pane_visible;
        let _ = self
            .render_lifecycle
            .update_product_visibility(product_focus.active_workspace, pane_visible);
        self.sync_session_presentability(was_presentable);
        if was_presentable && !self.render_lifecycle.can_present() {
            // Hidden Tabs and zoomed-out Panes leave the render tree, so cleanup
            // must happen at the visibility transition without another render.
            self.evict_presentation_resources(cx);
        }
        if !self.render_lifecycle.effects().animations_active {
            self.stop_surface_animations();
        }
        if native_service_blocked {
            self.apply_terminal_input_focus(false);
        }
        true
    }

    pub(crate) fn synchronize_native_service_hierarchy_generation(&mut self, generation: u64) {
        self.native_service_hierarchy_generation = generation;
    }

    pub(crate) fn set_accessibility_hierarchy(&mut self, presented: bool, order: usize) {
        self.accessibility_element.set_hierarchy(presented, order);
    }

    fn open_find(&mut self, _: &OpenTerminalFind, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.find_input {
            input.update(cx, |input, cx| input.select_all(cx));
        } else {
            let input = cx.new(|cx| {
                TextInput::new("terminal-find-input", "Terminal Find query", "", window, cx)
                    .placeholder("Find")
                    .variant(TextInputVariant::Bare)
                    .tab_behavior(TextInputTabBehavior::Propagate)
                    .debug_selector("terminal-find-input")
            });
            let input_id = input.entity_id();
            cx.subscribe_in(
                &input,
                window,
                move |pane, input, event: &TextInputEvent, window, cx| {
                    let is_current = input.entity_id() == input_id
                        && pane
                            .find_input
                            .as_ref()
                            .is_some_and(|current| current.entity_id() == input_id);
                    if !is_current {
                        return;
                    }
                    match event {
                        TextInputEvent::ValueChanged(_) => {
                            pane.find_query_changed(input.read(cx).value().to_owned());
                        }
                        TextInputEvent::Submitted => {
                            pane.find_next(&FindNext, window, cx);
                        }
                        TextInputEvent::Cancelled | TextInputEvent::CompositionCancelled => {
                            cx.defer_in(window, move |pane, window, cx| {
                                if pane
                                    .find_input
                                    .as_ref()
                                    .is_some_and(|current| current.entity_id() == input_id)
                                {
                                    pane.close_find(&CloseTerminalFind, window, cx);
                                }
                            });
                        }
                        TextInputEvent::FocusGained => {
                            pane.advance_native_service_focus_epoch();
                            let _ = pane.sync_terminal_input_focus(window, cx);
                            cx.notify();
                        }
                        TextInputEvent::TabForwardRequested => window.focus_next(),
                        TextInputEvent::TabBackwardRequested => window.focus_prev(),
                        TextInputEvent::FocusLost
                        | TextInputEvent::CompositionStarted
                        | TextInputEvent::CompositionCommitted
                        | TextInputEvent::ContextMenuOpened
                        | TextInputEvent::ContextMenuClosed => {}
                    }
                },
            )
            .detach();
            self.find_input = Some(input);
            self.find_query_changed(String::new());
        }
        self.focus_find(window, cx);
        cx.notify();
    }

    fn find_next(&mut self, _: &FindNext, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Some(session) = &self.terminal_session.session
            && self.find_input.is_some()
        {
            session.navigate_find(self.find_generation, FindDirection::Next);
        }
    }

    fn find_previous(&mut self, _: &FindPrevious, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Some(session) = &self.terminal_session.session
            && self.find_input.is_some()
        {
            session.navigate_find(self.find_generation, FindDirection::Previous);
        }
    }

    fn close_find(&mut self, _: &CloseTerminalFind, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_input.is_none() {
            return;
        }
        self.end_find_state();
        self.advance_native_service_focus_epoch();
        self.focus_handle.focus(window);
        let _ = self.sync_terminal_input_focus(window, cx);
        cx.notify();
    }

    fn end_find_state(&mut self) {
        if self.find_input.take().is_none() {
            return;
        }
        self.find_generation = self.find_generation.next();
        if let Some(session) = &self.terminal_session.session {
            session.end_find(self.find_generation);
        }
    }

    fn find_query_changed(&mut self, query: String) {
        if self.find_input.is_none() {
            return;
        }
        self.find_generation = self.find_generation.next();
        if let Some(session) = &self.terminal_session.session {
            session.set_find_query(self.find_generation, query);
        }
    }

    fn current_activity(&self, window: &Window, cx: &App) -> SurfaceActivity {
        SurfaceActivity {
            application_active: self.lifecycle_dependencies.activity.is_active(cx),
            operating_system_window_key: window.is_window_active(),
        }
    }

    fn update_application_activity(&mut self, activity: SurfaceActivity, cx: &mut Context<Self>) {
        self.application_active = activity.application_active;
        self.operating_system_window_key = activity.operating_system_window_key;
        let runtime = &self.lifecycle_dependencies.attention;
        let schedules =
            runtime.update_application_activation(activity.application_active, Instant::now());
        runtime.schedule(schedules, cx);
        self.lifecycle_dependencies
            .secure_input
            .update_application_activation(activity.application_active);
    }

    fn apply_attention(
        &self,
        pane: AttentionPaneId,
        effects: crate::terminal::attention::AttentionEffects,
        cx: &mut Context<Self>,
    ) {
        let runtime = &self.lifecycle_dependencies.attention;
        let schedules = runtime.apply(pane, effects, Instant::now());
        runtime.schedule(schedules, cx);
    }

    fn refresh_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native_attention_pane.is_none() {
            return;
        }
        let activity = self.current_activity(window, cx);
        self.update_application_activity(activity, cx);
        let (_, focus_gained) = self.sync_terminal_input_focus(window, cx);
        self.surface_active = terminal_surface_active(self.product_focus, activity);
        if focus_gained {
            self.clear_attention(cx);
        }
        if let Some(source) = &self.visibility_source {
            self.update_runtime_visibility(source.current(), cx);
        }
    }

    pub(crate) fn terminal_input_focused(&self, window: &Window, cx: &App) -> bool {
        self.terminal_input_focused_with_activity(
            window,
            self.current_activity(window, cx),
            window_modal_is_open(window, cx),
        )
    }

    fn terminal_input_focused_with_activity(
        &self,
        window: &Window,
        activity: SurfaceActivity,
        modal_open: bool,
    ) -> bool {
        if self.terminal_session.remote_input_blocked || self.native_attention_pane.is_none() {
            return false;
        }
        TerminalFocusCoordinator::is_focused(TerminalFocusFacts {
            active_workspace: self.product_focus.active_workspace,
            active_tab: self.product_focus.active_tab,
            focused_pane: self.product_focus.focused_pane,
            responder: self.focus_handle.is_focused(window),
            operating_system_window_key: activity.operating_system_window_key,
            application_active: activity.application_active,
            blocker: self.focus_coordinator.pane_blocker(
                self.product_focus.blocker,
                modal_open,
                self.context_menu.is_some(),
            ),
        })
    }

    fn sync_terminal_input_focus(&mut self, window: &Window, cx: &App) -> (bool, bool) {
        self.sync_terminal_input_focus_with_activity_and_modal(
            window,
            self.current_activity(window, cx),
            window_modal_is_open(window, cx),
        )
    }

    fn sync_terminal_input_focus_with_activity_and_modal(
        &mut self,
        window: &Window,
        activity: SurfaceActivity,
        modal_open: bool,
    ) -> (bool, bool) {
        self.lifecycle_dependencies
            .secure_input
            .update_application_activation(activity.application_active);
        let focused = self.terminal_input_focused_with_activity(window, activity, modal_open);
        let focus_gained = !self.terminal_input_focus && focused;
        self.apply_terminal_input_focus(focused);
        (focused, focus_gained)
    }

    pub(crate) fn synchronize_terminal_input_focus(&mut self, window: &Window, cx: &App) -> bool {
        self.sync_terminal_input_focus(window, cx).0
    }

    fn apply_terminal_input_focus(&mut self, focused: bool) {
        if self.terminal_input_focus != focused {
            self.terminal_input_focus = focused;
            self.advance_native_service_focus_epoch();
            if !focused {
                self.key_input_adapter.reset();
            }
            if focused {
                self.pending_accessibility_notifications
                    .insert(AccessibilityNotification::Focus);
            }
            self.reset_blink_phase();
            if !focused {
                if let Some(confirmation) = self.pending_paste.take()
                    && let Some(session) = &self.terminal_session.session
                {
                    let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
                }
                self.ime.cancel();
                self.invalidate_preedit_layout();
                self.ime_suppressed_keys.clear();
            }
            if let Some(session) = &self.terminal_session.session {
                session.focus(focused);
            }
            self.sync_secure_input();
        }
    }

    fn clear_attention(&mut self, cx: &mut Context<Self>) {
        if self.attention.unread_count() == 0 && !self.attention.visual_bell() {
            return;
        }
        let effects = self.attention.clear();
        self.attention_visual = false;
        self.attention_generation = self.attention_generation.wrapping_add(1);
        self._attention_task.take();
        if let Some(pane) = self.native_attention_pane {
            self.apply_attention(pane, effects, cx);
        }
        cx.emit(TerminalPaneEvent::AttentionChanged { unread_count: 0 });
    }

    fn start_visual_bell(&mut self, cx: &mut Context<Self>) {
        if !self.render_lifecycle.effects().animations_active {
            return;
        }
        self.attention_generation = self.attention_generation.wrapping_add(1);
        let generation = self.attention_generation;
        self.attention_visual = true;
        self._attention_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VISUAL_BELL_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                if this.attention_generation == generation {
                    this.attention_visual = false;
                    this._attention_task.take();
                    cx.notify();
                }
            });
        }));
    }

    fn sync_secure_input(&self) {
        self.secure_input_pane
            .update(self.hidden_input, self.terminal_input_focus);
    }

    fn reset_hidden_input(&mut self) {
        self.hidden_input = false;
        self.sync_secure_input();
    }

    fn preedit_layout(&mut self) -> Option<PreeditLayout> {
        let Some(text) = self.ime.marked_text() else {
            self.preedit_layout = None;
            self.preedit_layout_key = None;
            return None;
        };
        let Some(position) = self.screen.cursor.position else {
            self.preedit_layout = None;
            self.preedit_layout_key = None;
            return None;
        };
        let Some(columns) = self.screen.rows.first().map(|row| row.len()) else {
            self.preedit_layout = None;
            self.preedit_layout_key = None;
            return None;
        };
        let caret_utf16 = self.ime.selected_range().end;
        let key = PreeditLayoutKey {
            marked_revision: self.marked_revision,
            start_row: usize::from(position.row),
            start_column: usize::from(position.column),
            columns,
            caret_utf16,
        };
        if self.preedit_layout_key.as_ref() != Some(&key) {
            self.preedit_layout = Some(layout_preedit(
                text,
                key.start_row,
                key.start_column,
                columns,
                caret_utf16,
            ));
            self.preedit_layout_key = Some(key);
        }
        self.preedit_layout.clone()
    }

    #[cfg(test)]
    fn mark_for_preedit_cache_test(&mut self, text: &str, selected_utf16: Range<usize>) {
        self.ime.replace_and_mark(None, text, Some(selected_utf16));
        self.invalidate_preedit_layout();
    }

    fn invalidate_preedit_layout(&mut self) {
        self.marked_revision = self.marked_revision.wrapping_add(1);
        self.preedit_layout = None;
        self.preedit_layout_key = None;
    }

    pub(crate) fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub(crate) fn current_directory(&self) -> Option<crate::terminal::metadata::CurrentDirectory> {
        self.terminal_session.current_directory.clone()
    }

    pub(crate) fn caption(&self) -> PaneCaptionFacts {
        use crate::terminal::metadata::{CommandState, TitleProvenance, sanitize_title};

        let metadata = &self.screen.metadata;
        let directory = match self.current_directory() {
            Some(crate::domain::CurrentDirectory::Local(path)) => {
                sanitize_title(&path.to_string_lossy())
            }
            Some(crate::domain::CurrentDirectory::Remote(path)) => sanitize_title(path.as_str()),
            None => sanitize_title(&metadata.directory.path),
        };
        let running = metadata
            .command
            .as_ref()
            .filter(|command| command.state == CommandState::Running);
        let label = if metadata.title.provenance == TitleProvenance::TerminalControl {
            sanitize_title(&metadata.title.value)
        } else if let Some(command) = running {
            sanitize_title(&command.line)
        } else {
            normalized_pane_title("", &self.fallback_title)
        };
        PaneCaptionFacts {
            origin: PaneOrigin::from_context(&metadata.context),
            directory: compact_home_directory(&directory, metadata.context.local_home()).into(),
            label: label.into(),
            running: running.is_some(),
        }
    }

    pub(crate) fn close_facts(&self) -> PaneCloseFacts<'_> {
        PaneCloseFacts {
            live_session: self.terminal_session.session.is_some(),
            state: &self.pane_state,
            disconnected: self.terminal_session.remote_input_blocked,
            metadata: &self.screen.metadata,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    #[cfg(test)]
    pub(crate) const fn font_size(&self) -> f32 {
        self.font_size
    }

    #[cfg(test)]
    pub(crate) const fn remote_session_state(&self) -> (bool, bool) {
        (
            self.terminal_session.remote_input_blocked,
            self.terminal_session.session.is_some(),
        )
    }

    #[cfg(test)]
    pub(crate) fn restart_state(&self) -> (bool, Option<&'static str>) {
        (
            self.terminal_session.session.is_some(),
            self.pane_state
                .failure()
                .map(crate::terminal::TerminalFailure::operation),
        )
    }

    pub(crate) fn close(&mut self) {
        if self.native_attention_pane.is_none() {
            return;
        }
        self.end_find_state();
        if let Some(confirmation) = self.pending_paste.take()
            && let Some(session) = &self.terminal_session.session
        {
            let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
        }
        self.blink_generation = self.blink_generation.wrapping_add(1);
        self.attention_generation = self.attention_generation.wrapping_add(1);
        self._blink_task.take();
        self._attention_task.take();
        self.terminal_session.retire_events();
        self._visibility_task.take();
        self.secure_input_pane.retire();
        if let Some(id) = self.native_attention_pane.take() {
            self.lifecycle_dependencies.attention.remove_pane(id);
        }
        self.render_lifecycle.release();
        self.visibility_source.take();
        self.context_menu = None;
        self.file_preview.dismiss();
        self.accessibility_element.set_hierarchy(false, usize::MAX);
        self.terminal_session.close();
    }

    fn validate_remote_generation(&self, generation: u64) -> Result<(), RemotePaneLifecycleError> {
        self.terminal_session
            .remote_facts()
            .validate_generation(generation)
    }

    /// Validates that `generation` may disconnect this Remote Pane without mutating it.
    pub(crate) fn can_disconnect_remote(
        &self,
        generation: u64,
    ) -> Result<(), RemotePaneLifecycleError> {
        self.validate_remote_generation(generation)
    }

    /// Suspends a Remote Pane for an authoritative Control Connection loss.
    ///
    /// The final screen, title, selection, and Find presentation remain owned by the Pane. Terminal
    /// input is blocked, prior-session event tasks are retired, and repeated notification for the
    /// same generation is idempotent. Stale generations and Local Panes are rejected unchanged.
    pub(crate) fn disconnect_remote(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneLifecycleError> {
        self.validate_remote_generation(generation)?;
        if self.terminal_session.remote_connection_generation == Some(generation)
            && self.terminal_session.remote_input_blocked
        {
            return Ok(());
        }
        self.terminal_session.remote_connection_generation = Some(generation);
        self.suspend_remote_session(cx);
        Ok(())
    }

    fn suspend_remote_session(&mut self, cx: &mut Context<Self>) {
        self.sync_directory_metadata(self.terminal_session.session_epoch);
        self.terminal_session.suspend();
        self.reset_hidden_input();
        self.apply_terminal_input_focus(false);
        self.context_menu = None;
        self.pressed_button = None;
        self.selection_copy_pending = false;
        self.pressed_link = None;
        cx.notify();
    }

    fn suspend_if_remote_channel_unavailable(&mut self, cx: &mut Context<Self>) -> bool {
        if self
            .terminal_session
            .session_factory
            .remote_channel_is_ready()
            != Some(false)
        {
            return false;
        }
        self.suspend_remote_session(cx);
        true
    }

    /// Binds one prepared channel to this disconnected Pane without mutating its session.
    ///
    /// The generation must advance and the token captures the current session epoch so delayed
    /// preparation cannot replace a successor. Dropping the token abandons the reserved launch.
    pub(crate) fn prepare_remote_restart(
        &self,
        session_factory: WorkspaceTerminalSessionFactory,
        generation: u64,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
    ) -> Result<PreparedRemotePaneRestart, RemotePaneLifecycleError> {
        let authority =
            RemoteRestartAuthority::prepare(self.terminal_session.remote_facts(), generation)?;
        if !session_factory.is_remote() {
            return Err(RemotePaneLifecycleError::LocalPane);
        }
        Ok(PreparedRemotePaneRestart {
            session_factory,
            prepared_launch,
            authority,
        })
    }

    pub(crate) fn can_commit_remote_restart(
        &self,
        prepared: &PreparedRemotePaneRestart,
    ) -> Result<(), RemotePaneLifecycleError> {
        prepared
            .authority
            .validate(self.terminal_session.remote_facts())
    }

    /// Commits a prevalidated successor Terminal Session in the existing Pane entity.
    ///
    /// The Pane and layout identities remain unchanged. Prior-session event and accessibility
    /// tasks are retired, generation caches reset, and the retained presentation remains visible
    /// until the successor publishes its first snapshot. Later startup failure is Pane-local.
    pub(crate) fn commit_remote_restart(
        &mut self,
        prepared: PreparedRemotePaneRestart,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneLifecycleError> {
        self.can_commit_remote_restart(&prepared)?;
        let restart_geometry = self.last_geometry;
        self.terminal_session.commit_restart(prepared);
        self.fallback_title =
            normalized_pane_title("", &self.terminal_session.session_factory.fallback_title())
                .into();
        self.reset_hidden_input();
        self.render_lifecycle.reset_session_presentations();
        self.pending_accessibility_notifications
            .insert(AccessibilityNotification::Value);
        self.pending_accessibility_notifications
            .insert(AccessibilityNotification::Selection);
        self.last_geometry = restart_geometry;
        self.pane_state = PaneTerminalState::Running;
        self.pending_recovery = None;
        self.recovery_retry_requested = None;
        self.status = None;
        self.state_revision = self.state_revision.wrapping_add(1);
        if let Some(geometry) = restart_geometry {
            self.start_session(geometry, cx);
        }
        let _ = self.sync_terminal_input_focus(window, cx);
        cx.notify();
        Ok(())
    }

    fn stop_surface_animations(&mut self) {
        self.reset_blink_phase();
        self.attention_generation = self.attention_generation.wrapping_add(1);
        self._attention_task.take();
        self.attention_visual = false;
    }

    fn reset_blink_phase(&mut self) {
        self.blink_generation = self.blink_generation.wrapping_add(1);
        self._blink_task.take();
        self.blink_phase_visible = true;
    }

    fn sync_presentation_blink(
        &mut self,
        surface_active: bool,
        terminal_input_focused: bool,
        cx: &mut Context<Self>,
    ) {
        let cursor_demanded =
            terminal_input_focused && self.screen.cursor.visible && self.screen.cursor.blinking;
        let demanded = surface_active && (self.screen.text_blinking || cursor_demanded);
        if demanded == self._blink_task.is_some() {
            return;
        }

        self.reset_blink_phase();
        if !demanded {
            return;
        }

        let generation = self.blink_generation;
        self._blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(PRESENTATION_BLINK_INTERVAL)
                    .await;
                let Ok(continue_blinking) = this.update(cx, |this, cx| {
                    if this.blink_generation != generation
                        || !this.render_lifecycle.effects().animations_active
                    {
                        return false;
                    }
                    this.blink_phase_visible = !this.blink_phase_visible;
                    cx.notify();
                    true
                }) else {
                    break;
                };
                if !continue_blinking {
                    break;
                }
            }
        }));
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<u64>> {
        let size = self.last_geometry?.grid();
        ScrollMetrics::for_rows(
            TOP_PADDING,
            f32::from(size.rows) * self.line_height,
            self.screen.scrollbar.total_rows,
            self.screen.scrollbar.visible_rows,
            self.screen.scrollbar.offset_rows,
        )
    }

    fn sync_scrollbar(&self, cx: &mut Context<Self>) {
        if !self.render_lifecycle.can_present() {
            return;
        }
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    fn last_valid_presentation(&self) -> crate::terminal::PresentationGeneration {
        self.last_valid_screen.generation
    }

    fn present_failure(
        &mut self,
        failure: TerminalFailure,
        preserve_frame: bool,
        recovery: Option<RecoveryAction>,
    ) -> bool {
        self.present_failure_at(failure, preserve_frame, recovery, self.screen.generation)
    }

    fn present_failure_at(
        &mut self,
        failure: TerminalFailure,
        preserve_frame: bool,
        recovery: Option<RecoveryAction>,
        generation: crate::terminal::PresentationGeneration,
    ) -> bool {
        if matches!(self.pane_state, PaneTerminalState::Exited(_))
            || self
                .pane_state
                .failure()
                .is_some_and(TerminalFailure::is_fatal)
        {
            return false;
        }
        let state = PaneTerminalState::failed(
            failure.clone(),
            preserve_frame.then(|| self.last_valid_presentation()),
        );
        if self.pane_state == state
            && self.pending_recovery.is_some_and(|pending| {
                recovery == Some(pending.action) && generation == pending.generation
            })
        {
            return false;
        }
        self.state_revision = self.state_revision.wrapping_add(1);
        self.diagnostics.record(&failure);
        self.pane_state = state;
        self.pending_recovery = recovery.map(|action| RecoveryToken {
            revision: self.state_revision,
            action,
            generation,
        });
        self.recovery_retry_requested = None;
        self.status = None;
        true
    }

    fn clear_recovery(&mut self, expected: RecoveryToken) -> bool {
        if self.pending_recovery != Some(expected)
            || self.state_revision != expected.revision
            || self
                .pane_state
                .failure()
                .is_none_or(TerminalFailure::is_fatal)
        {
            return false;
        }
        self.state_revision = self.state_revision.wrapping_add(1);
        self.pending_recovery = None;
        self.recovery_retry_requested = None;
        self.pane_state = PaneTerminalState::Running;
        self.status = None;
        true
    }

    fn begin_operation(
        &mut self,
        generation: crate::terminal::PresentationGeneration,
        recovery: Option<RecoveryToken>,
    ) -> OperationToken {
        self.next_operation_id = self.next_operation_id.wrapping_add(1);
        OperationToken {
            id: self.next_operation_id,
            state_revision: self.state_revision,
            generation,
            recovery,
        }
    }

    fn operation_is_current(&self, operation: OperationToken, latest: Option<u64>) -> bool {
        latest == Some(operation.id)
            && self.state_revision == operation.state_revision
            && operation
                .recovery
                .is_none_or(|recovery| self.pending_recovery == Some(recovery))
    }

    fn authoritative_status(&self) -> Option<String> {
        match &self.pane_state {
            PaneTerminalState::Running => self.status.clone(),
            PaneTerminalState::Exited(exit) => Some(exit.to_string()),
            PaneTerminalState::Failed { failure, .. } => Some(failure.to_string()),
        }
    }

    pub(super) fn record_scene_submission_attempt(
        &mut self,
        generation: crate::terminal::PresentationGeneration,
    ) {
        #[cfg(test)]
        self.scene_submission_attempts.push(generation);
        #[cfg(not(test))]
        let _ = generation;
    }

    pub(super) fn presentation_succeeded(
        &mut self,
        operation: OperationToken,
        graphics_attempt: GraphicsAttemptToken,
        screen: Arc<ScreenSnapshot>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let generation = screen.generation;
        if operation.generation != generation
            || !self.operation_is_current(operation, self.latest_presentation_operation)
        {
            return;
        }
        let graphics_presented = self
            .graphics_cache
            .update(cx, |cache, cx| cache.mark_presented(graphics_attempt, cx));
        if !graphics_presented {
            self.renderer_resource_failed(operation, graphics_attempt, cx);
            return;
        }
        self.render_lifecycle.mark_presented(generation);
        self.record_successfully_presented_screen(screen);
        if let Some(recovery) = operation.recovery
            && generation >= recovery.generation
            && matches!(
                recovery.action,
                RecoveryAction::Presentation | RecoveryAction::RendererResources
            )
            && self.clear_recovery(recovery)
        {
            cx.notify();
        }
    }

    fn record_successfully_presented_screen(&mut self, screen: Arc<ScreenSnapshot>) {
        if !Arc::ptr_eq(&screen, &self.screen)
            || self.screen_session_epoch != self.terminal_session.session_epoch
        {
            return;
        }
        if self.last_valid_screen_session_epoch != self.terminal_session.session_epoch
            || screen.generation >= self.last_valid_screen.generation
        {
            self.last_valid_screen = screen;
            self.last_valid_screen_session_epoch = self.terminal_session.session_epoch;
        }
    }

    pub(super) fn presentation_failed(
        &mut self,
        operation: OperationToken,
        graphics_attempt: GraphicsAttemptToken,
        cx: &mut Context<Self>,
    ) {
        self.graphics_cache.update(cx, |cache, cx| {
            cache.rollback(graphics_attempt, None, cx);
        });
        if !self.operation_is_current(operation, self.latest_presentation_operation) {
            return;
        }
        if self.present_failure_at(
            TerminalFailure::presentation("paint-terminal-presentation"),
            true,
            Some(RecoveryAction::Presentation),
            operation.generation,
        ) {
            cx.notify();
        }
    }

    pub(super) fn renderer_resource_failed(
        &mut self,
        operation: OperationToken,
        graphics_attempt: GraphicsAttemptToken,
        cx: &mut Context<Self>,
    ) {
        self.graphics_cache.update(cx, |cache, cx| {
            cache.rollback(graphics_attempt, None, cx);
        });
        if !self.operation_is_current(operation, self.latest_presentation_operation) {
            return;
        }
        if self.present_failure_at(
            TerminalFailure::resource("paint-terminal-graphics"),
            true,
            Some(RecoveryAction::RendererResources),
            operation.generation,
        ) {
            cx.notify();
        }
    }

    fn retry_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_recovery else {
            return;
        };
        if self.recovery_retry_requested == Some(pending) {
            return;
        }
        match pending.action {
            RecoveryAction::Presentation => {
                self.recovery_retry_requested = Some(pending);
                self.retry_presentation(window, cx);
            }
            RecoveryAction::RendererResources => {
                self.recovery_retry_requested = Some(pending);
                cx.notify();
            }
            RecoveryAction::StartSession => {
                self.recovery_retry_requested = Some(pending);
                self.terminal_session.session_start_attempted = false;
                self.last_geometry = None;
                cx.notify();
            }
            RecoveryAction::CopySelection => {
                self.copy_selection_with_recovery(Some(pending), window, cx);
            }
            RecoveryAction::ExportDiagnostics => {
                self.export_diagnostics_with_recovery(Some(pending), window, cx);
            }
        }
    }

    fn reveal_scrollbar(&self, cx: &mut Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }

    fn update_grid_bounds(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let geometry = terminal_geometry(
            bounds,
            self.cell_width,
            self.line_height,
            self.backing_scale,
        );
        self.grid_bounds = Some(terminal_grid_content_bounds(
            bounds,
            usize::from(geometry.grid().cols),
            self.cell_width,
        ));
        if self.last_geometry == Some(geometry) {
            return;
        }
        self.last_geometry = Some(geometry);
        self.sync_scrollbar(cx);

        if let Some(session) = &self.terminal_session.session {
            session.resize(geometry);
            return;
        }

        self.start_session(geometry, cx);
    }

    fn start_session(&mut self, geometry: TerminalGeometry, cx: &mut Context<Self>) {
        let Some(result) = self.terminal_session.start(geometry) else {
            return;
        };
        match result {
            Ok(started) => {
                if self
                    .recovery_retry_requested
                    .filter(|recovery| recovery.action == RecoveryAction::StartSession)
                    .is_some_and(|recovery| self.clear_recovery(recovery))
                {
                    cx.notify();
                }
                started.handle.focus(self.terminal_input_focus);
                started
                    .handle
                    .set_presentable(self.render_lifecycle.can_present());
                if let Some(input) = &self.find_input {
                    started
                        .handle
                        .set_find_query(self.find_generation, input.read(cx).value().to_owned());
                }
                self.terminal_session.attach(started, cx);
                self.flush_pending_file_insertion(cx);
            }
            Err(failure) => {
                self.present_failure(
                    failure.terminal_failure(),
                    failure.preserves_frame(self.terminal_session.session_factory.is_remote()),
                    Some(RecoveryAction::StartSession),
                );
                cx.notify();
            }
        }
    }

    fn update_runtime_visibility(&mut self, native: WindowVisibility, cx: &mut Context<Self>) {
        let was_presentable = self.render_lifecycle.can_present();
        let surface = SurfaceVisibility {
            application_active: self.application_active,
            key_window: self.operating_system_window_key,
            minimized: native.minimized,
            occluded: native.occluded,
            live_resize: native.live_resize,
            workspace_visible: self.product_focus.active_workspace,
            pane_visible: self.product_focus.active_tab && self.product_focus.pane_visible,
        };
        let effects = self.render_lifecycle.update_visibility(surface);
        self.sync_session_presentability(was_presentable);
        if was_presentable && !self.render_lifecycle.can_present() {
            self.evict_presentation_resources(cx);
        }
        if !effects.animations_active {
            self.stop_surface_animations();
        }
        self.sync_presentation_blink(effects.animations_active, self.terminal_input_focus, cx);
        let accessibility_restored = !was_presentable
            && self.render_lifecycle.can_present()
            && (self.accessibility_needs_presentation
                || !self.pending_accessibility_notifications.is_empty());
        if effects.request_redraw || accessibility_restored {
            cx.notify();
        }
    }

    fn sync_session_presentability(&self, was_presentable: bool) {
        let presentable = self.render_lifecycle.can_present();
        if was_presentable != presentable
            && let Some(session) = &self.terminal_session.session
        {
            session.set_presentable(presentable);
        }
    }

    fn evict_presentation_resources(&mut self, cx: &mut Context<Self>) {
        self.latest_presentation_operation = None;
        self.grid_presentation.evict();
        self.render_cache.update(cx, |cache, _| cache.evict());
        self.fallback_render_cache
            .update(cx, |cache, _| cache.evict());
        self.graphics_cache.update(cx, |cache, cx| cache.clear(cx));
        self.grid_bounds = None;
    }

    fn sync_native_accessibility(&mut self, window: &Window, focused: bool) {
        let notifications = self.pending_accessibility_notifications.take();
        let selection_sender = self
            .terminal_session
            .session
            .as_ref()
            .and_then(|session| session.accessibility_selection_sender());
        let demand_sender = self
            .terminal_session
            .session
            .as_ref()
            .and_then(|session| session.accessibility_demand_sender());
        self.pending_accessibility_notifications =
            self.accessibility_element
                .update(TerminalAccessibilityUpdate {
                    window,
                    model: self.accessibility.as_ref(),
                    bounds: self.grid_bounds,
                    cell_width: self.cell_width,
                    line_height: px(self.line_height),
                    font_family: self.font_family.as_ref(),
                    font_size: px(self.font_size),
                    focused,
                    notifications,
                    selection_sender,
                    demand_sender,
                });
        self.accessibility_needs_presentation = false;
    }

    fn sync_directory_metadata(&mut self, session_epoch: u64) -> bool {
        if self.terminal_session.session_epoch != session_epoch {
            return false;
        }
        let Some(snapshot) = self
            .terminal_session
            .session
            .as_ref()
            .and_then(|session| session.directory_snapshot())
        else {
            return false;
        };
        self.accept_directory_metadata(snapshot)
    }

    fn accept_directory_metadata(
        &mut self,
        snapshot: crate::terminal::SessionDirectorySnapshot,
    ) -> bool {
        if self
            .terminal_session
            .accepted_directory_revision
            .is_some_and(|revision| snapshot.revision < revision)
        {
            return false;
        }
        self.terminal_session.accepted_directory_revision = Some(snapshot.revision);
        let changed = self.terminal_session.current_directory != snapshot.current;
        self.terminal_session.current_directory = snapshot.current;
        changed
    }

    fn handle_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) -> bool {
        match event {
            SessionEvent::CurrentDirectoryChanged => {}
            SessionEvent::Screen(screen) => {
                if self
                    .terminal_session
                    .accepted_screen_generation
                    .is_some_and(|generation| screen.generation <= generation)
                {
                    return false;
                }
                let caption_changed = self.screen.metadata.directory != screen.metadata.directory
                    || self.screen.metadata.title != screen.metadata.title
                    || self.screen.metadata.command != screen.metadata.command;
                let title = normalized_pane_title(&screen.title, &self.fallback_title);
                if self.title.as_ref() != title {
                    self.title = title.into();
                    cx.emit(TerminalPaneEvent::TitleChanged(self.title.clone()));
                }
                let _ = self.render_lifecycle.observe_snapshot(screen.generation);
                self.terminal_session.accepted_screen_generation = Some(screen.generation);
                self.screen = screen;
                self.screen_session_epoch = self.terminal_session.session_epoch;
                let current = (self.screen.metadata.freshness
                    == crate::terminal::metadata::MetadataFreshness::Live)
                    .then(|| {
                        self.screen
                            .metadata
                            .context
                            .current_directory(&self.screen.metadata.directory.path)
                    })
                    .flatten();
                self.accept_directory_metadata(crate::terminal::SessionDirectorySnapshot {
                    revision: self.screen.metadata.revision,
                    current,
                });
                if caption_changed {
                    cx.emit(TerminalPaneEvent::CaptionChanged);
                }
                self.reconcile_pending_accessibility();
                self.sync_scrollbar(cx);
            }
            SessionEvent::Attention(event) => {
                let effects = self.attention.observe(
                    event,
                    AttentionFacts {
                        terminal_input_focus: self.terminal_input_focus,
                        surface_active: self.surface_active,
                        application_active: self.application_active,
                    },
                    Instant::now(),
                );
                if effects.visual_bell {
                    self.start_visual_bell(cx);
                }
                let unread_count = effects.unread_count;
                if let Some(pane) = self.native_attention_pane {
                    self.apply_attention(pane, effects, cx);
                }
                cx.emit(TerminalPaneEvent::AttentionChanged { unread_count });
            }
            SessionEvent::HiddenInputChanged(hidden_input) => {
                self.hidden_input = hidden_input;
                self.sync_secure_input();
            }
            SessionEvent::Exited(status) => {
                if self.suspend_if_remote_channel_unavailable(cx) {
                    return false;
                }
                self.context_menu = None;
                self.file_preview.dismiss();
                if matches!(self.pane_state, PaneTerminalState::Exited(_))
                    || self
                        .pane_state
                        .failure()
                        .is_some_and(TerminalFailure::is_fatal)
                {
                    return false;
                }
                self.hidden_input = false;
                self.sync_secure_input();
                self.state_revision = self.state_revision.wrapping_add(1);
                self.pending_recovery = None;
                self.recovery_retry_requested = None;
                self.status = None;
                self.pane_state = PaneTerminalState::exited(status);
                cx.emit(TerminalPaneEvent::Exited);
            }
            SessionEvent::Failed(failure) => {
                if self.suspend_if_remote_channel_unavailable(cx) {
                    return false;
                }
                self.context_menu = None;
                self.file_preview.dismiss();
                self.hidden_input = false;
                self.sync_secure_input();
                let failure = TerminalFailure::from_session(&failure);
                self.present_failure(failure, true, None);
            }
        }
        true
    }

    fn handle_session_event(
        &mut self,
        session_epoch: u64,
        event: SessionEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        self.terminal_session.session_epoch == session_epoch && self.handle_event(event, cx)
    }

    fn handle_session_accessibility(
        &mut self,
        session_epoch: u64,
        accessibility: Arc<TerminalAccessibilityModel>,
    ) {
        if self.terminal_session.session_epoch == session_epoch {
            self.handle_ordered_accessibility(session_epoch, accessibility);
        }
    }

    fn handle_ordered_accessibility(
        &mut self,
        session_epoch: u64,
        accessibility: Arc<TerminalAccessibilityModel>,
    ) {
        match self.terminal_session.accepted_screen_generation {
            Some(screen_generation) if accessibility.generation() < screen_generation => {}
            Some(screen_generation) if accessibility.generation() == screen_generation => {
                self.pending_accessibility = None;
                self.handle_accessibility(accessibility);
            }
            _ => {
                let replace =
                    self.pending_accessibility
                        .as_ref()
                        .is_none_or(|(pending_epoch, pending)| {
                            *pending_epoch != session_epoch
                                || accessibility.generation() >= pending.generation()
                        });
                if replace {
                    self.pending_accessibility = Some((session_epoch, accessibility));
                }
            }
        }
    }

    fn reconcile_pending_accessibility(&mut self) {
        let Some((session_epoch, accessibility)) = self.pending_accessibility.take() else {
            return;
        };
        if session_epoch != self.terminal_session.session_epoch {
            return;
        }
        match self.terminal_session.accepted_screen_generation {
            Some(screen_generation) if accessibility.generation() < screen_generation => {}
            Some(screen_generation) if accessibility.generation() == screen_generation => {
                self.handle_accessibility(accessibility);
            }
            _ => self.pending_accessibility = Some((session_epoch, accessibility)),
        }
    }

    fn handle_accessibility(&mut self, accessibility: Arc<TerminalAccessibilityModel>) {
        self.accessibility_needs_presentation |=
            !accessibility.shares_snapshot(self.accessibility.as_ref());
        if accessibility.active_screen() != self.accessibility.active_screen()
            || !accessibility.shares_document(self.accessibility.as_ref())
        {
            self.pending_accessibility_notifications
                .insert(AccessibilityNotification::Value);
        }
        if accessibility.selected_or_cursor_range() != self.accessibility.selected_or_cursor_range()
        {
            self.pending_accessibility_notifications
                .insert(AccessibilityNotification::Selection);
        }
        self.accessibility = accessibility;
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.synchronize_terminal_input_focus(window, cx) {
            return;
        }
        let input = self.key_input_adapter.key_down(event);
        if self.ime.marked_text().is_some() {
            if let KeyTranslation::Encoded(input) = &input
                && input.physical_key != PhysicalKey::Unidentified
                && !self.ime_suppressed_keys.contains(&input.physical_key)
            {
                self.ime_suppressed_keys.push(input.physical_key);
            }
            if !matches!(input, KeyTranslation::Unhandled(_)) {
                cx.stop_propagation();
            }
            return;
        }
        if self.send_key_translation(input, cx) {
            cx.stop_propagation();
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.synchronize_terminal_input_focus(window, cx) {
            return;
        }
        let input = self.key_input_adapter.key_up(event);
        if let KeyTranslation::Encoded(input) = &input
            && let Some(index) = self
                .ime_suppressed_keys
                .iter()
                .position(|key| *key == input.physical_key)
        {
            self.ime_suppressed_keys.swap_remove(index);
            cx.stop_propagation();
            return;
        }
        if self.ime.marked_text().is_some() {
            if !matches!(input, KeyTranslation::Unhandled(_)) {
                cx.stop_propagation();
            }
            return;
        }
        if self.send_key_translation(input, cx) {
            cx.stop_propagation();
        }
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = input_modifiers(event.modifiers);
        if self.pointer_modifiers != modifiers {
            self.pointer_modifiers = modifiers;
            cx.notify();
        }
        if !self.synchronize_terminal_input_focus(window, cx) {
            return;
        }
        if let Some(translation) = self.key_input_adapter.modifiers_changed(event) {
            self.send_key_translation(translation, cx);
        }
    }

    fn send_key_translation(
        &mut self,
        translation: KeyTranslation,
        cx: &mut Context<Self>,
    ) -> bool {
        let input = match translation {
            KeyTranslation::Encoded(input) => input,
            KeyTranslation::TextInput(text) => KeyInput::text_input(text),
            KeyTranslation::Unhandled(event) => {
                let kind = match event.kind {
                    TerminalKeyInputEventKind::KeyDown => DiagnosticKeyEventKind::KeyDown,
                    TerminalKeyInputEventKind::KeyUp => DiagnosticKeyEventKind::KeyUp,
                    TerminalKeyInputEventKind::ModifiersChanged => {
                        DiagnosticKeyEventKind::FlagsChanged
                    }
                };
                self.diagnostics
                    .record_unhandled_key(UnhandledKeyDiagnostic::new(
                        kind,
                        event.action,
                        event.native_key_code,
                    ));
                return false;
            }
        };
        let resets_cursor_blink = input.action != KeyAction::Release;
        if let Some(session) = &self.terminal_session.session {
            session.key(input);
            if resets_cursor_blink {
                self.clear_attention(cx);
            }
            if resets_cursor_blink && self.screen.cursor.visible && self.screen.cursor.blinking {
                self.reset_blink_phase();
                cx.notify();
            }
        }
        true
    }

    fn increase_font_size(
        &mut self,
        _: &IncreaseTerminalFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_font_size(self.font_size + FONT_SIZE_STEP, window, cx);
    }

    fn decrease_font_size(
        &mut self,
        _: &DecreaseTerminalFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_font_size(self.font_size - FONT_SIZE_STEP, window, cx);
    }

    fn reset_font_size(
        &mut self,
        _: &ResetTerminalFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_font_size(DEFAULT_FONT_SIZE, window, cx);
    }

    fn set_font_size(&mut self, font_size: f32, window: &mut Window, cx: &mut Context<Self>) {
        let font_size = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if self.font_size == font_size {
            return;
        }

        self.font_size = font_size;
        self.line_height = line_height_for_font_size(font_size);
        self.cell_width = measure_cell_width(window, &self.font_family, font_size);
        self.last_geometry = None;
        self.sync_scrollbar(cx);
        self.pending_accessibility_notifications
            .insert(AccessibilityNotification::Value);
        cx.notify();
    }

    fn update_backing_scale(&mut self, factor: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_backing_scale(factor, false, window, cx);
    }

    fn retry_presentation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_backing_scale(window.scale_factor(), true, window, cx);
    }

    fn apply_backing_scale(
        &mut self,
        factor: f32,
        force_resources: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backing_scale) = BackingScale::new(factor) else {
            self.present_failure(
                TerminalFailure::presentation("update-backing-scale"),
                true,
                Some(RecoveryAction::Presentation),
            );
            cx.notify();
            return;
        };
        if self.backing_scale == backing_scale && !force_resources {
            return;
        }

        self.backing_scale = backing_scale;
        self.cell_width = measure_cell_width(window, &self.font_family, self.font_size);
        self.last_geometry = None;
        let scale_change = self.render_lifecycle.update_scale(factor);
        if force_resources || scale_change == ScaleChange::ScaleResources {
            self.render_cache
                .update(cx, |cache, _| cache.invalidate_scale_dependent());
            self.fallback_render_cache
                .update(cx, |cache, _| cache.invalidate_scale_dependent());
        }
        self.sync_scrollbar(cx);
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_menu.is_some() {
            cx.stop_propagation();
            return;
        }
        self.pointer_modifiers = input_modifiers(event.modifiers);
        self.focus(window);
        self.clear_attention(cx);
        if !self.synchronize_terminal_input_focus(window, cx) {
            return;
        }
        let Some(button) = pointer_button(event.button) else {
            return;
        };
        if self.pressed_button.is_some() {
            cx.stop_propagation();
            return;
        }
        let Some(position) = self.surface_position(event.position, false) else {
            return;
        };

        if button == PointerButton::Left
            && event.modifiers.platform
            && let Some(link) = self.link_at(position)
        {
            self.pressed_link = Some((self.screen.generation, link));
            cx.stop_propagation();
            return;
        }

        self.pressed_button = Some(button);
        self.selection_copy_pending = button == PointerButton::Left
            && pointer_uses_text_cursor(
                self.screen.mouse_tracking,
                self.pointer_modifiers.shift,
                self.shift_selection,
            );
        if let Some(session) = &self.terminal_session.session {
            session.pointer(PointerInput {
                generation: self.screen.generation,
                phase: PointerPhase::Press,
                button: Some(button),
                position,
                modifiers: self.pointer_modifiers,
                shift_selection: self.shift_selection,
            });
        }
        cx.stop_propagation();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_menu.is_some() {
            cx.stop_propagation();
            return;
        }
        self.pointer_modifiers = input_modifiers(event.modifiers);
        let dragging = self.pressed_button.is_some();
        let Some(position) = self.surface_position(event.position, dragging) else {
            return;
        };
        let hovered_link = self
            .link_cell_at(position)
            .map(|(cell, target)| HoveredTerminalLink {
                generation: self.screen.generation,
                cell,
                target,
            });
        if self.hovered_link != hovered_link {
            self.hovered_link = hovered_link;
            cx.notify();
        }
        if self.terminal_session.remote_input_blocked {
            cx.stop_propagation();
            return;
        }
        if self.pressed_link.is_some() {
            cx.stop_propagation();
            return;
        }
        if let Some(session) = &self.terminal_session.session {
            session.pointer(PointerInput {
                generation: self.screen.generation,
                phase: PointerPhase::Motion,
                button: self.pressed_button,
                position,
                modifiers: self.pointer_modifiers,
                shift_selection: self.shift_selection,
            });
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.is_some() {
            cx.stop_propagation();
            return;
        }
        self.pointer_modifiers = input_modifiers(event.modifiers);
        let Some(button) = pointer_button(event.button) else {
            return;
        };
        if let Some((generation, pressed)) = self.pressed_link.take() {
            let current = self
                .surface_position(event.position, false)
                .and_then(|position| self.link_at(position));
            if let Some(url) = activated_link(
                self.terminal_session.local_file_capabilities,
                generation,
                &pressed,
                self.screen.generation,
                current.as_ref(),
                event.modifiers.platform,
            ) {
                cx.open_url(&url);
            }
            cx.stop_propagation();
            return;
        }
        if self.pressed_button != Some(button) {
            return;
        }
        self.pressed_button = None;
        let copy_selection = std::mem::take(&mut self.selection_copy_pending);
        let Some(position) = self.surface_position(event.position, true) else {
            return;
        };

        let input = PointerInput {
            generation: self.screen.generation,
            phase: PointerPhase::Release,
            button: Some(button),
            position,
            modifiers: self.pointer_modifiers,
            shift_selection: self.shift_selection,
        };
        let copy = self.terminal_session.session.as_ref().and_then(|session| {
            if copy_selection {
                Some(session.pointer_and_copy_selection(input))
            } else {
                session.pointer(input);
                None
            }
        });
        if let Some(copy) = copy {
            self.publish_selection_copy(copy, None, cx);
        }
        cx.stop_propagation();
    }

    fn on_mouse_up_out(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_menu.is_some() {
            return;
        }
        self.on_mouse_up(event, window, cx);
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_menu.is_some() {
            cx.stop_propagation();
            return;
        }
        let Some(position) = self.surface_position(event.position, false) else {
            return;
        };
        self.reveal_scrollbar(cx);
        if self.terminal_session.remote_input_blocked {
            cx.stop_propagation();
            return;
        }
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => point(
                f32::from(delta.x) / f32::from(self.cell_width),
                f32::from(delta.y) / self.line_height,
            ),
            ScrollDelta::Lines(delta) => point(delta.x, delta.y),
        };
        let phase = resolve_wheel_phase(
            event.touch_phase,
            self.lifecycle_dependencies.wheel.current(_window),
        );
        let (horizontal_steps, vertical_steps) =
            self.wheel_accumulator.push(delta.x, delta.y, phase);

        if (horizontal_steps != 0 || vertical_steps != 0)
            && let Some(session) = &self.terminal_session.session
        {
            session.wheel(WheelInput {
                generation: self.screen.generation,
                horizontal_steps,
                vertical_steps,
                phase,
                position,
                modifiers: input_modifiers(event.modifiers),
                shift_selection: self.shift_selection,
            });
        }
        cx.stop_propagation();
    }

    fn copy_selection(&mut self, _: &CopySelection, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection_with_recovery(None, window, cx);
    }

    fn edit_copy(&mut self, _: &EditCopy, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_handle.is_focused(window) {
            self.copy_selection_with_recovery(None, window, cx);
        }
    }

    fn copy_selection_with_recovery(
        &mut self,
        recovery: Option<RecoveryToken>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.terminal_session.session.as_ref() else {
            return;
        };
        self.publish_selection_copy(session.copy_selection(), recovery, cx);
    }

    fn publish_selection_copy(
        &mut self,
        result: Result<Option<SelectionCopy>, SelectionCopyError>,
        recovery: Option<RecoveryToken>,
        cx: &mut Context<Self>,
    ) {
        if let Some(copy) = self.selection_copy_from_result(result, cx) {
            if let Err(error) = self.selection_pasteboard.write(copy, cx) {
                let _ = error;
                self.present_failure(
                    TerminalFailure::platform("write-selection-pasteboard"),
                    true,
                    Some(RecoveryAction::CopySelection),
                );
                cx.notify();
            } else if recovery.is_some_and(|recovery| self.clear_recovery(recovery)) {
                cx.notify();
            }
        }
    }

    fn ordered_selection_copy(&mut self, cx: &mut Context<Self>) -> Option<SelectionCopy> {
        let session = self.terminal_session.session.as_ref()?;
        self.selection_copy_from_result(session.copy_selection(), cx)
    }

    fn selection_copy_from_result(
        &mut self,
        result: Result<Option<SelectionCopy>, SelectionCopyError>,
        cx: &mut Context<Self>,
    ) -> Option<SelectionCopy> {
        match result {
            Ok(Some(copy)) if !copy.plain_text.is_empty() => Some(copy),
            Err(SelectionCopyError::Formatting) => {
                self.present_failure(
                    TerminalFailure::emulator("format-terminal-selection"),
                    true,
                    None,
                );
                cx.notify();
                None
            }
            Err(SelectionCopyError::WorkerStopped) => {
                self.present_failure(
                    TerminalFailure::resource("receive-selection-reply"),
                    true,
                    Some(RecoveryAction::CopySelection),
                );
                cx.notify();
                None
            }
            Ok(None | Some(_)) => None,
        }
    }

    pub(crate) fn native_service_selection(
        &mut self,
        origin: NativeServiceOrigin,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<SelectionCopy> {
        self.sync_terminal_input_focus(window, cx);
        self.native_service_origin_matches(origin)
            .then(|| self.ordered_selection_copy(cx))
            .flatten()
    }

    fn edit_paste(&mut self, _: &EditPaste, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_handle.is_focused(window) {
            self.paste_clipboard(&PasteClipboard, window, cx);
        }
    }

    fn paste_clipboard(&mut self, _: &PasteClipboard, window: &mut Window, cx: &mut Context<Self>) {
        let terminal_input_focused = self.synchronize_terminal_input_focus(window, cx);
        let Ok(Some(insertion)) = PastePayload::clipboard(
            self.file_insertion,
            self.file_clipboard.as_ref(),
            || cx.read_from_clipboard().and_then(|item| item.text()),
            terminal_input_focused,
            self.terminal_session.local_file_capabilities,
        ) else {
            return;
        };
        if !insertion.text().is_empty() {
            self.request_paste_text(insertion, cx);
        }
    }

    pub(crate) fn insert_native_service_text(
        &mut self,
        origin: NativeServiceOrigin,
        text: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.sync_terminal_input_focus(window, cx);
        if !self.native_service_origin_matches(origin) || !self.terminal_input_focus {
            return false;
        }
        let Ok(insertion) = PastePayload::service_text(text, true) else {
            return false;
        };
        if insertion.text().is_empty() {
            return false;
        }
        self.request_paste_text(insertion, cx);
        true
    }

    fn insert_dropped_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_dropped_file_paths(paths.paths(), window, cx);
    }

    fn insert_dropped_file_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let insertion = match PastePayload::prepare_dropped_files(
            self.file_insertion,
            paths,
            self.terminal_session.local_file_capabilities,
        ) {
            Ok(Some(insertion)) => insertion,
            Ok(None) => return,
            Err(message) => {
                self.status = Some(format!("File drop rejected: {message}"));
                cx.notify();
                return;
            }
        };
        self.pending_file_insertion = Some(insertion);
        window.activate_window();
        self.focus(window);
        cx.emit(TerminalPaneEvent::FocusRequested);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn insert_dropped_file_paths_for_test(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_dropped_file_paths(paths, window, cx);
    }

    fn flush_pending_file_insertion(&mut self, cx: &mut Context<Self>) {
        if !self.terminal_input_focus || self.terminal_session.session.is_none() {
            return;
        }
        let Some(insertion) = self.pending_file_insertion.take() else {
            return;
        };
        self.request_paste_text(insertion, cx);
    }

    fn native_context_actions(&self) -> NativeContextActions {
        NativeContextActions::from_presence(
            self.terminal_session.local_file_capabilities,
            self.screen.selection_present,
            self.current_hovered_link(),
        )
    }

    fn context_menu_actions(&self, menu: &TerminalContextMenuState) -> NativeContextActions {
        let current = self.link_at(menu.position);
        menu.actions(
            self.terminal_session.local_file_capabilities,
            self.screen.generation,
            self.screen.selection_present,
            current.as_ref(),
        )
    }

    fn request_context_menu(
        &mut self,
        request: &spaceterm_ui::ContextMenuOpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = window.modifiers();
        if !opens_terminal_context_menu(
            PointerButton::Right,
            self.screen.mouse_tracking,
            modifiers.shift,
            self.shift_selection,
        ) {
            return false;
        }
        let Some(position) = self.surface_position(request.position(), false) else {
            return false;
        };

        self.pointer_modifiers = input_modifiers(modifiers);
        self.focus(window);
        self.clear_attention(cx);
        if !self.synchronize_terminal_input_focus(window, cx) {
            return false;
        }

        let link = self.link_at(position);
        let file_preview_eligible = NativeContextActions::from_presence(
            self.terminal_session.local_file_capabilities,
            false,
            link.as_ref(),
        )
        .file_preview;
        self.context_menu = Some(TerminalContextMenuState {
            generation: self.screen.generation,
            position,
            selection_present: self.screen.selection_present,
            file_preview_eligible,
            link,
        });
        self.sync_terminal_input_focus(window, cx);
        cx.notify();
        true
    }

    fn context_menu_closed(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    fn context_menu_available(&self) -> bool {
        matches!(self.pane_state, PaneTerminalState::Running)
            && self.product_focus.active_workspace
            && self.product_focus.active_tab
            && self.product_focus.focused_pane
            && self.product_focus.blocker.is_none()
            && !self.focus_coordinator.native_dialog_open()
    }

    fn perform_context_menu_command(
        &mut self,
        menu: TerminalContextMenuState,
        command: TerminalContextMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        let current = self.link_at(menu.position);
        let link = revalidated_context_link(
            menu.generation,
            menu.link.as_ref(),
            self.screen.generation,
            current.as_ref(),
        )
        .cloned();
        let actions = menu.actions(
            self.terminal_session.local_file_capabilities,
            self.screen.generation,
            self.screen.selection_present,
            current.as_ref(),
        );
        self.sync_terminal_input_focus(window, cx);

        match command {
            TerminalContextMenuCommand::Paste => self.paste_clipboard(&PasteClipboard, window, cx),
            TerminalContextMenuCommand::Find => self.open_find(&OpenTerminalFind, window, cx),
            TerminalContextMenuCommand::Copy if actions.copy => {
                if let Some(session) = &self.terminal_session.session {
                    self.publish_selection_copy(
                        session.copy_selection_at(menu.generation),
                        None,
                        cx,
                    );
                }
            }
            TerminalContextMenuCommand::OpenLink if actions.open_link => {
                if let Some(url) = link.and_then(|link| {
                    link.activation_url(self.terminal_session.local_file_capabilities)
                }) {
                    cx.open_url(&url);
                }
            }
            TerminalContextMenuCommand::FilePreview if actions.file_preview => {
                if let Some(link) = link {
                    self.preview_context_link(&link, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn preview_context_link(
        &mut self,
        link: &crate::terminal::HyperlinkTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(target) =
            FilePreviewTarget::from_link(link, self.terminal_session.local_file_capabilities)
        else {
            self.file_preview.dismiss();
            return;
        };
        if self.file_preview.preview(&target).is_err() {
            self.present_failure(TerminalFailure::platform("preview-local-file"), true, None);
            cx.notify();
        }
    }

    fn current_hovered_link(&self) -> Option<&crate::terminal::HyperlinkTarget> {
        hovered_link_for_generation(self.hovered_link.as_ref(), self.screen.generation)
            .map(|hovered| &hovered.target)
    }

    pub(crate) fn native_service_status(
        &mut self,
        workspace_id: WorkspaceId,
        tab_id: TabId,
        pane_id: PaneId,
        hierarchy_generation: u64,
        window: &Window,
        cx: &App,
    ) -> NativeServiceStatus {
        self.sync_terminal_input_focus(window, cx);
        let session_available = self.terminal_session.session.is_some();
        let capabilities = NativeServiceCapabilities::new(
            session_available && self.native_context_actions().copy,
            session_available && self.terminal_input_focus,
        );
        let origin = session_available.then(|| {
            NativeServiceOrigin::new(
                workspace_id,
                tab_id,
                pane_id,
                self.terminal_session.native_service_session_identity,
                self.native_service_focus_epoch.get(),
                hierarchy_generation,
            )
        });
        NativeServiceStatus::new(capabilities, origin)
    }

    fn request_paste_text(&mut self, text: PastePayload, cx: &mut Context<Self>) {
        let Some(session) = &self.terminal_session.session else {
            return;
        };
        let guard = self.paste_request_guard();
        let receiver = session.request_paste(text);
        cx.spawn(async move |this, cx| {
            let outcome = receiver.recv().await;
            let _ = this.update(cx, |this, cx| {
                if !this.paste_request_guard_is_current(guard) {
                    if let Ok(Ok(PasteRequestOutcome::ConfirmationRequired(confirmation))) = outcome
                        && this.terminal_session.native_service_session_identity
                            == guard.session_identity
                        && let Some(session) = &this.terminal_session.session
                    {
                        let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
                    }
                    return;
                }
                match outcome {
                    Ok(Ok(PasteRequestOutcome::Written)) => this.clear_attention(cx),
                    Ok(Ok(PasteRequestOutcome::ConfirmationRequired(confirmation))) => {
                        this.pending_paste = Some(confirmation);
                        cx.notify();
                    }
                    Ok(Ok(PasteRequestOutcome::Rejected(rejection))) => {
                        this.status = Some(format!("Paste rejected: {rejection}"));
                        cx.notify();
                    }
                    Ok(Err(_)) | Err(_) => {
                        this.status = Some(
                            "Paste request failed before any terminal input was written".to_owned(),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn paste_request_guard(&self) -> PasteRequestGuard {
        PasteRequestGuard {
            session_identity: self.terminal_session.native_service_session_identity,
            focus_epoch: self.native_service_focus_epoch.get(),
            hierarchy_generation: self.native_service_hierarchy_generation,
        }
    }

    fn paste_request_guard_is_current(&self, guard: PasteRequestGuard) -> bool {
        self.terminal_session.session.is_some()
            && self.terminal_input_focus
            && self.terminal_session.native_service_session_identity == guard.session_identity
            && self.native_service_focus_epoch.get() == guard.focus_epoch
            && self.native_service_hierarchy_generation == guard.hierarchy_generation
    }

    fn native_service_origin_matches(&self, origin: NativeServiceOrigin) -> bool {
        self.terminal_session.session.is_some()
            && self.terminal_session.native_service_session_identity == origin.session_identity()
            && self.native_service_focus_epoch.get() == origin.focus_epoch()
            && self.native_service_hierarchy_generation == origin.hierarchy_generation()
    }

    fn export_diagnostics(
        &mut self,
        _: &ExportTerminalDiagnostics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.export_diagnostics_with_recovery(None, window, cx);
    }

    fn export_diagnostics_with_recovery(
        &mut self,
        recovery: Option<RecoveryToken>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_coordinator.set_native_dialog_open(true);
        let _ = self.sync_terminal_input_focus(window, cx);
        cx.notify();
        let operation = self.begin_operation(self.screen.generation, recovery);
        self.latest_export_operation = Some(operation.id);
        let directory = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let receiver = cx.prompt_for_new_path(&directory, Some("SpaceTerm-diagnostics.txt"));
        let diagnostics = self.diagnostics.clone();
        cx.spawn(async move |this, cx| {
            let response = receiver.await;
            let _ = this.update(cx, |this, cx| {
                this.focus_coordinator.set_native_dialog_open(false);
                cx.notify();
            });
            let Ok(Ok(Some(path))) = response else {
                return;
            };
            let exported_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { diagnostics.export(&exported_path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_export(operation, result, path, cx);
            });
        })
        .detach();
    }

    fn finish_export(
        &mut self,
        operation: OperationToken,
        result: std::io::Result<()>,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if !self.operation_is_current(operation, self.latest_export_operation) {
            return;
        }
        if result.is_ok() {
            if let Some(recovery) = operation.recovery {
                self.clear_recovery(recovery);
            }
            if matches!(self.pane_state, PaneTerminalState::Running) {
                self.status = Some(format!("Diagnostics exported to {}", path.display()));
            }
        } else {
            self.present_failure_at(
                TerminalFailure::resource("export-diagnostics-file"),
                true,
                Some(RecoveryAction::ExportDiagnostics),
                operation.generation,
            );
        }
        cx.notify();
    }

    fn confirm_unsafe_paste(
        &mut self,
        _: &ConfirmUnsafePaste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resolve_pending_paste(PasteDecision::Confirm, window, cx);
    }

    fn cancel_unsafe_paste(
        &mut self,
        _: &CancelUnsafePaste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resolve_pending_paste(PasteDecision::Cancel, window, cx);
    }

    fn resolve_pending_paste(
        &mut self,
        mut decision: PasteDecision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(confirmation) = self.pending_paste.take() else {
            return;
        };
        if !self.synchronize_terminal_input_focus(window, cx) {
            decision = PasteDecision::Cancel;
        }
        let Some(session) = &self.terminal_session.session else {
            return;
        };
        let receiver = session.resolve_paste(confirmation.id, decision);
        self.focus(window);
        cx.notify();
        cx.spawn(async move |this, cx| match receiver.recv().await {
            Ok(Ok(PasteResolution::Written)) => {
                let _ = this.update(cx, |this, cx| this.clear_attention(cx));
            }
            Ok(Ok(PasteResolution::Cancelled)) => {}
            Ok(Ok(PasteResolution::Stale)) => {
                let _ = this.update(cx, |this, cx| {
                    this.status = Some(
                        "Paste confirmation expired without writing terminal input".to_owned(),
                    );
                    cx.notify();
                });
            }
            Ok(Err(_)) | Err(_) => {
                let _ = this.update(cx, |this, cx| {
                    this.status = Some(
                        "Paste confirmation was lost without writing terminal input".to_owned(),
                    );
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn surface_position(
        &self,
        position: gpui::Point<Pixels>,
        allow_outside: bool,
    ) -> Option<SurfacePosition> {
        terminal_surface_position(
            self.grid_bounds?,
            position,
            self.last_geometry?,
            allow_outside,
        )
    }

    fn link_at(&self, position: SurfacePosition) -> Option<crate::terminal::HyperlinkTarget> {
        self.link_cell_at(position).map(|(_, link)| link)
    }

    fn link_cell_at(
        &self,
        position: SurfacePosition,
    ) -> Option<(CellGridPosition, crate::terminal::HyperlinkTarget)> {
        let cell = self
            .last_geometry?
            .cell_at_backing_position(BackingPosition::new(position.x, position.y));
        let link = self
            .screen
            .rows
            .get(usize::from(cell.row))?
            .get(usize::from(cell.col))?
            .hyperlink
            .clone()
            .filter(|link| link.is_available(self.terminal_session.local_file_capabilities))?;
        Some((cell, link.as_ref().clone()))
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let input = self.find_input.as_ref()?.clone();
        let snapshot = self
            .screen
            .find
            .as_ref()
            .filter(|snapshot| snapshot.generation == self.find_generation);
        let result_label = if input.read(cx).value().is_empty() {
            "Find".to_owned()
        } else {
            snapshot.map_or_else(
                || "Searching…".to_owned(),
                |snapshot| {
                    if snapshot.total_matches == 0 {
                        return "No matches".to_owned();
                    }
                    format!(
                        "{}/{}",
                        snapshot
                            .current_match
                            .map_or_else(|| "–".to_owned(), |index| index.to_string()),
                        snapshot.total_matches
                    )
                },
            )
        };
        let has_results = snapshot.is_some_and(|snapshot| snapshot.total_matches > 0);
        let pane = cx.entity().downgrade();
        let previous_pane = pane.clone();
        let next_pane = pane.clone();
        let close_pane = pane.clone();

        Some(
            div()
                .id("terminal-find-bar")
                .debug_selector(|| "terminal-find-bar".to_owned())
                .absolute()
                .top(px(8.0))
                .right(px(8.0))
                .w(px(360.0))
                .max_w(relative(0.94))
                .h(px(32.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(5.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(gpui_color(ACTIVE_THEME.border))
                .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
                .shadow_md()
                .block_mouse_except_scroll()
                .key_context(TERMINAL_FIND_KEY_CONTEXT)
                .tab_group()
                .on_action(|_: &FocusNextTerminalFindControl, window, cx| {
                    window.focus_next();
                    cx.stop_propagation();
                })
                .on_action(|_: &FocusPreviousTerminalFindControl, window, cx| {
                    window.focus_prev();
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .id("terminal-find-field")
                        .relative()
                        .h(px(24.0))
                        .min_w(px(60.0))
                        .flex_grow()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .px(px(5.0))
                        .rounded(px(4.0))
                        .bg(gpui_color(ACTIVE_THEME.element_background))
                        .text_size(px(13.0))
                        .text_color(gpui_color(ACTIVE_THEME.text))
                        .whitespace_nowrap()
                        .child(input),
                )
                .child(
                    div()
                        .debug_selector(|| "terminal-find-result-label".to_owned())
                        .min_w(px(42.0))
                        .flex_shrink_0()
                        .text_size(px(11.0))
                        .text_color(gpui_color(ACTIVE_THEME.text_muted))
                        .child(result_label),
                )
                .child(find_icon_button(
                    "terminal-find-previous",
                    "Find Previous",
                    IconName::ChevronUp,
                    has_results,
                    move |window, cx| {
                        let _ = previous_pane.update(cx, |pane, cx| {
                            pane.find_previous(&FindPrevious, window, cx);
                        });
                    },
                ))
                .child(find_icon_button(
                    "terminal-find-next",
                    "Find Next",
                    IconName::ChevronDown,
                    has_results,
                    move |window, cx| {
                        let _ = next_pane.update(cx, |pane, cx| {
                            pane.find_next(&FindNext, window, cx);
                        });
                    },
                ))
                .child(find_icon_button(
                    "terminal-find-close",
                    "Close Find",
                    IconName::X,
                    true,
                    move |window, cx| {
                        let _ = close_pane.update(cx, |pane, cx| {
                            pane.close_find(&CloseTerminalFind, window, cx);
                        });
                    },
                ))
                .into_any_element(),
        )
    }
}

fn find_icon_button(
    id: &'static str,
    accessibility_name: &'static str,
    icon: IconName,
    enabled: bool,
    on_activate: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    IconButton::new(id, accessibility_name, move |foreground| {
        Icon::new(icon, px(12.0), foreground).into_any_element()
    })
    .variant(ButtonVariant::Ghost)
    .size(ButtonSize::Small)
    .disabled(!enabled)
    .tab_stop(true)
    .debug_selector(id)
    .tooltip(
        Tooltip::new(
            SharedString::from(format!("{id}-tooltip")),
            accessibility_name,
        )
        .debug_selector(format!("{id}-tooltip")),
    )
    .on_activate(move |_, window, cx| on_activate(window, cx))
    .into_any_element()
}

impl EventEmitter<TerminalPaneEvent> for TerminalPane {}

impl EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        if self.ime.marked_text().is_some() {
            let (text, adjusted) = self.ime.text_for_utf16_range(range)?;
            *adjusted_range = Some(adjusted);
            return Some(text);
        }
        if range.start < self.accessibility.visible_range().end
            && self.accessibility.line_for_index(range.start).is_none()
        {
            return None;
        }
        let text = self.accessibility.text_for_range(range.clone())?;
        *adjusted_range = Some(range);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !ignore_disabled_input && !self.terminal_input_focused(window, _cx) {
            return None;
        }
        let range = if self.ime.marked_text().is_some() {
            self.ime.selected_range()
        } else {
            self.accessibility
                .selection_range()
                .unwrap_or_else(|| self.accessibility.cursor_range())
        };
        Some(UTF16Selection {
            range,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.ime.marked_range()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ime.cancel();
        self.invalidate_preedit_layout();
        self.pending_accessibility_notifications
            .insert(AccessibilityNotification::Value);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.synchronize_terminal_input_focus(window, cx) {
            self.ime.cancel();
            self.invalidate_preedit_layout();
            return;
        }
        self.ime.commit(text);
        self.invalidate_preedit_layout();
        if let Some(text) = self.ime.take_commit() {
            let translation = self.key_input_adapter.input_method_commit(text);
            self.send_key_translation(translation, cx);
        }
        self.pending_accessibility_notifications
            .insert(AccessibilityNotification::Value);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.synchronize_terminal_input_focus(window, cx) {
            self.ime.cancel();
            self.invalidate_preedit_layout();
            return;
        }
        self.ime
            .replace_and_mark(range, new_text, new_selected_range);
        self.invalidate_preedit_layout();
        self.pending_accessibility_notifications
            .insert(AccessibilityNotification::Value);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if let Some(marked_text) = self.ime.marked_text() {
            let position = self.screen.cursor.position?;
            let columns = self.screen.rows.first()?.len();
            let layout = layout_preedit(
                marked_text,
                usize::from(position.row),
                usize::from(position.column),
                columns,
                range_utf16.end,
            );
            return Some(ime_candidate_bounds(
                element_bounds,
                columns,
                self.cell_width,
                px(self.line_height),
                layout.caret,
            ));
        }
        let grid = self.grid_bounds?;
        let geometry = AccessibilityGeometry::new(
            f32::from(grid.origin.x),
            f32::from(grid.origin.y),
            f32::from(self.cell_width),
            self.line_height,
        )?;
        let (x, y, width, height) = self.accessibility.bounds_for_range(range_utf16, geometry)?;
        Some(Bounds::new(
            point(px(x), px(y)),
            size(px(width), px(height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if self.ime.marked_text().is_some() {
            return Some(self.ime.selected_range().end);
        }
        let grid = self.grid_bounds?;
        let geometry = AccessibilityGeometry::new(
            f32::from(grid.origin.x),
            f32::from(grid.origin.y),
            f32::from(self.cell_width),
            self.line_height,
        )?;
        self.accessibility
            .index_for_point(f32::from(point.x), f32::from(point.y), geometry)
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        self.close();
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let native_activity = self.current_activity(window, cx);
        self.update_application_activity(native_activity, cx);
        let pane = cx.entity().downgrade();
        let (terminal_input_focused, focus_gained) = self
            .sync_terminal_input_focus_with_activity_and_modal(
                window,
                native_activity,
                window_modal_is_open(window, cx),
            );
        self.flush_pending_file_insertion(cx);
        let surface_active = terminal_surface_active(self.product_focus, native_activity);
        let native_visibility = self
            .visibility_source
            .as_ref()
            .map(|source| source.current())
            .unwrap_or(WindowVisibility {
                minimized: false,
                occluded: true,
                live_resize: false,
            });
        let surface_visibility = SurfaceVisibility {
            application_active: native_activity.application_active,
            key_window: native_activity.operating_system_window_key,
            minimized: native_visibility.minimized,
            occluded: native_visibility.occluded,
            live_resize: native_visibility.live_resize,
            workspace_visible: self.product_focus.active_workspace,
            pane_visible: self.product_focus.active_tab && self.product_focus.pane_visible,
        };
        let was_presentable = self.render_lifecycle.can_present();
        let lifecycle_effects = self.render_lifecycle.update_visibility(surface_visibility);
        self.sync_session_presentability(was_presentable);
        if !lifecycle_effects.animations_active {
            self.stop_surface_animations();
        }
        self.sync_scrollbar(cx);
        if lifecycle_effects.request_redraw {
            cx.notify();
        }
        self.surface_active = surface_active;
        self.application_active = native_activity.application_active;
        if focus_gained {
            self.clear_attention(cx);
        }
        self.sync_presentation_blink(
            lifecycle_effects.animations_active,
            terminal_input_focused,
            cx,
        );
        if !self.render_lifecycle.can_present() {
            self.evict_presentation_resources(cx);
            return div()
                .size_full()
                .bg(gpui_color(self.screen.background))
                .into_any_element();
        }
        let recovery_holds_presentation = self.pending_recovery.is_some_and(|pending| {
            matches!(
                pending.action,
                RecoveryAction::Presentation | RecoveryAction::RendererResources
            ) && self.recovery_retry_requested != Some(pending)
        });
        let mut display_screen = if recovery_holds_presentation {
            Arc::clone(&self.last_valid_screen)
        } else {
            Arc::clone(&self.screen)
        };
        let fallback_graphics = self
            .graphics_cache
            .read_with(cx, |cache, _| cache.last_presented());
        let recovery = self
            .recovery_retry_requested
            .filter(|recovery| self.pending_recovery == Some(*recovery))
            .filter(|recovery| {
                matches!(
                    recovery.action,
                    RecoveryAction::Presentation | RecoveryAction::RendererResources
                )
            });
        let presentation_generation = self.render_lifecycle.take_frame().or_else(|| {
            recovery.and_then(|_| self.render_lifecycle.retry_frame(display_screen.generation))
        });
        let graphics_attempt_allowed = !recovery_holds_presentation
            && presentation_generation == Some(display_screen.generation);
        let (graphics, graphics_attempt, graphics_attempted) =
            self.graphics_cache.update(cx, |cache, cx| {
                if !graphics_attempt_allowed {
                    return (cache.last_presented(), None, false);
                }
                match cache.sync(
                    display_screen.active_screen,
                    &display_screen.graphics,
                    window,
                    cx,
                ) {
                    Ok(preparation) => (preparation.graphics, Some(preparation.token), true),
                    Err(_) => (cache.last_presented(), None, true),
                }
            });
        if graphics_attempted && graphics_attempt.is_none() {
            if self.present_failure(
                TerminalFailure::resource("prepare-terminal-graphics"),
                true,
                Some(RecoveryAction::RendererResources),
            ) {
                cx.notify();
            }
            display_screen = Arc::clone(&self.last_valid_screen);
        }
        let presentation_operation = graphics_attempt.map(|_| {
            let operation = self.begin_operation(display_screen.generation, recovery);
            self.latest_presentation_operation = Some(operation.id);
            operation
        });
        let displaying_current = Arc::ptr_eq(&display_screen, &self.screen);
        let display_render_cache = if displaying_current {
            self.render_cache.clone()
        } else {
            self.fallback_render_cache.clone()
        };
        let background = gpui_color(display_screen.background);
        let active_hovered_link = displaying_current
            .then(|| {
                active_hovered_link(
                    self.hovered_link.as_ref(),
                    self.screen.generation,
                    self.pointer_modifiers.platform,
                )
                .cloned()
            })
            .flatten();
        let native_context_actions = self.native_context_actions();
        let paste_confirmation = self.pending_paste;
        let key_context = if paste_confirmation.is_some() {
            TERMINAL_PASTE_CONFIRMATION_KEY_CONTEXT
        } else {
            TERMINAL_KEY_CONTEXT
        };
        self.sync_scrollbar(cx);
        let scrollbar = self.scrollbar.clone();
        let pointer_uses_text_cursor = pointer_uses_text_cursor(
            display_screen.mouse_tracking,
            self.pointer_modifiers.shift,
            self.shift_selection,
        );
        let preedit = displaying_current.then(|| self.preedit_layout()).flatten();
        let attention_visual = self.attention_visual;
        let find_spans = self
            .find_input
            .as_ref()
            .and_then(|_| display_screen.find.as_ref())
            .filter(|snapshot| snapshot.generation == self.find_generation)
            .map_or_else(|| Arc::from([]), |snapshot| snapshot.visible_spans.clone());
        let find_bar = self.render_find_bar(cx);
        let status = self.authoritative_status();
        let (status_color, status_icon) = match self.pane_state {
            PaneTerminalState::Failed { .. } => (ACTIVE_THEME.error, IconName::TriangleAlert),
            PaneTerminalState::Exited(_) => (ACTIVE_THEME.text_muted, IconName::Square),
            PaneTerminalState::Running => (ACTIVE_THEME.info, IconName::Info),
        };
        let diagnostics_available =
            self.pane_state.failure().is_some() && self.diagnostics.record_count() > 0;
        let recovery_available = self.pending_recovery.is_some();
        let last_valid_frame_preserved = self.pane_state.last_valid_frame().is_some();
        let export_pane = cx.entity().downgrade();
        let retry_pane = cx.entity().downgrade();
        let native_context_selector = format!(
            "terminal-native-context-copy-{}-open-{}-file-preview-{}-failure-{}-last-frame-{}",
            native_context_actions.copy,
            native_context_actions.open_link,
            native_context_actions.file_preview,
            diagnostics_available,
            last_valid_frame_preserved,
        );
        let terminal_grid = self.grid_presentation.render(
            &display_screen,
            display_render_cache,
            TerminalGridConfiguration {
                terminal_input_focused,
                font_family: self.font_family.clone(),
                font_size: px(self.font_size),
                line_height: px(self.line_height),
                cell_width: self.cell_width,
                preedit,
                focus_handle: self.focus_handle.clone(),
                input: cx.entity(),
                blink_phase_visible: self.blink_phase_visible,
                scale_factor: window.scale_factor(),
                find_spans,
                graphics,
                presentation_operation,
                graphics_attempt,
                graphics_cache: self.graphics_cache.clone(),
                active_hyperlink: active_hovered_link
                    .as_ref()
                    .map(|hovered| (hovered.target.identity, hovered.cell)),
                fallback: (presentation_operation.is_some()
                    && !Arc::ptr_eq(&display_screen, &self.last_valid_screen))
                .then(|| {
                    (
                        Arc::clone(&self.last_valid_screen),
                        self.fallback_render_cache.clone(),
                        fallback_graphics,
                    )
                }),
                paint_fault: {
                    #[cfg(test)]
                    {
                        self.paint_fault.take()
                    }
                    #[cfg(not(test))]
                    {
                        None
                    }
                },
            },
            cx,
        );
        let context_menu_target = self.context_menu.clone();
        let context_menu_actions = context_menu_target
            .as_ref()
            .map_or(native_context_actions, |menu| {
                self.context_menu_actions(menu)
            });
        let context_menu_available = self.context_menu_available();
        let context_menu_entries = terminal_context_menu_entries(
            context_menu_actions,
            crate::desktop_profile::DesktopPresentation::get(cx),
        );
        let context_open_pane = pane.clone();
        let context_activation_pane = pane.clone();
        let context_activation_target = context_menu_target.clone();
        let context_lifecycle_pane = pane.clone();
        let context_target_size = self
            .grid_bounds
            .map_or(size(px(1.0), px(1.0)), |bounds| bounds.size);
        let context_menu = ContextMenu::new(
            "terminal-context-menu",
            "Terminal context actions",
            div()
                .w(context_target_size.width)
                .h(context_target_size.height),
            context_menu_entries,
        )
        .size(MenuSize::Wide)
        .preserve_trigger_cursor()
        .disabled(!context_menu_available)
        .debug_selector("terminal-context-menu")
        .on_open_request(move |request, window, cx| {
            context_open_pane
                .update(cx, |pane, cx| {
                    pane.request_context_menu(request, window, cx)
                })
                .unwrap_or(false)
        })
        .on_activate(move |activation, window, cx| {
            let Some(target) = context_activation_target.clone() else {
                return;
            };
            let command = *activation.action();
            let _ = context_activation_pane.update(cx, |pane, cx| {
                pane.perform_context_menu_command(target, command, window, cx);
            });
        })
        .on_lifecycle(move |event, cx| {
            if matches!(event, MenuLifecycleEvent::Closed(_)) {
                let _ = context_lifecycle_pane.update(cx, |pane, cx| {
                    pane.context_menu_closed(cx);
                });
            }
        });

        div()
            .debug_selector(move || native_context_selector.clone())
            .on_children_prepainted(move |children, window, cx| {
                let Some(bounds) = children.first().copied() else {
                    return;
                };
                let _ = pane.update(cx, |pane, cx| {
                    pane.update_grid_bounds(bounds, cx);
                    pane.sync_native_accessibility(window, terminal_input_focused);
                });
            })
            .id("terminal-pane")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(background)
            .px(px(HORIZONTAL_PADDING))
            .pt(px(TOP_PADDING))
            .pb(px(BOTTOM_PADDING))
            .when(pointer_uses_text_cursor, |root| root.cursor_text())
            .when(!pointer_uses_text_cursor, |root| root.cursor_default())
            .when(active_hovered_link.is_some(), |root| root.cursor_pointer())
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::edit_copy))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::edit_paste))
            .on_action(cx.listener(Self::export_diagnostics))
            .on_drop(cx.listener(Self::insert_dropped_files))
            .on_action(cx.listener(Self::confirm_unsafe_paste))
            .on_action(cx.listener(Self::cancel_unsafe_paste))
            .on_action(cx.listener(Self::increase_font_size))
            .on_action(cx.listener(Self::decrease_font_size))
            .on_action(cx.listener(Self::reset_font_size))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .on_action(cx.listener(Self::close_find))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_any_mouse_down(cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up_out))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_mouse_up_out))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::on_mouse_up_out))
            .child(terminal_grid)
            .child(scrollbar)
            .when_some(find_bar, |root, find_bar| root.child(find_bar))
            .when(attention_visual, |root| {
                root.child(
                    div()
                        .debug_selector(|| "terminal-visual-bell".to_owned())
                        .absolute()
                        .inset_0()
                        .border_2()
                        .border_color(gpui_color(ACTIVE_THEME.warning)),
                )
            })
            .when_some(
                active_hovered_link.filter(|_| paste_confirmation.is_none() && status.is_none()),
                |root, link| {
                    root.child(
                        div()
                            .debug_selector(|| "terminal-link-preview".to_owned())
                            .absolute()
                            .left(px(8.0))
                            .bottom(px(8.0))
                            .max_w(px(520.0))
                            .px(px(6.0))
                            .py(px(3.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(gpui_color(ACTIVE_THEME.border))
                            .bg(gpui_color(ACTIVE_THEME.element_active))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
                            .text_sm()
                            .overflow_hidden()
                            .child(div().truncate().child(link.target.value)),
                    )
                },
            )
            .when_some(paste_confirmation, |root, confirmation| {
                root.child(render_paste_confirmation(
                    confirmation,
                    cx.entity().downgrade(),
                ))
            })
            .when_some(
                status.filter(|_| paste_confirmation.is_none()),
                |root, status| {
                    root.child(
                        div()
                            .debug_selector(|| "terminal-status".to_owned())
                            .absolute()
                            .right(px(HORIZONTAL_PADDING))
                            .bottom(px(BOTTOM_PADDING))
                            .max_w(relative(0.94))
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(gpui_color(status_color))
                            .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
                            .text_color(gpui_color(ACTIVE_THEME.text))
                            .text_sm()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(8.0))
                                    .child(Icon::new(
                                        status_icon,
                                        px(14.0),
                                        gpui_color(status_color),
                                    ))
                                    .child(div().min_w_0().whitespace_normal().child(status)),
                            )
                            .when(recovery_available, |status| {
                                status.child(
                                    Button::new("retry-terminal-recovery", "Retry")
                                        .variant(ButtonVariant::Link)
                                        .size(ButtonSize::Compact)
                                        .debug_selector("retry-terminal-recovery")
                                        .on_activate(move |_, window, cx| {
                                            let _ = retry_pane.update(cx, |pane, cx| {
                                                pane.retry_recovery(window, cx);
                                            });
                                        }),
                                )
                            })
                            .when(diagnostics_available, |status| {
                                status.child(
                                    Button::new(
                                        "export-terminal-diagnostics",
                                        "Export Diagnostics",
                                    )
                                    .variant(ButtonVariant::Link)
                                    .size(ButtonSize::Compact)
                                    .debug_selector("export-terminal-diagnostics")
                                    .on_activate(
                                        move |_, window, cx| {
                                            let _ = export_pane.update(cx, |pane, cx| {
                                                pane.export_diagnostics(
                                                    &ExportTerminalDiagnostics,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        },
                                    ),
                                )
                            }),
                    )
                },
            )
            .child(
                div()
                    .absolute()
                    .left(px(HORIZONTAL_PADDING))
                    .right(px(HORIZONTAL_PADDING))
                    .top(px(TOP_PADDING))
                    .bottom(px(BOTTOM_PADDING))
                    .child(context_menu),
            )
            .into_any_element()
    }
}

fn render_paste_confirmation(
    confirmation: PasteConfirmation,
    pane: gpui::WeakEntity<TerminalPane>,
) -> impl IntoElement {
    let cancel_pane = pane.clone();
    let explanation = if confirmation.risk.control_bytes || confirmation.risk.closing_fence {
        "This text contains control sequences that may change terminal behavior or execute commands."
    } else {
        "Pasting multiple lines may execute commands in your shell."
    };

    div()
        .debug_selector(|| "unsafe-paste-confirmation".to_owned())
        .absolute()
        .left(px(16.0))
        .right(px(16.0))
        .bottom(px(16.0))
        .flex()
        .flex_col()
        .items_start()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(gpui_color(ACTIVE_THEME.warning_border))
        .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
        .text_color(gpui_color(ACTIVE_THEME.text))
        .text_sm()
        .occlude()
        .child(div().w_full().whitespace_normal().child(format!(
            "Paste {} bytes across {} lines? {explanation}",
            confirmation.byte_len, confirmation.line_count
        )))
        .child(
            div()
                .w_full()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    Button::new("cancel-unsafe-paste", "Cancel")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Small)
                        .role(ButtonRole::Cancel)
                        .debug_selector("cancel-unsafe-paste")
                        .on_activate(move |_, window, cx| {
                            let _ = cancel_pane.update(cx, |pane, cx| {
                                pane.cancel_unsafe_paste(&CancelUnsafePaste, window, cx);
                            });
                        }),
                )
                .child(
                    Button::new("confirm-unsafe-paste", "Paste")
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Small)
                        .debug_selector("confirm-unsafe-paste")
                        .on_activate(move |_, window, cx| {
                            let _ = pane.update(cx, |pane, cx| {
                                pane.confirm_unsafe_paste(&ConfirmUnsafePaste, window, cx);
                            });
                        }),
                ),
        )
}

fn terminal_font(cx: &App) -> SharedString {
    let font_names = cx.text_system().all_font_names();
    select_terminal_font(&font_names).into()
}

fn select_terminal_font(font_names: &[String]) -> &'static str {
    [
        "JetBrainsMono Nerd Font",
        "JetBrainsMono Nerd Font Mono",
        "JetBrains Mono",
        "Menlo",
    ]
    .into_iter()
    .find(|candidate| {
        font_names
            .iter()
            .any(|available| available.eq_ignore_ascii_case(candidate))
    })
    .unwrap_or("Menlo")
}

fn measure_cell_width(window: &mut Window, family: &SharedString, font_size: f32) -> Pixels {
    let run = TextRun {
        len: 1,
        font: font(family.clone()),
        color: gpui_color(ACTIVE_THEME.terminal_foreground).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line("M".into(), px(font_size), &[run], None)
        .width
}

fn line_height_for_font_size(font_size: f32) -> f32 {
    font_size * DEFAULT_LINE_HEIGHT / DEFAULT_FONT_SIZE
}

fn terminal_geometry(
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: f32,
    backing_scale: BackingScale,
) -> TerminalGeometry {
    let width = f32::from(bounds.size.width).max(f32::from(cell_width));
    let height = f32::from(bounds.size.height).max(line_height);

    TerminalGeometry::from_viewport(
        LogicalSize::new(width, height),
        LogicalCellSize::new(f32::from(cell_width), line_height),
        backing_scale,
        CellGridSize::new(MIN_COLS, MIN_ROWS),
    )
}

fn ime_candidate_bounds(
    element_bounds: Bounds<Pixels>,
    columns: usize,
    cell_width: Pixels,
    line_height: Pixels,
    caret: PreeditPosition,
) -> Bounds<Pixels> {
    let grid_left = terminal_grid_content_bounds(element_bounds, columns, cell_width).left();
    Bounds::new(
        point(
            grid_left + cell_width * caret.column as f32,
            element_bounds.top() + line_height * caret.row as f32,
        ),
        size(cell_width, line_height),
    )
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

/// The identity one Pane caption presents: where its Terminal runs, where it is, and what it runs.
pub(crate) struct PaneCaptionFacts {
    pub(crate) origin: PaneOrigin,
    pub(crate) directory: SharedString,
    pub(crate) label: SharedString,
    pub(crate) running: bool,
}

/// The account and machine one Pane runs on, split so a caption can emphasize each part.
///
/// `remote` is the Local or Remote classification itself, never inferred from the spelling of
/// `host`. A Remote destination that names no account leaves `user` empty.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct PaneOrigin {
    pub(crate) user: SharedString,
    pub(crate) host: SharedString,
    pub(crate) remote: bool,
}

impl PaneOrigin {
    fn from_context(context: &crate::terminal::metadata::TerminalMetadataContext) -> Self {
        use crate::terminal::metadata::{TerminalOrigin, sanitize_title};

        match context.origin() {
            TerminalOrigin::Local { user, host } => Self {
                user: sanitize_title(user.unwrap_or_default()).into(),
                host: sanitize_title(short_hostname(host.unwrap_or_default())).into(),
                remote: false,
            },
            TerminalOrigin::Remote { destination } => {
                let (user, host) = match destination.rsplit_once('@') {
                    Some((user, host)) => (user, host),
                    None => ("", destination),
                };
                Self {
                    user: sanitize_title(user).into(),
                    host: sanitize_title(short_hostname(host)).into(),
                    remote: true,
                }
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.user.is_empty() && self.host.is_empty()
    }
}

/// Drops the multicast DNS suffix a host may append to a machine name.
///
/// Every other spelling, including an address, is presented exactly as reported.
fn short_hostname(host: &str) -> &str {
    host.strip_suffix(".local")
        .filter(|short| !short.is_empty())
        .unwrap_or(host)
}

/// Abbreviates a displayed local directory against the local home spelling.
///
/// Remote directories have no local home and are returned unchanged.
fn compact_home_directory(directory: &str, home: Option<&str>) -> String {
    let Some(home) = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty() && directory.starts_with(home))
    else {
        return directory.to_owned();
    };
    match &directory[home.len()..] {
        "" | "/" => "~".to_owned(),
        rest if rest.starts_with('/') => format!("~{rest}"),
        _ => directory.to_owned(),
    }
}

fn normalized_pane_title(reported_title: &str, fallback_title: &str) -> String {
    let reported = reported_title
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_PANE_TITLE_CHARACTERS)
        .collect::<String>();
    if !reported.is_empty() {
        return reported;
    }

    let fallback = fallback_title
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_PANE_TITLE_CHARACTERS)
        .collect::<String>();
    if fallback.is_empty() {
        "Terminal".to_owned()
    } else {
        fallback
    }
}

fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Left),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Right => Some(PointerButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

fn input_modifiers(modifiers: gpui::Modifiers) -> InputModifiers {
    InputModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
        platform: modifiers.platform,
        ..InputModifiers::default()
    }
}

fn pointer_uses_text_cursor(
    mouse_tracking: bool,
    shift: bool,
    shift_selection: ShiftSelectionPolicy,
) -> bool {
    !mouse_tracking || (shift && shift_selection == ShiftSelectionPolicy::OverrideApplicationMouse)
}

fn opens_terminal_context_menu(
    button: PointerButton,
    mouse_tracking: bool,
    shift: bool,
    shift_selection: ShiftSelectionPolicy,
) -> bool {
    button == PointerButton::Right
        && pointer_uses_text_cursor(mouse_tracking, shift, shift_selection)
}

fn terminal_surface_position(
    bounds: Bounds<Pixels>,
    position: gpui::Point<Pixels>,
    geometry: TerminalGeometry,
    allow_outside: bool,
) -> Option<SurfacePosition> {
    if !allow_outside && !bounds.contains(&position) {
        return None;
    }

    let local_x = f32::from(position.x - bounds.origin.x);
    let local_y = f32::from(position.y - bounds.origin.y);
    let backing = geometry.to_backing_position(LogicalPosition::new(local_x, local_y));
    Some(SurfacePosition {
        x: backing.x,
        y: backing.y,
    })
}

fn hovered_link_for_generation(
    hovered: Option<&HoveredTerminalLink>,
    current_generation: crate::terminal::PresentationGeneration,
) -> Option<&HoveredTerminalLink> {
    hovered.filter(|hovered| hovered.generation == current_generation)
}

fn active_hovered_link(
    hovered: Option<&HoveredTerminalLink>,
    current_generation: crate::terminal::PresentationGeneration,
    platform_modifier: bool,
) -> Option<&HoveredTerminalLink> {
    platform_modifier
        .then(|| hovered_link_for_generation(hovered, current_generation))
        .flatten()
}

#[derive(Default)]
struct WheelAccumulator {
    horizontal: f32,
    vertical: f32,
}

impl WheelAccumulator {
    fn push(&mut self, horizontal: f32, vertical: f32, phase: WheelPhase) -> (i32, i32) {
        if phase == WheelPhase::GestureStarted {
            *self = Self::default();
        }
        self.horizontal += horizontal;
        self.vertical += vertical;
        let steps = (self.horizontal.trunc() as i32, self.vertical.trunc() as i32);
        self.horizontal -= steps.0 as f32;
        self.vertical -= steps.1 as f32;
        if matches!(
            phase,
            WheelPhase::GestureEnded
                | WheelPhase::GestureCancelled
                | WheelPhase::MomentumEnded
                | WheelPhase::MomentumCancelled
        ) {
            *self = Self::default();
        }
        steps
    }
}

#[cfg(test)]
#[path = "terminal_pane/tests.rs"]
mod tests;
