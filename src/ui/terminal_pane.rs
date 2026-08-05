use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render,
    ScrollWheelEvent, SharedString, Task, TextRun, Window, div, font, px, rgba,
};

use super::overlay_scrollbar::{OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics};
use super::terminal_element::{
    TerminalGridCache, TerminalGridElement, terminal_grid_content_bounds,
};
use super::{
    CopySelection, DecreaseTerminalFontSize, IncreaseTerminalFontSize, PasteClipboard,
    ResetTerminalFontSize, TERMINAL_KEY_CONTEXT,
};
use crate::terminal::{
    GridSize, InputModifiers, KeyCode, KeyInput, PointerButton, PointerInput, PointerPhase,
    ScreenSnapshot, SessionEvent, SurfacePosition, TerminalSessionFactory, TerminalSessionHandle,
    WheelInput,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalPaneEvent {
    FocusRequested,
    TitleChanged(SharedString),
    Exited,
}

pub(crate) struct TerminalPane {
    session_factory: Rc<dyn TerminalSessionFactory>,
    working_directory: PathBuf,
    session: Option<Box<dyn TerminalSessionHandle>>,
    session_start_attempted: bool,
    screen: Arc<ScreenSnapshot>,
    status: Option<String>,
    fallback_title: SharedString,
    title: SharedString,
    focus_handle: FocusHandle,
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    cell_width: Pixels,
    last_grid_size: Option<GridSize>,
    grid_bounds: Option<Bounds<Pixels>>,
    pressed_button: Option<PointerButton>,
    wheel_remainder: f32,
    scrollbar: Entity<OverlayScrollbar<u64>>,
    render_cache: TerminalGridCache,
    _event_task: Option<Task<()>>,
}

impl TerminalPane {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        working_directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family, DEFAULT_FONT_SIZE);
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
                        session.scroll_to(*rows);
                    }
                }
            },
        )
        .detach();

        Self {
            session_factory,
            working_directory,
            session: None,
            session_start_attempted: false,
            screen: ScreenSnapshot::empty(),
            status: None,
            title: fallback_title.clone(),
            fallback_title,
            focus_handle,
            font_family,
            font_size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
            cell_width,
            last_grid_size: None,
            grid_bounds: None,
            pressed_button: None,
            wheel_remainder: 0.0,
            scrollbar,
            render_cache: TerminalGridCache::new(),
            _event_task: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
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
        self._event_task.take();
        close_session(&mut self.session);
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<u64>> {
        let size = self.last_grid_size?;
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
        let size = grid_size(bounds, self.cell_width, self.line_height);
        self.grid_bounds = Some(terminal_grid_content_bounds(
            bounds,
            usize::from(size.cols),
            self.cell_width,
        ));
        if self.last_grid_size == Some(size) {
            return;
        }
        self.last_grid_size = Some(size);
        self.sync_scrollbar(cx);

        if let Some(session) = &self.session {
            session.resize(size);
            return;
        }

        if self.session_start_attempted {
            return;
        }
        self.session_start_attempted = true;

        match self.session_factory.start(size, &self.working_directory) {
            Ok(started) => {
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
                let title = normalized_pane_title(&screen.title, &self.fallback_title);
                if self.title.as_ref() != title {
                    self.title = title.into();
                    cx.emit(TerminalPaneEvent::TitleChanged(self.title.clone()));
                }
                self.screen = screen;
                self.sync_scrollbar(cx);
            }
            SessionEvent::Exited(status) => {
                eprintln!("{status}");
                self.status = Some(status);
                cx.emit(TerminalPaneEvent::Exited);
            }
            SessionEvent::Error(status) => {
                eprintln!("{status}");
                self.status = Some(status);
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = encode_key(event) {
            if let Some(session) = &self.session {
                session.key(input);
            }
            cx.stop_propagation();
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
        self.last_grid_size = None;
        self.sync_scrollbar(cx);
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                phase: PointerPhase::Press,
                button: Some(button),
                position,
                modifiers: input_modifiers(event.modifiers),
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
        let dragging = self.pressed_button.is_some();
        let Some(position) = self.surface_position(event.position, dragging) else {
            return;
        };
        if let Some(session) = &self.session {
            session.pointer(PointerInput {
                phase: PointerPhase::Motion,
                button: self.pressed_button,
                position,
                modifiers: input_modifiers(event.modifiers),
            });
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
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
                phase: PointerPhase::Release,
                button: Some(button),
                position,
                modifiers: input_modifiers(event.modifiers),
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
                steps,
                position,
                modifiers: input_modifiers(event.modifiers),
            });
        }
        cx.stop_propagation();
    }

    fn copy_selection(&mut self, _: &CopySelection, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let receiver = session.request_selection_text();
        cx.spawn(async move |this, cx| match receiver.recv().await {
            Ok(Ok(Some(text))) if !text.is_empty() => {
                let _ = this.update(cx, |_this, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
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
        if !text.is_empty()
            && let Some(session) = &self.session
        {
            session.paste(text);
        }
    }

    fn surface_position(
        &self,
        position: gpui::Point<Pixels>,
        allow_outside: bool,
    ) -> Option<SurfacePosition> {
        terminal_surface_position(
            self.grid_bounds?,
            position,
            self.cell_width,
            self.line_height,
            self.last_grid_size?,
            allow_outside,
        )
    }
}

impl EventEmitter<TerminalPaneEvent> for TerminalPane {}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        self.close();
    }
}

impl Render for TerminalPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = cx.entity().downgrade();
        let background = gpui_color(self.screen.background);
        let status = self.status.clone();
        self.sync_scrollbar(cx);
        let scrollbar = self.scrollbar.clone();
        let terminal_grid = TerminalGridElement::new(
            &self.screen,
            &self.font_family,
            px(self.font_size),
            px(self.line_height),
            self.cell_width,
            &mut self.render_cache,
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
            .cursor_text()
            .key_context(TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::increase_font_size))
            .on_action(cx.listener(Self::decrease_font_size))
            .on_action(cx.listener(Self::reset_font_size))
            .on_key_down(cx.listener(Self::on_key_down))
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
            .when_some(status, |root, status| {
                root.child(
                    div()
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

fn terminal_font(cx: &App) -> SharedString {
    let font_names = cx.text_system().all_font_names();
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
    .into()
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

fn grid_size(bounds: Bounds<Pixels>, cell_width: Pixels, line_height: f32) -> GridSize {
    let width = f32::from(bounds.size.width).max(f32::from(cell_width));
    let height = f32::from(bounds.size.height).max(line_height);

    GridSize {
        cols: ((width / f32::from(cell_width)).floor() as u16).max(MIN_COLS),
        rows: ((height / line_height).floor() as u16).max(MIN_ROWS),
        cell_width_px: f32::from(cell_width).round().clamp(1.0, u16::MAX as f32) as u16,
        cell_height_px: line_height.round().clamp(1.0, u16::MAX as f32) as u16,
    }
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

fn close_session(session: &mut Option<Box<dyn TerminalSessionHandle>>) {
    session.take();
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
    }
}

fn terminal_surface_position(
    bounds: Bounds<Pixels>,
    position: gpui::Point<Pixels>,
    rendered_cell_width: Pixels,
    rendered_line_height: f32,
    grid_size: GridSize,
    allow_outside: bool,
) -> Option<SurfacePosition> {
    if !allow_outside && !bounds.contains(&position) {
        return None;
    }

    let local_x = f32::from(position.x - bounds.origin.x);
    let local_y = f32::from(position.y - bounds.origin.y);
    Some(SurfacePosition {
        x: local_x * f32::from(grid_size.cell_width_px) / f32::from(rendered_cell_width),
        y: local_y * f32::from(grid_size.cell_height_px) / rendered_line_height,
    })
}

fn accumulate_wheel_steps(remainder: &mut f32, delta: f32) -> i32 {
    *remainder += delta;
    let steps = remainder.trunc() as i32;
    *remainder -= steps as f32;
    steps
}

fn encode_key(event: &KeyDownEvent) -> Option<KeyInput> {
    let keystroke = &event.keystroke;
    if keystroke.modifiers.platform || keystroke.modifiers.function {
        return None;
    }

    let code = match keystroke.key.as_str() {
        "enter" => KeyCode::Enter,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "escape" => KeyCode::Escape,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        "right" => KeyCode::ArrowRight,
        "left" => KeyCode::ArrowLeft,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "insert" => KeyCode::Insert,
        "delete" => KeyCode::Delete,
        key => {
            let character = single_char(key).or_else(|| {
                keystroke
                    .key_char
                    .as_deref()
                    .and_then(|text| text.chars().next())
            })?;
            KeyCode::Character(character)
        }
    };

    Some(KeyInput {
        code,
        text: matches!(code, KeyCode::Character(_))
            .then(|| keystroke.key_char.clone())
            .flatten(),
        modifiers: input_modifiers(keystroke.modifiers),
    })
}

fn single_char(value: &str) -> Option<char> {
    let mut characters = value.chars();
    let first = characters.next()?;
    characters.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::{Entity, Keystroke, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };

    fn terminal_pane(cx: &mut TestAppContext) -> (Entity<TerminalPane>, &mut VisualTestContext) {
        cx.update(crate::ui::init);
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                .with_start_failure("terminal session unavailable in UI test"),
        );
        let (pane, cx) = cx.add_window_view(|window, cx| {
            TerminalPane::new(
                session_factory,
                PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            pane.update(cx, |pane, _cx| pane.focus(window));
        });
        cx.run_until_parked();
        (pane, cx)
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

    #[test]
    fn reported_terminal_title_should_replace_the_shell_fallback() {
        assert_eq!(
            normalized_pane_title("  Claude Code  ", "zsh"),
            "Claude Code"
        );
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
            Some(KeyInput {
                code: KeyCode::Character('a'),
                text: Some("a".to_owned()),
                modifiers: InputModifiers::default(),
            })
        );
        assert_eq!(
            encode_key(&event("enter", None, Modifiers::default())),
            Some(KeyInput {
                code: KeyCode::Enter,
                text: None,
                modifiers: InputModifiers::default(),
            })
        );
        assert_eq!(
            encode_key(&event("up", None, Modifiers::default())),
            Some(KeyInput {
                code: KeyCode::ArrowUp,
                text: None,
                modifiers: InputModifiers::default(),
            })
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
            Some(KeyInput {
                code: KeyCode::Character('c'),
                text: Some("c".to_owned()),
                modifiers: InputModifiers {
                    control: true,
                    alt: true,
                    ..InputModifiers::default()
                },
            })
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
            Some(KeyInput {
                code: KeyCode::Character('d'),
                text: Some("d".to_owned()),
                modifiers: InputModifiers {
                    control: true,
                    ..InputModifiers::default()
                },
            })
        );
    }

    #[test]
    fn leaves_command_shortcuts_for_the_application() {
        let modifiers = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert!(encode_key(&event("q", None, modifiers)).is_none());
    }

    #[test]
    fn maps_rendered_positions_to_reported_terminal_geometry() {
        let bounds = Bounds::new(
            gpui::point(px(10.0), px(20.0)),
            gpui::size(px(75.0), px(40.0)),
        );
        let grid = GridSize {
            cols: 10,
            rows: 2,
            cell_width_px: 8,
            cell_height_px: 20,
        };

        let position = terminal_surface_position(
            bounds,
            gpui::point(px(47.5), px(30.0)),
            px(7.5),
            20.0,
            grid,
            false,
        )
        .unwrap();

        assert_eq!(position, SurfacePosition { x: 40.0, y: 10.0 });
        assert!(
            terminal_surface_position(
                bounds,
                gpui::point(px(9.0), px(20.0)),
                px(7.5),
                20.0,
                grid,
                false,
            )
            .is_none()
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
            }
        );
    }

    #[gpui::test]
    fn terminal_scrollbar_should_request_exact_row_offsets(cx: &mut TestAppContext) {
        let (pane, cx) = terminal_pane(cx);
        let records = TestTerminalSessionRecords::default();
        let session = TestTerminalSessionFactory::new(records.clone())
            .start(
                GridSize {
                    cols: 80,
                    rows: 24,
                    cell_width_px: 8,
                    cell_height_px: 16,
                },
                std::path::Path::new("/tmp/spaceterm-terminal-pane-test"),
            )
            .expect("the test terminal session should start");
        let scrollbar = pane.update(cx, |pane, _| {
            pane.session = Some(session.handle);
            pane.scrollbar.clone()
        });

        scrollbar.update(cx, |_, cx| {
            cx.emit(OverlayScrollbarEvent::OffsetRequested(u64::MAX - 1));
        });
        cx.run_until_parked();

        assert_eq!(
            records.commands().last().map(|input| &input.command),
            Some(&RecordedSessionCommand::ScrollTo(u64::MAX - 1))
        );
    }

    #[test]
    fn close_session_should_drop_a_handle_exactly_once_when_repeated() {
        let records = TestTerminalSessionRecords::default();
        let started = TestTerminalSessionFactory::new(records.clone())
            .start(
                GridSize {
                    cols: 80,
                    rows: 24,
                    cell_width_px: 8,
                    cell_height_px: 16,
                },
                std::path::Path::new("/tmp/spaceterm-terminal-pane-test"),
            )
            .expect("the test terminal session should start");
        let mut session = Some(started.handle);

        close_session(&mut session);
        close_session(&mut session);

        assert_eq!(records.dropped_session_ids(), vec![1]);
    }
}
