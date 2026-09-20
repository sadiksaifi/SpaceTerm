//! One presentation system for every surface that floats over window content.
//!
//! A floating surface sits over content that is already painted. Its shared shell filters spatial
//! detail when requested, constrains the backdrop's straight color, admits the native window
//! backing when available, and adds a small host-relative elevation wash. Every floating family
//! selects a semantic [`FloatingRole`] and receives the complete treatment for it: backdrop tone,
//! coverage limit, wash, filter, outer edge, internal divider, corner geometry, content inset,
//! elevation, clipping, and window layer. No call site chooses its own alpha, blur, radius, border,
//! separator, or shadow.
//!
//! Controls nested inside a floating surface resolve against that surface rather than against the
//! window root. [`FloatingShell::mount`] enters a host scope for the complete lifetime of the
//! surface's descendants, covering layout, prepaint, and paint, so retained entities and custom
//! elements resolve their own presentation correctly rather than depending on a render-time swap.
//! Content that defers carries its own surface, which re-enters the scope inside the deferred draw.

use std::cell::Cell;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Rgba, Styled, Window, deferred, hsla, px, rgba,
};

use crate::{
    ButtonTheme, ComboBoxTheme, ControlShadow, ControlShadowLayer, MenuTheme, ProgressTheme,
    SearchFieldTheme, SegmentedControlTheme, TextInputTheme, ToggleTheme,
    appearance::normalized_scale,
};

/// The deferred priority of a tooltip, which stays below every interactive surface.
const TOOLTIP_PRIORITY: usize = 0;
/// The deferred priority of an anchored popup.
///
/// Anchored popups share one priority. A surface hosting another popup draws normally so GPUI
/// never needs to enqueue a deferred draw from inside an existing deferred draw.
const POPOVER_PRIORITY: usize = 1;

/// What one floating surface is for.
///
/// The roles share one material family and one hairline language. They differ in the geometry and
/// elevation their purpose asks for: a transient anchored popup is smaller and closer to the
/// content it came from than a focal command surface, and a quiet readout carries less of both than
/// a panel that takes input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatingRole {
    /// A transient surface anchored to the control that opened it: menus, context menus, pickers,
    /// and the ComboBox popup.
    Popover,
    /// A focal search surface that takes the center of the window, such as the Command Palette.
    Command,
    /// A window-modal alert or dialog.
    Modal,
    /// Short supplementary text attached to a control.
    Tooltip,
    /// A Pane-local panel that accepts pointer or keyboard input.
    Notice,
    /// A Pane-local readout that reports without accepting input.
    Readout,
}

impl FloatingRole {
    /// The unscaled corner radius and content inset of this role.
    ///
    /// The inset is the distance from the surface edge to the rows or controls resting directly on
    /// it, and gives every nested row its concentric radius.
    const fn geometry(self) -> (f32, f32) {
        match self {
            Self::Popover => (10.0, 4.0),
            Self::Command => (12.0, 6.0),
            Self::Modal => (14.0, 0.0),
            Self::Tooltip => (7.0, 0.0),
            Self::Notice => (10.0, 4.0),
            Self::Readout => (8.0, 0.0),
        }
    }

    /// Whether this role paints the window's raised material or its quiet readout material.
    const fn quiet_material(self) -> bool {
        matches!(self, Self::Readout)
    }

