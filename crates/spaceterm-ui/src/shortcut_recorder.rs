//! The reusable shortcut recorder: a compact field that captures one key chord.
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Context, ElementId, EventEmitter, FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke,
    Modifiers, ModifiersChangedEvent, SharedString, Subscription, Window, accesskit, div,
};

use crate::chord_capture::ChordRelease;
use crate::{CapturedKey, ChordCapture, FieldState, TextInputVariant};

/// The field's width in multiples of its own height.
///
/// A fixed proportion keeps every recorder in a column the same width, so a list of shortcuts
/// reads as one aligned column, and the width follows density with the height it is measured in.
const WIDTH_IN_HEIGHTS: f32 = 4.5;

/// Decides whether a captured chord is acceptable, explaining a refusal in the caller's words.
pub type ShortcutValidator = Rc<dyn Fn(&Keystroke, &App) -> Result<(), SharedString>>;

/// Presents the modifiers held while recording, in the host's notation.
pub type ShortcutModifierFormatter = Rc<dyn Fn(Modifiers) -> SharedString>;

/// What one recording produced.
#[derive(Clone, Debug, PartialEq)]
pub enum ShortcutRecorderEvent {
    /// A chord the validator accepted. Recording has ended.
    Recorded(Keystroke),
    /// A chord the validator refused, with the validator's explanation. Recording continues so the
    /// next chord can replace it.
    Rejected(SharedString),
    /// Delete or Backspace asked for no shortcut. Recording has ended.
    Cleared,
    /// Recording ended without a change: Escape, Tab, a second click, lost focus, or an inactive
    /// window.
    Cancelled,
}

/// One recording in progress, owned so that ending it releases the keystroke interception.
struct Recording {
    held: Modifiers,
    rejected: bool,
    _capture: ChordCapture,
}

/// How the field's text is painted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortcutTone {
    /// A shortcut, or the modifiers being held for one, in the shortcut face.
    Value,
    /// The empty label or the recording prompt, in the placeholder paint.
    Placeholder,
}

/// A compact field that shows one shortcut and records a replacement.
///
/// The recorder owns capture only. Its consumer supplies the value through
/// [`set_value`](Self::set_value), accepts chords through [`validator`](Self::validator), and
/// applies [`ShortcutRecorderEvent`]s. While recording, it captures every keystroke ahead of key
/// bindings and menu Shortcuts, so a chord such as closing the window is recorded rather than
/// performed.
pub struct ShortcutRecorder {
    id: ElementId,
    accessibility_name: SharedString,
    focus_handle: FocusHandle,
    value: Option<SharedString>,
    empty_label: SharedString,
    recording_placeholder: SharedString,
    validator: ShortcutValidator,
    format_modifiers: ShortcutModifierFormatter,
    disabled: bool,
    recording: Option<Recording>,
    /// The chord just recorded, while its keys are still held.
    release: Option<ChordRelease>,
    debug_selector: Option<SharedString>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<ShortcutRecorderEvent> for ShortcutRecorder {}

impl ShortcutRecorder {
    /// Creates an enabled recorder with no value that accepts every chord.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle().tab_stop(true);
        let subscriptions = [
            cx.on_blur(&focus_handle, window, |recorder, _, cx| {
                recorder.cancel_recording(cx);
            }),
            cx.observe_window_activation(window, |recorder, window, cx| {
                if !window.is_window_active() {
                    recorder.cancel_recording(cx);
                }
            }),
        ];
        Self {
            id: id.into(),
            accessibility_name: accessibility_name.into(),
            focus_handle,
            value: None,
            empty_label: SharedString::default(),
            recording_placeholder: SharedString::default(),
            validator: Rc::new(|_, _| Ok(())),
            format_modifiers: Rc::new(|_| SharedString::default()),
            disabled: false,
            recording: None,
            release: None,
            debug_selector: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn empty_label(mut self, label: impl Into<SharedString>) -> Self {
        self.empty_label = label.into();
        self
    }

    /// Sets the prompt shown while recording and no modifier is held.
    pub fn recording_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.recording_placeholder = placeholder.into();
        self
    }

    /// Sets the policy deciding which chords complete a recording.
    pub fn validator(
        mut self,
        validator: impl Fn(&Keystroke, &App) -> Result<(), SharedString> + 'static,
    ) -> Self {
        self.validator = Rc::new(validator);
        self
    }

    /// Sets how held modifiers are presented while recording.
    pub fn modifier_formatter(
        mut self,
        formatter: impl Fn(Modifiers) -> SharedString + 'static,
    ) -> Self {
        self.format_modifiers = Rc::new(formatter);
        self
    }

    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// The logical accessibility name retained alongside the presented shortcut.
    pub fn accessibility_name(&self) -> &SharedString {
        &self.accessibility_name
    }

    /// Replaces the presented shortcut, already formatted in the host's notation.
    pub fn set_value(&mut self, value: Option<SharedString>, cx: &mut Context<Self>) {
        if self.value != value {
            self.value = value;
            cx.notify();
        }
    }

