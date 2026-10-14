use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::ffi;

use super::StorageFailure;
use super::directory::{Directory, Entry, Kind, io_failure, mutex};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct Context {
    pub(super) directory: Arc<Directory>,
    pub(super) read_only: bool,
    native_vfs: usize,
    canonical: CString,
    failure: Mutex<Option<StorageFailure>>,
    pub(super) uncertain: AtomicBool,
    pub(super) created: AtomicBool,
    #[cfg(any(target_os = "macos", test, feature = "test-support"))]
    pub(super) database: Mutex<std::sync::Weak<Entry>>,
    #[cfg(any(test, feature = "test-support"))]
    pub(super) faults: Mutex<Option<(FaultPoint, usize)>>,
    #[cfg(any(test, feature = "test-support"))]
    pub(super) barrier: Mutex<Option<super::testing::Barrier>>,
    #[cfg(any(test, feature = "test-support"))]
    pub(super) sql_calls: AtomicU64,
    #[cfg(all(any(test, feature = "test-support"), target_os = "macos"))]
    pub(super) durability_events: Mutex<Vec<DurabilityEvent>>,
}

#[cfg(all(any(test, feature = "test-support"), target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DurabilityEvent {
    JournalSync,
    DatabaseSync,
    JournalDeleted,
    DirectorySync,
    DeleteFullSync,
}

#[cfg(any(test, feature = "test-support"))]
pub(super) use super::testing::FaultPoint;

impl Context {
    pub(super) fn reset(&self) {
        *mutex(&self.failure) = None;
        self.uncertain.store(false, Ordering::Relaxed);
    }

    pub(super) fn classify(&self, error: rusqlite::Error) -> StorageFailure {
        self.failure_or(super::classify_sqlite(error))
    }

    pub(super) fn failure_or(&self, fallback: StorageFailure) -> StorageFailure {
        mutex(&self.failure).take().unwrap_or(fallback)
    }

    fn fail(&self, failure: StorageFailure, code: i32, uncertain: bool) -> i32 {
        let mut recorded = mutex(&self.failure);
        if recorded.is_none() {
            *recorded = Some(failure);
        }
        if uncertain {
            self.uncertain.store(true, Ordering::Relaxed);
        }
        code
    }

