use libghostty_vt::Error;
use libghostty_vt::terminal::{CompressionActivity, CompressionMode, CompressionResult};

use super::TerminalEmulator;

impl TerminalEmulator {
    pub(crate) fn compression_activity(&self) -> Result<CompressionActivity, Error> {
        self.terminal.compression_activity()
    }

    pub(crate) fn compress_scrollback(&mut self) -> Result<CompressionResult, Error> {
        self.terminal.compress(CompressionMode::Incremental)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::fmt::Write as _;

    use crate::terminal::FindQueryGeneration;
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };

    use super::*;

    #[test]
    fn restored_history_preserves_find_and_selection() {
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(120, 40),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        );
        let mut emulator = TerminalEmulator::new(geometry).unwrap();
        let mut output = String::new();
        for index in 0..1_000 {
            let marker = if index == 600 { " NEEDLE" } else { "" };
            writeln!(
                output,
                "{index:05} Repeated terminal history with enough text to fill a native page{marker}.\r"
            )
            .unwrap();
        }
        emulator.feed(output.as_bytes());
        assert!(emulator.terminal.scrollback_rows().unwrap() >= 400);
        let selection = emulator.terminal.select_all().unwrap().unwrap();
        emulator.terminal.set_selection(Some(&selection)).unwrap();
        let selected_before = emulator.selection_text().unwrap().unwrap();
        assert!(selected_before.contains("00600"));
        emulator.set_find_query(FindQueryGeneration::test(1), "NEEDLE".to_owned());
        let found_before = emulator
            .snapshot()
            .unwrap()
            .unwrap()
            .find
            .as_ref()
            .unwrap()
            .total_matches;
        assert_eq!(found_before, 1);

        let compress = |emulator: &mut TerminalEmulator| loop {
            match emulator.compress_scrollback().unwrap() {
                CompressionResult::Pending => continue,
                result => break result,
            }
        };
        assert_eq!(compress(&mut emulator), CompressionResult::Complete);
        // Search older retained history before selection copying restores cold pages.
        emulator.set_find_query(FindQueryGeneration::test(2), "NEEDLE".to_owned());
        let found_after = emulator
            .snapshot()
            .unwrap()
            .unwrap()
            .find
            .as_ref()
            .unwrap()
            .total_matches;
        assert_eq!(found_after, found_before);

        // Find restores history too, so compress again before testing selection copying.
        assert_eq!(compress(&mut emulator), CompressionResult::Complete);
        assert_eq!(emulator.selection_text().unwrap().unwrap(), selected_before);
    }
}
