#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Color {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

impl Color {
    pub(crate) const fn rgb(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as u8,
            g: ((hex >> 8) & 0xff) as u8,
            b: (hex & 0xff) as u8,
            a: 0xff,
        }
    }

    pub(crate) const fn rgba(hex: u32) -> Self {
        Self {
            r: ((hex >> 24) & 0xff) as u8,
            g: ((hex >> 16) & 0xff) as u8,
            b: ((hex >> 8) & 0xff) as u8,
            a: (hex & 0xff) as u8,
        }
    }

    pub(crate) const fn from_rgb_components(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xff }
    }

    pub(crate) const fn rgba_hex(self) -> u32 {
        (self.r as u32) << 24 | (self.g as u32) << 16 | (self.b as u32) << 8 | self.a as u32
    }

    pub(crate) const fn is_opaque(self) -> bool {
        self.a == 0xff
    }

    pub(crate) fn canonical_hex(self) -> String {
        format!("#{:08x}", self.rgba_hex())
    }
}

impl Color {
    pub(crate) fn parse(value: &str) -> Result<Self, ()> {
        let hex = value.strip_prefix('#').ok_or(())?;
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(());
        }
        let number = u32::from_str_radix(hex, 16).map_err(|_| ())?;
        match hex.len() {
            3 => Ok(Self::rgb(
                ((number >> 8) & 15) * 0x110000
                    + ((number >> 4) & 15) * 0x1100
                    + (number & 15) * 0x11,
            )),
            4 => Ok(Self::rgba(
                ((number >> 12) & 15) * 0x11000000
                    + ((number >> 8) & 15) * 0x110000
                    + ((number >> 4) & 15) * 0x1100
                    + (number & 15) * 0x11,
            )),
            6 => Ok(Self::rgb(number)),
            8 => Ok(Self::rgba(number)),
            _ => Err(()),
        }
    }
}

impl serde::Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.canonical_hex())
    }
}

impl<'de> serde::Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&value).map_err(|()| serde::de::Error::custom("invalid color"))
    }
}

