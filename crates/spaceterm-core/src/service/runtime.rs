//! Authenticate and retain independent server artifacts without selecting or starting a runtime.
use super::*;
use crate::update::ordering::{Candidate, Floor};
use base64::Engine;
use std::{
    collections::BTreeSet,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

mod files;
pub(super) mod launcher;
mod layout;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
pub mod updates;

pub use layout::{Layout, bundle_identifier, bundle_name};

const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;
/// Signed entries reach visitors in pieces of at most this many bytes.
const CHUNK: usize = 64 * 1024;
#[cfg(target_os = "macos")]
const TARGET: &str = "aarch64-apple-darwin";
#[cfg(target_os = "linux")]
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Authenticated signed declarations used before staging an update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateMetadata {
    pub version: String,
    pub commit: String,
    pub protocols: crate::update::facts::Protocols,
    /// The build's Update Floor, never newer than a Release `version`. None exactly for a
    /// Preflight or Development build.
    pub floor: Option<Floor>,
}

impl CandidateMetadata {
    /// The build as a source offers it to an update engine: its version and Update Floor.
    ///
    /// # Errors
    /// Returns RuntimeUnavailable for a version the build document cannot hold.
    pub fn candidate(&self) -> Result<Candidate, CoreError> {
        crate::update::facts::Version::parse(&self.version)
            .ok()
            .and_then(|version| Candidate::new(version, self.floor).ok())
            .ok_or(CoreError::RuntimeUnavailable)
    }
}

/// Authenticate the exact captured archive and signature for one application and target.
/// This verifies the digest, archive contents and protocol major without I/O.
///
/// # Errors
/// Returns RuntimeUnavailable for invalid authentication, bounds, target or archive contents;
/// CandidateChanged for a different digest; IncompatibleProtocol when the signed protocol major
/// is not `required_major`.
pub fn verify_candidate_bytes(
    identity: &str,
    archive: &[u8],
    signature: &[u8],
    expected_sha256: &str,
    target: &str,
    required_major: u16,
) -> Result<CandidateMetadata, CoreError> {
    verified_candidate_build(
        identity,
        archive,
        signature,
        expected_sha256,
        target,
        Some(required_major),
    )
    .map(Into::into)
}

/// What the command-line launcher path holds. Only [`CliLauncher::Missing`] and SpaceTerm's
/// own links are ever replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliLauncher {
    Missing,
    /// A link into this identity's retained installations whose executable exists.
    Linked,
    /// A link into this identity's retained installations whose executable is gone.
    Stale,
    /// A link into another of this identity's installations than the approved runtime, such as
    /// one an update couldn't repoint.
    Outdated,
    /// A stale or outdated managed link that can't be replaced, because another tool's entry
    /// holds the temporary name at [`Service::cli_temporary_path`].
    Blocked,
    /// A link to anything else, such as another tool.
    Foreign,
    /// A file, directory or other entry that is not a link.
    NotLink,
}

/// The outcome of linking the command-line launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliLink {
    /// The launcher names the selected runtime.
    Linked,
    /// Another tool's launcher is there and stays.
    Kept,
    /// Another tool's entry holds the temporary name a replacement needs, and stays.
    Blocked,
}

/// What keeping an existing launcher on the approved runtime did. Never fatal to the service
/// operation that asked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Repoint {
    /// The managed launcher names the approved runtime.
    Followed,
    /// There is no launcher, or another tool owns it.
    Untouched,
    /// Another tool's entry holds the temporary name, so the launcher stays on its earlier
    /// installation.
    Blocked,
    /// The managed launcher stays on its earlier installation.
    Failed(CoreError),
}

impl Service {
    /// Retain an installer-selected signed package and select its CLI under the service lock.
    /// The unpacked installation must carry its original artifact.tar.gz and artifact.sig.
    /// This never registers or starts a service, or opens a credential store.
    pub fn retain_cli(&self, installation: &Path) -> Result<bool, CoreError> {
        let archive = archive(&installation.join("artifact.tar.gz"))?;
        let signature = source(&installation.join("artifact.sig"), 64)?;
        authenticate(&archive, &signature, &self.identity)?;
        let info = contents(&archive, &self.identity, |_, _, _, _| Ok(()))?;
        info.require_compatibility(Some(crate::protocol::MAJOR))?;
        let retained = retain(
            &self.paths.data.join("installations"),
            &self.identity,
            &archive,
            &signature,
            &info,
        )?;
        let data = Directory::open(&self.paths.data, true)?;
        let _operation = data.lock("service.lock", false)?;
        match self.select_cli_locked(&data, || Ok(retained.directory))? {
            CliLink::Linked => Ok(true),
            CliLink::Kept => Ok(false),
            CliLink::Blocked => Err(CoreError::StorageUnavailable),
        }
    }

