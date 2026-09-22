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
mod field_frame;
mod floating_surface;
#[cfg(test)]
mod floating_surface_tests;
mod fuzzy;
mod icon;
mod list_row;
mod menu;
mod middle_truncated_text;
mod modal;
mod overlay_scrollbar;
mod progress;
#[cfg(test)]
mod progress_tests;
mod resize_handle;
mod search_field;
mod segmented_control;
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
pub use field_frame::{FieldFrameTheme, FieldState, field_frame, field_surface};
pub use floating_surface::{
    ControlHost, ControlHostElement, ControlThemeScope, ControlThemeScopeElement,
    ControlWindowActivity, ControlWindowActivityElement, FloatingLayer, FloatingRole,
    FloatingShell, FloatingSurfacePaint, FloatingSurfacePaints, FloatingSurfaceTheme,
    SurfaceControlThemes, floating_surface_theme,
};
pub use fuzzy::{FuzzyMatch, FuzzyTarget, fuzzy_filter, highlight_ranges};
pub use icon::{CustomIconName, EmbeddedAssets, Icon, IconName};
pub use list_row::{ListRowPaint, ListRowPaints};
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
pub use progress::{
    FrameSpinner, ProgressBar, ProgressMetrics, ProgressMotion, ProgressPaint, ProgressRing,
    ProgressSize, ProgressSizes, ProgressTheme,
};
pub use resize_handle::{
    ResizeAxis, ResizeFinishReason, ResizeHandle, ResizeHandleEvent, ResizeHandleMetrics,
    ResizeHandlePaint, ResizeHandleTarget, ResizeHandleTheme, ResizeInputSource,
    ResizeInteractionId,
};
pub use search_field::{SearchField, SearchFieldMetrics, SearchFieldPaint, SearchFieldTheme};
pub use segmented_control::{
    MAXIMUM_SEGMENTED_OPTIONS, SegmentedActivationSource, SegmentedBuildError, SegmentedChange,
    SegmentedControl, SegmentedControlTheme, SegmentedMetrics, SegmentedOption, SegmentedPaint,
    SegmentedPaints, SegmentedSize, SegmentedSizes, SegmentedValuePaints,
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

/// Development-only visual state pinning for the production renderer.
///
/// This does not arm interaction or acquire keyboard focus. Production builds omit the Interface.
#[cfg(feature = "appearance-exerciser")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlPreviewState {
    Normal,
    Hovered,
    Pressed,
    Focused,
}
#[cfg(feature = "appearance-exerciser")]
impl ControlPreviewState {
    pub(crate) fn hovered(self) -> bool {
        matches!(self, Self::Hovered | Self::Pressed)
    }
    pub(crate) fn pressed(self) -> bool {
        self == Self::Pressed
    }
    pub(crate) fn focused(self) -> bool {
        self == Self::Focused
    }
}

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
    progress: ProgressTheme,
    scrollbar: ScrollbarTheme,
    resize_handle: ResizeHandleTheme,
    segmented_control: SegmentedControlTheme,
    search_field: SearchFieldTheme,
    menu: MenuTheme,
    command_palette: CommandPaletteTheme,
    combo_box: ComboBoxTheme,
    text_input: TextInputTheme,
    tooltip: TooltipTheme,
    modal: ModalTheme,
    floating: Option<FloatingSurfaceTheme>,
    floating_controls: Option<SurfaceControlThemes>,
    title_bar_controls: Option<SurfaceControlThemes>,
    panel_controls: Option<SurfaceControlThemes>,
    card_controls: Option<SurfaceControlThemes>,
}

impl gpui::Global for ControlThemeCatalog {}

/// Application-owned border paints for an ordinary control's interaction states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlBorderStates {
    pub(crate) normal: gpui::Rgba,
    pub(crate) hovered: gpui::Rgba,
    pub(crate) pressed: gpui::Rgba,
    pub(crate) disabled: gpui::Rgba,
}