    /// The semantic elevation of this role, from one scheme shadow ink.
    ///
    /// Elevation states how far a surface is from the content it covers. A readout barely leaves
    /// the Pane, a tooltip floats just above its control, anchored popups and Pane notices share
    /// one lift, and command and modal surfaces take the window.
    fn elevation(self, ink: Hsla) -> ControlShadow {
        let layer = |weight: f32, y: f32, blur: f32, spread: f32| {
            ControlShadowLayer::new(
                Hsla {
                    a: (ink.a * weight).clamp(0.0, 1.0),
                    ..ink
                },
                px(0.0),
                px(y),
                px(blur),
                px(spread),
            )
        };
        match self {
            Self::Readout => ControlShadow::single(layer(0.55, 1.0, 3.0, -1.0)),
            Self::Tooltip => ControlShadow::single(layer(0.75, 2.0, 6.0, -2.0)),
            Self::Popover | Self::Notice => {
                ControlShadow::double(layer(1.0, 6.0, 16.0, -4.0), layer(0.55, 2.0, 4.0, -2.0))
            }
            Self::Command | Self::Modal => {
                ControlShadow::double(layer(1.0, 16.0, 36.0, -10.0), layer(0.65, 4.0, 10.0, -4.0))
            }
        }
    }

    /// The window layer this role reaches.
    ///
    /// GPUI collects every deferred draw once per frame and refuses to enqueue another while it is
    /// processing them, so a surface that currently hosts another floating surface must draw as an
    /// ordinary last child and let the surface it hosts defer above it.
    pub fn layer(self, hosts_nested_surface: bool) -> FloatingLayer {
        if hosts_nested_surface {
            return FloatingLayer::Normal;
        }
        match self {
            Self::Tooltip => FloatingLayer::Deferred(TOOLTIP_PRIORITY),
            Self::Popover => FloatingLayer::Deferred(POPOVER_PRIORITY),
            Self::Command | Self::Modal | Self::Notice | Self::Readout => FloatingLayer::Normal,
        }
    }
}

/// The layer one floating surface reaches in its Operating-System Window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatingLayer {
    /// An ordinary child painted in tree order, which may host a deferred surface of its own.
    Normal,
    /// A deferred draw at the given priority, painted above ordinary window content.
    Deferred(usize),
}

/// Presents one already-positioned floating surface on the layer its role reaches.
pub(crate) fn present(layer: FloatingLayer, surface: impl IntoElement) -> AnyElement {
    match layer {
        FloatingLayer::Normal => surface.into_any_element(),
        FloatingLayer::Deferred(priority) => {
            deferred(surface).with_priority(priority).into_any_element()
        }
    }
}

/// One resolved floating backdrop tone, elevation wash, and pair of hairlines.
///
/// `edge` bounds the surface against arbitrary content beneath it. `divider` is the quieter rule
/// that groups content inside the surface, and is deliberately a different strength: an outer
/// boundary and an internal separator do not say the same thing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatingSurfacePaint {
    material: Rgba,
    backdrop_tone: Rgba,
    edge: Rgba,
    divider: Rgba,
}

impl FloatingSurfacePaint {
    /// Creates one complete floating paint, initially without backdrop color treatment.
    pub fn new(material: Rgba, edge: Rgba, divider: Rgba) -> Self {
        Self {
            material,
            backdrop_tone: Rgba::default(),
            edge,
            divider,
        }
    }

    /// Adds alpha-preserving color treatment for already-painted content behind the surface.
    pub fn backdrop_tone(mut self, tone: Rgba) -> Self {
        self.backdrop_tone = tone;
        self
    }
}

/// The complete set of treatments the window's floating surfaces apply.
///
/// Interactive floating surfaces share one raised treatment, which makes them read as one system.
/// A Pane-local readout is the single exception: it reports rather than accepts input, and the
/// application authors it as its own quieter product color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatingSurfacePaints {
    raised: FloatingSurfacePaint,
    readout: FloatingSurfacePaint,
}

impl FloatingSurfacePaints {
    /// Creates the complete bounded material catalog.
    pub fn new(raised: FloatingSurfacePaint, readout: FloatingSurfacePaint) -> Self {
        Self { raised, readout }
    }
}

/// The application-installed presentation of every in-window floating surface.
///
/// The application resolves the window's material, hairlines, shadow ink, and scrim once. Geometry,
/// elevation, clipping, and layering belong to this Module, so a role means the same thing in every
/// family.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatingSurfaceTheme {
    paints: FloatingSurfacePaints,
    shadow_ink: Hsla,
    scrim: Rgba,
    backdrop_blur: Pixels,
    backdrop_alpha_limit: f32,
    spacing_scale: f32,
}

