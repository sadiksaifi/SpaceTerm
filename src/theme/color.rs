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
}
