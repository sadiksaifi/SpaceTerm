//! Descriptor-relative private storage. No symlinks, unsafe modes, or foreign owners are repaired.
use crate::ServerError;
use crate::protocol;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

// Bound retained hierarchy and edit records independently of live repository snapshots.
// The authenticated transport also enforces its own 16 MiB frame limit.
const MAX_RECORD: usize = 12 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub format: String,
    pub server: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<crate::organization::Organization>,
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct SocketIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

pub(crate) struct Directory(File);
/// An exclusive lock that ends when this guard drops.
///
/// flock belongs to the open file description, not the descriptor. A child that another thread
/// forked holds a copy of the descriptor until it reaches exec, so closing alone would keep the
/// lock held and make a later nonblocking attempt report contention.
pub struct Lock(File);
impl Lock {
    /// Take an exclusive lock on an open directory, waiting for any other holder.
    pub(crate) fn directory(directory: &File) -> Result<Self, ServerError> {
        // The copy shares the directory's open file description, so it holds the same lock.
        let file = directory
            .try_clone()
            .map_err(|_| ServerError::StorageUnavailable)?;
        // SAFETY: flock acts only on the supplied live descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(ServerError::StorageUnavailable);
        }
        Ok(Self(file))
    }
    /// Give up unlock-on-drop. The lock then lasts until every copy of its descriptor closes,
    /// including a copy that a child process inherits across exec.
    pub fn into_inheritable(self) -> File {
        let lock = std::mem::ManuallyDrop::new(self);
        // SAFETY: ManuallyDrop never runs this guard's Drop, so the descriptor moves out once.
        unsafe { std::ptr::read(&lock.0) }
    }
}
/// How often [`Directory::wait_lock`] observes cancellation. The lock itself wakes it at once.
const CANCEL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// How a bounded wait for a lock ended.
pub enum LockWait {
    Locked(Lock),
    /// The holder kept the lock past the limit.
    Held,
    Cancelled,
}
impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: this guard owns the live descriptor.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
impl Directory {
    pub fn open(path: &Path, create: bool) -> Result<Self, ServerError> {
        if !path.is_absolute() {
            return Err(ServerError::UnsafeStorage);
        }
        let mut dir = File::open("/").map_err(|_| ServerError::StorageUnavailable)?;
        let names: Vec<_> = path
            .components()
            .filter(|v| *v != Component::RootDir)
            .collect();
        for (index, name) in names.iter().enumerate() {
            let Component::Normal(name) = name else {
                return Err(ServerError::UnsafeStorage);
            };
            let name = CString::new(name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
            if create {
                // SAFETY: directory descriptor and NUL-terminated component are live.
                let result = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
                if result == 0 {
                    dir.sync_all()
                        .map_err(|_| ServerError::StorageUnavailable)?;
                }
                if result != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(ServerError::StorageUnavailable);
                }
            }
            // SAFETY: openat receives a live directory and component; successful fd is owned below.
            let fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(classify_open());
            }
            // SAFETY: successful openat transferred ownership of a fresh descriptor.
            dir = unsafe { File::from_raw_fd(fd) };
            if index + 1 == names.len() {
                private(&dir, true)?;
            }
        }
        Ok(Self(dir))
    }
    pub fn verify_path(&self, path: &Path) -> Result<(), ServerError> {
        let retained = self
            .0
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        let current =
            std::fs::symlink_metadata(path).map_err(|_| ServerError::StorageUnavailable)?;
        if !current.is_dir() || retained.dev() != current.dev() || retained.ino() != current.ino() {
            return Err(ServerError::UnsafeStorage);
        }
        private(&self.0, true)
    }
    pub fn socket_identity(
        &self,
        name: &std::ffi::CStr,
    ) -> Result<Option<SocketIdentity>, ServerError> {
        // SAFETY: zero is valid for every field of stat; fstatat fills the retained output below.
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: descriptor, terminated leaf name, and writable output remain live for this call.
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(ServerError::StorageUnavailable)
            };
        }
        // SAFETY: geteuid has no preconditions.
        if metadata.st_uid != unsafe { libc::geteuid() }
            || metadata.st_mode & libc::S_IFMT != libc::S_IFSOCK
            || metadata.st_mode & 0o7777 != 0o600
        {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(Some(SocketIdentity {
            device: metadata.st_dev,
            inode: metadata.st_ino,
        }))
    }
    pub fn remove_socket(
        &self,
        name: &std::ffi::CStr,
        expected: SocketIdentity,
    ) -> Result<(), ServerError> {
        // Revalidate immediately before descriptor-relative deletion. Changed entries stay intact.
        if self.socket_identity(name)? != Some(expected) {
            return Err(ServerError::UnsafeStorage);
        }
        // SAFETY: descriptor and terminated single-component name remain live for this call.
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(ServerError::StorageUnavailable);
        }
        self.0
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)
    }
    /// Removes one private record. A record that is already absent is removed.
    #[cfg(feature = "client")]
    pub(crate) fn remove(&self, name: &std::ffi::CStr) -> Result<(), ServerError> {
        // SAFETY: descriptor and terminated single-component name remain live for this call.
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound
        {
            return Err(ServerError::StorageUnavailable);
        }
        self.0
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)
    }
    /// Publish a prepared private socket without replacing any existing ready endpoint.
    /// Caller holds server.lock. Both names must remain beneath this retained directory.
    pub fn publish_socket(
        &self,
        pending: &std::ffi::CStr,
        expected: SocketIdentity,
    ) -> Result<(), ServerError> {
        if self.socket_identity(pending)? != Some(expected) || self.socket_identity(c"s")?.is_some()
        {
            return Err(ServerError::UnsafeStorage);
        }
        // SAFETY: both terminated names are leaves relative to the retained directory.
        // linkat never replaces an existing destination, including a concurrent unsafe entry.
        if unsafe {
            libc::linkat(
                self.0.as_raw_fd(),
                pending.as_ptr(),
                self.0.as_raw_fd(),
                c"s".as_ptr(),
                0,
            )
        } != 0
        {
            return Err(
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
                    ServerError::UnsafeStorage
                } else {
                    ServerError::StorageUnavailable
                },
            );
        }
        if self.socket_identity(c"s")? != Some(expected) {
            return Err(ServerError::UnsafeStorage);
        }
        self.remove_socket(pending, expected)
    }
    fn file(&self, name: &str, create: bool) -> Result<File, ServerError> {
        let name = CString::new(name).map_err(|_| ServerError::UnsafeStorage)?;
        let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // Exclusive creation makes concurrent setup deterministic on macOS, where competing
        // non-exclusive O_CREAT opens can return ENOENT even while the directory is retained.
        // SAFETY: directory descriptor and leaf name remain live; returned fd is owned below.
        let mut fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags
                    | if create {
                        libc::O_CREAT | libc::O_EXCL
                    } else {
                        0
                    },
                0o600,
            )
        };
        if fd < 0
            && create
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists
        {
            // SAFETY: this only opens an existing no-follow leaf beneath the retained directory.
            fd = unsafe { libc::openat(self.0.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(classify_open());
        }
        // SAFETY: successful openat transferred a fresh descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        private(&file, false)?;
        Ok(file)
    }
    /// Take an exclusive lock that ends when the returned guard drops.
    pub fn lock(&self, name: &str, nonblocking: bool) -> Result<Lock, ServerError> {
        let file = self.file(name, true)?;
        // SAFETY: flock acts only on the supplied live descriptor.
        if unsafe {
            libc::flock(
                file.as_raw_fd(),
                libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 },
            )
        } != 0
        {
            return Err(ServerError::StorageUnavailable);
        }
        Ok(Lock(file))
    }
    /// Attempt an exclusive lock, distinguishing contention from unavailable storage.
    pub fn try_lock(&self, name: &str) -> Result<Option<Lock>, ServerError> {
        let file = self.file(name, true)?;
        // SAFETY: flock acts only on the supplied live descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(Lock(file)));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(ServerError::StorageUnavailable)
        }
    }
    /// Wait at most `limit` for an exclusive lock that another holder may keep for a long time.
    ///
    /// A helper thread blocks in flock, so the waiter wakes as soon as the holder releases the
    /// lock, never on a polling interval. `cancelled` is observed every [`CANCEL_INTERVAL`]. A
    /// waiter that returns before the helper acquires leaves it blocked until the holder
    /// releases; the helper then releases the lock at once.
    pub fn wait_lock(
        &self,
        name: &str,
        limit: std::time::Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<LockWait, ServerError> {
        if let Some(lock) = self.try_lock(name)? {
            return Ok(LockWait::Locked(lock));
        }
        let file = self.file(name, true)?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("spaceterm-lock-wait".to_owned())
            .spawn(move || {
                let result = loop {
                    // SAFETY: flock acts only on the supplied live descriptor.
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                        break Ok(Lock(file));
                    }
                    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                        break Err(ServerError::StorageUnavailable);
                    }
                };
                // A waiter that already returned dropped its receiver. The lock then drops here.
                let _ = sender.send(result);
            })
            .map_err(|_| ServerError::StorageUnavailable)?;
        let deadline = std::time::Instant::now() + limit;
        loop {
            if cancelled() {
                return Ok(LockWait::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(LockWait::Held);
            }
            match receiver.recv_timeout(remaining.min(CANCEL_INTERVAL)) {
                Ok(result) => return result.map(LockWait::Locked),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ServerError::StorageUnavailable);
                }
            }
        }
    }
    /// Validate and synchronize retained state before setup or authentication accepts it.
    pub fn read(&self) -> Result<Record, ServerError> {
        let (record, file) = self.read_visible()?;
        // A previous publication may have renamed successfully but failed to sync its directory.
        // Visible bytes alone do not confirm durability, including after reopening the store.
        file.sync_all()
            .and_then(|()| self.0.sync_all())
            .map_err(|_| ServerError::StorageUnavailable)?;
        Ok(record)
    }
    /// Validates the visible record without confirming its durability. Only a caller that
    /// replaces the whole record and then syncs it, as [`Self::write`] does, may skip the sync:
    /// the replacement's own file and directory syncs make everything it carries forward durable.
    pub(crate) fn read_unconfirmed(&self) -> Result<Record, ServerError> {
        self.read_visible().map(|(record, _)| record)
    }
    fn read_visible(&self) -> Result<(Record, File), ServerError> {
        let file = self.file("identity.json", false)?;
        let mut bytes = Vec::new();
        (&file)
            .take((MAX_RECORD + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ServerError::StorageUnavailable)?;
        if bytes.len() > MAX_RECORD {
            return Err(ServerError::UnsafeStorage);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ServerError::UnsafeStorage)?;
        if value.get("format").and_then(|v| v.as_str()) != Some("account-local-1") {
            return Err(ServerError::IncompatibleState);
        }
        // Valid JSON in this format can retain a schema this build no longer understands.
        let record: Record =
            serde_json::from_value(value).map_err(|_| ServerError::IncompatibleState)?;
        if record.format != "account-local-1" || record.server == [0; 32] {
            return Err(ServerError::UnsafeStorage);
        }
        if let Some(value) = &record.organization {
            value.validate()?;
        }
        Ok((record, file))
    }
    pub fn read_or_create(&self) -> Result<Record, ServerError> {
        match self.read() {
            Ok(record) => Ok(record),
            Err(ServerError::NotSetUp) => {
                let record = Record {
                    format: "account-local-1".into(),
                    server: protocol::random()?,
                    organization: None,
                };
                self.write(&record)?;
                Ok(record)
            }
            Err(error) => Err(error),
        }
    }
    /// Caller holds setup.lock. Publish once after fsync so crashes never truncate the old identity.
    pub fn write(&self, record: &Record) -> Result<(), ServerError> {
        if let Some(value) = &record.organization {
            value.validate()?;
        }
        let bytes = serde_json::to_vec(record).map_err(|_| ServerError::StorageUnavailable)?;
        self.write_bytes(c"identity.json", &bytes)
    }
    pub(crate) fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, ServerError> {
        let file = match self.file(name, false) {
            Ok(file) => file,
            Err(ServerError::NotSetUp) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        (&file)
            .take((MAX_RECORD + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ServerError::StorageUnavailable)?;
        if bytes.len() > MAX_RECORD {
            return Err(ServerError::UnsafeStorage);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| ServerError::UnsafeStorage)?;
        file.sync_all()
            .and_then(|()| self.0.sync_all())
            .map_err(|_| ServerError::StorageUnavailable)?;
        Ok(Some(value))
    }
    pub(crate) fn write_json<T: Serialize>(
        &self,
        name: &std::ffi::CStr,
        value: &T,
    ) -> Result<(), ServerError> {
        self.write_bytes(
            name,
            &serde_json::to_vec(value).map_err(|_| ServerError::StorageUnavailable)?,
        )
    }
    fn write_bytes(&self, destination: &std::ffi::CStr, bytes: &[u8]) -> Result<(), ServerError> {
        if bytes.len() > MAX_RECORD {
            return Err(ServerError::StorageUnavailable);
        }
        let name = format!(
            "pending-{}",
            u128::from_be_bytes(
                protocol::random()?[..16]
                    .try_into()
                    .map_err(|_| ServerError::StorageUnavailable)?
            )
        );
        let c_name = CString::new(name.as_str()).map_err(|_| ServerError::StorageUnavailable)?;
        // SAFETY: openat creates a unique private regular file beneath the retained directory.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(ServerError::StorageUnavailable);
        }
        // SAFETY: successful openat transferred ownership of this descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| ServerError::StorageUnavailable)?;
            // SAFETY: both names are single components relative to the same live directory.
            if unsafe {
                libc::renameat(
                    self.0.as_raw_fd(),
                    c_name.as_ptr(),
                    self.0.as_raw_fd(),
                    destination.as_ptr(),
                )
            } != 0
            {
                return Err(ServerError::StorageUnavailable);
            }
            self.0
                .sync_all()
                .map_err(|_| ServerError::StorageUnavailable)
        })();
        // SAFETY: cleanup removes only the unique temporary name; rename may already have removed it.
        unsafe {
            libc::unlinkat(self.0.as_raw_fd(), c_name.as_ptr(), 0);
        }
        result
    }
}
fn private(file: &File, directory: bool) -> Result<(), ServerError> {
    let meta = file
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    // SAFETY: geteuid has no preconditions.
    if meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o7777 != if directory { 0o700 } else { 0o600 }
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file() || meta.nlink() != 1
        }
    {
        return Err(ServerError::UnsafeStorage);
    }
    Ok(())
}
fn classify_open() -> ServerError {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ENOENT) => ServerError::NotSetUp,
        Some(libc::ELOOP | libc::ENOTDIR) => ServerError::UnsafeStorage,
        _ => ServerError::StorageUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
    };
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "spaceterm-storage-{}",
                u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
            ));
            Self(path)
        }
        fn open(&self) -> Directory {
            Directory::open(&self.0, true).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[cfg(feature = "client")]
    #[test]
    fn personal_arrangements_survive_reopen_and_remain_fenced_to_each_client_and_server() {
        use crate::client_view::{
            ClientView, ClientViewStore, Layout, LayoutNode, TabView, WorkspaceView,
        };
        let fixture = Fixture::new();
        let _directory = fixture.open();
        let first = fixture.0.join("first");
        let second = fixture.0.join("second");
        let view = ClientView {
            workspaces: vec![WorkspaceView {
                workspace: [1; 32],
                tabs: vec![TabView {
                    tab: [2; 32],
                    layout: Layout {
                        nodes: vec![LayoutNode::Pane([3; 32])],
                    },
                }],
            }],
        };
        let mut client =
            ClientViewStore::open(Directory::open(&first, true).unwrap(), [8; 32], [9; 32])
                .unwrap();
        client.save(view.clone()).unwrap();
        assert!(
            ClientViewStore::open(Directory::open(&first, false).unwrap(), [8; 32], [9; 32])
                .is_err(),
            "one writer per desktop"
        );
        drop(client);
        let client =
            ClientViewStore::open(Directory::open(&first, false).unwrap(), [8; 32], [9; 32])
                .unwrap();
        let other =
            ClientViewStore::open(Directory::open(&second, true).unwrap(), [8; 32], [10; 32])
                .unwrap();
        assert_eq!(
            (client.view(), other.view()),
            (&view, &ClientView::default())
        );
        drop(client);
        // Rejected identities must release the writer slot for the next attempt.
        assert_eq!(
            ClientViewStore::open(Directory::open(&first, false).unwrap(), [7; 32], [9; 32]).err(),
            Some(ServerError::AuthenticationRefused)
        );
        assert_eq!(
            ClientViewStore::open(Directory::open(&first, false).unwrap(), [8; 32], [10; 32]).err(),
            Some(ServerError::AuthenticationRefused)
        );
        let reopened =
            ClientViewStore::open(Directory::open(&first, false).unwrap(), [8; 32], [9; 32])
                .unwrap();
        assert_eq!(reopened.view(), &view);
        assert_eq!(
            fs::metadata(first.join("view.json")).unwrap().mode() & 0o7777,
            0o600
        );
        assert!(!fixture.0.join("settings.json").exists());
    }
    #[cfg(feature = "client")]
    #[test]
    fn a_retained_inventory_survives_reopen_until_discarded_and_never_blocks_the_views() {
        use crate::client_view::{
            ClientViewStore, RetainedInventory, RetainedPane, RetainedTab, RetainedWorkspace,
        };
        use crate::hierarchy::{Axis, MAX_WORKSPACES};
        let fixture = Fixture::new();
        let workspace = |id: u8| RetainedWorkspace {
            id: [id; 32],
            name: format!("Workspace {id}"),
            root_tab: [id.wrapping_add(100); 32],
            tabs: vec![RetainedTab {
                id: [id.wrapping_add(100); 32],
                root_pane: [id.wrapping_add(200); 32],
                panes: vec![RetainedPane {
                    id: [id.wrapping_add(200); 32],
                    source: None,
                    axis: Axis::Right,
                    shell: Some("zsh".into()),
                }],
            }],
        };
        let retained = RetainedInventory {
            revision: 4,
            workspaces: vec![workspace(1)],
        };
        let mut store = ClientViewStore::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(store.retained_inventory(), None);
        store.save_inventory(retained.clone()).unwrap();
        // A copy beyond the server's bounds is refused and the previous copy stays.
        let oversized = RetainedInventory {
            revision: 5,
            workspaces: (1..=u8::try_from(MAX_WORKSPACES + 1).unwrap())
                .map(workspace)
                .collect(),
        };
        assert_eq!(
            store.save_inventory(oversized),
            Err(ServerError::UnsafeStorage)
        );
        drop(store);
        let store = ClientViewStore::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(store.retained_inventory(), Some(&retained));
        drop(store);
        let other = ClientViewStore::open(fixture.open(), [1; 32], [3; 32]).unwrap();
        assert_eq!(
            other.retained_inventory(),
            None,
            "another client's copy is never read"
        );
        drop(other);
        // An unreadable copy is absent and leaves the client views readable.
        fs::write(fixture.0.join("inventory.json"), b"incomplete document").unwrap();
        let mut store = ClientViewStore::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(store.retained_inventory(), None);
        store.save_inventory(retained.clone()).unwrap();
        store.discard_inventory().unwrap();
        drop(store);
        assert!(!fixture.0.join("inventory.json").exists());
        let store = ClientViewStore::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(store.retained_inventory(), None);
    }
    #[cfg(feature = "client")]
    #[test]
    fn unreadable_client_views_are_preserved_instead_of_replaced_with_defaults() {
        use crate::client_view::{ClientView, ClientViewStore, WorkspaceView};
        let fixture = Fixture::new();
        let directory = fixture.open();
        let mut store = ClientViewStore::open(directory, [1; 32], [2; 32]).unwrap();
        store
            .save(ClientView {
                workspaces: vec![WorkspaceView {
                    workspace: [3; 32],
                    tabs: Vec::new(),
                }],
            })
            .unwrap();
        drop(store);
        fs::write(fixture.0.join("view.json"), b"incomplete document").unwrap();
        assert!(matches!(
            ClientViewStore::open(fixture.open(), [1; 32], [2; 32]),
            Err(ServerError::UnsafeStorage)
        ));
        assert_eq!(
            fs::read(fixture.0.join("view.json")).unwrap(),
            b"incomplete document"
        );
    }
    #[cfg(feature = "client")]
    #[test]
    fn detached_attachments_and_unacknowledged_releases_survive_reopen_without_settings() {
        use crate::attachments::Attachments;
        use crate::hierarchy::{Axis, Inventory, Pane, Tab, Workspace};
        let fixture = Fixture::new();
        // The lock's descriptor is close-on-exec, but a process another test forks holds a copy
        // until it executes, and the copy keeps the lock. A reopen retries that contention
        // briefly.
        let reopen = |server: [u8; 32], client: [u8; 32]| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match Attachments::open(fixture.open(), server, client) {
                    Err(ServerError::StorageUnavailable)
                        if std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    result => return result,
                }
            }
        };
        let inventory = Inventory {
            repositories: None,
            revision: 1,
            close: None,
            workspaces: vec![Workspace {
                id: [1; 32],
                name: "work".into(),
                name_version: 0,
                pin_version: 0,
                pinned_directory: None,
                root_tab: [2; 32],
                tabs: vec![Tab {
                    worktree: None,
                    worktree_membership_fixed: false,
                    worktree_repository: None,
                    id: [2; 32],
                    root_pane: [3; 32],
                    panes: vec![Pane {
                        id: [3; 32],
                        source: None,
                        axis: Axis::Right,
                        starting_directory: "/tmp".into(),
                        current_directory: None,
                        repository_directory: None,
                        presentation: None,
                        confirmed_directory: None,
                        creator: None,
                    }],
                }],
            }],
        };
        let original = {
            let choices = Attachments::open(fixture.open(), [8; 32], [9; 32]).unwrap();
            choices.initialize(&inventory, Some([1; 32])).unwrap();
            let permit = choices.permit([3; 32]).unwrap();
            choices.detach([1; 32]).unwrap();
            assert!(!permit.active());
            assert!(
                Attachments::open(fixture.open(), [8; 32], [9; 32]).is_err(),
                "one writer"
            );
            permit.attachment()
        };
        let choices = reopen([8; 32], [9; 32]).unwrap();
        choices.reconcile(&inventory).unwrap();
        assert!(!choices.is_attached([1; 32]));
        assert_eq!(choices.pending(), vec![original]);
        choices.attach([1; 32]).unwrap();
        let later = choices.permit([3; 32]).unwrap().attachment();
        assert!(later.generation > original.generation);
        choices.released(original).unwrap();
        assert!(choices.is_attached([1; 32]));
        drop(choices);
        assert!(matches!(
            reopen([7; 32], [9; 32]),
            Err(ServerError::AuthenticationRefused)
        ));
        assert!(matches!(
            reopen([8; 32], [10; 32]),
            Err(ServerError::AuthenticationRefused)
        ));
        assert_eq!(
            fs::metadata(fixture.0.join("attachments.json"))
                .unwrap()
                .mode()
                & 0o7777,
            0o600
        );
        assert!(!fixture.0.join("settings.json").exists());
        fs::write(fixture.0.join("attachments.json"), b"incomplete document").unwrap();
        assert!(matches!(
            reopen([8; 32], [9; 32]),
            Err(ServerError::UnsafeStorage)
        ));
        assert_eq!(
            fs::read(fixture.0.join("attachments.json")).unwrap(),
            b"incomplete document"
        );
    }

    #[test]
    fn repeated_setup_retains_nonsecret_identity_with_private_modes() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let record = data.read_or_create().unwrap();
        let original = record.server;
        data.write(&record).unwrap();
        let restored = data.read_or_create().unwrap();
        assert_eq!(restored.server, original);
        assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(
            fs::metadata(fixture.0.join("identity.json"))
                .unwrap()
                .mode()
                & 0o7777,
            0o600
        );
    }
    #[test]
    fn organization_is_retained_with_nonsecret_identity() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let original = data.read_or_create().unwrap();
        let mut value = serde_json::to_value(&original).unwrap();
        value["organization"] = serde_json::json!({
            "version": 1, "workspace": vec![1; 32], "tab": vec![2; 32], "pane": vec![3; 32],
            "name": "Development", "pinned_directory": null,
            "starting_directory": "/tmp", "confirmed_directory": null
        });
        std::fs::write(
            fixture.0.join("identity.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let reopened = fixture.open();
        let restored = reopened.read().unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), value);
    }

    #[test]
    fn older_shared_organization_is_incompatible_and_left_unchanged() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        let first = crate::organization::Organization::first(fixture.0.clone()).unwrap();
        record.organization = Some(
            crate::hierarchy::State::from_organization(&first)
                .envelope(&fixture.0)
                .unwrap(),
        );
        let mut value = serde_json::to_value(&record).unwrap();
        let shared = value["organization"]["shared"].as_object_mut().unwrap();
        // Retained organization from before automatic naming has neither field.
        assert!(shared.remove("names").is_some());
        assert!(shared.remove("home_directory").is_some());
        let original = serde_json::to_vec(&value).unwrap();
        let path = fixture.0.join("identity.json");
        fs::write(&path, &original).unwrap();
        assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);

        let reopened = fixture.open();
        assert_eq!(reopened.read().err(), Some(ServerError::IncompatibleState));
        assert_eq!(
            reopened.read_or_create().err(),
            Some(ServerError::IncompatibleState)
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[cfg(feature = "client")]
    #[test]
    fn pending_edits_and_validated_conflict_drafts_survive_client_reopen() {
        use crate::hierarchy::{ClientJournal, Mutation, Outcome, RetainedOutcome};
        let fixture = Fixture::new();
        let mutation = Mutation::RenameWorkspace {
            workspace: [3; 32],
            version: 7,
            name: "Draft".into(),
        };
        let mut journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        journal.submit(4, mutation.clone()).unwrap();
        // A concurrently spawned child can inherit the close-on-exec lock until exec. Closing
        // the owner's handle must release ownership even while that inherited descriptor lives.
        let (mut release, gate) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: the child uses only read and _exit before terminating, with no Rust cleanup.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let mut byte = 0_u8;
            // SAFETY: the inherited descriptor and one-byte writable buffer are live.
            unsafe {
                libc::read(gate.as_raw_fd(), (&mut byte as *mut u8).cast(), 1);
                libc::_exit(0);
            }
        }
        drop(journal);
        let reopened = ClientJournal::open(fixture.open(), [1; 32], [2; 32]);
        release.write_all(&[1]).unwrap();
        // SAFETY: this reaps only the child created above; no other process is signalled.
        unsafe {
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
        let mut journal = reopened.unwrap();
        assert_eq!(journal.pending().unwrap().mutation, mutation);
        assert_eq!(journal.pending().unwrap().sequence, 4);
        let result = RetainedOutcome {
            sequence: 4,
            mutation: mutation.clone(),
            outcome: Outcome::Conflict,
        };
        assert_eq!(
            journal.retire(RetainedOutcome {
                sequence: 5,
                ..result.clone()
            }),
            Err(ServerError::Protocol)
        );
        assert_eq!(
            journal.pending().unwrap().sequence,
            4,
            "a mismatch retires nothing"
        );
        journal.retire(result.clone()).unwrap();
        drop(journal);
        let journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert!(journal.pending().is_none());
        assert_eq!(journal.last(), Some(&result));
        drop(journal);
        assert!(matches!(
            ClientJournal::open(fixture.open(), [9; 32], [2; 32]),
            Err(ServerError::AuthenticationRefused)
        ));
    }

    #[cfg(all(feature = "client", target_os = "linux"))]
    #[test]
    fn uncertain_edit_journal_cannot_resume_until_its_actual_outcome_is_synchronized() {
        use crate::hierarchy::{ClientJournal, Mutation};
        let fixture = Fixture::new();
        let data = fixture.open();
        let mut journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        let fault = sync_failure::fail_directory(&data.0);
        assert_eq!(
            journal.submit(
                1,
                Mutation::CreateWorkspace {
                    name: "Draft".into(),
                    directory: None,
                }
            ),
            Err(ServerError::StorageUnavailable)
        );
        drop(journal);
        assert!(matches!(
            ClientJournal::open(fixture.open(), [1; 32], [2; 32]),
            Err(ServerError::StorageUnavailable)
        ));
        drop(fault);
        let journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(journal.pending().unwrap().sequence, 1);
    }
    #[test]
    fn corrupt_identity_is_preserved_instead_of_replaced() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut file = data.file("identity.json", true).unwrap();
        file.write_all(b"interrupted or unsupported identity")
            .unwrap();
        assert!(matches!(
            data.read_or_create(),
            Err(ServerError::UnsafeStorage)
        ));
        assert_eq!(
            fs::read(fixture.0.join("identity.json")).unwrap(),
            b"interrupted or unsupported identity"
        );
    }
    #[test]
    fn refuses_symlinked_or_world_readable_identity() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        data.read_or_create().unwrap();
        let path = fixture.0.join("identity.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(data.read(), Err(ServerError::UnsafeStorage)));
        fs::rename(&path, fixture.0.join("other")).unwrap();
        symlink("other", &path).unwrap();
        assert!(matches!(
            data.read_or_create(),
            Err(ServerError::UnsafeStorage)
        ));
    }
    #[test]
    fn concurrent_setup_serializes_identity_creation() {
        let fixture = Fixture::new();
        fixture.open();
        let mut threads = Vec::new();
        for _ in 0..4 {
            let path = fixture.0.clone();
            threads.push(std::thread::spawn(move || {
                let directory = Directory::open(&path, true).unwrap();
                let _lock = directory.lock("setup.lock", false).unwrap();
                directory.read_or_create().unwrap().server
            }));
        }
        let keys: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(keys.iter().all(|key| *key == keys[0]));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uncertain_identity_stays_unavailable_until_synchronization_recovers() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        record.server = [42; 32];

        let fault = sync_failure::fail_directory(&data.0);
        assert_eq!(data.write(&record), Err(ServerError::StorageUnavailable));
        // The rename succeeded. Recovery must confirm durability, not just parse visible bytes.
        let visible: Record =
            serde_json::from_slice(&fs::read(fixture.0.join("identity.json")).unwrap()).unwrap();
        assert_eq!(visible.server, [42; 32]);
        let reopened = fixture.open();
        assert!(matches!(
            reopened.read(),
            Err(ServerError::StorageUnavailable)
        ));
        assert!(matches!(
            reopened.read_or_create(),
            Err(ServerError::StorageUnavailable)
        ));

        drop(fault);
        let recovered = reopened.read_or_create().unwrap();
        assert_eq!(recovered.server, record.server);
        assert_eq!(recovered.server, [42; 32]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uncertain_organization_publication_blocks_recovery_until_synced() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        record.server = [42; 32];
        let mut organization = crate::organization::Organization::first(fixture.0.clone()).unwrap();
        organization.name = "Retained work".into();
        organization.pinned_directory = Some(fixture.0.clone());
        organization.confirmed_directory = Some("/".into());
        let mut shared = crate::hierarchy::State::from_organization(&organization);
        let creation = crate::hierarchy::Mutation::CreateTab {
            workspace: organization.workspace,
            source: None,
        };
        let accepted = shared
            .apply([42; 32], 1, creation.clone(), &fixture.0, |_| true)
            .unwrap();
        organization = shared.envelope(&fixture.0).unwrap();
        record.organization = Some(organization.clone());
        let fault = sync_failure::fail_directory(&data.0);
        assert_eq!(data.write(&record), Err(ServerError::StorageUnavailable));
        let reopened = fixture.open();
        assert!(matches!(
            reopened.read(),
            Err(ServerError::StorageUnavailable)
        ));
        drop(fault);
        let recovered = reopened.read_or_create().unwrap();
        assert_eq!(recovered.server, record.server);
        assert_eq!(recovered.server, [42; 32]);
        assert!(recovered.organization.as_ref() == Some(&organization));
        let mut restored =
            crate::hierarchy::State::from_organization(recovered.organization.as_ref().unwrap());
        assert_eq!(
            restored
                .apply([42; 32], 1, creation, &fixture.0, |_| true)
                .unwrap(),
            accepted
        );
        assert_eq!(restored.inventory.workspaces[0].tabs.len(), 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_organization_publication_syncs_only_its_replacement() {
        let fixture = Fixture::new();
        let data = fixture.open();
        {
            let _lock = data.lock("setup.lock", false).unwrap();
            data.read_or_create().unwrap();
        }
        let store = crate::organization::OrganizationStore(fixture.open());
        let organization = crate::organization::Organization::first(fixture.0.clone()).unwrap();
        let syncs = sync_failure::count(|| store.write(Some(&organization)).unwrap());
        assert_eq!(syncs, 2, "the new file and the directory");
        assert!(store.read().unwrap() == Some(organization));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_pending_write_preserves_the_previous_organization() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        let original = crate::organization::Organization::first(fixture.0.clone()).unwrap();
        record.organization = Some(original.clone());
        data.write(&record).unwrap();
        record.organization.as_mut().unwrap().name = "Uncommitted".into();
        let fault = sync_failure::fail_files();
        assert_eq!(data.write(&record), Err(ServerError::StorageUnavailable));
        assert!(matches!(
            fixture.open().read(),
            Err(ServerError::StorageUnavailable)
        ));
        drop(fault);
        let recovered = fixture.open().read_or_create().unwrap();
        assert!(recovered.organization.as_ref() == Some(&original));
        assert_eq!(recovered.server, record.server);
    }

    // Interpose only the native syscall in the Linux test executable. Production storage has
    // no fault switch, and other threads and directories keep using the real filesystem.
    #[cfg(target_os = "linux")]
    mod sync_failure {
        use super::*;
        use std::cell::Cell;

        std::thread_local! {
            static DIRECTORY: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
            static FILES: Cell<bool> = const { Cell::new(false) };
            static SYNCS: Cell<usize> = const { Cell::new(0) };
        }

        /// The syncs `work` issues on this thread.
        pub(super) fn count(work: impl FnOnce()) -> usize {
            let before = SYNCS.get();
            work();
            SYNCS.get() - before
        }

        pub(super) struct Failure;

        pub(super) fn fail_directory(directory: &File) -> Failure {
            let metadata = directory.metadata().unwrap();
            DIRECTORY.set(Some((metadata.dev(), metadata.ino())));
            Failure
        }

        pub(super) fn fail_files() -> Failure {
            FILES.set(true);
            Failure
        }

        impl Drop for Failure {
            fn drop(&mut self) {
                DIRECTORY.set(None);
                FILES.set(false);
            }
        }

        #[unsafe(export_name = "fsync")]
        extern "C" fn fsync_with_failure(fd: libc::c_int) -> libc::c_int {
            let _ = SYNCS.try_with(|syncs| syncs.set(syncs.get() + 1));
            // SAFETY: stat is writable and retained throughout fstat.
            let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: fstat accepts any descriptor, reporting invalid ones without dereferencing it.
            if unsafe { libc::fstat(fd, &mut metadata) } == 0
                && (DIRECTORY
                    .try_with(|target| target.get() == Some((metadata.st_dev, metadata.st_ino)))
                    .unwrap_or(false)
                    || (metadata.st_mode & libc::S_IFMT == libc::S_IFREG
                        && FILES.try_with(|flag| flag.get()).unwrap_or(false)))
            {
                // SAFETY: __errno_location returns this thread's writable errno pointer.
                unsafe { *libc::__errno_location() = libc::EIO };
                return -1;
            }
            // SAFETY: the fsync syscall takes exactly one descriptor. Bypass this override to
            // preserve the native operation for every other directory and thread.
            unsafe { libc::syscall(libc::SYS_fsync, fd) as libc::c_int }
        }
    }
}
