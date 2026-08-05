use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use gpui::ClipboardItem;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle, IntoElement,
    KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollWheelEvent, SharedString, Task, TextRun, UTF16Selection,
    Window, div, font, point, px, rgba, size,
};

use super::overlay_scrollbar::{OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics};
use super::terminal_element::{
    TerminalGridCache, TerminalGridConfiguration, TerminalGridElement, terminal_grid_content_bounds,
};
use super::terminal_focus::{TerminalFocusCoordinator, TerminalFocusFacts, TerminalProductFocus};
use super::terminal_ime::{PreeditLayout, PreeditPosition, TerminalIme, layout_preedit};
use super::{
    AllowOsc52Clipboard, CancelUnsafePaste, ConfirmUnsafePaste, CopySelection,
    DecreaseTerminalFontSize, DenyOsc52Clipboard, IncreaseTerminalFontSize, PasteClipboard,
    ResetTerminalFontSize, TERMINAL_KEY_CONTEXT,
};
use crate::platform::macos_keyboard::{
    KeyTranslation, MacosKeyboardBridge, NativeKeyEvent, NativeKeyEventKind, UnhandledKeyEvent,
};
use crate::terminal::geometry::{
    BackingScale, CellGridSize, LogicalCellSize, LogicalPosition, LogicalSize, TerminalGeometry,
};
use crate::terminal::{
    InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, Osc52Access,
    Osc52AuthorizationDecision, Osc52AuthorizationRequest, Osc52Target, PasteConfirmation,
    PasteDecision, PasteRequestOutcome, PasteResolution, PhysicalKey, PointerButton, PointerInput,
    PointerPhase, ScreenSnapshot, SelectionCopy, SessionEvent, ShiftSelectionPolicy,
    SurfacePosition, TerminalSessionHandle, WheelInput, WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};

const DEFAULT_FONT_SIZE: f32 = 14.0;
const DEFAULT_LINE_HEIGHT: f32 = 20.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 32.0;
const FONT_SIZE_STEP: f32 = 1.0;
const HORIZONTAL_PADDING: f32 = 4.0;
const VERTICAL_PADDING: f32 = 8.0;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
const MAX_PANE_TITLE_CHARACTERS: usize = 256;
const TEXT_BLINK_INTERVAL: Duration = Duration::from_millis(600);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalPaneEvent {
    FocusRequested,
    TitleChanged(SharedString),
    Exited,
}

pub(crate) struct TerminalPane {
    session_factory: WorkspaceTerminalSessionFactory,
    session: Option<Box<dyn TerminalSessionHandle>>,
    session_start_attempted: bool,
    screen: Arc<ScreenSnapshot>,
    status: Option<String>,
    fallback_title: SharedString,
    title: SharedString,
    focus_handle: FocusHandle,
    product_focus: TerminalProductFocus,
    terminal_input_focus: bool,
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
    wheel_remainder: f32,
    scrollbar: Entity<OverlayScrollbar<u64>>,
    render_cache: TerminalGridCache,
    keyboard_bridge: MacosKeyboardBridge,
    ime: TerminalIme,
    ime_suppressed_keys: Vec<PhysicalKey>,
    pending_paste: Option<PasteConfirmation>,
    pending_osc52: Option<Osc52AuthorizationRequest>,
    text_blink_visible: bool,
    text_blink_generation: u64,
    _text_blink_task: Option<Task<()>>,
    _event_task: Option<Task<()>>,
}

