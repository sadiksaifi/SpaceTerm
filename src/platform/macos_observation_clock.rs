#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_continuous_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

pub(crate) fn continuous_time_ns() -> Option<u64> {
    static TIMEBASE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    let &(numer, denom) = TIMEBASE
        .get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            // SAFETY: `info` is writable for the duration of this synchronous system call.
            let status = unsafe { mach_timebase_info(&raw mut info) };
            (status == 0 && info.numer != 0 && info.denom != 0).then_some((info.numer, info.denom))
        })
        .as_ref()?;
    // SAFETY: `mach_continuous_time` has no arguments and is available on the supported macOS.
    let ticks = unsafe { mach_continuous_time() };
    let nanoseconds = u128::from(ticks)
        .checked_mul(u128::from(numer))?
        .checked_div(u128::from(denom))?;
    u64::try_from(nanoseconds).ok()
}

#[derive(Debug)]
pub(crate) struct ContinuousClock;
impl crate::observation::ContinuousClock for ContinuousClock {
    fn now_ns(&self) -> Option<u64> {
        continuous_time_ns()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuous_clock_should_return_nondecreasing_checked_nanoseconds() {
        let first = continuous_time_ns().unwrap();
        assert!(first > 0);
        assert!(continuous_time_ns().unwrap() >= first);
    }
}