    /// Link the command-line launcher exactly as the installer does, for a client acting with
    /// this account's authority. The launcher follows the approved runtime; without one it
    /// follows the installation `provision` retains, which runs under the service lock.
    /// Another tool's launcher, or its entry at the temporary name, stays untouched. Another
    /// service operation that outlasts the operation grace returns ServiceBusy.
    pub fn link_cli(
        &self,
        provision: impl FnOnce() -> Result<PathBuf, CoreError>,
    ) -> Result<CliLink, CoreError> {
        let data = Directory::open(&self.paths.data, true)?;
        let _operation = match data.wait_lock("service.lock", OPERATION_GRACE, None)? {
            LockWait::Locked(lock) => lock,
            LockWait::Held | LockWait::Cancelled => return Err(CoreError::ServiceBusy),
        };
        self.select_cli_locked(&data, provision)
    }

    /// Read what the command-line launcher path holds, without following it or taking a lock.
    /// Unreadable service policy makes no launcher Outdated.
    pub fn cli_launcher(&self) -> Result<CliLauncher, CoreError> {
        let approved = self
            .read()
            .ok()
            .flatten()
            .map(|(_, state)| self.runtime(&state));
        launcher::inspect(
            self.cli_home()?,
            &self.identity,
            &self.paths.data.join("installations"),
            approved.as_deref(),
        )
    }

    /// Where the installer and [`Self::link_cli`] place this identity's launcher.
    pub fn cli_launcher_path(&self) -> Result<PathBuf, CoreError> {
        Ok(self.cli_home()?.join(".local/bin").join(&self.identity))
    }

    /// The temporary name a replacement links from, which [`CliLauncher::Blocked`] names.
    pub fn cli_temporary_path(&self) -> Result<PathBuf, CoreError> {
        Ok(launcher::temporary(
            &self.cli_home()?.join(".local/bin"),
            &self.identity,
        ))
    }

    fn select_cli_locked(
        &self,
        data: &Directory,
        provision: impl FnOnce() -> Result<PathBuf, CoreError>,
    ) -> Result<CliLink, CoreError> {
        let runtime = match Self::state(data)? {
            Some(state) => self.runtime(&state),
            None => provision()?,
        };
        verify(&runtime, &self.identity)?;
        launcher::select(
            self.cli_home()?,
            &self.identity,
            &self.paths.data.join("installations"),
            &runtime,
        )
    }

    pub(super) fn cli_home(&self) -> Result<&Path, CoreError> {
        let suffix = Path::new(".local/share").join(&self.identity).join("core");
        if !self.paths.data.ends_with(suffix) {
            return Err(CoreError::UnsafeStorage);
        }
        self.paths
            .data
            .ancestors()
            .nth(4)
            .ok_or(CoreError::UnsafeStorage)
    }

    /// Keep an existing managed launcher on the policy's approved runtime. The service lock
    /// must be held. Services do not create a new PATH launcher as a side effect.
    ///
    /// The launcher never blocks a service operation: a failure leaves it on its earlier
    /// installation, where [`Self::cli_launcher`] reads it as Outdated or Stale and a client
    /// offers to link it again.
    pub(super) fn repoint_cli_locked(&self, runtime: &Path) -> Repoint {
        let Ok(home) = self.cli_home() else {
            return Repoint::Failed(CoreError::UnsafeStorage);
        };
        if !home.join(".local/bin").join(&self.identity).is_symlink() {
            return Repoint::Untouched;
        }
        match launcher::select(
            home,
            &self.identity,
            &self.paths.data.join("installations"),
            runtime,
        ) {
            Ok(CliLink::Linked) => Repoint::Followed,
            Ok(CliLink::Kept) => Repoint::Untouched,
            Ok(CliLink::Blocked) => Repoint::Blocked,
            Err(error) => Repoint::Failed(error),
        }
    }

    /// Authenticate an update for this native target without comparing protocol majors.
    /// Activation still verifies that the native executable reports its own signed manifest.
    pub fn inspect_update_candidate(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
    ) -> Result<CandidateMetadata, CoreError> {
        let (_, _, info) = update_candidate(&self.identity, archive, signature, expected_sha256)?;
        Ok(info.into())
    }

