//! Opt-in, content-free frame and native source counters for native regression fixtures.

use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! frame_counters {
    ($(#[doc = $doc:literal] $field:ident => $counter:ident),+ $(,)?) => {
        /// Cumulative process-wide frame activity, available with `native-test-support`.
        ///
        /// Counters never reset. Each value is read atomically, but the complete
        /// snapshot is not a transaction across the native and main threads.
        /// Native regression fixtures compare snapshots after bounded run-loop
        /// drains. The snapshot contains no terminal or window content.
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
        pub struct FrameTestSnapshot {
            $(#[doc = $doc] pub $field: u64),+
        }

        #[derive(Clone, Copy)]
        pub(crate) enum Counter {
            $($counter),+
        }

        static COUNTERS: [AtomicU64; [$(stringify!($counter)),+].len()] =
            [const { AtomicU64::new(0) }; [$(stringify!($counter)),+].len()];

        impl FrameTestSnapshot {
            /// Reads cumulative counters without resetting or logging them.
            pub fn capture() -> Self {
                Self {
                    $($field: COUNTERS[Counter::$counter as usize].load(Ordering::Relaxed)),+
                }
            }
        }
    };
}

frame_counters! {
    /// CoreVideo output callbacks before per-window main-queue coalescing.
    native_vsync_callbacks => NativeVsync,
    /// Logical GPUI frame callbacks, whether or not they draw or present.
    logical_frames => LogicalFrame,
    /// Scene rebuilds, including direct draws outside logical frame callbacks.
    scene_draws => SceneDraw,
    /// Scene submissions to the platform renderer.
    scene_presents => ScenePresent,
    /// Successfully created native CVDisplayLinks retained by the display registry.
    native_links_created => NativeLinkCreated,
    /// Successful native CVDisplayLink start calls.
    native_links_started => NativeLinkStarted,
    /// Successful native CVDisplayLink stop calls.
    native_links_stopped => NativeLinkStopped,
    /// Window dispatch sources created and resumed.
    window_sources_created => WindowSourceCreated,
    /// Window dispatch sources cancelled and released by their final owner.
    window_sources_released => WindowSourceReleased,
    /// Successful window subscriptions to a display registry entry.
    window_sources_subscribed => WindowSourceSubscribed,
    /// Window subscriptions removed from a display registry entry.
    window_sources_unsubscribed => WindowSourceUnsubscribed,
}

pub(crate) fn record(counter: Counter, amount: u64) {
    COUNTERS[counter as usize].fetch_add(amount, Ordering::Relaxed);
}
