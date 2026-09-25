//! Shared macOS display pacing. Adapted from Zed's Apache-2.0 implementation:
//! https://github.com/zed-industries/zed/blob/e91b82c106817f2419207ebf81f1da766698ac95/crates/gpui_macos/src/display_link.rs
//!
//! CoreVideo can deliver a final callback after stop returns. Keep one native link
//! per display alive for the process lifetime, and give its callback only the display
//! identifier. Window sources are removed from the registry before cancellation and
//! release, so a late callback never dereferences a closed window's source.

use crate::{
    dispatch_get_main_queue,
    dispatch_sys::{
        _dispatch_source_type_data_add, dispatch_object_t, dispatch_release, dispatch_resume,
        dispatch_set_context, dispatch_source_cancel, dispatch_source_create,
        dispatch_source_merge_data, dispatch_source_set_event_handler_f, dispatch_source_t,
    },
};
use anyhow::Result;
use core_graphics::display::CGDirectDisplayID;
use std::{
    collections::{BTreeMap, btree_map},
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};
use util::ResultExt;

#[cfg(feature = "native-test-support")]
use crate::frame_test_support::{Counter, record};

static REGISTRY: Mutex<Registry<sys::DisplayLink, Arc<FrameRequestSource>>> =
    Mutex::new(Registry::new());

struct Registry<L, S> {
    displays: BTreeMap<CGDirectDisplayID, DisplayEntry<L, S>>,
    next_subscriber_id: u64,
}

struct DisplayEntry<L, S> {
    link: L,
    running: bool,
    subscribers: Vec<(SubscriberId, S)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SubscriberId(u64);

impl<L, S> Registry<L, S> {
    const fn new() -> Self {
        Self {
            displays: BTreeMap::new(),
            next_subscriber_id: 0,
        }
    }

    fn has_display(&self, display_id: CGDirectDisplayID) -> bool {
        self.displays.contains_key(&display_id)
    }

    fn for_each_subscriber(&self, display_id: CGDirectDisplayID, mut wake: impl FnMut(&S)) {
        if let Some(entry) = self.displays.get(&display_id) {
            for (_, source) in &entry.subscribers {
                wake(source);
            }
        }
    }

    fn start_failed(&mut self, display_id: CGDirectDisplayID, subscriber_id: SubscriberId) {
        if let Some(entry) = self.displays.get_mut(&display_id) {
            entry.running = false;
            entry.subscribers.retain(|(id, _)| *id != subscriber_id);
        }
    }
}

impl<L: Clone, S> Registry<L, S> {
    fn subscribe(
        &mut self,
        display_id: CGDirectDisplayID,
        new_link: Option<L>,
        source: S,
    ) -> Result<(SubscriberId, Option<L>)> {
        let next_subscriber_id = self
            .next_subscriber_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("display subscriber identifiers exhausted"))?;
        let entry = match (self.displays.entry(display_id), new_link) {
            (btree_map::Entry::Occupied(entry), _) => entry.into_mut(),
            (btree_map::Entry::Vacant(entry), Some(link)) => entry.insert(DisplayEntry {
                link,
                running: false,
                subscribers: Vec::new(),
            }),
            (btree_map::Entry::Vacant(_), None) => {
                anyhow::bail!("display link registry entry unavailable");
            }
        };
        let subscriber_id = SubscriberId(self.next_subscriber_id);
        self.next_subscriber_id = next_subscriber_id;
        entry.subscribers.push((subscriber_id, source));
        let link_to_start = if entry.running {
            None
        } else {
            entry.running = true;
            Some(entry.link.clone())
        };
        Ok((subscriber_id, link_to_start))
    }

    fn unsubscribe(
        &mut self,
        display_id: CGDirectDisplayID,
        subscriber_id: SubscriberId,
    ) -> Option<L> {
        let entry = self.displays.get_mut(&display_id)?;
        entry.subscribers.retain(|(id, _)| *id != subscriber_id);
        if entry.subscribers.is_empty() && entry.running {
            entry.running = false;
            Some(entry.link.clone())
        } else {
            None
        }
    }
}