    /// Authenticate a signed remote-target candidate without executing or extracting it.
    pub fn inspect_candidate_for_target(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
        target: &str,
        required_major: u16,
    ) -> Result<CandidateMetadata, CoreError> {
        let (_, _, info) = candidate_for_target(
            &self.identity,
            archive,
            signature,
            expected_sha256,
            target,
            Some(required_major),
        )?;
        Ok(info.into())
    }
    /// Retain this exact signed independent artifact in the owning account's installation store.
    ///
    /// Identity comes from this Service. Target must be the supported native target; commit and
    /// version must exactly match the signed build document. No executable is selected from PATH
    /// or beside the desktop. Source builds must supply an approved signed artifact explicitly.
    ///
    /// This operation only provisions immutable files. It never writes service policy, server
    /// identity or organization, starts a service or Terminal Session, clears Stop, or opens a signing credential.
    /// The caller must reuse existing service policy before requesting first-use provisioning.
    /// Concurrent requests share the installation directory lock and reuse only a verified match.
    ///
    /// # Errors
    /// Returns RuntimeUnavailable for an invalid signature, layout or exact build; UnsafeStorage
    /// or StorageUnavailable for unsafe or inaccessible account storage. Existing entries are
    /// never repaired or replaced. Installation/startup remains a separate Service operation.
    pub fn provision(
        &self,
        archive: &Path,
        signature: &Path,
        target: &str,
        commit: &str,
        version: &str,
    ) -> Result<RetainedInstallation, CoreError> {
        provision(
            &self.paths.data.join("installations"),
            &self.identity,
            archive,
            signature,
            target,
            commit,
            version,
        )
    }

    /// Read and authenticate a pinned candidate without creating any installation-store entry.
    /// Native executable build verification remains part of explicit provisioning.
    ///
    /// # Errors
    /// Returns the same pin, signature, layout and compatibility classifications as provisioning.
    pub fn inspect_candidate(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
        required_major: u16,
    ) -> Result<CandidateMetadata, CoreError> {
        let (_, _, info) = candidate(
            &self.identity,
            archive,
            signature,
            expected_sha256,
            Some(required_major),
        )?;
        Ok(info.into())
    }

    /// Verify and retain the archive pinned by the running client, independently of its release.
    /// This changes no service policy, approved installation, Stop intent or running work.
    ///
    /// # Errors
    /// Returns CandidateChanged for a different whole-archive digest, IncompatibleProtocol for
    /// another protocol major, and the ordinary verification
    /// or storage classifications for an invalid artifact or inaccessible installation store.
    pub fn provision_candidate(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
        required_major: u16,
    ) -> Result<RetainedInstallation, CoreError> {
        provision_candidate(
            &self.paths.data.join("installations"),
            &self.identity,
            archive,
            signature,
            expected_sha256,
            Some(required_major),
        )
    }

    /// Verify and retain a candidate of any protocol major for remote administration, which
    /// reaches the server over the version-stable service lane.
    pub fn stage_candidate(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
    ) -> Result<RetainedInstallation, CoreError> {
        provision_candidate(
            &self.paths.data.join("installations"),
            &self.identity,
            archive,
            signature,
            expected_sha256,
            None,
        )
    }
}

