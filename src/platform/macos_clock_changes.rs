//! The system clock's change notification, for schedules kept in wall-clock time.
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSObjectProtocol, NSSystemClockDidChangeNotification,
};

/// Calls its callback after each change to the system clock until it drops.
pub(crate) struct ClockChanges {
    center: Retained<NSNotificationCenter>,
    observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl ClockChanges {
    pub(crate) fn observe(changed: Box<dyn Fn() + Send + Sync>) -> Self {
        let center = NSNotificationCenter::defaultCenter();
        let block = RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| changed());
        // SAFETY: the block owns a Send and Sync callback, so Foundation may run it on the thread
        // that posts the notification. Drop removes the observer before the block is released.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSSystemClockDidChangeNotification),
                None,
                None,
                &block,
            )
        };
        Self { center, observer }
    }
}

impl Drop for ClockChanges {
    fn drop(&mut self) {
        // SAFETY: this is the observer token this center returned during registration.
        unsafe { self.center.removeObserver((*self.observer).as_ref()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_posted_clock_change_reaches_the_callback_until_the_observation_drops() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let changes = ClockChanges::observe(Box::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        }));
        // SAFETY: the name is Foundation's constant and no object is attached.
        let post = || unsafe {
            NSNotificationCenter::defaultCenter()
                .postNotificationName_object(NSSystemClockDidChangeNotification, None)
        };
        post();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(changes);
        post();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
