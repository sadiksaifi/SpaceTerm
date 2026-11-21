//! Adapts decoded frames to the desktop's colors, link authority and diagnostics.
use super::*;
use crate::appearance::Color;
use crate::platform::local_filesystem::LocalFilesystemAuthority;
use crate::runtime::HyperlinkTarget;
pub(super) use frames::UpdateError;
use spaceterm_core::diagnostics::TerminalCounters;
#[cfg(test)]
use spaceterm_core::protocol::screen::Page;
use spaceterm_core::protocol::screen::{Colors, ScreenUpdate};
use spaceterm_terminal_client::screen as frames;
pub(super) type DecodedScreen = frames::DecodedScreen<HyperlinkTarget, Color>;

pub(super) struct ScreenDecoder {
    frames: frames::ScreenDecoder<HyperlinkTarget, Color>,
    counters: Arc<TerminalCounters>,
}

impl ScreenDecoder {
    pub(super) fn new(counters: Arc<TerminalCounters>) -> Self {
        Self {
            frames: frames::ScreenDecoder::new(),
            counters,
        }
    }

    pub(super) fn apply(
        &mut self,
        frame: u64,
        base: Option<u64>,
        update: ScreenUpdate,
        local_files: Option<&LocalFilesystemAuthority>,
    ) -> Result<&DecodedScreen, UpdateError> {
        let screen = self.frames.apply(frame, base, update, |link| {
            super::screen::decode_hyperlink(link, local_files)
        })?;
        self.counters.record_frame_decoded();
        Ok(screen)
    }

    #[cfg(test)]
    pub(super) fn frame(&self) -> Option<u64> {
        self.frames.frame()
    }
}

#[cfg(test)]
pub(super) fn decode_page(
    page: &Page,
    columns: u16,
    local_files: Option<&LocalFilesystemAuthority>,
) -> Result<Vec<(RowSnapshot, bool)>, UpdateError> {
    frames::decode_page(page, columns, |link| {
        super::screen::decode_hyperlink(link, local_files)
    })
}

fn rgb(color: [u8; 3]) -> crate::appearance::Color {
    crate::appearance::Color::from_rgb_components(color[0], color[1], color[2])
}

/// The text of each row of a keyframe, with a wide character's spacer as a space.
#[cfg(test)]
pub(super) fn keyframe_lines(update: &ScreenUpdate) -> Vec<String> {
    ScreenDecoder::new(Arc::default())
        .apply(1, None, update.clone(), None)
        .map(lines)
        .unwrap_or_default()
}

