//! Capture-once OpenSSH selection from the inherited desktop launch environment.
use crate::ssh::command::{OpenSshExecutable, OpenSshExecutableError};
use std::{ffi::OsStr, path::PathBuf};

const MAXIMUM_PATH_BYTES: usize = 16 * 1024;
const MAXIMUM_PATH_ENTRIES: usize = 128;

pub(super) fn capture(path: Option<&OsStr>) -> Result<OpenSshExecutable, OpenSshExecutableError> {
    let selected = path
        .filter(|path| {
            let bytes = path.as_encoded_bytes();
            bytes.len() <= MAXIMUM_PATH_BYTES && !bytes.iter().any(u8::is_ascii_control)
        })
        .and_then(|path| {
            std::env::split_paths(path)
                .take(MAXIMUM_PATH_ENTRIES)
                .filter(|directory| directory.is_absolute())
                .find_map(|directory| {
                    let path = directory.join("ssh");
                    let executable = OpenSshExecutable::new(path.clone()).ok()?;
                    is_executable_file(&path).then_some(executable)
                })
        });
    selected
        .map(Ok)
        .unwrap_or_else(|| OpenSshExecutable::new(PathBuf::from("/usr/bin/ssh")))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: the NUL-terminated local path stays live during this non-mutating access check.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "spaceterm-ssh-selection-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn directory(&self, name: &str, executable: bool) -> PathBuf {
            let directory = self.0.join(name);
            std::fs::create_dir(&directory).unwrap();
            let executable_path = directory.join("ssh");
            std::fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(
                executable_path,
                std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
            )
            .unwrap();
            directory
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn linux_ssh_executable_selects_first_executable_in_absolute_inherited_directories() {
        let fixture = Fixture::new();
        let non_executable = fixture.directory("non-executable", false);
        let first = fixture.directory("nix-profile", true);
        let later = fixture.directory("later", true);
        let path = std::env::join_paths([non_executable, first.clone(), later]).unwrap();
        assert_eq!(
            capture(Some(&path)).unwrap(),
            OpenSshExecutable::new(first.join("ssh")).unwrap()
        );
    }

    #[test]
    fn linux_ssh_executable_skips_relative_entries_even_when_they_contain_ssh() {
        let fixture = Fixture::new();
        let relative_target = fixture.directory("relative", true);
        let absolute = fixture.directory("absolute", true);
        let mut relative = PathBuf::new();
        for _ in std::env::current_dir()
            .unwrap()
            .components()
            .filter(|component| matches!(component, std::path::Component::Normal(_)))
        {
            relative.push("..");
        }
        relative.push(relative_target.strip_prefix("/").unwrap());
        assert!(relative.join("ssh").is_file());
        let path = std::env::join_paths([PathBuf::new(), relative, absolute.clone()]).unwrap();
        assert_eq!(
            capture(Some(&path)).unwrap(),
            OpenSshExecutable::new(absolute.join("ssh")).unwrap()
        );
    }

    #[test]
    fn linux_ssh_executable_falls_back_for_missing_unsafe_or_excessive_search_paths() {
        let fixture = Fixture::new();
        let executable = fixture.directory("bounded", true);
        let too_many = std::env::join_paths(
            std::iter::repeat_n(PathBuf::from("relative"), 128).chain(std::iter::once(executable)),
        )
        .unwrap();
        let oversized = std::ffi::OsString::from("x".repeat(16 * 1024 + 1));
        for path in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("relative::/missing-ssh-fixture")),
            Some(OsStr::new("/unsafe\nvalue")),
            Some(oversized.as_os_str()),
            Some(too_many.as_os_str()),
        ] {
            assert_eq!(
                capture(path).unwrap(),
                OpenSshExecutable::new(PathBuf::from("/usr/bin/ssh")).unwrap()
            );
        }
    }

    #[test]
    fn linux_ssh_executable_accepts_profile_symlinks_but_skips_non_regular_candidates() {
        let fixture = Fixture::new();
        let target = fixture.directory("target", true).join("ssh");
        let profile = fixture.0.join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::os::unix::fs::symlink(target, profile.join("ssh")).unwrap();
        let directory_candidate = fixture.0.join("directory");
        std::fs::create_dir_all(directory_candidate.join("ssh")).unwrap();
        let path = std::env::join_paths([directory_candidate, profile.clone()]).unwrap();
        let retained = capture(Some(&path)).unwrap();
        std::fs::remove_file(profile.join("ssh")).unwrap();
        assert_eq!(
            retained,
            OpenSshExecutable::new(profile.join("ssh")).unwrap()
        );
        assert_eq!(format!("{retained:?}"), "OpenSshExecutable(<redacted>)");
    }
}
