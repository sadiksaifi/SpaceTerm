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