    fn kind(&self, name: *const c_char) -> Result<Kind, StorageFailure> {
        if name.is_null() {
            return Err(StorageFailure::InvalidState);
        }
        // SAFETY: SQLite supplies a terminated filename to each VFS callback.
        let name = unsafe { CStr::from_ptr(name) }.to_bytes();
        for kind in [Kind::Database, Kind::Journal, Kind::Wal, Kind::Shm] {
            let suffix = &kind.name().to_bytes()[super::DATABASE_FILE_NAME.len()..];
            if name.strip_prefix(self.canonical.as_bytes()) == Some(suffix) {
                return Ok(kind);
            }
        }
        Err(StorageFailure::UnsafeStorage)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn hit(&self, point: FaultPoint) -> bool {
        let callback = {
            let mut barrier = mutex(&self.barrier);
            if barrier
                .as_ref()
                .is_some_and(|barrier| barrier.point == point)
            {
                barrier.take().map(|barrier| barrier.callback)
            } else {
                None
            }
        };
        if let Some(callback) = callback {
            callback();
        }
        let mut faults = mutex(&self.faults);
        if let Some((armed, remaining)) = faults.as_mut()
            && *armed == point
        {
            if *remaining == 0 {
                *faults = None;
                return true;
            }
            *remaining -= 1;
        }
        false
    }

    fn sync_directory(&self) -> Result<(), StorageFailure> {
        #[cfg(any(test, feature = "test-support"))]
        if self.hit(FaultPoint::DirectorySync) {
            return Err(StorageFailure::Io);
        }
        self.directory.sync()?;
        #[cfg(all(any(test, feature = "test-support"), target_os = "macos"))]
        mutex(&self.durability_events).push(DurabilityEvent::DirectorySync);
        Ok(())
    }

    fn sync_journal_deletion(&self) -> Result<(), StorageFailure> {
        self.sync_directory()?;
        #[cfg(target_os = "macos")]
        {
            // Directory fsync publishes the unlink to the device, but does not
            // flush its volatile cache. F_FULLFSYNC on the already-open database
            // supplies that final device barrier. Earlier journal/database syncs
            // cannot make a later namespace change durable. No path is reopened,
            // and an unsupported/failed barrier has no weaker fallback.
            let database = mutex(&self.database)
                .upgrade()
                .ok_or(StorageFailure::UnsafeStorage)?;
            database.verify(&self.directory)?;
            #[cfg(any(test, feature = "test-support"))]
            if self.hit(FaultPoint::DeleteFullSync) {
                return Err(StorageFailure::Io);
            }
            // SAFETY: this is SQLite's retained regular-file descriptor on the
            // same admitted local filesystem as the synchronized directory.
            if unsafe { libc::fcntl(database.fd(), libc::F_FULLFSYNC) } != 0 {
                return Err(io_failure());
            }
            #[cfg(any(test, feature = "test-support"))]
            mutex(&self.durability_events).push(DurabilityEvent::DeleteFullSync);
        }
        Ok(())
    }
}

pub(super) struct Registration {
    vfs: Box<ffi::sqlite3_vfs>,
    name: CString,
    pub(super) context: Arc<Context>,
}

// SAFETY: registration pointers address owned, stable boxes. A connection moves
// between threads, but rusqlite does not allow concurrent use of that connection.
// Shared directory/inode state is separately serialized by mutexes.
unsafe impl Send for Registration {}

impl Registration {
    pub(super) fn new(directory: Arc<Directory>, read_only: bool) -> Result<Self, StorageFailure> {
        // SAFETY: SQLite initializes its VFS registry internally. The native VFS
        // is borrowed only for clock, randomness, and sleep, never file access.
        let native = unsafe { ffi::sqlite3_vfs_find(c"unix".as_ptr()) };
        if native.is_null() {
            return Err(StorageFailure::Unavailable);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("spaceterm-storage-{id}"))
            .map_err(|_| StorageFailure::Unavailable)?;
        let canonical = CString::new(format!(
            "/spaceterm-storage-{id}/{}",
            super::DATABASE_FILE_NAME
        ))
        .map_err(|_| StorageFailure::Unavailable)?;
        let context = Arc::new(Context {
            directory,
            read_only,
            native_vfs: native as usize,
            canonical,
            failure: Mutex::new(None),
            uncertain: AtomicBool::new(false),
            created: AtomicBool::new(false),
            #[cfg(any(target_os = "macos", test, feature = "test-support"))]
            database: Mutex::new(std::sync::Weak::new()),
            #[cfg(any(test, feature = "test-support"))]
            faults: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            barrier: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            sql_calls: AtomicU64::new(0),
            #[cfg(all(any(test, feature = "test-support"), target_os = "macos"))]
            durability_events: Mutex::new(Vec::new()),
        });
        // SAFETY: zero is valid for optional callbacks and unused VFS fields.
        let mut vfs: Box<ffi::sqlite3_vfs> = Box::new(unsafe { std::mem::zeroed() });
        vfs.iVersion = 2;
        vfs.szOsFile = std::mem::size_of::<VfsFile>() as c_int;
        vfs.mxPathname = 128;
        vfs.zName = name.as_ptr();
        vfs.pAppData = Arc::as_ptr(&context).cast_mut().cast();
        vfs.xOpen = Some(open);
        vfs.xDelete = Some(delete);
        vfs.xAccess = Some(access);
        vfs.xFullPathname = Some(full_path);
        vfs.xRandomness = Some(randomness);
        vfs.xSleep = Some(sleep);
        vfs.xCurrentTime = Some(current_time);
        vfs.xCurrentTimeInt64 = Some(current_time_int64);
        vfs.xGetLastError = Some(last_error);
        // No extension loader, mmap, shared memory, or path-based file callbacks.
        if unsafe { ffi::sqlite3_vfs_register(&mut *vfs, 0) } != ffi::SQLITE_OK {
            return Err(StorageFailure::Unavailable);
        }
        Ok(Self { vfs, name, context })
    }