impl FloatingSurfaceTheme {
    /// Creates the complete floating presentation from resolved application colors.
    pub fn new(paints: FloatingSurfacePaints, shadow_ink: Hsla, scrim: Rgba) -> Self {
        Self {
            paints,
            shadow_ink,
            scrim,
            backdrop_blur: px(0.0),
            backdrop_alpha_limit: 1.0,
            spacing_scale: 1.0,
        }
    }

    /// Requests one logical-pixel Gaussian sigma for every shell in this catalog.
    pub fn backdrop_blur(mut self, radius: Pixels) -> Self {
        self.backdrop_blur = radius.max(px(0.0));
        self
    }

    /// Limits already-painted framebuffer coverage beneath every floating shell.
    ///
    /// Reducing the limit reveals the Operating-System Window backing while retaining the
    /// filtered content's straight color. Invalid values preserve the framebuffer.
    pub fn backdrop_alpha_limit(mut self, limit: f32) -> Self {
        self.backdrop_alpha_limit = if limit.is_finite() {
            limit.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self
    }

    /// The scrim painted beneath a window-modal surface.
    pub fn scrim(&self) -> Rgba {
        self.scrim
    }

    /// The complete resolved presentation of one semantic role.
    pub fn shell(&self, role: FloatingRole) -> FloatingShell {
        let (radius, inset) = role.geometry();
        let scale = normalized_scale(self.spacing_scale);
        let paint = if role.quiet_material() {
            self.paints.readout
        } else {
            self.paints.raised
        };
        FloatingShell {
            role,
            paint,
            elevation: role.elevation(self.shadow_ink),
            backdrop_blur: self.backdrop_blur,
            backdrop_alpha_limit: self.backdrop_alpha_limit,
            corner_radius: px(radius * scale),
            content_inset: px(inset * scale),
        }
    }

    pub(crate) fn scaled_metrics(self, _text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            spacing_scale: self.spacing_scale * normalized_scale(spacing_scale),
            ..self
        }
    }
}

impl Default for FloatingSurfaceTheme {
    /// A neutral legible fallback for fixtures that install control families directly.
    ///
    /// Production windows always install a resolved catalog.
    fn default() -> Self {
        let raised =
            FloatingSurfacePaint::new(rgba(0x1f2023f2), rgba(0xffffff26), rgba(0xffffff14));
        Self::new(
            FloatingSurfacePaints::new(raised, raised),
            hsla(0.0, 0.0, 0.0, 0.28),
            rgba(0x00000066),
        )
    }
}

impl gpui::Global for FloatingSurfaceTheme {}

/// Returns the installed floating presentation, or the bounded fallback.
pub(crate) fn floating_theme(cx: &App) -> FloatingSurfaceTheme {
    cx.try_global::<FloatingSurfaceTheme>()
        .copied()
        .unwrap_or_default()
}

/// Returns the complete resolved presentation of one role.
pub(crate) fn shell(role: FloatingRole, cx: &App) -> FloatingShell {
    floating_theme(cx).shell(role)
}

/// The complete resolved presentation of one floating surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatingShell {
    role: FloatingRole,
    paint: FloatingSurfacePaint,
    elevation: ControlShadow,
    backdrop_blur: Pixels,
    backdrop_alpha_limit: f32,
    corner_radius: Pixels,
    content_inset: Pixels,
}

impl FloatingShell {
    /// The semantic role this shell resolves.
    pub fn role(&self) -> FloatingRole {
        self.role
    }

    /// The surface's outer corner radius.
    pub fn corner_radius(&self) -> Pixels {
        self.corner_radius
    }

    /// The distance from the surface edge to the rows and controls resting on it.
    pub fn content_inset(&self) -> Pixels {
        self.content_inset
    }