// SAFETY: CoreVideo handles may be retained and used across threads. Registry
// mutation and CoreVideo start/stop stay on the main thread; the output callback
// only reads subscriber sources while holding the registry lock.
unsafe impl Send for DisplayEntry<sys::DisplayLink, Arc<FrameRequestSource>> {}

fn lock_registry() -> MutexGuard<'static, Registry<sys::DisplayLink, Arc<FrameRequestSource>>> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

fn debug_assert_main_thread() {
    #[cfg(debug_assertions)]
    {
        use objc::{class, msg_send, sel, sel_impl};
        let is_main_thread: objc::runtime::BOOL =
            unsafe { msg_send![class!(NSThread), isMainThread] };
        debug_assert!(
            is_main_thread == objc::runtime::YES,
            "display link lifecycle must remain on the main thread"
        );
    }
}

unsafe extern "C" fn display_link_callback(
    _display_link_out: *mut sys::CVDisplayLink,
    _current_time: *const sys::CVTimeStamp,
    _output_time: *const sys::CVTimeStamp,
    _flags_in: i64,
    _flags_out: *mut i64,
    display_id: *mut c_void,
) -> i32 {
    #[cfg(feature = "native-test-support")]
    record(Counter::NativeVsync, 1);
    let display_id = display_id as usize as CGDirectDisplayID;
    lock_registry().for_each_subscriber(display_id, |source| unsafe {
        dispatch_source_merge_data(source.0, 1);
    });
    0
}

fn subscribe(
    display_id: CGDirectDisplayID,
    source: Arc<FrameRequestSource>,
) -> Result<SubscriberId> {
    debug_assert_main_thread();
    let needs_link = !lock_registry().has_display(display_id);
    // Never call CoreVideo under the registry lock: its output callback may hold
    // CoreVideo's internal locks while waiting for this registry.
    let new_link = if needs_link {
        let link = unsafe {
            sys::DisplayLink::new(
                display_id,
                display_link_callback,
                display_id as usize as *mut c_void,
            )?
        };
        #[cfg(feature = "native-test-support")]
        record(Counter::NativeLinkCreated, 1);
        Some(link)
    } else {
        None
    };
    let (subscriber_id, link_to_start) = lock_registry().subscribe(display_id, new_link, source)?;
    if let Some(mut link) = link_to_start {
        if let Err(error) = unsafe { link.start() } {
            lock_registry().start_failed(display_id, subscriber_id);
            return Err(error);
        }
        #[cfg(feature = "native-test-support")]
        record(Counter::NativeLinkStarted, 1);
    }
    #[cfg(feature = "native-test-support")]
    record(Counter::WindowSourceSubscribed, 1);
    Ok(subscriber_id)
}

fn unsubscribe(display_id: CGDirectDisplayID, subscriber_id: SubscriberId) -> Result<()> {
    debug_assert_main_thread();
    let link_to_stop = lock_registry().unsubscribe(display_id, subscriber_id);
    #[cfg(feature = "native-test-support")]
    record(Counter::WindowSourceUnsubscribed, 1);
    if let Some(mut link) = link_to_stop {
        unsafe { link.stop()? };
        #[cfg(feature = "native-test-support")]
        record(Counter::NativeLinkStopped, 1);
    }
    Ok(())
}

struct FrameRequestSource(dispatch_source_t);

// SAFETY: Dispatch sources are thread-safe refcounted objects. The registry's
// output callback only merges data; handlers and lifecycle run on the main queue.
unsafe impl Send for FrameRequestSource {}
unsafe impl Sync for FrameRequestSource {}

impl Drop for FrameRequestSource {
    fn drop(&mut self) {
        unsafe {
            dispatch_source_cancel(self.0);
            dispatch_release(dispatch_object_t { _ds: self.0 });
        }
        #[cfg(feature = "native-test-support")]
        record(Counter::WindowSourceReleased, 1);
    }
}

