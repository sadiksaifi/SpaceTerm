//! The release notes the release pipeline hands to a desktop build.
//!
//! build.rs and tests/build_release_notes.rs include this file, so it uses only the standard
//! library.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Names the git-cliff notes file for this build. Unset or empty, the build carries no notes.
pub const SOURCE: &str = "SPACETERM_RELEASE_NOTES";
/// The file in `OUT_DIR` that the desktop includes. It is empty when the build carries no notes.
pub const EMBEDDED: &str = "release-notes.md";
const MAX_BYTES: u64 = 64 * 1024;

/// Runs in a build script: embeds the notes file [`SOURCE`] names into `OUT_DIR`, tells Cargo to
/// embed them again when the name or the file changes, and sets `cfg(spaceterm_release_notes)` and
/// `SPACETERM_RELEASE_NOTES_DATE`. A desktop build for `release_tag` must carry notes.
pub fn emit(root: &Path, release_tag: Option<&str>) -> Result<(), &'static str> {
    println!("cargo:rustc-check-cfg=cfg(spaceterm_release_notes)");
    println!("cargo:rerun-if-env-changed={SOURCE}");
    let source = env::var_os(SOURCE)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if let Some(path) = &source {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if source.is_none() && release_tag.is_some() && env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        return Err(
            "a desktop release build requires the release notes SPACETERM_RELEASE_NOTES names",
        );
    }
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo supplies OUT_DIR")?);
    let released = if embed(source.as_deref(), &out_dir)? {
        println!("cargo:rustc-cfg=spaceterm_release_notes");
        release_date(root, release_tag)?
    } else {
        String::new()
    };
    println!("cargo:rustc-env=SPACETERM_RELEASE_NOTES_DATE={released}");
    Ok(())
}

/// Writes the notes the desktop embeds into `out_dir`: the trimmed contents of `source`, or
/// nothing without a source. Returns whether the build carries notes.
pub fn embed(source: Option<&Path>, out_dir: &Path) -> Result<bool, &'static str> {
    let notes = match source {
        Some(path) => read(path)?,
        None => String::new(),
    };
    fs::write(out_dir.join(EMBEDDED), &notes)
        .map_err(|_| "the embedded release notes could not be written")?;
    Ok(!notes.is_empty())
}

fn read(path: &Path) -> Result<String, &'static str> {
    let metadata = fs::metadata(path).map_err(|_| "the release notes file is unavailable")?;
    if !metadata.is_file() {
        return Err("the release notes source is not a file");
    }
    if metadata.len() > MAX_BYTES {
        return Err("the release notes file is larger than 64 KiB");
    }
    let bytes = fs::read(path).map_err(|_| "the release notes file is unavailable")?;
    let text = String::from_utf8(bytes).map_err(|_| "the release notes file is not UTF-8")?;
    let notes = text.trim();
    if notes.is_empty() {
        return Err("the release notes file is empty");
    }
    Ok(notes.to_owned())
}

/// The day this build was released as `YYYY-MM-DD`: the release tag's date, or the commit date
/// of a development build.
pub fn release_date(root: &Path, release_tag: Option<&str>) -> Result<String, &'static str> {
    let reference;
    let arguments: &[&str] = match release_tag {
        Some(tag) => {
            reference = format!("refs/tags/{tag}");
            &["for-each-ref", "--format=%(taggerdate:short)", &reference]
        }
        None => &["show", "-s", "--format=%cs", "HEAD"],
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("--no-optional-locks")
        .args(arguments)
        .output()
        .map_err(|_| "Git is required to resolve the release date")?;
    let date = String::from_utf8(output.stdout).unwrap_or_default();
    let date = date.trim();
    if !output.status.success() || date.is_empty() {
        return Err("the release date is unavailable");
    }
    Ok(date.to_owned())
}