impl ControlBorderStates {
    /// Creates border paints without changing the control's focus indicator or geometry.
    pub fn new(
        normal: gpui::Rgba,
        hovered: gpui::Rgba,
        pressed: gpui::Rgba,
        disabled: gpui::Rgba,
    ) -> Self {
        Self {
            normal,
            hovered,
            pressed,
            disabled,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct InstalledControlThemeCatalogs {
    active: Box<ControlThemeCatalog>,
    inactive: Box<ControlThemeCatalog>,
    settings_active: Option<Box<ControlThemeCatalog>>,
    settings_inactive: Option<Box<ControlThemeCatalog>>,
}

impl gpui::Global for InstalledControlThemeCatalogs {}

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

/// A paired catalog replacement did not preserve one appearance generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlThemeCatalogPairError {
    /// Reusable controls must be initialized before their catalogs can be replaced.
    NotInitialized,
    /// Active and inactive variants must describe the same application appearance revision.
    GenerationMismatch,
}

impl std::fmt::Display for ControlThemeCatalogPairError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInitialized => formatter.write_str("reusable controls are not initialized"),
            Self::GenerationMismatch => formatter
                .write_str("active and inactive control catalogs have different generations"),
        }
    }
}

impl std::error::Error for ControlThemeCatalogPairError {}

impl ControlThemeCatalog {
    /// Applies one ordinary-control elevation policy to Buttons and ComboBox triggers on every
    /// prepared material host.
    pub fn ordinary_control_elevation(
        mut self,
        shadow: ControlShadow,
        border: Option<gpui::Rgba>,
    ) -> Self {
        self.button = self.button.secondary_elevation(shadow, border);
        self.combo_box = self.combo_box.ordinary_elevation(shadow, border);
        for host in [
            &mut self.title_bar_controls,
            &mut self.panel_controls,
            &mut self.card_controls,
            &mut self.floating_controls,
        ]
        .into_iter()
        .flatten()
        {
            *host = host.clone().ordinary_control_elevation(shadow, border);
        }
        self
    }

    /// Applies ordinary-control state borders on every prepared host.
    pub fn ordinary_control_borders(mut self, borders: ControlBorderStates) -> Self {
        self.button = self.button.secondary_borders(borders);
        self.combo_box = self.combo_box.ordinary_borders(borders);
        for host in [
            &mut self.title_bar_controls,
            &mut self.panel_controls,
            &mut self.card_controls,
            &mut self.floating_controls,
        ]
        .into_iter()
        .flatten()
        {
            *host = host.clone().ordinary_control_borders(borders);
        }
        self
    }

    /// Applies the toggle and segmented-control subpart elevation policy on every prepared host.
    pub fn toggle_segmented_elevation(
        mut self,
        track_shadow: ControlShadow,
        track_border: Option<gpui::Rgba>,
        thumb_shadow: ControlShadow,
        thumb_border: Option<gpui::Rgba>,
    ) -> Self {
        self.toggle = self
            .toggle
            .elevation(track_shadow, track_border, thumb_shadow, thumb_border);
        self.segmented_control =
            self.segmented_control
                .track_elevation(track_shadow, track_border, track_border);
        for host in [
            &mut self.title_bar_controls,
            &mut self.panel_controls,
            &mut self.card_controls,
            &mut self.floating_controls,
        ]
        .into_iter()
        .flatten()
        {
            *host = host.clone().toggle_segmented_elevation(
                track_shadow,
                track_border,
                thumb_shadow,
                thumb_border,
            );
        }
        self
    }