pub(super) fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
/// The nonproduction key that Preflight and Development archives verify under.
fn public_key(identity: &str) -> Result<Vec<u8>, CoreError> {
    #[cfg(test)]
    if test_support::active() {
        return Ok(if identity == "spaceterm" {
            test_support::production_key()
        } else {
            test_support::key()
        });
    }
    let key = match identity {
        "spaceterm-preflight" | "spaceterm-development" => {
            #[derive(Deserialize)]
            struct NonproductionTrust {
                public_key: String,
            }
            // The seed in this file is deliberately public and has no Release authority.
            serde_json::from_str::<NonproductionTrust>(include_str!(
                "../../../../packaging/core-nonproduction-trust.json"
            ))
            .map_err(|_| CoreError::RuntimeUnavailable)?
            .public_key
        }
        _ => return Err(CoreError::RuntimeUnavailable),
    };
    base64::engine::general_purpose::STANDARD
        .decode(key.trim())
        .map_err(|_| CoreError::RuntimeUnavailable)
}
/// Verifies an archive's raw signature for `identity`. A Release archive verifies under the
/// current or the backup key of `packaging/update-trust.json`, and under nothing else.
/// Preflight and Development archives verify under the nonproduction key.
pub(super) fn authenticate(
    bytes: &[u8],
    signature: &[u8],
    identity: &str,
) -> Result<(), CoreError> {
    #[cfg(test)]
    let release = identity == "spaceterm" && !test_support::active();
    #[cfg(not(test))]
    let release = identity == "spaceterm";
    if release {
        return crate::update::trust::UpdateTrust::release()
            .map_err(|_| CoreError::RuntimeUnavailable)?
            .verify(bytes, signature)
            .map_err(|_| CoreError::RuntimeUnavailable);
    }
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key(identity)?)
        .verify(bytes, signature)
        .map_err(|_| CoreError::RuntimeUnavailable)
}
/// Recheck an installation's authenticated retained declarations. Any protocol major passes:
/// the build information must be self-consistent and native, and the service lane reaches it
/// across majors.
pub(super) fn verify(path: &Path, identity: &str) -> Result<(), CoreError> {
    let directory = owned_directory(path)?;
    let bytes = files::Snapshot::read(&directory, Path::new("artifact.tar.gz"), MAX_ARCHIVE)?;
    let signature = read_file(&directory, Path::new("artifact.sig"), 64)?;
    if path.file_name().and_then(|v| v.to_str()) != Some(&digest(&bytes)) {
        return Err(CoreError::RuntimeUnavailable);
    }
    authenticate(&bytes, &signature, identity)?;
    let info = verify_contents(&directory, &bytes, identity)?;
    verify_native(path, identity)?;
    verify_executable(path, &info)?;
    verify_directory_path(&directory, path)
}
fn verify_native(path: &Path, identity: &str) -> Result<(), CoreError> {
    #[cfg(test)]
    if test_support::active() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let layout = Layout::host(identity).ok_or(CoreError::RuntimeUnavailable)?;
        let requirement = native_requirement(identity, option_env!("SPACETERM_APPLE_TEAM_ID"))
            .ok_or(CoreError::RuntimeUnavailable)?;
        verify_signature(&path.join(layout.signed()), &requirement)?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (path, identity);
    Ok(())
}
/// The code requirement an installed version's bundle must satisfy. Release adds Apple's
/// Developer ID requirement for the release Team ID, which release packaging supplies at build
/// time as `SPACETERM_APPLE_TEAM_ID`, so every Release version from that team satisfies it. A
/// Release build without a valid Team ID admits no installed version.
#[cfg(target_os = "macos")]
fn native_requirement(identity: &str, team: Option<&str>) -> Option<String> {
    let identifier = format!("identifier \"{}\"", bundle_identifier(identity));
    if identity != "spaceterm" {
        return Some(identifier);
    }
    let team = team.filter(|team| {
        team.len() == 10
            && team
                .bytes()
                .all(|v| v.is_ascii_digit() || v.is_ascii_uppercase())
    })?;
    Some(format!(
        "{identifier} and anchor apple generic and certificate \
         1[field.1.2.840.113635.100.6.2.6] and certificate \
         leaf[field.1.2.840.113635.100.6.1.13] and certificate leaf[subject.OU] = \"{team}\""
    ))
}
#[cfg(target_os = "macos")]
fn verify_signature(signed: &Path, requirement: &str) -> Result<(), CoreError> {
    command(
        "/usr/bin/codesign",
        &[
            "--verify".into(),
            "--strict".into(),
            "-R".into(),
            format!("={requirement}").into(),
            signed.as_os_str().to_owned(),
        ],
    )
    .map(|_| ())
    .map_err(|_| CoreError::RuntimeUnavailable)
}
fn verify_executable(path: &Path, expected: &Build) -> Result<(), CoreError> {
    #[cfg(test)]
    let bytes = if test_support::active() {
        test_support::native_build(path, &expected.identity)
    } else {
        executable_build(path, &expected.identity)?
    };
    #[cfg(not(test))]
    let bytes = executable_build(path, &expected.identity)?;
    let reported = Build::parse(&bytes, &expected.identity)?;
    // The archive records stapling after compilation. All compiled facts still match exactly.
    let mut compiled = expected.clone();
    compiled.stapled = false;
    if reported != compiled {
        return Err(CoreError::RuntimeUnavailable);
    }
    Ok(())
}
fn executable_build(path: &Path, identity: &str) -> Result<Vec<u8>, CoreError> {
    let layout = Layout::host(identity).ok_or(CoreError::RuntimeUnavailable)?;
    let bytes = execute(
        Command::new(path.join(layout.executable()))
            .args(["internal", "build-info"])
            .env_clear()
            .current_dir(path),
    )
    .map_err(|_| CoreError::RuntimeUnavailable)?;
    if bytes.len() > 4096 {
        return Err(CoreError::RuntimeUnavailable);
    }
    Ok(bytes)
}

/// The verified build document of the installation at `path`.
pub(super) fn build(path: &Path, identity: &str) -> Result<CandidateMetadata, CoreError> {
    verify(path, identity)?;
    let directory = owned_directory(path)?;
    Build::parse(
        &read_file(&directory, Path::new("build.json"), 4096)?,
        identity,
    )
    .map(Into::into)
}

