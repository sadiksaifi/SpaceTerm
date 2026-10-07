//! Finds git and the GitHub CLI at fixed install locations.
//!
//! An app launched from the Finder or the Dock has a minimal PATH, so discovery never searches it.
//! It never selects `/usr/bin/git`: when Command Line Tools are missing, that stub opens the
//! "install developer tools" dialog instead of running git.

use std::path::{Path, PathBuf};

use crate::repository_status::{RepositoryToolDiscovery, ToolInventory};

/// The `xcode-select` stub refused as a git candidate.
const SYSTEM_STUB: &str = "/usr/bin/git";

/// One place git may be installed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ToolCandidate {
    Fixed(PathBuf),
    /// `usr/bin/git` inside the developer directory a symbolic link names, read at discovery.
    DeveloperDirectory {
        link: PathBuf,
    },
}

/// Checks each candidate in order and keeps the first executable regular file.
pub(crate) struct MacosRepositoryToolDiscovery {
    git: Vec<ToolCandidate>,
    github_cli: Vec<PathBuf>,
}

impl MacosRepositoryToolDiscovery {
    /// Homebrew on Apple silicon and Intel, the `xcode-select` developer directory, Xcode, and
    /// Command Line Tools, in that order.
    pub(crate) fn new() -> Self {
        Self::with_candidates(
            vec![
                ToolCandidate::Fixed(PathBuf::from("/opt/homebrew/bin/git")),
                ToolCandidate::Fixed(PathBuf::from("/usr/local/bin/git")),
                ToolCandidate::DeveloperDirectory {
                    link: PathBuf::from("/var/db/xcode_select_link"),
                },
                ToolCandidate::Fixed(PathBuf::from(
                    "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
                )),
                ToolCandidate::Fixed(PathBuf::from(
                    "/Library/Developer/CommandLineTools/usr/bin/git",
                )),
            ],
            vec![
                PathBuf::from("/opt/homebrew/bin/gh"),
                PathBuf::from("/usr/local/bin/gh"),
            ],
        )
    }

    pub(crate) fn with_candidates(git: Vec<ToolCandidate>, github_cli: Vec<PathBuf>) -> Self {
        Self { git, github_cli }
    }
}

impl RepositoryToolDiscovery for MacosRepositoryToolDiscovery {
    fn discover(&self) -> ToolInventory {
        ToolInventory {
            git: self
                .git
                .iter()
                .filter_map(ToolCandidate::resolve)
                .find(|path| path != Path::new(SYSTEM_STUB) && is_executable_file(path)),
            github_cli: self
                .github_cli
                .iter()
                .find(|path| is_executable_file(path))
                .cloned(),
        }
    }
}

impl ToolCandidate {
    fn resolve(&self) -> Option<PathBuf> {
        match self {
            Self::Fixed(path) => Some(path.clone()),
            Self::DeveloperDirectory { link } => {
                let target = std::fs::read_link(link).ok()?;
                let target = if target.is_absolute() {
                    target
                } else {
                    link.parent()?.join(target)
                };
                Some(target.join("usr/bin/git"))
            }
        }
    }
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_absolute() || !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
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

        fn program(&self, relative: &str, executable: bool) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
            )
            .unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn macos_repository_tools_select_the_first_executable_regular_file() {
        let fixture = Fixture::new();
        let unexecutable = fixture.program("homebrew/git", false);
        let directory = fixture.0.join("local/git");
        std::fs::create_dir_all(&directory).unwrap();
        let installed = fixture.program("tools/git", true);
        let later = fixture.program("later/git", true);
        let gh = fixture.program("local/gh", true);
        let discovery = MacosRepositoryToolDiscovery::with_candidates(
            vec![
                ToolCandidate::Fixed(fixture.0.join("missing/git")),
                ToolCandidate::Fixed(unexecutable),
                ToolCandidate::Fixed(directory),
                ToolCandidate::Fixed(installed.clone()),
                ToolCandidate::Fixed(later),
            ],
            vec![fixture.0.join("missing/gh"), gh.clone()],
        );

        assert_eq!(
            discovery.discover(),
            ToolInventory {
                git: Some(installed),
                github_cli: Some(gh),
            }
        );
    }

    #[test]
    fn macos_repository_tools_follow_the_developer_directory_link() {
        let fixture = Fixture::new();
        let developer = fixture.0.join("Xcode-beta.app/Contents/Developer");
        let git = fixture.program("Xcode-beta.app/Contents/Developer/usr/bin/git", true);
        let absolute = fixture.0.join("absolute_link");
        symlink(&developer, &absolute).unwrap();
        let relative = fixture.0.join("relative_link");
        symlink("Xcode-beta.app/Contents/Developer", &relative).unwrap();

        for link in [absolute, relative] {
            let discovery = MacosRepositoryToolDiscovery::with_candidates(
                vec![
                    ToolCandidate::DeveloperDirectory {
                        link: fixture.0.join("missing_link"),
                    },
                    ToolCandidate::DeveloperDirectory { link },
                ],
                Vec::new(),
            );
            assert_eq!(discovery.discover().git, Some(git.clone()));
        }
    }

    #[test]
    fn macos_repository_tools_never_select_the_system_git_stub() {
        let fixture = Fixture::new();
        let root_link = fixture.0.join("root_link");
        symlink("/", &root_link).unwrap();
        let discovery = MacosRepositoryToolDiscovery::with_candidates(
            vec![
                ToolCandidate::Fixed(PathBuf::from(SYSTEM_STUB)),
                ToolCandidate::DeveloperDirectory { link: root_link },
            ],
            Vec::new(),
        );

        assert_eq!(discovery.discover(), ToolInventory::default());
    }

    #[test]
    fn macos_repository_tools_default_candidates_exclude_the_system_stub() {
        let discovery = MacosRepositoryToolDiscovery::new();

        assert!(
            !discovery
                .git
                .contains(&ToolCandidate::Fixed(PathBuf::from(SYSTEM_STUB)))
        );
        assert_ne!(discovery.discover().git, Some(PathBuf::from(SYSTEM_STUB)));
    }
}
