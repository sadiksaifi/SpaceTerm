//! Embedding the release pipeline's notes file into a desktop build.

#[path = "../build/release_notes.rs"]
mod release_notes;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Command},
    sync::atomic::{AtomicUsize, Ordering},
};

use release_notes::{EMBEDDED, SOURCE, embed, release_date};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "spaceterm-build-release-notes-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn embedded(&self) -> String {
        fs::read_to_string(self.0.join(EMBEDDED)).unwrap()
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const SAMPLE: &str = "\n### Features\n\n- **panes:** Let panes swap places ([087ed1d](https://github.com/sadiksaifi/SpaceTerm/commit/087ed1d2))\n\n";

#[test]
fn the_release_pipeline_names_the_notes_file_with_this_variable() {
    assert_eq!(SOURCE, "SPACETERM_RELEASE_NOTES");
}

#[test]
fn a_notes_file_is_embedded_trimmed() {
    let scratch = Scratch::new();
    let source = scratch.file("notes.md", SAMPLE.as_bytes());
    assert_eq!(embed(Some(&source), scratch.path()), Ok(true));
    assert_eq!(
        scratch.embedded(),
        "### Features\n\n- **panes:** Let panes swap places ([087ed1d](https://github.com/sadiksaifi/SpaceTerm/commit/087ed1d2))"
    );
}

#[test]
fn a_build_without_a_notes_file_embeds_nothing() {
    let scratch = Scratch::new();
    scratch.file(EMBEDDED, b"left by an earlier build");
    assert_eq!(embed(None, scratch.path()), Ok(false));
    assert_eq!(scratch.embedded(), "");
}

#[test]
fn a_named_notes_file_must_hold_bounded_utf8_notes() {
    let scratch = Scratch::new();
    let missing = scratch.path().join("missing.md");
    let blank = scratch.file("blank.md", b" \n\n ");
    let binary = scratch.file("binary.md", b"\xff\xfe");
    let large = scratch.file("large.md", &vec![b'a'; 64 * 1024 + 1]);
    for source in [&missing, &blank, &binary, &large] {
        assert!(embed(Some(source), scratch.path()).is_err(), "{source:?}");
    }
    let limit = scratch.file("limit.md", &vec![b'a'; 64 * 1024]);
    assert_eq!(embed(Some(&limit), scratch.path()), Ok(true));
}

/// Runs Git in `root` with `date` as the committer and tagger date.
fn git_at(root: &Path, date: &str, arguments: &[&str]) {
    let status = Command::new("git")
        .env("GIT_COMMITTER_DATE", date)
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(["-c", "tag.gpgSign=false", "-c", "commit.gpgSign=false"])
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
}

#[test]
fn release_date_is_the_tag_date_or_the_commit_date() {
    let scratch = Scratch::new();
    let root = scratch.path();
    git_at(root, "2026-10-30T12:00:00+0000", &["init", "-q"]);
    git_at(
        root,
        "2026-10-30T12:00:00+0000",
        &["commit", "-qm", "Release", "--allow-empty"],
    );
    git_at(
        root,
        "2026-11-03T12:00:00+0000",
        &["tag", "-a", "v0.9.0", "-m", "Release"],
    );
    assert_eq!(
        release_date(root, Some("v0.9.0")).as_deref(),
        Ok("2026-11-03")
    );
    assert_eq!(release_date(root, None).as_deref(), Ok("2026-10-30"));
    assert!(release_date(root, Some("v1.0.0")).is_err());
}
