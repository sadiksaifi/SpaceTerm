//! Requested native backdrop and effective presentation are independent from scheme classification.
use serde::{Deserialize, Serialize};

use super::ChromeColors;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WindowBackgroundAppearance {
    #[default]
    Opaque,
    Transparent,
    Blurred,
}

/// Independent facts that constrain native-window and in-window composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompositionCapabilities {
    pub(crate) native_window_transparency: bool,
    pub(crate) reduce_transparency: bool,
    pub(crate) increase_contrast: bool,
    pub(crate) show_borders: bool,
    pub(crate) reduce_motion: bool,
    pub(crate) differentiate_without_color: bool,
}

impl CompositionCapabilities {
    pub(crate) const fn new(
        native_window_transparency: bool,
        accessibility_allows_transparency: bool,
    ) -> Self {
        Self {
            native_window_transparency,
            reduce_transparency: !accessibility_allows_transparency,
            increase_contrast: false,
            show_borders: false,
            reduce_motion: false,
            differentiate_without_color: false,
        }
    }

    const fn accessibility_allows_transparency(self) -> bool {
        !self.reduce_transparency && !self.increase_contrast
    }
}

impl Default for CompositionCapabilities {
    fn default() -> Self {
        Self::new(false, true)
    }
}

/// The layer a painted background belongs to in the window's material hierarchy.
///
/// One window sheet admits the native backdrop. Resting surfaces add only the color difference
/// from that sheet, so nested controls do not repeatedly obscure the backdrop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SurfaceRole {
    /// The single continuous tint beneath the window content.
    Sheet,
    /// Sidebar, title bar, Tab strip, stage perimeter, Split gaps and Pane corner fillets.
    Base,
    /// Anything resting on the base without covering other content: Panes, Tab and sidebar
    /// chips, buttons, fields, steppers, toggles and rows.
    Surface,
    /// Menus, popovers, dialogs and tooltips, which filter and tint rendered content beneath them.
    Floating,
}

/// The resolved translucency of every surface role, carried as one scalar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SurfaceMaterials {
    glass: u8,
}

impl SurfaceMaterials {
    /// Every surface keeps its authored color; the window has a known opaque backing.
    pub(crate) const OPAQUE: Self = Self { glass: 0 };

    /// What a resting surface still paints at the maximum setting, and what a floating one does.
    ///
    /// The sheet is the window's transmission: one continuous tint over everything, so the
    /// maximum setting hands the desktop all of it and keeps none. A resting surface paints only
    /// its difference from that sheet, and that difference is the whole of what tells a Pane from
    /// the shell, a card from the page, or a selected chip from the row beside it. Giving it up
    /// buys a few percent more desktop and costs the window its hierarchy, so a resting surface
    /// keeps enough of its color to lift several levels off a pale desktop instead.
    ///
    const RESTING_RESIDUAL: f32 = 0.42;
    /// How strongly a floating material constrains backdrop color at maximum transparency.
    ///
    /// The treatment preserves backdrop alpha, so this strength does not cover the native window
    /// material. The same curve applies with blur on and off.
    const FLOATING_RESIDUAL: f32 = 0.70;
    /// The most coverage a floating shell adds after the window's glass has engaged.
    const FLOATING_WASH_CEILING: u8 = 20;
    /// How much already-painted in-window content a floating shell may retain at maximum
    /// transparency. The remainder exposes the effective native window backdrop.
    const FLOATING_BACKDROP_RETENTION: f32 = 0.15;
    /// The Pane backdrop's lift toward the scheme's elevated surface, reached by `GLASS_ENGAGED_AT`.
    ///
    /// Panes stay close to the base, below the brighter cards and selected controls.
    /// This limits the light tint added to the Terminal surface.
    const LIFT_BRIGHT: f32 = 0.5;
    const LIFT_DARK: f32 = 0.5;
    /// Terminal colors need a steadier backing than navigation surfaces in Dark.
    const PANE_PROTECTION_DARK: f32 = 0.5;
    /// How much more ink a dark ladder spends at the maximum setting than over its own base.
    ///
    /// A dark rung is solved as light ink over the scheme's near-black base, but the backdrop the
    /// window admits is lighter than that base, so the same overlay covers less distance and the
    /// rungs close up as the setting rises. The overlay grows with the setting instead, staying
    /// within a few percent of the ceiling so every rung still transmits nearly all its backing.
    const DARK_BACKING_GAIN: f64 = 0.5;
    /// The setting by which the window's glass behaves as glass: the Pane lift is complete and
    /// resting surfaces spend no more than their ladder ceiling. Both ease in from the opaque
    /// presentation below it, so leaving 0 moves the surfaces continuously instead of stepping.
    const GLASS_ENGAGED_AT: f32 = 0.12;

