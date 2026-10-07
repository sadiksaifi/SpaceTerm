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

use crate::repository_status::{
    ApplyMarkers, MAXIMUM_MARKER_BYTES, OperationMarkers, RepositoryMarkerReader, StepMarkers,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct UnixRepositoryMarkerReader;

#[allow(dead_code, reason = "Repository Status composition selects it in a later change")]
impl UnixRepositoryMarkerReader {
    pub(crate) const fn new() -> Self {
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

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::platform::unix_adapter_tests::short_temporary_root;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let root = short_temporary_root().join(format!(
                "spaceterm-markers-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }

        fn read(&self) -> OperationMarkers {
            UnixRepositoryMarkerReader::new().read(&self.0)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unix_repository_markers_are_absent_in_a_quiet_or_missing_git_directory() {
        let fixture = Fixture::new();
        fixture.write("HEAD", b"ref: refs/heads/main\n");

        assert_eq!(fixture.read(), OperationMarkers::default());
        assert_eq!(
            UnixRepositoryMarkerReader::new().read(&fixture.0.join("missing")),
            OperationMarkers::default()
        );
    }

    #[test]
    fn unix_repository_markers_read_rebase_steps_and_operation_files() {
        let fixture = Fixture::new();
        fixture.write("rebase-merge/msgnum", b"3\n");
        fixture.write("rebase-merge/end", b"7\n");
        fixture.write("rebase-apply/next", b"2\n");
        fixture.write("rebase-apply/last", b"5\n");
        fixture.write("rebase-apply/applying", b"");
        for name in ["MERGE_HEAD", "REVERT_HEAD", "CHERRY_PICK_HEAD", "BISECT_LOG"] {
            fixture.write(name, b"0123456789abcdef\n");
        }

        assert_eq!(
            fixture.read(),
            OperationMarkers {
                rebase_merge: Some(StepMarkers {
                    current: Some(b"3\n".to_vec()),
                    total: Some(b"7\n".to_vec()),
                }),
                rebase_apply: Some(ApplyMarkers {
                    step: StepMarkers {
                        current: Some(b"2\n".to_vec()),
                        total: Some(b"5\n".to_vec()),
                    },
                    rebasing: false,
                    applying: true,
                }),
                merge_head: true,
                revert_head: true,
                cherry_pick_head: true,
                bisect_log: true,
            }
        );
    }

    #[test]
    fn unix_repository_markers_reject_symbolic_links() {
        let fixture = Fixture::new();
        let elsewhere = Fixture::new();
        let target = elsewhere.write("rebase-merge/msgnum", b"3\n");
        elsewhere.write("MERGE_HEAD", b"0123456789abcdef\n");
        symlink(target.parent().unwrap(), fixture.0.join("rebase-merge")).unwrap();
        symlink(elsewhere.0.join("MERGE_HEAD"), fixture.0.join("MERGE_HEAD")).unwrap();
        fixture.write("rebase-apply/last", b"5\n");
        symlink(&target, fixture.0.join("rebase-apply/next")).unwrap();
        symlink(
            elsewhere.0.join("MERGE_HEAD"),
            fixture.0.join("rebase-apply/rebasing"),
        )
        .unwrap();

        assert_eq!(
            fixture.read(),
            OperationMarkers {
                rebase_apply: Some(ApplyMarkers {
                    step: StepMarkers {
                        current: None,
                        total: Some(b"5\n".to_vec()),
                    },
                    rebasing: false,
                    applying: false,
                }),
                ..OperationMarkers::default()
            }
        );
    }

    #[test]
    fn unix_repository_markers_reject_oversized_and_non_regular_files() {
        let fixture = Fixture::new();
        fixture.write("rebase-merge/msgnum", &[b'1'; MAXIMUM_MARKER_BYTES + 1]);
        fixture.write("rebase-merge/end", &[b'9'; MAXIMUM_MARKER_BYTES]);
        std::fs::create_dir(fixture.0.join("MERGE_HEAD")).unwrap();
        let fifo = CString::new(
            fixture
                .0
                .join("rebase-apply/next")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        std::fs::create_dir(fixture.0.join("rebase-apply")).unwrap();
        // SAFETY: the NUL-terminated path stays live during the call.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        assert_eq!(
            fixture.read(),
            OperationMarkers {
                rebase_merge: Some(StepMarkers {
                    current: None,
                    total: Some(vec![b'9'; MAXIMUM_MARKER_BYTES]),
                }),
                rebase_apply: Some(ApplyMarkers::default()),
                ..OperationMarkers::default()
            }
        );
    }
}
