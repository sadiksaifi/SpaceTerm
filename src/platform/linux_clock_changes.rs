//! The system clock's changes, read from a realtime timer that the kernel cancels on each clock
//! change, for schedules kept in wall-clock time.
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

/// A time no schedule reaches, so the timer never expires and only a clock change ends a read.
const NEVER: libc::time_t = 4_102_444_800;
/// How far past the clock the timer is armed when the clock is already past [`NEVER`].
const CENTURY: libc::time_t = 100 * 366 * 24 * 60 * 60;

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

/// The deadline to arm: [`NEVER`], or a century past `now` once the clock is past it, so an armed
/// timer is never already expired.
fn deadline(now: libc::time_t) -> libc::time_t {
    NEVER.max(now.saturating_add(CENTURY))
}

fn realtime() -> libc::time_t {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: writes the current realtime clock into the live local value.
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
    now.tv_sec
}

/// Arms the timer to expire at `deadline`, cancelled by any clock change. Returns the error
/// number on failure.
fn arm(timer: &OwnedFd, deadline: libc::time_t) -> Result<(), i32> {
    let value = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: deadline,
            tv_nsec: 0,
        },
    };
    // SAFETY: the descriptor is open and `value` outlives the call.
    let armed = unsafe {
        libc::timerfd_settime(
            timer.as_raw_fd(),
            libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
            &value,
            std::ptr::null_mut(),
        )
    };
    if armed < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default());
    }
    Ok(())
}

/// Blocks until the timer expires, returning `Ok`, or until a clock change or a failure ends the
/// read, returning its error number.
fn read(timer: &OwnedFd) -> Result<(), i32> {
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
        return Ok(());
    }
    Err(std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or_default())
}

/// Arms the timer and blocks until the clock changes. Returns `false` when the timer fails.
fn wait(timer: &OwnedFd) -> bool {
    loop {
        match arm(timer, deadline(realtime())) {
            Ok(()) => {}
            Err(error) => return error == libc::ECANCELED,
        }
        match read(timer) {
            // The far timer expired, which only a clock past it causes. Arm it past the clock.
            Ok(()) => {}
            Err(libc::ECANCELED) => return true,
            Err(libc::EINTR) => {}
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readable(timer: &OwnedFd) -> bool {
        let mut poll = libc::pollfd {
            fd: timer.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: polls one live descriptor without waiting.
        unsafe { libc::poll(&mut poll, 1, 0) == 1 }
    }

    /// An expired timer is armed again past the clock, so the next read blocks instead of
    /// returning at once, whatever the clock reads.
    #[test]
    fn an_expired_timer_is_armed_past_the_clock_so_the_next_read_blocks() {
        let timer = timer().unwrap();
        let now = realtime();
        arm(&timer, now - 1).unwrap();
        assert!(readable(&timer), "a past deadline expires at once");
        read(&timer).unwrap();

        arm(&timer, deadline(now)).unwrap();
        assert!(
            !readable(&timer),
            "the next deadline lies ahead of the clock"
        );

        for now in [0, NEVER - 1, NEVER, NEVER + 1, 10 * NEVER] {
            assert!(deadline(now) > now, "the deadline for {now} is ahead of it");
        }
        assert_eq!(
            deadline(now),
            NEVER,
            "an ordinary clock arms the fixed far time"
        );
    }
}
