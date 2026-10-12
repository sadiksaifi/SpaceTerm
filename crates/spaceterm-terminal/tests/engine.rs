#![cfg(feature = "engine")]

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use spaceterm_terminal::appearance::{
    Appearance, AppearanceGeneration, ResolvedTerminalAppearance, TerminalAppearanceUpdate,
    TerminalColors,
};
use spaceterm_terminal::geometry::{
    BackingScale, CellGridSize, LogicalCellSize, LogicalSize, TerminalGeometry,
};
use spaceterm_terminal::identity;
use spaceterm_terminal::local_filesystem::{
    LocalFilesystemAuthority, LocalFilesystemError, LocalIdentityObservation, LocalIdentitySource,
};
use spaceterm_terminal::local_path::LocalPathSemantics;
use spaceterm_terminal::metadata::{LocalMachine, TerminalMetadataContext};
use spaceterm_terminal::{FindQueryGeneration, KeyInput, TerminalColor, TerminalEmulator};

struct NoLocalFiles;

impl LocalIdentitySource for NoLocalFiles {
    fn identify(&self, _: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
        Err(LocalFilesystemError::PermissionDenied)
    }
}

#[test]
fn headless_engine_preserves_styled_wide_cells_find_and_input_method_bytes() {
    let geometry = TerminalGeometry::from_viewport(
        LogicalSize::new(64.0, 32.0),
        LogicalCellSize::new(8.0, 16.0),
        BackingScale::ONE,
        CellGridSize::new(8, 2),
    );
    let appearance = TerminalAppearanceUpdate::new(
        AppearanceGeneration::INITIAL,
        Arc::new(ResolvedTerminalAppearance {
            appearance: Appearance::Dark,
            colors: TerminalColors::default(),
            bold_as_bright: true,
        }),
    );
    let mut emulator = TerminalEmulator::new_with_local_filesystem(
        geometry,
        TerminalMetadataContext::local(LocalPathSemantics::Posix, "", LocalMachine::default()),
        "shell",
        identity::TERM_FALLBACK,
        Instant::now(),
        LocalFilesystemAuthority::new(LocalPathSemantics::Posix, Arc::new(NoLocalFiles)),
        appearance,
    )
    .unwrap();
    emulator.feed("\x1b[31;1mA界\x1b[0m".as_bytes());
    let query = FindQueryGeneration::default().next();
    emulator.set_find_query(query, "界".to_owned());
    let screen = emulator.snapshot().unwrap().unwrap();

    assert_eq!(screen.rows[0][0].text, "A");
    assert!(screen.rows[0][0].bold);
    assert_eq!(
        screen.rows[0][0].foreground_source,
        TerminalColor::Palette(1)
    );
    assert_eq!(screen.rows[0][1].text, "界");
    assert!(screen.rows[0][2].spacer_tail);
    assert_eq!(screen.find.as_ref().unwrap().total_matches, 1);
    assert_eq!(
        emulator
            .key(KeyInput::input_method_commit("日本語"))
            .unwrap()
            .bytes,
        "日本語".as_bytes(),
    );
}
