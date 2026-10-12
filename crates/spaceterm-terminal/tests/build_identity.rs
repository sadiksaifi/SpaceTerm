//! Cargo identity invalidation in a real, isolated Git checkout.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

struct Checkout(PathBuf);

impl Checkout {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-terminal-build-identity-{}",
            process::id()
        ));
        fs::create_dir(&root).unwrap();
        let checkout = Self(root);
        fs::create_dir_all(checkout.path().join("src")).unwrap();
        fs::create_dir_all(checkout.path().join("build")).unwrap();
        fs::create_dir_all(checkout.path().join("crates/spaceterm-terminal/src")).unwrap();
        checkout.write(".gitignore", "/target/\n/Cargo.lock\n");
        checkout.write(
            "Cargo.toml",
            r#"[package]
name = "identity-regression"
version = "0.1.0"
edition = "2024"

[workspace]
members = ["crates/spaceterm-terminal"]

[dependencies]
spaceterm-terminal = { path = "crates/spaceterm-terminal" }
"#,
        );
        checkout.write("build.rs", include_str!("../../../build.rs"));
        checkout.write(
            "build/identity.rs",
            include_str!("../../../build/identity.rs"),
        );
        checkout.write(
            "src/main.rs",
            r#"fn main() {
    println!("{}", env!("SPACETERM_VERSION"));
    println!("{}", spaceterm_terminal::identity::PROGRAM_VERSION);
    println!("{}", spaceterm_terminal::identity::XTVERSION);
}
"#,
        );
        checkout.write(
            "crates/spaceterm-terminal/Cargo.toml",
            r#"[package]
name = "spaceterm-terminal"
version = "0.1.0"
edition = "2024"

[features]
engine = []
"#,
        );
        checkout.write(
            "crates/spaceterm-terminal/build.rs",
            include_str!("../build.rs"),
        );
        checkout.write(
            "crates/spaceterm-terminal/src/lib.rs",
            "pub mod identity;\n",
        );
        checkout.write(
            "crates/spaceterm-terminal/src/identity.rs",
            include_str!("../src/identity.rs"),
        );
        checkout.git(&["init", "-q"]);
        checkout.git(&["add", "."]);
        checkout.git(&["commit", "-qm", "Identity fixture"]);
        checkout
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, path: &str, contents: &str) {
        fs::write(self.path().join(path), contents).unwrap();
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .arg("--no-optional-locks")
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

    fn versions(&self) -> Vec<String> {
        let output = Command::new(env!("CARGO"))
            .current_dir(self.path())
            .args(["run", "--offline", "--quiet"])
            .env("CARGO_TARGET_DIR", self.path().join("target"))
            .env_remove("SPACETERM_PACKAGED")
            .env_remove("SPACETERM_RELEASE_TAG")
            .env_remove("SPACETERM_SPARKLE_DIR")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "identity build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path());
    }
}

#[test]
fn terminal_identity_tracks_unstaged_root_build_script_changes() {
    let checkout = Checkout::new();
    let commit = checkout.git(&["rev-parse", "HEAD"]);
    let clean = format!("dev.{}", &commit[..12]);
    assert_eq!(
        checkout.versions(),
        [clean.clone(), clean.clone(), format!("SpaceTerm {clean}")]
    );

    // Only the root build script changes. Reuse the same Cargo command, features and target.
    checkout.write(
        "build.rs",
        &format!(
            "{}\n// Changed build input.\n",
            include_str!("../../../build.rs")
        ),
    );
    assert_eq!(checkout.git(&["status", "--porcelain"]), "M build.rs");
    let versions = checkout.versions();
    assert_eq!(versions[0], format!("{clean}.dirty"));
    assert_eq!(
        versions[1], versions[0],
        "terminal identity retained a clean build"
    );
    assert_eq!(versions[2], format!("SpaceTerm {}", versions[0]));
}
