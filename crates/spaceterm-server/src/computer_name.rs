//! The name of the computer a client runs on, read from the operating system and never from the
//! environment.

/// Reads this computer's name. It may change while a client runs, so callers ask at each use.
pub(crate) trait ComputerName: Send + Sync {
    fn computer_name(&self) -> Option<String>;
}

/// The name a person gave this computer, or its host name where the system keeps no other.
pub(crate) struct SystemComputerName;

impl ComputerName for SystemComputerName {
    fn computer_name(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        if let Some(name) = macos::computer_name() {
            return Some(name);
        }
        host_name()
    }
}

fn host_name() -> Option<String> {
    let mut bytes = [0u8; 256];
    // SAFETY: gethostname writes at most the given length into the retained buffer.
    if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return None;
    }
    let end = bytes.iter().position(|b| *b == 0)?;
    String::from_utf8(bytes[..end].to_vec()).ok()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_char, c_void};

    const UTF8: u32 = 0x0800_0100;

    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" {
        fn SCDynamicStoreCopyComputerName(
            store: *const c_void,
            encoding: *mut u32,
        ) -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }

    /// The Computer Name from Sharing settings, such as "Sadik’s MacBook Pro".
    pub(super) fn computer_name() -> Option<String> {
        // SAFETY: a null store asks for the current value, and the copied string is released once.
        let name =
            unsafe { SCDynamicStoreCopyComputerName(std::ptr::null(), std::ptr::null_mut()) };
        if name.is_null() {
            return None;
        }
        let mut bytes = [0u8; 1024];
        // SAFETY: the buffer outlives the call, which writes at most its length.
        let copied = unsafe {
            CFStringGetCString(name, bytes.as_mut_ptr().cast(), bytes.len() as isize, UTF8)
        };
        // SAFETY: `name` came from a Copy function and is released exactly once.
        unsafe { CFRelease(name) };
        if copied == 0 {
            return None;
        }
        let end = bytes.iter().position(|b| *b == 0)?;
        String::from_utf8(bytes[..end].to_vec()).ok()
    }
}
