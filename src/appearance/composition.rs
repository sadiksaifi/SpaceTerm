//! Requested native backdrop and effective presentation are independent from scheme classification.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WindowBackgroundAppearance {
    #[default]
    Opaque,
    Transparent,
    Blurred,
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
    /// Menus, popovers, dialogs and tooltips, which cover rendered content. GPUI cannot blur what
    /// is painted beneath them, so they stay denser to keep covered text from bleeding through.
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

    /// Floating surfaces give their color up more slowly, because they cover rendered content.
    const FLOATING_RETENTION: f32 = 0.3;
    /// What a resting surface still paints at the maximum setting, and what a floating one does.
    ///
    /// The sheet is the window's transmission: one continuous tint over everything, so the
    /// maximum setting hands the desktop all of it and keeps none. A resting surface paints only
    /// its difference from that sheet, and that difference is the whole of what tells a Pane from
    /// the shell, a card from the page, or a selected chip from the row beside it. Giving it up
    /// buys a few percent more desktop and costs the window its hierarchy, so a resting surface
    /// keeps enough of its color to lift several levels off a pale desktop instead. A floating
    /// surface covers live content rather than resting beside it, and keeps enough to stop what
    /// it covers from reading through.
    const RESTING_RESIDUAL: f32 = 0.42;
    const FLOATING_RESIDUAL: f32 = 0.12;
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
            SurfaceRole::Floating => (Self::FLOATING_RESIDUAL, Self::FLOATING_RETENTION),
        }
    }

    /// The share of an authored color one role still holds at this setting.
    ///
    /// The sheet fades linearly. Other roles approach their residual through a quadratic curve,
    /// retaining more color at high transparency with a smaller change near the default.
    /// Residuals below a half keep the curve strictly decreasing across the whole range.
    fn presence(self, role: SurfaceRole) -> f32 {
        let admitted = self.admitted();
        (1.0 - admitted) + Self::transmission(role).0 * admitted * admitted
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
    pub(crate) fn elevation(self, base: super::Color) -> f32 {
        let lift = if Self::is_bright(base) {
            Self::LIFT_BRIGHT
        } else {
            Self::LIFT_DARK
        };
        self.engagement() * lift
    }

    /// Dark backing beneath a Pane's elevation tint. Light keeps its existing material.
    pub(crate) fn pane_protection(self, base: super::Color) -> f32 {
        if Self::is_bright(base) {
            0.0
        } else {
            Self::PANE_PROTECTION_DARK * self.engagement()
        }
    }

    pub(crate) const fn is_opaque(self) -> bool {
        self.glass == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedWindowComposition {
    pub(crate) requested: WindowBackgroundAppearance,
    pub(crate) effective: WindowBackgroundAppearance,
    pub(crate) materials: SurfaceMaterials,
}

impl ResolvedWindowComposition {
    pub(crate) fn resolve(
        preferences: &super::preferences::BackgroundPreferences,
        supported: bool,
    ) -> Self {
        let requested = if preferences.blur {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Transparent
        };
        let materials = SurfaceMaterials::derive(preferences.transparency);
        // A window with no glass keeps its opaque backing, whatever backdrop was asked for: an
        // effect behind a fully painted window costs a backdrop for nothing.
        let enabled = supported && !materials.is_opaque();
        Self {
            requested,
            effective: if enabled {
                requested
            } else {
                WindowBackgroundAppearance::Opaque
            },
            materials: if enabled {
                materials
            } else {
                SurfaceMaterials::OPAQUE
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Color;

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
            "a menu must still cover the content it is drawn over"
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