    /// The most a near-neutral resting surface may add over the window's sheet.
    ///
    /// A bright scheme needs an almost opaque white to reproduce its surfaces over its own base,
    /// and a dark scheme needs only a sliver of light ink, so the two appearances spend different
    /// amounts of paint for the same authored step. Both stay translucent.
    const LADDER_CEILING_BRIGHT: f64 = 0.45;
    const LADDER_CEILING_DARK: f64 = 0.12;
    /// The channel spread within which a color reads as a neutral rather than as a stated hue,
    /// and the spread by which it reads entirely as a hue.
    const NEUTRAL_SPREAD: f64 = 32.0;
    const STATED_SPREAD: f64 = 64.0;
    /// The distance from the base within which a neutral surface reads as one rung of the
    /// scheme's elevation ladder, and the distance by which it reads as its own color instead.
    const LADDER_NEAR: f64 = 48.0;
    const LADDER_FAR: f64 = 96.0;

    /// The setting controls transmission through the window sheet linearly.
    fn derive(transparency: f32) -> Self {
        let amount = if transparency.is_finite() {
            transparency.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            glass: (amount * 255.0).round() as u8,
        }
    }

    fn admitted(self) -> f32 {
        f32::from(self.glass) / 255.0
    }

    /// How far the window's glass has engaged: 0 at the opaque presentation, 1 from
    /// `GLASS_ENGAGED_AT` upward.
    fn engagement(self) -> f32 {
        (self.admitted() / Self::GLASS_ENGAGED_AT).min(1.0)
    }

    /// What one role keeps at the maximum setting, and how quickly it gives the rest up.
    fn transmission(role: SurfaceRole) -> (f32, f32) {
        match role {
            SurfaceRole::Sheet => (0.0, 1.0),
            SurfaceRole::Base | SurfaceRole::Surface => (Self::RESTING_RESIDUAL, 1.0),
            SurfaceRole::Floating => (Self::FLOATING_RESIDUAL, 1.0),
        }
    }

    /// The share of an authored color one role still holds at this setting.
    ///
    /// The sheet fades linearly. Resting roles approach their residual through a quadratic curve,
    /// retaining more color at high transparency with a smaller change near the default.
    /// Residuals below a half keep that curve strictly decreasing across the whole range.
    ///
    /// Floating surfaces use a linear curve because their larger residual would make the
    /// quadratic curve non-monotonic.
    fn presence(self, role: SurfaceRole) -> f32 {
        let admitted = self.admitted();
        let residual = Self::transmission(role).0;
        if matches!(role, SurfaceRole::Floating) {
            return 1.0 - (1.0 - residual) * admitted;
        }
        (1.0 - admitted) + residual * admitted * admitted
    }

    /// How much of an authored surface color survives over the window's backdrop.
    pub(crate) fn alpha(self, role: SurfaceRole) -> u8 {
        (255.0 * self.presence(role).powf(Self::transmission(role).1)).round() as u8
    }

    /// Fits one appearance's reconstruction alphas into the overlay a resting surface may spend.
    ///
    /// An alpha small enough to fit passes through untouched, so a surface authored close to the
    /// base keeps exactly the weight it asked for. The crowded top of the range is compressed
    /// into whatever is left below the ceiling, so surfaces that each need almost all the ink
    /// still land on distinct overlays instead of collapsing onto the ceiling together. The map
    /// is continuous, never increases an alpha, and preserves order.
    fn compress(alpha: f64, ceiling: f64) -> f64 {
        let knee = ceiling * (1.0 - ceiling);
        if alpha <= knee {
            alpha
        } else {
            knee + (alpha - knee) * (ceiling - knee) / (1.0 - knee)
        }
    }

    /// How far a base and a surface read as two rungs of one neutral elevation ladder rather than
    /// as two stated colors.
    ///
    /// Only a ladder is compressed: its rungs share one ink, so spending less of it keeps their
    /// order and their spacing. A saturated fill and a surface authored far from the base are
    /// each saying something the backdrop cannot say for them, and keep the alpha they need.
    /// Both judgements fade across a range rather than switching at a threshold, so retuning a
    /// palette moves the result gradually instead of stepping.
    fn ladder_membership(base: [f64; 3], target: [f64; 3]) -> f64 {
        let spread = |channels: [f64; 3]| {
            channels.into_iter().fold(f64::NEG_INFINITY, f64::max)
                - channels.into_iter().fold(f64::INFINITY, f64::min)
        };
        let distance = base
            .iter()
            .zip(target)
            .map(|(&base, target)| (base - target).abs())
            .fold(0.0_f64, f64::max);
        let fade =
            |value: f64, whole: f64, none: f64| ((none - value) / (none - whole)).clamp(0.0, 1.0);
        fade(spread(base), Self::NEUTRAL_SPREAD, Self::STATED_SPREAD)
            * fade(spread(target), Self::NEUTRAL_SPREAD, Self::STATED_SPREAD)
            * fade(distance, Self::LADDER_NEAR, Self::LADDER_FAR)
    }

