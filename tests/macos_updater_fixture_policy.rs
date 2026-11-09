//! The macOS updater fixture's link to SpaceTerm's quit policy. The test task compiles this file
//! with `src/platform/quit_policy.rs` into a static library, so the fixture answers Sparkle's
//! quit requests and runs the relaunch quit with the app's own policy. Only the fixture links it.

#[allow(dead_code, reason = "the fixture uses only the relaunch quit")]
#[path = "../src/platform/quit_policy.rs"]
mod quit_policy;

use std::cell::RefCell;

use quit_policy::{RelaunchQuit, quit_request};

thread_local! {
    static RELAUNCH: RefCell<RelaunchQuit<u64>> = RefCell::default();
}

/// The fixture names an approval by a number, and no approval by 0.
fn approval(id: u64) -> Option<u64> {
    (id != 0).then_some(id)
}

/// [`RelaunchQuit::request`] for the current Apple Event, or none when `in_event` is false.
#[unsafe(no_mangle)]
pub extern "C" fn spt_fixture_quit_request(
    event_class: u32,
    event_id: u32,
    has_reason: bool,
    in_event: bool,
    approved: u64,
) -> u8 {
    let request = quit_request(in_event.then_some((event_class, event_id, has_reason)));
    RELAUNCH.with_borrow_mut(|quit| quit.request(request, approval(approved))) as u8
}

/// [`RelaunchQuit::saved`].
#[unsafe(no_mangle)]
pub extern "C" fn spt_fixture_settings_saved(saved: bool, approved: u64) -> u8 {
    RELAUNCH.with_borrow_mut(|quit| quit.saved(saved, approval(approved))) as u8
}

/// [`RelaunchQuit::released`].
#[unsafe(no_mangle)]
pub extern "C" fn spt_fixture_relaunch_released(updater_quits: bool, approved: u64) -> u8 {
    RELAUNCH.with_borrow_mut(|quit| quit.released(updater_quits, approval(approved))) as u8
}