pub struct DisplayLink {
    display_id: CGDirectDisplayID,
    frame_requests: Arc<FrameRequestSource>,
    registration: Option<SubscriberId>,
    _main_thread: PhantomData<Rc<()>>,
}

impl DisplayLink {
    pub fn new(
        display_id: CGDirectDisplayID,
        data: *mut c_void,
        callback: unsafe extern "C" fn(*mut c_void),
    ) -> Result<Self> {
        debug_assert_main_thread();
        let frame_requests = unsafe {
            let source = dispatch_source_create(
                &_dispatch_source_type_data_add,
                0,
                0,
                dispatch_get_main_queue(),
            );
            anyhow::ensure!(!source.is_null(), "could not create window frame source");
            dispatch_set_context(dispatch_object_t { _ds: source }, data);
            dispatch_source_set_event_handler_f(source, Some(callback));
            // Resume once for its lifetime. Dropping a suspended source is unsafe,
            // and source suspension is unnecessary when the registry unsubscribes it.
            dispatch_resume(dispatch_object_t { _ds: source });
            #[cfg(feature = "native-test-support")]
            record(Counter::WindowSourceCreated, 1);
            Arc::new(FrameRequestSource(source))
        };
        Ok(Self {
            display_id,
            frame_requests,
            registration: None,
            _main_thread: PhantomData,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        debug_assert_main_thread();
        if self.registration.is_none() {
            self.registration = Some(subscribe(self.display_id, self.frame_requests.clone())?);
            // The first pending frame should not wait for CoreVideo to restart
            // its refresh phase. Enqueue it on the same main-queue source;
            // subsequent frames remain paced by the native display link.
            unsafe { dispatch_source_merge_data(self.frame_requests.0, 1) };
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        debug_assert_main_thread();
        if let Some(subscriber_id) = self.registration.take() {
            unsubscribe(self.display_id, subscriber_id)?;
        }
        Ok(())
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        self.stop().log_err();
        // Removal under the registry lock makes the source unreachable to late
        // CoreVideo callbacks before the final Arc cancels and releases it.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_display_stops_only_after_its_last_window_unsubscribes() {
        let mut registry = Registry::new();
        let (first, start) = registry
            .subscribe(1, Some("display one"), "window one")
            .unwrap();
        assert_eq!(start, Some("display one"));
        let (second, start) = registry.subscribe(1, None, "window two").unwrap();
        assert_eq!(start, None);
        assert_eq!(registry.unsubscribe(1, first), None);
        let mut awakened = Vec::new();
        registry.for_each_subscriber(1, |source| awakened.push(*source));
        assert_eq!(awakened, ["window two"]);
        assert_eq!(registry.unsubscribe(1, second), Some("display one"));
        assert_eq!(registry.unsubscribe(1, second), None);
        registry.for_each_subscriber(1, |_| panic!("removed windows must not receive ticks"));
    }

    #[test]
    fn display_changes_leave_other_windows_running_and_reuse_returning_display() {
        let mut registry = Registry::new();
        let (first, _) = registry
            .subscribe(1, Some("display one"), "window one")
            .unwrap();
        let (second, _) = registry
            .subscribe(2, Some("display two"), "window two")
            .unwrap();
        assert_eq!(registry.unsubscribe(1, first), Some("display one"));
        let (moved, start) = registry.subscribe(2, None, "window one").unwrap();
        assert_eq!(start, None);
        assert_eq!(registry.unsubscribe(2, moved), None);
        let (_, start) = registry.subscribe(1, None, "window one").unwrap();
        assert_eq!(start, Some("display one"));
        assert_eq!(registry.unsubscribe(2, second), Some("display two"));
        assert_eq!(registry.displays.len(), 2);
    }

    #[test]
    fn failed_start_releases_subscription_and_can_retry_the_retained_link() {
        let mut registry = Registry::new();
        let source = Arc::new(());
        let weak = Arc::downgrade(&source);
        let (first, _) = registry.subscribe(1, Some("display one"), source).unwrap();
        registry.start_failed(1, first);
        assert!(weak.upgrade().is_none());
        registry.for_each_subscriber(1, |_| panic!("failed starts must not retain subscribers"));
        let (_, start) = registry.subscribe(1, None, Arc::new(())).unwrap();
        assert_eq!(start, Some("display one"));
    }

    #[test]
    fn repeated_window_lifetimes_release_sources_without_accumulating_native_links() {
        let mut registry = Registry::new();
        for iteration in 0..10_000 {
            let source = Arc::new(());
            let weak = Arc::downgrade(&source);
            let (subscriber, start) = registry
                .subscribe(1, (iteration == 0).then_some("display one"), source)
                .unwrap();
            assert_eq!(start, Some("display one"));
            assert_eq!(registry.unsubscribe(1, subscriber), Some("display one"));
            assert!(weak.upgrade().is_none());
        }
        assert_eq!(registry.displays.len(), 1);
        registry.for_each_subscriber(1, |_| panic!("closed windows must not receive late ticks"));
    }
}

mod sys {
    //! Derived from display-link crate under the following license:
    //! <https://github.com/BrainiumLLC/display-link/blob/master/LICENSE-MIT>
    //! Apple docs: [CVDisplayLink](https://developer.apple.com/documentation/corevideo/cvdisplaylinkoutputcallback?language=objc)
    #![allow(dead_code, non_upper_case_globals)]

    use anyhow::Result;
    use core_graphics::display::CGDirectDisplayID;
    use foreign_types::{ForeignType, foreign_type};
    use std::{
        ffi::c_void,
        fmt::{self, Debug, Formatter},
    };

    #[derive(Debug)]
    pub enum CVDisplayLink {}

    foreign_type! {
        pub unsafe type DisplayLink {
            type CType = CVDisplayLink;
            fn drop = CVDisplayLinkRelease;
            fn clone = CVDisplayLinkRetain;
        }
    }

    impl Debug for DisplayLink {
        fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
            formatter
                .debug_tuple("DisplayLink")
                .field(&self.as_ptr())
                .finish()
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct CVTimeStamp {
        pub version: u32,
        pub video_time_scale: i32,
        pub video_time: i64,
        pub host_time: u64,
        pub rate_scalar: f64,
        pub video_refresh_period: i64,
        pub smpte_time: CVSMPTETime,
        pub flags: u64,
        pub reserved: u64,
    }

    pub type CVTimeStampFlags = u64;

    pub const kCVTimeStampVideoTimeValid: CVTimeStampFlags = 1 << 0;
    pub const kCVTimeStampHostTimeValid: CVTimeStampFlags = 1 << 1;
    pub const kCVTimeStampSMPTETimeValid: CVTimeStampFlags = 1 << 2;
    pub const kCVTimeStampVideoRefreshPeriodValid: CVTimeStampFlags = 1 << 3;
    pub const kCVTimeStampRateScalarValid: CVTimeStampFlags = 1 << 4;
    pub const kCVTimeStampTopField: CVTimeStampFlags = 1 << 16;
    pub const kCVTimeStampBottomField: CVTimeStampFlags = 1 << 17;
    pub const kCVTimeStampVideoHostTimeValid: CVTimeStampFlags =
        kCVTimeStampVideoTimeValid | kCVTimeStampHostTimeValid;
    pub const kCVTimeStampIsInterlaced: CVTimeStampFlags =
        kCVTimeStampTopField | kCVTimeStampBottomField;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct CVSMPTETime {
        pub subframes: i16,
        pub subframe_divisor: i16,
        pub counter: u32,
        pub time_type: u32,
        pub flags: u32,
        pub hours: i16,
        pub minutes: i16,
        pub seconds: i16,
        pub frames: i16,
    }

    pub type CVSMPTETimeType = u32;

    pub const kCVSMPTETimeType24: CVSMPTETimeType = 0;
    pub const kCVSMPTETimeType25: CVSMPTETimeType = 1;
    pub const kCVSMPTETimeType30Drop: CVSMPTETimeType = 2;
    pub const kCVSMPTETimeType30: CVSMPTETimeType = 3;
    pub const kCVSMPTETimeType2997: CVSMPTETimeType = 4;
    pub const kCVSMPTETimeType2997Drop: CVSMPTETimeType = 5;
    pub const kCVSMPTETimeType60: CVSMPTETimeType = 6;
    pub const kCVSMPTETimeType5994: CVSMPTETimeType = 7;

    pub type CVSMPTETimeFlags = u32;

    pub const kCVSMPTETimeValid: CVSMPTETimeFlags = 1 << 0;
    pub const kCVSMPTETimeRunning: CVSMPTETimeFlags = 1 << 1;

    pub type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
        display_link_out: *mut CVDisplayLink,
        // A pointer to the current timestamp. This represents the timestamp when the callback is called.
        current_time: *const CVTimeStamp,
        // A pointer to the output timestamp. This represents the timestamp for when the frame will be displayed.
        output_time: *const CVTimeStamp,
        // Unused
        flags_in: i64,
        // Unused
        flags_out: *mut i64,
        // A pointer to app-defined data.
        display_link_context: *mut c_void,
    ) -> i32;

    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "CoreVideo", kind = "framework")]
    #[allow(improper_ctypes, unknown_lints, clippy::duplicated_attributes)]
    unsafe extern "C" {
        pub fn CVDisplayLinkCreateWithActiveCGDisplays(
            display_link_out: *mut *mut CVDisplayLink,
        ) -> i32;
        pub fn CVDisplayLinkSetCurrentCGDisplay(
            display_link: &mut DisplayLinkRef,
            display_id: u32,
        ) -> i32;
        pub fn CVDisplayLinkSetOutputCallback(
            display_link: &mut DisplayLinkRef,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> i32;
        pub fn CVDisplayLinkStart(display_link: &mut DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkStop(display_link: &mut DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkRelease(display_link: *mut CVDisplayLink);
        pub fn CVDisplayLinkRetain(display_link: *mut CVDisplayLink) -> *mut CVDisplayLink;
    }

    impl DisplayLink {
        /// Apple docs: [CVDisplayLinkCreateWithCGDisplay](https://developer.apple.com/documentation/corevideo/1456981-cvdisplaylinkcreatewithcgdisplay?language=objc)
        pub unsafe fn new(
            display_id: CGDirectDisplayID,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> Result<Self> {
            unsafe {
                let mut display_link: *mut CVDisplayLink = 0 as _;

                let code = CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link);
                anyhow::ensure!(code == 0, "could not create display link, code: {}", code);

                let mut display_link = DisplayLink::from_ptr(display_link);

                let code = CVDisplayLinkSetOutputCallback(&mut display_link, callback, user_info);
                anyhow::ensure!(code == 0, "could not set output callback, code: {}", code);

                let code = CVDisplayLinkSetCurrentCGDisplay(&mut display_link, display_id);
                anyhow::ensure!(
                    code == 0,
                    "could not assign display to display link, code: {}",
                    code
                );

                Ok(display_link)
            }
        }
    }

    impl DisplayLinkRef {
        /// Apple docs: [CVDisplayLinkStart](https://developer.apple.com/documentation/corevideo/1457193-cvdisplaylinkstart?language=objc)
        pub unsafe fn start(&mut self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStart(self);
                anyhow::ensure!(code == 0, "could not start display link, code: {}", code);
                Ok(())
            }
        }

        /// Apple docs: [CVDisplayLinkStop](https://developer.apple.com/documentation/corevideo/1457281-cvdisplaylinkstop?language=objc)
        pub unsafe fn stop(&mut self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStop(self);
                anyhow::ensure!(code == 0, "could not stop display link, code: {}", code);
                Ok(())
            }
        }
    }
}
