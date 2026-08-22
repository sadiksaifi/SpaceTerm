use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::domain::CanonicalDirectoryId;

/// Pure data identity token consumed by the domain for duplicate detection.
pub(crate) fn canonical_identity(path: &Path) -> Option<CanonicalDirectoryId> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(CanonicalDirectoryId::new(metadata.dev(), metadata.ino()))
}

/// True when path currently exists, is a local directory, and is readable.
pub(crate) fn is_valid_local_directory(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_dir() && std::fs::read_dir(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unique_temp_root(label: &str) -> TempDirGuard {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "spaceterm-fs-identity-{label}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create unique temp root");
        TempDirGuard(dir)
    }

    #[test]
    fn existing_directory_yields_identity_and_missing_path_does_not() {
        let root = unique_temp_root("basic");
        assert!(canonical_identity(root.path()).is_some());
        assert!(canonical_identity(&root.0.join("missing")).is_none());
    }

    #[test]
    fn regular_file_yields_identity() {
        let root = unique_temp_root("file");
        let file = root.0.join("notes.txt");
        std::fs::write(&file, b"payload").expect("write temp file");
        assert!(canonical_identity(&file).is_some());
    }

    #[test]
    fn distinct_directories_have_distinct_identities() {
        let root = unique_temp_root("distinct");
        let first = root.0.join("first");
        let second = root.0.join("second");
        std::fs::create_dir_all(&first).expect("create first");
        std::fs::create_dir_all(&second).expect("create second");
        assert_ne!(canonical_identity(&first), canonical_identity(&second));
    }

    #[test]
    fn symlink_to_directory_shares_target_identity() {
        use std::os::unix::fs::symlink;

        let root = unique_temp_root("symlink");
        let target = root.0.join("target");
        let link = root.0.join("link");
        std::fs::create_dir_all(&target).expect("create symlink target");
        symlink(&target, &link).expect("create symlink");
        assert_eq!(canonical_identity(&link), canonical_identity(&target));
    }

    #[test]
    fn trailing_slash_preserves_identity() {
        let root = unique_temp_root("trailing");
        let dir = root.0.join("dir");
        std::fs::create_dir_all(&dir).expect("create dir");
        let with_slash = PathBuf::from(format!("{}/", dir.to_string_lossy()));
        assert_eq!(canonical_identity(&with_slash), canonical_identity(&dir));
    }

    #[test]
    fn dot_dot_path_resolves_to_same_directory() {
        let root = unique_temp_root("dotdot");
        let nested = root.0.join("a/b");
        std::fs::create_dir_all(&nested).expect("create nested dirs");
        let detour = root.0.join("a/b/../b");
        assert_eq!(canonical_identity(&detour), canonical_identity(&nested));
    }

    #[test]
    fn validity_distinguishes_directory_file_and_missing() {
        let root = unique_temp_root("validity");
        let dir = root.0.join("dir");
        let file = root.0.join("file.txt");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(&file, b"payload").expect("write file");
        assert!(is_valid_local_directory(&dir));
        assert!(!is_valid_local_directory(&file));
        assert!(!is_valid_local_directory(&root.0.join("missing")));
    }
}
