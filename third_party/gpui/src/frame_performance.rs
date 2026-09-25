//! Opt-in, content-free frame and native source counters for performance fixtures.

use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! frame_counters {
    ($(#[doc = $doc:literal] $field:ident => $counter:ident),+ $(,)?) => {
        /// Cumulative process-wide frame activity, available with `performance-probes`.
        ///
        /// Counters never reset. Each value is read atomically, but the complete
        /// snapshot is not a transaction across the native and main threads.
        /// Measure rates from differences between snapshots and an external
        /// monotonic clock. The snapshot contains no terminal or window content.
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
        pub struct FramePerformanceSnapshot {
            $(#[doc = $doc] pub $field: u64),+
        }

        #[derive(Clone, Copy)]
        pub(crate) enum Counter {
            $($counter),+
        }

        static COUNTERS: [AtomicU64; [$(stringify!($counter)),+].len()] =
            [const { AtomicU64::new(0) }; [$(stringify!($counter)),+].len()];

        impl FramePerformanceSnapshot {
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
    /// Display-link callbacks delivered to a window on the main queue.
    window_vsync_callbacks => WindowVsync,
    /// Synchronous AppKit layer or native activation frame callbacks.
    native_sync_frames => NativeSyncFrame,
    /// Logical GPUI frame callbacks, whether or not they draw or present.
    logical_frames => LogicalFrame,
    /// Logical callbacks that neither rebuild nor present a scene.
    clean_frames => CleanFrame,
    /// Scene rebuilds, including direct draws outside logical frame callbacks.
    scene_draws => SceneDraw,
    /// Scene submissions to the platform renderer.
    scene_presents => ScenePresent,
    /// Metal scene submissions containing at least one path primitive.
    scene_draws_with_paths => SceneWithPaths,
    /// Successful sets of drawable-sized Metal path target allocations.
    path_texture_allocation_sets => PathTextureAllocationSet,
    /// Individual queued next-frame callbacks executed by logical frame callbacks.
    next_frame_callbacks => NextFrameCallback,
    /// Logical frames that consume a nonempty next-frame callback queue.
    frames_with_callbacks => FrameWithCallbacks,
    /// Logical frames with dirty scene state after queued callbacks execute.
    frames_with_dirty_scene => FrameWithDirtyScene,
    /// Logical frames with a prepared scene awaiting presentation.
    frames_with_pending_presentation => FrameWithPendingPresentation,
    /// Logical frames inside the existing active-window input presentation grace.
    frames_with_input_grace => FrameWithInputGrace,
    /// Logical frames whose native request requires presentation.
    frames_requiring_presentation => FrameRequiringPresentation,
    /// Logical frames whose native request forces a scene rebuild.
    frames_forcing_render => FrameForcingRender,
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
    /// Native start failures whose pending window subscription was removed.
    native_start_failures => NativeStartFailure,
}

pub(crate) fn record(counter: Counter, amount: u64) {
    COUNTERS[counter as usize].fetch_add(amount, Ordering::Relaxed);
}