pub(super) fn describe(path: &Path, identity: &str) -> Result<(String, String), CoreError> {
    verify(path, identity)?;
    let directory = owned_directory(path)?;
    let info = Build::parse(
        &read_file(&directory, Path::new("build.json"), 4096)?,
        identity,
    )?;
    Ok((info.version, info.commit))
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Build {
    schema: u32,
    /// True only for a macOS Release bundle that acquired a ticket after compilation.
    stapled: bool,
    identity: String,
    target: String,
    commit: String,
    version: String,
    /// Never newer than a Release `version`. Null exactly for a Preflight or Development
    /// build, whose version names a commit. The key is required.
    #[serde(deserialize_with = "crate::update::ordering::present")]
    floor: Option<Floor>,
    protocols: crate::update::facts::Protocols,
    source_sha256: Option<String>,
}
impl From<Build> for CandidateMetadata {
    fn from(info: Build) -> Self {
        Self {
            version: info.version,
            commit: info.commit,
            protocols: info.protocols,
            floor: info.floor,
        }
    }
}
impl Build {
    /// `None` accepts any work protocol major.
    fn require_compatibility(&self, required_major: Option<u16>) -> Result<(), CoreError> {
        if let Some(major) = required_major
            && self.protocols.work.major != major
        {
            return Err(CoreError::IncompatibleProtocol(
                if self.protocols.work.major > major {
                    crate::protocol::version::Incompatible::CallerTooOld
                } else {
                    crate::protocol::version::Incompatible::ListenerTooOld
                },
            ));
        }
        Ok(())
    }

    fn parse(bytes: &[u8], identity: &str) -> Result<Self, CoreError> {
        Self::parse_for_target(bytes, identity, TARGET)
    }
    fn parse_for_target(bytes: &[u8], identity: &str, target: &str) -> Result<Self, CoreError> {
        if !matches!(target, "aarch64-apple-darwin" | "x86_64-unknown-linux-gnu") {
            return Err(CoreError::RuntimeUnavailable);
        }
        if bytes.len() > 4096 {
            return Err(CoreError::RuntimeUnavailable);
        }
        let info: Self =
            serde_json::from_slice(bytes).map_err(|_| CoreError::RuntimeUnavailable)?;
        if info.commit.len() != 40
            || !info
                .commit
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        {
            return Err(CoreError::RuntimeUnavailable);
        }
        let version = info.version.strip_prefix("dev.").is_some_and(|suffix| {
            let revision = if matches!(
                info.identity.as_str(),
                "spaceterm-development" | "spaceterm-preflight"
            ) {
                suffix.strip_suffix(".dirty").unwrap_or(suffix)
            } else {
                suffix
            };
            revision == &info.commit[..12]
        }) || (info.version.split('.').count() == 3
            && info.version.split('.').all(|part| {
                !part.is_empty()
                    && (part == "0" || !part.starts_with('0'))
                    && part.bytes().all(|v| v.is_ascii_digit())
            }));
        let floored = crate::update::facts::Version::parse(&info.version)
            .is_ok_and(|version| Candidate::new(version, info.floor).is_ok());
        if info.schema != 1
            || (info.stapled
                && (info.identity != "spaceterm" || info.target != "aarch64-apple-darwin"))
            || !floored
            || info.identity != identity
            || info.target != target
            || info.version.len() > 64
            || !version
            || info
                .source_sha256
                .as_deref()
                .is_some_and(|value| !digest_valid(value))
            || [
                info.protocols.work,
                info.protocols.service,
                info.protocols.desktop_channel,
            ]
            .iter()
            .any(|version| version.major == 0)
        {
            return Err(CoreError::RuntimeUnavailable);
        }
        Ok(info)
    }
}

/// One signed archive entry, read in pieces through the parser's fixed buffer.
pub(super) struct Member<'a> {
    reader: &'a mut dyn Read,
    size: u64,
    remaining: u64,
    buffer: &'a mut [u8],
}
impl<'a> Member<'a> {
    pub(super) fn new(reader: &'a mut dyn Read, size: u64, buffer: &'a mut [u8]) -> Self {
        Self {
            reader,
            size,
            remaining: size,
            buffer,
        }
    }

    /// The entry's signed length.
    pub(super) fn size(&self) -> u64 {
        self.size
    }

    /// The next piece of the entry, or `None` after its last byte.
    ///
    /// # Errors
    /// Returns RuntimeUnavailable when the archive can't be read or ends before the entry does.
    pub(super) fn next(&mut self) -> Result<Option<&[u8]>, CoreError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let wanted = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(self.buffer.len());
        let read = loop {
            match self.reader.read(&mut self.buffer[..wanted]) {
                Ok(0) => return Err(CoreError::RuntimeUnavailable),
                Ok(read) => break read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(CoreError::RuntimeUnavailable),
            }
        };
        self.remaining -= read as u64;
        Ok(Some(&self.buffer[..read]))
    }
}