    /// Resolve a fill against the opaque scheme reference. A thin overlay reconstructs the
    /// target color over that reference while retaining as much backdrop variation as possible.
    pub(crate) fn paint(
        self,
        role: SurfaceRole,
        base: super::Color,
        target: super::Color,
    ) -> super::Color {
        if self.is_opaque() || matches!(role, SurfaceRole::Sheet | SurfaceRole::Floating) {
            return target.multiply_opacity(self.alpha(role));
        }
        let b = [base.r, base.g, base.b].map(f64::from);
        let t = [target.r, target.g, target.b].map(f64::from);
        let alpha = b
            .iter()
            .zip(t)
            .map(|(&b, t)| {
                if t > b {
                    (t - b) / (255.0 - b)
                } else if t < b {
                    (b - t) / b
                } else {
                    0.0
                }
            })
            .fold(0.0_f64, f64::max);
        if alpha == 0.0 {
            return target.with_alpha(0);
        }
        let ink: [u8; 3] = std::array::from_fn(|i| {
            ((t[i] - (1.0 - alpha) * b[i]) / alpha)
                .round()
                .clamp(0.0, 255.0) as u8
        });
        // A bright scheme's surfaces each need almost opaque white to reproduce their reference
        // color, so reproducing them exactly would paint the backdrop out and, once clamped to
        // one ceiling, would paint every one of them the same. Compressing the ladder instead
        // keeps the Pane, the selected chip and the hovered row apart at every setting. Saturated
        // action and status fills retain their color strength for readable foregrounds.
        let bright = Self::is_bright(base);
        let ceiling = if bright {
            Self::LADDER_CEILING_BRIGHT
        } else {
            Self::LADDER_CEILING_DARK
        };
        let ceiling = 1.0 - (1.0 - ceiling) * f64::from(self.engagement());
        let ladder = Self::ladder_membership(b, t);
        let mut opacity = alpha + (Self::compress(alpha, ceiling) - alpha) * ladder;
        let mut retention = self.alpha(role);
        if !bright {
            // A dark rung's overlay is already a sliver of ink under the ceiling, so fading it
            // with the sheet buys no visible backdrop and costs the window its hierarchy. The
            // ladder keeps its overlay, grown to cover the lighter backdrop it now rests on;
            // saturated fills keep fading as before. Scaling every rung alike keeps their order
            // and their spacing.
            opacity *= 1.0 + Self::DARK_BACKING_GAIN * f64::from(self.admitted()) * ladder;
            retention =
                (f64::from(retention) + (255.0 - f64::from(retention)) * ladder).round() as u8;
        }
        super::Color::rgb((u32::from(ink[0]) << 16) | (u32::from(ink[1]) << 8) | u32::from(ink[2]))
            .with_alpha((opacity.min(1.0) * f64::from(target.a)).round() as u8)
            .multiply_opacity(retention)
    }

    /// Resolves a decorative edge as a host-relative overlay when glass is active.
    ///
    /// Opaque and accessibility-limited presentations retain the authored edge exactly. An
    /// explicit transparent edge also stays absent. Otherwise authored alpha is first composed
    /// into the semantic target, then reconstructed as the smallest overlay that preserves the
    /// edge's direction away from its immediate host.
    pub(crate) fn edge(self, host: super::Color, edge: super::Color) -> super::Color {
        if self.is_opaque() || edge.a == 0 {
            return edge;
        }
        edge.source_over(host).relative_overlay(host)
    }

    /// Resolves the small host-relative lift that separates a floating shell from its backdrop.
    ///
    /// Backdrop tone owns color legibility without changing framebuffer alpha. This wash carries
    /// only elevation, so nested shells cannot rebuild the dense slab that the tone replaced.
    pub(crate) fn floating_wash(self, base: super::Color, target: super::Color) -> super::Color {
        if self.is_opaque() {
            return target;
        }
        let overlay = Self { glass: u8::MAX }.paint(SurfaceRole::Surface, base, target);
        let overlay = overlay.with_alpha(overlay.a.min(Self::FLOATING_WASH_CEILING));
        let amount = f64::from(self.engagement());
        let target_alpha = f64::from(target.a) / 255.0;
        let overlay_alpha = f64::from(overlay.a) / 255.0;
        let alpha = target_alpha * (1.0 - amount) + overlay_alpha * amount;
        if alpha == 0.0 {
            return super::Color::rgba(0);
        }
        let channel = |target: u8, overlay: u8| {
            ((f64::from(target) * target_alpha * (1.0 - amount)
                + f64::from(overlay) * overlay_alpha * amount)
                / alpha)
                .round() as u8
        };
        super::Color {
            r: channel(target.r, overlay.r),
            g: channel(target.g, overlay.g),
            b: channel(target.b, overlay.b),
            a: (alpha * 255.0).round() as u8,
        }
    }

    /// Whether a base belongs to a bright scheme, whose surfaces lift with white ink that has
    /// little room left to work in, rather than to a dark one.
    fn is_bright(base: super::Color) -> bool {
        [base.r, base.g, base.b]
            .iter()
            .all(|channel| *channel >= 128)
    }

