//! Reusable GPUI controls for SpaceTerm.
//!
//! The crate owns interaction and editing behavior while the application supplies all product
//! colors and surrounding chrome from its canonical theme.

mod anchored_placement;
mod appearance;
mod button;
#[cfg(test)]
mod catalog_tests;
mod combo_box;
#[cfg(test)]
mod combo_box_tests;
mod command_palette;
mod icon;
mod menu;
mod middle_truncated_text;
mod modal;
mod overlay_scrollbar;
mod resize_handle;
mod text_input;
mod toggle;
mod tooltip;
mod window_drag_region;

use gpui::App;

pub use anchored_placement::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, AnchoredTextDirection,
};
pub use appearance::{ControlShadow, ControlShadowLayer, ControlTypography};
pub use button::{
    Button, ButtonActivation, ButtonActivationSource, ButtonMetrics, ButtonPaint, ButtonRole,
    ButtonShape, ButtonSize, ButtonSizes, ButtonTheme, ButtonVariant, ButtonVariantStyle,
    ButtonVariants, IconButton,
};
pub use combo_box::{
    ComboBox, ComboBoxAcceptance, ComboBoxAccessory, ComboBoxActivationSource, ComboBoxCloseReason,
    ComboBoxCopy, ComboBoxFallback, ComboBoxHandle, ComboBoxItem, ComboBoxKeybindingProfile,
    ComboBoxLifecycleEvent, ComboBoxMetrics, ComboBoxPaint, ComboBoxTheme,
    install_combo_box_keybindings, install_portable_combo_box_keybindings,
    window_combo_box_is_open,
};
pub use command_palette::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteAction, CommandPaletteActivation,
    CommandPaletteActivationPolicy, CommandPaletteActivationSource, CommandPaletteCloseReason,
    CommandPaletteConfirm, CommandPaletteEvent, CommandPaletteFallback, CommandPaletteGeneration,
    CommandPaletteHint, CommandPaletteItem, CommandPaletteKeybindingProfile,
    CommandPaletteLifecycleEvent, CommandPaletteMatching, CommandPaletteMetrics,
    CommandPalettePaint, CommandPaletteQuery, CommandPaletteReplacementFocus, CommandPaletteTheme,
    install_command_palette_keybindings,
};
pub use icon::{CustomIconName, EmbeddedAssets, Icon, IconName};
pub use menu::{
    ContextMenu, ContextMenuOpenRequest, Menu, MenuActivation, MenuActivationSource, MenuAlignment,
    MenuCloseReason, MenuEntry, MenuKeybindingProfile, MenuLifecycleEvent, MenuMetrics, MenuPaint,
    MenuPlacement, MenuPlacementConfig, MenuRadioOption, MenuSize, MenuSizes, MenuTheme, Picker,
    PickerBuildError, PickerChange, PickerOption, dismiss_active_menu, install_menu_keybindings,
    window_menu_is_open,
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
    install_modal_keybindings, install_modal_policy, install_modal_theme,
    install_portable_modal_keybindings, window_modal_is_open,
};
pub use overlay_scrollbar::{
    OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics, ScrollOffset, ScrollbarMetrics,
    ScrollbarTheme,
};
pub use resize_handle::{
    ResizeAxis, ResizeFinishReason, ResizeHandle, ResizeHandleEvent, ResizeHandleMetrics,
    ResizeHandlePaint, ResizeHandleTarget, ResizeHandleTheme, ResizeInputSource,
    ResizeInteractionId,
};
pub use text_input::{
    Copy as EditCopy, Cut as EditCut, Paste as EditPaste, Redo as EditRedo,
    SelectAll as EditSelectAll, TextInput, TextInputChangeSource, TextInputComposition,
    TextInputContentMode, TextInputEscapeBehavior, TextInputEvent, TextInputHomeEndBehavior,
    TextInputKeybindingProfile, TextInputMetrics, TextInputPaint, TextInputReturnBehavior,
    TextInputSelection, TextInputTabBehavior, TextInputTheme, TextInputValueChanged,
    TextInputVariant, TextInputVariants, Undo as EditUndo, install_text_input_keybindings,
};
pub use toggle::{
    Checkbox, CheckboxChange, CheckboxState, Switch, SwitchChange, ToggleActivationSource,
    ToggleMetrics, TogglePaint, TogglePaints, ToggleSize, ToggleSizes, ToggleTheme,
    ToggleValuePaints,
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
#[derive(Clone, Debug, PartialEq)]
pub struct ControlThemeCatalog {
    generation: ControlThemeGeneration,
    typography: ControlTypography,
    button: ButtonTheme,
    toggle: ToggleTheme,
    scrollbar: ScrollbarTheme,
    resize_handle: ResizeHandleTheme,
    menu: MenuTheme,
    command_palette: CommandPaletteTheme,
    combo_box: ComboBoxTheme,
    text_input: TextInputTheme,
    tooltip: TooltipTheme,
    modal: ModalTheme,
}

impl gpui::Global for ControlThemeCatalog {}

/// The application-issued generation shared by every family in one control catalog.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControlThemeGeneration(u64);

impl ControlThemeGeneration {
    /// Creates a generation from the application appearance revision.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric appearance generation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The result of replacing the installed reusable-control presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlThemeReplacement {
    /// A different complete catalog was installed and windows were refreshed.
    Applied,
    /// The supplied catalog was identical to the installed catalog.
    Unchanged,
}

/// Replacement was requested before reusable controls were initialized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlThemeReplacementError;

impl std::fmt::Display for ControlThemeReplacementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("reusable controls are not initialized")
    }
}

