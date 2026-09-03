//! Reusable GPUI controls for SpaceTerm.
//!
//! The crate owns interaction and editing behavior while the application supplies all product
//! colors and surrounding chrome from its canonical theme.

mod button;
mod command_palette;
mod icon;
mod menu;
mod middle_truncated_text;
mod modal;
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
    CommandPaletteActivationPolicy, CommandPaletteActivationSource, CommandPaletteCloseReason,
    CommandPaletteConfirm, CommandPaletteEvent, CommandPaletteGeneration, CommandPaletteHint,
    CommandPaletteItem, CommandPaletteLifecycleEvent, CommandPaletteMatching,
    CommandPaletteMetrics, CommandPalettePaint, CommandPaletteQuery,
    CommandPaletteReplacementFocus, CommandPaletteTheme,
};
pub use icon::{Icon, IconName};
pub use menu::{
    ContextMenu, ContextMenuOpenRequest, Menu, MenuActivation, MenuActivationSource, MenuAlignment,
    MenuCloseReason, MenuEntry, MenuLifecycleEvent, MenuMetrics, MenuPaint, MenuPlacement,
    MenuPlacementConfig, MenuRadioOption, MenuSize, MenuSizes, MenuTheme, Picker, PickerBuildError,
    PickerChange, PickerOption, dismiss_active_menu, window_menu_is_open,
};
pub use middle_truncated_text::MiddleTruncatedText;
pub use modal::{
    Alert, AlertAccessory, AlertIntent, AlertOutcome, AlertSuppression, DeterminateProgress,
    Dialog, DialogActionRequest, DialogCloseDecision, DialogCompletion, DialogFocusTarget,
    DialogInitialFocus, DialogOutcome, DialogPendingCompletion, DialogSize,
    MAX_ALERT_DETAIL_CHARACTERS, MAX_ALERT_MESSAGE_CHARACTERS, MAX_PROGRESS_DETAIL_CHARACTERS,
    MAX_PROGRESS_STATUS_CHARACTERS, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalActivationSource, ModalCloseReason, ModalDesktopPolicy,
    ModalDismissalError, ModalId, ModalKeybindingProfile, ModalLayer, ModalLifecycleEvent,
    ModalMetrics, ModalPaint, ModalPresentationError, ModalPresentationHandle, ModalPresentationId,
    ModalStaleGenerationError, ModalTerminalOutcomeError, ModalTextField, ModalTheme,
    ModalUpdateError, ModalValidationError, ProgressCancelDecision, ProgressCancellation,
    ProgressCancellationCompletion, ProgressDialog, ProgressDialogHandle, ProgressDialogOutcome,
    ProgressDialogUpdate, ProgressState, ProgressValueError, TextDirection,
    install_modal_keybindings, install_modal_policy, install_modal_theme, window_modal_is_open,
};
pub use overlay_scrollbar::{
    OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics, ScrollOffset, ScrollbarTheme,
};
pub use resize_handle::{
    ResizeAxis, ResizeFinishReason, ResizeHandle, ResizeHandleEvent, ResizeHandleMetrics,
    ResizeHandlePaint, ResizeHandleTarget, ResizeHandleTheme, ResizeInputSource,
    ResizeInteractionId,
};
pub use text_input::{
    Copy as EditCopy, Cut as EditCut, Paste as EditPaste, Redo as EditRedo,
    SelectAll as EditSelectAll, TextInput, TextInputChangeSource, TextInputComposition,
    TextInputContentMode, TextInputEscapeBehavior, TextInputEvent, TextInputKeybindingProfile,
    TextInputMetrics, TextInputPaint, TextInputReturnBehavior, TextInputSelection,
    TextInputTabBehavior, TextInputTheme, TextInputValueChanged, TextInputVariant,
    TextInputVariants, Undo as EditUndo, install_text_input_keybindings,
};
pub use tooltip::{
    Tooltip, TooltipLayer, TooltipMetrics, TooltipPaint, TooltipTarget, TooltipTargetVisibility,
    TooltipTheme,
};
pub use window_drag_region::{
    WindowDragFinishReason, WindowDragInteractionId, WindowDragRegion, WindowDragRegionEvent,
    WindowDragRegionResponse, WindowDragRegionStatus,
};

/// Bounded application-owned presentation catalog for every reusable control family.
///
/// The catalog keeps initialization stable as the library gains cohesive control families and
/// does not expose an arbitrary style map or call-site paint escape hatch.
pub struct ControlThemeCatalog {
    button: ButtonTheme,
    scrollbar: ScrollbarTheme,
    resize_handle: ResizeHandleTheme,
    menu: MenuTheme,
    command_palette: CommandPaletteTheme,
    text_input: TextInputTheme,
    tooltip: TooltipTheme,
    modal: ModalTheme,
}

impl ControlThemeCatalog {
    /// Creates the complete catalog required by [`init`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded catalog has one required entry for each reusable control family"
    )]
    pub fn new(
        button: ButtonTheme,
        scrollbar: ScrollbarTheme,
        resize_handle: ResizeHandleTheme,
        menu: MenuTheme,
        command_palette: CommandPaletteTheme,
        text_input: TextInputTheme,
        tooltip: TooltipTheme,
        modal: ModalTheme,
    ) -> Self {
        Self {
            button,
            scrollbar,
            resize_handle,
            menu,
            command_palette,
            text_input,
            tooltip,
            modal,
        }
    }
}

/// Installs the complete shared control catalog and platform-neutral control behavior.
///
/// Applications install desktop policy, modal key equivalents, and text-input keybindings
/// explicitly with [`install_modal_policy`], [`install_modal_keybindings`], and
/// [`install_text_input_keybindings`].
pub fn init(cx: &mut App, catalog: ControlThemeCatalog) -> gpui::Result<()> {
    icon::register_font(cx)?;
    cx.set_global(catalog.button);
    cx.set_global(catalog.scrollbar);
    cx.set_global(catalog.resize_handle);
    cx.set_global(catalog.menu);
    cx.set_global(catalog.command_palette);
    cx.set_global(catalog.text_input);
    cx.set_global(catalog.tooltip);
    cx.set_global(catalog.modal);
    button::init(cx);
    text_input::init(cx);
    menu::init(cx);
    command_palette::init(cx);
    tooltip::init(cx);
    modal::init(cx);
    Ok(())
}