    /// How far a Pane's default backdrop over `base` moves toward the scheme's elevated surface. A
    /// near-black Terminal background that admits the desktop reads as an opening, not a surface,
    /// so the lift reaches its full value by the default setting. Zero while opaque.
    pub(super) fn elevation(self, base: super::Color) -> f32 {
        let lift = if Self::is_bright(base) {
            Self::LIFT_BRIGHT
        } else {
            Self::LIFT_DARK
        };
        self.engagement() * lift
    }

    /// Composes the Pane tint and its readability backing in one fill.
    pub(crate) fn pane_surface(
        self,
        base: super::Color,
        elevated: super::Color,
        terminal_background: super::Color,
    ) -> super::Color {
        let target = terminal_background.mix(elevated, f64::from(self.elevation(base)));
        let lift = self.paint(SurfaceRole::Surface, base, target);
        let protection = self.pane_protection(base);
        if protection == 0.0 {
            return lift;
        }
        let backing = terminal_background.multiply_opacity((protection * 255.0).round() as u8);
        lift.source_over(backing)
    }

    /// Dark backing beneath a Pane's elevation tint. Light keeps its existing material.
    fn pane_protection(self, base: super::Color) -> f32 {
        if Self::is_bright(base) {
            0.0
        } else {
            Self::PANE_PROTECTION_DARK * self.engagement()
        }
    }

    pub(crate) const fn is_opaque(self) -> bool {
        self.glass == 0
    }