/// One parser owns the signed layout for both extraction and retained-content verification.
/// Visitors receive each entry as a [`Member`]; the parser reads whatever a visitor leaves.
fn contents(
    bytes: &[u8],
    identity: &str,
    visit: impl FnMut(&Path, bool, u32, &mut Member<'_>) -> Result<(), CoreError>,
) -> Result<Build, CoreError> {
    contents_for_target(bytes, identity, TARGET, visit)
}
fn contents_for_target(
    bytes: &[u8],
    identity: &str,
    target: &str,
    mut visit: impl FnMut(&Path, bool, u32, &mut Member<'_>) -> Result<(), CoreError>,
) -> Result<Build, CoreError> {
    let mut buffer = vec![0; CHUNK];
    let layout = Layout::for_target(target, identity).ok_or(CoreError::RuntimeUnavailable)?;
    let main = layout.executable();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut names = BTreeSet::new();
    let mut expanded = 0u64;
    let mut manifest = None;
    let mut executable = false;
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for item in archive
        .entries()
        .map_err(|_| CoreError::RuntimeUnavailable)?
    {
        let mut item = item.map_err(|_| CoreError::RuntimeUnavailable)?;
        let raw = item.path_bytes();
        let path = item
            .path()
            .map_err(|_| CoreError::RuntimeUnavailable)?
            .into_owned();
        let directory = item.header().entry_type().is_dir();
        let canonical_path: PathBuf = path.components().collect();
        let canonical = canonical_path.as_os_str().as_bytes();
        if raw.as_ref() != canonical && !(directory && raw.strip_suffix(b"/") == Some(canonical)) {
            return Err(CoreError::RuntimeUnavailable);
        }
        let path = path
            .strip_prefix("core")
            .map_err(|_| CoreError::RuntimeUnavailable)?;
        if path
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
            || names.len() >= 128
            || !names.insert(path.to_owned())
            || (!directory && !item.header().entry_type().is_file())
            || (path.as_os_str().is_empty() && !directory)
        {
            return Err(CoreError::RuntimeUnavailable);
        }
        let mode = item
            .header()
            .mode()
            .map_err(|_| CoreError::RuntimeUnavailable)?;
        let expected_mode = if directory || path == main {
            0o755
        } else {
            0o644
        };
        if mode != expected_mode {
            return Err(CoreError::RuntimeUnavailable);
        }
        if directory {
            directories.insert(path.to_owned());
        } else {
            files.insert(path.to_owned());
        }
        let size = item.size();
        expanded = expanded
            .checked_add(size)
            .ok_or(CoreError::RuntimeUnavailable)?;
        if expanded > MAX_EXPANDED || (directory && size != 0) {
            return Err(CoreError::RuntimeUnavailable);
        }
        if path == main && !directory {
            executable = true;
        }
        if path == Path::new("build.json") {
            if size > 4096 {
                return Err(CoreError::RuntimeUnavailable);
            }
            let mut signed = Vec::new();
            let mut member = Member::new(&mut item, size, &mut buffer);
            while let Some(piece) = member.next()? {
                signed.extend_from_slice(piece);
            }
            manifest = Some(Build::parse_for_target(&signed, identity, target)?);
            visit(
                path,
                directory,
                mode,
                &mut Member::new(&mut signed.as_slice(), size, &mut buffer),
            )?;
        } else {
            let mut member = Member::new(&mut item, size, &mut buffer);
            visit(path, directory, mode, &mut member)?;
            while member.next()?.is_some() {}
        }
    }
    let mut required: BTreeSet<PathBuf> = serde_json::from_str::<Vec<PathBuf>>(include_str!(
        "../../../../packaging/core-resources.json"
    ))
    .map_err(|_| CoreError::RuntimeUnavailable)?
    .iter()
    .map(|listed| layout.resource(listed))
    .collect::<Option<_>>()
    .ok_or(CoreError::RuntimeUnavailable)?;
    required.extend([PathBuf::from("build.json"), main]);
    let manifest = manifest.ok_or(CoreError::RuntimeUnavailable)?;
    required.extend(layout.bundle_files(manifest.stapled));
    let terminfo_entries = ["terminfo/x/xterm-spaceterm", "terminfo/78/xterm-spaceterm"]
        .map(|entry| layout.resources().join(entry));
    let terminfo: Vec<_> = files
        .iter()
        .filter(|path| terminfo_entries.contains(path))
        .cloned()
        .collect();
    if terminfo.is_empty() {
        return Err(CoreError::RuntimeUnavailable);
    }
    required.extend(terminfo);
    let required_directories: BTreeSet<_> = files
        .iter()
        .flat_map(|path| path.ancestors().skip(1).map(Path::to_owned))
        .collect();
    if !executable || files != required || !directories.is_subset(&required_directories) {
        return Err(CoreError::RuntimeUnavailable);
    }
    Ok(manifest)
}
fn verify_contents(directory: &File, bytes: &[u8], identity: &str) -> Result<Build, CoreError> {
    let mut expected = BTreeSet::from([
        PathBuf::from("artifact.tar.gz"),
        PathBuf::from("artifact.sig"),
    ]);
    let mut installed = vec![0; CHUNK];
    let info = contents(bytes, identity, |path, is_directory, mode, signed| {
        if !path.as_os_str().is_empty() {
            let file = open_relative(directory, path, is_directory)?;
            let metadata = if is_directory {
                file.metadata().map_err(|_| CoreError::StorageUnavailable)?
            } else {
                files::regular(&file)?
            };
            if metadata.is_dir() != is_directory || metadata.mode() & 0o7777 != mode {
                return Err(CoreError::RuntimeUnavailable);
            }
            if !is_directory {
                if metadata.len() != signed.size() {
                    return Err(CoreError::RuntimeUnavailable);
                }
                compare(file, signed, &mut installed)?;
            }
            expected.insert(path.to_owned());
            expected.extend(
                path.ancestors()
                    .skip(1)
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Path::to_owned),
            );
        }
        Ok(())
    })?;
    fn scan(
        directory: &File,
        prefix: &Path,
        names: &mut BTreeSet<PathBuf>,
    ) -> Result<(), CoreError> {
        for name in files::names(directory)? {
            let path = prefix.join(&name);
            if names.len() >= 256 {
                return Err(CoreError::RuntimeUnavailable);
            }
            names.insert(path.clone());
            let entry = open_relative(directory, Path::new(&name), false)?;
            if entry
                .metadata()
                .map_err(|_| CoreError::StorageUnavailable)?
                .is_dir()
            {
                scan(&entry, &path, names)?;
            }
        }
        Ok(())
    }
    let mut actual = BTreeSet::new();
    scan(directory, Path::new(""), &mut actual)?;
    if actual != expected {
        return Err(CoreError::RuntimeUnavailable);
    }
    Ok(info)
}
/// Compare an installed file with its signed entry piece by piece, through `buffer`.
fn compare(mut file: File, signed: &mut Member<'_>, buffer: &mut [u8]) -> Result<(), CoreError> {
    let unread = |error: std::io::Error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            CoreError::RuntimeUnavailable
        } else {
            CoreError::StorageUnavailable
        }
    };
    while let Some(piece) = signed.next()? {
        let installed = &mut buffer[..piece.len()];
        file.read_exact(installed).map_err(unread)?;
        if installed != piece {
            return Err(CoreError::RuntimeUnavailable);
        }
    }
    files::ended(&mut file)
}
fn read_file(directory: &File, path: &Path, maximum: u64) -> Result<Vec<u8>, CoreError> {
    bounded(open_relative(directory, path, false)?, maximum)
}
fn bounded(file: File, maximum: u64) -> Result<Vec<u8>, CoreError> {
    if files::regular(&file)?.len() > maximum {
        return Err(CoreError::RuntimeUnavailable);
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CoreError::StorageUnavailable)?;
    if bytes.len() as u64 > maximum {
        return Err(CoreError::RuntimeUnavailable);
    }
    Ok(bytes)
}
fn source(path: &Path, maximum: u64) -> Result<Vec<u8>, CoreError> {
    bounded(open_source(path)?, maximum)
}
/// The package archive at `path`, copied once outside the heap. Authentication, parsing and
/// retention all read this copy, so the bytes installed are the bytes authenticated.
fn archive(path: &Path) -> Result<files::Snapshot, CoreError> {
    files::Snapshot::of(open_source(path)?, MAX_ARCHIVE)
}
fn open_source(path: &Path) -> Result<File, CoreError> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| CoreError::RuntimeUnavailable)
}
fn provision(
    parent: &Path,
    identity: &str,
    archive: &Path,
    signature: &Path,
    target: &str,
    commit: &str,
    version: &str,
) -> Result<RetainedInstallation, CoreError> {
    let bytes = self::archive(archive)?;
    let signature = source(signature, 64)?;
    authenticate(&bytes, &signature, identity)?;
    let info = contents(&bytes, identity, |_, _, _, _| Ok(()))?;
    if info.target != target || info.commit != commit || info.version != version {
        return Err(CoreError::RuntimeUnavailable);
    }
    retain(parent, identity, &bytes, &signature, &info)
}

