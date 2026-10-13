use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
struct Probe;
#[global_allocator]
static ALLOCATOR: Probe = Probe;
thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}
fn charge(bytes: isize) {
    if ACTIVE.try_with(Cell::get).unwrap_or(false) {
        LIVE.with(|live| {
            let next = live.get() + bytes;
            live.set(next);
            PEAK.with(|peak| peak.set(peak.get().max(next.max(0) as usize)));
        });
    }
}
// SAFETY: every allocation is delegated unchanged to System. Thread-local
// counters only observe sizes and never inspect or retain the pointers.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            charge(layout.size() as isize);
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            charge(layout.size() as isize);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        charge(-(layout.size() as isize));
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            charge(size as isize - layout.size() as isize);
        }
        result
    }
}
pub fn measure<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ACTIVE.with(|active| active.set(false));
        }
    }
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let reset = Reset;
    let result = operation();
    let peak = PEAK.with(Cell::get);
    drop(reset);
    (result, peak)
}
