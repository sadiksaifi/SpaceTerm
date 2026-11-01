use std::path::PathBuf;

#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "desktop")]
pub(crate) use desktop::*;

/// Isolated resource layout supplied explicitly to portable launch policy.
pub(crate) struct ShellResourcesFixture(PathBuf);
impl ShellResourcesFixture {
    pub(crate) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spaceterm-resources-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        for relative in [
            "bash/spaceterm.bash",
            "elvish/lib/spaceterm-integration.elv",
            "elvish/rc.elv",
            "fish/vendor_conf.d/spaceterm-shell-integration.fish",
            "nushell/vendor/autoload/spaceterm.nu",
            "zsh/.zshenv",
        ] {
            let path = root.join("shell-integration").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        Self(root)
    }
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for ShellResourcesFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Heap allocations one thread made while [`count_allocations`] measured it.
#[cfg_attr(
    not(feature = "desktop"),
    allow(dead_code, reason = "the re-bind cost test drives the desktop client")
)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Allocations {
    pub(crate) count: usize,
    pub(crate) bytes: usize,
}

std::thread_local! {
    // Copy and const-initialized, so reading it from the allocator never allocates.
    static ALLOCATIONS: std::cell::Cell<Option<Allocations>> = const { std::cell::Cell::new(None) };
}

/// Counts the test thread's allocations and forwards every request to the system allocator.
/// Other threads, and the test thread outside [`count_allocations`], are not counted.
struct CountingAllocator;

impl CountingAllocator {
    fn record(bytes: usize) {
        let _ = ALLOCATIONS.try_with(|allocations| {
            if let Some(mut counted) = allocations.get() {
                counted.count += 1;
                counted.bytes += bytes;
                allocations.set(Some(counted));
            }
        });
    }
}

// SAFETY: every method forwards its arguments unchanged to `System`, which upholds the
// `GlobalAlloc` contract; recording touches only a const-initialized thread-local `Cell`.
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::record(layout.size());
        // SAFETY: the caller's contract for `alloc` is passed through unchanged.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::record(layout.size());
        // SAFETY: the caller's contract for `alloc_zeroed` is passed through unchanged.
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size);
        // SAFETY: the caller's contract for `realloc` is passed through unchanged.
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: the caller's contract for `dealloc` is passed through unchanged.
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING_ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `measured` and returns the heap allocations this thread made during it.
#[cfg_attr(
    not(feature = "desktop"),
    allow(dead_code, reason = "the re-bind cost test drives the desktop client")
)]
pub(crate) fn count_allocations<T>(measured: impl FnOnce() -> T) -> (T, Allocations) {
    ALLOCATIONS.with(|allocations| allocations.set(Some(Allocations::default())));
    let value = measured();
    let counted = ALLOCATIONS.with(|allocations| allocations.take().unwrap_or_default());
    (value, counted)
}
