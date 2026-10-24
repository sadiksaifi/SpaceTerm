//! Convert Unix timestamps through the host's local calendar and time zone.

use crate::desktop_profile::LocalTime;

pub(super) fn local_time(unix_seconds: u64) -> Option<LocalTime> {
    let seconds = libc::time_t::try_from(unix_seconds).ok()?;
    // SAFETY: `tm` is a plain C struct that localtime_r fully initializes on success, and both
    // pointers stay valid for the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(LocalTime {
        year: tm.tm_year,
        month: usize::try_from(tm.tm_mon).ok()?,
        day: u32::try_from(tm.tm_mday).ok()?,
        hour: u32::try_from(tm.tm_hour).ok()?,
        minute: u32::try_from(tm.tm_min).ok()?,
    })
}
