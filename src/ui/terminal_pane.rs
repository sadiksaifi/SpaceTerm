use std::cell::Cell;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::render_lifecycle::{RenderLifecycle, ScaleChange, SurfaceVisibility};
use super::terminal_context_menu::{TerminalContextMenuCommand, terminal_context_menu_entries};
use super::terminal_element::PaintPreflightFault;
use super::terminal_element::{
    TerminalGridCache, TerminalGridConfiguration, TerminalGridElement, terminal_grid_content_bounds,
};
use super::terminal_focus::{
    TerminalFocusBlocker, TerminalFocusCoordinator, TerminalFocusFacts, TerminalProductFocus,
};
use super::terminal_graphics::{
    GraphicsAttemptToken, GraphicsRollbackProof, TerminalGraphicsCache,
};
use super::terminal_ime::{PreeditLayout, PreeditPosition, TerminalIme, layout_preedit};
use super::{
    AllowOsc52Clipboard, CancelUnsafePaste, CloseTerminalFind, ConfirmUnsafePaste, CopySelection,
    DecreaseTerminalFontSize, DenyOsc52Clipboard, ExportTerminalDiagnostics, FindNext,
    FindPrevious, FocusNextTerminalFindControl, FocusPreviousTerminalFindControl,
    IncreaseTerminalFontSize, OpenTerminalFind, PasteClipboard, ResetTerminalFontSize,
    TERMINAL_FIND_KEY_CONTEXT, TERMINAL_KEY_CONTEXT, TERMINAL_OSC52_AUTHORIZATION_KEY_CONTEXT,
    TERMINAL_PASTE_CONFIRMATION_KEY_CONTEXT,
};
use crate::domain::{PaneId, WindowId, WorkspaceId};
use crate::platform::acceptance_observation::{
    FailureActionCase, FailureActionController, FailureActionEvent, FailureActionPhase,
    FailureActionRequest, FailureActionResult, FailurePaneState, FailurePendingRecovery,
};
use crate::platform::macos_accessibility::{MacosAccessibilityElement, MacosAccessibilityUpdate};
#[cfg(not(test))]
use crate::platform::macos_application;
use crate::platform::macos_attention::{
    AttentionPaneId, AttentionSchedules, reconcile_scheduled as reconcile_attention_schedule,
    register_pane as register_attention_pane, remove_pane as remove_attention_pane,
    update_application_activation as update_attention_application_activation,
};
#[cfg(not(test))]
use crate::platform::macos_attention::{MacosAttentionPlatform, apply_attention_effects};
use crate::platform::macos_keyboard::{
    KeyTranslation, MacosKeyboardBridge, NativeKeyEvent, NativeKeyEventKind, UnhandledKeyEvent,
};
use crate::platform::macos_pasteboard::read_file_urls;
use crate::platform::macos_quick_look::{MacosQuickLook, QuickLookPlatform};
use crate::platform::macos_render_lifecycle::{
    NativeWindowVisibility, NativeWindowVisibilitySource, current_window_visibility,
};
use crate::platform::macos_scroll::current_wheel_phase;
use crate::platform::macos_secure_input::{
    SecureInputPaneId, register_pane as register_secure_input_pane,
    remove_pane as remove_secure_input_pane,
    update_application_activation as update_secure_input_application_activation,
    update_pane as update_secure_input_pane,
};
use crate::terminal::attention::AttentionState;
use crate::terminal::geometry::{
    BackingPosition, BackingScale, CellGridSize, LogicalCellSize, LogicalPosition, LogicalSize,
    TerminalGeometry,
};
use crate::terminal::{
    AccessibilityGeometry, AccessibilityNotification, AccessibilityNotifications, AttentionFacts,
    DiagnosticBundle, DiagnosticKeyEventKind, FindDirection, FindQueryGeneration, InputModifiers,
    KeyAction, KeyInput, NativeContextActions, NativeInsertion, NativeServiceCapabilities,
    NativeServiceOrigin, NativeServiceStatus, OptionAsAltPolicy, Osc52Access,
    Osc52AuthorizationDecision, Osc52AuthorizationRequest, Osc52Target, PaneTerminalState,
    PasteConfirmation, PasteDecision, PasteRequestOutcome, PasteResolution, PhysicalKey,
    PointerButton, PointerInput, PointerPhase, PreparedWorkspaceTerminalLaunch, QuickLookTarget,
    RemoteChannelUnavailable, ScreenSnapshot, SelectionCopy, SelectionCopyError, SessionEvent,
    ShiftSelectionPolicy, SurfacePosition, TerminalAccessibilityModel, TerminalFailure,
    TerminalLocalFileCapabilities, TerminalSessionHandle, UnhandledKeyDiagnostic, WheelInput,
    WheelPhase, WorkspaceTerminalSessionFactory,
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
use gpui_symbols::{Icon, SymbolWeight};
use spaceterm_ui::{
    Button, ButtonRole, ButtonSize, ButtonVariant, ContextMenu, EditCopy, EditPaste, IconButton,
    MenuLifecycleEvent, MenuSize, OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics,
    TextInput, TextInputEvent, TextInputTabBehavior, TextInputVariant, Tooltip,
    window_modal_is_open,
};

const DEFAULT_FONT_SIZE: f32 = 18.0;
const DEFAULT_LINE_HEIGHT: f32 = 20.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 32.0;
const FONT_SIZE_STEP: f32 = 1.0;
const HORIZONTAL_PADDING: f32 = 4.0;
const VERTICAL_PADDING: f32 = 8.0;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
const MAX_PANE_TITLE_CHARACTERS: usize = 256;
const PRESENTATION_BLINK_INTERVAL: Duration = Duration::from_millis(600);
const VISUAL_BELL_DURATION: Duration = Duration::from_millis(120);
const RUNTIME_VISIBILITY_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[cfg(test)]
fn current_application_active(cx: &App) -> bool {
    cx.active_window().is_some()
}

#[cfg(not(test))]
fn current_application_active(_: &App) -> bool {
    macos_application::is_active()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeActivity {
    application_active: bool,
    operating_system_window_key: bool,
}

impl NativeActivity {
    fn current(window: &Window, cx: &App) -> Self {
        Self {
            application_active: current_application_active(cx),
            operating_system_window_key: window.is_window_active(),
        }
    }
}

fn terminal_surface_active(product_focus: TerminalProductFocus, activity: NativeActivity) -> bool {
    product_focus.active_workspace
        && product_focus.active_window
        && activity.operating_system_window_key
}

fn apply_native_attention(
    pane: AttentionPaneId,
    effects: crate::terminal::attention::AttentionEffects,
    cx: &mut Context<TerminalPane>,
) {
    #[cfg(not(test))]
    schedule_attention_retries(
        apply_attention_effects(&mut MacosAttentionPlatform::new(pane), effects),
        cx,
    );
    #[cfg(test)]
    let _ = (pane, effects, cx);
}

fn schedule_attention_retries(schedules: AttentionSchedules, cx: &mut Context<TerminalPane>) {
    for schedule in schedules.into_array().into_iter().flatten() {
        cx.spawn(async move |_, cx| {
            let mut schedule = schedule;
            loop {
                cx.background_executor()
                    .timer(schedule.delay_from(Instant::now()))
                    .await;
                let Some(next) = reconcile_attention_schedule(schedule)
                    .into_array()
                    .into_iter()
                    .flatten()
                    .next()
                else {
                    break;
                };
                schedule = next;
            }
        })
        .detach();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalPaneEvent {
    FocusRequested,
    TitleChanged(SharedString),
    ReportedWorkingDirectoryChanged(PathBuf),
    AttentionChanged { unread_count: u32 },
    Exited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PasteRequestGuard {
    session_identity: u64,
    focus_epoch: u64,
    hierarchy_generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct TerminalContextMenuState {
    generation: crate::terminal::PresentationGeneration,
    position: SurfacePosition,
    link: Option<crate::terminal::HyperlinkTarget>,
    selection_present: bool,
    quick_look_eligible: bool,
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

#[derive(Default)]
struct SelectionPasteboard {
    fail_next_write: bool,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum RemotePaneLifecycleError {
    #[error("the Pane does not own a remote Terminal Session")]
    LocalPane,
    #[error("remote connection generation {received} is stale; current generation is {current}")]
    StaleGeneration { current: u64, received: u64 },
    #[error("the remote Pane is not disconnected")]
    NotDisconnected,
    #[error("the prepared remote restart no longer matches the Pane session epoch")]
    SessionChanged,
    #[error(transparent)]
    ChannelUnavailable(#[from] RemoteChannelUnavailable),
}

pub(crate) struct PreparedRemotePaneRestart {
    session_factory: WorkspaceTerminalSessionFactory,
    prepared_launch: PreparedWorkspaceTerminalLaunch,
    generation: u64,
    expected_epoch: u64,
}

impl SelectionPasteboard {
    fn write(&mut self, copy: SelectionCopy, cx: &mut App) -> Result<(), String> {
        if std::mem::take(&mut self.fail_next_write) {
            return Err("injected native pasteboard failure".to_owned());
        }
        write_selection_copy(copy, cx)
    }

    fn fail_next_write(&mut self) {
        self.fail_next_write = true;
    }
}

pub(crate) struct TerminalPane {
    session_factory: WorkspaceTerminalSessionFactory,
    prepared_launch: Option<PreparedWorkspaceTerminalLaunch>,
    local_file_capabilities: TerminalLocalFileCapabilities,
    session: Option<Box<dyn TerminalSessionHandle>>,
    session_start_attempted: bool,
    session_epoch: u64,
    accepted_screen_generation: Option<crate::terminal::PresentationGeneration>,
    remote_connection_generation: Option<u64>,
    remote_input_blocked: bool,
    remote_restart_start_pending: bool,
    acceptance_observation_claimed: bool,
    runtime_observation: Option<crate::terminal::RuntimeObservation>,
    failure_actions: Option<FailureActionController>,
    failure_action_request: Option<FailureActionRequest>,
    failure_action_trigger_pending: bool,
    failure_action_recovery_frame: Option<(u64, crate::terminal::PresentationGeneration)>,
    failure_action_resource_rollback: GraphicsRollbackProof,
    native_service_session_identity: u64,
    native_service_focus_epoch: Cell<u64>,
    native_service_hierarchy_generation: u64,
    screen: Arc<ScreenSnapshot>,
    screen_session_epoch: u64,
    last_valid_screen: Arc<ScreenSnapshot>,
    last_valid_screen_session_epoch: u64,
    accessibility: Arc<TerminalAccessibilityModel>,
    accessibility_element: MacosAccessibilityElement,
    pending_accessibility_notifications: AccessibilityNotifications,
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
    native_modal_open: bool,
    terminal_input_focus: bool,
    surface_active: bool,
    application_active: bool,
    attention: AttentionState,
    attention_visual: bool,
    attention_generation: u64,
    native_attention_pane: Option<AttentionPaneId>,
    hidden_input: bool,
    secure_input_pane: Option<SecureInputPaneId>,
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    cell_width: Pixels,
    backing_scale: BackingScale,
    last_geometry: Option<TerminalGeometry>,
    grid_bounds: Option<Bounds<Pixels>>,
    pressed_button: Option<PointerButton>,
    pointer_modifiers: InputModifiers,
    shift_selection: ShiftSelectionPolicy,
    wheel_accumulator: WheelAccumulator,
    scrollbar: Entity<OverlayScrollbar<u64>>,
    render_cache: Entity<TerminalGridCache>,
    fallback_render_cache: Entity<TerminalGridCache>,
    paint_fault: Option<PaintPreflightFault>,
    graphics_cache: Entity<TerminalGraphicsCache>,
    selection_pasteboard: SelectionPasteboard,
    keyboard_bridge: MacosKeyboardBridge,
    ime: TerminalIme,
    preedit_layout: Option<PreeditLayout>,
    preedit_layout_key: Option<PreeditLayoutKey>,
    marked_revision: u64,
    ime_suppressed_keys: Vec<PhysicalKey>,
    pending_file_insertion: Option<NativeInsertion>,
    pending_paste: Option<PasteConfirmation>,
    pending_osc52: Option<Osc52AuthorizationRequest>,
    hovered_link: Option<(
        crate::terminal::PresentationGeneration,
        crate::terminal::HyperlinkTarget,
    )>,
    pressed_link: Option<(
        crate::terminal::PresentationGeneration,
        crate::terminal::HyperlinkTarget,
    )>,
    quick_look: Box<dyn QuickLookPlatform>,
    context_menu: Option<TerminalContextMenuState>,
    blink_phase_visible: bool,
    blink_generation: u64,
    _blink_task: Option<Task<()>>,
    _attention_task: Option<Task<()>>,
    _event_task: Option<Task<()>>,
    _accessibility_task: Option<Task<()>>,
    runtime_visibility_source: Option<NativeWindowVisibilitySource>,
    _runtime_visibility_task: Option<Task<()>>,
    _failure_action_task: Option<Task<()>>,
}

impl TerminalPane {
    #[cfg(test)]
    pub(crate) fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prepared_launch = session_factory.prepare_child_launch().ok();
        Self::new_with_quick_look(
            session_factory,
            prepared_launch,
            Box::new(MacosQuickLook::default()),
            window,
            cx,
        )
    }

    pub(crate) fn new_with_prepared_launch(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_quick_look(
            session_factory,
            Some(prepared_launch),
            Box::new(MacosQuickLook::default()),
            window,
            cx,
        )
    }

    fn new_with_quick_look(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: Option<PreparedWorkspaceTerminalLaunch>,
        quick_look: Box<dyn QuickLookPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family, DEFAULT_FONT_SIZE);
        let backing_scale = BackingScale::new(window.scale_factor()).unwrap_or(BackingScale::ONE);
        let fallback_title: SharedString =
            normalized_pane_title("", &session_factory.fallback_title()).into();
        let local_file_capabilities = session_factory.local_file_capabilities();
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("terminal-scrollbar"));
        let render_cache = cx.new(|_| TerminalGridCache::new());
        let fallback_render_cache = cx.new(|_| TerminalGridCache::new());
        let graphics_cache = cx.new(|_| TerminalGraphicsCache::default());
        cx.on_release(|pane, cx| {
            pane.graphics_cache.update(cx, |cache, cx| cache.clear(cx));
        })
        .detach();
        let screen = ScreenSnapshot::empty();
        let accessibility = Arc::new(TerminalAccessibilityModel::from_screen(&screen));
        let accessibility_element = MacosAccessibilityElement::new(
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
                    if let Some(session) = &pane.session {
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
            pane.sync_terminal_input_focus(window, cx);
            cx.notify();
        })
        .detach();
        cx.on_focus(&focus_handle, window, |pane, window, cx| {
            pane.sync_terminal_input_focus(window, cx);
            cx.notify();
        })
        .detach();
        cx.on_blur(&focus_handle, window, |pane, window, cx| {
            pane.sync_terminal_input_focus(window, cx);
            cx.notify();
        })
        .detach();

        Self {
            session_factory,
            prepared_launch,
            local_file_capabilities,
            session: None,
            session_start_attempted: false,
            session_epoch: 0,
            accepted_screen_generation: None,
            remote_connection_generation: None,
            remote_input_blocked: false,
            remote_restart_start_pending: false,
            acceptance_observation_claimed: false,
            runtime_observation: None,
            failure_actions: None,
            failure_action_request: None,
            failure_action_trigger_pending: false,
            failure_action_recovery_frame: None,
            failure_action_resource_rollback: GraphicsRollbackProof::default(),
            native_service_session_identity: 0,
            native_service_focus_epoch: Cell::new(0),
            native_service_hierarchy_generation: 0,
            screen_session_epoch: 0,
            last_valid_screen: Arc::clone(&screen),
            last_valid_screen_session_epoch: 0,
            screen,
            accessibility,
            accessibility_element,
            pending_accessibility_notifications: AccessibilityNotifications::default(),
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
            native_modal_open: false,
            terminal_input_focus: false,
            surface_active: false,
            application_active: false,
            attention: AttentionState::default(),
            attention_visual: false,
            attention_generation: 0,
            native_attention_pane: Some(register_attention_pane()),
            hidden_input: false,
            secure_input_pane: Some(register_secure_input_pane()),
            font_family,
            font_size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
            cell_width,
            backing_scale,
            last_geometry: None,
            grid_bounds: None,
            pressed_button: None,
            pointer_modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
            wheel_accumulator: WheelAccumulator::default(),
            scrollbar,
            render_cache,
            fallback_render_cache,
            paint_fault: None,
            graphics_cache,
            selection_pasteboard: SelectionPasteboard::default(),
            keyboard_bridge: MacosKeyboardBridge::new(OptionAsAltPolicy::default()),
            ime: TerminalIme::default(),
            preedit_layout: None,
            preedit_layout_key: None,
            marked_revision: 0,
            ime_suppressed_keys: Vec::new(),
            pending_file_insertion: None,
            pending_paste: None,
            pending_osc52: None,
            hovered_link: None,
            pressed_link: None,
            quick_look,
            context_menu: None,
            blink_phase_visible: true,
            blink_generation: 0,
            _blink_task: None,
            _attention_task: None,
            _event_task: None,
            _accessibility_task: None,
            runtime_visibility_source: None,
            _runtime_visibility_task: None,
            _failure_action_task: None,
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

    pub(crate) fn set_product_focus(&mut self, product_focus: TerminalProductFocus) -> bool {
        if self.product_focus == product_focus {
            return false;
        }
        self.native_service_hierarchy_generation =
            self.native_service_hierarchy_generation.wrapping_add(1);
        if self.product_focus.focused_pane && !product_focus.focused_pane {
            self.end_find_state();
        }
        let native_service_blocked = !product_focus.active_workspace
            || !product_focus.active_window
            || !product_focus.focused_pane
            || product_focus.blocker.is_some();
        let pane_inactive = !product_focus.active_workspace
            || !product_focus.active_window
            || !product_focus.focused_pane;
        if pane_inactive {
            self.quick_look.dismiss();
        }
        if native_service_blocked {
            self.pending_file_insertion = None;
            self.context_menu = None;
        }
        if native_service_blocked
            && let Some(confirmation) = self.pending_paste.take()
            && let Some(session) = &self.session
        {
            let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
        }
        if native_service_blocked
            && let Some(request) = self.pending_osc52.take()
            && let Some(session) = &self.session
        {
            session.resolve_osc52_authorization(request.id, Osc52AuthorizationDecision::Deny);
        }
        self.product_focus = product_focus;
        let pane_visible = product_focus.active_window && product_focus.pane_visible;
        let _ = self
            .render_lifecycle
            .update_product_visibility(product_focus.active_workspace, pane_visible);
        if let Some(observation) = &self.runtime_observation {
            observation.product_visibility(product_focus.active_workspace, pane_visible);
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
        if let Some(session) = &self.session
            && self.find_input.is_some()
        {
            session.navigate_find(self.find_generation, FindDirection::Next);
        }
    }

    fn find_previous(&mut self, _: &FindPrevious, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Some(session) = &self.session
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
        if let Some(session) = &self.session {
            session.end_find(self.find_generation);
        }
    }

    fn find_query_changed(&mut self, query: String) {
        if self.find_input.is_none() {
            return;
        }
        self.find_generation = self.find_generation.next();
        if let Some(session) = &self.session {
            session.set_find_query(self.find_generation, query);
        }
    }

    pub(crate) fn terminal_input_focused(&self, window: &Window, cx: &App) -> bool {
        self.terminal_input_focused_with_activity(
            window,
            NativeActivity::current(window, cx),
            window_modal_is_open(window, cx),
        )
    }

    fn terminal_input_focused_with_activity(
        &self,
        window: &Window,
        activity: NativeActivity,
        modal_open: bool,
    ) -> bool {
        if self.remote_input_blocked {
            return false;
        }
        TerminalFocusCoordinator::is_focused(TerminalFocusFacts {
            active_workspace: self.product_focus.active_workspace,
            active_window: self.product_focus.active_window,
            focused_pane: self.product_focus.focused_pane,
            responder: self.focus_handle.is_focused(window),
            operating_system_window_key: activity.operating_system_window_key,
            application_active: activity.application_active,
            blocker: modal_open
                .then_some(TerminalFocusBlocker::Modal)
                .or(self
                    .native_modal_open
                    .then_some(TerminalFocusBlocker::Modal))
                .or_else(|| {
                    self.context_menu
                        .is_some()
                        .then_some(TerminalFocusBlocker::ContextMenu)
                })
                .or(self.product_focus.blocker),
        })
    }

    fn sync_terminal_input_focus(&mut self, window: &Window, cx: &App) -> (bool, bool) {
        self.sync_terminal_input_focus_with_activity_and_modal(
            window,
            NativeActivity::current(window, cx),
            window_modal_is_open(window, cx),
        )
    }

    fn sync_terminal_input_focus_with_activity_and_modal(
        &mut self,
        window: &Window,
        activity: NativeActivity,
        modal_open: bool,
    ) -> (bool, bool) {
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
                self.keyboard_bridge.reset_pressed_modifiers();
            }
            if focused {
                self.pending_accessibility_notifications
                    .insert(AccessibilityNotification::Focus);
            }
            self.reset_blink_phase();
            if !focused {
                if let Some(confirmation) = self.pending_paste.take()
                    && let Some(session) = &self.session
                {
                    let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
                }
                if let Some(request) = self.pending_osc52.take()
                    && let Some(session) = &self.session
                {
                    session
                        .resolve_osc52_authorization(request.id, Osc52AuthorizationDecision::Deny);
                }
                self.ime.cancel();
                self.invalidate_preedit_layout();
                self.ime_suppressed_keys.clear();
            }
            if let Some(session) = &self.session {
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
            apply_native_attention(pane, effects, cx);
        }
        cx.emit(TerminalPaneEvent::AttentionChanged { unread_count: 0 });
    }

    fn start_visual_bell(&mut self, cx: &mut Context<Self>) {
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
        if let Some(id) = self.secure_input_pane {
            update_secure_input_pane(id, self.hidden_input, self.terminal_input_focus);
        }
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

    fn observe_presented_frame(
        &mut self,
        generation: crate::terminal::PresentationGeneration,
        rows: u16,
        columns: u16,
    ) {
        if !self.acceptance_observation_claimed || !self.render_lifecycle.is_presented(generation) {
            return;
        }
        if let Some(observation) = &self.runtime_observation {
            observation.next_frame(generation.as_u64());
        }
        if self.failure_action_request.as_ref().is_some_and(|request| {
            self.failure_action_recovery_frame == Some((request.sequence, generation))
                && !self.failure_action_trigger_pending
                && matches!(
                    request.case,
                    FailureActionCase::PresentationInvalidScale
                        | FailureActionCase::PresentationGlyph
                        | FailureActionCase::RendererImagePreflight
                        | FailureActionCase::RendererResourceBeforeSync
                        | FailureActionCase::RendererResourceAfterStaging
                )
        }) {
            self.complete_failure_action(FailureActionResult::Recovered);
        }
        if let Some(observation) =
            crate::platform::acceptance_observation::prepare_once(rows, columns)
            && let Err(error) = observation.emit()
        {
            eprintln!("failed to emit acceptance observation: {error}");
        }
    }

    fn schedule_presented_frame_observation(
        &self,
        generation: crate::terminal::PresentationGeneration,
        rows: u16,
        columns: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.acceptance_observation_claimed {
            return;
        }
        #[cfg(not(test))]
        {
            let pane = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = pane.update(cx, |pane, _| {
                    pane.observe_presented_frame(generation, rows, columns);
                });
            });
            cx.notify();
        }
        #[cfg(test)]
        cx.defer_in(window, move |pane, _, _| {
            pane.observe_presented_frame(generation, rows, columns);
        });
    }

    pub(crate) fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub(crate) fn reported_working_directory(&self) -> Option<PathBuf> {
        use crate::terminal::metadata::MetadataFreshness;

        (self.screen.metadata.context.is_local()
            && self.screen.metadata.freshness == MetadataFreshness::Live)
            .then(|| PathBuf::from(self.screen.metadata.directory.path.as_ref()))
            .filter(|path| path.is_absolute())
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
        (self.remote_input_blocked, self.session.is_some())
    }

    #[cfg(test)]
    pub(crate) fn restart_state(&self) -> (bool, Option<&'static str>) {
        (
            self.session.is_some(),
            self.pane_state
                .failure()
                .map(crate::terminal::TerminalFailure::operation),
        )
    }

    pub(crate) fn close(&mut self) {
        self.end_find_state();
        if let Some(observation) = &self.runtime_observation {
            observation.pane_released();
        }
        if let Some(confirmation) = self.pending_paste.take()
            && let Some(session) = &self.session
        {
            let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
        }
        if let Some(request) = self.pending_osc52.take()
            && let Some(session) = &self.session
        {
            session.resolve_osc52_authorization(request.id, Osc52AuthorizationDecision::Deny);
        }
        self.blink_generation = self.blink_generation.wrapping_add(1);
        self.attention_generation = self.attention_generation.wrapping_add(1);
        self._blink_task.take();
        self._attention_task.take();
        self._event_task.take();
        self._accessibility_task.take();
        self._runtime_visibility_task.take();
        self.runtime_visibility_source.take();
        self.render_lifecycle.release();
        self.context_menu = None;
        self.quick_look.dismiss();
        self.accessibility_element.set_hierarchy(false, usize::MAX);
        let session_was_attached = self.session.take().is_some();
        if self.failure_action_request.as_ref().is_some_and(|request| {
            matches!(
                request.case,
                FailureActionCase::PtyFatal | FailureActionCase::EmulatorFatal
            )
        }) && self
            .pane_state
            .failure()
            .is_some_and(TerminalFailure::is_fatal)
        {
            self.emit_failure_action(FailureActionPhase::Completed, FailureActionResult::Closed);
            self.failure_action_request = None;
            self.failure_action_trigger_pending = false;
            self.failure_action_recovery_frame = None;
        }
        if session_was_attached {
            self.native_service_session_identity =
                self.native_service_session_identity.wrapping_add(1);
        }
        self._failure_action_task.take();
        self.failure_actions.take();
        if let Some(id) = self.secure_input_pane.take() {
            remove_secure_input_pane(id);
        }
        if let Some(id) = self.native_attention_pane.take() {
            remove_attention_pane(id);
        }
    }

    fn validate_remote_generation(&self, generation: u64) -> Result<(), RemotePaneLifecycleError> {
        if !self.session_factory.is_remote() {
            return Err(RemotePaneLifecycleError::LocalPane);
        }
        if let Some(current) = self.remote_connection_generation
            && generation < current
        {
            return Err(RemotePaneLifecycleError::StaleGeneration {
                current,
                received: generation,
            });
        }
        Ok(())
    }

    pub(crate) fn can_disconnect_remote(
        &self,
        generation: u64,
    ) -> Result<(), RemotePaneLifecycleError> {
        self.validate_remote_generation(generation)
    }

    pub(crate) fn disconnect_remote(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneLifecycleError> {
        self.validate_remote_generation(generation)?;
        if self.remote_connection_generation == Some(generation) && self.remote_input_blocked {
            return Ok(());
        }
        self.remote_connection_generation = Some(generation);
        self.suspend_remote_session(cx);
        Ok(())
    }

    fn suspend_remote_session(&mut self, cx: &mut Context<Self>) {
        self.remote_input_blocked = true;
        self.session_epoch = self.session_epoch.wrapping_add(1);
        self._event_task.take();
        self._accessibility_task.take();
        self.reset_hidden_input();
        self.apply_terminal_input_focus(false);
        self.context_menu = None;
        self.pressed_button = None;
        self.pressed_link = None;
        cx.notify();
    }

    fn suspend_if_remote_channel_unavailable(&mut self, cx: &mut Context<Self>) -> bool {
        if self.session_factory.remote_channel_is_ready() != Some(false) {
            return false;
        }
        self.suspend_remote_session(cx);
        true
    }

    pub(crate) fn prepare_remote_restart(
        &self,
        session_factory: WorkspaceTerminalSessionFactory,
        generation: u64,
    ) -> Result<PreparedRemotePaneRestart, RemotePaneLifecycleError> {
        self.validate_remote_generation(generation)?;
        if !self.remote_input_blocked {
            return Err(RemotePaneLifecycleError::NotDisconnected);
        }
        let Some(disconnected_generation) = self.remote_connection_generation else {
            return Err(RemotePaneLifecycleError::NotDisconnected);
        };
        if generation <= disconnected_generation {
            return Err(RemotePaneLifecycleError::StaleGeneration {
                current: disconnected_generation,
                received: generation,
            });
        }
        if !session_factory.is_remote() {
            return Err(RemotePaneLifecycleError::LocalPane);
        }
        let prepared_launch = session_factory.prepare_child_launch()?;
        Ok(PreparedRemotePaneRestart {
            session_factory,
            prepared_launch,
            generation,
            expected_epoch: self.session_epoch,
        })
    }

    pub(crate) fn can_commit_remote_restart(
        &self,
        prepared: &PreparedRemotePaneRestart,
    ) -> Result<(), RemotePaneLifecycleError> {
        if self.session_epoch != prepared.expected_epoch {
            return Err(RemotePaneLifecycleError::SessionChanged);
        }
        if !self.remote_input_blocked {
            return Err(RemotePaneLifecycleError::NotDisconnected);
        }
        let Some(disconnected_generation) = self.remote_connection_generation else {
            return Err(RemotePaneLifecycleError::NotDisconnected);
        };
        if prepared.generation <= disconnected_generation {
            return Err(RemotePaneLifecycleError::StaleGeneration {
                current: disconnected_generation,
                received: prepared.generation,
            });
        }
        Ok(())
    }

    pub(crate) fn commit_remote_restart(
        &mut self,
        prepared: PreparedRemotePaneRestart,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RemotePaneLifecycleError> {
        self.can_commit_remote_restart(&prepared)?;
        let restart_geometry = self.last_geometry;
        self.session_epoch = self.session_epoch.wrapping_add(1);
        self._event_task.take();
        self._accessibility_task.take();
        if let Some(observation) = self.runtime_observation.take() {
            observation.pane_released();
        }
        self._runtime_visibility_task.take();
        self.runtime_visibility_source.take();
        self.acceptance_observation_claimed = false;
        self.session.take();
        self.native_service_session_identity = self.native_service_session_identity.wrapping_add(1);
        self.session_factory = prepared.session_factory;
        self.prepared_launch = Some(prepared.prepared_launch);
        self.local_file_capabilities = self.session_factory.local_file_capabilities();
        self.fallback_title =
            normalized_pane_title("", &self.session_factory.fallback_title()).into();
        self.session_start_attempted = false;
        self.remote_connection_generation = Some(prepared.generation);
        self.reset_hidden_input();
        self.remote_input_blocked = false;
        self.remote_restart_start_pending = true;
        self.accepted_screen_generation = None;
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
                    if this.blink_generation != generation {
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
            VERTICAL_PADDING,
            f32::from(size.rows) * self.line_height,
            self.screen.scrollbar.total_rows,
            self.screen.scrollbar.visible_rows,
            self.screen.scrollbar.offset_rows,
        )
    }

    fn sync_scrollbar(&self, cx: &mut Context<Self>) {
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
        window: &mut Window,
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
        let rows = screen.size.rows;
        let columns = screen.size.cols;
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
            if !self.failure_action_trigger_pending
                && let Some(request) = &self.failure_action_request
            {
                self.failure_action_recovery_frame = Some((request.sequence, generation));
            }
            cx.notify();
        }
        self.schedule_presented_frame_observation(generation, rows, columns, window, cx);
    }

    fn record_successfully_presented_screen(&mut self, screen: Arc<ScreenSnapshot>) {
        if !Arc::ptr_eq(&screen, &self.screen) || self.screen_session_epoch != self.session_epoch {
            return;
        }
        if self.last_valid_screen_session_epoch != self.session_epoch
            || screen.generation >= self.last_valid_screen.generation
        {
            self.last_valid_screen = screen;
            self.last_valid_screen_session_epoch = self.session_epoch;
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
            self.emit_injected_failure_if_matching();
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
            self.emit_injected_failure_if_matching();
            cx.notify();
        }
    }

    fn retry_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_recovery else {
            return;
        };
        if self.failure_action_request.is_some() {
            self.emit_failure_action(
                FailureActionPhase::RetryRequested,
                FailureActionResult::Accepted,
            );
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
                self.session_start_attempted = false;
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
        if self.acceptance_observation_claimed {
            crate::platform::acceptance_observation::update_geometry(observation_geometry(
                geometry,
            ));
        }
        self.sync_scrollbar(cx);

        if let Some(session) = &self.session {
            session.resize(geometry);
            return;
        }

        self.start_session(geometry, cx);
    }

    fn start_session(&mut self, geometry: TerminalGeometry, cx: &mut Context<Self>) {
        if self.session_start_attempted {
            return;
        }
        self.session_start_attempted = true;

        let claimed = crate::platform::acceptance_observation::claim_session(
            self.font_family.as_ref(),
            observation_geometry(geometry),
        );
        self.runtime_observation = claimed.as_ref().map(|claimed| claimed.runtime.clone());
        self.failure_actions = claimed.and_then(|claimed| claimed.failure_actions);
        self.acceptance_observation_claimed = self.runtime_observation.is_some();
        if self.acceptance_observation_claimed {
            self.start_runtime_visibility_monitor(cx);
            self.start_failure_action_monitor(cx);
        }

        let prepared_launch = match self.prepared_launch.take() {
            Some(prepared_launch) => prepared_launch,
            None => match self.session_factory.prepare_child_launch() {
                Ok(prepared_launch) => prepared_launch,
                Err(error) => {
                    let _ = error;
                    self.present_failure(
                        TerminalFailure::platform("prepare-session-channel"),
                        false,
                        Some(RecoveryAction::StartSession),
                    );
                    cx.notify();
                    return;
                }
            },
        };
        match self.session_factory.start(geometry, prepared_launch) {
            Ok(started) => {
                self.remote_restart_start_pending = false;
                if self
                    .recovery_retry_requested
                    .filter(|recovery| recovery.action == RecoveryAction::StartSession)
                    .is_some_and(|recovery| self.clear_recovery(recovery))
                {
                    cx.notify();
                }
                started.handle.focus(self.terminal_input_focus);
                if let Some(input) = &self.find_input {
                    started
                        .handle
                        .set_find_query(self.find_generation, input.read(cx).value().to_owned());
                }
                self.native_service_session_identity =
                    self.native_service_session_identity.wrapping_add(1);
                self.session = Some(started.handle);
                self.flush_pending_file_insertion(cx);
                let receiver = started.events;
                let observation = self.runtime_observation.clone();
                let accessibility_receiver = started.accessibility;
                let session_epoch = self.session_epoch;
                self._event_task = Some(cx.spawn(async move |this, cx| {
                    while let Ok(event) = receiver.recv().await {
                        let mut events = vec![event];
                        if let Some(observation) = &observation {
                            if let Ok(event) = receiver.try_recv() {
                                events.push(event);
                            }
                            observation.ui_dispatch(events.len(), receiver.len());
                        } else {
                            while let Ok(event) = receiver.try_recv() {
                                events.push(event);
                            }
                        }
                        if this
                            .update(cx, |this, cx| {
                                for event in events {
                                    this.handle_session_event(session_epoch, event, cx);
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                        if observation.is_some() {
                            cx.background_executor()
                                .timer(Duration::from_millis(1))
                                .await;
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
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(error) => {
                let _ = error;
                let operation = if self.remote_restart_start_pending {
                    "restart-remote-session"
                } else {
                    "start-session-worker"
                };
                self.remote_restart_start_pending = false;
                self.present_failure(
                    TerminalFailure::platform(operation),
                    self.session_factory.is_remote(),
                    Some(RecoveryAction::StartSession),
                );
                cx.notify();
            }
        }
    }

    fn start_runtime_visibility_monitor(&mut self, cx: &mut Context<Self>) {
        let Some(source) = NativeWindowVisibilitySource::capture() else {
            if let Some(observation) = &self.runtime_observation {
                observation.fail();
            }
            return;
        };
        self.runtime_visibility_source = Some(source);
        self._runtime_visibility_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(RUNTIME_VISIBILITY_POLL_INTERVAL)
                    .await;
                if this
                    .update(cx, |pane, cx| {
                        let Some(native) = pane
                            .runtime_visibility_source
                            .as_ref()
                            .map(NativeWindowVisibilitySource::current)
                        else {
                            return;
                        };
                        pane.update_runtime_visibility(native, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn start_failure_action_monitor(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = self.failure_actions.clone() else {
            return;
        };
        self._failure_action_task = Some(cx.spawn(async move |this, cx| {
            while let Some(request) = controller.receive().await {
                if this
                    .update(cx, |pane, cx| {
                        pane.arm_failure_action(request, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn arm_failure_action(&mut self, request: FailureActionRequest, cx: &mut Context<Self>) {
        if self.failure_action_request.is_some() {
            if let Some(observation) = &self.runtime_observation {
                observation.fail();
            }
            return;
        }
        let case = request.case;
        self.failure_action_request = Some(request);
        self.failure_action_trigger_pending = true;
        self.failure_action_recovery_frame = None;
        self.failure_action_resource_rollback = GraphicsRollbackProof::default();
        self.emit_failure_action(FailureActionPhase::Armed, FailureActionResult::Accepted);
        match case {
            FailureActionCase::PresentationInvalidScale => {
                self.failure_action_trigger_pending = true;
            }
            FailureActionCase::PresentationGlyph => {
                self.paint_fault = Some(PaintPreflightFault::Glyph(0));
                self.failure_action_trigger_pending = true;
            }
            FailureActionCase::RendererImagePreflight => {
                self.paint_fault = Some(PaintPreflightFault::Image(0));
                self.failure_action_trigger_pending = true;
            }
            FailureActionCase::RendererResourceBeforeSync => {
                self.graphics_cache
                    .update(cx, |cache, _| cache.fail_next_sync());
                self.failure_action_trigger_pending = true;
            }
            FailureActionCase::RendererResourceAfterStaging => {
                self.graphics_cache
                    .update(cx, |cache, _| cache.fail_after_staging());
                self.failure_action_trigger_pending = true;
            }
            FailureActionCase::PasteboardWrite => {
                self.selection_pasteboard.fail_next_write();
            }
            FailureActionCase::PtyFatal => {
                if let Some(session) = &self.session {
                    session
                        .inject_acceptance_failure(crate::terminal::AcceptanceSessionFailure::Pty);
                } else if let Some(observation) = &self.runtime_observation {
                    observation.fail();
                }
            }
            FailureActionCase::EmulatorFatal => {
                if let Some(session) = &self.session {
                    session.inject_acceptance_failure(
                        crate::terminal::AcceptanceSessionFailure::Emulator,
                    );
                } else if let Some(observation) = &self.runtime_observation {
                    observation.fail();
                }
            }
            FailureActionCase::NormalExitControl => {}
        }
        cx.notify();
    }

    fn trigger_immediate_failure_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.failure_action_request.as_ref() else {
            return;
        };
        if request.case != FailureActionCase::PresentationInvalidScale
            || !self.failure_action_trigger_pending
        {
            return;
        }
        self.apply_backing_scale(f32::NAN, false, window, cx);
        self.emit_injected_failure_if_matching();
    }

    fn emit_injected_failure_if_matching(&mut self) {
        let Some(request) = self.failure_action_request.as_ref() else {
            return;
        };
        let Some(failure) = self.pane_state.failure() else {
            return;
        };
        let matches = match request.case {
            FailureActionCase::PresentationInvalidScale => {
                failure.class() == crate::terminal::FailureClass::Presentation
                    && failure.operation() == "update-backing-scale"
            }
            FailureActionCase::PresentationGlyph => {
                failure.class() == crate::terminal::FailureClass::Presentation
                    && failure.operation() == "paint-terminal-presentation"
            }
            FailureActionCase::RendererImagePreflight => {
                failure.class() == crate::terminal::FailureClass::Resource
                    && failure.operation() == "paint-terminal-graphics"
            }
            FailureActionCase::RendererResourceBeforeSync
            | FailureActionCase::RendererResourceAfterStaging => {
                failure.class() == crate::terminal::FailureClass::Resource
                    && failure.operation() == "prepare-terminal-graphics"
            }
            FailureActionCase::PasteboardWrite => {
                failure.class() == crate::terminal::FailureClass::Platform
                    && failure.operation() == "write-selection-pasteboard"
            }
            FailureActionCase::PtyFatal => {
                failure.class() == crate::terminal::FailureClass::Pty
                    && failure.operation() == "read-shell-output"
            }
            FailureActionCase::EmulatorFatal => {
                failure.class() == crate::terminal::FailureClass::Emulator
                    && failure.operation() == "session-runtime"
            }
            FailureActionCase::NormalExitControl => false,
        };
        if matches && self.failure_action_trigger_pending {
            self.failure_action_trigger_pending = false;
            self.emit_failure_action(
                FailureActionPhase::Injected,
                FailureActionResult::FailedState,
            );
        }
    }

    fn complete_failure_action(&mut self, result: FailureActionResult) {
        self.emit_failure_action(FailureActionPhase::Completed, result);
        self.failure_action_request = None;
        self.failure_action_trigger_pending = false;
        self.failure_action_recovery_frame = None;
    }

    fn emit_failure_action(&self, phase: FailureActionPhase, result: FailureActionResult) {
        let (Some(controller), Some(request)) =
            (&self.failure_actions, &self.failure_action_request)
        else {
            return;
        };
        let failure = self.pane_state.failure();
        let session_attached = self.session.is_some();
        let pane_state = match &self.pane_state {
            PaneTerminalState::Running => FailurePaneState::Running,
            PaneTerminalState::Failed { .. } => FailurePaneState::Failed,
            PaneTerminalState::Exited(_) => FailurePaneState::Exited,
        };
        let pending_recovery = match self.pending_recovery.map(|pending| pending.action) {
            Some(RecoveryAction::Presentation) => FailurePendingRecovery::Presentation,
            Some(RecoveryAction::RendererResources) => FailurePendingRecovery::RendererResources,
            Some(RecoveryAction::CopySelection) => FailurePendingRecovery::CopySelection,
            Some(RecoveryAction::StartSession | RecoveryAction::ExportDiagnostics) | None => {
                FailurePendingRecovery::None
            }
        };
        let emitted = controller.emit(FailureActionEvent {
            request: request.clone(),
            phase,
            result,
            pane_identity: self.native_service_session_identity,
            pane_state,
            failure_class: failure.map(TerminalFailure::class),
            recoverability: failure.map(TerminalFailure::recoverability),
            failure_operation: failure.map(TerminalFailure::operation),
            state_revision: self.state_revision,
            latest_generation: self.screen.generation.as_u64(),
            last_valid_generation: self.last_valid_screen.generation.as_u64(),
            visible_generation: self
                .render_lifecycle
                .presented_generation()
                .map(crate::terminal::PresentationGeneration::as_u64),
            pending_recovery,
            terminal_input_usable: self.terminal_input_focus
                && session_attached
                && failure.is_none_or(|failure| !failure.is_fatal()),
            session_attached,
            resource_staged_count: self.failure_action_resource_rollback.staged_count,
            resource_staged_bytes: self.failure_action_resource_rollback.staged_bytes,
            resource_rolled_back_count: self.failure_action_resource_rollback.rolled_back_count,
            resource_rolled_back_bytes: self.failure_action_resource_rollback.rolled_back_bytes,
        });
        if !emitted && let Some(observation) = &self.runtime_observation {
            observation.fail();
        }
    }

    fn update_runtime_visibility(
        &mut self,
        native: NativeWindowVisibility,
        cx: &mut Context<Self>,
    ) {
        let surface = SurfaceVisibility {
            application_active: self.application_active,
            key_window: self.product_focus.active_window,
            minimized: native.minimized,
            occluded: native.occluded,
            live_resize: native.live_resize,
            workspace_visible: self.product_focus.active_workspace,
            pane_visible: self.product_focus.active_window && self.product_focus.pane_visible,
        };
        let effects = self.render_lifecycle.update_visibility(surface);
        if let Some(observation) = &self.runtime_observation {
            observation.visibility(crate::terminal::RuntimeVisibility {
                presentable: !surface.minimized
                    && !surface.occluded
                    && surface.workspace_visible
                    && surface.pane_visible,
                minimized: surface.minimized,
                occluded: surface.occluded,
                workspace_visible: surface.workspace_visible,
                pane_visible: surface.pane_visible,
                live_resize: surface.live_resize,
            });
        }
        if effects.request_redraw {
            cx.notify();
        }
    }

    fn sync_native_accessibility(&mut self, window: &Window, focused: bool) {
        let notifications = self.pending_accessibility_notifications.take();
        #[cfg(all(target_os = "macos", not(test)))]
        let selection_sender = self
            .session
            .as_ref()
            .and_then(|session| session.accessibility_selection_sender());
        self.pending_accessibility_notifications =
            self.accessibility_element.update(MacosAccessibilityUpdate {
                window,
                model: self.accessibility.as_ref(),
                bounds: self.grid_bounds,
                cell_width: self.cell_width,
                line_height: px(self.line_height),
                font_family: self.font_family.as_ref(),
                font_size: px(self.font_size),
                focused,
                notifications,
                #[cfg(all(target_os = "macos", not(test)))]
                selection_sender,
            });
    }

    fn handle_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Screen(screen) => {
                if let Some(observation) = &self.runtime_observation {
                    observation.ui_screen_received();
                }
                if self
                    .accepted_screen_generation
                    .is_some_and(|generation| screen.generation < generation)
                {
                    return;
                }
                let title = normalized_pane_title(&screen.title, &self.fallback_title);
                if self.title.as_ref() != title {
                    self.title = title.into();
                    cx.emit(TerminalPaneEvent::TitleChanged(self.title.clone()));
                }
                if screen.metadata.context.is_local()
                    && screen.metadata.freshness
                        == crate::terminal::metadata::MetadataFreshness::Live
                    && (self.screen.metadata.directory.path != screen.metadata.directory.path
                        || self.screen.metadata.freshness != screen.metadata.freshness)
                {
                    let path = PathBuf::from(screen.metadata.directory.path.as_ref());
                    if path.is_absolute() {
                        cx.emit(TerminalPaneEvent::ReportedWorkingDirectoryChanged(path));
                    }
                }
                let _ = self.render_lifecycle.observe_snapshot(screen.generation);
                self.accepted_screen_generation = Some(screen.generation);
                if let Some(observation) = &self.runtime_observation {
                    observation.ui_screen_applied(
                        screen.generation.as_u64(),
                        screen.scrollbar.total_rows,
                        screen.scrollbar.visible_rows,
                        screen.scrollbar.offset_rows,
                        screen.selection_present,
                    );
                }
                self.screen = screen;
                self.screen_session_epoch = self.session_epoch;
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
                    apply_native_attention(pane, effects, cx);
                }
                cx.emit(TerminalPaneEvent::AttentionChanged { unread_count });
            }
            SessionEvent::HiddenInputChanged(hidden_input) => {
                self.hidden_input = hidden_input;
                self.sync_secure_input();
            }
            SessionEvent::Osc52Authorization(request) => {
                if let Some(previous) = self.pending_osc52.replace(request)
                    && let Some(session) = &self.session
                {
                    session
                        .resolve_osc52_authorization(previous.id, Osc52AuthorizationDecision::Deny);
                }
            }
            SessionEvent::Osc52AuthorizationExpired(id) => {
                if self.pending_osc52.is_some_and(|request| request.id == id) {
                    self.pending_osc52 = None;
                }
            }
            SessionEvent::Exited(status) => {
                if self.suspend_if_remote_channel_unavailable(cx) {
                    return;
                }
                self.context_menu = None;
                self.quick_look.dismiss();
                if matches!(self.pane_state, PaneTerminalState::Exited(_))
                    || self
                        .pane_state
                        .failure()
                        .is_some_and(TerminalFailure::is_fatal)
                {
                    return;
                }
                self.hidden_input = false;
                self.sync_secure_input();
                self.state_revision = self.state_revision.wrapping_add(1);
                self.pending_recovery = None;
                self.recovery_retry_requested = None;
                self.status = None;
                let normal_exit_control =
                    self.failure_action_request.as_ref().is_some_and(|request| {
                        request.case == FailureActionCase::NormalExitControl
                            && matches!(status, crate::terminal::SessionExit::Success)
                    });
                self.pane_state = PaneTerminalState::exited(status);
                if normal_exit_control {
                    self.complete_failure_action(FailureActionResult::Exited);
                }
                cx.emit(TerminalPaneEvent::Exited);
            }
            SessionEvent::Failed(failure) => {
                if self.suspend_if_remote_channel_unavailable(cx) {
                    return;
                }
                self.context_menu = None;
                self.quick_look.dismiss();
                self.hidden_input = false;
                self.sync_secure_input();
                let failure = TerminalFailure::from_session(&failure);
                if self.present_failure(failure, true, None) {
                    self.emit_injected_failure_if_matching();
                }
            }
        }
    }

    fn handle_session_event(
        &mut self,
        session_epoch: u64,
        event: SessionEvent,
        cx: &mut Context<Self>,
    ) {
        if self.session_epoch == session_epoch {
            self.handle_event(event, cx);
        }
    }

    fn handle_session_accessibility(
        &mut self,
        session_epoch: u64,
        accessibility: Arc<TerminalAccessibilityModel>,
    ) {
        if self.session_epoch == session_epoch {
            self.handle_accessibility(accessibility);
        }
    }

    fn handle_accessibility(&mut self, accessibility: Arc<TerminalAccessibilityModel>) {
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
        let action = if event.is_held {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        };
        let input = NativeKeyEvent::current_key(action)
            .map(|event| self.keyboard_bridge.translate(event))
            .unwrap_or_else(|| encode_key(event));
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
        let input = NativeKeyEvent::current_key(KeyAction::Release)
            .map(|event| self.keyboard_bridge.translate(event))
            .unwrap_or_else(|| encode_keystroke(&event.keystroke, KeyAction::Release));
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
        let Some(event) = NativeKeyEvent::current_modifier() else {
            return;
        };
        let translation = self.keyboard_bridge.modifier_transition(event);
        self.send_key_translation(translation, cx);
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
                    NativeKeyEventKind::KeyDown => DiagnosticKeyEventKind::KeyDown,
                    NativeKeyEventKind::KeyUp => DiagnosticKeyEventKind::KeyUp,
                    NativeKeyEventKind::FlagsChanged => DiagnosticKeyEventKind::FlagsChanged,
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
        if let Some(session) = &self.session {
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
        if let Some(session) = &self.session {
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
            .link_at(position)
            .map(|link| (self.screen.generation, link));
        if self.hovered_link != hovered_link {
            self.hovered_link = hovered_link;
            cx.notify();
        }
        if self.remote_input_blocked {
            cx.stop_propagation();
            return;
        }
        if self.pressed_link.is_some() {
            cx.stop_propagation();
            return;
        }
        if let Some(session) = &self.session {
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
                self.local_file_capabilities,
                generation,
                &pressed,
                self.screen.generation,
                current.as_ref(),
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
        let Some(position) = self.surface_position(event.position, true) else {
            return;
        };

        if let Some(session) = &self.session {
            session.pointer(PointerInput {
                generation: self.screen.generation,
                phase: PointerPhase::Release,
                button: Some(button),
                position,
                modifiers: self.pointer_modifiers,
                shift_selection: self.shift_selection,
            });
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
        if self.remote_input_blocked {
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
        let phase = current_wheel_phase().unwrap_or(match event.touch_phase {
            gpui::TouchPhase::Started => WheelPhase::GestureStarted,
            gpui::TouchPhase::Moved => WheelPhase::GestureChanged,
            gpui::TouchPhase::Ended => WheelPhase::GestureEnded,
        });
        let (horizontal_steps, vertical_steps) =
            self.wheel_accumulator.push(delta.x, delta.y, phase);

        if (horizontal_steps != 0 || vertical_steps != 0)
            && let Some(session) = &self.session
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
        if let Some(copy) = self.ordered_selection_copy(cx) {
            if let Err(error) = self.selection_pasteboard.write(copy, cx) {
                let _ = error;
                self.present_failure(
                    TerminalFailure::platform("write-selection-pasteboard"),
                    true,
                    Some(RecoveryAction::CopySelection),
                );
                self.emit_injected_failure_if_matching();
                cx.notify();
            } else if recovery.is_some_and(|recovery| self.clear_recovery(recovery)) {
                if self
                    .failure_action_request
                    .as_ref()
                    .is_some_and(|request| request.case == FailureActionCase::PasteboardWrite)
                {
                    self.complete_failure_action(FailureActionResult::Recovered);
                }
                cx.notify();
            }
        }
    }

    fn ordered_selection_copy(&mut self, cx: &mut Context<Self>) -> Option<SelectionCopy> {
        let session = self.session.as_ref()?;
        match session.copy_selection() {
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
        let paths = self
            .local_file_capabilities
            .are_enabled()
            .then(|| read_file_urls().unwrap_or_default())
            .unwrap_or_default();
        let insertion = if paths.is_empty() {
            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                return;
            };
            NativeInsertion::service_text(text, terminal_input_focused)
        } else {
            NativeInsertion::dropped_files(
                &paths,
                terminal_input_focused,
                self.local_file_capabilities,
            )
        };
        let Ok(insertion) = insertion else {
            return;
        };
        if !insertion.text().is_empty() {
            self.request_paste_text(insertion.into_text(), cx);
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
        let Ok(insertion) = NativeInsertion::service_text(text, true) else {
            return false;
        };
        if insertion.text().is_empty() {
            return false;
        }
        self.request_paste_text(insertion.into_text(), cx);
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
        if !self.local_file_capabilities.are_enabled() {
            return;
        }
        let insertion =
            match NativeInsertion::prepare_dropped_files(paths, self.local_file_capabilities) {
                Ok(insertion) => insertion,
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
        if !self.terminal_input_focus || self.session.is_none() {
            return;
        }
        let Some(insertion) = self.pending_file_insertion.take() else {
            return;
        };
        self.request_paste_text(insertion.into_text(), cx);
    }

    fn native_context_actions(&self) -> NativeContextActions {
        NativeContextActions::from_presence(
            self.local_file_capabilities,
            self.screen.selection_present,
            self.current_hovered_link(),
        )
    }

    fn context_menu_actions(&self, menu: &TerminalContextMenuState) -> NativeContextActions {
        let current = self.link_at(menu.position);
        let link = revalidated_context_link(
            menu.generation,
            menu.link.as_ref(),
            self.screen.generation,
            current.as_ref(),
        );
        let selection_is_current = menu.generation == self.screen.generation;
        let mut actions = NativeContextActions::from_presence(
            self.local_file_capabilities,
            selection_is_current && menu.selection_present && self.screen.selection_present,
            link,
        );
        actions.quick_look = menu.quick_look_eligible && link.is_some();
        actions
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
        let quick_look_eligible =
            NativeContextActions::from_presence(self.local_file_capabilities, false, link.as_ref())
                .quick_look;
        self.context_menu = Some(TerminalContextMenuState {
            generation: self.screen.generation,
            position,
            selection_present: self.screen.selection_present,
            quick_look_eligible,
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
            && self.product_focus.active_window
            && self.product_focus.focused_pane
            && self.product_focus.blocker.is_none()
            && !self.native_modal_open
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
        let selection_is_current = menu.generation == self.screen.generation;
        let mut actions = NativeContextActions::from_presence(
            self.local_file_capabilities,
            selection_is_current && menu.selection_present && self.screen.selection_present,
            link.as_ref(),
        );
        actions.quick_look = menu.quick_look_eligible && link.is_some();
        self.sync_terminal_input_focus(window, cx);

        match command {
            TerminalContextMenuCommand::Copy if actions.copy => {
                self.copy_selection(&CopySelection, window, cx);
            }
            TerminalContextMenuCommand::OpenLink if actions.open_link => {
                if let Some(url) =
                    link.and_then(|link| link.activation_url(self.local_file_capabilities))
                {
                    cx.open_url(&url);
                }
            }
            TerminalContextMenuCommand::QuickLook if actions.quick_look => {
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
        let Some(target) = QuickLookTarget::from_link(link, self.local_file_capabilities) else {
            return;
        };
        if self.quick_look.preview(&target).is_err() {
            self.present_failure(TerminalFailure::platform("preview-local-file"), true, None);
            cx.notify();
        }
    }

    fn current_hovered_link(&self) -> Option<&crate::terminal::HyperlinkTarget> {
        hovered_link_for_generation(self.hovered_link.as_ref(), self.screen.generation)
    }

    pub(crate) fn native_service_status(
        &mut self,
        workspace_id: WorkspaceId,
        window_id: WindowId,
        pane_id: PaneId,
        hierarchy_generation: u64,
        window: &Window,
        cx: &App,
    ) -> NativeServiceStatus {
        self.sync_terminal_input_focus(window, cx);
        let session_available = self.session.is_some();
        let capabilities = NativeServiceCapabilities::new(
            session_available && self.native_context_actions().copy,
            session_available && self.terminal_input_focus,
        );
        let origin = session_available.then(|| {
            NativeServiceOrigin::new(
                workspace_id,
                window_id,
                pane_id,
                self.native_service_session_identity,
                self.native_service_focus_epoch.get(),
                hierarchy_generation,
            )
        });
        NativeServiceStatus::new(capabilities, origin)
    }

    fn request_paste_text(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let guard = self.paste_request_guard();
        let receiver = session.request_paste(text);
        cx.spawn(async move |this, cx| {
            let outcome = receiver.recv().await;
            let _ = this.update(cx, |this, cx| {
                if !this.paste_request_guard_is_current(guard) {
                    if let Ok(Ok(PasteRequestOutcome::ConfirmationRequired(confirmation))) = outcome
                        && this.native_service_session_identity == guard.session_identity
                        && let Some(session) = &this.session
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
            session_identity: self.native_service_session_identity,
            focus_epoch: self.native_service_focus_epoch.get(),
            hierarchy_generation: self.native_service_hierarchy_generation,
        }
    }

    fn paste_request_guard_is_current(&self, guard: PasteRequestGuard) -> bool {
        self.session.is_some()
            && self.terminal_input_focus
            && self.native_service_session_identity == guard.session_identity
            && self.native_service_focus_epoch.get() == guard.focus_epoch
            && self.native_service_hierarchy_generation == guard.hierarchy_generation
    }

    fn native_service_origin_matches(&self, origin: NativeServiceOrigin) -> bool {
        self.session.is_some()
            && self.native_service_session_identity == origin.session_identity()
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
        self.native_modal_open = true;
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
                this.native_modal_open = false;
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
        let Some(session) = &self.session else {
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

    fn allow_osc52_clipboard(
        &mut self,
        _: &AllowOsc52Clipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resolve_osc52_authorization(Osc52AuthorizationDecision::Allow, window, cx);
    }

    fn deny_osc52_clipboard(
        &mut self,
        _: &DenyOsc52Clipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resolve_osc52_authorization(Osc52AuthorizationDecision::Deny, window, cx);
    }

    fn resolve_osc52_authorization(
        &mut self,
        mut decision: Osc52AuthorizationDecision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = self.pending_osc52.take() else {
            return;
        };
        if !self.synchronize_terminal_input_focus(window, cx) {
            decision = Osc52AuthorizationDecision::Deny;
        }
        if let Some(session) = &self.session {
            session.resolve_osc52_authorization(request.id, decision);
        }
        self.focus(window);
        cx.notify();
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
        let cell = self
            .last_geometry?
            .cell_at_backing_position(BackingPosition::new(position.x, position.y));
        self.screen
            .rows
            .get(usize::from(cell.row))?
            .get(usize::from(cell.col))?
            .hyperlink
            .clone()
            .filter(|link| !link.is_local_file() || self.local_file_capabilities.are_enabled())
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let input = self.find_input.as_ref()?.clone();
        let snapshot = self
            .screen
            .find
            .as_ref()
            .filter(|snapshot| snapshot.generation == self.find_generation);
        let result_label = snapshot.map_or_else(
            || "–/–".to_owned(),
            |snapshot| {
                format!(
                    "{}/{}",
                    snapshot
                        .current_match
                        .map_or_else(|| "–".to_owned(), |index| index.to_string()),
                    snapshot.total_matches
                )
            },
        );
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
                .bg(gpui_color(ACTIVE_THEME.background))
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
                        .text_size(px(11.0))
                        .text_color(gpui_color(ACTIVE_THEME.text_muted))
                        .child(result_label),
                )
                .child(find_icon_button(
                    "terminal-find-previous",
                    "Find Previous",
                    "chevron.up",
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
                    "chevron.down",
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
                    "xmark",
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
    symbol: &'static str,
    enabled: bool,
    on_activate: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    IconButton::new(id, accessibility_name, move |foreground| {
        Icon::new(symbol)
            .weight(SymbolWeight::Medium)
            .size(px(11.0))
            .color(foreground)
            .into_any_element()
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
            self.send_key_translation(
                KeyTranslation::Encoded(KeyInput::input_method_commit(text)),
                cx,
            );
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
        self.trigger_immediate_failure_action(window, cx);
        let native_activity = NativeActivity::current(window, cx);
        schedule_attention_retries(
            update_attention_application_activation(native_activity.application_active),
            cx,
        );
        update_secure_input_application_activation(native_activity.application_active);
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
            .runtime_visibility_source
            .as_ref()
            .map(NativeWindowVisibilitySource::current)
            .unwrap_or_else(current_window_visibility);
        let surface_visibility = SurfaceVisibility {
            application_active: native_activity.application_active,
            key_window: native_activity.operating_system_window_key,
            minimized: native_visibility.minimized,
            occluded: native_visibility.occluded,
            live_resize: native_visibility.live_resize,
            workspace_visible: self.product_focus.active_workspace,
            pane_visible: self.product_focus.active_window && self.product_focus.pane_visible,
        };
        let lifecycle_effects = self.render_lifecycle.update_visibility(surface_visibility);
        if let Some(observation) = &self.runtime_observation {
            observation.visibility(crate::terminal::RuntimeVisibility {
                presentable: !surface_visibility.minimized
                    && !surface_visibility.occluded
                    && surface_visibility.workspace_visible
                    && surface_visibility.pane_visible,
                minimized: surface_visibility.minimized,
                occluded: surface_visibility.occluded,
                workspace_visible: surface_visibility.workspace_visible,
                pane_visible: surface_visibility.pane_visible,
                live_resize: surface_visibility.live_resize,
            });
            observation.render_started(self.screen.generation.as_u64());
        }
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
        let acceptance_retry = self
            .failure_action_request
            .as_ref()
            .filter(|_| self.failure_action_trigger_pending)
            .is_some_and(|request| {
                matches!(
                    request.case,
                    FailureActionCase::PresentationGlyph
                        | FailureActionCase::RendererImagePreflight
                        | FailureActionCase::RendererResourceBeforeSync
                        | FailureActionCase::RendererResourceAfterStaging
                )
            });
        let presentation_generation = self
            .render_lifecycle
            .take_frame()
            .or_else(|| {
                recovery.and_then(|_| self.render_lifecycle.retry_frame(display_screen.generation))
            })
            .or_else(|| {
                acceptance_retry
                    .then(|| self.render_lifecycle.retry_frame(display_screen.generation))
                    .flatten()
            });
        let graphics_attempt_allowed = !recovery_holds_presentation
            && presentation_generation == Some(display_screen.generation);
        let (graphics, graphics_attempt, graphics_attempted, injected_rollback) =
            self.graphics_cache.update(cx, |cache, cx| {
                if !graphics_attempt_allowed {
                    return (cache.last_presented(), None, false, None);
                }
                match cache.sync(
                    display_screen.active_screen,
                    &display_screen.graphics,
                    window,
                    cx,
                ) {
                    Ok(preparation) => (preparation.graphics, Some(preparation.token), true, None),
                    Err(_) => (
                        cache.last_presented(),
                        None,
                        true,
                        cache.take_injected_rollback(),
                    ),
                }
            });
        if let Some(proof) = injected_rollback {
            self.failure_action_resource_rollback = proof;
        }
        if graphics_attempted && graphics_attempt.is_none() {
            if self.present_failure(
                TerminalFailure::resource("prepare-terminal-graphics"),
                true,
                Some(RecoveryAction::RendererResources),
            ) {
                self.emit_injected_failure_if_matching();
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
        let hovered_link = displaying_current
            .then(|| self.current_hovered_link().cloned())
            .flatten();
        let native_context_actions = self.native_context_actions();
        let link_cell_width = self.cell_width;
        let link_line_height = self.line_height;
        let link_rows = display_screen.rows.clone();
        let link_highlights = hovered_link.as_ref().map_or_else(Vec::new, |hovered| {
            link_rows
                .iter()
                .enumerate()
                .flat_map(|(row, cells)| {
                    cells
                        .iter()
                        .enumerate()
                        .filter(move |(_, cell)| {
                            cell.hyperlink
                                .as_ref()
                                .is_some_and(|link| link.identity == hovered.identity)
                        })
                        .map(move |(column, _)| {
                            div()
                                .absolute()
                                .left(px(
                                    HORIZONTAL_PADDING + column as f32 * f32::from(link_cell_width)
                                ))
                                .top(px(VERTICAL_PADDING + row as f32 * link_line_height))
                                .w(link_cell_width)
                                .h(px(link_line_height))
                                .border_b_1()
                                .border_color(gpui_color(ACTIVE_THEME.link_text_hover))
                        })
                })
                .collect::<Vec<_>>()
        });
        let paste_confirmation = self.pending_paste;
        let osc52_authorization = self.pending_osc52;
        let key_context = if paste_confirmation.is_some() {
            TERMINAL_PASTE_CONFIRMATION_KEY_CONTEXT
        } else if osc52_authorization.is_some() {
            TERMINAL_OSC52_AUTHORIZATION_KEY_CONTEXT
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
        let diagnostics_available =
            self.pane_state.failure().is_some() && self.diagnostics.record_count() > 0;
        let recovery_available = self.pending_recovery.is_some();
        let last_valid_frame_preserved = self.pane_state.last_valid_frame().is_some();
        let export_pane = cx.entity().downgrade();
        let retry_pane = cx.entity().downgrade();
        let native_context_selector = format!(
            "terminal-native-context-copy-{}-open-{}-quick-look-{}-failure-{}-last-frame-{}",
            native_context_actions.copy,
            native_context_actions.open_link,
            native_context_actions.quick_look,
            diagnostics_available,
            last_valid_frame_preserved,
        );
        let terminal_grid = TerminalGridElement::new(
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
                fallback: (presentation_operation.is_some()
                    && !Arc::ptr_eq(&display_screen, &self.last_valid_screen))
                .then(|| {
                    (
                        Arc::clone(&self.last_valid_screen),
                        self.fallback_render_cache.clone(),
                        fallback_graphics,
                    )
                }),
                paint_fault: self.paint_fault.take(),
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
        let context_menu_entries = terminal_context_menu_entries(context_menu_actions);
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
        .size(MenuSize::Regular)
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
            .py(px(VERTICAL_PADDING))
            .when(pointer_uses_text_cursor, |root| root.cursor_text())
            .when(!pointer_uses_text_cursor, |root| root.cursor_default())
            .when(hovered_link.is_some(), |root| root.cursor_pointer())
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
            .on_action(cx.listener(Self::allow_osc52_clipboard))
            .on_action(cx.listener(Self::deny_osc52_clipboard))
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
            .children(link_highlights)
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
            .when_some(hovered_link, |root, link| {
                root.child(
                    div()
                        .absolute()
                        .left(px(8.0))
                        .bottom(px(8.0))
                        .px(px(6.0))
                        .bg(gpui_color(ACTIVE_THEME.background))
                        .child(link.value),
                )
            })
            .when_some(paste_confirmation, |root, confirmation| {
                root.child(render_paste_confirmation(
                    confirmation,
                    cx.entity().downgrade(),
                ))
            })
            .when_some(osc52_authorization, |root, request| {
                root.child(render_osc52_authorization(request, cx.entity().downgrade()))
            })
            .when_some(status, |root, status| {
                root.child(
                    div()
                        .debug_selector(|| "terminal-status".to_owned())
                        .absolute()
                        .right(px(HORIZONTAL_PADDING))
                        .bottom(px(VERTICAL_PADDING))
                        .max_w(px(520.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(gpui_color(ACTIVE_THEME.element_active))
                        .text_color(gpui_color(ACTIVE_THEME.text))
                        .text_sm()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(status)
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
                                Button::new("export-terminal-diagnostics", "Export Diagnostics")
                                    .variant(ButtonVariant::Link)
                                    .size(ButtonSize::Compact)
                                    .debug_selector("export-terminal-diagnostics")
                                    .on_activate(move |_, window, cx| {
                                        let _ = export_pane.update(cx, |pane, cx| {
                                            pane.export_diagnostics(
                                                &ExportTerminalDiagnostics,
                                                window,
                                                cx,
                                            );
                                        });
                                    }),
                            )
                        }),
                )
            })
            .child(
                div()
                    .absolute()
                    .left(px(HORIZONTAL_PADDING))
                    .right(px(HORIZONTAL_PADDING))
                    .top(px(VERTICAL_PADDING))
                    .bottom(px(VERTICAL_PADDING))
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
    let risks = [
        confirmation.risk.multiline.then_some("multiple lines"),
        confirmation
            .risk
            .control_bytes
            .then_some("terminal control bytes"),
        confirmation
            .risk
            .closing_fence
            .then_some("a bracketed-paste closing fence"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");

    div()
        .debug_selector(|| "unsafe-paste-confirmation".to_owned())
        .absolute()
        .left(px(16.0))
        .right(px(16.0))
        .bottom(px(16.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(gpui_color(ACTIVE_THEME.warning_border))
        .bg(gpui_color(ACTIVE_THEME.warning_background))
        .text_color(gpui_color(ACTIVE_THEME.text))
        .text_sm()
        .occlude()
        .child(format!(
            "Paste {} bytes across {} lines? Detected {risks}.",
            confirmation.byte_len, confirmation.line_count
        ))
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
        )
}

fn render_osc52_authorization(
    request: Osc52AuthorizationRequest,
    pane: gpui::WeakEntity<TerminalPane>,
) -> impl IntoElement {
    let deny_pane = pane.clone();
    let access = match request.access {
        Osc52Access::Read => "read",
        Osc52Access::Write => "write",
    };
    let target = match request.target {
        Osc52Target::Standard => "system clipboard",
        Osc52Target::Selection => "selection clipboard",
        Osc52Target::Primary => "primary clipboard",
    };
    let detail = match request.access {
        Osc52Access::Read => format!("Allow the terminal program to read the {target}?"),
        Osc52Access::Write => format!(
            "Allow the terminal program to {access} {} bytes to the {target}?",
            request.byte_len
        ),
    };

    div()
        .debug_selector(|| "osc52-authorization".to_owned())
        .absolute()
        .left(px(16.0))
        .right(px(16.0))
        .bottom(px(16.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(gpui_color(ACTIVE_THEME.warning_border))
        .bg(gpui_color(ACTIVE_THEME.warning_background))
        .text_color(gpui_color(ACTIVE_THEME.text))
        .text_sm()
        .occlude()
        .child(detail)
        .child(
            Button::new("deny-osc52-clipboard", "Deny")
                .variant(ButtonVariant::Secondary)
                .size(ButtonSize::Small)
                .role(ButtonRole::Cancel)
                .debug_selector("deny-osc52-clipboard")
                .on_activate(move |_, window, cx| {
                    let _ = deny_pane.update(cx, |pane, cx| {
                        pane.deny_osc52_clipboard(&DenyOsc52Clipboard, window, cx);
                    });
                }),
        )
        .child(
            Button::new("allow-osc52-clipboard", "Allow")
                .variant(ButtonVariant::Primary)
                .size(ButtonSize::Small)
                .debug_selector("allow-osc52-clipboard")
                .on_activate(move |_, window, cx| {
                    let _ = pane.update(cx, |pane, cx| {
                        pane.allow_osc52_clipboard(&AllowOsc52Clipboard, window, cx);
                    });
                }),
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

fn observation_geometry(
    geometry: TerminalGeometry,
) -> crate::platform::acceptance_observation::ObservationGeometry {
    let grid = geometry.grid();
    let logical = geometry.logical_grid_size();
    let backing = geometry.backing_grid_size();
    crate::platform::acceptance_observation::ObservationGeometry {
        rows: grid.rows,
        columns: grid.cols,
        logical_width: logical.width,
        logical_height: logical.height,
        backing_pixel_width: backing.width,
        backing_pixel_height: backing.height,
    }
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

fn write_selection_copy(copy: SelectionCopy, cx: &mut App) -> Result<(), String> {
    #[cfg(test)]
    {
        cx.write_to_clipboard(ClipboardItem::new_string(copy.plain_text));
        let _ = copy.html;
        Ok(())
    }
    #[cfg(not(test))]
    {
        let _ = cx;
        crate::platform::macos_pasteboard::write_selection(&copy.plain_text, copy.html.as_deref())
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

fn activated_link(
    local_file_capabilities: TerminalLocalFileCapabilities,
    pressed_generation: crate::terminal::PresentationGeneration,
    pressed: &crate::terminal::HyperlinkTarget,
    current_generation: crate::terminal::PresentationGeneration,
    current: Option<&crate::terminal::HyperlinkTarget>,
) -> Option<String> {
    (pressed_generation == current_generation
        && current.is_some_and(|link| link.identity == pressed.identity))
    .then(|| pressed.activation_url(local_file_capabilities))
    .flatten()
}

fn revalidated_context_link<'a>(
    clicked_generation: crate::terminal::PresentationGeneration,
    clicked: Option<&'a crate::terminal::HyperlinkTarget>,
    current_generation: crate::terminal::PresentationGeneration,
    current: Option<&crate::terminal::HyperlinkTarget>,
) -> Option<&'a crate::terminal::HyperlinkTarget> {
    clicked.filter(|clicked| {
        clicked_generation == current_generation
            && current.is_some_and(|current| current.identity == clicked.identity)
    })
}

fn hovered_link_for_generation(
    hovered: Option<&(
        crate::terminal::PresentationGeneration,
        crate::terminal::HyperlinkTarget,
    )>,
    current_generation: crate::terminal::PresentationGeneration,
) -> Option<&crate::terminal::HyperlinkTarget> {
    hovered
        .filter(|(generation, _)| *generation == current_generation)
        .map(|(_, link)| link)
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

fn encode_key(event: &KeyDownEvent) -> KeyTranslation {
    encode_keystroke(
        &event.keystroke,
        if event.is_held {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        },
    )
}

fn encode_keystroke(keystroke: &gpui::Keystroke, action: KeyAction) -> KeyTranslation {
    let physical_key = physical_key(&keystroke.key);
    let text = keystroke
        .key_char
        .clone()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
    let unshifted_codepoint = single_char(&keystroke.key).map(unshifted_character);

    let input = KeyInput {
        action,
        physical_key,
        native_key_code: None,
        logical_key: keystroke.key.clone(),
        text,
        unshifted_codepoint,
        modifiers: input_modifiers(keystroke.modifiers),
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::default(),
    };
    let allows_text_input =
        !input.modifiers.control && !input.modifiers.platform && !input.modifiers.alt;
    match input.validate() {
        Ok(()) => KeyTranslation::Encoded(input),
        Err(_) => match input.text {
            Some(text) if action != KeyAction::Release && allows_text_input => {
                KeyTranslation::TextInput(text)
            }
            _ => KeyTranslation::Unhandled(UnhandledKeyEvent {
                kind: match action {
                    KeyAction::Press | KeyAction::Repeat => NativeKeyEventKind::KeyDown,
                    KeyAction::Release => NativeKeyEventKind::KeyUp,
                },
                action,
                native_key_code: None,
            }),
        },
    }
}

fn physical_key(key: &str) -> PhysicalKey {
    match key {
        "enter" => PhysicalKey::Enter,
        "backspace" => PhysicalKey::Backspace,
        "tab" => PhysicalKey::Tab,
        "escape" => PhysicalKey::Escape,
        "up" => PhysicalKey::ArrowUp,
        "down" => PhysicalKey::ArrowDown,
        "right" => PhysicalKey::ArrowRight,
        "left" => PhysicalKey::ArrowLeft,
        "home" => PhysicalKey::Home,
        "end" => PhysicalKey::End,
        "pageup" => PhysicalKey::PageUp,
        "pagedown" => PhysicalKey::PageDown,
        "insert" => PhysicalKey::Insert,
        "delete" => PhysicalKey::Delete,
        "f1" => PhysicalKey::F1,
        "f2" => PhysicalKey::F2,
        "f3" => PhysicalKey::F3,
        "f4" => PhysicalKey::F4,
        "f5" => PhysicalKey::F5,
        "f6" => PhysicalKey::F6,
        "f7" => PhysicalKey::F7,
        "f8" => PhysicalKey::F8,
        "f9" => PhysicalKey::F9,
        "f10" => PhysicalKey::F10,
        "f11" => PhysicalKey::F11,
        "f12" => PhysicalKey::F12,
        "f13" => PhysicalKey::F13,
        "f14" => PhysicalKey::F14,
        "f15" => PhysicalKey::F15,
        "f16" => PhysicalKey::F16,
        "f17" => PhysicalKey::F17,
        "f18" => PhysicalKey::F18,
        "f19" => PhysicalKey::F19,
        "f20" => PhysicalKey::F20,
        "f21" => PhysicalKey::F21,
        "f22" => PhysicalKey::F22,
        "f23" => PhysicalKey::F23,
        "f24" => PhysicalKey::F24,
        "f25" => PhysicalKey::F25,
        "space" | " " => PhysicalKey::Space,
        value => single_char(value)
            .map(physical_character_key)
            .unwrap_or(PhysicalKey::Unidentified),
    }
}

fn physical_character_key(character: char) -> PhysicalKey {
    match unshifted_character(character) {
        '`' => PhysicalKey::Backquote,
        '\\' => PhysicalKey::Backslash,
        '[' => PhysicalKey::BracketLeft,
        ']' => PhysicalKey::BracketRight,
        ',' => PhysicalKey::Comma,
        '0' => PhysicalKey::Digit0,
        '1' => PhysicalKey::Digit1,
        '2' => PhysicalKey::Digit2,
        '3' => PhysicalKey::Digit3,
        '4' => PhysicalKey::Digit4,
        '5' => PhysicalKey::Digit5,
        '6' => PhysicalKey::Digit6,
        '7' => PhysicalKey::Digit7,
        '8' => PhysicalKey::Digit8,
        '9' => PhysicalKey::Digit9,
        '=' => PhysicalKey::Equal,
        'a' => PhysicalKey::A,
        'b' => PhysicalKey::B,
        'c' => PhysicalKey::C,
        'd' => PhysicalKey::D,
        'e' => PhysicalKey::E,
        'f' => PhysicalKey::F,
        'g' => PhysicalKey::G,
        'h' => PhysicalKey::H,
        'i' => PhysicalKey::I,
        'j' => PhysicalKey::J,
        'k' => PhysicalKey::K,
        'l' => PhysicalKey::L,
        'm' => PhysicalKey::M,
        'n' => PhysicalKey::N,
        'o' => PhysicalKey::O,
        'p' => PhysicalKey::P,
        'q' => PhysicalKey::Q,
        'r' => PhysicalKey::R,
        's' => PhysicalKey::S,
        't' => PhysicalKey::T,
        'u' => PhysicalKey::U,
        'v' => PhysicalKey::V,
        'w' => PhysicalKey::W,
        'x' => PhysicalKey::X,
        'y' => PhysicalKey::Y,
        'z' => PhysicalKey::Z,
        '-' => PhysicalKey::Minus,
        '.' => PhysicalKey::Period,
        '\'' => PhysicalKey::Quote,
        ';' => PhysicalKey::Semicolon,
        '/' => PhysicalKey::Slash,
        ' ' => PhysicalKey::Space,
        _ => PhysicalKey::Unidentified,
    }
}

fn unshifted_character(character: char) -> char {
    match character {
        '~' => '`',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        character if character.is_ascii_uppercase() => character.to_ascii_lowercase(),
        character => character,
    }
}

fn single_char(value: &str) -> Option<char> {
    let mut characters = value.chars();
    let first = characters.next()?;
    characters.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{
        EmptyView, Entity, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
    };

    use super::*;
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
        test_workspace_directory,
    };
    use crate::terminal::{
        LocalTerminalLaunchPlan, RemoteTerminalChannelProvider, ScrollbarSnapshot, SessionExit,
        SessionFailure, TerminalLaunchPlan, TerminalSessionFactory,
    };

    #[test]
    fn active_application_with_non_key_window_suppresses_inactive_only_notification() {
        let activity = NativeActivity {
            application_active: true,
            operating_system_window_key: false,
        };
        let mut attention = AttentionState::default();

        let effects = attention.observe(
            crate::terminal::attention::AttentionEvent::Bell,
            AttentionFacts {
                terminal_input_focus: false,
                surface_active: terminal_surface_active(TerminalProductFocus::default(), activity),
                application_active: activity.application_active,
            },
            Instant::now(),
        );

        assert_eq!(
            (effects.request_dock_attention, effects.notification),
            (true, None)
        );
    }

    struct KeyPropagationProbe {
        pane: Entity<TerminalPane>,
        propagated_key_downs: Rc<Cell<usize>>,
    }

    struct RecordingQuickLookPresenter {
        previews: Rc<Cell<usize>>,
        dismissals: Rc<Cell<usize>>,
    }

    impl QuickLookPlatform for RecordingQuickLookPresenter {
        fn preview(
            &mut self,
            _: &QuickLookTarget,
        ) -> Result<(), crate::platform::macos_quick_look::QuickLookError> {
            self.previews.set(self.previews.get() + 1);
            Ok(())
        }

        fn dismiss(&mut self) {
            self.dismissals.set(self.dismissals.get() + 1);
        }
    }

    impl Render for KeyPropagationProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let propagated_key_downs = Rc::clone(&self.propagated_key_downs);
            div()
                .size_full()
                .on_key_down(move |_, _, _| {
                    propagated_key_downs.set(propagated_key_downs.get() + 1);
                })
                .child(self.pane.clone())
        }
    }

    fn blinking_screen() -> Arc<ScreenSnapshot> {
        ScreenSnapshot::from_test_parts(
            Arc::from([Arc::from([crate::terminal::CellSnapshot {
                text: "x".to_owned(),
                foreground_source: crate::terminal::TerminalColor::Default,
                background_source: crate::terminal::TerminalColor::Default,
                inverse: false,
                bold: false,
                faint: false,
                italic: false,
                blinking: true,
                invisible: false,
                underline: crate::terminal::TerminalUnderlineSnapshot::None,
                underline_source: crate::terminal::TerminalColor::Default,
                strikethrough: false,
                overline: false,
                selected: false,
                spacer_tail: false,
                semantic_content: crate::terminal::CellSemanticSnapshot::Output,
                hyperlink: None,
            }])]),
            ScrollbarSnapshot::default(),
            "blink",
        )
    }

    fn blinking_cursor_screen(visible: bool, blinking: bool) -> Arc<ScreenSnapshot> {
        let mut screen = ScreenSnapshot::from_test_parts(
            Arc::from([Arc::from([crate::terminal::CellSnapshot {
                text: "x".to_owned(),
                foreground_source: crate::terminal::TerminalColor::Default,
                background_source: crate::terminal::TerminalColor::Default,
                inverse: false,
                bold: false,
                faint: false,
                italic: false,
                blinking: false,
                invisible: false,
                underline: crate::terminal::TerminalUnderlineSnapshot::None,
                underline_source: crate::terminal::TerminalColor::Default,
                strikethrough: false,
                overline: false,
                selected: false,
                spacer_tail: false,
                semantic_content: crate::terminal::CellSemanticSnapshot::Output,
                hyperlink: None,
            }])]),
            ScrollbarSnapshot::default(),
            "cursor blink",
        );
        Arc::make_mut(&mut screen).cursor = crate::terminal::CursorSnapshot {
            position: Some(crate::terminal::CursorPositionSnapshot {
                column: 0,
                row: 0,
                width_cells: 1,
            }),
            visible,
            blinking,
            ..crate::terminal::CursorSnapshot::default()
        };
        screen
    }

    fn context_action_screen(
        link: Option<crate::terminal::HyperlinkTarget>,
        selection_present: bool,
    ) -> Arc<ScreenSnapshot> {
        let mut screen = ScreenSnapshot::from_test_parts_at(
            Arc::from([Arc::from([crate::terminal::CellSnapshot {
                text: "x".to_owned(),
                foreground_source: crate::terminal::TerminalColor::Default,
                background_source: crate::terminal::TerminalColor::Default,
                inverse: false,
                bold: false,
                faint: false,
                italic: false,
                blinking: false,
                invisible: false,
                underline: crate::terminal::TerminalUnderlineSnapshot::None,
                underline_source: crate::terminal::TerminalColor::Default,
                strikethrough: false,
                overline: false,
                selected: selection_present,
                spacer_tail: false,
                semantic_content: crate::terminal::CellSemanticSnapshot::Output,
                hyperlink: link,
            }])]),
            ScrollbarSnapshot::default(),
            "context action",
            7,
        );
        Arc::make_mut(&mut screen).selection_present = selection_present;
        screen
    }

    fn graphics_screen(generation: u64, image_id: u32) -> Arc<ScreenSnapshot> {
        graphics_screen_with_images(generation, &[image_id])
    }

    fn graphics_screen_with_images(generation: u64, image_ids: &[u32]) -> Arc<ScreenSnapshot> {
        let mut screen = ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            ScrollbarSnapshot::default(),
            "graphics",
            generation,
        );
        Arc::make_mut(&mut screen).graphics = crate::terminal::GraphicsSnapshot {
            generation,
            placement_generation: generation,
            images: image_ids
                .iter()
                .map(|image_id| {
                    Arc::new(crate::terminal::ImageSnapshot {
                        key: crate::terminal::ImageKey {
                            image_id: *image_id,
                            generation,
                        },
                        width: 1,
                        height: 1,
                        rgba: Arc::from([10, 20, 30, 255]),
                    })
                })
                .collect::<Vec<_>>()
                .into(),
            placements: image_ids
                .iter()
                .enumerate()
                .map(
                    |(index, image_id)| crate::terminal::ImagePlacementSnapshot {
                        image: crate::terminal::ImageKey {
                            image_id: *image_id,
                            generation,
                        },
                        placement_id: index as u32,
                        z: 0,
                        viewport_col: index as i32,
                        viewport_row: 0,
                        cell_offset_x: 0,
                        cell_offset_y: 0,
                        source_x: 0,
                        source_y: 0,
                        source_width: 1,
                        source_height: 1,
                        destination_width: 1,
                        destination_height: 1,
                        unicode_placeholder: false,
                    },
                )
                .collect::<Vec<_>>()
                .into(),
        };
        screen
    }

    fn text_screen(generation: u64, rows: &[&str]) -> Arc<ScreenSnapshot> {
        let rows = rows
            .iter()
            .map(|row| {
                row.chars()
                    .map(|character| crate::terminal::CellSnapshot {
                        text: character.to_string(),
                        foreground_source: crate::terminal::TerminalColor::Default,
                        background_source: crate::terminal::TerminalColor::Default,
                        inverse: false,
                        bold: false,
                        faint: false,
                        italic: false,
                        blinking: false,
                        invisible: false,
                        underline: crate::terminal::TerminalUnderlineSnapshot::None,
                        underline_source: crate::terminal::TerminalColor::Default,
                        strikethrough: false,
                        overline: false,
                        selected: false,
                        spacer_tail: false,
                        semantic_content: crate::terminal::CellSemanticSnapshot::Output,
                        hyperlink: None,
                    })
                    .collect::<Vec<_>>()
                    .into()
            })
            .collect::<Vec<_>>()
            .into();
        ScreenSnapshot::from_test_parts_at(
            rows,
            ScrollbarSnapshot::default(),
            "text preflight",
            generation,
        )
    }

    fn terminal_pane(cx: &mut TestAppContext) -> (Entity<TerminalPane>, &mut VisualTestContext) {
        cx.update(crate::ui::init);
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                .with_start_failure("terminal session unavailable in UI test"),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _cx| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx)
    }

    fn directory_screen(
        generation: u64,
        path: &str,
        freshness: crate::terminal::metadata::MetadataFreshness,
    ) -> Arc<ScreenSnapshot> {
        let mut screen = ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            ScrollbarSnapshot::default(),
            "directory",
            generation,
        );
        {
            let screen = Arc::make_mut(&mut screen);
            let metadata = Arc::make_mut(&mut screen.metadata);
            metadata.directory.path = Arc::from(path);
            metadata.freshness = freshness;
        }
        screen
    }

    fn remote_directory_screen(generation: u64, path: &str) -> Arc<ScreenSnapshot> {
        let mut screen = directory_screen(
            generation,
            path,
            crate::terminal::metadata::MetadataFreshness::Live,
        );
        Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata).context =
            crate::terminal::metadata::TerminalMetadataContext::Remote(
                crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                    crate::domain::SshDestination::new("user@remote".to_owned()).unwrap(),
                    crate::domain::RemoteWorkspaceDirectory::new(path.to_owned()).unwrap(),
                ),
            );
        screen
    }

    #[gpui::test]
    fn only_live_absolute_directory_metadata_should_emit_workspace_reports(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        let reports = Rc::new(RefCell::new(Vec::new()));
        let observed_reports = Rc::clone(&reports);
        pane.update(cx, |_, cx| {
            cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
                if let TerminalPaneEvent::ReportedWorkingDirectoryChanged(path) = event {
                    observed_reports.borrow_mut().push(path.clone());
                }
            })
            .detach();
        });

        pane.update(cx, |pane, cx| {
            pane.handle_event(
                SessionEvent::Screen(directory_screen(
                    1,
                    "/Users/test/live",
                    crate::terminal::metadata::MetadataFreshness::Live,
                )),
                cx,
            );
            pane.handle_event(
                SessionEvent::Screen(directory_screen(
                    2,
                    "/Users/test/stale",
                    crate::terminal::metadata::MetadataFreshness::Stale,
                )),
                cx,
            );
            pane.handle_event(
                SessionEvent::Screen(directory_screen(
                    3,
                    "remote-or-relative",
                    crate::terminal::metadata::MetadataFreshness::Live,
                )),
                cx,
            );
            pane.handle_event(
                SessionEvent::Screen(remote_directory_screen(4, "/srv/remote-project")),
                cx,
            );
        });

        assert_eq!(
            reports.borrow().as_slice(),
            [PathBuf::from("/Users/test/live")]
        );
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.reported_working_directory()),
            None
        );
    }

    #[gpui::test]
    fn visual_bell_presentation_state_clears_on_focus_or_input(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);

        pane.update(cx, |pane, cx| {
            pane.terminal_input_focus = true;
            pane.handle_event(
                SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
                cx,
            );
        });
        assert!(pane.read_with(cx, |pane, _| pane.attention_visual));

        pane.update(cx, |pane, cx| pane.clear_attention(cx));
        assert!(!pane.read_with(cx, |pane, _| pane.attention_visual));
    }

    #[gpui::test]
    fn accepted_input_method_commit_clears_pending_attention(cx: &mut TestAppContext) {
        let (pane, cx, _records) = connected_terminal_pane(cx);
        pane.update(cx, |pane, cx| {
            pane.terminal_input_focus = false;
            pane.handle_event(
                SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
                cx,
            );
            pane.terminal_input_focus = true;
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.attention.unread_count()),
            1
        );

        pane.update(cx, |pane, cx| {
            pane.send_key_translation(
                KeyTranslation::Encoded(KeyInput::input_method_commit("界")),
                cx,
            );
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| pane.attention.unread_count()),
            0
        );
    }

    #[gpui::test]
    fn accepted_paste_clears_pending_attention(cx: &mut TestAppContext) {
        let (pane, cx, _records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::Written),
            Ok(PasteResolution::Cancelled),
        );
        pane.update(cx, |pane, cx| {
            pane.terminal_input_focus = false;
            pane.handle_event(
                SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
                cx,
            );
            pane.terminal_input_focus = true;
        });
        cx.write_to_clipboard(ClipboardItem::new_string("accepted paste".to_owned()));

        cx.dispatch_action(PasteClipboard);
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| pane.attention.unread_count()),
            0
        );
    }

    #[gpui::test]
    fn stale_guarded_written_paste_does_not_clear_newer_attention(cx: &mut TestAppContext) {
        let (pane, cx, _records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::Written),
            Ok(PasteResolution::Cancelled),
        );
        cx.write_to_clipboard(ClipboardItem::new_string("stale paste".to_owned()));

        cx.dispatch_action(PasteClipboard);
        pane.update(cx, |pane, cx| {
            pane.advance_native_service_focus_epoch();
            pane.terminal_input_focus = false;
            pane.handle_event(
                SessionEvent::Attention(
                    crate::terminal::attention::AttentionEvent::CommandFinished {
                        exit_status: Some(0),
                        duration: Duration::from_secs(1),
                    },
                ),
                cx,
            );
            pane.terminal_input_focus = true;
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.attention.unread_count()),
            1
        );

        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| pane.attention.unread_count()),
            1
        );
    }

    fn connected_terminal_pane(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TerminalPane>,
        &mut VisualTestContext,
        TestTerminalSessionRecords,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-keyboard-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _cx| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx, records)
    }

    fn remote_workspace_session_factory(
        records: TestTerminalSessionRecords,
    ) -> WorkspaceTerminalSessionFactory {
        remote_workspace_session_factory_with_readiness(records, Arc::new(AtomicBool::new(true)))
    }

    struct ToggleRemoteChannelProvider {
        ready: Arc<AtomicBool>,
        command_context: Arc<SshCommandContext>,
    }

    impl RemoteTerminalChannelProvider for ToggleRemoteChannelProvider {
        fn is_ready(&self) -> bool {
            self.ready.load(Ordering::SeqCst)
        }

        fn prepare(
            &self,
        ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable>
        {
            if !self.is_ready() {
                return Err(RemoteChannelUnavailable);
            }
            Ok(self.command_context.prepare_pane_channel(
                ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
            ))
        }
    }

    fn remote_workspace_session_factory_with_readiness(
        records: TestTerminalSessionRecords,
        ready: Arc<AtomicBool>,
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
        let channel_provider = Arc::new(ToggleRemoteChannelProvider {
            ready,
            command_context,
        });
        WorkspaceTerminalSessionFactory::new_remote(
            Rc::new(TestTerminalSessionFactory::new(records)),
            test_workspace_directory(PathBuf::from("/local/home")),
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                destination,
                crate::domain::RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
            ),
            "project on remote".to_owned(),
            channel_provider,
        )
    }

    fn connected_remote_terminal_pane(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TerminalPane>,
        &mut VisualTestContext,
        TestTerminalSessionRecords,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory = remote_workspace_session_factory(records.clone());
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx, records)
    }

    fn connected_remote_terminal_pane_with_readiness(
        cx: &mut TestAppContext,
        ready: Arc<AtomicBool>,
    ) -> (
        Entity<TerminalPane>,
        &mut VisualTestContext,
        TestTerminalSessionRecords,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory =
            remote_workspace_session_factory_with_readiness(records.clone(), ready);
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx, records)
    }

    #[gpui::test]
    fn remote_restart_ignores_prior_epoch_events_and_accepts_fresh_generation_one(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_remote_terminal_pane(cx);
        let exits = Rc::new(Cell::new(0));
        let observed_exits = Rc::clone(&exits);
        pane.update(cx, |_, cx| {
            cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
                if matches!(event, TerminalPaneEvent::Exited) {
                    observed_exits.set(observed_exits.get() + 1);
                }
            })
            .detach();
        });

        let (old_epoch, old_accessibility) = pane.update(cx, |pane, cx| {
            let old_epoch = pane.session_epoch;
            pane.handle_session_event(
                old_epoch,
                SessionEvent::Screen(text_screen(90, &["old"])),
                cx,
            );
            pane.record_successfully_presented_screen(Arc::clone(&pane.screen));
            let old_accessibility = Arc::new(TerminalAccessibilityModel::from_screen(
                &text_screen(90, &["old"]),
            ));
            pane.handle_session_accessibility(old_epoch, Arc::clone(&old_accessibility));
            (old_epoch, old_accessibility)
        });
        let factory = pane.read_with(cx, |pane, _| pane.session_factory.clone());
        pane.update(cx, |pane, cx| pane.disconnect_remote(7, cx).unwrap());
        let prepared = pane
            .read_with(cx, |pane, _| pane.prepare_remote_restart(factory, 8))
            .unwrap();
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.commit_remote_restart(prepared, window, cx).unwrap()
            });
        });
        cx.run_until_parked();
        assert_eq!(records.starts().len(), 2);
        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.screen.generation,
                pane.title.clone(),
                Arc::ptr_eq(&pane.accessibility, &old_accessibility),
            )),
            (
                crate::terminal::PresentationGeneration::test(90),
                SharedString::from("text preflight"),
                true,
            )
        );

        pane.update(cx, |pane, cx| {
            pane.handle_session_event(
                old_epoch,
                SessionEvent::Screen(text_screen(99, &["stale"])),
                cx,
            );
            pane.handle_session_event(old_epoch, SessionEvent::Exited(SessionExit::Success), cx);
            pane.handle_session_accessibility(
                old_epoch,
                Arc::new(TerminalAccessibilityModel::from_screen(&text_screen(
                    99,
                    &["stale"],
                ))),
            );
            let current_epoch = pane.session_epoch;
            pane.handle_session_event(
                current_epoch,
                SessionEvent::Screen(text_screen(1, &["fresh"])),
                cx,
            );
            pane.record_successfully_presented_screen(Arc::clone(&pane.screen));
        });

        let state = pane.read_with(cx, |pane, _| {
            (
                pane.screen.generation,
                pane.last_valid_screen.generation,
                pane.title.clone(),
                Arc::ptr_eq(&pane.accessibility, &old_accessibility),
            )
        });
        assert_eq!(state.0, crate::terminal::PresentationGeneration::test(1));
        assert_eq!(state.1, crate::terminal::PresentationGeneration::test(1));
        assert_eq!(state.2.as_ref(), "text preflight");
        assert!(state.3);
        assert_eq!(exits.get(), 0);

        let successor_epoch = pane.read_with(cx, |pane, _| pane.session_epoch);
        let delayed_disconnect = pane.update(cx, |pane, cx| pane.disconnect_remote(7, cx));
        assert_eq!(
            delayed_disconnect,
            Err(RemotePaneLifecycleError::StaleGeneration {
                current: 8,
                received: 7,
            })
        );
        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.session_epoch,
                pane.remote_input_blocked
            )),
            (successor_epoch, false)
        );
    }

    #[gpui::test]
    fn disconnected_remote_pane_blocks_input_but_preserves_copy_selection_and_find(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_remote_terminal_pane(cx);
        let pointer_position = pane.read_with(cx, |pane, _| {
            pane.grid_bounds
                .expect("terminal grid was painted")
                .center()
        });
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.handle_event(SessionEvent::Screen(context_action_screen(None, true)), cx);
                pane.open_find(&OpenTerminalFind, window, cx);
                pane.disconnect_remote(3, cx).unwrap();
                pane.copy_selection(&CopySelection, window, cx);
            });
        });
        cx.simulate_keystrokes("blocked");
        cx.simulate_mouse_move(pointer_position, None, Modifiers::none());
        cx.simulate_event(ScrollWheelEvent {
            position: pointer_position,
            delta: ScrollDelta::Lines(point(0.0, -1.0)),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            pane.read_with(cx, |pane, _| {
                (
                    pane.screen.selection_present,
                    pane.find_input.is_some(),
                    pane.terminal_input_focused(window, cx),
                    pane.session.is_some(),
                )
            })
        });
        assert_eq!(state, (true, true, false, true));
        assert!(
            records.commands().iter().any(|call| {
                matches!(call.command, RecordedSessionCommand::RequestSelectionCopy)
            })
        );
        assert!(!records.commands().iter().any(|call| matches!(
            call.command,
            RecordedSessionCommand::Key(_)
                | RecordedSessionCommand::Pointer(_)
                | RecordedSessionCommand::Wheel(_)
        )));
    }

    #[gpui::test]
    fn failed_master_event_before_disconnect_retains_remote_pane(cx: &mut TestAppContext) {
        let ready = Arc::new(AtomicBool::new(true));
        let (pane, cx, _) = connected_remote_terminal_pane_with_readiness(cx, Arc::clone(&ready));
        ready.store(false, Ordering::SeqCst);

        pane.update(cx, |pane, cx| {
            let epoch = pane.session_epoch;
            pane.handle_session_event(
                epoch,
                SessionEvent::Failed(SessionFailure::Runtime("master exited".to_owned())),
                cx,
            );
            pane.disconnect_remote(7, cx).unwrap();
        });

        assert!(pane.read_with(cx, |pane, _| {
            pane.remote_input_blocked
                && pane.remote_connection_generation == Some(7)
                && pane.pane_state == PaneTerminalState::Running
        }));
    }

    #[gpui::test]
    fn exited_master_event_before_disconnect_retains_remote_pane(cx: &mut TestAppContext) {
        let ready = Arc::new(AtomicBool::new(true));
        let (pane, cx, _) = connected_remote_terminal_pane_with_readiness(cx, Arc::clone(&ready));
        let exits = Rc::new(Cell::new(0));
        let observed_exits = Rc::clone(&exits);
        pane.update(cx, |_, cx| {
            cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
                if matches!(event, TerminalPaneEvent::Exited) {
                    observed_exits.set(observed_exits.get() + 1);
                }
            })
            .detach();
        });
        ready.store(false, Ordering::SeqCst);

        pane.update(cx, |pane, cx| {
            let epoch = pane.session_epoch;
            pane.handle_session_event(epoch, SessionEvent::Exited(SessionExit::Success), cx);
            pane.disconnect_remote(7, cx).unwrap();
        });

        assert_eq!(exits.get(), 0);
        assert!(pane.read_with(cx, |pane, _| {
            pane.remote_input_blocked
                && pane.remote_connection_generation == Some(7)
                && pane.pane_state == PaneTerminalState::Running
        }));
    }

    #[gpui::test]
    fn authoritative_disconnect_before_terminal_events_ignores_exit_and_failure(
        cx: &mut TestAppContext,
    ) {
        let ready = Arc::new(AtomicBool::new(true));
        let (pane, cx, _) = connected_remote_terminal_pane_with_readiness(cx, Arc::clone(&ready));
        let exits = Rc::new(Cell::new(0));
        let observed_exits = Rc::clone(&exits);
        pane.update(cx, |_, cx| {
            cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
                if matches!(event, TerminalPaneEvent::Exited) {
                    observed_exits.set(observed_exits.get() + 1);
                }
            })
            .detach();
        });
        ready.store(false, Ordering::SeqCst);

        pane.update(cx, |pane, cx| {
            let old_epoch = pane.session_epoch;
            pane.disconnect_remote(7, cx).unwrap();
            pane.handle_session_event(
                old_epoch,
                SessionEvent::Failed(SessionFailure::Runtime("late failure".to_owned())),
                cx,
            );
            pane.handle_session_event(
                old_epoch,
                SessionEvent::Exited(SessionExit::ExitCode(255)),
                cx,
            );
        });

        assert_eq!(exits.get(), 0);
        assert!(pane.read_with(cx, |pane, _| {
            pane.remote_input_blocked && pane.pane_state == PaneTerminalState::Running
        }));
    }

    #[gpui::test]
    fn disconnect_and_restart_clear_hidden_input_before_successor_focus(cx: &mut TestAppContext) {
        let (pane, cx, _) = connected_remote_terminal_pane(cx);
        pane.update(cx, |pane, cx| {
            pane.handle_event(SessionEvent::HiddenInputChanged(true), cx);
        });
        assert!(pane.read_with(cx, |pane, _| pane.hidden_input && pane.terminal_input_focus));

        let factory = pane.read_with(cx, |pane, _| pane.session_factory.clone());
        pane.update(cx, |pane, cx| pane.disconnect_remote(7, cx).unwrap());
        assert!(pane.read_with(cx, |pane, _| {
            !pane.hidden_input && !pane.terminal_input_focus
        }));

        let prepared = pane
            .read_with(cx, |pane, _| pane.prepare_remote_restart(factory, 8))
            .unwrap();
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.commit_remote_restart(prepared, window, cx).unwrap();
                assert!(!pane.hidden_input);
            });
        });
    }

    #[gpui::test]
    fn local_pane_rejects_remote_disconnect_without_mutation(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let epoch = pane.read_with(cx, |pane, _| pane.session_epoch);
        let result = pane.update(cx, |pane, cx| pane.disconnect_remote(1, cx));
        assert_eq!(result, Err(RemotePaneLifecycleError::LocalPane));
        assert_eq!(pane.read_with(cx, |pane, _| pane.session_epoch), epoch);
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn remote_pane_disables_local_file_actions_but_preserves_text_services_and_web_links(
        cx: &mut TestAppContext,
    ) {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-remote-pane-capabilities-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        std::fs::write(&file, b"preview").unwrap();
        let local_link = crate::terminal::HyperlinkTarget::osc8(
            "file:preview.txt",
            &directory,
            None,
            TerminalLocalFileCapabilities::Enabled,
        )
        .unwrap();
        let web_link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
        let previews = Rc::new(Cell::new(0));
        let (pane, cx, records) = connected_remote_terminal_pane(cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.screen = context_action_screen(Some(local_link.clone()), true);
                pane.last_geometry = Some(TerminalGeometry::from_grid(
                    CellGridSize::new(1, 1),
                    LogicalCellSize::new(f32::from(pane.cell_width), pane.line_height),
                    BackingScale::ONE,
                ));
                pane.quick_look = Box::new(RecordingQuickLookPresenter {
                    previews: Rc::clone(&previews),
                    dismissals: Rc::new(Cell::new(0)),
                });
                let menu = TerminalContextMenuState {
                    generation: pane.screen.generation,
                    position: SurfacePosition::default(),
                    link: Some(local_link.clone()),
                    selection_present: true,
                    quick_look_eligible: true,
                };
                assert_eq!(
                    pane.context_menu_actions(&menu),
                    NativeContextActions {
                        copy: true,
                        open_link: false,
                        quick_look: false,
                    }
                );
                pane.perform_context_menu_command(
                    menu,
                    TerminalContextMenuCommand::QuickLook,
                    window,
                    cx,
                );
                pane.insert_dropped_file_paths_for_test(&[file.clone()], window, cx);
            });
        });
        cx.run_until_parked();
        cx.write_to_clipboard(ClipboardItem::new_string("clipboard text".to_owned()));
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.paste_clipboard(&PasteClipboard, window, cx);
            });
        });
        cx.run_until_parked();
        let origin = current_native_service_origin(&pane, cx);
        let inserted = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.insert_native_service_text(origin, "ordinary text".to_owned(), window, cx)
            })
        });
        cx.run_until_parked();
        let osc52_request = Osc52AuthorizationRequest {
            id: crate::terminal::Osc52AuthorizationId::new(97),
            access: Osc52Access::Write,
            target: Osc52Target::Standard,
            byte_len: 12,
        };
        records
            .last_event_sender()
            .unwrap()
            .send_blocking(SessionEvent::Osc52Authorization(osc52_request))
            .unwrap();
        cx.run_until_parked();
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.resolve_osc52_authorization(Osc52AuthorizationDecision::Allow, window, cx);
                pane.open_find(&OpenTerminalFind, window, cx);
            });
        });

        assert!(inserted);
        assert_eq!(previews.get(), 0);
        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_some()));
        let generation = pane.read_with(cx, |pane, _| pane.screen.generation);
        assert_eq!(
            activated_link(
                TerminalLocalFileCapabilities::Disabled,
                generation,
                &web_link,
                generation,
                Some(&web_link),
            ),
            Some("https://example.test".to_owned())
        );
        assert!(records.commands().iter().any(|call| {
            call.command == RecordedSessionCommand::RequestPaste("ordinary text".to_owned())
        }));
        assert!(records.commands().iter().any(|call| {
            call.command == RecordedSessionCommand::RequestPaste("clipboard text".to_owned())
        }));
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolveOsc52Authorization(
                    osc52_request.id,
                    Osc52AuthorizationDecision::Allow,
                )
        }));
        assert!(records.commands().iter().all(|call| {
            !matches!(
                &call.command,
                RecordedSessionCommand::RequestPaste(text)
                    if text.contains(file.to_string_lossy().as_ref())
            )
        }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn accessibility_uses_its_bounded_latest_lane_instead_of_screen_rows(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        records
            .last_event_sender()
            .unwrap()
            .send_blocking(SessionEvent::Screen(blinking_screen()))
            .unwrap();
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.accessibility.text().is_empty()));

        let sender = records.last_accessibility_sender().unwrap();
        for text in ["stale", "latest"] {
            sender
                .force_send(Arc::new(TerminalAccessibilityModel::new(
                    vec![crate::terminal::AccessibilityLine::new(
                        vec![crate::terminal::AccessibilityCell::new(text, 1, false)],
                        false,
                    )],
                    0..1,
                    Some((0, 0)),
                )))
                .unwrap();
        }
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.accessibility.text() == "latest"));
    }

    fn accessibility_model(index: usize) -> Arc<TerminalAccessibilityModel> {
        Arc::new(TerminalAccessibilityModel::new(
            vec![crate::terminal::AccessibilityLine::new(
                vec![
                    crate::terminal::AccessibilityCell::new(format!("update-{index}"), 1, false),
                    crate::terminal::AccessibilityCell::new("x", 1, false),
                ],
                false,
            )],
            0..1,
            Some((0, if index.is_multiple_of(2) { 0 } else { 1 })),
        ))
    }

    fn prepare_accessibility_presentation(pane: &Entity<TerminalPane>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| {
                pane.set_accessibility_hierarchy(true, 0);
                pane.sync_native_accessibility(window, false);
            });
        });
    }

    fn visible_surface() -> SurfaceVisibility {
        SurfaceVisibility {
            application_active: true,
            key_window: true,
            minimized: false,
            occluded: false,
            live_resize: false,
            workspace_visible: true,
            pane_visible: true,
        }
    }

    #[gpui::test]
    fn minimized_pane_coalesces_sustained_accessibility_updates_until_restore(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        prepare_accessibility_presentation(&pane, cx);
        pane.update(cx, |pane, _| {
            pane.render_lifecycle.update_visibility(SurfaceVisibility {
                minimized: true,
                ..visible_surface()
            });
            for index in 0..4_096 {
                pane.handle_accessibility(accessibility_model(index));
            }
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.len(),
                    pane.pending_accessibility_notifications
                        .contains(AccessibilityNotification::Value),
                    pane.pending_accessibility_notifications
                        .contains(AccessibilityNotification::Selection),
                    pane.accessibility.text().to_owned(),
                )
            }),
            (2, true, true, "update-4095x".to_owned())
        );

        cx.update(|window, cx| {
            pane.update(cx, |pane, _| {
                pane.render_lifecycle.update_visibility(visible_surface());
                pane.sync_native_accessibility(window, false);
            });
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.is_empty(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .iter()
                        .collect::<Vec<_>>(),
                    pane.accessibility_element.model().text().to_owned(),
                )
            }),
            (
                true,
                vec![
                    AccessibilityNotification::Value,
                    AccessibilityNotification::Selection,
                ],
                "update-4095x".to_owned(),
            )
        );
    }

    #[gpui::test]
    fn occluded_pane_coalesces_sustained_accessibility_updates_until_restore(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        prepare_accessibility_presentation(&pane, cx);
        pane.update(cx, |pane, _| {
            pane.render_lifecycle.update_visibility(SurfaceVisibility {
                occluded: true,
                ..visible_surface()
            });
            for index in 0..4_096 {
                pane.handle_accessibility(accessibility_model(index));
            }
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.len(),
                    pane.accessibility.text().to_owned(),
                )
            }),
            (2, "update-4095x".to_owned())
        );

        cx.update(|window, cx| {
            pane.update(cx, |pane, _| {
                pane.render_lifecycle.update_visibility(visible_surface());
                pane.sync_native_accessibility(window, false);
            });
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.is_empty(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .iter()
                        .collect::<Vec<_>>(),
                    pane.accessibility_element.model().text().to_owned(),
                )
            }),
            (
                true,
                vec![
                    AccessibilityNotification::Value,
                    AccessibilityNotification::Selection,
                ],
                "update-4095x".to_owned(),
            )
        );
    }

    #[gpui::test]
    fn zoom_hidden_pane_retains_only_bounded_accessibility_state_until_restore(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        prepare_accessibility_presentation(&pane, cx);
        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus {
                pane_visible: false,
                focused_pane: false,
                ..TerminalProductFocus::default()
            });
            pane.set_accessibility_hierarchy(false, usize::MAX);
            for index in 0..4_096 {
                pane.handle_accessibility(accessibility_model(index));
            }
        });
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.len(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .is_empty(),
                    pane.accessibility.text().to_owned(),
                )
            }),
            (2, true, "update-4095x".to_owned())
        );

        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus::default());
            pane.set_accessibility_hierarchy(true, 0);
        });
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.is_empty(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .iter()
                        .collect::<Vec<_>>(),
                    pane.accessibility_element.model().text().to_owned(),
                )
            }),
            (
                true,
                vec![
                    AccessibilityNotification::Value,
                    AccessibilityNotification::Selection,
                ],
                "update-4095x".to_owned(),
            )
        );
    }

    #[gpui::test]
    fn inactive_workspace_retains_only_bounded_accessibility_state_until_restore(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        prepare_accessibility_presentation(&pane, cx);
        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus {
                active_workspace: false,
                active_window: false,
                pane_visible: false,
                focused_pane: false,
                blocker: None,
            });
            pane.set_accessibility_hierarchy(false, usize::MAX);
            for index in 0..4_096 {
                pane.handle_accessibility(accessibility_model(index));
            }
        });
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.len(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .is_empty(),
                    pane.accessibility.text().to_owned(),
                )
            }),
            (2, true, "update-4095x".to_owned())
        );

        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus::default());
            pane.set_accessibility_hierarchy(true, 0);
        });
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.is_empty(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .iter()
                        .collect::<Vec<_>>(),
                    pane.accessibility_element.model().text().to_owned(),
                )
            }),
            (
                true,
                vec![
                    AccessibilityNotification::Value,
                    AccessibilityNotification::Selection,
                ],
                "update-4095x".to_owned(),
            )
        );
    }

    #[gpui::test]
    fn hidden_focus_gain_is_delivered_once_when_the_pane_becomes_presented(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        prepare_accessibility_presentation(&pane, cx);
        pane.update(cx, |pane, _| {
            pane.set_accessibility_hierarchy(false, usize::MAX);
            pane.apply_terminal_input_focus(false);
            pane.apply_terminal_input_focus(true);
        });
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| pane.sync_native_accessibility(window, true));
        });
        assert!(pane.read_with(cx, |pane, _| {
            pane.pending_accessibility_notifications
                .contains(AccessibilityNotification::Focus)
                && pane
                    .accessibility_element
                    .delivered_notifications()
                    .is_empty()
        }));

        pane.update(cx, |pane, _| pane.set_accessibility_hierarchy(true, 0));
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| pane.sync_native_accessibility(window, true));
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.is_empty(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .iter()
                        .collect::<Vec<_>>(),
                )
            }),
            (true, vec![AccessibilityNotification::Focus])
        );
    }

    #[gpui::test]
    fn focus_out_and_in_between_presentations_delivers_one_retained_focus_notification(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        prepare_accessibility_presentation(&pane, cx);
        cx.update(|window, cx| {
            pane.update(cx, |pane, _| {
                pane.apply_terminal_input_focus(true);
                pane.sync_native_accessibility(window, true);
            });
        });

        cx.update(|window, cx| {
            pane.update(cx, |pane, pane_cx| {
                pane.set_product_focus(TerminalProductFocus {
                    blocker: Some(TerminalFocusBlocker::Modal),
                    ..TerminalProductFocus::default()
                });
                pane.set_product_focus(TerminalProductFocus::default());
                assert!(pane.synchronize_terminal_input_focus(window, pane_cx));
                pane.sync_native_accessibility(window, true);
            });
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| {
                (
                    pane.pending_accessibility_notifications.is_empty(),
                    pane.accessibility_element
                        .delivered_notifications()
                        .iter()
                        .collect::<Vec<_>>(),
                )
            }),
            (true, vec![AccessibilityNotification::Focus])
        );
    }

    fn connected_terminal_pane_with_key_propagation(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TerminalPane>,
        &mut VisualTestContext,
        TestTerminalSessionRecords,
        Rc<Cell<usize>>,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-keyboard-propagation-test",
            )),
        );
        let propagated_key_downs = Rc::new(Cell::new(0));
        let propagated_for_probe = Rc::clone(&propagated_key_downs);
        let (probe, cx) = cx.add_window_view(|window, cx| {
            let pane = cx.new(|cx| TerminalPane::new(session_factory, window, cx));
            KeyPropagationProbe {
                pane,
                propagated_key_downs: propagated_for_probe,
            }
        });
        let pane = probe.read_with(cx, |probe, _| probe.pane.clone());
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx, records, propagated_key_downs)
    }

    fn terminal_pane_with_selection_copy(
        cx: &mut TestAppContext,
        copy: SelectionCopy,
    ) -> (
        Entity<TerminalPane>,
        &mut VisualTestContext,
        TestTerminalSessionRecords,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(records.clone())
                .with_selection_copy_response(Ok(Some(copy))),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-copy-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _cx| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx, records)
    }

    fn terminal_pane_with_paste_response(
        cx: &mut TestAppContext,
        response: Result<PasteRequestOutcome, String>,
        resolution: Result<PasteResolution, String>,
    ) -> (
        Entity<TerminalPane>,
        &mut VisualTestContext,
        TestTerminalSessionRecords,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(records.clone())
                .with_paste_response(response)
                .with_paste_resolution(resolution),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-paste-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _cx| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx, records)
    }

    fn current_native_service_origin(
        pane: &Entity<TerminalPane>,
        cx: &mut VisualTestContext,
    ) -> NativeServiceOrigin {
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.native_service_status(
                    WorkspaceId::new(1),
                    WindowId::new(1),
                    PaneId::new(1),
                    pane.native_service_hierarchy_generation,
                    window,
                    cx,
                )
                .origin
                .expect("the connected test terminal must expose its Service origin")
            })
        })
    }

    #[gpui::test]
    fn command_equals_should_increase_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd-=");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (18.0, 19.0));
    }

    #[gpui::test]
    fn font_size_changes_notify_accessibility_when_terminal_text_is_static(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                let accessibility = Arc::clone(&pane.accessibility);
                pane.pending_accessibility_notifications = AccessibilityNotifications::default();
                pane.set_font_size(15.0, window, cx);

                assert!(Arc::ptr_eq(&pane.accessibility, &accessibility));
                assert_eq!(
                    pane.pending_accessibility_notifications
                        .iter()
                        .collect::<Vec<_>>(),
                    vec![AccessibilityNotification::Value]
                );
            });
        });
    }

    fn terminal_find_input(
        pane: &Entity<TerminalPane>,
        cx: &mut VisualTestContext,
    ) -> Entity<TextInput> {
        pane.read_with(cx, |pane, _| {
            pane.find_input
                .clone()
                .expect("Terminal Find should remain open")
        })
    }

    fn replace_terminal_find_input(
        input: &Entity<TextInput>,
        text: &str,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, text, window, cx);
            });
        });
    }

    #[gpui::test]
    fn terminal_find_open_edit_navigate_and_close_are_pane_scoped(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);

        cx.dispatch_action(OpenTerminalFind);
        let input = terminal_find_input(&pane, cx);
        replace_terminal_find_input(&input, "日本", cx);
        cx.dispatch_action(FindNext);
        cx.dispatch_action(FindPrevious);
        cx.dispatch_action(CloseTerminalFind);

        let commands = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::SetFindQuery(generation, query) => {
                    Some((format!("set:{query}"), generation))
                }
                RecordedSessionCommand::NavigateFind(generation, FindDirection::Next) => {
                    Some(("next".to_owned(), generation))
                }
                RecordedSessionCommand::NavigateFind(generation, FindDirection::Previous) => {
                    Some(("previous".to_owned(), generation))
                }
                RecordedSessionCommand::EndFind(generation) => Some(("end".to_owned(), generation)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            commands
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["set:", "set:日本", "next", "previous", "end"]
        );
        assert!(commands[0].1 < commands[1].1);
        assert_eq!(commands[1].1, commands[2].1);
        assert_eq!(commands[2].1, commands[3].1);
        assert!(commands[3].1 < commands[4].1);
        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
    }

    #[gpui::test]
    fn terminal_find_should_handle_native_select_all_and_cut(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        let input = terminal_find_input(&pane, cx);
        replace_terminal_find_input(&input, "needle", cx);
        let command_count = records.commands().len();

        cx.dispatch_action(spaceterm_ui::EditSelectAll);
        assert_eq!(
            input.read_with(cx, |input, _| input.selection().range()),
            0..6
        );
        cx.dispatch_action(spaceterm_ui::EditCopy);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("needle".to_owned())
        );
        assert!(
            records
                .commands()
                .into_iter()
                .skip(command_count)
                .all(|call| !matches!(call.command, RecordedSessionCommand::RequestSelectionCopy))
        );

        cx.dispatch_action(spaceterm_ui::EditCut);

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("needle".to_owned())
        );
        assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
    }

    #[gpui::test]
    fn terminal_find_should_handle_native_undo_and_redo(cx: &mut TestAppContext) {
        let (pane, cx, _) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        let input = terminal_find_input(&pane, cx);
        replace_terminal_find_input(&input, "needle", cx);

        cx.dispatch_action(spaceterm_ui::EditUndo);
        assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");

        cx.dispatch_action(spaceterm_ui::EditRedo);
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "needle"
        );
    }

    #[gpui::test]
    fn repeated_terminal_find_selects_and_refocuses_the_existing_query(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        let input = terminal_find_input(&pane, cx);
        replace_terminal_find_input(&input, "needle", cx);
        cx.update(|window, cx| pane.update(cx, |pane, _| pane.focus(window)));

        cx.dispatch_action(OpenTerminalFind);

        let reopened = terminal_find_input(&pane, cx);
        assert_eq!(reopened.entity_id(), input.entity_id());
        assert_eq!(
            reopened.read_with(cx, |input, _| input.selection().range()),
            0..6
        );
        assert!(reopened.read_with(cx, |input, _| input.is_focused()));
        assert_eq!(
            records
                .commands()
                .iter()
                .filter(|call| matches!(call.command, RecordedSessionCommand::SetFindQuery(_, _)))
                .count(),
            2
        );
    }

    #[gpui::test]
    fn dropped_old_terminal_find_input_cannot_change_a_later_find(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        let old_input = terminal_find_input(&pane, cx);
        cx.dispatch_action(CloseTerminalFind);
        cx.dispatch_action(OpenTerminalFind);
        let new_input = terminal_find_input(&pane, cx);
        let command_count = records.commands().len();

        replace_terminal_find_input(&old_input, "stale", cx);

        assert_ne!(old_input.entity_id(), new_input.entity_id());
        assert_eq!(
            new_input.read_with(cx, |input, _| input.value().to_owned()),
            ""
        );
        assert_eq!(records.commands().len(), command_count);
    }

    #[gpui::test]
    fn losing_focused_pane_status_closes_terminal_find(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus {
                active_workspace: true,
                active_window: true,
                focused_pane: true,
                ..TerminalProductFocus::default()
            });
        });
        cx.dispatch_action(OpenTerminalFind);

        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus {
                active_workspace: true,
                active_window: true,
                focused_pane: false,
                ..TerminalProductFocus::default()
            });
        });

        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
        assert!(
            records
                .commands()
                .iter()
                .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_)))
        );
    }

    #[gpui::test]
    fn terminal_find_renders_shared_input_and_moves_responder_focus(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let command_count = records.commands().len();
        let focus_epoch = pane.read_with(cx, |pane, _| pane.native_service_focus_epoch.get());

        cx.dispatch_action(OpenTerminalFind);
        cx.run_until_parked();
        let input = terminal_find_input(&pane, cx);

        assert!(cx.debug_bounds("terminal-find-bar").is_some());
        let input_bounds = cx
            .debug_bounds("terminal-find-input")
            .expect("the shared Find input should be rendered");
        assert!(
            input_bounds.size.width > px(0.0) && input_bounds.size.height > px(0.0),
            "the shared Find input collapsed inside its context-menu decorator: {input_bounds:?}"
        );
        assert!(input.read_with(cx, |input, _| input.is_focused()));
        assert!(pane.read_with(cx, |pane, _| pane.native_service_focus_epoch.get()) > focus_epoch);
        assert!(cx.update(|window, app| {
            pane.read_with(app, |pane, _| !pane.focus_handle.is_focused(window))
        }));

        cx.dispatch_action(CloseTerminalFind);
        cx.simulate_keystrokes("a");
        cx.run_until_parked();
        let commands = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                command @ (RecordedSessionCommand::Focus(_) | RecordedSessionCommand::Key(_)) => {
                    Some(command)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(matches!(commands[0], RecordedSessionCommand::Focus(false)));
        assert!(matches!(commands[1], RecordedSessionCommand::Focus(true)));
        assert!(matches!(commands[2], RecordedSessionCommand::Key(_)));
    }

    #[gpui::test]
    fn terminal_click_restores_terminal_responder_without_closing_find(cx: &mut TestAppContext) {
        let (pane, cx, _) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        let input = terminal_find_input(&pane, cx);
        let terminal = pane.read_with(cx, |pane, _| {
            pane.grid_bounds
                .expect("Terminal grid must be measured")
                .center()
        });

        cx.simulate_click(terminal, Modifiers::none());
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_some()));
        assert!(!input.read_with(cx, |input, _| input.is_focused()));
        assert!(cx.update(|window, app| {
            pane.read_with(app, |pane, _| pane.focus_handle.is_focused(window))
        }));
    }

    #[gpui::test]
    fn terminal_find_submit_navigates_and_escape_cancels(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);

        cx.simulate_keystrokes("enter escape");
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
        assert!(cx.update(|window, app| {
            pane.read_with(app, |pane, _| pane.focus_handle.is_focused(window))
        }));
        assert!(records.commands().iter().any(|call| matches!(
            call.command,
            RecordedSessionCommand::NavigateFind(_, FindDirection::Next)
        )));
        assert!(
            records
                .commands()
                .iter()
                .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_)))
        );
    }

    #[gpui::test]
    fn one_escape_closes_terminal_find_during_active_composition(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        let input = terminal_find_input(&pane, cx);
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "に", None, window, cx);
            });
        });

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
        assert!(
            records
                .commands()
                .iter()
                .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_)))
        );
    }

    #[gpui::test]
    fn terminal_find_enter_and_escape_work_after_each_button_receives_tab_focus(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);

        for tab_count in 1..=3 {
            cx.dispatch_action(OpenTerminalFind);
            pane.update(cx, |pane, cx| {
                Arc::make_mut(&mut pane.screen).find =
                    Some(Arc::new(crate::terminal::TerminalFindSnapshot {
                        generation: pane.find_generation,
                        total_matches: 1,
                        current_match: Some(1),
                        visible_spans: Arc::from([]),
                    }));
                cx.notify();
            });
            cx.run_until_parked();
            let command_count = records.commands().len();

            for _ in 0..tab_count {
                cx.simulate_keystrokes("tab");
            }
            cx.simulate_keystrokes("enter escape");
            cx.run_until_parked();

            assert!(
                pane.read_with(cx, |pane, _| pane.find_input.is_none()),
                "Escape did not close Find after tabbing to button {tab_count}"
            );
            let commands = records.commands();
            assert!(
                commands.iter().skip(command_count).any(|call| matches!(
                    call.command,
                    RecordedSessionCommand::NavigateFind(_, FindDirection::Next)
                )),
                "Enter did not navigate after tabbing to button {tab_count}"
            );
            assert!(
                commands
                    .iter()
                    .skip(command_count)
                    .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_))),
                "Escape did not end Find after tabbing to button {tab_count}"
            );
        }
    }

    #[gpui::test]
    fn terminal_find_tab_delegates_to_its_composite_controls(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        let input = terminal_find_input(&pane, cx);

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert!(!input.read_with(cx, |input, _| input.is_focused()));
        assert!(!cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));

        cx.simulate_keystrokes("shift-tab x");
        cx.run_until_parked();

        assert!(input.read_with(cx, |input, _| input.is_focused()));
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "x"
        );
        assert!(records.commands().iter().any(|call| matches!(
            &call.command,
            RecordedSessionCommand::SetFindQuery(_, query) if query == "x"
        )));
    }

    #[gpui::test]
    fn terminal_find_ime_candidate_geometry_comes_from_shared_input(cx: &mut TestAppContext) {
        let (pane, cx, _) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        cx.run_until_parked();
        let input = terminal_find_input(&pane, cx);
        let input_bounds = cx
            .debug_bounds("terminal-find-input")
            .expect("the shared Find input was not rendered");

        let candidate = cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
                input
                    .bounds_for_range(1..1, input_bounds, window, cx)
                    .expect("the shared input should expose candidate geometry")
            })
        });

        assert!(input.read_with(cx, |input, _| input.composition().is_some()));
        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_some()));
        assert!(candidate.left() > input_bounds.left());
        assert_eq!(candidate.top(), input_bounds.top());
        assert_eq!(candidate.bottom(), input_bounds.bottom());
    }

    #[gpui::test]
    fn terminal_find_buttons_should_disable_navigation_without_results_and_close_find(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.dispatch_action(OpenTerminalFind);
        cx.run_until_parked();
        let command_count = records.commands().len();

        for selector in ["terminal-find-previous", "terminal-find-next"] {
            let button = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} was not rendered"));
            cx.simulate_click(button.center(), Modifiers::none());
        }
        let close = cx
            .debug_bounds("terminal-find-close")
            .expect("the Find close button was not rendered");
        cx.simulate_click(close.center(), Modifiers::none());
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
        assert!(
            records
                .commands()
                .into_iter()
                .skip(command_count)
                .all(|call| !matches!(call.command, RecordedSessionCommand::NavigateFind(_, _)))
        );
    }

    #[gpui::test]
    fn pointer_press_should_follow_synchronous_terminal_focus_admission(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let command_count = records.commands().len();
        let position = pane.read_with(cx, |pane, _| {
            pane.grid_bounds
                .expect("Terminal grid must be measured")
                .center()
        });

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.apply_terminal_input_focus(false);
                pane.on_mouse_down(
                    &MouseDownEvent {
                        position,
                        modifiers: Modifiers::none(),
                        button: MouseButton::Left,
                        click_count: 1,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
            });
        });
        let commands = records
            .commands()
            .into_iter()
            .skip(command_count)
            .map(|call| call.command)
            .collect::<Vec<_>>();

        assert!(matches!(commands[0], RecordedSessionCommand::Focus(false)));
        assert!(matches!(commands[1], RecordedSessionCommand::Focus(true)));
        assert!(matches!(
            commands[2],
            RecordedSessionCommand::Pointer(PointerInput {
                phase: PointerPhase::Press,
                ..
            })
        ));
    }

    #[gpui::test]
    fn text_blink_uses_an_injected_clock_only_while_visible_content_demands_it(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(TerminalProductFocus {
                    active_workspace: true,
                    active_window: true,
                    ..TerminalProductFocus::default()
                });
                pane.handle_event(SessionEvent::Screen(blinking_screen()), cx);
                cx.notify();
            });
        });
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

        cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(TerminalProductFocus {
                    active_workspace: true,
                    active_window: false,
                    ..TerminalProductFocus::default()
                });
                cx.notify();
            });
        });
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

        cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL * 2);
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    }

    #[gpui::test]
    fn focused_cursor_blink_uses_the_injected_pane_clock(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(TerminalProductFocus {
                    active_workspace: true,
                    active_window: true,
                    focused_pane: true,
                    ..TerminalProductFocus::default()
                });
                pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
                cx.notify();
            });
        });
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

        cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    }

    #[gpui::test]
    fn cursor_blink_resets_on_accepted_input_and_focus_gain(cx: &mut TestAppContext) {
        let (pane, cx, _records) = connected_terminal_pane(cx);
        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(TerminalProductFocus {
                    active_workspace: true,
                    active_window: true,
                    focused_pane: true,
                    ..TerminalProductFocus::default()
                });
                pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
                cx.notify();
            });
        });
        cx.run_until_parked();

        cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.send_key_translation(
                    KeyTranslation::Encoded(KeyInput::input_method_commit("x")),
                    cx,
                );
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

        cx.executor()
            .advance_clock(PRESENTATION_BLINK_INTERVAL - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(TerminalProductFocus {
                    active_workspace: true,
                    active_window: true,
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                });
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(TerminalProductFocus {
                    active_workspace: true,
                    active_window: true,
                    focused_pane: true,
                    ..TerminalProductFocus::default()
                });
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

        cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    }

    #[gpui::test]
    fn cursor_blink_has_no_task_when_steady_hidden_or_unfocused_and_close_cancels(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx) = terminal_pane(cx);
        let focused = TerminalProductFocus {
            active_workspace: true,
            active_window: true,
            focused_pane: true,
            ..TerminalProductFocus::default()
        };

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(focused);
                pane.handle_event(
                    SessionEvent::Screen(blinking_cursor_screen(true, false)),
                    cx,
                );
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.handle_event(
                    SessionEvent::Screen(blinking_cursor_screen(false, true)),
                    cx,
                );
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(TerminalProductFocus {
                    focused_pane: false,
                    ..focused
                });
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));
        assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));

        cx.update(|_window, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_product_focus(focused);
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

        let generation = pane.read_with(cx, |pane, _| pane.blink_generation);
        let phase = pane.read_with(cx, |pane, _| pane.blink_phase_visible);
        pane.update(cx, |pane, _| pane.close());
        assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));
        assert_ne!(
            pane.read_with(cx, |pane, _| pane.blink_generation),
            generation
        );
        cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
        cx.run_until_parked();
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.blink_phase_visible),
            phase
        );
    }

    #[test]
    fn pointer_presentation_matches_the_effective_mouse_route() {
        let policy = ShiftSelectionPolicy::OverrideApplicationMouse;

        assert!(pointer_uses_text_cursor(false, false, policy));
        assert!(!pointer_uses_text_cursor(true, false, policy));
        assert!(pointer_uses_text_cursor(true, true, policy));
        assert!(!pointer_uses_text_cursor(
            true,
            true,
            ShiftSelectionPolicy::ReportToApplication,
        ));
    }

    #[test]
    fn context_menu_preserves_application_mouse_tracking_and_shift_override() {
        let policy = ShiftSelectionPolicy::OverrideApplicationMouse;

        assert!(opens_terminal_context_menu(
            PointerButton::Right,
            false,
            false,
            policy,
        ));
        assert!(!opens_terminal_context_menu(
            PointerButton::Right,
            true,
            false,
            policy,
        ));
        assert!(opens_terminal_context_menu(
            PointerButton::Right,
            true,
            true,
            policy,
        ));
        assert!(!opens_terminal_context_menu(
            PointerButton::Right,
            true,
            true,
            ShiftSelectionPolicy::ReportToApplication,
        ));
        assert!(!opens_terminal_context_menu(
            PointerButton::Left,
            false,
            false,
            policy,
        ));
    }

    #[gpui::test]
    fn command_minus_should_decrease_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd--");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (18.0, 17.0));
    }

    #[gpui::test]
    fn command_zero_should_reset_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| pane.set_font_size(20.0, window, cx));
        });
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd-0");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (20.0, DEFAULT_FONT_SIZE));
    }

    #[gpui::test]
    fn command_actions_resolve_before_the_raw_terminal_key_handler(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd-=");

        assert_eq!(
            pane.read_with(cx, |pane, _cx| pane.font_size()),
            before + 1.0
        );
        assert!(
            records
                .commands()
                .iter()
                .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
        );
    }

    #[gpui::test]
    fn copy_action_requests_semantic_selection_and_writes_plain_text_pasteboard(
        cx: &mut TestAppContext,
    ) {
        let (_pane, cx, records) = terminal_pane_with_selection_copy(
            cx,
            SelectionCopy {
                plain_text: "alpha\nbeta".to_owned(),
                html: Some("<pre>alpha\nbeta</pre>".to_owned()),
            },
        );

        cx.simulate_keystrokes("cmd-c");

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("alpha\nbeta".to_owned())
        );
        assert!(
            records.commands().iter().any(|call| {
                matches!(call.command, RecordedSessionCommand::RequestSelectionCopy)
            })
        );
    }

    #[gpui::test]
    fn copy_updates_the_pasteboard_before_the_action_returns(cx: &mut TestAppContext) {
        let (pane, cx, _records) = terminal_pane_with_selection_copy(
            cx,
            SelectionCopy {
                plain_text: "new selection".to_owned(),
                html: None,
            },
        );
        cx.write_to_clipboard(ClipboardItem::new_string("old clipboard".to_owned()));

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.copy_selection(&CopySelection, window, cx);
            });
        });

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("new selection".to_owned())
        );
    }

    #[gpui::test]
    fn native_service_selection_uses_the_ordered_terminal_selection_query(cx: &mut TestAppContext) {
        let (pane, cx, records) = terminal_pane_with_selection_copy(
            cx,
            SelectionCopy {
                plain_text: "authoritative selection".to_owned(),
                html: None,
            },
        );

        let origin = current_native_service_origin(&pane, cx);
        let selection = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.native_service_selection(origin, window, cx)
            })
        });
        let requested_selection = records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedSessionCommand::RequestSelectionCopy));

        assert_eq!(
            (selection.map(|copy| copy.plain_text), requested_selection),
            (Some("authoritative selection".to_owned()), true),
        );
    }

    #[gpui::test]
    fn native_service_return_routes_through_paste_payload_instead_of_ime(cx: &mut TestAppContext) {
        let confirmation = PasteConfirmation {
            id: crate::terminal::PasteConfirmationId::new(17),
            byte_len: 12,
            line_count: 2,
            risk: crate::terminal::PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            },
        };
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
            Ok(PasteResolution::Cancelled),
        );
        let origin = current_native_service_origin(&pane, cx);

        let accepted = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.insert_native_service_text(origin, "first\nsecond".to_owned(), window, cx)
            })
        });
        cx.run_until_parked();
        let commands = records.commands();
        let paste_payload = commands.iter().find_map(|call| match &call.command {
            RecordedSessionCommand::RequestPaste(text) => Some(text.clone()),
            _ => None,
        });
        let ime_input = commands
            .iter()
            .any(|call| matches!(call.command, RecordedSessionCommand::Key(_)));
        let pending_confirmation = pane.read_with(cx, |pane, _| pane.pending_paste);

        assert_eq!(
            (accepted, paste_payload, ime_input, pending_confirmation,),
            (
                true,
                Some("first\nsecond".to_owned()),
                false,
                Some(confirmation),
            ),
        );
    }

    #[gpui::test]
    fn focus_loss_before_paste_reply_cancels_stale_confirmation(cx: &mut TestAppContext) {
        let confirmation = PasteConfirmation {
            id: crate::terminal::PasteConfirmationId::new(18),
            byte_len: 12,
            line_count: 2,
            risk: crate::terminal::PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            },
        };
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
            Ok(PasteResolution::Cancelled),
        );
        let origin = current_native_service_origin(&pane, cx);

        let accepted = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                let accepted =
                    pane.insert_native_service_text(origin, "first\nsecond".to_owned(), window, cx);
                pane.open_find(&OpenTerminalFind, window, cx);
                accepted
            })
        });
        cx.run_until_parked();

        assert!(accepted);
        assert_eq!(pane.read_with(cx, |pane, _| pane.pending_paste), None);
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
        }));
    }

    #[gpui::test]
    fn hierarchy_change_before_paste_reply_cancels_stale_confirmation(cx: &mut TestAppContext) {
        let confirmation = PasteConfirmation {
            id: crate::terminal::PasteConfirmationId::new(19),
            byte_len: 12,
            line_count: 2,
            risk: crate::terminal::PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            },
        };
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
            Ok(PasteResolution::Cancelled),
        );
        let origin = current_native_service_origin(&pane, cx);

        let accepted = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                let accepted =
                    pane.insert_native_service_text(origin, "first\nsecond".to_owned(), window, cx);
                pane.synchronize_native_service_hierarchy_generation(
                    origin.hierarchy_generation().wrapping_add(1),
                );
                accepted
            })
        });
        cx.run_until_parked();

        assert!(accepted);
        assert_eq!(pane.read_with(cx, |pane, _| pane.pending_paste), None);
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
        }));
    }

    #[gpui::test]
    fn responder_focus_away_and_back_invalidates_the_previous_service_origin(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::Written),
            Ok(PasteResolution::Written),
        );
        let origin = current_native_service_origin(&pane, cx);

        let accepted = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.open_find(&OpenTerminalFind, window, cx);
                pane.focus(window);
                pane.insert_native_service_text(origin, "stale return".to_owned(), window, cx)
            })
        });

        assert!(!accepted);
        assert!(
            !records
                .commands()
                .iter()
                .any(|call| { matches!(call.command, RecordedSessionCommand::RequestPaste(_)) })
        );
    }

    #[gpui::test]
    fn native_service_return_is_rejected_without_terminal_input_focus(cx: &mut TestAppContext) {
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::Written),
            Ok(PasteResolution::Written),
        );
        let origin = current_native_service_origin(&pane, cx);
        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus {
                active_workspace: false,
                ..TerminalProductFocus::default()
            });
        });

        let accepted = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.insert_native_service_text(
                    origin,
                    "must not be inserted".to_owned(),
                    window,
                    cx,
                )
            })
        });
        let requested_paste = records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedSessionCommand::RequestPaste(_)));

        assert_eq!((accepted, requested_paste), (false, false));
    }

    #[gpui::test]
    fn unsafe_paste_confirmation_retains_terminal_focus_and_keeps_only_metadata_in_ui(
        cx: &mut TestAppContext,
    ) {
        let confirmation = PasteConfirmation {
            id: crate::terminal::PasteConfirmationId::new(7),
            byte_len: 12,
            line_count: 2,
            risk: crate::terminal::PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            },
        };
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
            Ok(PasteResolution::Written),
        );
        cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));

        cx.dispatch_action(PasteClipboard);
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| pane.pending_paste),
            Some(confirmation)
        );
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
        assert!(
            records
                .commands()
                .iter()
                .any(|call| { matches!(call.command, RecordedSessionCommand::RequestPaste(_)) })
        );

        let confirm = cx
            .debug_bounds("confirm-unsafe-paste")
            .expect("unsafe Paste should expose its confirmation button");
        cx.simulate_click(confirm.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Confirm)
        }));
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
    }

    #[gpui::test]
    fn unsafe_paste_prompt_enter_should_confirm_without_moving_responder_focus(
        cx: &mut TestAppContext,
    ) {
        let confirmation = PasteConfirmation {
            id: crate::terminal::PasteConfirmationId::new(6),
            byte_len: 12,
            line_count: 2,
            risk: crate::terminal::PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            },
        };
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
            Ok(PasteResolution::Written),
        );
        cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));
        cx.dispatch_action(PasteClipboard);
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Confirm)
        }));
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
    }

    #[gpui::test]
    fn unsafe_paste_prompt_escape_should_cancel_without_moving_responder_focus(
        cx: &mut TestAppContext,
    ) {
        let confirmation = PasteConfirmation {
            id: crate::terminal::PasteConfirmationId::new(8),
            byte_len: 12,
            line_count: 2,
            risk: crate::terminal::PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            },
        };
        let (_pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
            Ok(PasteResolution::Cancelled),
        );
        cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));
        cx.dispatch_action(PasteClipboard);
        cx.run_until_parked();

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
        }));
    }

    #[gpui::test]
    fn losing_product_focus_cancels_pending_paste_without_confirming_it(cx: &mut TestAppContext) {
        let confirmation = PasteConfirmation {
            id: crate::terminal::PasteConfirmationId::new(9),
            byte_len: 12,
            line_count: 2,
            risk: crate::terminal::PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            },
        };
        let (pane, cx, records) = terminal_pane_with_paste_response(
            cx,
            Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
            Ok(PasteResolution::Cancelled),
        );
        cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));
        cx.dispatch_action(PasteClipboard);
        cx.run_until_parked();

        pane.update(cx, |pane, _| {
            pane.set_product_focus(TerminalProductFocus {
                active_workspace: false,
                ..TerminalProductFocus::default()
            });
        });
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.pending_paste.is_none()));
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
        }));
    }

    #[gpui::test]
    fn osc52_prompt_retains_focus_and_resolves_only_opaque_metadata(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let request = Osc52AuthorizationRequest {
            id: crate::terminal::Osc52AuthorizationId::new(11),
            access: Osc52Access::Write,
            target: Osc52Target::Standard,
            byte_len: 42,
        };

        records
            .last_event_sender()
            .unwrap()
            .send_blocking(SessionEvent::Osc52Authorization(request))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| pane.pending_osc52),
            Some(request)
        );
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));

        let deny = cx
            .debug_bounds("deny-osc52-clipboard")
            .expect("OSC 52 authorization should expose its denial button");
        cx.simulate_click(deny.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolveOsc52Authorization(
                    request.id,
                    Osc52AuthorizationDecision::Deny,
                )
        }));
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
    }

    #[gpui::test]
    fn raw_key_down_and_key_up_reach_the_session_as_distinct_actions(cx: &mut TestAppContext) {
        let (_pane, cx, records) = connected_terminal_pane(cx);
        let keystroke = Keystroke {
            key: "a".to_owned(),
            key_char: Some("a".to_owned()),
            modifiers: Modifiers::default(),
        };

        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });

        let actions = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Key(input) => Some(input.action),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(actions, [KeyAction::Press, KeyAction::Release]);
    }

    #[gpui::test]
    fn unhandled_key_translation_preserves_pane_presentation_and_propagates(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        pane.update(cx, |pane, cx| {
            pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
            pane.blink_phase_visible = false;
        });
        cx.run_until_parked();
        let (
            presentation_generation,
            pending_frame,
            blink_generation,
            blink_phase_visible,
            diagnostic_count,
        ) = pane.read_with(cx, |pane, _| {
            (
                pane.screen.generation,
                pane.render_lifecycle.take_frame(),
                pane.blink_generation,
                pane.blink_phase_visible,
                pane.diagnostics.record_count(),
            )
        });
        let key_count_before = records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
            .count();

        let handled = pane.update(cx, |pane, cx| {
            pane.send_key_translation(
                KeyTranslation::Unhandled(UnhandledKeyEvent {
                    kind: NativeKeyEventKind::KeyDown,
                    action: KeyAction::Press,
                    native_key_code: Some(u16::MAX),
                }),
                cx,
            )
        });

        let key_count_after = records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
            .count();
        let after = pane.read_with(cx, |pane, _| {
            (
                pane.screen.generation,
                pane.render_lifecycle.take_frame(),
                pane.blink_generation,
                pane.blink_phase_visible,
                pane.diagnostics.record_count(),
                pane.authoritative_status(),
                pane.pane_state.clone(),
            )
        });
        assert_eq!(
            (
                handled,
                key_count_before,
                key_count_after,
                after,
                cx.debug_bounds("terminal-status"),
            ),
            (
                false,
                0,
                0,
                (
                    presentation_generation,
                    pending_frame,
                    blink_generation,
                    blink_phase_visible,
                    diagnostic_count + 1,
                    None,
                    PaneTerminalState::Running,
                ),
                None,
            )
        );
    }

    #[gpui::test]
    fn unhandled_key_down_preserves_attention_and_propagates(cx: &mut TestAppContext) {
        let (pane, cx, _records, propagated_key_downs) =
            connected_terminal_pane_with_key_propagation(cx);
        let attention_events = Rc::new(Cell::new(0));
        let attention_events_for_subscription = Rc::clone(&attention_events);
        pane.update(cx, |_, cx| {
            cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
                if matches!(event, TerminalPaneEvent::AttentionChanged { .. }) {
                    attention_events_for_subscription
                        .set(attention_events_for_subscription.get() + 1);
                }
            })
            .detach();
        });
        pane.update(cx, |pane, cx| {
            pane.terminal_input_focus = false;
            pane.handle_event(
                SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
                cx,
            );
            pane.terminal_input_focus = true;
        });
        let before = pane.read_with(cx, |pane, _| {
            (
                pane.attention.unread_count(),
                pane.attention.visual_bell(),
                pane.attention_visual,
                pane.attention_generation,
                pane._attention_task.is_some(),
                pane.diagnostics.record_count(),
            )
        });
        let attention_events_before = attention_events.get();

        cx.simulate_event(event("hyper", None, Modifiers::default()));

        let after = pane.read_with(cx, |pane, _| {
            (
                pane.attention.unread_count(),
                pane.attention.visual_bell(),
                pane.attention_visual,
                pane.attention_generation,
                pane._attention_task.is_some(),
                pane.diagnostics.record_count(),
            )
        });
        assert_eq!(
            (
                before,
                after,
                attention_events_before,
                attention_events.get(),
                propagated_key_downs.get(),
            ),
            (
                (1, true, true, before.3, true, before.5),
                (1, true, true, before.3, true, before.5 + 1),
                attention_events_before,
                attention_events_before,
                1,
            )
        );
    }

    #[gpui::test]
    fn printable_text_without_physical_identity_reaches_the_terminal_session(
        cx: &mut TestAppContext,
    ) {
        let (_pane, cx, records) = connected_terminal_pane(cx);

        cx.simulate_event(event("hyper", Some("界"), Modifiers::default()));

        let inputs = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Key(input) => Some(input),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(inputs, [KeyInput::text_input("界")]);
    }

    #[gpui::test]
    fn authoritative_focus_transitions_share_one_deduplicated_session_path(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let focus_commands = || {
            records
                .commands()
                .into_iter()
                .filter_map(|call| match call.command {
                    RecordedSessionCommand::Focus(focused) => Some(focused),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(focus_commands(), vec![false, true]);

        cx.update(|_window, app| {
            pane.update(app, |pane, app| {
                pane.set_product_focus(TerminalProductFocus {
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                });
                app.notify();
            });
        });
        cx.run_until_parked();
        assert_eq!(focus_commands(), vec![false, true, false]);

        cx.update(|_window, app| {
            pane.update(app, |pane, app| {
                pane.set_product_focus(TerminalProductFocus {
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                });
                app.notify();
            });
        });
        cx.run_until_parked();
        assert_eq!(focus_commands(), vec![false, true, false]);

        cx.update(|_window, app| {
            pane.update(app, |pane, app| {
                pane.set_product_focus(TerminalProductFocus::default());
                app.notify();
            });
        });
        cx.run_until_parked();
        assert_eq!(focus_commands(), vec![false, true, false, true]);

        cx.deactivate_window();
        assert_eq!(focus_commands(), vec![false, true, false, true, false]);
    }

    #[gpui::test]
    fn non_key_operating_system_window_should_remain_distinct_from_active_application(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let command_count = records.commands().len();
        let second_window = cx.add_window(|_, _| EmptyView);

        second_window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        let non_key = cx.update(|window, cx| {
            assert!(cx.active_window().is_some());
            assert!(!window.is_window_active());
            pane.read(cx).terminal_input_focused(window, cx)
        });
        assert!(!non_key);

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let focus_edges = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some((call.session_id, focused)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
        assert_eq!(focus_edges, [(1, false), (1, true)]);
    }

    #[gpui::test]
    fn native_save_panel_should_block_before_prompt_and_restore_after_cancel(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let command_count = records.commands().len();

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.export_diagnostics(&ExportTerminalDiagnostics, window, cx);
            });
        });

        let blocked = cx.update(|window, cx| {
            let pane = pane.read(cx);
            (
                pane.native_modal_open,
                pane.terminal_input_focused(window, cx),
            )
        });
        assert_eq!(
            (blocked, cx.did_prompt_for_new_path()),
            ((true, false), true)
        );

        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();

        let restored = cx.update(|window, cx| {
            let pane = pane.read(cx);
            (
                pane.native_modal_open,
                pane.terminal_input_focused(window, cx),
            )
        });
        let focus_edges = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!((restored, focus_edges), ((false, true), vec![false, true]));
    }

    #[gpui::test]
    fn file_drop_from_inactive_app_focuses_before_requesting_paste(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.deactivate_window();

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.insert_dropped_file_paths_for_test(
                    &[PathBuf::from("/tmp/a dropped file")],
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        let mut relevant = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(true) => Some(RecordedSessionCommand::Focus(true)),
                RecordedSessionCommand::RequestPaste(text) => {
                    Some(RecordedSessionCommand::RequestPaste(text))
                }
                _ => None,
            })
            .rev()
            .take(2)
            .collect::<Vec<_>>();
        relevant.reverse();
        assert_eq!(
            relevant,
            vec![
                RecordedSessionCommand::Focus(true),
                RecordedSessionCommand::RequestPaste("'/tmp/a dropped file'".to_owned()),
            ]
        );
    }

    fn event(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                key: key.to_owned(),
                key_char: key_char.map(ToOwned::to_owned),
                modifiers,
            },
            is_held: false,
        }
    }

    fn expected_key(
        physical_key: PhysicalKey,
        logical_key: &str,
        text: Option<&str>,
        modifiers: InputModifiers,
    ) -> KeyInput {
        KeyInput {
            action: KeyAction::Press,
            physical_key,
            native_key_code: None,
            logical_key: logical_key.to_owned(),
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: single_char(logical_key).map(unshifted_character),
            modifiers,
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: OptionAsAltPolicy::default(),
        }
    }

    #[test]
    fn reported_terminal_title_should_replace_the_shell_fallback() {
        assert_eq!(
            normalized_pane_title("  Claude Code  ", "zsh"),
            "Claude Code"
        );
    }

    #[test]
    fn preferred_terminal_font_is_selected_when_present() {
        let available = vec!["Menlo".to_owned(), "JetBrains Mono".to_owned()];

        assert_eq!(select_terminal_font(&available), "JetBrains Mono");
    }

    #[test]
    fn system_monospace_font_is_selected_when_preferred_fonts_are_absent() {
        let available = vec!["Helvetica".to_owned(), "Menlo".to_owned()];

        assert_eq!(select_terminal_font(&available), "Menlo");
    }

    #[test]
    fn ime_candidate_bounds_follow_wrapped_wide_preedit_caret() {
        let element_bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(50.0), px(60.0)));
        let layout = layout_preedit("界", 0, 4, 5, 1);

        assert_eq!(
            ime_candidate_bounds(element_bounds, 5, px(10.0), px(20.0), layout.caret),
            Bounds::new(point(px(30.0), px(40.0)), size(px(10.0), px(20.0)))
        );
    }

    #[gpui::test]
    fn unchanged_marked_text_reuses_logical_preedit_clusters(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        pane.update(cx, |pane, _| {
            pane.screen = blinking_cursor_screen(true, false);
            pane.mark_for_preedit_cache_test("かな", 2..2);
        });

        let first = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());
        let second = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());

        assert!(Arc::ptr_eq(&first.clusters, &second.clusters));
    }

    #[gpui::test]
    fn marked_text_edit_replaces_logical_preedit_clusters(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        pane.update(cx, |pane, _| {
            pane.screen = blinking_cursor_screen(true, false);
            pane.mark_for_preedit_cache_test("か", 1..1);
        });
        let first = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());
        pane.update(cx, |pane, _| pane.mark_for_preedit_cache_test("かな", 2..2));

        let second = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());

        assert!(!Arc::ptr_eq(&first.clusters, &second.clusters));
    }

    #[gpui::test]
    fn native_shaper_resolves_emoji_through_terminal_fallbacks(cx: &mut TestAppContext) {
        let (_pane, cx) = terminal_pane(cx);

        cx.update(|window, _cx| {
            let text = "👩\u{200d}💻";
            let run = TextRun {
                len: text.len(),
                font: crate::ui::terminal_element::terminal_cell_font(
                    &"Menlo".into(),
                    false,
                    false,
                ),
                color: gpui_color(ACTIVE_THEME.terminal_foreground).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };

            let shaped =
                window
                    .text_system()
                    .shape_line(text.into(), px(DEFAULT_FONT_SIZE), &[run], None);

            assert_eq!(shaped.len(), text.len());
            assert!(
                shaped
                    .runs
                    .iter()
                    .flat_map(|run| &run.glyphs)
                    .any(|glyph| glyph.is_emoji)
            );
        });
    }

    #[gpui::test]
    fn marked_text_stays_local_and_commits_each_input_method_once(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let screen_before = pane.read_with(cx, |pane, _| pane.screen.clone());
        let committed = [
            ("´", "é"),
            ("にほん", "日本"),
            ("ㅎㅏㄴ", "한"),
            ("zhong", "中"),
            ("👩\u{200d}", "👩\u{200d}💻"),
        ];

        for (marked, commit) in committed {
            let key_count_before_mark = records
                .commands()
                .iter()
                .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
                .count();
            cx.update(|window, app| {
                pane.update(app, |pane, pane_cx| {
                    pane.replace_and_mark_text_in_range(
                        None,
                        marked,
                        Some(marked.encode_utf16().count()..marked.encode_utf16().count()),
                        window,
                        pane_cx,
                    );
                });
            });
            assert_eq!(
                records
                    .commands()
                    .iter()
                    .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
                    .count(),
                key_count_before_mark
            );
            assert!(pane.read_with(cx, |pane, _| Arc::ptr_eq(&pane.screen, &screen_before)));
            cx.update(|window, app| {
                pane.update(app, |pane, pane_cx| {
                    pane.replace_text_in_range(None, commit, window, pane_cx);
                });
            });
        }

        let commits = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Key(input) if input.is_input_method_commit() => input.text,
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(commits, ["é", "日本", "한", "中", "👩\u{200d}💻"]);
    }

    #[gpui::test]
    fn cancellation_and_focus_loss_discard_marked_text_without_bytes(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let mark = |pane: &Entity<TerminalPane>, cx: &mut VisualTestContext| {
            cx.update(|window, app| {
                pane.update(app, |pane, pane_cx| {
                    pane.replace_and_mark_text_in_range(None, "かな", Some(2..2), window, pane_cx);
                });
            });
        };

        mark(&pane, cx);
        cx.update(|window, app| {
            pane.update(app, |pane, pane_cx| pane.unmark_text(window, pane_cx));
        });
        assert!(pane.read_with(cx, |pane, _| pane.ime.marked_text().is_none()));

        mark(&pane, cx);
        cx.update(|_window, app| {
            pane.update(app, |pane, app| {
                pane.set_product_focus(TerminalProductFocus {
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                });
                app.notify();
            });
        });
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.ime.marked_text().is_none()));
        assert!(
            records
                .commands()
                .iter()
                .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
        );
    }

    #[gpui::test]
    fn raw_key_callbacks_are_suppressed_while_marked_text_is_active(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        cx.update(|window, app| {
            pane.update(app, |pane, pane_cx| {
                pane.replace_and_mark_text_in_range(None, "に", Some(1..1), window, pane_cx);
            });
        });

        cx.simulate_event(event("a", Some("a"), Modifiers::default()));

        assert!(
            records
                .commands()
                .iter()
                .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
        );

        cx.update(|window, app| {
            pane.update(app, |pane, pane_cx| {
                pane.replace_text_in_range(None, "日", window, pane_cx);
            });
        });
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke {
                key: "a".to_owned(),
                key_char: Some("a".to_owned()),
                modifiers: Modifiers::default(),
            },
        });

        let keys = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Key(input) => Some(input),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 1);
        assert!(keys[0].is_input_method_commit());
    }

    #[test]
    fn empty_terminal_title_should_restore_the_shell_fallback() {
        assert_eq!(normalized_pane_title("\n\t", "zsh"), "zsh");
    }

    #[test]
    fn pane_title_should_remove_control_characters() {
        assert_eq!(
            normalized_pane_title("cargo\u{7} test", "zsh"),
            "cargo test"
        );
    }

    #[test]
    fn maps_printable_text_and_terminal_keys() {
        assert_eq!(
            encode_key(&event("a", Some("a"), Modifiers::default())),
            KeyTranslation::Encoded(expected_key(
                PhysicalKey::A,
                "a",
                Some("a"),
                InputModifiers::default(),
            ))
        );
        assert_eq!(
            encode_key(&event("enter", None, Modifiers::default())),
            KeyTranslation::Encoded(expected_key(
                PhysicalKey::Enter,
                "enter",
                None,
                InputModifiers::default(),
            ))
        );
        assert_eq!(
            encode_key(&event("up", None, Modifiers::default())),
            KeyTranslation::Encoded(expected_key(
                PhysicalKey::ArrowUp,
                "up",
                None,
                InputModifiers::default(),
            ))
        );
    }

    #[test]
    fn preserves_control_and_alt_for_worker_side_encoding() {
        let modifiers = Modifiers {
            control: true,
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(
            encode_key(&event("c", Some("c"), modifiers)),
            KeyTranslation::Encoded(expected_key(
                PhysicalKey::C,
                "c",
                Some("c"),
                InputModifiers {
                    control: true,
                    alt: true,
                    ..InputModifiers::default()
                },
            ))
        );
    }

    #[test]
    fn control_d_should_be_sent_to_the_shell_as_eof_input() {
        let modifiers = Modifiers {
            control: true,
            ..Modifiers::default()
        };

        assert_eq!(
            encode_key(&event("d", Some("d"), modifiers)),
            KeyTranslation::Encoded(expected_key(
                PhysicalKey::D,
                "d",
                Some("d"),
                InputModifiers {
                    control: true,
                    ..InputModifiers::default()
                },
            ))
        );
    }

    #[test]
    fn preserves_unbound_command_keys_for_terminal_protocol_encoding() {
        let modifiers = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert_eq!(
            encode_key(&event("q", None, modifiers)),
            KeyTranslation::Encoded(expected_key(
                PhysicalKey::Q,
                "q",
                None,
                InputModifiers {
                    platform: true,
                    ..InputModifiers::default()
                },
            ))
        );
    }

    #[test]
    fn unsupported_keys_are_unhandled_without_native_identity() {
        assert_eq!(
            encode_key(&event("hyper", None, Modifiers::default())),
            KeyTranslation::Unhandled(UnhandledKeyEvent {
                kind: NativeKeyEventKind::KeyDown,
                action: KeyAction::Press,
                native_key_code: None,
            })
        );
    }

    #[test]
    fn unsupported_command_keys_with_text_are_unhandled() {
        let modifiers = Modifiers {
            platform: true,
            ..Modifiers::default()
        };

        assert_eq!(
            encode_key(&event("hyper", Some("x"), modifiers)),
            KeyTranslation::Unhandled(UnhandledKeyEvent {
                kind: NativeKeyEventKind::KeyDown,
                action: KeyAction::Press,
                native_key_code: None,
            })
        );
    }

    #[test]
    fn maps_rendered_positions_to_reported_terminal_geometry() {
        let bounds = Bounds::new(
            gpui::point(px(10.0), px(20.0)),
            gpui::size(px(75.0), px(40.0)),
        );
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(10, 2),
            LogicalCellSize::new(7.5, 20.0),
            BackingScale::ONE,
        );

        let position =
            terminal_surface_position(bounds, gpui::point(px(47.5), px(30.0)), geometry, false)
                .unwrap();

        assert_eq!(position, SurfacePosition { x: 37.5, y: 10.0 });
        assert!(
            terminal_surface_position(bounds, gpui::point(px(9.0), px(20.0)), geometry, false,)
                .is_none()
        );
        assert_eq!(
            terminal_surface_position(bounds, gpui::point(px(2.5), px(65.0)), geometry, true),
            Some(SurfacePosition { x: -7.5, y: 45.0 })
        );
    }

    #[test]
    fn accumulates_fractional_trackpad_scroll_into_terminal_steps() {
        let mut accumulator = WheelAccumulator::default();
        assert_eq!(
            accumulator.push(0.4, 0.4, WheelPhase::GestureStarted),
            (0, 0)
        );
        assert_eq!(
            accumulator.push(0.7, 0.7, WheelPhase::GestureChanged),
            (1, 1)
        );
        assert_eq!(
            accumulator.push(-1.3, -1.3, WheelPhase::MomentumStarted),
            (-1, -1)
        );
        assert_eq!(
            accumulator.push(0.0, 0.0, WheelPhase::MomentumCancelled),
            (0, 0)
        );
        assert_eq!((accumulator.horizontal, accumulator.vertical), (0.0, 0.0));
    }

    #[test]
    fn hyperlinks_open_only_on_same_generation_explicit_release() {
        let link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
        let first = crate::terminal::PresentationGeneration::test(1);
        let second = crate::terminal::PresentationGeneration::test(2);

        assert_eq!(
            activated_link(
                TerminalLocalFileCapabilities::Enabled,
                first,
                &link,
                first,
                Some(&link),
            ),
            Some("https://example.test".to_owned())
        );
        assert_eq!(
            activated_link(
                TerminalLocalFileCapabilities::Enabled,
                first,
                &link,
                second,
                Some(&link),
            ),
            None
        );
        assert_eq!(
            activated_link(
                TerminalLocalFileCapabilities::Enabled,
                first,
                &link,
                first,
                None,
            ),
            None
        );
    }

    #[test]
    fn hovered_link_is_inert_after_presentation_generation_advances() {
        let link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
        let first = crate::terminal::PresentationGeneration::test(1);
        let second = crate::terminal::PresentationGeneration::test(2);
        let hovered = (first, link);

        assert!(hovered_link_for_generation(Some(&hovered), first).is_some());
        assert_eq!(hovered_link_for_generation(Some(&hovered), second), None);
        assert_eq!(
            NativeContextActions::from_presence(
                TerminalLocalFileCapabilities::Enabled,
                false,
                hovered_link_for_generation(Some(&hovered), second),
            ),
            NativeContextActions::default()
        );
    }

    #[test]
    fn context_link_requires_the_clicked_generation_and_identity() {
        let clicked = crate::terminal::HyperlinkTarget::url("https://example.test/first").unwrap();
        let replacement =
            crate::terminal::HyperlinkTarget::url("https://example.test/second").unwrap();
        let first = crate::terminal::PresentationGeneration::test(1);
        let second = crate::terminal::PresentationGeneration::test(2);

        assert_eq!(
            revalidated_context_link(first, Some(&clicked), first, Some(&clicked)),
            Some(&clicked)
        );
        assert_eq!(
            revalidated_context_link(first, Some(&clicked), second, Some(&clicked)),
            None
        );
        assert_eq!(
            revalidated_context_link(first, Some(&clicked), first, Some(&replacement)),
            None
        );
        assert_eq!(revalidated_context_link(first, None, first, None), None);
    }

    #[gpui::test]
    fn context_copy_requires_the_frozen_presentation_generation(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);

        let (current, stale) = pane.update(cx, |pane, _| {
            pane.screen = context_action_screen(None, true);
            let mut menu = TerminalContextMenuState {
                generation: pane.screen.generation,
                position: SurfacePosition::default(),
                link: None,
                selection_present: true,
                quick_look_eligible: false,
            };
            let current = pane.context_menu_actions(&menu);
            menu.generation = crate::terminal::PresentationGeneration::test(6);
            (current, pane.context_menu_actions(&menu))
        });

        assert!(current.copy);
        assert!(!stale.copy);
    }

    #[gpui::test]
    fn context_menu_focus_blocker_reports_focus_out_before_focus_in(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.context_menu = Some(TerminalContextMenuState {
                    generation: pane.screen.generation,
                    position: SurfacePosition::default(),
                    link: None,
                    selection_present: false,
                    quick_look_eligible: false,
                });
                pane.sync_terminal_input_focus(window, cx);
                pane.context_menu_closed(cx);
                pane.sync_terminal_input_focus(window, cx);
            });
        });

        let reports = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reports, [false, true, false, true]);
    }

    #[gpui::test]
    fn local_right_click_opens_the_packaged_menu_and_copy_revalidates_selection(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        pane.update(cx, |pane, cx| {
            pane.screen = context_action_screen(
                crate::terminal::HyperlinkTarget::url("https://example.test"),
                true,
            );
            cx.notify();
        });
        cx.run_until_parked();
        let click = pane.read_with(cx, |pane, _| {
            let bounds = pane.grid_bounds.expect("terminal grid was painted");
            point(
                bounds.origin.x + pane.cell_width / 2.0,
                bounds.origin.y + px(pane.line_height / 2.0),
            )
        });

        cx.simulate_mouse_down(click, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(click, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();

        cx.simulate_mouse_move(click, None, Modifiers::none());
        cx.simulate_event(ScrollWheelEvent {
            position: click,
            delta: ScrollDelta::Lines(point(0.0, -1.0)),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("terminal-context-menu").is_some());
        assert!(
            cx.debug_bounds("terminal-context-menu-row-copy-enabled")
                .is_some()
        );
        assert!(
            cx.debug_bounds("terminal-context-menu-row-open-link-enabled")
                .is_some()
        );
        assert!(
            cx.debug_bounds("terminal-context-menu-row-quick-look-disabled")
                .is_some()
        );
        assert!(records.commands().iter().all(|call| !matches!(
            call.command,
            RecordedSessionCommand::Pointer(_) | RecordedSessionCommand::Wheel(_)
        )));

        let copy = cx
            .debug_bounds("terminal-context-menu-row-copy-enabled")
            .expect("Copy row was not rendered")
            .center();
        cx.simulate_mouse_move(copy, None, Modifiers::none());
        cx.simulate_click(copy, Modifiers::none());
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
        let relevant = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => {
                    Some(RecordedSessionCommand::Focus(focused))
                }
                RecordedSessionCommand::RequestSelectionCopy => {
                    Some(RecordedSessionCommand::RequestSelectionCopy)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            &relevant[relevant.len() - 3..],
            [
                RecordedSessionCommand::Focus(false),
                RecordedSessionCommand::Focus(true),
                RecordedSessionCommand::RequestSelectionCopy,
            ]
        );
    }

    #[gpui::test]
    fn context_menu_keys_never_reach_the_terminal_session(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        pane.update(cx, |pane, cx| {
            pane.screen = context_action_screen(None, true);
            cx.notify();
        });
        cx.run_until_parked();
        let click = pane.read_with(cx, |pane, _| {
            let bounds = pane.grid_bounds.expect("terminal grid was painted");
            bounds.center()
        });

        cx.simulate_mouse_down(click, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(click, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_some()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
        assert!(
            records
                .commands()
                .iter()
                .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
        );
    }

    #[gpui::test]
    fn quick_look_command_revalidates_then_calls_the_retained_presenter(cx: &mut TestAppContext) {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-context-quick-look-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        std::fs::write(&file, b"preview").unwrap();
        let link = crate::terminal::HyperlinkTarget::osc8(
            "file:preview.txt",
            &directory,
            None,
            TerminalLocalFileCapabilities::Enabled,
        )
        .unwrap();
        let previews = Rc::new(Cell::new(0));
        let dismissals = Rc::new(Cell::new(0));
        let (pane, cx, _records) = connected_terminal_pane(cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.screen = context_action_screen(Some(link.clone()), false);
                pane.last_geometry = Some(TerminalGeometry::from_grid(
                    CellGridSize::new(1, 1),
                    LogicalCellSize::new(f32::from(pane.cell_width), pane.line_height),
                    BackingScale::ONE,
                ));
                pane.quick_look = Box::new(RecordingQuickLookPresenter {
                    previews: Rc::clone(&previews),
                    dismissals: Rc::clone(&dismissals),
                });
                let menu = TerminalContextMenuState {
                    generation: pane.screen.generation,
                    position: SurfacePosition::default(),
                    link: Some(link),
                    selection_present: false,
                    quick_look_eligible: true,
                };
                pane.context_menu = Some(menu.clone());
                pane.sync_terminal_input_focus(window, cx);
                pane.perform_context_menu_command(
                    menu,
                    TerminalContextMenuCommand::QuickLook,
                    window,
                    cx,
                );
            });
        });

        assert_eq!(previews.get(), 1);
        assert_eq!(dismissals.get(), 0);
        pane.update(cx, |pane, _| pane.close());
        assert_eq!(dismissals.get(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn stale_context_generation_never_reaches_the_presenter(cx: &mut TestAppContext) {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-stale-context-quick-look-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("preview.txt");
        std::fs::write(&file, b"preview").unwrap();
        let link = crate::terminal::HyperlinkTarget::osc8(
            "file:preview.txt",
            &directory,
            None,
            TerminalLocalFileCapabilities::Enabled,
        )
        .unwrap();
        let previews = Rc::new(Cell::new(0));
        let dismissals = Rc::new(Cell::new(0));
        let (pane, cx, _records) = connected_terminal_pane(cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                let clicked_screen = context_action_screen(Some(link.clone()), false);
                pane.last_geometry = Some(TerminalGeometry::from_grid(
                    CellGridSize::new(1, 1),
                    LogicalCellSize::new(f32::from(pane.cell_width), pane.line_height),
                    BackingScale::ONE,
                ));
                pane.quick_look = Box::new(RecordingQuickLookPresenter {
                    previews: Rc::clone(&previews),
                    dismissals: Rc::clone(&dismissals),
                });
                let menu = TerminalContextMenuState {
                    generation: clicked_screen.generation,
                    position: SurfacePosition::default(),
                    link: Some(link),
                    selection_present: false,
                    quick_look_eligible: true,
                };
                pane.context_menu = Some(menu.clone());
                pane.screen = ScreenSnapshot::from_test_parts_at(
                    clicked_screen.rows.clone(),
                    ScrollbarSnapshot::default(),
                    "advanced",
                    8,
                );
                pane.sync_terminal_input_focus(window, cx);
                pane.perform_context_menu_command(
                    menu,
                    TerminalContextMenuCommand::QuickLook,
                    window,
                    cx,
                );
            });
        });

        assert_eq!(previews.get(), 0);
        assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
        pane.update(cx, |pane, _| pane.close());
        assert_eq!(dismissals.get(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn osc52_prompt_command_enter_should_allow_without_moving_responder_focus(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let request = Osc52AuthorizationRequest {
            id: crate::terminal::Osc52AuthorizationId::new(10),
            access: Osc52Access::Write,
            target: Osc52Target::Standard,
            byte_len: 42,
        };
        records
            .last_event_sender()
            .unwrap()
            .send_blocking(SessionEvent::Osc52Authorization(request))
            .unwrap();
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolveOsc52Authorization(
                    request.id,
                    Osc52AuthorizationDecision::Allow,
                )
        }));
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
    }

    #[gpui::test]
    fn osc52_prompt_escape_should_choose_the_safe_denial_action(cx: &mut TestAppContext) {
        let (_pane, cx, records) = connected_terminal_pane(cx);
        let request = Osc52AuthorizationRequest {
            id: crate::terminal::Osc52AuthorizationId::new(12),
            access: Osc52Access::Read,
            target: Osc52Target::Standard,
            byte_len: 0,
        };
        records
            .last_event_sender()
            .unwrap()
            .send_blocking(SessionEvent::Osc52Authorization(request))
            .unwrap();
        cx.run_until_parked();

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolveOsc52Authorization(
                    request.id,
                    Osc52AuthorizationDecision::Deny,
                )
        }));
    }

    #[gpui::test]
    fn session_exit_dismisses_quick_look_and_the_context_menu(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let dismissals = Rc::new(Cell::new(0));

        pane.update(cx, |pane, cx| {
            pane.quick_look = Box::new(RecordingQuickLookPresenter {
                previews: Rc::new(Cell::new(0)),
                dismissals: Rc::clone(&dismissals),
            });
            pane.context_menu = Some(TerminalContextMenuState {
                generation: pane.screen.generation,
                position: SurfacePosition::default(),
                link: None,
                selection_present: false,
                quick_look_eligible: false,
            });
            pane.handle_event(
                SessionEvent::Exited(crate::terminal::SessionExit::Success),
                cx,
            );
        });

        assert_eq!(dismissals.get(), 1);
        assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
    }

    #[gpui::test]
    fn context_copy_stays_eligible_for_offscreen_selection_presence(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);

        let actions = pane.update(cx, |pane, _| {
            let mut screen =
                ScreenSnapshot::from_test_parts(Arc::from([]), ScrollbarSnapshot::default(), "");
            Arc::make_mut(&mut screen).selection_present = true;
            pane.screen = screen;
            pane.native_context_actions()
        });

        assert!(actions.copy);
    }

    #[test]
    fn maps_gpui_buttons_and_modifiers_to_terminal_input() {
        assert_eq!(pointer_button(MouseButton::Left), Some(PointerButton::Left));
        assert_eq!(
            pointer_button(MouseButton::Navigate(gpui::NavigationDirection::Back)),
            None
        );
        assert_eq!(
            input_modifiers(Modifiers {
                control: true,
                alt: true,
                shift: true,
                platform: true,
                function: true,
            }),
            InputModifiers {
                shift: true,
                alt: true,
                control: true,
                platform: true,
                ..InputModifiers::default()
            }
        );
    }

    #[gpui::test]
    fn terminal_scrollbar_should_request_exact_row_offsets(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let records = TestTerminalSessionRecords::default();
        let session = TestTerminalSessionFactory::new(records.clone())
            .start(
                TerminalGeometry::from_grid(
                    CellGridSize::new(80, 24),
                    LogicalCellSize::new(8.0, 16.0),
                    BackingScale::ONE,
                ),
                TerminalLaunchPlan::Local(LocalTerminalLaunchPlan::new(test_workspace_directory(
                    PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
                ))),
            )
            .expect("the test terminal session should start");
        let screen =
            ScreenSnapshot::from_test_parts_at(Arc::from([]), ScrollbarSnapshot::default(), "", 7);
        let generation = screen.generation;
        let scrollbar = pane.update(cx, |pane, _| {
            pane.session = Some(session.handle);
            pane.screen = screen;
            pane.scrollbar.clone()
        });

        scrollbar.update(cx, |_, cx| {
            cx.emit(OverlayScrollbarEvent::OffsetRequested(u64::MAX - 1));
        });
        cx.run_until_parked();

        assert_eq!(
            records.commands().last().map(|input| &input.command),
            Some(&RecordedSessionCommand::ScrollTo(u64::MAX - 1, generation,))
        );
    }

    #[gpui::test]
    fn terminal_pane_should_ignore_an_older_screen_presentation(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let events = records.last_event_sender().unwrap();
        events
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts_at(
                Arc::from([]),
                ScrollbarSnapshot::default(),
                "newest",
                2,
            )))
            .unwrap();
        cx.run_until_parked();
        events
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts_at(
                Arc::from([]),
                ScrollbarSnapshot::default(),
                "stale",
                1,
            )))
            .unwrap();
        cx.run_until_parked();

        let (generation, title) = pane.update(cx, |pane, _| {
            (pane.screen.generation, pane.title.to_string())
        });
        assert_eq!(
            generation,
            ScreenSnapshot::from_test_parts_at(Arc::from([]), ScrollbarSnapshot::default(), "", 2,)
                .generation
        );
        assert_eq!(title, "newest");
    }

    #[gpui::test]
    fn backing_scale_change_should_preserve_the_grid_and_resize_backing_pixels(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-scale-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.run_until_parked();
        let initial = records.starts()[0].geometry;

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.update_backing_scale(1.0, window, cx);
            });
        });
        cx.run_until_parked();
        let resized = records
            .commands()
            .into_iter()
            .find_map(|call| match call.command {
                RecordedSessionCommand::Resize(geometry) => Some(geometry),
                _ => None,
            })
            .expect("a backing-scale change should resize the Terminal Session");

        assert_eq!(
            (
                initial.grid(),
                resized.grid(),
                initial.backing_grid_size().width,
                resized.backing_grid_size().width,
            ),
            (
                initial.grid(),
                initial.grid(),
                resized.backing_grid_size().width * 2,
                resized.backing_grid_size().width,
            )
        );
    }

    #[gpui::test]
    fn terminal_pane_close_should_drop_its_session_once_when_repeated(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.run_until_parked();

        pane.update(cx, |pane, _| {
            pane.close();
            pane.close();
        });

        assert_eq!(records.dropped_session_ids(), vec![1]);
    }

    #[gpui::test]
    fn presentation_failure_retry_preserves_and_restores_the_current_presentation(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, _records) = connected_terminal_pane(cx);
        let generation = pane.read_with(cx, |pane, _| pane.screen.generation);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.update_backing_scale(f32::NAN, window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.screen.generation,
                pane.pane_state.last_valid_frame(),
                pane.pane_state
                    .failure()
                    .map(crate::terminal::TerminalFailure::class),
                pane.session.is_some(),
            )),
            (
                generation,
                Some(generation),
                Some(crate::terminal::FailureClass::Presentation),
                true,
            )
        );

        let retry = cx
            .debug_bounds("retry-terminal-recovery")
            .expect("recoverable presentation failure should expose Retry");
        cx.simulate_click(retry.center(), Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.pane_state.clone(),
                pane.status.clone(),
                pane.screen.generation,
                pane.session.is_some(),
            )),
            (PaneTerminalState::Running, None, generation, true)
        );
    }

    #[gpui::test]
    fn second_row_preflight_failure_submits_only_the_last_valid_generation(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let events = records.last_event_sender().unwrap();
        events
            .try_send(SessionEvent::Screen(text_screen(1, &["old", "frame"])))
            .unwrap();
        cx.run_until_parked();
        let submissions_before = pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len());

        pane.update(cx, |pane, _| {
            pane.paint_fault = Some(PaintPreflightFault::Row(1));
        });
        events
            .try_send(SessionEvent::Screen(text_screen(2, &["new", "frame"])))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.screen.generation,
                pane.last_valid_screen.generation,
                pane.pane_state.last_valid_frame(),
                pane.pane_state.failure().map(TerminalFailure::class),
            )),
            (
                crate::terminal::PresentationGeneration::test(2),
                crate::terminal::PresentationGeneration::test(1),
                Some(crate::terminal::PresentationGeneration::test(1)),
                Some(crate::terminal::FailureClass::Presentation),
            )
        );
        let submissions = pane.read_with(cx, |pane, _| {
            pane.scene_submission_attempts[submissions_before..].to_vec()
        });
        assert!(!submissions.contains(&crate::terminal::PresentationGeneration::test(2)));
        assert_eq!(
            submissions.last(),
            Some(&crate::terminal::PresentationGeneration::test(1))
        );
    }

    #[gpui::test]
    fn failed_candidate_is_unobserved_and_retry_observes_the_committed_generation_once(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let events = records.last_event_sender().unwrap();
        events
            .try_send(SessionEvent::Screen(text_screen(1, &["old"])))
            .unwrap();
        cx.run_until_parked();
        let observation = crate::terminal::RuntimeObservation::new();
        pane.update(cx, |pane, _| {
            pane.acceptance_observation_claimed = true;
            pane.runtime_observation = Some(observation.clone());
            pane.paint_fault = Some(PaintPreflightFault::Row(0));
        });
        let before = observation.sample();

        events
            .try_send(SessionEvent::Screen(text_screen(2, &["new"])))
            .unwrap();
        cx.run_until_parked();

        let failed = observation.sample();
        assert_eq!(failed.next_frame_count, before.next_frame_count);
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.last_valid_screen.generation),
            crate::terminal::PresentationGeneration::test(1)
        );

        let retry = cx
            .debug_bounds("retry-terminal-recovery")
            .expect("failed candidate should expose Retry");
        cx.simulate_click(retry.center(), Modifiers::none());
        cx.run_until_parked();

        let retried = observation.sample();
        assert_eq!(retried.next_frame_count, before.next_frame_count + 1);
        assert_eq!(retried.next_frame_generation, 2);
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.last_valid_screen.generation),
            crate::terminal::PresentationGeneration::test(2)
        );
    }

    #[gpui::test]
    fn ordinary_pane_has_no_failure_action_monitor_or_armed_seam(cx: &mut TestAppContext) {
        let (pane, cx, _) = connected_terminal_pane(cx);
        assert!(pane.read_with(cx, |pane, _| {
            pane.failure_actions.is_none()
                && pane._failure_action_task.is_none()
                && pane.failure_action_request.is_none()
                && pane.failure_action_recovery_frame.is_none()
                && pane.paint_fault.is_none()
                && pane.failure_action_resource_rollback == GraphicsRollbackProof::default()
        }));
    }

    #[gpui::test]
    fn failure_action_monitor_holds_one_request_until_matching_presented_frame(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, _) = connected_terminal_pane(cx);
        let (controller, requests, events) = FailureActionController::test_channel();
        let first = FailureActionRequest {
            id: "a".repeat(64),
            sequence: 0,
            case: FailureActionCase::PresentationGlyph,
        };
        let second = FailureActionRequest {
            id: "b".repeat(64),
            sequence: 1,
            case: FailureActionCase::NormalExitControl,
        };
        pane.update(cx, |pane, cx| {
            pane.failure_actions = Some(controller);
            pane.acceptance_observation_claimed = true;
            pane.start_failure_action_monitor(cx);
        });
        requests.try_send(first.clone()).unwrap();
        cx.run_until_parked();
        assert_eq!(events.try_recv().unwrap().phase, FailureActionPhase::Armed);
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.failure_action_request.clone()),
            Some(first.clone())
        );

        pane.update(cx, |pane, cx| {
            pane.present_failure_at(
                TerminalFailure::presentation("paint-terminal-presentation"),
                true,
                Some(RecoveryAction::Presentation),
                crate::terminal::PresentationGeneration::test(2),
            );
            pane.emit_injected_failure_if_matching();
            pane.failure_action_recovery_frame =
                Some((0, crate::terminal::PresentationGeneration::test(2)));
            pane.render_lifecycle
                .observe_snapshot(crate::terminal::PresentationGeneration::test(2));
            pane.render_lifecycle
                .mark_presented(crate::terminal::PresentationGeneration::test(2));
            pane.pane_state = PaneTerminalState::Running;
            pane.pending_recovery = None;
            pane.observe_presented_frame(crate::terminal::PresentationGeneration::test(1), 24, 80);
            assert!(pane.failure_action_request.is_some());
            pane.observe_presented_frame(crate::terminal::PresentationGeneration::test(2), 24, 80);
            cx.notify();
        });
        let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            emitted.iter().map(|event| event.phase).collect::<Vec<_>>(),
            vec![FailureActionPhase::Injected, FailureActionPhase::Completed]
        );
        requests.try_send(second.clone()).unwrap();
        cx.run_until_parked();
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.failure_action_request.clone()),
            Some(second)
        );
        assert_eq!(events.try_recv().unwrap().phase, FailureActionPhase::Armed);
    }

    #[gpui::test]
    fn fatal_failure_action_close_is_detached_once_and_stops_monitor(cx: &mut TestAppContext) {
        let (pane, cx, _) = connected_terminal_pane(cx);
        let (controller, requests, events) = FailureActionController::test_channel();
        pane.update(cx, |pane, cx| {
            pane.failure_actions = Some(controller);
            pane.start_failure_action_monitor(cx);
        });
        requests
            .try_send(FailureActionRequest {
                id: "c".repeat(64),
                sequence: 0,
                case: FailureActionCase::PtyFatal,
            })
            .unwrap();
        cx.run_until_parked();
        pane.update(cx, |pane, cx| {
            pane.handle_event(
                SessionEvent::Failed(crate::terminal::SessionFailure::PtyRead {
                    read_error: "acceptance-injected".to_owned(),
                    exit_status: "acceptance-injected".to_owned(),
                }),
                cx,
            );
            pane.close();
            pane.close();
        });
        cx.run_until_parked();
        let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            emitted.iter().map(|event| event.phase).collect::<Vec<_>>(),
            vec![
                FailureActionPhase::Armed,
                FailureActionPhase::Injected,
                FailureActionPhase::Completed,
            ]
        );
        assert!(!emitted.last().unwrap().session_attached);
        assert!(!emitted.last().unwrap().terminal_input_usable);
        assert!(requests.is_closed());
    }

    #[gpui::test]
    fn candidate_and_fallback_use_isolated_render_caches(cx: &mut TestAppContext) {
        let (pane, cx, _records) = connected_terminal_pane(cx);

        let (candidate, fallback) = pane.read_with(cx, |pane, _| {
            (
                pane.render_cache.entity_id(),
                pane.fallback_render_cache.entity_id(),
            )
        });

        assert_ne!(candidate, fallback);
    }

    #[gpui::test]
    fn second_glyph_preflight_failure_submits_only_the_last_valid_generation(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let events = records.last_event_sender().unwrap();
        events
            .try_send(SessionEvent::Screen(text_screen(1, &["old"])))
            .unwrap();
        cx.run_until_parked();
        let submissions_before = pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len());

        pane.update(cx, |pane, _| {
            pane.paint_fault = Some(PaintPreflightFault::Glyph(1));
        });
        events
            .try_send(SessionEvent::Screen(text_screen(2, &["new"])))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.screen.generation,
                pane.last_valid_screen.generation,
                pane.pane_state.failure().map(TerminalFailure::class),
            )),
            (
                crate::terminal::PresentationGeneration::test(2),
                crate::terminal::PresentationGeneration::test(1),
                Some(crate::terminal::FailureClass::Presentation),
            )
        );
        let submissions = pane.read_with(cx, |pane, _| {
            pane.scene_submission_attempts[submissions_before..].to_vec()
        });
        assert!(!submissions.contains(&crate::terminal::PresentationGeneration::test(2)));
    }

    #[gpui::test]
    fn second_image_preflight_failure_rolls_back_the_unpresented_generation(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let events = records.last_event_sender().unwrap();
        events
            .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
            .unwrap();
        cx.run_until_parked();
        let submissions_before = pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len());

        pane.update(cx, |pane, _| {
            pane.paint_fault = Some(PaintPreflightFault::Image(1));
        });
        events
            .try_send(SessionEvent::Screen(graphics_screen_with_images(
                2,
                &[2, 3],
            )))
            .unwrap();
        cx.run_until_parked();

        let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
        assert_eq!(
            (
                pane.read_with(cx, |pane, _| pane.last_valid_screen.generation),
                pane.read_with(cx, |pane, _| {
                    pane.pane_state.failure().map(TerminalFailure::class)
                }),
                cache.read_with(cx, |cache, _| cache.cached_image_keys()),
                cache.read_with(cx, |cache, _| cache.staged_image_keys()),
            ),
            (
                crate::terminal::PresentationGeneration::test(1),
                Some(crate::terminal::FailureClass::Resource),
                vec![crate::terminal::ImageKey {
                    image_id: 1,
                    generation: 1,
                }],
                Vec::new(),
            )
        );
        let submissions = pane.read_with(cx, |pane, _| {
            pane.scene_submission_attempts[submissions_before..].to_vec()
        });
        assert!(!submissions.contains(&crate::terminal::PresentationGeneration::test(2)));
    }

    #[gpui::test]
    fn graphics_post_mutation_failures_remain_quota_bounded_for_changing_keys(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        records
            .last_event_sender()
            .unwrap()
            .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
            .unwrap();
        cx.run_until_parked();
        let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
        let retained = cache.read_with(cx, |cache, _| cache.retained_bytes());

        for generation in 2..18 {
            let screen = graphics_screen(generation, generation as u32);
            let result = cx.update(|window, cx| {
                cache.update(cx, |cache, cx| {
                    cache.fail_after_staging();
                    cache.sync(screen.active_screen, &screen.graphics, window, cx)
                })
            });
            assert!(result.is_err());
            let rollback = cache.update(cx, |cache, _| cache.take_injected_rollback());
            assert_eq!(
                rollback,
                Some(GraphicsRollbackProof {
                    staged_count: 1,
                    staged_bytes: 4,
                    rolled_back_count: 1,
                    rolled_back_bytes: 4,
                })
            );
            assert_eq!(
                cache.read_with(cx, |cache, _| (
                    cache.cached_image_keys(),
                    cache.staged_image_keys(),
                    cache.retained_bytes(),
                )),
                (
                    vec![crate::terminal::ImageKey {
                        image_id: 1,
                        generation: 1,
                    }],
                    Vec::new(),
                    retained,
                )
            );
        }
    }

    #[gpui::test]
    fn after_staging_failure_action_reports_actual_bounded_rollback(cx: &mut TestAppContext) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let session_events = records.last_event_sender().unwrap();
        session_events
            .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
            .unwrap();
        cx.run_until_parked();
        let (controller, requests, action_events) = FailureActionController::test_channel();
        pane.update(cx, |pane, cx| {
            pane.failure_actions = Some(controller);
            pane.start_failure_action_monitor(cx);
        });
        requests
            .try_send(FailureActionRequest {
                id: "f".repeat(64),
                sequence: 0,
                case: FailureActionCase::RendererResourceAfterStaging,
            })
            .unwrap();
        cx.run_until_parked();
        let armed = action_events.try_recv().unwrap();
        assert_eq!(armed.phase, FailureActionPhase::Armed);
        assert_eq!(armed.resource_staged_count, 0);
        assert_eq!(armed.resource_rolled_back_count, 0);

        session_events
            .try_send(SessionEvent::Screen(graphics_screen(2, 2)))
            .unwrap();
        cx.run_until_parked();
        let injected = action_events.try_recv().unwrap();
        assert_eq!(injected.phase, FailureActionPhase::Injected);
        assert!(injected.resource_staged_count > 0);
        assert!(injected.resource_staged_bytes > 0);
        assert_eq!(
            injected.resource_rolled_back_count,
            injected.resource_staged_count
        );
        assert_eq!(
            injected.resource_rolled_back_bytes,
            injected.resource_staged_bytes
        );
    }

    #[gpui::test]
    fn stale_graphics_attempt_cannot_roll_back_a_newer_same_generation_stage(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        records
            .last_event_sender()
            .unwrap()
            .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
            .unwrap();
        cx.run_until_parked();
        let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
        let screen_a = graphics_screen(2, 2);
        let screen_b = graphics_screen(2, 3);
        let attempt_a = cx.update(|window, cx| {
            cache
                .update(cx, |cache, cx| {
                    cache.sync(screen_a.active_screen, &screen_a.graphics, window, cx)
                })
                .unwrap()
                .token
        });
        let attempt_b = cx.update(|window, cx| {
            cache
                .update(cx, |cache, cx| {
                    cache.sync(screen_b.active_screen, &screen_b.graphics, window, cx)
                })
                .unwrap()
                .token
        });

        cache.update(cx, |cache, cx| {
            assert!(!cache.rollback(attempt_a, None, cx));
            assert_eq!(
                cache.staged_image_keys(),
                vec![crate::terminal::ImageKey {
                    image_id: 3,
                    generation: 2,
                }]
            );
            assert!(cache.mark_presented(attempt_b, cx));
        });
        assert_eq!(
            cache.read_with(cx, |cache, _| cache.cached_image_keys()),
            vec![crate::terminal::ImageKey {
                image_id: 3,
                generation: 2,
            }]
        );
    }

    #[gpui::test]
    fn stale_recoverable_and_export_completions_cannot_mask_a_fatal_failure(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, _records) = connected_terminal_pane(cx);
        let (recovery, export) = pane.update(cx, |pane, _| {
            pane.present_failure(
                TerminalFailure::platform("recoverable-operation"),
                true,
                Some(RecoveryAction::CopySelection),
            );
            let recovery = pane.pending_recovery.unwrap();
            let export = pane.begin_operation(pane.screen.generation, None);
            pane.latest_export_operation = Some(export.id);
            (recovery, export)
        });
        pane.update(cx, |pane, cx| {
            pane.handle_event(
                SessionEvent::Failed(SessionFailure::PtyRead {
                    read_error: "unavailable".to_owned(),
                    exit_status: "exit code 7".to_owned(),
                }),
                cx,
            );
            assert!(!pane.clear_recovery(recovery));
            pane.finish_export(export, Ok(()), PathBuf::from("stale"), cx);
            pane.finish_export(
                export,
                Err(std::io::Error::other("stale failure")),
                PathBuf::from("stale"),
                cx,
            );
            pane.handle_event(
                SessionEvent::Exited(crate::terminal::SessionExit::Success),
                cx,
            );
        });

        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.pane_state.failure().map(TerminalFailure::class),
                pane.pending_recovery,
                pane.authoritative_status(),
            )),
            (
                Some(crate::terminal::FailureClass::Pty),
                None,
                Some(
                    "PTY failed during read-shell-output. Close this Pane and restart the terminal command."
                        .to_owned(),
                ),
            )
        );
    }

    #[gpui::test]
    fn newer_same_action_and_export_tokens_reject_stale_completions(cx: &mut TestAppContext) {
        let (pane, cx, _records) = connected_terminal_pane(cx);
        pane.update(cx, |pane, cx| {
            pane.present_failure_at(
                TerminalFailure::resource("first-renderer-attempt"),
                true,
                Some(RecoveryAction::RendererResources),
                crate::terminal::PresentationGeneration::test(1),
            );
            let first_recovery = pane.pending_recovery.unwrap();
            pane.present_failure_at(
                TerminalFailure::resource("second-renderer-attempt"),
                true,
                Some(RecoveryAction::RendererResources),
                crate::terminal::PresentationGeneration::test(2),
            );
            let second_recovery = pane.pending_recovery.unwrap();
            assert!(!pane.clear_recovery(first_recovery));
            assert_eq!(pane.pending_recovery, Some(second_recovery));

            let first_export = pane.begin_operation(pane.screen.generation, None);
            let second_export = pane.begin_operation(pane.screen.generation, None);
            pane.latest_export_operation = Some(second_export.id);
            pane.finish_export(
                first_export,
                Err(std::io::Error::other("stale export")),
                PathBuf::from("stale"),
                cx,
            );
            assert_eq!(pane.pending_recovery, Some(second_recovery));
            assert_eq!(
                pane.pane_state.failure().map(TerminalFailure::operation),
                Some("second-renderer-attempt")
            );
        });
    }

    #[gpui::test]
    fn normal_exit_remains_distinct_from_stale_failures(cx: &mut TestAppContext) {
        let (pane, cx, _records) = connected_terminal_pane(cx);
        pane.update(cx, |pane, cx| {
            pane.handle_event(
                SessionEvent::Exited(crate::terminal::SessionExit::Success),
                cx,
            );
            pane.present_failure(
                TerminalFailure::resource("stale-resource-operation"),
                true,
                Some(RecoveryAction::RendererResources),
            );
            pane.handle_event(
                SessionEvent::Failed(SessionFailure::Runtime("stale fatal".to_owned())),
                cx,
            );
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.pane_state.clone(),
                pane.pending_recovery,
                pane.authoritative_status(),
            )),
            (
                PaneTerminalState::exited(crate::terminal::SessionExit::Success),
                None,
                Some("Shell exited successfully".to_owned()),
            )
        );
    }

    #[gpui::test]
    fn renderer_resource_retry_retains_the_previous_gpu_cache_until_success(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);
        let events = records
            .last_event_sender()
            .expect("the connected Pane should own a Terminal Session");
        events
            .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
            .unwrap();
        cx.run_until_parked();

        pane.update(cx, |pane, cx| {
            pane.graphics_cache
                .update(cx, |cache, _| cache.fail_next_sync());
        });
        events
            .try_send(SessionEvent::Screen(graphics_screen(2, 2)))
            .unwrap();
        cx.run_until_parked();

        let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
        assert_eq!(
            (
                pane.read_with(cx, |pane, _| pane.screen.generation),
                pane.read_with(cx, |pane, _| pane.pane_state.last_valid_frame()),
                pane.read_with(cx, |pane, _| {
                    pane.pane_state
                        .failure()
                        .map(crate::terminal::TerminalFailure::class)
                }),
                cache.read_with(cx, |cache, _| cache.cached_image_keys()),
            ),
            (
                crate::terminal::PresentationGeneration::test(2),
                Some(crate::terminal::PresentationGeneration::test(1)),
                Some(crate::terminal::FailureClass::Resource),
                vec![crate::terminal::ImageKey {
                    image_id: 1,
                    generation: 1,
                }],
            )
        );

        let retry = cx
            .debug_bounds("retry-terminal-recovery")
            .expect("recoverable renderer failure should expose Retry");
        cx.simulate_click(retry.center(), Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            (
                pane.read_with(cx, |pane, _| pane.pane_state.clone()),
                pane.read_with(cx, |pane, _| pane.status.clone()),
                cache.read_with(cx, |cache, _| cache.cached_image_keys()),
            ),
            (
                PaneTerminalState::Running,
                None,
                vec![crate::terminal::ImageKey {
                    image_id: 2,
                    generation: 2,
                }],
            )
        );
    }

    #[gpui::test]
    fn native_platform_retry_keeps_the_session_usable_and_clears_transient_failure(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = terminal_pane_with_selection_copy(
            cx,
            SelectionCopy {
                plain_text: "recovered selection".to_owned(),
                html: None,
            },
        );
        pane.update(cx, |pane, _| {
            pane.selection_pasteboard.fail_next_write();
        });

        cx.simulate_keystrokes("cmd-c");
        cx.run_until_parked();

        assert_eq!(
            pane.read_with(cx, |pane, _| (
                pane.pane_state
                    .failure()
                    .map(crate::terminal::TerminalFailure::class),
                pane.session.is_some(),
            )),
            (Some(crate::terminal::FailureClass::Platform), true)
        );
        let retry = cx
            .debug_bounds("retry-terminal-recovery")
            .expect("recoverable native failure should expose Retry");
        cx.simulate_click(retry.center(), Modifiers::none());
        cx.run_until_parked();

        let copy_requests = records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedSessionCommand::RequestSelectionCopy))
            .count();
        assert_eq!(
            (
                pane.read_with(cx, |pane, _| pane.pane_state.clone()),
                pane.read_with(cx, |pane, _| pane.status.clone()),
                pane.read_with(cx, |pane, _| pane.session.is_some()),
                cx.read_from_clipboard().and_then(|item| item.text()),
                copy_requests,
            ),
            (
                PaneTerminalState::Running,
                None,
                true,
                Some("recovered selection".to_owned()),
                2,
            )
        );
    }

    #[gpui::test]
    fn pasteboard_failure_action_completes_only_after_a_real_retry_write(cx: &mut TestAppContext) {
        let (pane, cx, _) = terminal_pane_with_selection_copy(
            cx,
            SelectionCopy {
                plain_text: "acceptance selection".to_owned(),
                html: None,
            },
        );
        let (controller, requests, events) = FailureActionController::test_channel();
        pane.update(cx, |pane, cx| {
            pane.failure_actions = Some(controller);
            pane.start_failure_action_monitor(cx);
        });
        requests
            .try_send(FailureActionRequest {
                id: "d".repeat(64),
                sequence: 0,
                case: FailureActionCase::PasteboardWrite,
            })
            .unwrap();
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-c");
        cx.run_until_parked();
        let before_retry = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            before_retry
                .iter()
                .map(|event| event.phase)
                .collect::<Vec<_>>(),
            vec![FailureActionPhase::Armed, FailureActionPhase::Injected]
        );
        assert!(pane.read_with(cx, |pane, _| pane.failure_action_request.is_some()));

        let retry = cx
            .debug_bounds("retry-terminal-recovery")
            .expect("pasteboard failure action should expose Retry");
        cx.simulate_click(retry.center(), Modifiers::none());
        cx.run_until_parked();
        let retried = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            retried.iter().map(|event| event.phase).collect::<Vec<_>>(),
            vec![
                FailureActionPhase::RetryRequested,
                FailureActionPhase::Completed,
            ]
        );
        assert!(retried.last().unwrap().terminal_input_usable);
        assert!(retried.last().unwrap().session_attached);
        assert!(pane.read_with(cx, |pane, _| pane.failure_action_request.is_none()));
    }

    #[gpui::test]
    fn pasteboard_failure_action_does_not_complete_without_a_selection(cx: &mut TestAppContext) {
        let (pane, cx, _) = connected_terminal_pane(cx);
        let (controller, requests, events) = FailureActionController::test_channel();
        pane.update(cx, |pane, cx| {
            pane.failure_actions = Some(controller);
            pane.start_failure_action_monitor(cx);
        });
        requests
            .try_send(FailureActionRequest {
                id: "e".repeat(64),
                sequence: 0,
                case: FailureActionCase::PasteboardWrite,
            })
            .unwrap();
        cx.run_until_parked();
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.present_failure(
                    TerminalFailure::platform("write-selection-pasteboard"),
                    true,
                    Some(RecoveryAction::CopySelection),
                );
                pane.emit_injected_failure_if_matching();
                pane.retry_recovery(window, cx);
            });
        });
        let phases = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event.phase)
            .collect::<Vec<_>>();
        assert_eq!(
            phases,
            vec![
                FailureActionPhase::Armed,
                FailureActionPhase::Injected,
                FailureActionPhase::RetryRequested,
            ]
        );
        assert!(pane.read_with(cx, |pane, _| pane.failure_action_request.is_some()));
    }

    #[gpui::test]
    fn terminal_failure_should_keep_the_pane_visible_with_a_failure_status(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-terminal-pane-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        let exits = Rc::new(Cell::new(0));
        let exits_for_subscription = Rc::clone(&exits);
        pane.update(cx, |_, cx| {
            cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
                if matches!(event, TerminalPaneEvent::Exited) {
                    exits_for_subscription.update(|exits| exits + 1);
                }
            })
            .detach();
        });
        cx.run_until_parked();
        let sender = records
            .event_sender(1)
            .expect("rendering the Pane must start its Terminal Session");

        sender
            .try_send(SessionEvent::Failed(SessionFailure::PtyRead {
                read_error: "read unavailable".to_owned(),
                exit_status: "exit code 7".to_owned(),
            }))
            .unwrap();
        cx.run_until_parked();

        let state = pane.read_with(cx, |pane, _| {
            (
                pane.authoritative_status(),
                pane.session.is_some(),
                pane.pane_state
                    .failure()
                    .map(crate::terminal::TerminalFailure::class),
                pane.diagnostics.record_count(),
            )
        });
        assert_eq!(
            state,
            (
                Some(
                    "PTY failed during read-shell-output. Close this Pane and restart the terminal command."
                        .to_owned()
                ),
                true,
                Some(crate::terminal::FailureClass::Pty),
                1,
            )
        );
        assert_eq!(
            (exits.get(), records.dropped_session_ids()),
            (0, Vec::new())
        );
        assert!(cx.debug_bounds("terminal-status").is_some());
        assert!(cx.debug_bounds("retry-terminal-recovery").is_none());
        let export = cx
            .debug_bounds("export-terminal-diagnostics")
            .expect("typed failure should expose explicit diagnostic export");
        cx.simulate_click(export.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(cx.did_prompt_for_new_path());
    }
}