fn provision_candidate(
    parent: &Path,
    identity: &str,
    archive: &Path,
    signature: &Path,
    expected_sha256: &str,
    required_major: Option<u16>,
) -> Result<RetainedInstallation, CoreError> {
    let (bytes, signature, info) = candidate(
        identity,
        archive,
        signature,
        expected_sha256,
        required_major,
    )?;
    retain(parent, identity, &bytes, &signature, &info)
}

fn update_candidate(
    identity: &str,
    archive: &Path,
    signature: &Path,
    expected_sha256: &str,
) -> Result<(files::Snapshot, Vec<u8>, Build), CoreError> {
    let bytes = self::archive(archive)?;
    let signature = source(signature, 64)?;
    authenticate(&bytes, &signature, identity)?;
    if !digest_valid(expected_sha256) || digest(&bytes) != expected_sha256 {
        return Err(CoreError::CandidateChanged);
    }
    let info = contents(&bytes, identity, |_, _, _, _| Ok(()))?;
    Ok((bytes, signature, info))
}

fn candidate(
    identity: &str,
    archive: &Path,
    signature: &Path,
    expected_sha256: &str,
    required_major: Option<u16>,
) -> Result<(files::Snapshot, Vec<u8>, Build), CoreError> {
    candidate_for_target(
        identity,
        archive,
        signature,
        expected_sha256,
        TARGET,
        required_major,
    )
}
fn candidate_for_target(
    identity: &str,
    archive: &Path,
    signature: &Path,
    expected_sha256: &str,
    target: &str,
    required_major: Option<u16>,
) -> Result<(files::Snapshot, Vec<u8>, Build), CoreError> {
    let bytes = self::archive(archive)?;
    let signature = source(signature, 64)?;
    let info = verified_candidate_build(
        identity,
        &bytes,
        &signature,
        expected_sha256,
        target,
        required_major,
    )?;
    Ok((bytes, signature, info))
}

