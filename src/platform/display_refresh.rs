//! The frame rate a Pane declares to its server: the fastest refresh rate among the connected
//! displays, as one of the protocol's rates.
use std::sync::atomic::{AtomicU8, Ordering};

/// The rate declared when no display could be read.
const UNKNOWN: u8 = 60;
/// The last rate read on the main thread. Zero before any reading.
static LAST: AtomicU8 = AtomicU8::new(0);

/// 120 when a connected display refreshes at 120 Hz or faster, otherwise 60. The displays are
/// read only on the main thread; another thread receives the last reading, or 60 before any.
pub(crate) fn frame_rate() -> u8 {
    if let Some(fastest) = fastest_display() {
        LAST.store(protocol_rate(fastest), Ordering::Relaxed);
    }
    match LAST.load(Ordering::Relaxed) {
        0 => UNKNOWN,
        rate => rate,
    }
}

/// The protocol rate for a display's maximum refresh rate in hertz.
fn protocol_rate(hz: isize) -> u8 {
    if hz >= 120 { 120 } else { 60 }
}

#[cfg(target_os = "macos")]
fn fastest_display() -> Option<isize> {
    let main_thread = objc2::MainThreadMarker::new()?;
    objc2_app_kit::NSScreen::screens(main_thread)
        .iter()
        .map(|screen| screen.maximumFramesPerSecond())
        .max()
}

/// GPUI does not report display refresh rates on Linux.
#[cfg(not(target_os = "macos"))]
fn fastest_display() -> Option<isize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_map_to_the_protocol_rates() {
        assert_eq!(protocol_rate(60), 60);
        assert_eq!(protocol_rate(90), 60);
        assert_eq!(protocol_rate(120), 120);
        assert_eq!(protocol_rate(144), 120);
        assert_eq!(protocol_rate(0), 60);
    }
}
