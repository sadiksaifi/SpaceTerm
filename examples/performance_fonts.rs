//! Native macOS microbenchmark for SpaceTerm's startup font classification.
//! This mirrors appearance_runtime::capture_fonts without opening the application.

use gpui::{App, Application, font, px};
use std::hint::black_box;
use std::time::Instant;

const DEFAULT_TERMINAL_FAMILIES: [&str; 4] = [
    "JetBrainsMono Nerd Font",
    "JetBrainsMono Nerd Font Mono",
    "JetBrains Mono",
    "Menlo",
];

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "full".to_owned());
    assert!(matches!(mode.as_str(), "listing" | "full" | "selected"));

    Application::new().run(move |cx| measure(&mode, cx));
}

fn measure(mode: &str, cx: &mut App) {
    let start = Instant::now();
    let text = cx.text_system();
    let names = text.all_font_names();
    let name_count = names.len();
    let mut classified = 0;
    if mode != "listing" {
        for family in names {
            if mode == "selected" && !DEFAULT_TERMINAL_FAMILIES.contains(&family.as_str()) {
                continue;
            }
            let id = text.resolve_font(&font(family.clone()));
            let widths = ['i', 'M', '0', ' '].map(|ch| text.advance(id, px(18.0), ch));
            let monospace = widths.iter().all(|width| width.is_ok())
                && widths.windows(2).all(|pair| {
                    (f32::from(pair[0].as_ref().unwrap().width)
                        - f32::from(pair[1].as_ref().unwrap().width))
                    .abs()
                        < 0.01
                });
            black_box((format!("{id:?}"), family, monospace));
            classified += 1;
        }
    } else {
        black_box(names);
    }
    println!(
        "native_font_classification mode={mode} names={name_count} classified={classified} elapsed_us={}",
        start.elapsed().as_micros()
    );
    cx.quit();
}
