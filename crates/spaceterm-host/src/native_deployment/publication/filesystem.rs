//! Descriptor-relative fresh creation. No archive pathname is passed to a filesystem extractor.

use super::{FreshPublicationInode, NativePublicationProtection, PublicationFile};
use crate::{
    native_deployment::contracts::NativeFailure, protected_directory::ProtectedDirectoryError,
};
use sha2::{Digest, Sha256};
use spaceterm_distribution::manifest::{MAX_EXPANDED_BYTES, MAX_FILES};
use std::{
    collections::BTreeMap,
    ffi::{CString, OsStr},
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileExt, MetadataExt},
        },
    },
    path::{Component, Path},
    sync::Arc,
};

fn storage(_: std::io::Error) -> NativeFailure {
    NativeFailure::IoUnavailable
}
pub(crate) fn protection(error: ProtectedDirectoryError) -> NativeFailure {
    match error {
        ProtectedDirectoryError::NativeUnavailable => NativeFailure::CustodyUnavailable,
        ProtectedDirectoryError::TooLarge => NativeFailure::LimitExceeded,
        ProtectedDirectoryError::Missing => NativeFailure::InstallationUnavailable,
        ProtectedDirectoryError::Unavailable => NativeFailure::StorageUnavailable,
        _ => NativeFailure::UnsafeNativeState,
    }
}
fn component(name: &OsStr) -> Result<CString, NativeFailure> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.contains(&b'/') || matches!(bytes, b"." | b"..") {
        return Err(NativeFailure::InvalidRecord);
    }
    CString::new(bytes).map_err(|_| NativeFailure::InvalidRecord)
}
fn open_at(parent: &File, name: &CString, flags: i32) -> Result<File, NativeFailure> {
    // SAFETY: parent is retained and name is a single NUL-terminated component.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    // SAFETY: successful openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn sync(file: &File) -> Result<(), NativeFailure> {
    file.sync_all().map_err(storage)?;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: F_FULLFSYNC acts on the retained file, without pointer arguments.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
    }
    Ok(())
}
fn mode(file: &File, value: u32) -> Result<(), NativeFailure> {
    // SAFETY: fchmod changes only this owned fresh inode.
    if unsafe { libc::fchmod(file.as_raw_fd(), value as libc::mode_t) } != 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    Ok(())
}

#[derive(Eq, PartialEq)]
struct Version {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    content: Option<(u64, i64, i64, i64, i64)>,
}
impl Version {
    fn of(file: &File, directory: bool) -> Result<Self, NativeFailure> {
        let m = file.metadata().map_err(storage)?;
        if (directory && !m.is_dir()) || (!directory && (!m.is_file() || m.nlink() != 1)) {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(Self {
            device: m.dev(),
            inode: m.ino(),
            mode: m.mode(),
            owner: m.uid(),
            group: m.gid(),
            content: (!directory).then(|| {
                (
                    m.size(),
                    m.mtime(),
                    m.mtime_nsec(),
                    m.ctime(),
                    m.ctime_nsec(),
                )
            }),
        })
    }
    fn link_matches(&self, parent: &File, name: &CString) -> Result<(), NativeFailure> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: initialized output is read only after successful descriptor-relative lstat.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        // SAFETY: fstatat initialized the entire native stat structure.
        let stat = unsafe { stat.assume_init() };
        #[cfg(target_os = "macos")]
        let (device, mode) = (stat.st_dev as u64, u32::from(stat.st_mode));
        #[cfg(not(target_os = "macos"))]
        let (device, mode) = (stat.st_dev, stat.st_mode);
        if device != self.device
            || stat.st_ino != self.inode
            || mode != self.mode
            || stat.st_uid != self.owner
            || stat.st_gid != self.group
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(())
    }
}
struct Directory {
    file: File,
    version: Version,
    link: Option<(usize, CString)>,
}

