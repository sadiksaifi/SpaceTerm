//! The system clock's changes, read from a realtime timer that the kernel cancels on each clock
//! change, for schedules kept in wall-clock time.
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

/// A time no schedule reaches, so the timer never expires and only a clock change ends a read.
const NEVER: libc::time_t = 4_102_444_800;

/// Calls `changed` after each change to the system clock, on a thread that sleeps in `read`
/// between changes and ends when the timer fails.
pub(crate) fn observe(changed: Box<dyn Fn() + Send + Sync>) {
    let _ = std::thread::Builder::new()
        .name("clock-changes".into())
        .spawn(move || {
            let Some(timer) = timer() else {
                return;
            };
            while wait(&timer) {
                changed();
            }
        });
}

fn timer() -> Option<OwnedFd> {
    // SAFETY: plain syscall with constant arguments; a valid result is a new descriptor.
    let fd = unsafe { libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC) };
    // SAFETY: a non-negative result is an open descriptor this function now owns.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Arms the timer and blocks until the clock changes. Returns `false` when the timer fails.
fn wait(timer: &OwnedFd) -> bool {
    let never = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: NEVER,
            tv_nsec: 0,
        },
    };
    loop {
        // SAFETY: the descriptor is open and `never` outlives the call.
        let armed = unsafe {
            libc::timerfd_settime(
                timer.as_raw_fd(),
                libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
                &never,
                std::ptr::null_mut(),
            )
        };
        if armed < 0 {
            return std::io::Error::last_os_error().raw_os_error() == Some(libc::ECANCELED);
        }
        let mut expirations = 0u64;
        // SAFETY: reads at most eight bytes into the live local integer.
        let read = unsafe {
            libc::read(
                timer.as_raw_fd(),
                (&raw mut expirations).cast(),
                std::mem::size_of::<u64>(),
            )
        };
        if read >= 0 {
            // The far timer expired, which only a clock past it causes. Arm it again.
            continue;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ECANCELED) => return true,
            Some(libc::EINTR) => {}
            _ => return false,
        }
    }
}
