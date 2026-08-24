//! Reusable GPUI controls for SpaceTerm.
//!
//! The crate owns interaction and editing behavior while the application supplies all product
//! colors and surrounding chrome from its canonical theme.

mod button;
mod menu;
mod middle_truncated_text;
mod overlay_scrollbar;
mod text_input;

use gpui::App;

pub use button::{
    Button, ButtonActivation, ButtonActivationSource, ButtonMetrics, ButtonPaint, ButtonRole,
    ButtonShape, ButtonSize, ButtonSizes, ButtonTheme, ButtonVariant, ButtonVariantStyle,
    ButtonVariants, IconButton,
};
pub use menu::{
    ContextMenu, ContextMenuOpenRequest, Menu, MenuActivation, MenuActivationSource, MenuAlignment,
    MenuCloseReason, MenuEntry, MenuLifecycleEvent, MenuMetrics, MenuPaint, MenuPlacement,
    MenuPlacementConfig, MenuRadioOption, MenuSize, MenuSizes, MenuTheme, Picker, PickerBuildError,
    PickerChange, PickerOption,
};
pub use middle_truncated_text::MiddleTruncatedText;
pub use overlay_scrollbar::{
    OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics, ScrollOffset, ScrollbarTheme,
};
pub use text_input::{TextInput, TextInputEvent, TextInputStyle};

/// Registers shared control themes, actions, and macOS key bindings.
pub fn init(
    cx: &mut App,
    button_theme: ButtonTheme,
    scrollbar_theme: ScrollbarTheme,
    menu_theme: MenuTheme,
) {
    cx.set_global(button_theme);
    cx.set_global(scrollbar_theme);
    cx.set_global(menu_theme);
    text_input::init(cx);
    menu::init(cx);
}