impl TerminalPane {
    pub(crate) fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family, DEFAULT_FONT_SIZE);
        let backing_scale = BackingScale::new(window.scale_factor()).unwrap_or(BackingScale::ONE);
        let fallback_title: SharedString =
            normalized_pane_title("", &session_factory.fallback_title()).into();
        let scrollbar = cx.new(|_| OverlayScrollbar::<u64>::new("terminal-scrollbar"));
        cx.subscribe_in(
            &scrollbar,
            window,
            |pane, _, event: &OverlayScrollbarEvent<u64>, window, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => {
                    pane.focus_handle.focus(window);
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
        cx.observe_window_activation(window, |_pane, _window, cx| cx.notify())
            .detach();

        Self {
            session_factory,
            session: None,
            session_start_attempted: false,
            screen: ScreenSnapshot::empty(),
            status: None,
            title: fallback_title.clone(),
            fallback_title,
            focus_handle,
            product_focus: TerminalProductFocus::default(),
            terminal_input_focus: false,
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
            wheel_remainder: 0.0,
            scrollbar,
            render_cache: TerminalGridCache::new(),
            keyboard_bridge: MacosKeyboardBridge::new(OptionAsAltPolicy::default()),
            ime: TerminalIme::default(),
            ime_suppressed_keys: Vec::new(),
            pending_paste: None,
            pending_osc52: None,
            text_blink_visible: true,
            text_blink_generation: 0,
            _text_blink_task: None,
            _event_task: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    pub(crate) fn set_product_focus(&mut self, product_focus: TerminalProductFocus) {
        if (!product_focus.active_workspace
            || !product_focus.active_window
            || !product_focus.focused_pane
            || product_focus.blocker.is_some())
            && let Some(confirmation) = self.pending_paste.take()
            && let Some(session) = &self.session
        {
            let _ = session.resolve_paste(confirmation.id, PasteDecision::Cancel);
        }
        if (!product_focus.active_workspace
            || !product_focus.active_window
            || !product_focus.focused_pane
            || product_focus.blocker.is_some())
            && let Some(request) = self.pending_osc52.take()
            && let Some(session) = &self.session
        {
            session.resolve_osc52_authorization(request.id, Osc52AuthorizationDecision::Deny);
        }
        self.product_focus = product_focus;
    }

    pub(crate) fn terminal_input_focused(&self, window: &Window) -> bool {
        let window_active = window.is_window_active();
        TerminalFocusCoordinator::is_focused(TerminalFocusFacts {
            active_workspace: self.product_focus.active_workspace,
            active_window: self.product_focus.active_window,
            focused_pane: self.product_focus.focused_pane,
            responder: self.focus_handle.is_focused(window),
            operating_system_window_key: window_active,
            application_active: window_active,
            blocker: self.product_focus.blocker,
        })
    }

    fn sync_terminal_input_focus(&mut self, window: &Window) -> bool {
        let focused = self.terminal_input_focused(window);
        if self.terminal_input_focus != focused {
            self.terminal_input_focus = focused;
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
                self.ime_suppressed_keys.clear();
            }
            if let Some(session) = &self.session {
                session.focus(focused);
            }
        }
        focused
    }

    fn preedit_layout(&self) -> Option<PreeditLayout> {
        let text = self.ime.marked_text()?;
        let position = self.screen.cursor.position?;
        let columns = self.screen.rows.first()?.len();
        Some(layout_preedit(
            text,
            usize::from(position.row),
            usize::from(position.column),
            columns,
            self.ime.selected_range().end,
        ))
    }

    pub(crate) fn title(&self) -> SharedString {
        self.title.clone()
    }

    #[cfg(test)]
    pub(crate) fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    #[cfg(test)]
    pub(crate) const fn font_size(&self) -> f32 {
        self.font_size
    }

    pub(crate) fn close(&mut self) {
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
        self.text_blink_generation = self.text_blink_generation.wrapping_add(1);
        self._text_blink_task.take();
        self._event_task.take();
        self.session.take();
    }

    fn sync_text_blink(&mut self, surface_active: bool, cx: &mut Context<Self>) {
        let demanded = surface_active && self.screen.text_blinking;
        if demanded == self._text_blink_task.is_some() {
            return;
        }

        self.text_blink_generation = self.text_blink_generation.wrapping_add(1);
        self._text_blink_task.take();
        self.text_blink_visible = true;
        if !demanded {
            return;
        }

        let generation = self.text_blink_generation;
        self._text_blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TEXT_BLINK_INTERVAL).await;
                let Ok(continue_blinking) = this.update(cx, |this, cx| {
                    if this.text_blink_generation != generation {
                        return false;
                    }
                    this.text_blink_visible = !this.text_blink_visible;
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

        if let Some(session) = &self.session {
            session.resize(geometry);
            return;
        }

        if self.session_start_attempted {
            return;
        }
        self.session_start_attempted = true;

        match self.session_factory.start(geometry) {
            Ok(started) => {
                started.handle.focus(self.terminal_input_focus);
                self.session = Some(started.handle);
                let receiver = started.events;
                self._event_task = Some(cx.spawn(async move |this, cx| {
                    while let Ok(event) = receiver.recv().await {
                        let mut events = vec![event];
                        while let Ok(event) = receiver.try_recv() {
                            events.push(event);
                        }
                        if this
                            .update(cx, |this, cx| {
                                for event in events {
                                    this.handle_event(event, cx);
                                }
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
                eprintln!("failed to start terminal session: {error:#}");
                self.status = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn handle_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Screen(screen) => {
                if screen.generation < self.screen.generation {
                    return;
                }
                let title = normalized_pane_title(&screen.title, &self.fallback_title);
                if self.title.as_ref() != title {
                    self.title = title.into();
                    cx.emit(TerminalPaneEvent::TitleChanged(self.title.clone()));
                }
                self.screen = screen;
                self.sync_scrollbar(cx);
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
                eprintln!("{status}");
                self.status = Some(status.to_string());
                cx.emit(TerminalPaneEvent::Exited);
            }
            SessionEvent::Failed(failure) => {
                let status = failure.to_string();
                eprintln!("{status}");
                self.status = Some(status);
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.terminal_input_focused(window) {
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
        if self.send_key_translation(input) {
            cx.stop_propagation();
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.terminal_input_focused(window) {
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
        if self.send_key_translation(input) {
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
        if !self.terminal_input_focused(window) {
            return;
        }
        let Some(event) = NativeKeyEvent::current_modifier() else {
            return;
        };
        let translation = self.keyboard_bridge.modifier_transition(event);
        self.send_key_translation(translation);
    }

    fn send_key_translation(&mut self, translation: KeyTranslation) -> bool {
        match translation {
            KeyTranslation::Encoded(input) => {
                if let Some(session) = &self.session {
                    session.key(input);
                }
                true
            }
            KeyTranslation::TextInput(text) => {
                if let Some(session) = &self.session {
                    session.key(KeyInput::text_input(text));
                }
                true
            }
            KeyTranslation::Unhandled(_) => false,
        }
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
        cx.notify();
    }

    fn update_backing_scale(&mut self, factor: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backing_scale) = BackingScale::new(factor) else {
            eprintln!("ignored invalid terminal backing scale: {factor}");
            return;
        };
        if self.backing_scale == backing_scale {
            return;
        }

        self.backing_scale = backing_scale;
        self.cell_width = measure_cell_width(window, &self.font_family, self.font_size);
        self.last_geometry = None;
        self.render_cache.invalidate_scale_dependent();
        self.sync_scrollbar(cx);
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pointer_modifiers = input_modifiers(event.modifiers);
        self.focus_handle.focus(window);
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
        self.pointer_modifiers = input_modifiers(event.modifiers);
        let dragging = self.pressed_button.is_some();
        let Some(position) = self.surface_position(event.position, dragging) else {
            return;
        };
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
        self.pointer_modifiers = input_modifiers(event.modifiers);
        let Some(button) = pointer_button(event.button) else {
            return;
        };
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

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.surface_position(event.position, false) else {
            return;
        };
        self.reveal_scrollbar(cx);
        let lines = f32::from(event.delta.pixel_delta(px(self.line_height)).y) / self.line_height;
        let steps = accumulate_wheel_steps(&mut self.wheel_remainder, lines);

        if steps != 0
            && let Some(session) = &self.session
        {
            session.wheel(WheelInput {
                generation: self.screen.generation,
                steps,
                position,
                modifiers: input_modifiers(event.modifiers),
                shift_selection: self.shift_selection,
            });
        }
        cx.stop_propagation();
    }

    fn copy_selection(&mut self, _: &CopySelection, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let receiver = session.request_selection_copy();
        cx.spawn(async move |this, cx| match receiver.recv().await {
            Ok(Ok(Some(copy))) if !copy.plain_text.is_empty() => {
                let _ = this.update(cx, |this, cx| {
                    if let Err(error) = write_selection_copy(copy, cx) {
                        eprintln!("failed to write terminal selection to pasteboard: {error}");
                        this.status = Some(error);
                        cx.notify();
                    }
                });
            }
            Ok(Err(error)) => {
                let _ = this.update(cx, |this, cx| {
                    eprintln!("failed to copy terminal selection: {error}");
                    this.status = Some(error);
                    cx.notify();
                });
            }
            Err(error) => {
                let _ = this.update(cx, |this, cx| {
                    let message = format!("terminal selection reply was lost: {error}");
                    eprintln!("{message}");
                    this.status = Some(message);
                    cx.notify();
                });
            }
            Ok(Ok(None | Some(_))) => {}
        })
        .detach();
    }

    fn paste_clipboard(
        &mut self,
        _: &PasteClipboard,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let receiver = session.request_paste(text);
        cx.spawn(async move |this, cx| match receiver.recv().await {
            Ok(Ok(PasteRequestOutcome::Written)) => {}
            Ok(Ok(PasteRequestOutcome::ConfirmationRequired(confirmation))) => {
                let _ = this.update(cx, |this, cx| {
                    this.pending_paste = Some(confirmation);
                    cx.notify();
                });
            }
            Ok(Ok(PasteRequestOutcome::Rejected(rejection))) => {
                let _ = this.update(cx, |this, cx| {
                    this.status = Some(format!("Paste rejected: {rejection}"));
                    cx.notify();
                });
            }
            Ok(Err(_)) | Err(_) => {
                let _ = this.update(cx, |this, cx| {
                    this.status = Some(
                        "Paste request failed before any terminal input was written".to_owned(),
                    );
                    cx.notify();
                });
            }
        })
        .detach();
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
        if !self.terminal_input_focused(window) {
            decision = PasteDecision::Cancel;
        }
        let Some(session) = &self.session else {
            return;
        };
        let receiver = session.resolve_paste(confirmation.id, decision);
        self.focus_handle.focus(window);
        cx.notify();
        cx.spawn(async move |this, cx| match receiver.recv().await {
            Ok(Ok(PasteResolution::Written | PasteResolution::Cancelled)) => {}
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
        if !self.terminal_input_focused(window) {
            decision = Osc52AuthorizationDecision::Deny;
        }
        if let Some(session) = &self.session {
            session.resolve_osc52_authorization(request.id, decision);
        }
        self.focus_handle.focus(window);
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
        let (text, adjusted) = self.ime.text_for_utf16_range(range)?;
        *adjusted_range = Some(adjusted);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !ignore_disabled_input && !self.terminal_input_focused(window) {
            return None;
        }
        Some(UTF16Selection {
            range: self.ime.selected_range(),
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
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.terminal_input_focused(window) {
            self.ime.cancel();
            return;
        }
        self.ime.commit(text);
        if let Some(text) = self.ime.take_commit() {
            self.send_key_translation(KeyTranslation::Encoded(KeyInput::input_method_commit(text)));
        }
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
        if !self.terminal_input_focused(window) {
            self.ime.cancel();
            return;
        }
        self.ime
            .replace_and_mark(range, new_text, new_selected_range);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let position = self.screen.cursor.position?;
        let columns = self.screen.rows.first()?.len();
        let marked_text = self.ime.marked_text().unwrap_or("");
        let layout = layout_preedit(
            marked_text,
            usize::from(position.row),
            usize::from(position.column),
            columns,
            range_utf16.end,
        );
        Some(ime_candidate_bounds(
            element_bounds,
            columns,
            self.cell_width,
            px(self.line_height),
            layout.caret,
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.ime.selected_range().end)
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        self.close();
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = cx.entity().downgrade();
        let terminal_input_focused = self.sync_terminal_input_focus(window);
        let surface_active = self.product_focus.active_workspace
            && self.product_focus.active_window
            && window.is_window_active();
        self.sync_text_blink(surface_active, cx);
        let background = gpui_color(self.screen.background);
        let status = self.status.clone();
        let paste_confirmation = self.pending_paste;
        let osc52_authorization = self.pending_osc52;
        self.sync_scrollbar(cx);
        let scrollbar = self.scrollbar.clone();
        let pointer_uses_text_cursor = pointer_uses_text_cursor(
            self.screen.mouse_tracking,
            self.pointer_modifiers.shift,
            self.shift_selection,
        );
        let preedit = self.preedit_layout();
        let terminal_grid = TerminalGridElement::new(
            &self.screen,
            &mut self.render_cache,
            TerminalGridConfiguration {
                terminal_input_focused,
                font_family: self.font_family.clone(),
                font_size: px(self.font_size),
                line_height: px(self.line_height),
                cell_width: self.cell_width,
                preedit,
                focus_handle: self.focus_handle.clone(),
                input: cx.entity(),
                text_blink_visible: self.text_blink_visible,
            },
        );

        div()
            .on_children_prepainted(move |children, _window, cx| {
                let Some(bounds) = children.first().copied() else {
                    return;
                };
                let _ = pane.update(cx, |pane, cx| pane.update_grid_bounds(bounds, cx));
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
            .key_context(TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::confirm_unsafe_paste))
            .on_action(cx.listener(Self::cancel_unsafe_paste))
            .on_action(cx.listener(Self::allow_osc52_clipboard))
            .on_action(cx.listener(Self::deny_osc52_clipboard))
            .on_action(cx.listener(Self::increase_font_size))
            .on_action(cx.listener(Self::decrease_font_size))
            .on_action(cx.listener(Self::reset_font_size))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_any_mouse_down(cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .child(terminal_grid)
            .child(scrollbar)
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
                        .child(status),
                )
            })
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
            div()
                .id("cancel-unsafe-paste")
                .cursor_pointer()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(gpui_color(ACTIVE_THEME.element_active))
                .child("Cancel")
                .on_click(move |_, window, cx| {
                    let _ = cancel_pane.update(cx, |pane, cx| {
                        pane.cancel_unsafe_paste(&CancelUnsafePaste, window, cx);
                    });
                    cx.stop_propagation();
                }),
        )
        .child(
            div()
                .id("confirm-unsafe-paste")
                .cursor_pointer()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(gpui_color(ACTIVE_THEME.element_active))
                .child("Paste")
                .on_click(move |_, window, cx| {
                    let _ = pane.update(cx, |pane, cx| {
                        pane.confirm_unsafe_paste(&ConfirmUnsafePaste, window, cx);
                    });
                    cx.stop_propagation();
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
            div()
                .id("deny-osc52-clipboard")
                .cursor_pointer()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(gpui_color(ACTIVE_THEME.element_active))
                .child("Deny")
                .on_click(move |_, window, cx| {
                    let _ = deny_pane.update(cx, |pane, cx| {
                        pane.deny_osc52_clipboard(&DenyOsc52Clipboard, window, cx);
                    });
                    cx.stop_propagation();
                }),
        )
        .child(
            div()
                .id("allow-osc52-clipboard")
                .cursor_pointer()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(gpui_color(ACTIVE_THEME.element_active))
                .child("Allow")
                .on_click(move |_, window, cx| {
                    let _ = pane.update(cx, |pane, cx| {
                        pane.allow_osc52_clipboard(&AllowOsc52Clipboard, window, cx);
                    });
                    cx.stop_propagation();
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

fn accumulate_wheel_steps(remainder: &mut f32, delta: f32) -> i32 {
    *remainder += delta;
    let steps = remainder.trunc() as i32;
    *remainder -= steps as f32;
    steps
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
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use gpui::{Entity, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{ScrollbarSnapshot, SessionFailure, TerminalSessionFactory};

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
                selected: false,
                spacer_tail: false,
            }])]),
            ScrollbarSnapshot::default(),
            "blink",
        )
    }

    fn terminal_pane(cx: &mut TestAppContext) -> (Entity<TerminalPane>, &mut VisualTestContext) {
        cx.update(crate::ui::init);
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                .with_start_failure("terminal session unavailable in UI test"),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
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
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-keyboard-test"),
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
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-copy-test"),
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
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-paste-test"),
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

    #[gpui::test]
    fn command_equals_should_increase_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd-=");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (14.0, 15.0));
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

        assert!(pane.read_with(cx, |pane, _| pane.text_blink_visible));
        assert!(pane.read_with(cx, |pane, _| pane._text_blink_task.is_some()));

        cx.executor().advance_clock(TEXT_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!pane.read_with(cx, |pane, _| pane.text_blink_visible));

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

        assert!(pane.read_with(cx, |pane, _| pane.text_blink_visible));
        assert!(pane.read_with(cx, |pane, _| pane._text_blink_task.is_none()));

        cx.executor().advance_clock(TEXT_BLINK_INTERVAL * 2);
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.text_blink_visible));
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

    #[gpui::test]
    fn command_minus_should_decrease_terminal_font_size(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let before = pane.read_with(cx, |pane, _cx| pane.font_size());

        cx.simulate_keystrokes("cmd--");
        let after = pane.read_with(cx, |pane, _cx| pane.font_size());

        assert_eq!((before, after), (14.0, 13.0));
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

        cx.dispatch_action(CopySelection);
        cx.run_until_parked();

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
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window)));
        assert!(
            records
                .commands()
                .iter()
                .any(|call| { matches!(call.command, RecordedSessionCommand::RequestPaste(_)) })
        );

        cx.dispatch_action(ConfirmUnsafePaste);
        cx.run_until_parked();
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Confirm)
        }));
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window)));
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
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window)));

        cx.dispatch_action(DenyOsc52Clipboard);
        cx.run_until_parked();
        assert!(records.commands().iter().any(|call| {
            call.command
                == RecordedSessionCommand::ResolveOsc52Authorization(
                    request.id,
                    Osc52AuthorizationDecision::Deny,
                )
        }));
        assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window)));
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
    fn unsupported_key_down_is_neither_sent_nor_presented_as_a_pane_failure(
        cx: &mut TestAppContext,
    ) {
        let (pane, cx, records) = connected_terminal_pane(cx);

        cx.simulate_event(event("hyper", None, Modifiers::default()));

        let key_count = records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
            .count();
        assert_eq!(
            (key_count, pane.read_with(cx, |pane, _| pane.status.clone())),
            (0, None)
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
        let mut remainder = 0.0;

        assert_eq!(accumulate_wheel_steps(&mut remainder, 0.4), 0);
        assert_eq!(accumulate_wheel_steps(&mut remainder, 0.7), 1);
        assert!((remainder - 0.1).abs() < f32::EPSILON * 4.0);
        assert_eq!(accumulate_wheel_steps(&mut remainder, -1.3), -1);
        assert!((remainder + 0.2).abs() < f32::EPSILON * 4.0);
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
                std::path::Path::new("/tmp/spaceterm-terminal-pane-test"),
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
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-scale-test"),
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
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
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
    fn terminal_failure_should_keep_the_pane_visible_with_a_failure_status(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
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

        let state = pane.read_with(cx, |pane, _| (pane.status.clone(), pane.session.is_some()));
        assert_eq!(
            state,
            (
                Some(
                    "Shell output failed: read unavailable; shell exited (exit code 7)".to_owned()
                ),
                true,
            )
        );
        assert_eq!(
            (exits.get(), records.dropped_session_ids()),
            (0, Vec::new())
        );
        assert!(cx.debug_bounds("terminal-status").is_some());
    }
}