fn verified_candidate_build(
    identity: &str,
    archive: &[u8],
    signature: &[u8],
    expected_sha256: &str,
    target: &str,
    required_major: Option<u16>,
) -> Result<Build, CoreError> {
    if archive.len() as u64 > MAX_ARCHIVE || signature.len() > 64 {
        return Err(CoreError::RuntimeUnavailable);
    }
    authenticate(archive, signature, identity)?;
    if !digest_valid(expected_sha256) || digest(archive) != expected_sha256 {
        return Err(CoreError::CandidateChanged);
    }
    let info = contents_for_target(archive, identity, target, |_, _, _, _| Ok(()))?;
    info.require_compatibility(required_major)?;
    Ok(info)
}

fn retain(
    parent: &Path,
    identity: &str,
    bytes: &[u8],
    signature: &[u8],
    info: &Build,
) -> Result<RetainedInstallation, CoreError> {
    let version = &info.version;
    let commit = &info.commit;
    // The account-private store is the lock used by both the Python installer and service controls.
    let directory = files::store(parent)?;
    let _directory = lock_directory(&directory)?;
    verify_directory_path(&directory, parent)?;
    let digest = digest(bytes);
    let destination = parent.join(&digest);
    let name = files::leaf(&destination)?;
    if files::exists(&directory, &name)? {
        let (retained_version, retained_commit) = describe(&destination, identity)?;
        if retained_version != *version || retained_commit != *commit {
            return Err(CoreError::RuntimeUnavailable);
        }
    } else {
        let prepared = files::Preparation::new(&directory)?;
        let mut buffer = vec![0; CHUNK];
        contents(bytes, identity, |path, is_directory, mode, data| {
            if is_directory {
                files::create_directories(&prepared.directory, path)?;
            } else {
                files::create_file(&prepared.directory, path, data, mode)?;
            }
            Ok(())
        })?;
        for (name, data) in [("artifact.tar.gz", bytes), ("artifact.sig", signature)] {
            files::create_file(
                &prepared.directory,
                Path::new(name),
                &mut Member::new(&mut &*data, data.len() as u64, &mut buffer),
                0o644,
            )?;
        }
        verify_contents(&prepared.directory, bytes, identity)?;
        let prepared_path = prepared.path(parent);
        verify_directory_path(&prepared.directory, &prepared_path)?;
        verify_native(&prepared_path, identity)?;
        verify_executable(&prepared_path, info)?;
        verify_directory_path(&directory, parent)?;
        prepared
            .directory
            .sync_all()
            .map_err(|_| CoreError::StorageUnavailable)?;
        prepared.publish(&name)?;
    }
    verify_directory_path(&directory, parent)?;
    Ok(RetainedInstallation {
        directory: destination,
        version: version.clone(),
        commit: commit.clone(),
    })
}
