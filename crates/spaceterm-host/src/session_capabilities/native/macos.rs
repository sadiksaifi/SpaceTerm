use super::CapabilityFailure;
use sha2::{Digest, Sha256};
use std::{ffi::c_void, ptr};

type Cf = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Cf);
    fn CFStringCreateWithCString(allocator: Cf, text: *const libc::c_char, encoding: u32) -> Cf;
    fn CFDictionaryGetValue(dictionary: Cf, key: Cf) -> Cf;
    fn CFArrayGetCount(array: Cf) -> isize;
    fn CFArrayGetValueAtIndex(array: Cf, index: isize) -> Cf;
    fn CFNumberGetValue(number: Cf, kind: i32, value: *mut c_void) -> bool;
    fn CFBooleanGetValue(value: Cf) -> bool;
    fn CFGetTypeID(value: Cf) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
}
#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCreate(allocator: Cf, name: Cf, callback: Cf, context: Cf) -> Cf;
    fn SCDynamicStoreCopyValue(store: Cf, key: Cf) -> Cf;
}
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SessionGetInfo(session: u32, actual: *mut u32, attributes: *mut u32) -> i32;
}
unsafe extern "C" {
    fn proc_listallpids(buffer: *mut c_void, size: i32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, argument: u64, buffer: *mut c_void, size: i32) -> i32;
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
}
struct Owned(Cf);
impl Owned {
    fn new(value: Cf) -> Result<Self, CapabilityFailure> {
        if value.is_null() {
            Err(CapabilityFailure::NativeUnavailable)
        } else {
            Ok(Self(value))
        }
    }
    fn text(value: &std::ffi::CStr) -> Result<Self, CapabilityFailure> {
        Self::new(unsafe { CFStringCreateWithCString(ptr::null(), value.as_ptr(), 0x0800_0100) })
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}
fn value(dictionary: Cf, name: &std::ffi::CStr) -> Result<Cf, CapabilityFailure> {
    let name = Owned::text(name)?;
    Ok(unsafe { CFDictionaryGetValue(dictionary, name.0) })
}
fn number(dictionary: Cf, name: &std::ffi::CStr) -> Result<u32, CapabilityFailure> {
    let number = value(dictionary, name)?;
    let mut result = 0_u32;
    if number.is_null()
        || unsafe {
            CFGetTypeID(number) != CFNumberGetTypeID()
                || !CFNumberGetValue(number, 3, (&mut result as *mut u32).cast())
        }
    {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    Ok(result)
}
#[repr(C)]
#[derive(Default)]
struct Identity {
    uuid: [u8; 16],
    unique: u64,
    parent: u64,
    version: u32,
    parent_version: u32,
    reserved: [u64; 2],
}
#[repr(C)]
#[derive(Default)]
struct ShortBsd {
    pid: u32,
    parent: u32,
    group: u32,
    status: u32,
    name: [u8; 16],
    flags: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    reserved: u32,
}
fn loginwindow(uid: u32) -> Result<u64, CapabilityFailure> {
    let mut pids = vec![0_i32; 8193];
    let count = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    if count <= 0 || count as usize >= pids.len() {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let mut found = None;
    for &pid in &pids[..count as usize] {
        let mut bsd = ShortBsd::default();
        let mut identity = Identity::default();
        let mut path = [0_u8; 4096];
        if pid <= 0
            || unsafe { proc_pidinfo(pid, 13, 0, (&mut bsd as *mut ShortBsd).cast(), 64) } != 64
            || bsd.uid != uid
            || unsafe { proc_pidpath(pid, path.as_mut_ptr().cast(), 4096) } <= 0
        {
            continue;
        }
        let expected = b"/System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow\0";
        if !path.starts_with(expected) {
            continue;
        }
        if unsafe { proc_pidinfo(pid, 17, 0, (&mut identity as *mut Identity).cast(), 56) } != 56
            || identity.unique == 0
            || found.is_some()
        {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        found = Some(identity.unique & !(1_u64 << 63));
    }
    found.ok_or(CapabilityFailure::NativeUnavailable)
}
pub(super) fn observe(uid: u32) -> Result<Option<[u8; 16]>, CapabilityFailure> {
    let name = Owned::text(c"SpaceTerm narrow session observer")?;
    let store =
        Owned::new(unsafe { SCDynamicStoreCreate(ptr::null(), name.0, ptr::null(), ptr::null()) })?;
    let key = Owned::text(c"State:/Users/ConsoleUser")?;
    let dictionary = Owned::new(unsafe { SCDynamicStoreCopyValue(store.0, key.0) })?;
    if unsafe { CFGetTypeID(dictionary.0) != CFDictionaryGetTypeID() } {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let sessions = value(dictionary.0, c"SessionInfo")?;
    if sessions.is_null() {
        return Ok(None);
    }
    if unsafe { CFGetTypeID(sessions) != CFArrayGetTypeID() } {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let count = unsafe { CFArrayGetCount(sessions) };
    if !(0..=64).contains(&count) {
        return Err(CapabilityFailure::LimitExceeded);
    }
    let mut result = None;
    for index in 0..count {
        let session = unsafe { CFArrayGetValueAtIndex(sessions, index) };
        if unsafe { CFGetTypeID(session) != CFDictionaryGetTypeID() } {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        let on_console = value(session, c"kCGSSessionOnConsoleKey")?;
        if on_console.is_null() || unsafe { CFGetTypeID(on_console) != CFBooleanGetTypeID() } {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        if !unsafe { CFBooleanGetValue(on_console) }
            || number(session, c"kCGSSessionUserIDKey")? != uid
        {
            continue;
        }
        let audit = number(session, c"kCGSSessionAuditIDKey")?;
        let mut actual = 0;
        let mut attributes = 0;
        if audit == 0
            || audit == u32::MAX
            || unsafe { SessionGetInfo(audit, &mut actual, &mut attributes) } != 0
            || actual != audit
            || attributes & 0x10 == 0
            || attributes & 1 != 0
        {
            return Ok(None);
        }
        let before = loginwindow(uid)?;
        let mut boot = [0_u8; 64];
        let mut length = boot.len();
        if unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                boot.as_mut_ptr().cast(),
                &mut length,
                ptr::null_mut(),
                0,
            )
        } != 0
            || length < 2
            || length > boot.len()
            || boot[length - 1] != 0
            || loginwindow(uid)? != before
        {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        let mut hash = Sha256::new();
        hash.update(b"SpaceTerm macOS GUI session v1\0");
        hash.update(&boot[..length]);
        hash.update(uid.to_le_bytes());
        hash.update(audit.to_le_bytes());
        hash.update(before.to_le_bytes());
        let digest = hash.finalize();
        if result
            .replace(
                digest[..16]
                    .try_into()
                    .map_err(|_| CapabilityFailure::NativeUnavailable)?,
            )
            .is_some()
        {
            return Err(CapabilityFailure::NativeUnavailable);
        }
    }
    Ok(result)
}