    /// The Gaussian sigma used to filter already-painted content beneath this shell.
    pub fn backdrop_blur_radius(&self) -> Pixels {
        self.backdrop_blur
    }

    /// Maximum framebuffer coverage retained beneath this shell.
    pub fn backdrop_alpha_limit(&self) -> f32 {
        self.backdrop_alpha_limit
    }

    /// The concentric radius of a row or control inset directly inside this surface.
    pub fn nested_radius(&self) -> Pixels {
        (self.corner_radius - self.content_inset).max(px(0.0))
    }

    /// The stable hairline every floating surface and its internal rules paint.
    ///
    /// Hairlines do not scale: a separator that thickens with the interface size stops reading as
    /// a rule and starts reading as a band.
    pub fn hairline(&self) -> Pixels {
        px(1.0)
    }

    /// The small host-relative wash that lifts the shell above its backdrop.
    pub fn material(&self) -> Rgba {
        self.paint.material
    }

    /// The color range applied to already-painted backdrop pixels without adding coverage.
    pub fn backdrop_tone(&self) -> Rgba {
        self.paint.backdrop_tone
    }

    /// The hairline bounding the surface against the content beneath it.
    pub fn edge(&self) -> Rgba {
        self.paint.edge
    }

    /// The quieter rule that groups content inside the surface.
    pub fn divider(&self) -> Rgba {
        self.paint.divider
    }

    /// The window layer this surface reaches.
    pub fn layer(&self, hosts_nested_surface: bool) -> FloatingLayer {
        self.role.layer(hosts_nested_surface)
    }

    /// Applies the complete surface treatment to a caller's frame.
    ///
    /// The caller keeps placement, size, and behavior; material, edge, corners, elevation, and
    /// clipping belong to the role.
    pub fn frame<E: Styled>(&self, frame: E) -> E {
        let frame = frame
            .overflow_hidden()
            .rounded(self.corner_radius)
            .border(self.hairline())
            .border_color(self.edge())
            .backdrop_alpha_limit(self.backdrop_alpha_limit);
        let frame = frame.backdrop_tone(self.backdrop_tone());
        let frame = if self.backdrop_blur > px(0.0) {
            frame.backdrop_blur(self.backdrop_blur)
        } else {
            frame
        };
        frame
            .bg(self.material())
            .shadow(self.elevation.layers())
            .shadow_outside_only()
    }

    /// Applies the surface treatment and hosts every descendant control on this surface.
    pub fn mount(&self, frame: impl Styled + IntoElement) -> ControlHostElement {
        ControlHost::Floating.mount(self.frame(frame))
    }

    /// Hosts descendant controls on this surface without painting a shell.
    ///
    /// Reserved for the layers that deliberately carry no surface of their own but still own the
    /// controls beneath them.
    pub fn host(&self, content: impl IntoElement) -> ControlHostElement {
        ControlHost::Floating.mount(content)
    }
}

/// The material host used to resolve descendant control presentation.
///
/// These roles select already-compiled paints, not surface effects. Panel and Card describe
/// resting surfaces over the window sheet; Floating describes a floating shell. The nearest
/// explicit host wins. A deferred popup must carry its own host into the deferred draw.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ControlHost {
    /// The window sheet and its root control themes.
    #[default]
    Window,
    /// A resting navigation or supporting panel.
    Panel,
    /// A resting card above the window's page surface.
    Card,
    /// A popup, dialog, tooltip, or Pane notice with a floating shell.
    Floating,
}

impl ControlHost {
    pub(crate) fn button_theme(self, cx: &App) -> &ButtonTheme {
        cx.try_global::<crate::ControlThemeCatalog>()
            .and_then(|catalog| catalog.hosted_controls(self))
            .map_or_else(|| cx.global::<ButtonTheme>(), |themes| &themes.button)
    }

