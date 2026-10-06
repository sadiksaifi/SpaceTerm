//! Release identity rules in real, isolated Git repositories.

#[path = "../build/identity.rs"]
mod identity;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{self, Command},
    sync::atomic::{AtomicUsize, Ordering},
};

use identity::{Identity, resolve, stable_version};

struct Repository(PathBuf);

impl Repository {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "spaceterm-build-identity-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let repository = Self(path);
        repository.git(&["init", "-q"]);
        repository.git(&["commit", "-qm", "Initial", "--allow-empty"]);
        repository
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(["-c", "tag.gpgSign=false", "-c", "commit.gpgSign=false"])
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {arguments:?} failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn annotated_tag(&self, tag: &str) {
        self.git(&["tag", "-a", tag, "-m", "Release"]);
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn release_uses_an_annotated_tag_on_head() {
    let repository = Repository::new();
    repository.annotated_tag("v0.1.0");
    assert_eq!(
        resolve(repository.path(), Some("v0.1.0")),
        Ok(Identity {
            version: "0.1.0".to_owned(),
            bundle_version: "0.1.0".to_owned(),
        })
    );
}

#[test]
fn release_rejects_lightweight_tags_and_tags_on_other_commits() {
    let repository = Repository::new();
    repository.git(&["tag", "v0.1.0"]);
    assert!(resolve(repository.path(), Some("v0.1.0")).is_err());
    repository.annotated_tag("v0.2.0");
    repository.git(&["commit", "-qm", "Next", "--allow-empty"]);
    assert!(resolve(repository.path(), Some("v0.2.0")).is_err());
    assert!(resolve(repository.path(), Some("v0.3.0")).is_err());
}

#[test]
fn release_rejects_a_dirty_checkout() {
    let repository = Repository::new();
    repository.annotated_tag("v0.1.0");
    fs::write(repository.path().join("changed"), "dirty").unwrap();
    assert!(resolve(repository.path(), Some("v0.1.0")).is_err());
}

#[test]
fn release_tags_are_canonical_stable_versions() {
    assert_eq!(stable_version("v0.1.0"), Some("0.1.0"));
    assert_eq!(stable_version("v10.20.30"), Some("10.20.30"));
    for tag in [
        "0.1.0",
        "v0.1",
        "v0.1.0.0",
        "v01.1.0",
        "v0.1.0-beta.1",
        "v0.1.0+build",
        "v0..0",
        "v+1.0.0",
    ] {
        assert_eq!(stable_version(tag), None, "{tag}");
    }
}

#[test]
fn development_build_is_never_a_release() {
    let repository = Repository::new();
    repository.annotated_tag("v0.1.0");
    let commit = repository.git(&["rev-parse", "HEAD"]);
    let clean = resolve(repository.path(), None).unwrap();
    assert_eq!(clean.version, format!("dev.{}", &commit[..12]));
    assert_eq!(clean.bundle_version, "0.0.0");
    fs::write(repository.path().join("changed"), "dirty").unwrap();
    let dirty = resolve(repository.path(), None).unwrap();
    assert_eq!(dirty.version, format!("dev.{}.dirty", &commit[..12]));
}

#[test]
fn watched_paths_exist_in_a_linked_worktree() {
    let repository = Repository::new();
    let worktree = repository.path().join("linked");
    repository.git(&["worktree", "add", "-q", worktree.to_str().unwrap()]);
    repository.git(&["pack-refs", "--all"]);
    let paths = identity::watched_paths(&worktree).unwrap();
    assert_eq!(paths.len(), 4);
    assert!(paths.iter().all(|path| path.exists()), "{paths:?}");
    let index = PathBuf::from(repository.git(&[
        "-C",
        worktree.to_str().unwrap(),
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "index",
    ]));
    assert!(paths.contains(&index), "{paths:?}");

    let git_file = fs::read_to_string(worktree.join(".git")).unwrap();
    let git_directory = PathBuf::from(git_file.trim().strip_prefix("gitdir: ").unwrap());
    let common_directory = repository.path().join(".git").canonicalize().unwrap();
    assert_eq!(
        paths.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            git_directory.join("HEAD"),
            common_directory.join("refs"),
            index,
            common_directory.join("packed-refs"),
        ])
    );
}