impl std::error::Error for ControlThemeReplacementError {}

impl ControlThemeCatalog {
    /// Creates the complete catalog required by [`init`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded catalog has one required entry for each reusable control family"
    )]
    pub fn new(
        button: ButtonTheme,
        toggle: ToggleTheme,
        scrollbar: ScrollbarTheme,
        resize_handle: ResizeHandleTheme,
        menu: MenuTheme,
        command_palette: CommandPaletteTheme,
        combo_box: ComboBoxTheme,
        text_input: TextInputTheme,
        tooltip: TooltipTheme,
        modal: ModalTheme,
    ) -> Self {
        Self {
            generation: ControlThemeGeneration::default(),
            typography: ControlTypography::default(),
            button,
            toggle,
            scrollbar,
            resize_handle,
            menu,
            command_palette,
            combo_box,
            text_input,
            tooltip,
            modal,
        }
    }

    /// Sets the generation shared by every family in this complete catalog.
    pub fn generation(mut self, generation: ControlThemeGeneration) -> Self {
        self.generation = generation;
        self
    }

    /// Sets complete resolved typography shared by every text-bearing control family.
    pub fn typography(mut self, typography: ControlTypography) -> Self {
        self.typography = typography;
        self
    }

    /// Returns the catalog's application-issued generation.
    pub fn installed_generation(&self) -> ControlThemeGeneration {
        self.generation
    }

    /// Returns the complete resolved typography used by this catalog.
    pub fn installed_typography(&self) -> &ControlTypography {
        &self.typography
    }

    /// Scales every control family's text and spacing metrics as one complete catalog.
    ///
    /// Text-bearing control heights grow from their scaled line box plus scaled padding. Stable
    /// hairlines, interaction timing, paint, typography, and the application generation remain
    /// unchanged.
    pub fn scale_metrics(mut self, text_scale: f32, spacing_scale: f32) -> Self {
        self.button = self.button.scaled_metrics(text_scale, spacing_scale);
        self.toggle = self.toggle.scaled_metrics(text_scale, spacing_scale);
        self.scrollbar = self.scrollbar.scaled_metrics(text_scale, spacing_scale);
        self.resize_handle = self.resize_handle.scaled_metrics(text_scale, spacing_scale);
        self.menu = self.menu.scaled_metrics(text_scale, spacing_scale);
        self.command_palette = self
            .command_palette
            .scaled_metrics(text_scale, spacing_scale);
        self.combo_box = self.combo_box.scaled_metrics(text_scale, spacing_scale);
        self.text_input = self.text_input.scaled_metrics(text_scale, spacing_scale);
        self.tooltip = self.tooltip.scaled_metrics(text_scale, spacing_scale);
        self.modal = self.modal.scaled_metrics(text_scale, spacing_scale);
        self
    }
}

/// Installs the shared control catalog and initializes control-owned state.
///
/// Applications install desktop policy, portable modal behavior, modal key equivalents,
/// Menu key equivalents, Command Palette key equivalents, ComboBox key equivalents, and
/// text-input keybindings
/// explicitly with
/// [`install_modal_policy`], [`install_portable_modal_keybindings`],
/// [`install_modal_keybindings`], [`install_menu_keybindings`],
/// [`install_command_palette_keybindings`],
/// [`install_portable_combo_box_keybindings`], [`install_combo_box_keybindings`], and
/// [`install_text_input_keybindings`].
pub fn init(cx: &mut App, catalog: ControlThemeCatalog) -> gpui::Result<()> {
    icon::register_font(cx)?;
    install_control_theme_catalog(cx, catalog);
    button::init(cx);
    text_input::init(cx);
    menu::init(cx);
    command_palette::init(cx);
    combo_box::init(cx);
    tooltip::init(cx);
    modal::init_core(cx);
    Ok(())
}

/// Replaces all reusable-control presentation without reinstalling fonts, coordinators, or
/// keybindings.
///
/// Existing control entities and open overlays retain their interaction state. Changed catalogs
/// refresh every Operating-System Window so custom text shaping and deferred overlay content use
/// the new generation on the next frame.
pub fn replace_control_theme_catalog(
    cx: &mut App,
    catalog: ControlThemeCatalog,
) -> Result<ControlThemeReplacement, ControlThemeReplacementError> {
    if !cx.has_global::<ControlThemeCatalog>() {
        return Err(ControlThemeReplacementError);
    }
    if cx.global::<ControlThemeCatalog>() == &catalog {
        return Ok(ControlThemeReplacement::Unchanged);
    }
    install_control_theme_catalog(cx, catalog);
    cx.refresh_windows();
    Ok(ControlThemeReplacement::Applied)
}

fn install_control_theme_catalog(cx: &mut App, catalog: ControlThemeCatalog) {
    cx.set_global(catalog.button);
    cx.set_global(catalog.toggle);
    cx.set_global(catalog.scrollbar);
    cx.set_global(catalog.resize_handle);
    cx.set_global(catalog.menu);
    cx.set_global(catalog.command_palette);
    cx.set_global(catalog.combo_box);
    cx.set_global(catalog.text_input);
    cx.set_global(catalog.tooltip);
    cx.set_global(catalog.modal);
    cx.set_global(catalog);
}

fn control_typography(cx: &App) -> ControlTypography {
    cx.try_global::<ControlThemeCatalog>()
        .map(|catalog| catalog.typography.clone())
        .unwrap_or_default()
}
