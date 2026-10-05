//! Application build identity from Git; see ADR 0012.
//!
//! build.rs and tests/build_identity.rs include this file, so it uses only the standard library.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, PartialEq, Eq)]
pub struct Identity {
    /// The full version: the release version, or `dev.<commit>[.dirty]`.
    pub version: String,
    /// The bundle version: the release version, or `0.0.0`.
    pub bundle_version: String,
}

fn git(root: &Path, arguments: &[&str]) -> Result<String, &'static str> {
    // Optional locks would let a status query rewrite the index during a build.
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("--no-optional-locks")
        .args(arguments)
        .output()
        .map_err(|_| "Git is required to resolve the build identity")?;
    if !output.status.success() {
        return Err("the Git build identity is unavailable");
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|_| "Git reported a non-UTF-8 build identity")
}

/// Returns the version of a `v<major>.<minor>.<patch>` tag in canonical stable SemVer.
pub fn stable_version(tag: &str) -> Option<&str> {
    let version = tag.strip_prefix('v')?;
    let canonical = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let parts: Vec<&str> = version.split('.').collect();
    (parts.len() == 3 && parts.iter().all(|part| canonical(part))).then_some(version)
}

/// Resolves the identity of `root`'s checkout. Only an annotated stable tag on a clean HEAD
/// produces a release identity.
pub fn resolve(root: &Path, release_tag: Option<&str>) -> Result<Identity, &'static str> {
    let commit = git(root, &["rev-parse", "HEAD"])?;
    let dirty = !git(root, &["status", "--porcelain", "--untracked-files=normal"])?.is_empty();
    let Some(tag) = release_tag else {
        let suffix = if dirty { ".dirty" } else { "" };
        return Ok(Identity {
            version: format!("dev.{}{suffix}", &commit[..12]),
            bundle_version: "0.0.0".to_owned(),
        });
    };
    let version =
        stable_version(tag).ok_or("a release tag is v followed by a canonical stable SemVer")?;
    let reference = format!("refs/tags/{tag}");
    if git(root, &["cat-file", "-t", &reference])? != "tag" {
        return Err("a release tag must be annotated");
    }
    if git(root, &["rev-parse", &format!("{reference}^{{commit}}")])? != commit {
        return Err("a release tag must identify HEAD");
    }
    if dirty {
        return Err("a release build requires a clean checkout");
    }
    Ok(Identity {
        version: version.to_owned(),
        bundle_version: version.to_owned(),
    })
}

/// The Git files whose changes can change the identity, including in a linked worktree.
pub fn watched_paths(root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    let git_directory = PathBuf::from(git(root, &["rev-parse", "--absolute-git-dir"])?);
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let mut paths = vec![git_directory.join("HEAD"), common.join("refs")];
    // The index changes when a change is staged, which can make the checkout dirty without
    // touching a source file Cargo watches. A linked worktree has its own index.
    // Cargo reruns a build script on every build while a watched path is missing.
    paths.extend(
        [git_directory.join("index"), common.join("packed-refs")]
            .into_iter()
            .filter(|path| path.exists()),
    );
    Ok(paths)
}