    pub(crate) fn toggle_theme(self, cx: &App) -> &ToggleTheme {
        cx.try_global::<crate::ControlThemeCatalog>()
            .and_then(|catalog| catalog.hosted_controls(self))
            .map_or_else(|| cx.global::<ToggleTheme>(), |themes| &themes.toggle)
    }

    /// Selects this host for layout, prepaint, and paint without drawing a surface.
    ///
    /// The caller owns the actual host material. Controls must resolve their themes inside
    /// these phases, rather than eagerly constructing themed frames before mounting them.
    /// This wrapper paints no fill, edge, shadow, or blur and does not alter interaction state.
    pub fn mount(self, content: impl IntoElement) -> ControlHostElement {
        ControlHostElement {
            content: content.into_any_element(),
            host: self,
        }
    }
}

thread_local! {
    static CURRENT_CONTROL_HOST: Cell<ControlHost> = const { Cell::new(ControlHost::Window) };
}

struct ControlHostGuard {
    previous: ControlHost,
}

impl Drop for ControlHostGuard {
    fn drop(&mut self) {
        CURRENT_CONTROL_HOST.with(|current| current.set(self.previous));
    }
}

fn enter_host<R>(host: ControlHost, work: impl FnOnce() -> R) -> R {
    let previous = CURRENT_CONTROL_HOST.with(|current| current.replace(host));
    let _guard = ControlHostGuard { previous };
    work()
}

/// Selects descendant control paints for the current material host without adding a surface.
pub struct ControlHostElement {
    content: AnyElement,
    host: ControlHost,
}

impl IntoElement for ControlHostElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ControlHostElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let host = self.host;
        let content = &mut self.content;
        (enter_host(host, || content.request_layout(window, cx)), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let host = self.host;
        let content = &mut self.content;
        enter_host(host, || content.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let host = self.host;
        let content = &mut self.content;
        enter_host(host, || content.paint(window, cx));
    }
}

/// The presentation ordinary controls take while they rest on one material host.
///
/// A control's fill is authored as a difference from the surface beneath it. On the window root
/// that difference is composed against the root; on a panel, card, or floating surface it must be
/// composed against that actual host. The application publishes all host bundles in one catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceControlThemes {
    button: ButtonTheme,
    toggle: ToggleTheme,
    progress: ProgressTheme,
    segmented_control: SegmentedControlTheme,
    search_field: SearchFieldTheme,
    text_input: TextInputTheme,
    menu: Option<MenuTheme>,
    combo_box: Option<ComboBoxTheme>,
}

impl SurfaceControlThemes {
    /// Creates the complete catalog of host-relative control presentation.
    pub fn new(
        button: ButtonTheme,
        toggle: ToggleTheme,
        progress: ProgressTheme,
        segmented_control: SegmentedControlTheme,
        search_field: SearchFieldTheme,
        text_input: TextInputTheme,
    ) -> Self {
        Self {
            button,
            toggle,
            progress,
            segmented_control,
            search_field,
            text_input,
            menu: None,
            combo_box: None,
        }
    }

    /// Sets host-relative trigger paints without changing popup presentation.
    pub fn triggers(mut self, menu: MenuTheme, combo_box: ComboBoxTheme) -> Self {
        self.menu = Some(menu);
        self.combo_box = Some(combo_box);
        self
    }

    pub(crate) fn scale_metrics(mut self, text_scale: f32, spacing_scale: f32) -> Self {
        self.button = self.button.scaled_metrics(text_scale, spacing_scale);
        self.toggle = self.toggle.scaled_metrics(text_scale, spacing_scale);
        self.progress = self.progress.scaled_metrics(text_scale, spacing_scale);
        self.segmented_control = self
            .segmented_control
            .scaled_metrics(text_scale, spacing_scale);
        self.search_field = self.search_field.scaled_metrics(text_scale, spacing_scale);
        self.text_input = self.text_input.scaled_metrics(text_scale, spacing_scale);
        self.menu = self
            .menu
            .map(|theme| theme.scaled_metrics(text_scale, spacing_scale));
        self.combo_box = self
            .combo_box
            .map(|theme| theme.scaled_metrics(text_scale, spacing_scale));
        self
    }
}