    /// Enables or disables the field. Disabling ends a recording in progress.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled == disabled {
            return;
        }
        if disabled {
            self.cancel_recording(cx);
        }
        self.disabled = disabled;
        self.focus_handle = self.focus_handle.clone().tab_stop(!disabled);
        cx.notify();
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Focuses the field and starts recording. Does nothing while disabled or already recording.
    pub fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled || self.recording.is_some() {
            return;
        }
        self.release = None;
        self.focus_handle.focus(window, cx);
        let capture = ChordCapture::start(self.focus_handle.clone(), window, cx, Self::captured);
        self.recording = Some(Recording {
            held: window.modifiers(),
            rejected: false,
            _capture: capture,
        });
        cx.notify();
    }

    pub fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        self.release = None;
        self.finish(ShortcutRecorderEvent::Cancelled, cx);
    }

    fn captured(&mut self, key: CapturedKey, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = match key {
            CapturedKey::Chord(keystroke) => keystroke,
            CapturedKey::Escape | CapturedKey::Traverse | CapturedKey::FocusLost => {
                self.cancel_recording(cx);
                return;
            }
            CapturedKey::Erase => {
                self.finish(ShortcutRecorderEvent::Cleared, cx);
                return;
            }
        };
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        match (self.validator)(&keystroke, cx) {
            Ok(()) => {
                self.release = Some(ChordRelease::hold(&keystroke, window, cx));
                self.finish(ShortcutRecorderEvent::Recorded(keystroke), cx);
            }
            Err(reason) => {
                recording.rejected = true;
                cx.emit(ShortcutRecorderEvent::Rejected(reason));
                cx.notify();
            }
        }
    }

    fn finish(&mut self, event: ShortcutRecorderEvent, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            cx.emit(event);
            cx.notify();
        }
    }

    fn toggle_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recording.is_some() {
            self.cancel_recording(cx);
        } else {
            self.start_recording(window, cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if self.recording.is_none()
            && !keystroke.modifiers.modified()
            && matches!(keystroke.key.as_str(), "enter" | "space")
        {
            cx.stop_propagation();
            self.start_recording(window, cx);
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if self
            .release
            .as_ref()
            .is_some_and(|release| release.is_released_by(event))
        {
            self.release = None;
        }
    }

    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.release = None;
        if let Some(recording) = self.recording.as_mut()
            && recording.held != event.modifiers
        {
            recording.held = event.modifiers;
            cx.notify();
        }
    }

    /// The text the field presents and how it is painted.
    pub(crate) fn presentation(&self) -> (SharedString, ShortcutTone) {
        match (&self.recording, &self.value) {
            (Some(recording), _) if recording.held.modified() => {
                ((self.format_modifiers)(recording.held), ShortcutTone::Value)
            }
            (Some(_), _) => (
                self.recording_placeholder.clone(),
                ShortcutTone::Placeholder,
            ),
            (None, Some(value)) => (value.clone(), ShortcutTone::Value),
            (None, None) => (self.empty_label.clone(), ShortcutTone::Placeholder),
        }
    }
}

impl Render for ShortcutRecorder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = crate::floating_surface::hosted_search_field_theme(cx).compact_field();
        let (value_paint, placeholder_paint) = crate::floating_surface::hosted_text_input_theme(cx)
            .paint(TextInputVariant::Standard)
            .value_paints(self.disabled);
        let typography = crate::control_typography(cx);
        let (text, tone) = self.presentation();
        let (font, color) = match tone {
            ShortcutTone::Value => (typography.shortcut().clone(), value_paint),
            ShortcutTone::Placeholder => (typography.regular().clone(), placeholder_paint),
        };
        let state = FieldState::default().disabled(self.disabled).invalid(
            self.recording
                .as_ref()
                .is_some_and(|recording| recording.rejected),
        );
        let selector = self.debug_selector.clone();
        let press_recorder = cx.entity().downgrade();
        crate::field_frame::themed_field_frame(
            field.frame,
            self.id.clone(),
            &self.focus_handle,
            state,
            field.corner_radius,
        )
        // The recorder has no editor inside, so its frame is the focus target.
        .track_focus(&self.focus_handle)
        .role(accesskit::Role::Button)
        .aria_label(self.accessibility_name.clone())
        .aria_value(
            self.value
                .clone()
                .unwrap_or_else(|| self.empty_label.clone()),
        )
        .aria_disabled(self.disabled)
        .when(self.recording.is_some(), |frame| {
            let announcement = SharedString::from(format!(
                "Recording Shortcut. {}",
                self.recording_placeholder
            ));
            frame
                .aria_description(announcement.clone())
                .a11y_synthetic_children(move |builder| {
                    let mut status = accesskit::Node::new(accesskit::Role::Status);
                    status.set_value(announcement.as_ref());
                    status.set_live(accesskit::Live::Polite);
                    if let Some(bounds) = builder.parent_node().bounds() {
                        status.set_bounds(bounds);
                    }
                    builder.push_child(builder.synthetic_node_id("recording-status"), status);
                })
        })
        .when_some(selector, |frame, selector| {
            frame.debug_selector(move || selector.to_string())
        })
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(field.height * WIDTH_IN_HEIGHTS)
        .h(field.height)
        .px(field.horizontal_padding)
        .text_size(field.label_size)
        .line_height(field.line_height)
        .font(font)
        .text_color(color)
        .when(!self.disabled, |frame| {
            frame
                .cursor_pointer()
                .on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
                    let _ = press_recorder.update(cx, |recorder, cx| {
                        recorder.toggle_recording(window, cx);
                    });
                })
                .on_click(cx.listener(|recorder, _, window, cx| {
                    recorder.toggle_recording(window, cx);
                }))
                .on_key_down(cx.listener(Self::key_down))
                .on_key_up(cx.listener(Self::key_up))
                .on_modifiers_changed(cx.listener(Self::modifiers_changed))
        })
        .child(div().min_w_0().truncate().child(text))
    }
}

#[cfg(test)]
#[path = "shortcut_recorder_tests.rs"]
mod tests;