    /// Maximum alpha that already-painted content may retain beneath a floating shell.
    ///
    /// This follows the effective window material, not the independently resolved floating
    /// material. An opaque or unsupported native window has no backing to reveal and therefore
    /// keeps the existing framebuffer intact.
    pub(crate) fn floating_backdrop_alpha_limit(self) -> f32 {
        1.0 - (1.0 - Self::FLOATING_BACKDROP_RETENTION) * self.admitted()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedWindowComposition {
    /// Platform capabilities captured with this resolved presentation.
    pub(crate) capabilities: CompositionCapabilities,
    pub(crate) requested: WindowBackgroundAppearance,
    /// Effective native Operating-System Window backdrop.
    pub(crate) effective: WindowBackgroundAppearance,
    /// Materials resting on the native window sheet.
    pub(crate) materials: SurfaceMaterials,
    /// In-window floating materials, independent of native-window capability.
    pub(crate) floating_materials: SurfaceMaterials,
    /// Whether floating shells filter already-painted GPUI content.
    pub(crate) floating_blur: bool,
}

impl ResolvedWindowComposition {
    pub(crate) fn resolve(
        preferences: &super::preferences::BackgroundPreferences,
        capabilities: CompositionCapabilities,
    ) -> Self {
        let requested = if preferences.blur {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Transparent
        };
        let requested_materials = SurfaceMaterials::derive(preferences.transparency);
        let accessibility_allows_transparency = capabilities.accessibility_allows_transparency();
        let floating_materials = if accessibility_allows_transparency {
            requested_materials
        } else {
            SurfaceMaterials::OPAQUE
        };
        // A window with no glass keeps its opaque backing, whatever backdrop was asked for: an
        // effect behind a fully painted window costs a backdrop for nothing.
        let native_enabled = capabilities.native_window_transparency
            && accessibility_allows_transparency
            && !requested_materials.is_opaque();
        Self {
            capabilities,
            requested,
            effective: if native_enabled {
                requested
            } else {
                WindowBackgroundAppearance::Opaque
            },
            materials: if native_enabled {
                requested_materials
            } else {
                SurfaceMaterials::OPAQUE
            },
            floating_materials,
            floating_blur: preferences.blur && !floating_materials.is_opaque(),
        }
    }
}

macro_rules! resting_fill_roles {
    ($apply:ident) => {
        $apply!(
            tab_active_background,
            tab_inactive_background,
            tab_inactive_selected_background,
            tab_active_hover_background,
            tab_hover_background,
            badge_background,
            preview_background,
            element_background,
            element_hover,
            element_active,
            element_selected,
            element_disabled,
            ghost_element_hover,
            ghost_element_active,
            ghost_element_selected,
            primary_background,
            primary_hover_background,
            primary_pressed_background,
            primary_disabled_background,
            destructive_background,
            destructive_hover_background,
            destructive_pressed_background,
            destructive_disabled_background,
            selection_background,
            selection_hover_background,
            selection_pressed_background,
            selection_disabled_background,
            row_background,
            row_hover_background,
            row_selected_background,
            row_selected_hover_background,
            toggle_on_background,
            toggle_on_hover_background,
            toggle_on_pressed_background,
            toggle_on_disabled_background,
            toggle_off_background,
            toggle_off_hover_background,
            toggle_off_pressed_background,
            toggle_off_disabled_background,
            info_background,
            success_background,
            warning_background,
            error_background,
        )
    };
}

impl ChromeColors {
    /// Canonical surface backing for the opaque foundation. Definitions retain authored RGBA;
    /// rendering and derived foregrounds use the same known root/panel/field hierarchy.
    pub(crate) fn opaque_presentation(&self) -> Self {
        self.presentation_over(self.background.with_alpha(255))
    }

    /// Resolves authored control fills against the raised host before root flattening loses
    /// their alpha. The host material is composited once; descendants inherit that reference.
    pub(crate) fn floating_presentation(&self) -> Self {
        let root = self.background.with_alpha(255);
        let host = self.elevated_surface_background.source_over(root);
        let mut paint = self.presentation_over(host);
        paint.elevated_surface_background = host;
        paint.input_background = self.input_background.source_over(host);
        paint.input_disabled_background = self.input_disabled_background.source_over(host);
        paint
    }

    /// Resolves authored control fills against one opaque semantic host.
    pub(crate) fn host_presentation(&self, host: super::Color) -> Self {
        let mut paint = self.presentation_over(host);
        paint.input_background = self.input_background.source_over(host);
        paint.input_disabled_background = self.input_disabled_background.source_over(host);
        paint
    }

    fn presentation_over(&self, root: super::Color) -> Self {
        let mut paint = self.clone();
        paint.background = root;
        paint.panel_background = self.panel_background.source_over(root);
        paint.elevated_surface_background = self.elevated_surface_background.source_over(root);
        paint.input_background = self.input_background.source_over(paint.panel_background);
        paint.input_disabled_background = self
            .input_disabled_background
            .source_over(paint.panel_background);
        if self.ghost_element_background.a != 0 {
            paint.ghost_element_background = self.ghost_element_background.source_over(root);
        }
        if self.ghost_element_disabled.a != 0 {
            paint.ghost_element_disabled = self.ghost_element_disabled.source_over(root);
        }
        macro_rules! on_root { ($($role:ident),+ $(,)?) => { $(paint.$role = self.$role.source_over(root);)+ }; }
        on_root!(title_bar_background, title_bar_inactive_background);
        resting_fill_roles!(on_root);
        paint
    }

    /// Presentation paints for a translucent window, from an opaque presentation.
    ///
    /// Background fills and their decorative edges become host-relative. Text, icons and semantic
    /// signals stay opaque, and contrast decisions keep using the opaque presentation this is
    /// derived from.
    pub(crate) fn material_presentation(&self, materials: super::SurfaceMaterials) -> Self {
        use super::SurfaceRole;
        let mut paint = self.clone();
        let surface = |role, color| materials.paint(role, self.background, color);
        let edge = |host, color| materials.edge(host, color);
        let filled_host = |fill: super::Color| {
            if fill.a == 0 { self.background } else { fill }
        };
        paint.background = surface(SurfaceRole::Base, self.background);
        paint.panel_background = surface(SurfaceRole::Base, self.panel_background);
        paint.title_bar_background = surface(SurfaceRole::Base, self.title_bar_background);
        paint.title_bar_inactive_background =
            surface(SurfaceRole::Base, self.title_bar_inactive_background);
        paint.elevated_surface_background =
            surface(SurfaceRole::Floating, self.elevated_surface_background);
        macro_rules! resting { ($($role:ident),+ $(,)?) => { $(paint.$role = surface(SurfaceRole::Surface, self.$role);)+ }; }
        resting!(
            input_background,
            input_disabled_background,
            ghost_element_background,
            ghost_element_disabled
        );
        resting_fill_roles!(resting);

        paint.border = edge(self.background, self.border);
        paint.border_variant = edge(self.background, self.border_variant);
        paint.border_disabled = edge(self.background, self.border_disabled);
        paint.resize_idle = edge(self.background, self.resize_idle);
        paint.resize_disabled = edge(self.background, self.resize_disabled);

        paint.input_border = edge(self.input_background, self.input_border);
        paint.input_disabled_border =
            edge(self.input_disabled_background, self.input_disabled_border);

        paint.element_border = edge(self.element_background, self.element_border);
        paint.element_hover_border = edge(self.element_hover, self.element_hover_border);
        paint.element_active_border = edge(self.element_active, self.element_active_border);
        paint.element_disabled_border = edge(self.element_disabled, self.element_disabled_border);

        paint.ghost_element_border = edge(
            filled_host(self.ghost_element_background),
            self.ghost_element_border,
        );
        paint.ghost_element_hover_border = edge(
            filled_host(self.ghost_element_hover),
            self.ghost_element_hover_border,
        );
        paint.ghost_element_active_border = edge(
            filled_host(self.ghost_element_active),
            self.ghost_element_active_border,
        );
        paint.ghost_element_disabled_border = edge(
            filled_host(self.ghost_element_disabled),
            self.ghost_element_disabled_border,
        );

        paint.outline_border = edge(self.element_background, self.outline_border);
        paint.outline_hover_border = edge(self.element_hover, self.outline_hover_border);
        paint.outline_pressed_border = edge(self.element_active, self.outline_pressed_border);
        paint.outline_disabled_border = edge(self.element_disabled, self.outline_disabled_border);

        paint.selection_border = edge(self.selection_background, self.selection_border);
        paint.selection_hover_border =
            edge(self.selection_hover_background, self.selection_hover_border);
        paint.selection_pressed_border = edge(
            self.selection_pressed_background,
            self.selection_pressed_border,
        );
        paint.selection_disabled_border = edge(
            self.selection_disabled_background,
            self.selection_disabled_border,
        );
        paint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{ChromeColors, Color};

    #[test]
    fn non_transparency_accessibility_capabilities_do_not_change_composition() {
        let preferences = crate::appearance::preferences::BackgroundPreferences::default();
        let baseline = ResolvedWindowComposition::resolve(
            &preferences,
            CompositionCapabilities::new(true, true),
        );
        let capabilities = CompositionCapabilities {
            native_window_transparency: true,
            reduce_transparency: false,
            increase_contrast: false,
            show_borders: true,
            reduce_motion: true,
            differentiate_without_color: true,
        };

        let resolved = ResolvedWindowComposition::resolve(&preferences, capabilities);

        assert_eq!(
            (
                resolved.requested,
                resolved.effective,
                resolved.materials,
                resolved.floating_materials,
                resolved.floating_blur,
            ),
            (
                baseline.requested,
                baseline.effective,
                baseline.materials,
                baseline.floating_materials,
                baseline.floating_blur,
            )
        );
    }

    #[test]
    fn decorative_edges_keep_exact_opaque_and_transparent_authorship() {
        let host = Color::rgb(0x202020);
        let authored = Color::rgba(0x90a0b080);
        let absent = Color::rgba(0x12345600);

        assert_eq!(SurfaceMaterials::OPAQUE.edge(host, authored), authored);
        assert_eq!(SurfaceMaterials::derive(1.0).edge(host, absent), absent);
    }

    #[test]
    fn material_edges_preserve_authored_alpha_composites_and_contrast_polarity() {
        let material = SurfaceMaterials::derive(1.0);
        for (host, authored, rises) in [
            (Color::rgb(0x202020), Color::rgba(0xe0e0e080), true),
            (Color::rgb(0xe0e0e0), Color::rgba(0x20202080), false),
        ] {
            let edge = material.edge(host, authored);
            let expected = authored.source_over(host);
            let actual = edge.source_over(host);

            assert_eq!(
                actual, expected,
                "authored alpha must survive as a composite"
            );
            assert!(edge.a < 255, "material edge retained opaque ink: {edge:?}");
            assert_eq!(actual.r > host.r, rises, "edge changed contrast polarity");
            assert_eq!(actual.g > host.g, rises, "edge changed contrast polarity");
            assert_eq!(actual.b > host.b, rises, "edge changed contrast polarity");
        }
    }

    #[test]
    fn material_presentation_changes_only_decorative_edge_families() {
        let colors = ChromeColors {
            background: Color::rgb(0x202020),
            input_background: Color::rgb(0x303030),
            input_border: Color::rgba(0xd0d0d080),
            element_background: Color::rgb(0x383838),
            element_border: Color::rgba(0x12345600),
            border_focused: Color::rgb(0x4080ff),
            input_focused_border: Color::rgb(0x5090ff),
            input_invalid_border: Color::rgb(0xff4050),
            primary_border: Color::rgb(0x3060c0),
            destructive_border: Color::rgb(0xc03030),
            toggle_off_border: Color::rgb(0x808080),
            error_border: Color::rgb(0xd04040),
            resize_idle: Color::rgba(0xc0c0c080),
            resize_disabled: Color::rgb(0x606060),
            resize_hovered: Color::rgb(0x80a0ff),
            resize_focused: Color::rgb(0x4080ff),
            resize_dragged: Color::rgb(0x2060d0),
            ..ChromeColors::default()
        };

        assert_eq!(
            colors.material_presentation(SurfaceMaterials::OPAQUE),
            colors,
            "opaque presentation must retain every authored edge exactly"
        );

        let paint = colors.material_presentation(SurfaceMaterials::derive(1.0));
        assert_eq!(paint.element_border, colors.element_border);
        assert_eq!(
            paint.input_border.source_over(colors.input_background),
            colors.input_border.source_over(colors.input_background),
            "custom authored alpha must retain its semantic composite"
        );
        assert!(paint.input_border.a < 255);
        assert_eq!(
            paint.resize_idle.source_over(colors.background),
            colors.resize_idle.source_over(colors.background),
            "idle resize structure must retain its authored composite"
        );
        assert_eq!(
            paint.resize_disabled.source_over(colors.background),
            colors.resize_disabled.source_over(colors.background),
            "disabled resize structure must retain its authored composite"
        );
        assert!(paint.resize_idle.a < 255 && paint.resize_disabled.a < 255);
        for (actual, semantic) in [
            (paint.border_focused, colors.border_focused),
            (paint.input_focused_border, colors.input_focused_border),
            (paint.input_invalid_border, colors.input_invalid_border),
            (paint.primary_border, colors.primary_border),
            (paint.destructive_border, colors.destructive_border),
            (paint.toggle_off_border, colors.toggle_off_border),
            (paint.error_border, colors.error_border),
            (paint.resize_hovered, colors.resize_hovered),
            (paint.resize_focused, colors.resize_focused),
            (paint.resize_dragged, colors.resize_dragged),
        ] {
            assert_eq!(actual, semantic, "semantic signal must stay authored");
        }
    }

    #[test]
    fn floating_fields_composite_directly_over_the_raised_host() {
        let colors = ChromeColors {
            background: Color::rgb(0x101010),
            panel_background: Color::rgb(0x903020),
            elevated_surface_background: Color::rgb(0x204060),
            input_background: Color::rgba(0xc0e0a080),
            input_disabled_background: Color::rgba(0x8060e040),
            ..ChromeColors::default()
        };
        let host = colors
            .elevated_surface_background
            .source_over(colors.background.with_alpha(255));
        let presentation = colors.floating_presentation();

        assert_eq!(
            (
                presentation.input_background,
                presentation.input_disabled_background
            ),
            (
                colors.input_background.source_over(host),
                colors.input_disabled_background.source_over(host),
            ),
        );
    }

    #[test]
    fn overlay_reconstructs_neutral_and_chromatic_targets_without_a_full_tint() {
        // Use almost-opaque material to exercise the overlay path without appreciable fading.
        let material = SurfaceMaterials::derive(1.0 / 255.0);
        for (base, target) in [
            (0x141517, 0x25272b),
            (0x203040, 0x305020),
            (0xffffff, 0xeeeeee),
            (0x000000, 0x111111),
        ] {
            let base = Color::rgb(base);
            let target = Color::rgb(target);
            let overlay = material.paint(SurfaceRole::Surface, base, target);
            let result = overlay.source_over(base);
            for (actual, expected) in [result.r, result.g, result.b]
                .into_iter()
                .zip([target.r, target.g, target.b])
            {
                assert!(actual.abs_diff(expected) <= 1);
            }
        }
        let base = Color::rgb(0x202020);
        assert_eq!(material.paint(SurfaceRole::Base, base, base).a, 0);
    }

    /// The rungs of one appearance's elevation ladder each need a different overlay, and a single
    /// ceiling used to hand the crowded bright ones the same paint: a Pane, a selected chip and a
    /// hovered row all became one flat wash. Compression must keep them apart at every setting.
    #[test]
    fn resting_surfaces_stay_distinct_where_one_ceiling_would_collapse_them() {
        let ladders = [
            // Bright base: every rung needs most of the white ink available.
            (0xdcdee3, [0xe3e5e9, 0xeceef2, 0xf6f8fb, 0xffffff]),
            // Dark base: the rungs already fit, and must not be compressed into each other.
            (0x141517, [0x161719, 0x1c1d21, 0x1e2024, 0x25272b]),
        ];
        for (base, rungs) in ladders {
            let base = Color::rgb(base);
            let alphas = |transparency| {
                let material = SurfaceMaterials::derive(transparency);
                rungs.map(|rung| {
                    material
                        .paint(SurfaceRole::Surface, base, Color::rgb(rung).with_alpha(255))
                        .a
                })
            };
            // Below the setting where the glass engages, a surface is still allowed to reproduce
            // the color it was authored as: the window it sits in is nearly opaque anyway.
            for transparency in [0.01, 0.05] {
                let rungs = alphas(transparency);
                assert!(
                    rungs.windows(2).all(|pair| pair[0] <= pair[1]),
                    "rungs must keep their authored order: {rungs:?} at {transparency}"
                );
            }
            for transparency in [0.15, 0.35, 0.7, 1.0] {
                let rungs = alphas(transparency);
                assert!(
                    rungs.windows(2).all(|pair| pair[0] <= pair[1]),
                    "rungs must keep their authored order: {rungs:?} at {transparency}"
                );
                assert!(
                    rungs.iter().all(|alpha| *alpha < 128),
                    "a resting surface keeps transmitting its backing: {rungs:?}"
                );
                assert!(
                    rungs[3] > rungs[1] && rungs[1] > 0,
                    "the top of the ladder must stay above its middle at {transparency}"
                );
            }
        }
        // The bright ladder is genuinely compressed where it used to collapse: reproducing a
        // white Pane exactly would spend the whole of the sheet's remaining transmission.
        let bright = SurfaceMaterials::derive(0.35);
        let exact = u16::from(bright.alpha(SurfaceRole::Surface));
        let painted = u16::from(
            bright
                .paint(
                    SurfaceRole::Surface,
                    Color::rgb(0xdcdee3),
                    Color::rgb(0xffffff),
                )
                .a,
        );
        assert!(painted * 2 < exact, "{painted} of {exact}");
    }

    /// The Stepper is one continuous control, so transmission may not step, reverse, or land on
    /// nothing: at the maximum setting a window is all the glass it can be and still a window.
    #[test]
    fn transmission_falls_smoothly_to_a_usable_maximum() {
        let base = Color::rgb(0xdcdee3);
        let mut previous = (255_u8, 255_u8, 255_u8);
        for glass in 0..=255_u8 {
            let material = SurfaceMaterials { glass };
            let sheet = material.alpha(SurfaceRole::Sheet);
            let floating = material.alpha(SurfaceRole::Floating);
            let raised = material
                .paint(SurfaceRole::Surface, base, Color::rgb(0xffffff))
                .a;
            let current = (sheet, floating, raised);
            assert!(
                current.0 <= previous.0 && current.1 <= previous.1 && current.2 <= previous.2,
                "transmission must not reverse at {glass}: {previous:?} then {current:?}"
            );
            assert!(
                previous.0 - current.0 <= 2 && previous.1 - current.1 <= 2,
                "transmission must not step at {glass}: {previous:?} then {current:?}"
            );
            previous = current;
        }
        let maximum = SurfaceMaterials::derive(1.0);
        assert_eq!(
            maximum.alpha(SurfaceRole::Sheet),
            0,
            "the maximum setting hands the desktop the whole of the window's tint"
        );
        assert!(
            maximum.alpha(SurfaceRole::Floating) >= 96,
            "a menu must still constrain the color of content it is drawn over"
        );
        // Nonzero is not the same as visible. A pale desktop is the hardest backing for a bright
        // scheme to lift off, because white ink has the least room to work in, so that is where
        // the surviving difference is measured.
        let pale = Color::rgb(0xe0e0e0);
        for (rung, lift) in [(0xffffff_u32, 5_u8), (0xf6f8fb, 4), (0xe3e5e9, 2)] {
            let surface = maximum
                .paint(SurfaceRole::Surface, base, Color::rgb(rung))
                .source_over(pale);
            assert!(
                surface.g.saturating_sub(pale.g) >= lift,
                "a resting surface must still carry its color over a pale desktop: {surface:?}"
            );
        }
    }

    /// Compression belongs to a neutral ladder. A saturated fill and a surface authored far from
    /// the base are each stating a color the backdrop cannot state for them, and both reach their
    /// authored value gradually rather than at a threshold.
    #[test]
    fn stated_colors_keep_the_alpha_they_need() {
        let base = Color::rgb(0xdcdee3);
        // Almost-opaque material, so the overlay is read without appreciable fading.
        let sheer = SurfaceMaterials::derive(1.0 / 255.0);
        // Saturated actions and status fills, and a neutral panel authored far from the base.
        for target in [0x1a63bb, 0xc23b34, 0x22713f, 0x1c1c1e] {
            let target = Color::rgb(target);
            let rendered = sheer
                .paint(SurfaceRole::Surface, base, target)
                .source_over(base);
            for (actual, expected) in [rendered.r, rendered.g, rendered.b]
                .into_iter()
                .zip([target.r, target.g, target.b])
            {
                assert!(
                    actual.abs_diff(expected) <= 1,
                    "{rendered:?} from {target:?}"
                );
            }
        }
        // A neutral surface walking away from the base leaves the ladder without a cliff.
        let material = SurfaceMaterials::derive(0.35);
        let opacity = |distance: u8| {
            let shade = 0xdc - distance;
            let target = Color::from_rgb_components(shade, shade + 2, shade + 7);
            material.paint(SurfaceRole::Surface, base, target).a
        };
        let mut previous = opacity(0);
        for distance in 1..=110_u8 {
            let current = opacity(distance);
            assert!(
                current >= previous && current - previous <= 3,
                "membership must fade rather than switch at {distance}: {previous} then {current}"
            );
            previous = current;
        }
    }

    /// Authored translucency is scaled, never replaced, and a transparent sentinel never becomes
    /// a visible surface.
    #[test]
    fn authored_alpha_is_scaled_and_sentinels_stay_invisible() {
        let material = SurfaceMaterials::derive(0.35);
        let base = Color::rgb(0xdcdee3);
        for role in [
            SurfaceRole::Sheet,
            SurfaceRole::Base,
            SurfaceRole::Surface,
            SurfaceRole::Floating,
        ] {
            assert_eq!(
                material.paint(role, base, Color::rgba(0)).a,
                0,
                "a transparent sentinel stays transparent"
            );
            let opaque = material.paint(role, base, Color::rgb(0xffffff)).a;
            let authored = material
                .paint(role, base, Color::rgb(0xffffff).with_alpha(128))
                .a;
            assert!(
                authored < opaque && u16::from(authored) * 2 <= u16::from(opaque) + 2,
                "authored translucency must survive the material: {authored} of {opaque}"
            );
        }
    }
}
