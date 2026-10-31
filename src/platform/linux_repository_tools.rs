//! Finds git and the GitHub CLI on the PATH captured from the desktop launch environment.

use super::bounded_path::{directories, is_executable_file};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::repository_status::{RepositoryToolDiscovery, ToolInventory};

/// Searches the absolute entries of the captured PATH, then the fallback directory.
pub(crate) struct LinuxRepositoryToolDiscovery {
    directories: Vec<PathBuf>,
}

impl LinuxRepositoryToolDiscovery {
    /// `path` is the PATH captured once at launch. `/usr/bin` is searched last.
    pub(crate) fn new(path: Option<&OsStr>) -> Self {
        Self::with_fallback(path, PathBuf::from("/usr/bin"))
    }

    pub(crate) fn with_fallback(path: Option<&OsStr>, fallback: PathBuf) -> Self {
        let mut directories: Vec<PathBuf> = directories(path).collect();
        directories.push(fallback);
        Self { directories }
    }

    fn find(&self, name: &str) -> Option<PathBuf> {
        let name = OsString::from(name);
        self.directories
            .iter()
            .map(|directory| directory.join(&name))
            .find(|path| is_executable_file(path))
    }
}

impl RepositoryToolDiscovery for LinuxRepositoryToolDiscovery {
    fn discover(&self) -> ToolInventory {
        ToolInventory {
            git: self.find("git"),
            github_cli: self.find("gh"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spaceterm-repository-tools-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn directory(&self, name: &str, programs: &[(&str, bool)]) -> PathBuf {
            let directory = self.0.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            for (program, executable) in programs {
                let path = directory.join(program);
                std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode(if *executable { 0o700 } else { 0o600 }),
                )
                .unwrap();
            }
            directory
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn linux_repository_tools_select_the_first_executable_in_absolute_path_entries() {
        let fixture = Fixture::new();
        let unexecutable = fixture.directory("unexecutable", &[("git", false), ("gh", false)]);
        let first = fixture.directory("first", &[("git", true)]);
        let second = fixture.directory("second", &[("git", true), ("gh", true)]);
        let fallback = fixture.directory("fallback", &[("git", true), ("gh", true)]);
        let path = std::env::join_paths([
            PathBuf::from("relative"),
            unexecutable,
            first.clone(),
            second.clone(),
        ])
        .unwrap();

        assert_eq!(
            LinuxRepositoryToolDiscovery::with_fallback(Some(&path), fallback).discover(),
            ToolInventory {
                git: Some(first.join("git")),
                github_cli: Some(second.join("gh")),
            }
        );
    }

    #[test]
    fn linux_repository_tools_fall_back_for_missing_unsafe_or_excessive_paths() {
        let fixture = Fixture::new();
        let listed = fixture.directory("listed", &[("git", true), ("gh", true)]);
        let fallback = fixture.directory("fallback", &[("git", true)]);
        let too_many = std::env::join_paths(
            std::iter::repeat_n(PathBuf::from("relative"), 128)
                .chain(std::iter::once(listed.clone())),
        )
        .unwrap();
        let oversized = OsString::from(format!("{}:{}", "x".repeat(16 * 1024), listed.display()));
        let unsafe_value = OsString::from(format!("{}:/unsafe\nvalue", listed.display()));

        for path in [
            None,
            Some(OsStr::new("")),
            Some(too_many.as_os_str()),
            Some(oversized.as_os_str()),
            Some(unsafe_value.as_os_str()),
        ] {
            assert_eq!(
                LinuxRepositoryToolDiscovery::with_fallback(path, fallback.clone()).discover(),
                ToolInventory {
                    git: Some(fallback.join("git")),
                    github_cli: None,
                }
            );
        }
    }

    #[test]
    fn linux_repository_tools_accept_profile_symlinks_but_skip_non_regular_candidates() {
        let fixture = Fixture::new();
        let target = fixture.directory("store", &[("git", true)]).join("git");
        let profile = fixture.directory("profile", &[]);
        symlink(target, profile.join("git")).unwrap();
        let directory_candidate = fixture.directory("directory", &[]);
        std::fs::create_dir(directory_candidate.join("git")).unwrap();
        let path = std::env::join_paths([directory_candidate, profile.clone()]).unwrap();

        assert_eq!(
            LinuxRepositoryToolDiscovery::with_fallback(Some(&path), fixture.0.join("missing"))
                .discover()
                .git,
            Some(profile.join("git"))
        );
    }
}
