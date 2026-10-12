use super::CapabilityFailure;
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs::OpenOptions,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::Path,
};

struct Library(*mut c_void);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.0) };
    }
}
impl Library {
    fn open() -> Result<Self, CapabilityFailure> {
        let library = unsafe {
            libc::dlopen(
                c"libsystemd.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        if library.is_null() {
            Err(CapabilityFailure::NativeUnavailable)
        } else {
            Ok(Self(library))
        }
    }
    fn symbol(&self, name: &CStr) -> Result<*mut c_void, CapabilityFailure> {
        let symbol = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        if symbol.is_null() {
            Err(CapabilityFailure::NativeUnavailable)
        } else {
            Ok(symbol)
        }
    }
}
struct Text(*mut c_char);
impl Drop for Text {
    fn drop(&mut self) {
        unsafe { libc::free(self.0.cast()) };
    }
}
fn text(value: &Text) -> Result<&CStr, CapabilityFailure> {
    if value.0.is_null() {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let value = unsafe { CStr::from_ptr(value.0) };
    if value.to_bytes().len() > 128 {
        return Err(CapabilityFailure::LimitExceeded);
    }
    Ok(value)
}
fn read_bounded(path: &Path, protected: bool) -> Result<Vec<u8>, CapabilityFailure> {
    if protected {
        for ancestor in path.ancestors() {
            let metadata = std::fs::symlink_metadata(ancestor)
                .map_err(|_| CapabilityFailure::NativeUnavailable)?;
            if metadata.file_type().is_symlink()
                || metadata.uid() != 0
                || metadata.mode() & 0o022 != 0
            {
                return Err(CapabilityFailure::NativeUnavailable);
            }
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    if bytes.len() > 8192 {
        return Err(CapabilityFailure::LimitExceeded);
    }
    Ok(bytes)
}
fn leader(id: &CStr) -> Result<i32, CapabilityFailure> {
    let id = id.to_bytes();
    if id.is_empty() || id.len() > 64 || !id.iter().all(|b| b.is_ascii_alphanumeric()) {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let path = Path::new("/run/systemd/sessions").join(std::ffi::OsStr::from_bytes(id));
    let bytes = read_bounded(&path, true)?;
    let mut leader = None;
    for line in bytes.split(|b| *b == b'\n') {
        if let Some(value) = line.strip_prefix(b"LEADER=") {
            let value = std::str::from_utf8(value)
                .ok()
                .and_then(|v| v.parse::<i32>().ok())
                .filter(|v| *v > 0)
                .ok_or(CapabilityFailure::NativeUnavailable)?;
            if leader.replace(value).is_some() {
                return Err(CapabilityFailure::NativeUnavailable);
            }
        }
    }
    leader.ok_or(CapabilityFailure::NativeUnavailable)
}
fn start_time(pid: i32, uid: u32) -> Result<u64, CapabilityFailure> {
    let path = std::path::PathBuf::from(format!("/proc/{pid}/stat"));
    let owner = std::fs::metadata(&path)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?
        .uid();
    // GDM can retain a root-owned session leader. Logind membership, checked
    // independently below, binds that leader to this account's session.
    if owner != uid && owner != 0 {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let bytes = read_bounded(&path, false)?;
    let end = bytes
        .iter()
        .rposition(|b| *b == b')')
        .ok_or(CapabilityFailure::NativeUnavailable)?;
    std::str::from_utf8(&bytes[end + 1..])
        .ok()
        .and_then(|v| v.split_ascii_whitespace().nth(19))
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .ok_or(CapabilityFailure::NativeUnavailable)
}
struct Observed {
    fingerprint: [u8; 16],
    descriptor: OwnedFd,
    record: std::fs::File,
}
pub(super) struct SessionReference {
    fingerprint: [u8; 16],
    incarnation: [u8; 16],
    descriptor: OwnedFd,
    _record: std::fs::File,
}
pub(super) fn observe(
    uid: u32,
    retained: &mut Option<SessionReference>,
) -> Result<Option<[u8; 16]>, CapabilityFailure> {
    let Some(observed) = current(uid)? else {
        *retained = None;
        return Ok(None);
    };
    if let Some(previous) = retained {
        let mut live = libc::pollfd {
            fd: previous.descriptor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if previous.fingerprint == observed.fingerprint
            && unsafe { libc::poll(&mut live, 1, 0) } == 0
        {
            return Ok(Some(previous.incarnation));
        }
    }
    // A retained pidfd detects replacement even if a recycled PID has the same
    // coarse procfs start tick. The retained logind record prevents inode reuse.
    let mut incarnation = [0; 16];
    getrandom::fill(&mut incarnation).map_err(|_| CapabilityFailure::NativeUnavailable)?;
    if incarnation == [0; 16] {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    *retained = Some(SessionReference {
        fingerprint: observed.fingerprint,
        incarnation,
        descriptor: observed.descriptor,
        _record: observed.record,
    });
    Ok(Some(incarnation))
}
fn current(uid: u32) -> Result<Option<Observed>, CapabilityFailure> {
    let library = Library::open()?;
    type Display = unsafe extern "C" fn(u32, *mut *mut c_char) -> i32;
    type User = unsafe extern "C" fn(*const c_char, *mut u32) -> i32;
    type Active = unsafe extern "C" fn(*const c_char) -> i32;
    type Property = unsafe extern "C" fn(*const c_char, *mut *mut c_char) -> i32;
    type PidSession = unsafe extern "C" fn(i32, *mut *mut c_char) -> i32;
    let display: Display = unsafe { std::mem::transmute(library.symbol(c"sd_uid_get_display")?) };
    let user: User = unsafe { std::mem::transmute(library.symbol(c"sd_session_get_uid")?) };
    let active: Active = unsafe { std::mem::transmute(library.symbol(c"sd_session_is_active")?) };
    let kind: Property = unsafe { std::mem::transmute(library.symbol(c"sd_session_get_type")?) };
    let class: Property = unsafe { std::mem::transmute(library.symbol(c"sd_session_get_class")?) };
    let pid_session: PidSession =
        unsafe { std::mem::transmute(library.symbol(c"sd_pid_get_session")?) };
    let mut session = Text(std::ptr::null_mut());
    let result = unsafe { display(uid, &mut session.0) };
    if result < 0 {
        return if [-libc::ENXIO, -libc::ENOENT, -libc::ENODATA].contains(&result) {
            Ok(None)
        } else {
            Err(CapabilityFailure::NativeUnavailable)
        };
    }
    let id = CString::new(text(&session)?.to_bytes())
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    let mut observed_uid = u32::MAX;
    let mut session_type = Text(std::ptr::null_mut());
    let mut session_class = Text(std::ptr::null_mut());
    if unsafe { user(id.as_ptr(), &mut observed_uid) } < 0
        || observed_uid != uid
        || unsafe { active(id.as_ptr()) } != 1
    {
        return Ok(None);
    }
    if unsafe { kind(id.as_ptr(), &mut session_type.0) } < 0
        || unsafe { class(id.as_ptr(), &mut session_class.0) } < 0
    {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    if ![b"x11".as_slice(), b"wayland".as_slice()].contains(&text(&session_type)?.to_bytes())
        || ![b"user".as_slice(), b"user-early".as_slice()]
            .contains(&text(&session_class)?.to_bytes())
    {
        return Ok(None);
    }
    let pid = leader(&id)?;
    let session_path =
        Path::new("/run/systemd/sessions").join(std::ffi::OsStr::from_bytes(id.to_bytes()));
    let record = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&session_path)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    let generation = record
        .metadata()
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    if generation.uid() != 0 || generation.mode() & 0o022 != 0 {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if descriptor < 0 {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor as i32) };
    let mut leader_session = Text(std::ptr::null_mut());
    if unsafe { pid_session(pid, &mut leader_session.0) } < 0
        || text(&leader_session)? != id.as_c_str()
    {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let before = start_time(pid, uid)?;
    let boot = read_bounded(Path::new("/proc/sys/kernel/random/boot_id"), false)?;
    if boot.len() != 37
        || boot[36] != b'\n'
        || !boot[..36]
            .iter()
            .all(|b| b.is_ascii_hexdigit() || *b == b'-')
    {
        return Err(CapabilityFailure::NativeUnavailable);
    }
    let mut poll = libc::pollfd {
        fd: descriptor.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, 0) } != 0
        || start_time(pid, uid)? != before
        || unsafe { active(id.as_ptr()) } != 1
        || leader(&id)? != pid
    {
        return Err(CapabilityFailure::StaleAssessment);
    }
    let mut current = Text(std::ptr::null_mut());
    let mut final_leader_session = Text(std::ptr::null_mut());
    if unsafe { display(uid, &mut current.0) } < 0
        || text(&current)? != id.as_c_str()
        || unsafe { pid_session(pid, &mut final_leader_session.0) } < 0
        || text(&final_leader_session)? != id.as_c_str()
    {
        return Err(CapabilityFailure::StaleAssessment);
    }
    let final_generation = std::fs::symlink_metadata(&session_path)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    if generation.dev() != final_generation.dev()
        || generation.ino() != final_generation.ino()
        || generation.ctime() != final_generation.ctime()
        || generation.ctime_nsec() != final_generation.ctime_nsec()
    {
        return Err(CapabilityFailure::StaleAssessment);
    }
    let mut hash = Sha256::new();
    hash.update(b"SpaceTerm logind GUI session v1\0");
    hash.update(boot);
    hash.update(uid.to_le_bytes());
    hash.update(id.to_bytes());
    hash.update(pid.to_le_bytes());
    hash.update(before.to_le_bytes());
    hash.update(generation.dev().to_le_bytes());
    hash.update(generation.ino().to_le_bytes());
    hash.update(generation.ctime().to_le_bytes());
    hash.update(generation.ctime_nsec().to_le_bytes());
    Ok(Some(Observed {
        fingerprint: hash.finalize()[..16]
            .try_into()
            .map_err(|_| CapabilityFailure::NativeUnavailable)?,
        descriptor,
        record,
    }))
}