    pub(super) fn name(&self) -> Result<&str, StorageFailure> {
        self.name.to_str().map_err(|_| StorageFailure::Unavailable)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: Database drops its Connection before this registration. No
        // sqlite file or connection retains the VFS when it is unregistered.
        unsafe {
            ffi::sqlite3_vfs_unregister(&mut *self.vfs);
        }
    }
}

#[repr(C)]
struct VfsFile {
    base: ffi::sqlite3_file,
    opened: *mut OpenFile,
}

struct OpenFile {
    entry: Arc<Entry>,
    context: Arc<Context>,
    owner: u64,
    level: i32,
}

impl OpenFile {
    fn verify(&self) -> Result<(), StorageFailure> {
        if self.context.read_only {
            self.context.directory.check_frozen()?;
        }
        self.entry.verify(&self.context.directory)
    }
    fn fail(&self, failure: StorageFailure, code: i32, uncertain: bool) -> i32 {
        self.context.fail(failure, code, uncertain)
    }
}

// SAFETY: callbacks receive the registered VFS/file layouts, whose backing
// allocations outlive the callback. SQLite serializes use of each open file.
unsafe fn context<'a>(vfs: *mut ffi::sqlite3_vfs) -> &'a Context {
    unsafe { &*((*vfs).pAppData.cast::<Context>()) }
}
unsafe fn opened<'a>(file: *mut ffi::sqlite3_file) -> &'a mut OpenFile {
    unsafe { &mut *(*(file.cast::<VfsFile>())).opened }
}

unsafe extern "C" fn open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    let ctx = unsafe { context(vfs) };
    // SAFETY: SQLite allocated szOsFile bytes and expects a null method table on failure.
    unsafe {
        ptr::write(
            file.cast::<VfsFile>(),
            VfsFile {
                base: ffi::sqlite3_file {
                    pMethods: ptr::null(),
                },
                opened: ptr::null_mut(),
            },
        );
    }
    let kind = match ctx.kind(name) {
        Ok(kind) => kind,
        Err(error) => return ctx.fail(error, ffi::SQLITE_CANTOPEN, false),
    };
    let admitted = match kind {
        Kind::Database => flags & ffi::SQLITE_OPEN_MAIN_DB != 0,
        Kind::Journal => flags & ffi::SQLITE_OPEN_MAIN_JOURNAL != 0,
        _ => false,
    };
    let access_admitted = if ctx.read_only {
        kind == Kind::Database
            && flags & ffi::SQLITE_OPEN_READONLY != 0
            && flags & (ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE) == 0
    } else {
        flags & ffi::SQLITE_OPEN_READWRITE != 0
    };
    if !admitted || !access_admitted || flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0 {
        return ctx.fail(StorageFailure::InvalidState, ffi::SQLITE_CANTOPEN, false);
    }
    let result = if ctx.read_only {
        ctx.directory
            .open_read_only_entry()
            .map(|entry| (entry, false))
    } else {
        ctx.directory
            .open_entry_with_evidence(kind, flags & ffi::SQLITE_OPEN_CREATE != 0)
    };
    let (entry, created) = match result {
        Ok(entry) => entry,
        Err(error) => return ctx.fail(error, ffi::SQLITE_CANTOPEN, !ctx.read_only),
    };
    if kind == Kind::Database {
        ctx.created.store(created, Ordering::Relaxed);
    }
    #[cfg(any(target_os = "macos", test, feature = "test-support"))]
    if kind == Kind::Database {
        *mutex(&ctx.database) = Arc::downgrade(&entry);
    }
    // SAFETY: pAppData is an Arc allocation retained by Registration. This file
    // adds one strong reference and releases it in xClose.
    let ctx_arc = unsafe {
        Arc::increment_strong_count(ctx);
        Arc::from_raw(ctx)
    };
    let state = Box::new(OpenFile {
        entry,
        context: ctx_arc,
        owner: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        level: ffi::SQLITE_LOCK_NONE,
    });
    unsafe {
        (*file.cast::<VfsFile>()).opened = Box::into_raw(state);
        (*file).pMethods = &IO_METHODS;
        if !out.is_null() {
            *out = flags;
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn delete(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _sync: c_int,
) -> c_int {
    let ctx = unsafe { context(vfs) };
    if ctx.read_only {
        return ctx.fail(
            StorageFailure::RecoveryRequired,
            ffi::SQLITE_READONLY,
            false,
        );
    }
    let result = ctx
        .kind(name)
        .and_then(|kind| ctx.directory.delete(kind))
        .and_then(|()| {
            #[cfg(all(any(test, feature = "test-support"), target_os = "macos"))]
            mutex(&ctx.durability_events).push(DurabilityEvent::JournalDeleted);
            ctx.sync_journal_deletion()
        });
    match result {
        Ok(()) => ffi::SQLITE_OK,
        Err(error) => ctx.fail(error, ffi::SQLITE_IOERR_DELETE, true),
    }
}

unsafe extern "C" fn access(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _flags: c_int,
    out: *mut c_int,
) -> c_int {
    let ctx = unsafe { context(vfs) };
    unsafe {
        *out = 0;
    }
    if ctx.read_only
        && let Err(error) = ctx.directory.check_frozen()
    {
        return ctx.fail(error, ffi::SQLITE_IOERR_ACCESS, false);
    }
    match ctx.kind(name).and_then(|kind| ctx.directory.check(kind)) {
        Ok(identity) => {
            unsafe {
                *out = i32::from(identity.is_some());
            }
            ffi::SQLITE_OK
        }
        Err(error) => ctx.fail(error, ffi::SQLITE_IOERR_ACCESS, false),
    }
}

unsafe extern "C" fn full_path(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    length: c_int,
    out: *mut c_char,
) -> c_int {
    let ctx = unsafe { context(vfs) };
    let input = unsafe { CStr::from_ptr(name) };
    if input != Kind::Database.name() && input != ctx.canonical.as_c_str() {
        return ctx.fail(StorageFailure::UnsafeStorage, ffi::SQLITE_CANTOPEN, false);
    }
    let bytes = ctx.canonical.as_bytes_with_nul();
    if length < 0 || bytes.len() > length as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr().cast(), out, bytes.len());
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn randomness(
    vfs: *mut ffi::sqlite3_vfs,
    amount: c_int,
    out: *mut c_char,
) -> c_int {
    let base = unsafe { context(vfs) }.native_vfs as *mut ffi::sqlite3_vfs;
    unsafe {
        (*base)
            .xRandomness
            .map_or(0, |callback| callback(base, amount, out))
    }
}
unsafe extern "C" fn sleep(vfs: *mut ffi::sqlite3_vfs, amount: c_int) -> c_int {
    let base = unsafe { context(vfs) }.native_vfs as *mut ffi::sqlite3_vfs;
    unsafe { (*base).xSleep.map_or(0, |callback| callback(base, amount)) }
}
unsafe extern "C" fn current_time(vfs: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    let base = unsafe { context(vfs) }.native_vfs as *mut ffi::sqlite3_vfs;
    unsafe {
        (*base)
            .xCurrentTime
            .map_or(ffi::SQLITE_ERROR, |callback| callback(base, out))
    }
}
unsafe extern "C" fn current_time_int64(vfs: *mut ffi::sqlite3_vfs, out: *mut i64) -> c_int {
    let base = unsafe { context(vfs) }.native_vfs as *mut ffi::sqlite3_vfs;
    unsafe {
        (*base)
            .xCurrentTimeInt64
            .map_or(ffi::SQLITE_ERROR, |callback| callback(base, out))
    }
}
unsafe extern "C" fn last_error(
    _vfs: *mut ffi::sqlite3_vfs,
    length: c_int,
    out: *mut c_char,
) -> c_int {
    if length > 0 {
        unsafe {
            *out = 0;
        }
    }
    0
}

unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: each successful xOpen installs exactly one Box, consumed here once.
    let outer = unsafe { &mut *file.cast::<VfsFile>() };
    let state = unsafe { Box::from_raw(outer.opened) };
    let mut level = state.level;
    let rc = state
        .entry
        .unlock(state.owner, ffi::SQLITE_LOCK_NONE, &mut level);
    if rc != ffi::SQLITE_OK {
        state.fail(StorageFailure::Io, rc, true);
    }
    outer.opened = ptr::null_mut();
    outer.base.pMethods = ptr::null();
    drop(state);
    rc
}

unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    out: *mut c_void,
    amount: c_int,
    offset: i64,
) -> c_int {
    let state = unsafe { opened(file) };
    if let Err(error) = state.verify() {
        return state.fail(error, ffi::SQLITE_IOERR_READ, false);
    }
    if amount < 0 || offset < 0 || offset.checked_add(i64::from(amount)).is_none() {
        return ffi::SQLITE_IOERR_READ;
    }
    if amount == 0 {
        return ffi::SQLITE_OK;
    }
    // SAFETY: SQLite supplies amount writable bytes and a valid file offset.
    let bytes = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), amount as usize) };
    let mut done = 0;
    while done < bytes.len() {
        let count = unsafe {
            libc::pread(
                state.entry.fd(),
                bytes[done..].as_mut_ptr().cast(),
                bytes.len() - done,
                offset + done as i64,
            )
        };
        if count < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return state.fail(io_failure(), ffi::SQLITE_IOERR_READ, false);
        }
        if count == 0 {
            bytes[done..].fill(0);
            return ffi::SQLITE_IOERR_SHORT_READ;
        }
        done += count as usize;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn write(
    file: *mut ffi::sqlite3_file,
    input: *const c_void,
    amount: c_int,
    offset: i64,
) -> c_int {
    let state = unsafe { opened(file) };
    if state.context.read_only {
        return state.fail(StorageFailure::Unavailable, ffi::SQLITE_READONLY, false);
    }
    if let Err(error) = state.verify() {
        return state.fail(error, ffi::SQLITE_IOERR_WRITE, true);
    }
    #[cfg(any(test, feature = "test-support"))]
    if state.context.hit(FaultPoint::Write)
        || (state.entry.kind == Kind::Database && state.context.hit(FaultPoint::DatabaseWrite))
    {
        return state.fail(StorageFailure::Io, ffi::SQLITE_IOERR_WRITE, true);
    }
    if amount < 0 || offset < 0 || offset.checked_add(i64::from(amount)).is_none() {
        return ffi::SQLITE_IOERR_WRITE;
    }
    if amount == 0 {
        return ffi::SQLITE_OK;
    }
    // SAFETY: SQLite supplies amount readable bytes and a valid file offset.
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), amount as usize) };
    let mut done = 0;
    while done < bytes.len() {
        let count = unsafe {
            libc::pwrite(
                state.entry.fd(),
                bytes[done..].as_ptr().cast(),
                bytes.len() - done,
                offset + done as i64,
            )
        };
        if count <= 0 {
            if count < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return state.fail(io_failure(), ffi::SQLITE_IOERR_WRITE, true);
        }
        done += count as usize;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn truncate(file: *mut ffi::sqlite3_file, length: i64) -> c_int {
    let state = unsafe { opened(file) };
    if state.context.read_only {
        return state.fail(StorageFailure::Unavailable, ffi::SQLITE_READONLY, false);
    }
    if let Err(error) = state.verify() {
        return state.fail(error, ffi::SQLITE_IOERR_TRUNCATE, true);
    }
    if length < 0 || unsafe { libc::ftruncate(state.entry.fd(), length) } != 0 {
        return state.fail(io_failure(), ffi::SQLITE_IOERR_TRUNCATE, true);
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn sync(file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    let state = unsafe { opened(file) };
    if state.context.read_only {
        return state.fail(StorageFailure::Unavailable, ffi::SQLITE_READONLY, false);
    }
    if let Err(error) = state.verify() {
        return state.fail(error, ffi::SQLITE_IOERR_FSYNC, true);
    }
    #[cfg(any(test, feature = "test-support"))]
    if state.context.hit(FaultPoint::Sync)
        || (state.entry.kind == Kind::Database && state.context.hit(FaultPoint::DatabaseSync))
    {
        return state.fail(StorageFailure::Io, ffi::SQLITE_IOERR_FSYNC, true);
    }
    // SAFETY: fd is a retained regular file. Never fall back after a failed sync.
    if unsafe { libc::fsync(state.entry.fd()) } != 0 {
        return state.fail(io_failure(), ffi::SQLITE_IOERR_FSYNC, true);
    }
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::fcntl(state.entry.fd(), libc::F_FULLFSYNC) } != 0 {
            return state.fail(io_failure(), ffi::SQLITE_IOERR_FSYNC, true);
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            let event = if state.entry.kind == Kind::Journal {
                DurabilityEvent::JournalSync
            } else {
                DurabilityEvent::DatabaseSync
            };
            mutex(&state.context.durability_events).push(event);
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn size(file: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
    let state = unsafe { opened(file) };
    if let Err(error) = state.verify() {
        return state.fail(error, ffi::SQLITE_IOERR_FSTAT, false);
    }
    // SAFETY: fstat initializes stat, and SQLite provides a writable i64.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(state.entry.fd(), &mut stat) } != 0 {
        return state.fail(io_failure(), ffi::SQLITE_IOERR_FSTAT, false);
    }
    unsafe {
        *out = stat.st_size;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn lock(file: *mut ffi::sqlite3_file, requested: c_int) -> c_int {
    let state = unsafe { opened(file) };
    if state.context.read_only && requested > ffi::SQLITE_LOCK_SHARED {
        return state.fail(StorageFailure::Unavailable, ffi::SQLITE_READONLY, false);
    }
    if let Err(error) = state.verify() {
        return state.fail(error, ffi::SQLITE_IOERR_LOCK, false);
    }
    let rc = state.entry.lock(state.owner, requested, &mut state.level);
    if matches!(rc, ffi::SQLITE_OK | ffi::SQLITE_BUSY) {
        rc
    } else {
        state.fail(StorageFailure::Io, rc, false)
    }
}
unsafe extern "C" fn unlock(file: *mut ffi::sqlite3_file, requested: c_int) -> c_int {
    let state = unsafe { opened(file) };
    let rc = state.entry.unlock(state.owner, requested, &mut state.level);
    // SQLite may ignore an unlock error during statement cleanup. Retain the
    // uncertainty independently so the next admission cannot erase it.
    if rc == ffi::SQLITE_OK {
        rc
    } else {
        state.fail(StorageFailure::Io, rc, true)
    }
}
unsafe extern "C" fn reserved(file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    let state = unsafe { opened(file) };
    if let Err(error) = state.verify() {
        return state.fail(error, ffi::SQLITE_IOERR_CHECKRESERVEDLOCK, false);
    }
    match state.entry.reserved() {
        Ok(value) => {
            unsafe {
                *out = i32::from(value);
            }
            ffi::SQLITE_OK
        }
        Err(code) => state.fail(StorageFailure::Io, code, false),
    }
}
unsafe extern "C" fn control(
    file: *mut ffi::sqlite3_file,
    operation: c_int,
    out: *mut c_void,
) -> c_int {
    let state = unsafe { opened(file) };
    match operation {
        ffi::SQLITE_FCNTL_LOCKSTATE => {
            unsafe {
                *out.cast::<c_int>() = state.level;
            }
            ffi::SQLITE_OK
        }
        ffi::SQLITE_FCNTL_HAS_MOVED => {
            if let Err(error) = state.verify() {
                return state.fail(error, ffi::SQLITE_IOERR_FSTAT, false);
            }
            unsafe {
                *out.cast::<c_int>() = 0;
            }
            ffi::SQLITE_OK
        }
        ffi::SQLITE_FCNTL_SIZE_HINT => ffi::SQLITE_OK,
        _ => ffi::SQLITE_NOTFOUND,
    }
}
unsafe extern "C" fn sector_size(_file: *mut ffi::sqlite3_file) -> c_int {
    4096
}
unsafe extern "C" fn characteristics(_file: *mut ffi::sqlite3_file) -> c_int {
    0
}

static IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(reserved),
    xFileControl: Some(control),
    xSectorSize: Some(sector_size),
    xDeviceCharacteristics: Some(characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};
