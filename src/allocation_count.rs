//! Counts the heap allocations one thread makes, so tests can bound the allocations of a path.
//!
//! Each test runs on its own thread, so concurrent tests do not count each other's allocations.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// The allocations this thread made since counting began, or `None` while not counting.
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

struct Counting;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// SAFETY: every operation forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }
}

fn record() {
    // A thread that is exiting has no counter left, and counts nothing.
    let _ = COUNT.try_with(|count| count.set(count.get().map(|count| count + 1)));
}

/// Runs `operation` and returns its result with the number of allocations, including
/// reallocations, it made on this thread.
pub(crate) fn count<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            COUNT.set(None);
        }
    }
    COUNT.set(Some(0));
    let stop = Stop;
    let result = operation();
    let count = COUNT.get().unwrap_or_default();
    drop(stop);
    (result, count)
}
