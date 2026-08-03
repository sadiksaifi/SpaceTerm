use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton, Pixels, Render,
    SharedString, Task, TextRun, Window, div, font, px, rgba,
};

use super::terminal_element::{TerminalGridCache, TerminalGridElement};
use crate::terminal::{GridSize, ScreenSnapshot, SessionEvent, TerminalSession};
use crate::theme::{ACTIVE_THEME, Color};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;
const PADDING: f32 = 12.0;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;

pub(crate) struct TerminalPane {
    session: Option<TerminalSession>,
    screen: Arc<ScreenSnapshot>,
    status: Option<String>,
    focus_handle: FocusHandle,
    font_family: SharedString,
    cell_width: Pixels,
    last_grid_size: Option<GridSize>,
    render_cache: TerminalGridCache,
    _event_task: Option<Task<()>>,
}

impl TerminalPane {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family);

        Self {
            session: None,
            screen: ScreenSnapshot::empty(),
            status: None,
            focus_handle,
            font_family,
            cell_width,
            last_grid_size: None,
            render_cache: TerminalGridCache::new(),
            _event_task: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    fn update_grid_bounds(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let size = grid_size(bounds, self.cell_width);
        if self.last_grid_size == Some(size) {
            return;
        }
        self.last_grid_size = Some(size);

        if let Some(session) = &self.session {
            session.resize(size);
            return;
        }

        match TerminalSession::start(size) {
            Ok((session, receiver)) => {
                self.session = Some(session);
                self._event_task = Some(cx.spawn(async move |this, cx| {
                    while let Ok(event) = receiver.recv().await {
                        let mut events = vec![event];
                        while let Ok(event) = receiver.try_recv() {
                            events.push(event);
                        }
                        if this
                            .update(cx, |this, cx| {
                                for event in events {
                                    this.handle_event(event);
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

    fn handle_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Screen(screen) => self.screen = screen,
            SessionEvent::Exited(status) | SessionEvent::Error(status) => {
                eprintln!("{status}");
                self.status = Some(status);
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let bytes = encode_key(event);
        if !bytes.is_empty() {
            if let Some(session) = &self.session {
                session.send_input(bytes);
            }
            cx.stop_propagation();
        }
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        self._event_task.take();
        self.session.take();
    }
}

impl Render for TerminalPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = cx.entity().downgrade();
        let background = gpui_color(self.screen.background);
        let status = self.status.clone();
        let terminal_grid = TerminalGridElement::new(
            &self.screen,
            &self.font_family,
            px(FONT_SIZE),
            px(LINE_HEIGHT),
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
            .p(px(PADDING))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| this.focus_handle.focus(window)),
            )
            .child(terminal_grid)
            .when_some(status, |root, status| {
                root.child(
                    div()
                        .absolute()
                        .right(px(PADDING))
                        .bottom(px(PADDING))
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

fn measure_cell_width(window: &mut Window, family: &SharedString) -> Pixels {
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
        .shape_line("M".into(), px(FONT_SIZE), &[run], None)
        .width
}

fn grid_size(bounds: Bounds<Pixels>, cell_width: Pixels) -> GridSize {
    let width = f32::from(bounds.size.width).max(f32::from(cell_width));
    let height = f32::from(bounds.size.height).max(LINE_HEIGHT);

    GridSize {
        cols: ((width / f32::from(cell_width)).floor() as u16).max(MIN_COLS),
        rows: ((height / LINE_HEIGHT).floor() as u16).max(MIN_ROWS),
        cell_width_px: f32::from(cell_width).round().clamp(1.0, u16::MAX as f32) as u16,
        cell_height_px: LINE_HEIGHT as u16,
    }
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

fn encode_key(event: &KeyDownEvent) -> Vec<u8> {
    let keystroke = &event.keystroke;
    if keystroke.modifiers.platform || keystroke.modifiers.function {
        return Vec::new();
    }

    let special = match keystroke.key.as_str() {
        "enter" => Some(b"\r".as_slice()),
        "backspace" => Some(b"\x7f".as_slice()),
        "tab" if keystroke.modifiers.shift => Some(b"\x1b[Z".as_slice()),
        "tab" => Some(b"\t".as_slice()),
        "escape" => Some(b"\x1b".as_slice()),
        "up" => Some(b"\x1b[A".as_slice()),
        "down" => Some(b"\x1b[B".as_slice()),
        "right" => Some(b"\x1b[C".as_slice()),
        "left" => Some(b"\x1b[D".as_slice()),
        "home" => Some(b"\x1b[H".as_slice()),
        "end" => Some(b"\x1b[F".as_slice()),
        "pageup" => Some(b"\x1b[5~".as_slice()),
        "pagedown" => Some(b"\x1b[6~".as_slice()),
        "insert" => Some(b"\x1b[2~".as_slice()),
        "delete" => Some(b"\x1b[3~".as_slice()),
        _ => None,
    };

    if let Some(bytes) = special {
        return with_optional_alt_prefix(bytes, keystroke.modifiers.alt);
    }

    if keystroke.modifiers.control
        && let Some(control) = control_byte(&keystroke.key)
    {
        return with_optional_alt_prefix(&[control], keystroke.modifiers.alt);
    }

    keystroke
        .key_char
        .as_deref()
        .map(str::as_bytes)
        .map_or_else(Vec::new, |bytes| bytes.to_vec())
}

fn with_optional_alt_prefix(bytes: &[u8], alt: bool) -> Vec<u8> {
    if !alt {
        return bytes.to_vec();
    }
    let mut encoded = Vec::with_capacity(bytes.len() + 1);
    encoded.push(0x1b);
    encoded.extend_from_slice(bytes);
    encoded
}

fn control_byte(key: &str) -> Option<u8> {
    let byte = *key.as_bytes().first()?;
    if key.len() != 1 || !byte.is_ascii() {
        return None;
    }

    match byte {
        b'?' => Some(0x7f),
        b' '..=b'_' => Some(byte & 0x1f),
        b'a'..=b'z' => Some(byte - b'a' + 1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, Modifiers};

    use super::*;

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
    fn encodes_printable_text_and_terminal_keys() {
        assert_eq!(
            encode_key(&event("a", Some("a"), Modifiers::default())),
            b"a"
        );
        assert_eq!(
            encode_key(&event("enter", None, Modifiers::default())),
            b"\r"
        );
        assert_eq!(
            encode_key(&event("up", None, Modifiers::default())),
            b"\x1b[A"
        );
    }

    #[test]
    fn encodes_control_characters() {
        let modifiers = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(encode_key(&event("c", Some("c"), modifiers)), vec![0x03]);
    }

    #[test]
    fn leaves_command_shortcuts_for_the_application() {
        let modifiers = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert!(encode_key(&event("q", None, modifiers)).is_empty());
    }
}
