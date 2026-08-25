//! Reusable GPUI controls for SpaceTerm.
//!
//! The crate owns interaction and editing behavior while the application supplies all product
//! colors and surrounding chrome from its canonical theme.

mod button;
mod command_palette;
mod menu;
mod middle_truncated_text;
mod overlay_scrollbar;
mod resize_handle;
mod text_input;
mod tooltip;
mod window_drag_region;

use gpui::App;

pub use button::{
    Button, ButtonActivation, ButtonActivationSource, ButtonMetrics, ButtonPaint, ButtonRole,
    ButtonShape, ButtonSize, ButtonSizes, ButtonTheme, ButtonVariant, ButtonVariantStyle,
    ButtonVariants, IconButton,
};
pub use command_palette::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteAction, CommandPaletteActivation,
    CommandPaletteActivationSource, CommandPaletteCloseReason, CommandPaletteEvent,
    CommandPaletteGeneration, CommandPaletteHint, CommandPaletteItem, CommandPaletteLifecycleEvent,
    CommandPaletteMetrics, CommandPalettePaint, CommandPaletteQuery,
    CommandPaletteReplacementFocus, CommandPaletteTheme,
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
pub use resize_handle::{
    ResizeAxis, ResizeFinishReason, ResizeHandle, ResizeHandleEvent, ResizeHandleMetrics,
    ResizeHandlePaint, ResizeHandleTheme, ResizeInputSource, ResizeInteractionId,
};
pub use text_input::{
    Copy as EditCopy, Cut as EditCut, Paste as EditPaste, Redo as EditRedo,
    SelectAll as EditSelectAll, TextInput, TextInputChangeSource, TextInputComposition,
    TextInputEvent, TextInputKeybindingProfile, TextInputMetrics, TextInputPaint,
    TextInputSelection, TextInputTabBehavior, TextInputTheme, TextInputValueChanged,
    TextInputVariant, TextInputVariants, Undo as EditUndo, install_text_input_keybindings,
};
pub use tooltip::{
    Tooltip, TooltipLayer, TooltipMetrics, TooltipPaint, TooltipTarget, TooltipTheme,
};
pub use window_drag_region::{
    WindowDragFinishReason, WindowDragInteractionId, WindowDragRegion, WindowDragRegionEvent,
    WindowDragRegionResponse, WindowDragRegionStatus,
};

/// Installs shared control themes and platform-neutral control behavior.
///
/// Applications select a text-input keybinding profile separately with
/// [`install_text_input_keybindings`].
#[expect(
    clippy::too_many_arguments,
    reason = "installation accepts one explicit application-owned theme per reusable control family"
)]
pub fn init(
    cx: &mut App,
    button_theme: ButtonTheme,
    scrollbar_theme: ScrollbarTheme,
    resize_handle_theme: ResizeHandleTheme,
    menu_theme: MenuTheme,
    command_palette_theme: CommandPaletteTheme,
    text_input_theme: TextInputTheme,
    tooltip_theme: TooltipTheme,
) {
    cx.set_global(button_theme);
    cx.set_global(scrollbar_theme);
    cx.set_global(resize_handle_theme);
    cx.set_global(menu_theme);
    cx.set_global(command_palette_theme);
    cx.set_global(text_input_theme);
    cx.set_global(tooltip_theme);
    text_input::init(cx);
    menu::init(cx);
    command_palette::init(cx);
    tooltip::init(cx);
}
