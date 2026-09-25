//! Exercises the real display-link adapter on the macOS main thread.

#[cfg(target_os = "macos")]
#[allow(
    dead_code,
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case
)]
mod dispatch_sys {
    include!(concat!(env!("OUT_DIR"), "/dispatch_sys.rs"));
}

#[cfg(target_os = "macos")]
fn dispatch_get_main_queue() -> dispatch_sys::dispatch_queue_t {
    std::ptr::addr_of!(dispatch_sys::_dispatch_main_q) as dispatch_sys::dispatch_queue_t
}

// Compile the production implementation directly, without widening GPUI's public API.
#[cfg(target_os = "macos")]
#[path = "../src/platform/mac/display_link.rs"]
mod display_link;

#[cfg(all(target_os = "macos", feature = "native-test-support"))]
#[allow(dead_code)]
#[path = "../src/frame_test_support.rs"]
mod frame_test_support;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    native::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("the display-link lifecycle fixture requires macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
mod native {
    use super::display_link::DisplayLink;
    use anyhow::{Result, anyhow, ensure};
    use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
    use core_graphics::display::CGDisplay;
    use std::{
        cell::Cell,
        ffi::c_void,
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct Probe {
        ticks: Cell<usize>,
        closed: Cell<bool>,
        late_ticks: Cell<usize>,
    }

    unsafe extern "C" fn tick(context: *mut c_void) {
        // Every probe remains alive until after the final main-queue drain.
        let probe = unsafe { &*context.cast::<Probe>() };
        probe.ticks.set(probe.ticks.get() + 1);
        if probe.closed.get() {
            probe.late_ticks.set(probe.late_ticks.get() + 1);
        }
    }

    fn source(display_id: u32, probe: &Probe) -> Result<DisplayLink> {
        DisplayLink::new(
            display_id,
            std::ptr::from_ref(probe).cast_mut().cast(),
            tick,
        )
        .map_err(|_| anyhow!("frame source creation failed"))
    }

    fn start(source: &mut DisplayLink) -> Result<()> {
        source
            .start()
            .map_err(|_| anyhow!("frame source start failed"))
    }

    fn stop(source: &mut DisplayLink) -> Result<()> {
        source
            .stop()
            .map_err(|_| anyhow!("frame source stop failed"))
    }

    fn pump_for(duration: Duration) {
        let until = Instant::now() + duration;
        while let Some(remaining) = until.checked_duration_since(Instant::now()) {
            CFRunLoop::run_in_mode(
                unsafe { kCFRunLoopDefaultMode },
                remaining.min(Duration::from_millis(20)),
                false,
            );
        }
    }

    fn await_ticks(probe: &Probe, previous: usize) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.ticks.get() == previous && Instant::now() < deadline {
            pump_for(Duration::from_millis(20));
        }
        ensure!(
            probe.ticks.get() > previous,
            "frame callback deadline exceeded"
        );
        Ok(())
    }

    pub(super) fn run() -> Result<()> {
        const CYCLES: usize = 32;
        let display_id = CGDisplay::main().id;
        let first = Box::<Probe>::default();
        let second = Box::<Probe>::default();
        let mut first_source = source(display_id, &first)?;
        let mut second_source = source(display_id, &second)?;
        start(&mut first_source)?;
        start(&mut first_source)?;
        ensure!(
            first.ticks.get() == 0,
            "starting a frame source invoked its callback synchronously"
        );
        start(&mut second_source)?;
        await_ticks(&first, 0)?;
        await_ticks(&second, 0)?;

        stop(&mut first_source)?;
        stop(&mut first_source)?;
        // A queued dispatch event may remain after unsubscribe. Drain it before
        // checking that stopped sources receive no further display ticks.
        pump_for(Duration::from_millis(100));
        let stopped_ticks = first.ticks.get();
        let continuing_ticks = second.ticks.get();
        pump_for(Duration::from_millis(100));
        ensure!(
            first.ticks.get() == stopped_ticks,
            "stopped source kept ticking"
        );
        ensure!(
            second.ticks.get() > continuing_ticks,
            "shared display stopped early"
        );

        start(&mut first_source)?;
        await_ticks(&first, stopped_ticks)?;
        drop(first_source);
        first.closed.set(true);
        pump_for(Duration::from_millis(100));
        let closed_ticks = first.ticks.get();
        let continuing_ticks = second.ticks.get();
        pump_for(Duration::from_millis(100));
        ensure!(
            first.ticks.get() == closed_ticks,
            "cancelled source kept ticking"
        );
        ensure!(
            second.ticks.get() > continuing_ticks,
            "closing one source stopped another"
        );

        drop(second_source);
        second.closed.set(true);
        // Keep all callback contexts alive to turn unexpected late delivery into
        // an assertion, instead of making the fixture itself dereference freed data.
        let mut probes = Vec::with_capacity(CYCLES * 2);
        for cycle in 0..CYCLES {
            let probe = Box::<Probe>::default();
            let mut current = source(display_id, &probe)?;
            start(&mut current)?;
            await_ticks(&probe, 0)?;
            stop(&mut current)?;
            let previous = probe.ticks.get();
            start(&mut current)?;
            await_ticks(&probe, previous)?;
            drop(current);
            probe.closed.set(true);
            probes.push(probe);

            let undelivered = Box::<Probe>::default();
            let mut pending = source(display_id, &undelivered)?;
            if cycle % 2 == 0 {
                // Cancel an initial asynchronous wake before the main queue
                // runs it. Other cycles still cover never-started sources.
                start(&mut pending)?;
            }
            drop(pending);
            undelivered.closed.set(true);
            probes.push(undelivered);
        }
        pump_for(Duration::from_millis(100));
        let settled_ticks = probes.iter().map(|probe| probe.ticks.get()).sum::<usize>();
        let settled_first = first.ticks.get();
        let settled_second = second.ticks.get();
        pump_for(Duration::from_millis(100));
        ensure!(
            probes.iter().map(|probe| probe.ticks.get()).sum::<usize>() == settled_ticks
                && first.ticks.get() == settled_first
                && second.ticks.get() == settled_second,
            "closed frame sources did not settle"
        );
        ensure!(
            probes
                .iter()
                .skip(1)
                .step_by(2)
                .all(|probe| probe.ticks.get() == 0),
            "cancelled undelivered sources received callbacks"
        );
        let late_ticks = probes
            .iter()
            .map(|probe| probe.late_ticks.get())
            .sum::<usize>()
            + first.late_ticks.get()
            + second.late_ticks.get();
        ensure!(late_ticks == 0, "cancelled source invoked a closed context");
        #[cfg(feature = "native-test-support")]
        {
            let snapshot = crate::frame_test_support::FrameTestSnapshot::capture();
            ensure!(
                snapshot.native_links_created == 1,
                "native links accumulated across lifetimes"
            );
            ensure!(
                snapshot.window_sources_created == (CYCLES * 2 + 2) as u64
                    && snapshot.window_sources_created == snapshot.window_sources_released,
                "window frame sources were not released"
            );
            ensure!(
                snapshot.window_sources_subscribed == snapshot.window_sources_unsubscribed
                    && snapshot.native_links_started == snapshot.native_links_stopped,
                "native frame subscriptions did not balance"
            );
            println!(
                "native_display_link_resources links_created={} sources_created={} sources_released={} subscriptions={} unsubscriptions={} native_starts={} native_stops={}",
                snapshot.native_links_created,
                snapshot.window_sources_created,
                snapshot.window_sources_released,
                snapshot.window_sources_subscribed,
                snapshot.window_sources_unsubscribed,
                snapshot.native_links_started,
                snapshot.native_links_stopped,
            );
        }
        println!(
            "native_display_link_lifecycle cycles={CYCLES} callback_contexts={} first_ticks={} second_ticks={} settled_cycle_ticks={settled_ticks} callbacks_during_close_drain={late_ticks} status=pass",
            probes.len() + 2,
            first.ticks.get(),
            second.ticks.get()
        );
        Ok(())
    }
}