impl Color {
    /// Replace straight alpha without changing color channels.
    pub(crate) const fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }

    /// Scale existing alpha, retaining authored translucency.
    pub(crate) const fn multiply_opacity(self, opacity: u8) -> Self {
        Self {
            a: ((self.a as u16 * opacity as u16 + 127) / 255) as u8,
            ..self
        }
    }

    /// Straight-alpha source-over, including when both layers are translucent.
    pub(crate) fn source_over(self, destination: Self) -> Self {
        let source_alpha = f64::from(self.a) / 255.0;
        let destination_alpha = f64::from(destination.a) / 255.0;
        let alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
        if alpha == 0.0 {
            return Self::rgba(0);
        }
        let channel = |source: u8, dest: u8| {
            ((f64::from(source) * source_alpha
                + f64::from(dest) * destination_alpha * (1.0 - source_alpha))
                / alpha)
                .round() as u8
        };
        Self {
            r: channel(self.r, destination.r),
            g: channel(self.g, destination.g),
            b: channel(self.b, destination.b),
            a: (alpha * 255.0).round() as u8,
        }
    }

    /// Express this opaque semantic target as the smallest straight-alpha overlay over `base`.
    ///
    /// The result preserves the target's direction away from its semantic host without retaining
    /// the opaque ink used to author that target. Callers must composite authored alpha into the
    /// target before invoking this operation.
    pub(crate) fn relative_overlay(self, base: Self) -> Self {
        let base_channels = [base.r, base.g, base.b].map(f64::from);
        let target_channels = [self.r, self.g, self.b].map(f64::from);
        let alpha = base_channels
            .iter()
            .zip(target_channels)
            .map(|(&base, target)| {
                if target > base {
                    (target - base) / (255.0 - base)
                } else if target < base {
                    (base - target) / base
                } else {
                    0.0
                }
            })
            .fold(0.0_f64, f64::max);
        if alpha == 0.0 {
            return Self::rgba(0);
        }
        let ink: [u8; 3] = std::array::from_fn(|index| {
            ((target_channels[index] - (1.0 - alpha) * base_channels[index]) / alpha)
                .round()
                .clamp(0.0, 255.0) as u8
        });
        Self::from_rgb_components(ink[0], ink[1], ink[2]).with_alpha((alpha * 255.0).round() as u8)
    }

    /// Interpolate straight RGBA values for authored surface ramps.
    pub(crate) fn mix(self, other: Self, amount: f64) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let channel =
            |a: u8, b: u8| (f64::from(a) * (1.0 - amount) + f64::from(b) * amount).round() as u8;
        Self {
            r: channel(self.r, other.r),
            g: channel(self.g, other.g),
            b: channel(self.b, other.b),
            a: channel(self.a, other.a),
        }
    }

    /// sRGB contrast of opaque presentation colors. Composite alpha before calling.
    pub(crate) fn contrast_ratio(self, other: Self) -> f64 {
        let luminance = |color: Self| {
            let linear = |channel: u8| {
                let v = f64::from(channel) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
        };
        let (a, b) = (luminance(self), luminance(other));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Finds the nearest readable luminance while retaining OKLab hue and chroma until the sRGB
    /// gamut boundary forces chroma inward.
    pub(crate) fn readable_preserving_chroma(
        self,
        backgrounds: &[Self],
        minimum_contrast: f64,
    ) -> Option<Self> {
        let minimum = |color: Self| {
            backgrounds
                .iter()
                .copied()
                .map(|background| color.source_over(background).contrast_ratio(background))
                .fold(f64::INFINITY, f64::min)
        };
        if minimum(self) >= minimum_contrast {
            return Some(self);
        }

        let [start_l, start_a, start_b] = self.oklab();
        let candidate_at = |lightness: f64| {
            let exact = Self::from_oklab(lightness, start_a, start_b, self.a);
            if exact.is_some() {
                return exact;
            }
            let mut lower = 0.0;
            let mut upper = 1.0;
            let mut candidate = Self::from_oklab(lightness, 0.0, 0.0, self.a)?;
            for _ in 0..12 {
                let scale = (lower + upper) / 2.0;
                if let Some(in_gamut) =
                    Self::from_oklab(lightness, start_a * scale, start_b * scale, self.a)
                {
                    candidate = in_gamut;
                    lower = scale;
                } else {
                    upper = scale;
                }
            }
            Some(candidate)
        };
        let solve_lightness = |end_l: f64| {
            let mut previous = 0.0;
            for step in 1..=16 {
                let amount = f64::from(step) / 16.0;
                let lightness = start_l + (end_l - start_l) * amount;
                let Some(mut readable) = candidate_at(lightness) else {
                    previous = amount;
                    continue;
                };
                if minimum(readable) < minimum_contrast {
                    previous = amount;
                    continue;
                }
                let mut lower = previous;
                let mut upper = amount;
                for _ in 0..12 {
                    let middle = (lower + upper) / 2.0;
                    let candidate_lightness = start_l + (end_l - start_l) * middle;
                    let Some(candidate) = candidate_at(candidate_lightness) else {
                        lower = middle;
                        continue;
                    };
                    if minimum(candidate) >= minimum_contrast {
                        readable = candidate;
                        upper = middle;
                    } else {
                        lower = middle;
                    }
                }
                return Some((readable, (end_l - start_l).abs() * upper));
            }
            None
        };

        let chromatic = [solve_lightness(0.0), solve_lightness(1.0)]
            .into_iter()
            .flatten()
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(color, _)| color);
        if chromatic.is_some() {
            return chromatic;
        }

        // A translucent proposal can exhaust both luminance directions without ever reaching the
        // requested floor. Only then let opacity and chroma yield together toward an achromatic
        // endpoint. This keeps the common path hue/chroma exact and bounds custom-scheme work.
        [Self::rgb(0), Self::rgb(0xffffff)]
            .into_iter()
            .filter_map(|endpoint| {
                let mut previous = 0.0;
                for step in 1..=16 {
                    let amount = f64::from(step) / 16.0;
                    let mut readable = self.mix(endpoint, amount);
                    if minimum(readable) < minimum_contrast {
                        previous = amount;
                        continue;
                    }
                    let mut lower = previous;
                    let mut upper = amount;
                    for _ in 0..12 {
                        let middle = (lower + upper) / 2.0;
                        let candidate = self.mix(endpoint, middle);
                        if minimum(candidate) >= minimum_contrast {
                            readable = candidate;
                            upper = middle;
                        } else {
                            lower = middle;
                        }
                    }
                    return Some((readable, upper));
                }
                None
            })
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(color, _)| color)
    }

    /// Finds the nearest readable luminance on one fixed polarity side.
    ///
    /// Hue and chroma remain exact until the sRGB gamut boundary forces chroma inward. Unlike
    /// [`Self::readable_preserving_chroma`], this never searches the opposite luminance endpoint
    /// and never changes opacity, so callers can reserve polarity flips and alpha changes for
    /// their own later fallback stages.
    pub(crate) fn readable_preserving_chroma_toward(
        self,
        backgrounds: &[Self],
        minimum_contrast: f64,
        light: bool,
    ) -> Option<Self> {
        let luminance = |color: Self| {
            let linear = |channel: u8| {
                let value = f64::from(channel) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
        };
        let minimum = |color: Self| {
            backgrounds
                .iter()
                .copied()
                .map(|background| color.source_over(background).contrast_ratio(background))
                .fold(f64::INFINITY, f64::min)
        };
        let on_requested_side = |color: Self| {
            backgrounds.iter().copied().all(|background| {
                let rendered = color.source_over(background);
                if light {
                    luminance(rendered) > luminance(background)
                } else {
                    luminance(rendered) < luminance(background)
                }
            })
        };
        if on_requested_side(self) && minimum(self) >= minimum_contrast {
            return Some(self);
        }

        let [start_l, start_a, start_b] = self.oklab();
        let candidate_at = |lightness: f64| {
            if let Some(exact) = Self::from_oklab(lightness, start_a, start_b, self.a) {
                return Some(exact);
            }
            let mut lower = 0.0;
            let mut upper = 1.0;
            let mut candidate = Self::from_oklab(lightness, 0.0, 0.0, self.a)?;
            for _ in 0..12 {
                let scale = (lower + upper) / 2.0;
                if let Some(in_gamut) =
                    Self::from_oklab(lightness, start_a * scale, start_b * scale, self.a)
                {
                    candidate = in_gamut;
                    lower = scale;
                } else {
                    upper = scale;
                }
            }
            Some(candidate)
        };
        let end_l = if light { 1.0 } else { 0.0 };
        let mut previous = 0.0;
        for step in 1..=16 {
            let amount = f64::from(step) / 16.0;
            let lightness = start_l + (end_l - start_l) * amount;
            let Some(mut readable) = candidate_at(lightness) else {
                previous = amount;
                continue;
            };
            if !on_requested_side(readable) || minimum(readable) < minimum_contrast {
                previous = amount;
                continue;
            }
            let mut lower = previous;
            let mut upper = amount;
            for _ in 0..12 {
                let middle = (lower + upper) / 2.0;
                let candidate_lightness = start_l + (end_l - start_l) * middle;
                let Some(candidate) = candidate_at(candidate_lightness) else {
                    lower = middle;
                    continue;
                };
                if on_requested_side(candidate) && minimum(candidate) >= minimum_contrast {
                    readable = candidate;
                    upper = middle;
                } else {
                    lower = middle;
                }
            }
            return Some(readable);
        }
        None
    }

    fn oklab(self) -> [f64; 3] {
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        let [r, g, b] = [self.r, self.g, self.b].map(linear);
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        [
            0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
            1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
            0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
        ]
    }

    fn from_oklab(lightness: f64, a: f64, b: f64, alpha: u8) -> Option<Self> {
        let l = (lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
        let m = (lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
        let s = (lightness - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
        let linear = [
            4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
            -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
            -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
        ];
        if linear
            .into_iter()
            .any(|channel| !(-1e-7..=1.0 + 1e-7).contains(&channel))
        {
            return None;
        }
        let encoded = linear.map(|channel| {
            let channel = channel.clamp(0.0, 1.0);
            let value = if channel <= 0.003_130_8 {
                12.92 * channel
            } else {
                1.055 * channel.powf(1.0 / 2.4) - 0.055
            };
            (value * 255.0).round() as u8
        });
        Some(Self {
            r: encoded[0],
            g: encoded[1],
            b: encoded[2],
            a: alpha,
        })
    }
}

#[cfg(test)]
mod composition_tests {
    use super::Color;
    #[test]
    fn alpha_replacement_and_multiplication_have_distinct_meanings() {
        let color = Color::rgba(0x12345680);
        assert_eq!(color.with_alpha(128), color);
        assert_eq!(color.multiply_opacity(128), Color::rgba(0x12345640));
    }
    #[test]
    fn source_over_retains_correct_alpha_for_two_translucent_layers() {
        assert_eq!(
            Color::rgba(0xff000080).source_over(Color::rgba(0x0000ff80)),
            Color::rgba(0xaa0055c0)
        );
        assert_eq!(Color::rgba(0).source_over(Color::rgba(0)), Color::rgba(0));
        assert_eq!(
            Color::rgb(0xabcdef).source_over(Color::rgba(0x12345680)),
            Color::rgb(0xabcdef)
        );
        assert_eq!(
            Color::rgba(0).source_over(Color::rgb(0xabcdef)),
            Color::rgb(0xabcdef)
        );
    }
    #[test]
    fn source_over_handles_multiple_layers_on_an_opaque_surface() {
        let red_blue = Color::rgba(0xff000080).source_over(Color::rgba(0x0000ff80));
        assert_eq!(
            red_blue.source_over(Color::rgb(0xffffff)),
            Color::rgb(0xbf3f7f)
        );
        assert_eq!(Color::rgb(0).contrast_ratio(Color::rgb(0xffffff)), 21.0);
    }

    #[test]
    fn relative_overlay_reconstructs_neutral_and_chromatic_targets() {
        for (base, target) in [
            (Color::rgb(0x202020), Color::rgb(0x343434)),
            (Color::rgb(0xe8e8e8), Color::rgb(0xd0d0d0)),
            (Color::rgb(0x203040), Color::rgb(0x406020)),
        ] {
            let overlay = target.relative_overlay(base);

            assert_eq!(overlay.source_over(base), target);
            assert!(overlay.a < 255, "{base:?} to {target:?} used opaque ink");
        }
        assert_eq!(
            Color::rgb(0x202020).relative_overlay(Color::rgb(0x202020)),
            Color::rgba(0)
        );
    }

    #[test]
    fn readable_color_moves_luminance_before_reducing_chroma() {
        let proposed = Color::rgb(0x1259b0);
        let background = Color::rgb(0xbcbcbc);
        let resolved = proposed
            .readable_preserving_chroma(&[background], 4.5)
            .expect("blackward movement should reach the requested floor");

        assert!(resolved.contrast_ratio(background) >= 4.5);
        assert_ne!(resolved, proposed);
        let [_, proposed_a, proposed_b] = proposed.oklab();
        let [_, resolved_a, resolved_b] = resolved.oklab();
        assert!((proposed_a - resolved_a).abs() < 0.01);
        assert!((proposed_b - resolved_b).abs() < 0.01);
    }

    #[test]
    fn readable_color_can_raise_opacity_after_luminance_is_exhausted() {
        let proposed = Color::rgba(0x1259b010);
        let background = Color::rgb(0xbcbcbc);
        let resolved = proposed
            .readable_preserving_chroma(&[background], 4.5)
            .expect("an opaque achromatic endpoint can carry readable content");

        assert!(resolved.source_over(background).contrast_ratio(background) >= 4.5);
        assert!(resolved.a > proposed.a);
    }
}
