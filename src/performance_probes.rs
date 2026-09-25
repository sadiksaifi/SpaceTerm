//! Explicit benchmark instrumentation, omitted from normal application builds.

use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Serialize)]
struct Sample {
    event: &'static str,
    elapsed_s: f64,
    counters: gpui::FramePerformanceSnapshot,
}

static STARTED: OnceLock<Instant> = OnceLock::new();
static STARTUP_STAGES: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StartupStage {
    AppRunEnter,
    AppRunCallbackEnter,
    SettingsLoadBegin,
    SettingsLoaded,
    AppearanceInstalled,
    InitialWindowOpenEnter,
    InitialWindowOpened,
}

#[derive(Serialize)]
struct StartupSample {
    event: &'static str,
    stage: StartupStage,
    elapsed_s: f64,
}

pub(crate) fn record_startup(stage: StartupStage) {
    let Some(started) = STARTED.get() else { return };
    let bit = 1 << stage as u8;
    if STARTUP_STAGES.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return;
    }
    let sample = StartupSample {
        event: "startup_stage",
        stage,
        elapsed_s: started.elapsed().as_secs_f64(),
    };
    let mut output = io::stderr().lock();
    if serde_json::to_writer(&mut output, &sample).is_ok() {
        let _ = output.write_all(b"\n");
    }
}

pub(crate) fn start() -> io::Result<()> {
    if std::env::var_os("SPACETERM_BENCH_FRAME_COUNTERS").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Ok(());
    }
    let started = Instant::now();
    let _ = STARTED.set(started);
    std::thread::Builder::new()
        .name("spaceterm-performance-sampler".into())
        .spawn(move || {
            // One sampler wake per second is shared by every measured variant.
            // Keep logging bounded even if the controlling harness disappears.
            for second in 0..=400 {
                std::thread::sleep(Duration::from_secs(second).saturating_sub(started.elapsed()));
                let sample = Sample {
                    event: "frame_counters",
                    elapsed_s: started.elapsed().as_secs_f64(),
                    counters: gpui::FramePerformanceSnapshot::capture(),
                };
                let mut output = io::stderr().lock();
                if serde_json::to_writer(&mut output, &sample).is_err()
                    || output.write_all(b"\n").is_err()
                {
                    return;
                }
            }
        })?;
    Ok(())
}
