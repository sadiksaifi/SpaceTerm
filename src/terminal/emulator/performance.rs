use super::*;
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};
use std::hint::black_box;

/// Measures parser plus immutable Screen construction, excluding GPUI and PTY I/O.
#[test]
#[ignore = "explicit optimized performance fixture; run mise run bench:one performance_snapshot"]
fn performance_snapshot() {
    assert!(
        !black_box(cfg!(debug_assertions)),
        "run this fixture in release mode"
    );
    const FRAMES: usize = 300;
    const REPETITIONS: usize = 7;
    for (name, text, changed_rows) in [
        ("ascii_full", "abcdefghijk 0123456789 ".repeat(5), 40),
        ("unicode_full", "e\u{301} 界 😀 ".repeat(12), 40),
        ("ascii_partial", "abcdefghijk 0123456789 ".repeat(5), 1),
    ] {
        let output = ["A", "B"].map(|prefix| {
            (1..=changed_rows)
                .map(|row| format!("\x1b[{row};1H{prefix}{text}\x1b[K"))
                .collect::<String>()
        });
        for repetition in 0..REPETITIONS {
            let geometry = TerminalGeometry::from_grid(
                CellGridSize::new(120, 40),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            );
            let mut emulator = TerminalEmulator::new(geometry).unwrap();
            for frame in 0..20 {
                emulator.feed(output[frame % 2].as_bytes());
                black_box(emulator.snapshot().unwrap().unwrap());
            }
            let started = Instant::now();
            for frame in 0..FRAMES {
                emulator.feed(black_box(output[frame % 2].as_bytes()));
                black_box(emulator.snapshot().unwrap().unwrap());
            }
            let elapsed = started.elapsed();
            println!(
                "{{\"fixture\":\"{name}\",\"repetition\":{repetition},\"frames\":{FRAMES},\"ns_per_frame\":{}}}",
                elapsed.as_nanos() / FRAMES as u128,
            );
        }
    }
}
