//! Embedding the release pipeline's notes file into a desktop build.

#[allow(dead_code, reason = "build.rs and the Cargo fixture below call emit")]
#[path = "../build/release_notes.rs"]
mod release_notes;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime},
};

use release_notes::{EMBEDDED, embed, release_date};

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

/// A small crate whose build script runs [`release_notes::emit`] as build.rs does, built by the
/// real Cargo into one reused target, so rebuild tracking is Cargo's own.
struct Fixture(Scratch);

impl Fixture {
    fn new() -> Self {
        let scratch = Scratch::new();
        let module = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/release_notes.rs");
        scratch.file(
            "Cargo.toml",
            b"[package]\nname = \"notes-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[features]\ndesktop = []\n\n[workspace]\n",
        );
        scratch.file(
            "build.rs",
            format!(
                "#[path = {module:?}]\nmod release_notes;\n\nfn main() {{\n    println!(\"cargo:rerun-if-env-changed=SPACETERM_RELEASE_TAG\");\n    let tag = std::env::var(\"SPACETERM_RELEASE_TAG\").ok();\n    let root = std::path::PathBuf::from(std::env::var_os(\"CARGO_MANIFEST_DIR\").unwrap());\n    release_notes::emit(&root, tag.as_deref()).unwrap_or_else(|error| panic!(\"{{error}}\"));\n}}\n"
            )
            .as_bytes(),
        );
        fs::create_dir(scratch.path().join("src")).unwrap();
        scratch.file(
            "src/main.rs",
            b"fn main() {\n    print!(\n        \"{}|{}\",\n        cfg!(spaceterm_release_notes),\n        include_str!(concat!(env!(\"OUT_DIR\"), \"/release-notes.md\"))\n    );\n}\n",
        );
        let root = scratch.path();
        git_at(root, "2026-11-03T12:00:00+0000", &["init", "-q"]);
        git_at(root, "2026-11-03T12:00:00+0000", &["add", "."]);
        git_at(
            root,
            "2026-11-03T12:00:00+0000",
            &["commit", "-qm", "Fixture"],
        );
        Self(scratch)
    }

    /// Builds and runs the fixture with `notes` as the notes variable, if any.
    fn run(&self, notes: Option<&Path>, tag: Option<&str>, desktop: bool) -> Output {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .arg("run")
            .arg("-q")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(self.0.path().join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", self.0.path().join("target"))
            .env_remove("SPACETERM_RELEASE_NOTES")
            .env_remove("SPACETERM_RELEASE_TAG");
        if desktop {
            command.args(["--features", "desktop"]);
        }
        if let Some(notes) = notes {
            command.env("SPACETERM_RELEASE_NOTES", notes);
        }
        if let Some(tag) = tag {
            command.env("SPACETERM_RELEASE_TAG", tag);
        }
        command.output().unwrap()
    }

    /// What the built fixture embedded: whether it carries notes, and the notes.
    fn embedded(&self, notes: Option<&Path>) -> String {
        let output = self.run(notes, None, true);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Writes notes last modified at `modified`. A time before every build leaves only the
    /// variable to tell Cargo about a new file; a later one marks an edit whatever the file
    /// system's time resolution.
    fn notes(&self, name: &str, contents: &str, modified: SystemTime) -> PathBuf {
        let path = self.0.file(name, contents.as_bytes());
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    }
}

fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3600)
}

#[test]
fn cargo_embeds_the_notes_again_when_the_named_file_or_its_contents_change() {
    let fixture = Fixture::new();
    let alpha = fixture.notes("alpha.md", "Alpha", long_ago());
    assert_eq!(fixture.embedded(Some(&alpha)), "true|Alpha");
    let beta = fixture.notes("beta.md", "Beta", long_ago());
    assert_eq!(
        fixture.embedded(Some(&beta)),
        "true|Beta",
        "the variable changed"
    );
    let beta = fixture.notes(
        "beta.md",
        "Beta, edited",
        SystemTime::now() + Duration::from_secs(2),
    );
    assert_eq!(
        fixture.embedded(Some(&beta)),
        "true|Beta, edited",
        "the named file changed"
    );
    assert_eq!(fixture.embedded(None), "false|", "the variable is gone");
}

#[test]
fn a_desktop_release_build_fails_without_notes() {
    let fixture = Fixture::new();
    git_at(
        fixture.0.path(),
        "2026-11-03T12:00:00+0000",
        &["tag", "-a", "v1.0.0", "-m", "Release"],
    );
    let missing = fixture.run(None, Some("v1.0.0"), true);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("a desktop release build requires the release notes"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let headless = fixture.run(None, Some("v1.0.0"), false);
    assert!(
        headless.status.success(),
        "a headless build carries no notes"
    );
    let notes = fixture.notes("notes.md", "Notes", long_ago());
    let release = fixture.run(Some(&notes), Some("v1.0.0"), true);
    assert_eq!(String::from_utf8_lossy(&release.stdout), "true|Notes");
}
