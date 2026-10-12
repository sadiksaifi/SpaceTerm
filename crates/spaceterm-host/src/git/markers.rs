//! Reads git operation marker files without following symbolic links.
//!
//! Every marker is opened relative to a descriptor for the git directory, so a marker that is a
//! symbolic link, or a link swapped in during the read, counts as absent instead of redirecting the
//! read elsewhere.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use crate::git::{
    ApplyMarkers, MAXIMUM_MARKER_BYTES, OperationMarkers, RepositoryMarkerReader, StepMarkers,
};

#[derive(Clone, Copy, Debug, Default)]
/// Read bounded Git operation markers without following child symbolic links.
pub struct UnixRepositoryMarkerReader;

impl UnixRepositoryMarkerReader {
    /// Construct the stateless native marker reader.
    pub const fn new() -> Self {
        Self
    }
}

impl RepositoryMarkerReader for UnixRepositoryMarkerReader {
    fn read(&self, git_directory: &Path) -> OperationMarkers {
        let Some(directory) = Directory::open(git_directory) else {
            return OperationMarkers::default();
        };
        OperationMarkers {
            rebase_merge: directory.child(c"rebase-merge").map(|rebase| StepMarkers {
                current: rebase.read(c"msgnum"),
                total: rebase.read(c"end"),
            }),
            rebase_apply: directory.child(c"rebase-apply").map(|apply| ApplyMarkers {
                step: StepMarkers {
                    current: apply.read(c"next"),
                    total: apply.read(c"last"),
                },
                rebasing: apply.is_file(c"rebasing"),
                applying: apply.is_file(c"applying"),
            }),
            merge_head: directory.is_file(c"MERGE_HEAD"),
            revert_head: directory.is_file(c"REVERT_HEAD"),
            cherry_pick_head: directory.is_file(c"CHERRY_PICK_HEAD"),
            bisect_log: directory.is_file(c"BISECT_LOG"),
        }
    }
}

struct Directory(OwnedFd);

impl Directory {
    /// Opens the git directory the probe reported. Its own path may contain symbolic links.
    fn open(path: &Path) -> Option<Self> {
        let path = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        // SAFETY: the NUL-terminated path stays live during the call.
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        owned(descriptor).map(Self)
    }

    /// Opens a child directory that is not a symbolic link.
    fn child(&self, name: &CStr) -> Option<Self> {
        // SAFETY: the directory descriptor is live and the name is NUL-terminated.
        let descriptor = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        owned(descriptor).map(Self)
    }

    /// Reads a regular file of at most [`MAXIMUM_MARKER_BYTES`]; anything else reads as absent.
    fn read(&self, name: &CStr) -> Option<Vec<u8>> {
        // O_NONBLOCK keeps a FIFO from blocking the open; the type check below then rejects it.
        // SAFETY: the directory descriptor is live and the name is NUL-terminated.
        let descriptor = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        let file = File::from(owned(descriptor)?);
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut contents = Vec::with_capacity(MAXIMUM_MARKER_BYTES + 1);
        file.take(MAXIMUM_MARKER_BYTES as u64 + 1)
            .read_to_end(&mut contents)
            .ok()?;
        (contents.len() <= MAXIMUM_MARKER_BYTES).then_some(contents)
    }

    /// Reports whether `name` is a regular file, without following a symbolic link.
    fn is_file(&self, name: &CStr) -> bool {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the directory descriptor is live, the name is NUL-terminated, and the status
        // pointer is valid for writes.
        let result = unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        // SAFETY: fstatat initialized the status after succeeding.
        result == 0 && unsafe { status.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFREG
    }
}

fn owned(descriptor: libc::c_int) -> Option<OwnedFd> {
    // SAFETY: a non-negative descriptor was just returned by open or openat and has no other owner.
    (descriptor >= 0).then(|| unsafe { OwnedFd::from_raw_fd(descriptor) })
}
