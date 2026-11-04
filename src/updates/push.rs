//! Pushing this desktop's embedded SpaceTerm Core build to a Machine. The desktop embeds one
//! signed Core archive per platform in its Core artifact catalog, the same bytes Add Machine
//! uploads over SSH. It verifies each once, offers it to every Machine whose Core would install
//! it, and streams it over that Core's service lane. The Core verifies it again before it
//! installs it beside the running version, which ends nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use spaceterm_core::service::lane::admin::{self, PushArchive};
use spaceterm_core::service::lane::client::{LaneError, Session};
use spaceterm_core::update::facts::{Platform, UpdateFacts, Version};
use spaceterm_core::update::ordering;

use super::machines::ActionFailure;

/// The largest embedded archive, in bytes, as the Core accepts.
const MAX_ARCHIVE: u64 = spaceterm_core::update::feed::MAX_ARCHIVE;

/// One embedded Core build, verified for this desktop's identity and its platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmbeddedCore {
    pub(crate) version: Version,
    archive: PathBuf,
    signature: Vec<u8>,
    /// 64 lowercase hex digits.
    sha256: String,
    length: u64,
}

/// The embedded builds of the catalog at `root`, verified on first use.
pub(crate) struct EmbeddedCores {
    root: Option<PathBuf>,
    identity: String,
    verified: Mutex<HashMap<Platform, Option<Arc<EmbeddedCore>>>>,
}

/// The Rust target a platform's Core archive is built for, which names its catalog directory.
fn target(platform: Platform) -> &'static str {
    match platform {
        Platform::MacosArm64 => "aarch64-apple-darwin",
        Platform::LinuxX86_64 => "x86_64-unknown-linux-gnu",
    }
}

impl EmbeddedCores {
    /// The catalog at `root`, as host composition finds it, for the build identity `identity`.
    pub(crate) fn new(root: Option<PathBuf>, identity: &str) -> Self {
        Self {
            root,
            identity: identity.to_owned(),
            verified: Mutex::default(),
        }
    }

    /// The embedded build for `platform`. None when the catalog has none or it does not verify.
    pub(crate) fn build(&self, platform: Platform) -> Option<Arc<EmbeddedCore>> {
        let mut verified = self.verified.lock().unwrap_or_else(|e| e.into_inner());
        verified
            .entry(platform)
            .or_insert_with(|| {
                self.root
                    .as_deref()
                    .and_then(|root| verify(&root.join(target(platform)), &self.identity, platform))
                    .map(Arc::new)
            })
            .clone()
    }

    /// The version "Update from This Mac" offers a Machine with `facts`: the embedded build for
    /// its platform, when its Core would install it. It is newer than every version the Core has
    /// and is not the skipped one.
    pub(crate) fn offer(&self, facts: &UpdateFacts) -> Option<Version> {
        let build = self.build(facts.platform)?;
        let mut installed = vec![facts.running.clone()];
        installed.extend(
            facts
                .pending
                .as_ref()
                .map(|pending| pending.version.clone()),
        );
        ordering::admit(&build.version, &installed, facts.skipped.as_ref())
            .ok()
            .map(|()| build.version.clone())
    }
}

/// Reads and verifies one catalog entry: `artifact.tar.gz`, its raw `artifact.sig` and its
/// `artifact.sha256`.
fn verify(directory: &Path, identity: &str, platform: Platform) -> Option<EmbeddedCore> {
    let archive = directory.join("artifact.tar.gz");
    let length = std::fs::metadata(&archive).ok()?.len();
    if !(1..=MAX_ARCHIVE).contains(&length) {
        return None;
    }
    let bytes = std::fs::read(&archive).ok()?;
    let signature = std::fs::read(directory.join("artifact.sig")).ok()?;
    let sha256 = std::fs::read_to_string(directory.join("artifact.sha256"))
        .ok()?
        .trim()
        .to_owned();
    let metadata = spaceterm_core::service::verify_candidate_bytes(
        identity,
        &bytes,
        &signature,
        &sha256,
        target(platform),
        spaceterm_core::protocol::MAJOR,
    )
    .ok()?;
    Some(EmbeddedCore {
        version: Version::parse(&metadata.version).ok()?,
        archive,
        signature,
        sha256,
        length: bytes.len() as u64,
    })
}

