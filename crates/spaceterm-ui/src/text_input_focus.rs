use gpui::{AnyWindowHandle, App, Global, Window};

/// The application window that retains editing while native UI borrows its key status.
///
/// The host supplies its active application's main window independently of GPUI's key-window
/// activation. An inactive application supplies `None`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TextInputFocus(Option<AnyWindowHandle>);

impl Global for TextInputFocus {}

impl TextInputFocus {
    /// Publishes the native editing owner and notifies controls when that owner changes.
    pub fn set(window: Option<AnyWindowHandle>, cx: &mut App) {
        let focus = Self(window);
        if cx.try_global::<Self>() != Some(&focus) {
            cx.set_global(focus);
        }
    }
}

pub(crate) fn retains_text_input(window: &Window, cx: &App) -> bool {
    window.is_window_active()
        || cx
            .try_global::<TextInputFocus>()
            .is_some_and(|focus| focus.0 == Some(window.window_handle()))
}