fn hosted(cx: &App) -> Option<&SurfaceControlThemes> {
    let host = CURRENT_CONTROL_HOST.with(Cell::get);
    cx.try_global::<crate::ControlThemeCatalog>()?
        .hosted_controls(host)
}

pub(crate) fn hosted_menu_theme(cx: &App) -> Option<&MenuTheme> {
    hosted(cx).and_then(|themes| themes.menu.as_ref())
}

pub(crate) fn hosted_combo_box_theme(cx: &App) -> Option<&ComboBoxTheme> {
    hosted(cx).and_then(|themes| themes.combo_box.as_ref())
}

/// The Button presentation for the surface the control currently rests on.
pub(crate) fn hosted_button_theme(cx: &App) -> &ButtonTheme {
    CURRENT_CONTROL_HOST.with(Cell::get).button_theme(cx)
}

/// The Checkbox and Switch presentation for the surface the control currently rests on.
pub(crate) fn hosted_toggle_theme(cx: &App) -> &ToggleTheme {
    hosted(cx).map_or_else(|| cx.global::<ToggleTheme>(), |themes| &themes.toggle)
}

/// The progress presentation for the surface the control currently rests on.
pub(crate) fn hosted_progress_theme(cx: &App) -> &ProgressTheme {
    hosted(cx).map_or_else(|| cx.global::<ProgressTheme>(), |themes| &themes.progress)
}

/// The Segmented Control presentation for the surface the control currently rests on.
pub(crate) fn hosted_segmented_control_theme(cx: &App) -> Option<&SegmentedControlTheme> {
    hosted(cx).map_or_else(
        || cx.try_global::<SegmentedControlTheme>(),
        |themes| Some(&themes.segmented_control),
    )
}

/// The Search Field presentation for the surface the control currently rests on.
pub(crate) fn hosted_search_field_theme(cx: &App) -> &SearchFieldTheme {
    hosted(cx).map_or_else(
        || cx.global::<SearchFieldTheme>(),
        |themes| &themes.search_field,
    )
}

/// The editable-text presentation for the surface the control currently rests on.
pub(crate) fn hosted_text_input_theme(cx: &App) -> &TextInputTheme {
    hosted(cx).map_or_else(
        || cx.global::<TextInputTheme>(),
        |themes| &themes.text_input,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdrop_blur_is_shared_by_every_role_and_does_not_scale_with_layout_density() {
        let theme = FloatingSurfaceTheme::default().backdrop_blur(px(20.0));
        let scaled = theme.scaled_metrics(1.5, 1.25);
        for role in [
            FloatingRole::Popover,
            FloatingRole::Command,
            FloatingRole::Modal,
            FloatingRole::Tooltip,
            FloatingRole::Notice,
            FloatingRole::Readout,
        ] {
            assert_eq!(theme.shell(role).backdrop_blur_radius(), px(20.0));
            assert_eq!(scaled.shell(role).backdrop_blur_radius(), px(20.0));
        }
    }

    #[test]
    fn spacing_scale_composes_across_catalog_scaling() {
        let theme = FloatingSurfaceTheme::default();
        let scaled = theme.scaled_metrics(1.0, 1.25);
        let identity_after_scale = scaled.scaled_metrics(1.0, 1.0);
        let composed = scaled.scaled_metrics(1.0, 1.2);
        let direct = theme.scaled_metrics(1.0, 1.5);

        for role in [FloatingRole::Popover, FloatingRole::Modal] {
            assert_eq!(identity_after_scale.shell(role), scaled.shell(role));
            assert_eq!(composed.shell(role), direct.shell(role));
        }
    }
}