/// Pushes `build` over `session` and returns the Core's facts after the install. `progress`
/// sees the bytes the Core acknowledged and the archive's length.
pub(crate) fn push(
    session: &mut Session,
    build: &EmbeddedCore,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<UpdateFacts, ActionFailure> {
    let mut file = std::fs::File::open(&build.archive).map_err(|_| ActionFailure::Failed)?;
    let length = build.length;
    admin::push(
        session,
        &PushArchive {
            version: &build.version,
            length,
            sha256: &build.sha256,
            signature: &build.signature,
        },
        &mut file,
        &mut |received| progress(received, length),
    )
    .map_err(|error| failure(&error))
}

/// The action failure of a lane error.
pub(crate) fn failure(error: &LaneError) -> ActionFailure {
    match error {
        LaneError::Code(code) => match code.as_str() {
            "service_busy" => ActionFailure::Busy,
            "changed" | "not_newer" | "version_skipped" => ActionFailure::Changed,
            "unreachable" => ActionFailure::Unreachable,
            _ => ActionFailure::Failed,
        },
        LaneError::Core(spaceterm_core::CoreError::Direct(
            spaceterm_core::direct::DirectError::Transport,
        )) => ActionFailure::Unreachable,
        LaneError::Core(_) | LaneError::Interrupted => ActionFailure::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::update::facts::{PendingVersion, Protocols, UpdateCheck};

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn facts(running: &str, pending: Option<&str>, skipped: Option<&str>) -> UpdateFacts {
        UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: version(running),
            pending: pending.map(|pending| PendingVersion {
                version: version(pending),
                protocols: Protocols::CURRENT,
                breaking: false,
                installed_at: 1,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            sessions: 0,
            operation: None,
            check: UpdateCheck::Never,
            skipped: skipped.map(version),
            last_failure: None,
        }
    }

    fn catalog(embedded: &str) -> EmbeddedCores {
        let cores = EmbeddedCores::new(None, "spaceterm-development");
        cores.verified.lock().unwrap().insert(
            Platform::LinuxX86_64,
            Some(Arc::new(EmbeddedCore {
                version: version(embedded),
                archive: PathBuf::from("/nonexistent"),
                signature: vec![0; 64],
                sha256: "ab".repeat(32),
                length: 1,
            })),
        );
        cores
    }

    #[test]
    fn the_embedded_build_is_offered_only_to_a_core_that_would_install_it() {
        let cores = catalog("0.9.0");
        assert_eq!(
            cores.offer(&facts("0.8.0", None, None)),
            Some(version("0.9.0"))
        );
        assert_eq!(cores.offer(&facts("0.9.0", None, None)), None);
        assert_eq!(cores.offer(&facts("1.0.0", None, None)), None);
        assert_eq!(cores.offer(&facts("0.8.0", Some("0.9.0"), None)), None);
        assert_eq!(cores.offer(&facts("0.8.0", Some("0.9.1"), None)), None);
        assert_eq!(cores.offer(&facts("0.8.0", None, Some("0.9.0"))), None);
        let mut other = facts("0.8.0", None, None);
        other.platform = Platform::MacosArm64;
        assert_eq!(cores.offer(&other), None, "no build for that platform");
    }

    #[test]
    fn a_catalog_without_a_root_or_a_valid_entry_offers_nothing() {
        let cores = EmbeddedCores::new(None, "spaceterm-development");
        assert_eq!(cores.build(Platform::LinuxX86_64), None);
        let root = std::env::temp_dir().join(format!("spaceterm-push-{}", std::process::id()));
        let entry = root.join(target(Platform::LinuxX86_64));
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("artifact.tar.gz"), b"not an archive").unwrap();
        std::fs::write(entry.join("artifact.sig"), [0; 64]).unwrap();
        std::fs::write(entry.join("artifact.sha256"), "ab".repeat(32)).unwrap();
        let cores = EmbeddedCores::new(Some(root.clone()), "spaceterm-development");
        assert_eq!(cores.build(Platform::LinuxX86_64), None);
        assert_eq!(cores.build(Platform::MacosArm64), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lane_failures_map_to_action_failures() {
        for (code, failure) in [
            ("service_busy", ActionFailure::Busy),
            ("not_newer", ActionFailure::Changed),
            ("version_skipped", ActionFailure::Changed),
            ("changed", ActionFailure::Changed),
            ("unreachable", ActionFailure::Unreachable),
            ("candidate_unverified", ActionFailure::Failed),
            ("disk_full", ActionFailure::Failed),
        ] {
            assert_eq!(
                super::failure(&LaneError::Code(code.into())),
                failure,
                "{code}"
            );
        }
        assert_eq!(
            super::failure(&LaneError::Core(spaceterm_core::CoreError::Direct(
                spaceterm_core::direct::DirectError::Transport
            ))),
            ActionFailure::Unreachable
        );
    }
}
