use std::ffi::{CStr, OsStr};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct CheckGate {
    endpoint: PathBuf,
    foreign: PathBuf,
    identity: (u64, u64),
    checked: Arc<Mutex<Option<PathBuf>>>,
}

static CHECK_GATE: Mutex<Option<CheckGate>> = Mutex::new(None);

pub struct CleanupRace {
    endpoint: PathBuf,
    checked: Arc<Mutex<Option<PathBuf>>>,
}

impl CleanupRace {
    pub fn arm(endpoint: &Path, foreign: &Path) -> Self {
        let metadata = fs::symlink_metadata(endpoint).unwrap();
        let checked = Arc::new(Mutex::new(None));
        let mut gate = CHECK_GATE.lock().unwrap();
        assert!(gate.is_none());
        *gate = Some(CheckGate {
            endpoint: endpoint.to_owned(),
            foreign: foreign.to_owned(),
            identity: (metadata.dev(), metadata.ino()),
            checked: Arc::clone(&checked),
        });
        Self {
            endpoint: endpoint.to_owned(),
            checked,
        }
    }

    pub fn restore_preserved_entry(&self) {
        let checked = self.checked.lock().unwrap();
        let path = checked
            .as_ref()
            .expect("the native cleanup check did not run");
        if path != &self.endpoint && path.exists() {
            fs::rename(path, &self.endpoint).unwrap();
        }
    }
}

impl Drop for CleanupRace {
    fn drop(&mut self) {
        CHECK_GATE.lock().unwrap().take();
    }
}

// Replace the entry after the real status syscall has returned the owned inode,
// before production can resolve a pathname for removal. The gate is confined to
// this test executable and never changes the returned native status snapshot.
#[unsafe(export_name = "fstatat")]
unsafe extern "C" fn check_entry(
    directory: libc::c_int,
    name: *const libc::c_char,
    status: *mut libc::stat,
    flags: libc::c_int,
) -> libc::c_int {
    // SAFETY: forward the caller's unchanged native arguments.
    let result = unsafe { native_fstatat(directory, name, status, flags) };
    if result == 0 {
        // SAFETY: successful fstatat initialized status; its name is NUL-terminated.
        let (identity, name) = unsafe {
            (
                (device(&*status), (*status).st_ino),
                CStr::from_ptr(name).to_bytes(),
            )
        };
        let gate = CHECK_GATE.lock().ok().and_then(|mut gate| {
            let matches = gate.as_ref().is_some_and(|gate| {
                identity == gate.identity
                    && (name == gate.endpoint.file_name().unwrap().as_bytes()
                        || name.starts_with(b".s-"))
            });
            if matches { gate.take() } else { None }
        });
        if let Some(gate) = gate {
            // SAFETY: preserve the native thread's errno across test-only renames.
            let error = unsafe { *errno() };
            let checked = gate
                .endpoint
                .parent()
                .unwrap()
                .join(OsStr::from_bytes(name));
            fs::rename(&checked, gate.endpoint.with_file_name("owned-displaced")).unwrap();
            fs::rename(&gate.foreign, &checked).unwrap();
            *gate.checked.lock().unwrap() = Some(checked);
            // SAFETY: errno still points to the current native thread's value.
            unsafe { *errno() = error };
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn device(status: &libc::stat) -> u64 {
    status.st_dev
}

#[cfg(target_os = "macos")]
fn device(status: &libc::stat) -> u64 {
    status.st_dev as u64
}

#[cfg(target_os = "linux")]
unsafe fn native_fstatat(
    directory: libc::c_int,
    name: *const libc::c_char,
    status: *mut libc::stat,
    flags: libc::c_int,
) -> libc::c_int {
    // SAFETY: forward the original fstatat arguments to the real syscall.
    unsafe { libc::syscall(libc::SYS_newfstatat, directory, name, status, flags) as libc::c_int }
}

#[cfg(target_os = "macos")]
unsafe fn native_fstatat(
    directory: libc::c_int,
    name: *const libc::c_char,
    status: *mut libc::stat,
    flags: libc::c_int,
) -> libc::c_int {
    type Fstatat = unsafe extern "C" fn(
        libc::c_int,
        *const libc::c_char,
        *mut libc::stat,
        libc::c_int,
    ) -> libc::c_int;
    static FSTATAT: std::sync::OnceLock<Fstatat> = std::sync::OnceLock::new();
    let fstatat = FSTATAT.get_or_init(|| {
        // SAFETY: RTLD_NEXT resolves the system function, not this test definition.
        let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"fstatat".as_ptr()) };
        assert!(!symbol.is_null());
        // SAFETY: the native symbol has fstatat's declared function signature.
        unsafe { std::mem::transmute::<*mut libc::c_void, Fstatat>(symbol) }
    });
    // SAFETY: forward the original arguments to the system function.
    unsafe { fstatat(directory, name, status, flags) }
}

#[cfg(target_os = "linux")]
unsafe fn errno() -> *mut libc::c_int {
    // SAFETY: libc returns the current thread's errno pointer.
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn errno() -> *mut libc::c_int {
    // SAFETY: libc returns the current thread's errno pointer.
    unsafe { libc::__error() }
}