pub(crate) struct DirectoryTree {
    nodes: Vec<Directory>,
    pub(crate) protection: Arc<dyn NativePublicationProtection>,
    generation: Option<usize>,
    children: BTreeMap<String, usize>,
}
impl DirectoryTree {
    pub(crate) fn open(
        path: &Path,
        protection: Arc<dyn NativePublicationProtection>,
    ) -> Result<Self, NativeFailure> {
        if !path.is_absolute()
            || path
                .as_os_str()
                .as_bytes()
                .split(|b| *b == b'/')
                .any(|p| matches!(p, b"." | b".."))
        {
            return Err(NativeFailure::InvalidRecord);
        }
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .map_err(storage)?;
        let mut tree = Self {
            nodes: vec![Directory {
                version: Version::of(&root, true)?,
                file: root,
                link: None,
            }],
            protection,
            generation: None,
            children: BTreeMap::new(),
        };
        for part in path.components() {
            match part {
                Component::RootDir => (),
                Component::Normal(name) => {
                    if tree.nodes.len() >= 255 {
                        return Err(NativeFailure::LimitExceeded);
                    }
                    tree.push_directory(tree.nodes.len() - 1, component(name)?)?;
                }
                _ => return Err(NativeFailure::InvalidRecord),
            }
        }
        tree.verify(&[])?;
        Ok(tree)
    }
    fn push_directory(&mut self, parent: usize, name: CString) -> Result<usize, NativeFailure> {
        let file = open_at(
            &self.nodes[parent].file,
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY,
        )?;
        let version = Version::of(&file, true)?;
        version.link_matches(&self.nodes[parent].file, &name)?;
        self.protection
            .verify_directory(&file)
            .map_err(protection)?;
        let index = self.nodes.len();
        self.nodes.push(Directory {
            file,
            version,
            link: Some((parent, name)),
        });
        Ok(index)
    }
    pub(crate) fn require_absent(&self, id: u64) -> Result<(), NativeFailure> {
        self.verify(&[])?;
        let name = component(OsStr::new(&id.to_string()))?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: no-follow existence check against the retained publication parent.
        let result = unsafe {
            libc::fstatat(
                self.parent().as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
            return Err(NativeFailure::IoUnavailable);
        }
        Ok(())
    }
    fn parent(&self) -> &File {
        &self.nodes[self.nodes.len() - 1].file
    }
    pub(crate) fn create_generation(&mut self, id: u64) -> Result<(), NativeFailure> {
        let parent = self.nodes.len() - 1;
        let name = component(OsStr::new(&id.to_string()))?;
        self.mkdir(parent, &name)?;
        let generation = self.push_directory(parent, name)?;
        self.record_directory(generation)?;
        self.generation = Some(generation);
        Ok(())
    }
    pub(crate) fn retain_generation(&mut self, id: u64) -> Result<(), NativeFailure> {
        let parent = self.nodes.len() - 1;
        self.generation =
            Some(self.push_directory(parent, component(OsStr::new(&id.to_string()))?)?);
        Ok(())
    }
    fn record_directory(&self, index: usize) -> Result<(), NativeFailure> {
        let node = &self.nodes[index];
        let (parent, _) = node.link.as_ref().ok_or(NativeFailure::UnsafeNativeState)?;
        self.protection.record_fresh(&FreshPublicationInode {
            parent: &self.nodes[*parent].file,
            destination: &node.file,
        })
    }
    fn mkdir(&self, parent: usize, name: &CString) -> Result<(), NativeFailure> {
        // SAFETY: mkdirat creates a new directory and refuses every preexisting entry.
        if unsafe { libc::mkdirat(self.nodes[parent].file.as_raw_fd(), name.as_ptr(), 0o700) } != 0
        {
            return Err(NativeFailure::IoUnavailable);
        }
        Ok(())
    }
    fn entry_parent(
        &mut self,
        relative: &str,
        fresh: bool,
    ) -> Result<(usize, CString), NativeFailure> {
        let mut parent = self.generation.ok_or(NativeFailure::UnsafeNativeState)?;
        let mut prefix = String::new();
        let mut parts = relative.split('/').peekable();
        while let Some(part) = parts.next() {
            let name = component(OsStr::new(part))?;
            if parts.peek().is_none() {
                return Ok((parent, name));
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            parent = if let Some(index) = self.children.get(&prefix) {
                *index
            } else {
                if self.children.len() >= MAX_FILES {
                    return Err(NativeFailure::LimitExceeded);
                }
                if fresh {
                    self.mkdir(parent, &name)?;
                }
                let index = self.push_directory(parent, name)?;
                if fresh {
                    self.record_directory(index)?;
                }
                self.children.insert(prefix.clone(), index);
                index
            };
        }
        Err(NativeFailure::InvalidRecord)
    }
    pub(crate) fn copy_file(
        &mut self,
        relative: &str,
        size: u64,
        permissions: u32,
        digest: [u8; 32],
        input: &mut dyn Read,
    ) -> Result<BoundFile, NativeFailure> {
        let (parent, name) = self.entry_parent(relative, true)?;
        let mut writer = open_at(
            &self.nodes[parent].file,
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        let initial = writer.metadata().map_err(storage)?;
        let mut remaining = size;
        let mut buffer = [0; 64 * 1024];
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            input.read_exact(&mut buffer[..count]).map_err(storage)?;
            writer.write_all(&buffer[..count]).map_err(storage)?;
            remaining -= count as u64;
        }
        mode(&writer, permissions)?;
        sync(&writer)?;
        // No writable mapping is ever created, and no writer is passed to a native peer.
        // An unsuccessful close is uncertainty. Never retry a descriptor number after close.
        let fd = writer.into_raw_fd();
        // SAFETY: fd is the uniquely owned staging writer whose ownership was just transferred.
        if unsafe { libc::close(fd) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        let file = open_at(&self.nodes[parent].file, &name, libc::O_RDONLY)?;
        let observed = file.metadata().map_err(storage)?;
        if (observed.dev(), observed.ino()) != (initial.dev(), initial.ino()) {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let bound = self.bind_file(relative, (parent, name), file, size, permissions, digest)?;
        self.protection.record_fresh(&FreshPublicationInode {
            parent: &self.nodes[parent].file,
            destination: &bound.file,
        })?;
        Ok(bound)
    }
    fn bind_file(
        &self,
        relative: &str,
        link: (usize, CString),
        file: File,
        size: u64,
        permissions: u32,
        digest: [u8; 32],
    ) -> Result<BoundFile, NativeFailure> {
        let (parent, name) = link;
        let metadata = file.metadata().map_err(storage)?;
        if metadata.size() != size || metadata.mode() & 0o7777 != permissions {
            return Err(NativeFailure::ArtifactRejected);
        }
        let version = Version::of(&file, false)?;
        let bound = BoundFile {
            relative: relative.to_owned(),
            parent,
            name,
            file,
            version,
            digest,
        };
        bound.verify(self)?;
        Ok(bound)
    }
    pub(crate) fn retain_file(
        &mut self,
        relative: &str,
        size: u64,
        permissions: u32,
        digest: [u8; 32],
    ) -> Result<BoundFile, NativeFailure> {
        let (parent, name) = self.entry_parent(relative, false)?;
        let file = open_at(&self.nodes[parent].file, &name, libc::O_RDONLY)?;
        self.bind_file(relative, (parent, name), file, size, permissions, digest)
    }
    pub(crate) fn read_record(
        &mut self,
        relative: &str,
        limit: usize,
    ) -> Result<(BoundFile, Vec<u8>), NativeFailure> {
        let (parent, name) = self.entry_parent(relative, false)?;
        let file = open_at(&self.nodes[parent].file, &name, libc::O_RDONLY)?;
        self.protection
            .verify_regular_file(&file)
            .map_err(protection)?;
        let before = Version::of(&file, false)?;
        let bytes = read_bounded(&file, limit)?;
        let digest = Sha256::digest(&bytes).into();
        let bound = self.bind_file(
            relative,
            (parent, name),
            file,
            bytes.len() as u64,
            0o400,
            digest,
        )?;
        if before != bound.version {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok((bound, bytes))
    }
    pub(crate) fn seal(&mut self) -> Result<(), NativeFailure> {
        let generation = self.generation.ok_or(NativeFailure::UnsafeNativeState)?;
        for node in self.nodes.iter_mut().skip(generation).rev() {
            mode(&node.file, 0o555)?;
            node.version = Version::of(&node.file, true)?;
            sync(&node.file)?;
        }
        // Persist the generation entry in its original protected publication parent.
        let (parent, _) = self.nodes[generation]
            .link
            .as_ref()
            .ok_or(NativeFailure::UnsafeNativeState)?;
        sync(&self.nodes[*parent].file)
    }
    pub(crate) fn observe(
        &self,
        files: &[BoundFile],
    ) -> Result<Vec<crate::protected_directory::observation::Observation>, NativeFailure> {
        use crate::protected_directory::observation::{MAX_HANDLES, Observation};
        let handles: Vec<_> = self
            .nodes
            .iter()
            .map(|node| &node.file)
            .chain(files.iter().map(|file| &file.file))
            .collect();
        let mut observations = Vec::new();
        for chunk in handles.chunks(MAX_HANDLES) {
            let mut observation = Observation::begin().map_err(protection)?;
            for handle in chunk {
                observation.enroll(handle).map_err(protection)?;
            }
            observations.push(observation);
        }
        Ok(observations)
    }
    pub(crate) fn verify(&self, files: &[BoundFile]) -> Result<(), NativeFailure> {
        for node in &self.nodes {
            self.protection
                .verify_directory(&node.file)
                .map_err(protection)?;
            if node.version != Version::of(&node.file, true)? {
                return Err(NativeFailure::UnsafeNativeState);
            }
            if let Some((parent, name)) = &node.link {
                node.version.link_matches(&self.nodes[*parent].file, name)?;
            }
        }
        for file in files {
            file.verify(self)?;
        }
        let handles: Vec<_> = self
            .nodes
            .iter()
            .map(|n| &n.file)
            .chain(files.iter().map(|f| &f.file))
            .collect();
        self.protection.verify_custody(&handles)
    }
}
use std::os::unix::fs::OpenOptionsExt;

pub(crate) struct BoundFile {
    pub(crate) relative: String,
    pub(crate) file: File,
    parent: usize,
    name: CString,
    version: Version,
    digest: [u8; 32],
}
impl BoundFile {
    fn verify(&self, tree: &DirectoryTree) -> Result<(), NativeFailure> {
        tree.protection
            .verify_regular_file(&self.file)
            .map_err(protection)?;
        if self.version != Version::of(&self.file, false)? {
            return Err(NativeFailure::UnsafeNativeState);
        }
        self.version
            .link_matches(&tree.nodes[self.parent].file, &self.name)?;
        if hash_file(&self.file)? != self.digest {
            return Err(NativeFailure::ArtifactRejected);
        }
        if self.version != Version::of(&self.file, false)? {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(())
    }
    pub(crate) fn view(&self) -> PublicationFile<'_> {
        PublicationFile {
            name: &self.relative,
            file: &self.file,
        }
    }
}
pub(crate) fn hash_file(file: &File) -> Result<[u8; 32], NativeFailure> {
    let size = file.metadata().map_err(storage)?.size();
    if size > MAX_EXPANDED_BYTES {
        return Err(NativeFailure::LimitExceeded);
    }
    let mut hash = Sha256::new();
    let mut offset = 0;
    let mut buffer = [0; 64 * 1024];
    while offset < size {
        let count = ((size - offset).min(buffer.len() as u64)) as usize;
        let read = file
            .read_at(&mut buffer[..count], offset)
            .map_err(storage)?;
        if read == 0 {
            return Err(NativeFailure::ArtifactRejected);
        }
        hash.update(&buffer[..read]);
        offset += read as u64;
    }
    if file.read_at(&mut buffer[..1], offset).map_err(storage)? != 0 {
        return Err(NativeFailure::ArtifactRejected);
    }
    Ok(hash.finalize().into())
}
fn read_bounded(file: &File, limit: usize) -> Result<Vec<u8>, NativeFailure> {
    let size = file.metadata().map_err(storage)?.len();
    if size > limit as u64 {
        return Err(NativeFailure::LimitExceeded);
    }
    let mut bytes = vec![0; size as usize];
    let mut offset = 0;
    while offset < bytes.len() {
        let n = file
            .read_at(&mut bytes[offset..], offset as u64)
            .map_err(storage)?;
        if n == 0 {
            return Err(NativeFailure::ArtifactRejected);
        }
        offset += n;
    }
    let mut extra = [0];
    if file.read_at(&mut extra, size).map_err(storage)? != 0 {
        return Err(NativeFailure::LimitExceeded);
    }
    Ok(bytes)
}
