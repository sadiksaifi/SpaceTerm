//! Shared straight RGBA primitive. Appearance owns production scheme resolution.
mod color;
pub(crate) use color::Color;

#[cfg(test)]
pub(crate) use fixture::VAGUE_PRO;
#[cfg(test)]
pub(crate) use fixture::VAGUE_PRO as ACTIVE_THEME;

/// Pinned upstream Terminal fixture translated through the real production importer.
/// This preserves independent drift checks without a second theme parser or build generator.
#[cfg(test)]
mod fixture {
    use super::Color;
    use crate::appearance::{CustomScheme, TerminalColors, ZedImportKind, import_zed};
    use std::sync::LazyLock;

    pub(crate) struct PlayerFixture {
        pub(crate) selection: Color,
    }
    pub(crate) struct TerminalFixture {
        pub(crate) terminal_foreground: Color,
        pub(crate) terminal_background: Color,
        pub(crate) terminal_bright_foreground: Color,
        pub(crate) terminal_dim_foreground: Color,
        pub(crate) players: [PlayerFixture; 1],
        colors: TerminalColors,
    }
    impl TerminalFixture {
        pub(crate) fn terminal_normal(&self) -> [Color; 8] {
            self.colors.normal
        }
        pub(crate) fn terminal_bright(&self) -> [Color; 8] {
            self.colors.bright
        }
        pub(crate) fn terminal_dim(&self) -> [Color; 8] {
            self.colors.dim
        }
    }
    pub(crate) static VAGUE_PRO: LazyLock<TerminalFixture> = LazyLock::new(|| {
        let bytes = include_bytes!("../third_party/vague-pro-zed/themes/vague-pro.json");
        let candidates = crate::appearance::list_zed_candidates(bytes).unwrap();
        let index = candidates
            .iter()
            .find(|candidate| candidate.name == "Vague Pro")
            .unwrap()
            .index;
        let schemes = import_zed(bytes, index, &[ZedImportKind::Terminal]).unwrap();
        let CustomScheme::Terminal(scheme) = &schemes[0] else {
            panic!("Terminal fixture kind");
        };
        let mut colors = TerminalColors::default();
        colors.apply(&scheme.colors);
        TerminalFixture {
            terminal_foreground: colors.foreground,
            terminal_background: colors.background,
            terminal_bright_foreground: colors.bright_foreground,
            terminal_dim_foreground: colors.dim_foreground,
            players: [PlayerFixture {
                selection: colors.selection_background,
            }],
            colors,
        }
    });
}
