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

/// What the allocator records for the measuring thread.
#[derive(Clone, Copy, Default)]
struct Counted {
    allocations: Allocations,
    /// Bytes allocated minus bytes freed.
    live: isize,
    /// The highest `live` reached.
    peak: isize,
}

std::thread_local! {
    // Copy and const-initialized, so reading it from the allocator never allocates.
    static COUNTED: std::cell::Cell<Option<Counted>> = const { std::cell::Cell::new(None) };
}

/// Counts the measuring thread's allocations and frees, and forwards every request to the system
/// allocator. Other threads, and the measuring thread outside [`count_allocations`] and
/// [`LiveBytes::measure`], are not counted.
struct CountingAllocator;

impl CountingAllocator {
    /// Records an allocation of `allocated` bytes, if any, that freed `freed` bytes.
    fn record(allocated: Option<usize>, freed: usize) {
        let _ = COUNTED.try_with(|counted| {
            if let Some(mut current) = counted.get() {
                if let Some(bytes) = allocated {
                    current.allocations.count += 1;
                    current.allocations.bytes += bytes;
                }
                current.live += allocated.unwrap_or(0) as isize - freed as isize;
                current.peak = current.peak.max(current.live);
                counted.set(Some(current));
            }
        });
    }

    fn measure<T>(start: Counted, measured: impl FnOnce() -> T) -> (T, Counted) {
        COUNTED.with(|counted| counted.set(Some(start)));
        let value = measured();
        let counted = COUNTED.with(|counted| counted.take().unwrap_or_default());
        (value, counted)
    }
}

// SAFETY: every method forwards its arguments unchanged to `System`, which upholds the
// `GlobalAlloc` contract; recording touches only a const-initialized thread-local `Cell`.
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::record(Some(layout.size()), 0);
        // SAFETY: the caller's contract for `alloc` is passed through unchanged.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::record(Some(layout.size()), 0);
        // SAFETY: the caller's contract for `alloc_zeroed` is passed through unchanged.
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        Self::record(Some(new_size), layout.size());
        // SAFETY: the caller's contract for `realloc` is passed through unchanged.
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        Self::record(None, layout.size());
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
    let (value, counted) = CountingAllocator::measure(Counted::default(), measured);
    (value, counted.allocations)
}

/// The heap bytes this thread holds from what it allocated inside [`Self::measure`], net of what
/// it freed there, summed over every measured section. Memory allocated outside and freed inside
/// counts as negative.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LiveBytes {
    live: isize,
    peak: isize,
}

impl LiveBytes {
    /// Runs `measured`, counting the allocations and frees this thread makes during it.
    pub(crate) fn measure<T>(&mut self, measured: impl FnOnce() -> T) -> T {
        let start = Counted {
            live: self.live,
            peak: self.peak,
            ..Counted::default()
        };
        let (value, counted) = CountingAllocator::measure(start, measured);
        self.live = counted.live;
        self.peak = counted.peak;
        value
    }

    /// The bytes held now.
    pub(crate) fn live(&self) -> isize {
        self.live
    }

    /// The most bytes held at any moment of a measured section.
    pub(crate) fn peak(&self) -> isize {
        self.peak
    }
}