    /// Creates the complete catalog required by [`init`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded catalog has one required entry for each reusable control family"
    )]
    pub fn new(
        button: ButtonTheme,
        toggle: ToggleTheme,
        progress: ProgressTheme,
        scrollbar: ScrollbarTheme,
        resize_handle: ResizeHandleTheme,
        segmented_control: SegmentedControlTheme,
        search_field: SearchFieldTheme,
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
            progress,
            scrollbar,
            resize_handle,
            segmented_control,
            search_field,
            menu,
            command_palette,
            combo_box,
            text_input,
            tooltip,
            modal,
            floating: None,
            floating_controls: None,
            title_bar_controls: None,
            panel_controls: None,
            card_controls: None,
        }
    }

    /// Installs the complete in-window floating presentation shared by every surface family.
    ///
    /// `surfaces` owns material, hairlines, geometry, elevation, and layering for each semantic
    /// role. `controls` is the same reusable control catalog resolved against the raised material,
    /// so a button or a field nested in a menu, palette, dialog, or Pane notice composes against
    /// the surface it rests on instead of against the window root.
    pub fn floating(
        mut self,
        surfaces: FloatingSurfaceTheme,
        controls: SurfaceControlThemes,
    ) -> Self {
        self.floating = Some(surfaces);
        self.floating_controls = Some(controls);
        self
    }

    /// Sets controls compiled against the actual resting panel and card materials.
    ///
    /// These bundles share the catalog's generation and metric scaling. Their hosts add no
    /// surface effects; callers still paint each panel or card exactly once.
    pub fn resting_controls(
        mut self,
        panel: SurfaceControlThemes,
        card: SurfaceControlThemes,
    ) -> Self {
        self.panel_controls = Some(panel);
        self.card_controls = Some(card);
        self
    }

    /// Sets controls compiled against the title bar's actual material.
    ///
    /// The bundle changes only controls inside an explicit [`ControlHost::TitleBar`] scope. If it
    /// is omitted, that scope uses the root Window presentation so existing applications retain
    /// their current behavior.
    pub fn title_bar_controls(mut self, controls: SurfaceControlThemes) -> Self {
        self.title_bar_controls = Some(controls);
        self
    }

    /// Returns the resolved override for one material host.
    ///
    /// Window controls use the root family themes, so Window returns `None`. An omitted host
    /// bundle also returns `None` and falls back to those root themes, never an enclosing host.
    pub fn hosted_controls(&self, host: ControlHost) -> Option<&SurfaceControlThemes> {
        match host {
            ControlHost::Window => None,
            ControlHost::TitleBar => self.title_bar_controls.as_ref(),
            ControlHost::Panel => self.panel_controls.as_ref(),
            ControlHost::Card => self.card_controls.as_ref(),
            ControlHost::Floating => self.floating_controls.as_ref(),
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

    /// Sets the shared focus-ring width without changing control borders, gaps, or radii.
    ///
    /// The width is a stable logical-point metric and does not participate in density scaling.
    pub fn focus_ring_width(mut self, width: gpui::Pixels) -> Self {
        self.button = self.button.focus_ring_width(width);
        self.toggle = self.toggle.focus_ring_width(width);
        self.segmented_control = self.segmented_control.focus_ring_width(width);
        self.search_field = self.search_field.focus_ring_width(width);
        self.text_input = self.text_input.focus_ring_width(width);
        self.menu = self.menu.focus_ring_width(width);
        self.combo_box = self.combo_box.focus_ring_width(width);
        self.floating_controls = self
            .floating_controls
            .map(|themes| themes.focus_ring_width(width));
        self.title_bar_controls = self
            .title_bar_controls
            .map(|themes| themes.focus_ring_width(width));
        self.panel_controls = self
            .panel_controls
            .map(|themes| themes.focus_ring_width(width));
        self.card_controls = self
            .card_controls
            .map(|themes| themes.focus_ring_width(width));
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
        self.progress = self.progress.scaled_metrics(text_scale, spacing_scale);
        self.scrollbar = self.scrollbar.scaled_metrics(text_scale, spacing_scale);
        self.resize_handle = self.resize_handle.scaled_metrics(text_scale, spacing_scale);
        self.segmented_control = self
            .segmented_control
            .scaled_metrics(text_scale, spacing_scale);
        self.search_field = self.search_field.scaled_metrics(text_scale, spacing_scale);
        self.menu = self.menu.scaled_metrics(text_scale, spacing_scale);
        self.command_palette = self
            .command_palette
            .scaled_metrics(text_scale, spacing_scale);
        self.combo_box = self.combo_box.scaled_metrics(text_scale, spacing_scale);
        self.text_input = self.text_input.scaled_metrics(text_scale, spacing_scale);
        self.tooltip = self.tooltip.scaled_metrics(text_scale, spacing_scale);
        self.modal = self.modal.scaled_metrics(text_scale, spacing_scale);
        self.floating = self
            .floating
            .map(|floating| floating.scaled_metrics(text_scale, spacing_scale));
        self.floating_controls = self
            .floating_controls
            .map(|controls| controls.scale_metrics(text_scale, spacing_scale));
        self.title_bar_controls = self
            .title_bar_controls
            .map(|controls| controls.scale_metrics(text_scale, spacing_scale));
        self.panel_controls = self
            .panel_controls
            .map(|controls| controls.scale_metrics(text_scale, spacing_scale));
        self.card_controls = self
            .card_controls
            .map(|controls| controls.scale_metrics(text_scale, spacing_scale));
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
    let catalog = Box::new(catalog);
    install_control_theme_catalogs(cx, catalog.clone(), catalog, None);
    initialize_control_state(cx);
    Ok(())
}

/// Installs the application and Settings Window presentation variants and initializes
/// control-owned state.
///
/// Heap-owned catalogs keep the complete scoped catalog set out of the caller's stack frame.
pub fn init_scoped_control_theme_catalogs(
    cx: &mut App,
    active: Box<ControlThemeCatalog>,
    inactive: Box<ControlThemeCatalog>,
    settings_active: Box<ControlThemeCatalog>,
    settings_inactive: Box<ControlThemeCatalog>,
) -> gpui::Result<()> {
    let generation = active.generation;
    if [
        inactive.generation,
        settings_active.generation,
        settings_inactive.generation,
    ]
    .into_iter()
    .any(|candidate| candidate != generation)
    {
        return Err(ControlThemeCatalogPairError::GenerationMismatch.into());
    }
    icon::register_font(cx)?;
    install_control_theme_catalogs(
        cx,
        active,
        inactive,
        Some((settings_active, settings_inactive)),
    );
    initialize_control_state(cx);
    Ok(())
}

fn initialize_control_state(cx: &mut App) {
    button::init(cx);
    text_input::init(cx);
    menu::init(cx);
    command_palette::init(cx);
    combo_box::init(cx);
    tooltip::init(cx);
    modal::init_core(cx);
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
    if !cx.has_global::<InstalledControlThemeCatalogs>() {
        return Err(ControlThemeReplacementError);
    }
    let installed = cx.global::<InstalledControlThemeCatalogs>();
    if installed.active.as_ref() == &catalog
        && installed.inactive.as_ref() == &catalog
        && installed.settings_active.is_none()
        && installed.settings_inactive.is_none()
    {
        return Ok(ControlThemeReplacement::Unchanged);
    }
    let catalog = Box::new(catalog);
    install_control_theme_catalogs(cx, catalog.clone(), catalog, None);
    cx.refresh_windows();
    Ok(ControlThemeReplacement::Applied)
}

/// Atomically replaces the active and inactive reusable-control presentation variants.
///
/// Both catalogs must carry the same application-issued generation. Existing entities retain
/// their interaction state, while every Operating-System Window selects its own immutable variant
/// through [`ControlWindowActivity`].
pub fn replace_control_theme_catalogs(
    cx: &mut App,
    active: ControlThemeCatalog,
    inactive: ControlThemeCatalog,
) -> Result<ControlThemeReplacement, ControlThemeCatalogPairError> {
    if active.generation != inactive.generation {
        return Err(ControlThemeCatalogPairError::GenerationMismatch);
    }
    if !cx.has_global::<InstalledControlThemeCatalogs>() {
        return Err(ControlThemeCatalogPairError::NotInitialized);
    }
    let installed = cx.global::<InstalledControlThemeCatalogs>();
    if installed.active.as_ref() == &active
        && installed.inactive.as_ref() == &inactive
        && installed.settings_active.is_none()
        && installed.settings_inactive.is_none()
    {
        return Ok(ControlThemeReplacement::Unchanged);
    }
    install_control_theme_catalogs(cx, Box::new(active), Box::new(inactive), None);
    cx.refresh_windows();
    Ok(ControlThemeReplacement::Applied)
}

/// Atomically replaces the application and Settings Window presentation variants.
///
/// The Settings pair is selected only inside an explicit [`ControlThemeScope::Settings`] scope;
/// every other window continues to use the application pair.
pub fn replace_scoped_control_theme_catalogs(
    cx: &mut App,
    active: Box<ControlThemeCatalog>,
    inactive: Box<ControlThemeCatalog>,
    settings_active: Box<ControlThemeCatalog>,
    settings_inactive: Box<ControlThemeCatalog>,
) -> Result<ControlThemeReplacement, ControlThemeCatalogPairError> {
    let generation = active.generation;
    if [
        inactive.generation,
        settings_active.generation,
        settings_inactive.generation,
    ]
    .into_iter()
    .any(|candidate| candidate != generation)
    {
        return Err(ControlThemeCatalogPairError::GenerationMismatch);
    }
    if !cx.has_global::<InstalledControlThemeCatalogs>() {
        return Err(ControlThemeCatalogPairError::NotInitialized);
    }
    let installed = cx.global::<InstalledControlThemeCatalogs>();
    if installed.active == active
        && installed.inactive == inactive
        && installed.settings_active.as_ref() == Some(&settings_active)
        && installed.settings_inactive.as_ref() == Some(&settings_inactive)
    {
        return Ok(ControlThemeReplacement::Unchanged);
    }
    install_control_theme_catalogs(
        cx,
        active,
        inactive,
        Some((settings_active, settings_inactive)),
    );
    cx.refresh_windows();
    Ok(ControlThemeReplacement::Applied)
}

fn install_control_theme_catalogs(
    cx: &mut App,
    active: Box<ControlThemeCatalog>,
    inactive: Box<ControlThemeCatalog>,
    settings: Option<(Box<ControlThemeCatalog>, Box<ControlThemeCatalog>)>,
) {
    debug_assert_eq!(active.generation, inactive.generation);
    let (settings_active, settings_inactive) = settings.unzip();
    cx.set_global(active.button);
    cx.set_global(active.toggle);
    cx.set_global(active.progress);
    cx.set_global(active.scrollbar);
    cx.set_global(active.resize_handle);
    cx.set_global(active.segmented_control);
    cx.set_global(active.search_field);
    cx.set_global(active.menu);
    cx.set_global(active.command_palette);
    cx.set_global(active.combo_box);
    cx.set_global(active.text_input);
    cx.set_global(active.tooltip);
    cx.set_global(active.modal);
    cx.set_global(active.floating.unwrap_or_default());
    cx.set_global(active.as_ref().clone());
    cx.set_global(InstalledControlThemeCatalogs {
        active,
        inactive,
        settings_active,
        settings_inactive,
    });
}

/// Restates a control's complete text style inside an interaction refinement.
///
/// GPUI replaces an element's text style wholesale when a hover, active, or group refinement
/// carries one, rather than merging field by field. A refinement that set only a color would
/// therefore drop the element's font, size, and line height, which reflows the row under the
/// pointer. Callers pass what their base style already uses.
pub(crate) fn refine_control_text(
    style: gpui::StyleRefinement,
    font: &gpui::Font,
    size: gpui::Pixels,
    line_height: f32,
    color: gpui::Rgba,
) -> gpui::StyleRefinement {
    use gpui::Styled as _;
    style
        .font(font.clone())
        .text_size(size)
        .line_height(gpui::relative(line_height))
        .text_color(color)
}

pub(crate) fn control_theme_catalog(cx: &App) -> Option<&ControlThemeCatalog> {
    cx.try_global::<InstalledControlThemeCatalogs>()
        .map(|catalogs| {
            let settings = match floating_surface::current_window_activity() {
                ControlWindowActivity::Active => catalogs.settings_active.as_ref(),
                ControlWindowActivity::Inactive => catalogs.settings_inactive.as_ref(),
            };
            let catalog =
                if floating_surface::current_control_theme_scope() == ControlThemeScope::Settings {
                    settings.unwrap_or_else(|| match floating_surface::current_window_activity() {
                        ControlWindowActivity::Active => &catalogs.active,
                        ControlWindowActivity::Inactive => &catalogs.inactive,
                    })
                } else {
                    match floating_surface::current_window_activity() {
                        ControlWindowActivity::Active => &catalogs.active,
                        ControlWindowActivity::Inactive => &catalogs.inactive,
                    }
                };
            catalog.as_ref()
        })
        .or_else(|| cx.try_global::<ControlThemeCatalog>())
}

fn control_typography(cx: &App) -> ControlTypography {
    control_theme_catalog(cx)
        .map(|catalog| catalog.typography.clone())
        .unwrap_or_default()
}