/// The rows a channel bound to `lease` receives now from `host`, or none once the lease no
/// longer presents the Pane.
#[cfg(test)]
pub(super) fn observed_lines(
    host: &mut impl crate::runtime::HostCommands,
    lease: pane::Lease,
) -> Vec<String> {
    let crate::runtime::Observed::Live(observation) = host.observe([1; 32], &lease) else {
        return Vec::new();
    };
    observation
        .screen
        .and_then(|capture| {
            crate::runtime::screen_encoder::ScreenEncoder::default().encode(&capture, 1, None)
        })
        .map(|update| keyframe_lines(&update))
        .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn lines(screen: &DecodedScreen) -> Vec<String> {
    screen
        .rows
        .iter()
        .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
        .collect()
}

/// Turns decoded frames into the screens the renderer draws, with the client's own colors
/// wherever the program did not override them. It keeps the client's default screen and the
/// projected palette until their inputs change, so consecutive screens share them.
#[derive(Default)]
pub(super) struct Presenter {
    /// Where the Pane's metadata comes from, when it is not this machine.
    metadata: Option<super::super::metadata::TerminalMetadataContext>,
    defaults: Option<Defaults>,
    palette: Option<Palette>,
}

/// The screen every snapshot starts from, for one appearance.
struct Defaults {
    appearance: TerminalAppearanceUpdate,
    screen: ScreenSnapshot,
}

/// The palette the renderer draws, projected from the program's palette and an appearance.
struct Palette {
    program: Vec<[u8; 3]>,
    overrides: [u64; 4],
    appearance: Arc<crate::appearance::ResolvedTerminalAppearance>,
    colors: Arc<[crate::appearance::Color; 256]>,
    overridden: Arc<[bool; 256]>,
}

impl Presenter {
    pub(super) fn new(
        remote_metadata: Option<&super::super::metadata::RemoteTerminalMetadataContext>,
    ) -> Self {
        Self {
            metadata: remote_metadata.map(|context| {
                super::super::metadata::TerminalMetadataContext::Remote(context.clone())
            }),
            ..Self::default()
        }
    }

    /// The screen the renderer draws for `decoded`. Graphics are left to the caller.
    pub(super) fn snapshot(
        &mut self,
        decoded: &DecodedScreen,
        generation: PresentationGeneration,
        appearance: &TerminalAppearanceUpdate,
    ) -> ScreenSnapshot {
        let configured = &appearance.appearance.colors;
        let mut screen = self.defaults(appearance).clone();
        let (palette, palette_overrides) = self.palette(&decoded.colors, appearance);
        screen.colors.palette = palette;
        screen.colors.palette_overrides = palette_overrides;
        decoded.apply_to_rows(&mut screen, generation);
        present_colors(decoded, &mut screen, configured);
        screen
    }

    fn defaults(&mut self, appearance: &TerminalAppearanceUpdate) -> &ScreenSnapshot {
        let current = self.defaults.as_ref().is_some_and(|defaults| {
            defaults.appearance.generation == appearance.generation
                && Arc::ptr_eq(&defaults.appearance.appearance, &appearance.appearance)
        });
        if !current {
            let mut screen = (*ScreenSnapshot::empty_with_appearance(
                crate::local_path::LocalPathSemantics::Posix,
                appearance,
            ))
            .clone();
            if let Some(context) = &self.metadata {
                Arc::make_mut(&mut screen.metadata).context = context.clone();
            }
            self.defaults = Some(Defaults {
                appearance: appearance.clone(),
                screen,
            });
        }
        &self
            .defaults
            .as_ref()
            .expect("the defaults were just set")
            .screen
    }

    fn palette(
        &mut self,
        colors: &Colors,
        appearance: &TerminalAppearanceUpdate,
    ) -> (Arc<[crate::appearance::Color; 256]>, Arc<[bool; 256]>) {
        let current = self.palette.as_ref().filter(|palette| {
            palette.overrides == colors.palette_overrides
                && Arc::ptr_eq(&palette.appearance, &appearance.appearance)
                && palette.program == colors.palette
        });
        if let Some(palette) = current {
            return (Arc::clone(&palette.colors), Arc::clone(&palette.overridden));
        }
        let configured = &appearance.appearance.colors;
        let overridden =
            |index: usize| colors.palette_overrides[index / 64] & (1 << (index % 64)) != 0;
        let mut projected = [crate::appearance::Color::BLACK; 256];
        for (index, (destination, source)) in projected.iter_mut().zip(&colors.palette).enumerate()
        {
            *destination = match index {
                0..8 if !overridden(index) => configured.normal[index],
                8..16 if !overridden(index) => configured.bright[index - 8],
                _ => rgb(*source),
            };
        }
        let palette = Palette {
            program: colors.palette.clone(),
            overrides: colors.palette_overrides,
            appearance: Arc::clone(&appearance.appearance),
            colors: Arc::new(projected),
            overridden: Arc::new(std::array::from_fn(overridden)),
        };
        let shared = (Arc::clone(&palette.colors), Arc::clone(&palette.overridden));
        self.palette = Some(palette);
        shared
    }
}

fn present_colors(
    decoded: &DecodedScreen,
    screen: &mut ScreenSnapshot,
    configured: &crate::appearance::TerminalColors,
) {
    let colors = &decoded.colors;
    let source = |overridden| {
        if overridden {
            TerminalDefaultColorSource::ProgramOverride
        } else {
            TerminalDefaultColorSource::HostDefault
        }
    };
    screen.colors.foreground_source = source(colors.foreground_overridden);
    screen.colors.background_source = source(colors.background_overridden);
    screen.colors.cursor_source = source(colors.cursor_overridden);
    screen.colors.foreground = if colors.foreground_overridden {
        rgb(colors.foreground)
    } else {
        configured.foreground
    };
    screen.colors.background = if colors.background_overridden {
        rgb(colors.background)
    } else {
        configured.background
    };
    screen.colors.reversed = decoded.modes.reversed;
    screen.background = screen.colors.effective_background();
    screen.cursor = decoded.cursor_snapshot(
        if colors.cursor_overridden {
            rgb(colors.cursor)
        } else {
            configured.cursor
        },
        if colors.cursor_overridden {
            rgb(colors.cursor_text)
        } else {
            configured.cursor_text.unwrap_or(screen.background)
        },
    );
}
