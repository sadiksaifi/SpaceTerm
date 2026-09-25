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
    use std::time::{Duration, Instant};

    use crate::terminal::FindQueryGeneration;
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };
    use libghostty_vt::selection::{FormatOptions, Selection};
    use libghostty_vt::terminal::{Point, PointCoordinate, ScrollViewport};
    use libghostty_vt::{Terminal, TerminalOptions};

    use super::*;

    #[repr(C)]
    #[derive(Default)]
    struct ProcessUsage {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        package_wakeups: u64,
        interrupt_wakeups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        phys_footprint: u64,
        start_time: u64,
        exit_time: u64,
    }

    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut ProcessUsage) -> i32;
    }

    fn footprint() -> Option<(u64, u64)> {
        let mut usage = ProcessUsage::default();
        let result = unsafe { proc_pid_rusage(std::process::id() as i32, 0, &raw mut usage) };
        (result == 0).then_some((usage.phys_footprint, usage.resident_size))
    }

    fn copy_history(terminal: &Terminal<'_, '_>, total_rows: u64) -> Vec<u8> {
        let last_row = u32::try_from(total_rows - 1).unwrap();
        let start = terminal
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y: 0 }))
            .unwrap();
        let end = terminal
            .grid_ref(Point::Screen(PointCoordinate {
                x: 119,
                y: last_row,
            }))
            .unwrap();
        let selection = Selection::new(start, end, false);
        terminal
            .format_selection_alloc(None, FormatOptions::new().with_selection(&selection))
            .unwrap()
            .unwrap()
            .to_vec()
    }

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

    #[test]
    #[ignore = "optimized native fixture; run with mise run bench:one performance_compression"]
    fn performance_compression() {
        const HISTORY_ROWS: usize = 10_000;
        let mut terminal = Terminal::new(TerminalOptions {
            cols: 120,
            rows: 40,
            max_scrollback: HISTORY_ROWS,
        })
        .unwrap();
        let mut output = String::with_capacity(HISTORY_ROWS * 120);
        for index in 0..HISTORY_ROWS + 40 {
            writeln!(
                output,
                "{index:05} Repeated terminal history with enough text to fill a native page and make the compression pass observable.\r"
            )
            .unwrap();
        }
        terminal.vt_write(output.as_bytes());
        let rows = terminal.scrollback_rows().unwrap();
        let total_rows = terminal.scrollbar().unwrap().total;
        assert!(rows >= 400, "fixture retained only {rows} Scrollback rows");
        let copied_before = copy_history(&terminal, total_rows);
        assert!(
            copied_before
                .windows(b"09800 Repeated".len())
                .any(|window| window == b"09800 Repeated"),
            "full-history copy omitted a middle Scrollback row"
        );
        let before = footprint();
        let mut steps = 0_usize;
        let mut max_step = Duration::ZERO;
        let started = Instant::now();
        let result = loop {
            let step_started = Instant::now();
            let result = terminal.compress(CompressionMode::Incremental).unwrap();
            max_step = max_step.max(step_started.elapsed());
            steps += 1;
            if result != CompressionResult::Pending || steps >= 100_000 {
                break result;
            }
        };
        let elapsed = started.elapsed();
        let after = footprint();
        let copied_after = copy_history(&terminal, total_rows);
        assert_eq!(copied_after, copied_before);
        terminal.scroll_viewport(ScrollViewport::Top);
        assert_eq!(copy_history(&terminal, total_rows), copied_before);
        terminal.scroll_viewport(ScrollViewport::Bottom);
        assert_eq!(copy_history(&terminal, total_rows), copied_before);
        eprintln!(
            "performance_compression rows={rows} steps={steps} result={result:?} elapsed_ms={:.3} max_step_ms={:.3} footprint_before_after={before:?}/{after:?}",
            elapsed.as_secs_f64() * 1_000.0,
            max_step.as_secs_f64() * 1_000.0,
        );
        assert_eq!(result, CompressionResult::Complete);
        assert!(steps < 100_000);
    }
}
