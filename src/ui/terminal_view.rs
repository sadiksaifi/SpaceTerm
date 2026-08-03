use gpui::prelude::*;
use gpui::{
    App, Context, FocusHandle, FontWeight, IntoElement, KeyDownEvent, MouseButton, Pixels, Render,
    SharedString, Subscription, Task, TextRun, Window, div, font, px, rgba,
};

use crate::terminal::{CellSnapshot, GridSize, ScreenSnapshot, SessionEvent, TerminalSession};
use crate::theme::{ACTIVE_THEME, Color};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;
const PADDING: f32 = 12.0;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;

pub(crate) struct TerminalView {
    session: Option<TerminalSession>,
    screen: ScreenSnapshot,
    status: Option<String>,
    focus_handle: FocusHandle,
    font_family: SharedString,
    cell_width: Pixels,
    _event_task: Option<Task<()>>,
    _bounds_subscription: Subscription,
}

impl TerminalView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);

        let font_family = terminal_font(cx);
        let cell_width = measure_cell_width(window, &font_family);
        let initial_size = grid_size(window, cell_width);

        let (session, receiver, status) = match TerminalSession::start(initial_size) {
            Ok((session, receiver)) => (Some(session), Some(receiver), None),
            Err(error) => {
                eprintln!("failed to start terminal session: {error:#}");
                (None, None, Some(error.to_string()))
            }
        };

        let event_task = receiver.map(|receiver| {
            cx.spawn(async move |this, cx| {
                while let Ok(event) = receiver.recv().await {
                    if this
                        .update(cx, |this, cx| {
                            this.handle_event(event);
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });

        let bounds_subscription = cx.observe_window_bounds(window, |this, window, _| {
            if let Some(session) = &this.session {
                session.resize(grid_size(window, this.cell_width));
            }
        });

        Self {
            session,
            screen: ScreenSnapshot::empty(),
            status,
            focus_handle,
            font_family,
            cell_width,
            _event_task: event_task,
            _bounds_subscription: bounds_subscription,
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

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_family = self.font_family.clone();
        let cell_width = self.cell_width;
        let background = gpui_color(self.screen.background);
        let rows = self.screen.rows.clone();
        let status = self.status.clone();

        div()
            .id("terminal-pane")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(background)
            .p(px(PADDING))
            .font_family(font_family)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| this.focus_handle.focus(window)),
            )
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(rows.into_iter().map(move |row| {
                        div()
                            .h(px(LINE_HEIGHT))
                            .w_full()
                            .flex()
                            .flex_row()
                            .flex_shrink_0()
                            .children(
                                row.into_iter()
                                    .map(move |cell| render_cell(cell, cell_width)),
                            )
                    })),
            )
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

fn render_cell(cell: CellSnapshot, cell_width: Pixels) -> impl IntoElement {
    let (foreground, background) = if cell.cursor {
        (
            gpui_color(ACTIVE_THEME.terminal_background),
            gpui_color(ACTIVE_THEME.terminal_foreground),
        )
    } else {
        (gpui_color(cell.foreground), gpui_color(cell.background))
    };

    div()
        .w(cell_width)
        .h(px(LINE_HEIGHT))
        .flex_none()
        .bg(background)
        .text_color(foreground)
        .when(cell.bold, |cell| cell.font_weight(FontWeight::BOLD))
        .when(cell.italic, |cell| cell.italic())
        .child(cell.text)
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

fn grid_size(window: &Window, cell_width: Pixels) -> GridSize {
    let viewport = window.viewport_size();
    let usable_width = (f32::from(viewport.width) - PADDING * 2.0).max(f32::from(cell_width));
    let usable_height = (f32::from(viewport.height) - PADDING * 2.0).max(LINE_HEIGHT);

    GridSize {
        cols: ((usable_width / f32::from(cell_width)).floor() as u16).max(MIN_COLS),
        rows: ((usable_height / LINE_HEIGHT).floor() as u16).max(MIN_ROWS),
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
