//! Authenticate and retain independent server artifacts without selecting or starting a runtime.
use super::*;
use base64::Engine;
use std::{
    collections::BTreeSet,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

mod files;
mod launcher;
mod layout;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

pub use layout::{Layout, bundle_identifier, bundle_name};

const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;
#[cfg(target_os = "macos")]
const TARGET: &str = "aarch64-apple-darwin";
#[cfg(target_os = "linux")]
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Authenticated signed declarations used before staging an update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateMetadata {
    pub version: String,
    pub commit: String,
    pub protocol_major: u16,
    pub capabilities: Vec<String>,
}

/// Authenticate the exact captured archive and signature for one application and target.
/// This verifies the digest, archive contents and required protocol capabilities without I/O.
///
/// # Errors
/// Returns RuntimeUnavailable for invalid authentication, bounds, target or archive contents;
/// CandidateChanged for a different digest; IncompatibleProtocol or MissingRequiredCapability
/// when the signed declarations do not meet the connection requirements.
pub fn verify_candidate_bytes(
    identity: &str,
    archive: &[u8],
    signature: &[u8],
    expected_sha256: &str,
    target: &str,
    required_major: u16,
    required_capabilities: &[&str],
) -> Result<CandidateMetadata, ServerError> {
    verified_candidate_build(
        identity,
        archive,
        signature,
        expected_sha256,
        target,
        Some(required_major),
        required_capabilities,
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
    Failed(ServerError),
}

/// An installer preview carries only bounded service effects and a nonsecret approval token.
#[cfg(feature = "client")]
#[derive(Debug)]
pub enum CliUninstall {
    Preview {
        token: String,
        expires_at: u64,
        sessions: Vec<crate::hierarchy::Id>,
    },
    Completed,
}

#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UninstallConfirmation {
    token: String,
    expires_at: u64,
    observation: String,
}

impl Service {
    /// Retain an installer-selected signed package and select its CLI under the service lock.
    /// The unpacked installation must carry its original artifact.tar.gz and artifact.sig.
    /// This never registers or starts a service, or opens a credential store.
    pub fn retain_cli(&self, installation: &Path) -> Result<bool, ServerError> {
        let archive = source(&installation.join("artifact.tar.gz"), MAX_ARCHIVE)?;
        let signature = source(&installation.join("artifact.sig"), 64)?;
        authenticate(&archive, &signature, &self.identity)?;
        let info = contents(&archive, &self.identity, |_, _, _, _| Ok(()))?;
        info.require_compatibility(Some(crate::protocol::MAJOR), &["workspace_policy"])?;
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
            CliLink::Blocked => Err(ServerError::StorageUnavailable),
        }
    }

    /// Link the command-line launcher exactly as the installer does, for a client acting with
    /// this account's authority. The launcher follows the approved runtime; without one it
    /// follows the installation `provision` retains, which runs under the service lock.
    /// Another tool's launcher, or its entry at the temporary name, stays untouched. Another
    /// service operation that outlasts the operation grace returns ServiceBusy.
    pub fn link_cli(
        &self,
        provision: impl FnOnce() -> Result<PathBuf, ServerError>,
    ) -> Result<CliLink, ServerError> {
        let data = Directory::open(&self.paths.data, true)?;
        let _operation = match data.wait_lock("service.lock", OPERATION_GRACE, None)? {
            LockWait::Locked(lock) => lock,
            LockWait::Held | LockWait::Cancelled => return Err(ServerError::ServiceBusy),
        };
        self.select_cli_locked(&data, provision)
    }

    /// Read what the command-line launcher path holds, without following it or taking a lock.
    /// Unreadable service policy makes no launcher Outdated.
    pub fn cli_launcher(&self) -> Result<CliLauncher, ServerError> {
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
    pub fn cli_launcher_path(&self) -> Result<PathBuf, ServerError> {
        Ok(self.cli_home()?.join(".local/bin").join(&self.identity))
    }

    /// The temporary name a replacement links from, which [`CliLauncher::Blocked`] names.
    pub fn cli_temporary_path(&self) -> Result<PathBuf, ServerError> {
        Ok(launcher::temporary(
            &self.cli_home()?.join(".local/bin"),
            &self.identity,
        ))
    }

    fn select_cli_locked(
        &self,
        data: &Directory,
        provision: impl FnOnce() -> Result<PathBuf, ServerError>,
    ) -> Result<CliLink, ServerError> {
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

    fn cli_home(&self) -> Result<&Path, ServerError> {
        let suffix = Path::new(".local/share")
            .join(&self.identity)
            .join("server");
        if !self.paths.data.ends_with(suffix) {
            return Err(ServerError::UnsafeStorage);
        }
        self.paths
            .data
            .ancestors()
            .nth(4)
            .ok_or(ServerError::UnsafeStorage)
    }

    /// Keep an existing managed launcher on the policy's approved runtime. The service lock
    /// must be held. Services do not create a new PATH launcher as a side effect.
    ///
    /// The launcher never blocks a service operation: a failure leaves it on its earlier
    /// installation, where [`Self::cli_launcher`] reads it as Outdated or Stale and a client
    /// offers to link it again.
    pub(super) fn repoint_cli_locked(&self, runtime: &Path) -> Repoint {
        let Ok(home) = self.cli_home() else {
            return Repoint::Failed(ServerError::UnsafeStorage);
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

    /// Preview an uninstall or confirm its exact service and live Session observation.
    /// A preview is retained for ten minutes. Neither preview nor retention starts a server.
    #[cfg(feature = "client")]
    pub fn uninstall_cli(&self, confirmation: Option<&str>) -> Result<CliUninstall, ServerError> {
        // Admission waits for the service lock, so establish this passive link first.
        let initial = self.passive_status()?;
        let link = if initial.running {
            Some(
                crate::ServerConnection::for_cli(&self.identity, crate::CliRoute::Local, None)?
                    .link()?,
            )
        } else {
            None
        };
        let data = Directory::open(&self.paths.data, true)?;
        let _operation = data.lock("service.lock", false)?;
        data.verify_path(&self.paths.data)?;
        let state = Self::state(&data)?;
        let status = self.passive_status()?;
        if status.running != initial.running {
            return Err(ServerError::Changed);
        }
        let sessions = if let Some(link) = link {
            if !matches!(link.health(), crate::connection::link::Health::Live) {
                return Err(ServerError::Changed);
            }
            let (_, revision) = link.next_inventory(0).map_err(|_| ServerError::Protocol)?;
            let close = revision
                .inventory
                .close
                .as_ref()
                .ok_or(ServerError::Protocol)?;
            close
                .sessions
                .iter()
                .filter_map(|pane| pane.session)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let observation =
            activation::confirmation_token(&(&self.identity, &state, &status, &sessions))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| ServerError::StorageUnavailable)?
            .as_secs();
        if let Some(token) = confirmation {
            let preview = data
                .read_json::<UninstallConfirmation>("cli-uninstall.json")?
                .ok_or(ServerError::Changed)?;
            if preview.token != token
                || preview.observation != observation
                || now > preview.expires_at
            {
                return Err(ServerError::Changed);
            }
            data.remove(c"cli-uninstall.json")?;
        } else if status.running {
            let token = format!(
                "c1-{}",
                crate::protocol::random()?
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let expires_at = now + 600;
            data.write_json(
                c"cli-uninstall.json",
                &UninstallConfirmation {
                    token: token.clone(),
                    expires_at,
                    observation,
                },
            )?;
            return Ok(CliUninstall::Preview {
                token,
                expires_at,
                sessions,
            });
        }
        if state.is_some() {
            self.apply_control_locked(&data, control::Action::Uninstall, Some(status.running))?;
        }
        launcher::remove(
            self.cli_home()?,
            &self.identity,
            &self.paths.data.join("installations"),
        )?;
        Ok(CliUninstall::Completed)
    }

    /// Authenticate an update for this native target without comparing protocol majors.
    /// Activation still verifies that the native executable reports its own signed manifest.
    pub fn inspect_update_candidate(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
    ) -> Result<CandidateMetadata, ServerError> {
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
        required_capabilities: &[&str],
    ) -> Result<CandidateMetadata, ServerError> {
        let (_, _, info) = candidate_for_target(
            &self.identity,
            archive,
            signature,
            expected_sha256,
            target,
            Some(required_major),
            required_capabilities,
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
    ) -> Result<RetainedInstallation, ServerError> {
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
        required_capabilities: &[&str],
    ) -> Result<CandidateMetadata, ServerError> {
        let (_, _, info) = candidate(
            &self.identity,
            archive,
            signature,
            expected_sha256,
            Some(required_major),
            required_capabilities,
        )?;
        Ok(info.into())
    }

    /// Verify and retain the archive pinned by the running client, independently of its release.
    ///
    /// Required package capabilities gate staging; optional ones do not.
    /// This changes no service policy, approved installation, Stop intent or running work.
    ///
    /// # Errors
    /// Returns CandidateChanged for a different whole-archive digest, IncompatibleProtocol or
    /// MissingRequiredCapability for unsupported declarations, and the ordinary verification
    /// or storage classifications for an invalid artifact or inaccessible installation store.
    pub fn provision_candidate(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
        required_major: u16,
        required_capabilities: &[&str],
    ) -> Result<RetainedInstallation, ServerError> {
        provision_candidate(
            &self.paths.data.join("installations"),
            &self.identity,
            archive,
            signature,
            expected_sha256,
            Some(required_major),
            required_capabilities,
        )
    }

    /// Verify and retain a candidate of any protocol major for remote administration, which
    /// reaches the server over the version-stable service lane.
    pub fn stage_candidate(
        &self,
        archive: &Path,
        signature: &Path,
        expected_sha256: &str,
        required_capabilities: &[&str],
    ) -> Result<RetainedInstallation, ServerError> {
        provision_candidate(
            &self.paths.data.join("installations"),
            &self.identity,
            archive,
            signature,
            expected_sha256,
            None,
            required_capabilities,
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
fn public_key(identity: &str) -> Result<Vec<u8>, ServerError> {
    #[cfg(test)]
    if test_support::active() {
        return Ok(if identity == "spaceterm" {
            test_support::production_key()
        } else {
            test_support::key()
        });
    }
    let key = match identity {
        "spaceterm" => {
            let template = include_str!("../../../../packaging/macos/spaceterm/Info.plist");
            template
                .split_once("<key>SUPublicEDKey</key>")
                .and_then(|(_, tail)| tail.trim_start().strip_prefix("<string>"))
                .and_then(|tail| tail.split_once("</string>"))
                .ok_or(ServerError::RuntimeUnavailable)?
                .0
                .to_owned()
        }
        "spaceterm-preflight" | "spaceterm-development" => {
            #[derive(Deserialize)]
            struct NonproductionTrust {
                public_key: String,
            }
            // The seed in this file is deliberately public and has no Release authority.
            serde_json::from_str::<NonproductionTrust>(include_str!(
                "../../../../packaging/server-nonproduction-trust.json"
            ))
            .map_err(|_| ServerError::RuntimeUnavailable)?
            .public_key
        }
        _ => return Err(ServerError::RuntimeUnavailable),
    };
    base64::engine::general_purpose::STANDARD
        .decode(key.trim())
        .map_err(|_| ServerError::RuntimeUnavailable)
}
pub(super) fn authenticate(
    bytes: &[u8],
    signature: &[u8],
    identity: &str,
) -> Result<(), ServerError> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key(identity)?)
        .verify(bytes, signature)
        .map_err(|_| ServerError::RuntimeUnavailable)
}
pub(super) fn verify(path: &Path, identity: &str) -> Result<(), ServerError> {
    verify_installation(path, identity, None)
}

/// Recheck authenticated retained declarations at activation admission and dispatch. Any
/// protocol major activates: the candidate's own build information must be self-consistent and
/// native, and the service lane reaches it across majors.
pub(super) fn verify_candidate_installation(
    path: &Path,
    identity: &str,
    required_capabilities: &[&str],
) -> Result<(), ServerError> {
    verify_installation(path, identity, Some((None, required_capabilities)))
}

fn verify_installation(
    path: &Path,
    identity: &str,
    candidate_requirements: Option<(Option<u16>, &[&str])>,
) -> Result<(), ServerError> {
    let directory = owned_directory(path)?;
    let bytes = read_file(&directory, Path::new("artifact.tar.gz"), MAX_ARCHIVE)?;
    let signature = read_file(&directory, Path::new("artifact.sig"), 64)?;
    if path.file_name().and_then(|v| v.to_str()) != Some(&digest(&bytes)) {
        return Err(ServerError::RuntimeUnavailable);
    }
    authenticate(&bytes, &signature, identity)?;
    let info = verify_contents(&directory, &bytes, identity)?;
    if let Some((major, capabilities)) = candidate_requirements {
        info.require_compatibility(major, capabilities)?;
    }
    verify_native(path, identity)?;
    verify_executable(path, &info)?;
    verify_directory_path(&directory, path)
}
fn verify_native(path: &Path, identity: &str) -> Result<(), ServerError> {
    #[cfg(test)]
    if test_support::active() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let layout = Layout::host(identity).ok_or(ServerError::RuntimeUnavailable)?;
        let mut requirement = format!("identifier \"{}\"", bundle_identifier(identity));
        if identity == "spaceterm" {
            let template = include_str!("../../../../packaging/macos/spaceterm/Packager.toml");
            let signer = template
                .lines()
                .find_map(|line| line.strip_prefix("signing-identity = \""))
                .and_then(|v| v.strip_suffix('"'))
                .ok_or(ServerError::RuntimeUnavailable)?;
            requirement.push_str(&format!(" and certificate leaf = H\"{signer}\""));
        }
        command(
            "/usr/bin/codesign",
            &[
                "--verify".into(),
                "--strict".into(),
                "-R".into(),
                format!("={requirement}").into(),
                path.join(layout.signed()).into_os_string(),
            ],
        )
        .map_err(|_| ServerError::RuntimeUnavailable)?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (path, identity);
    Ok(())
}
fn verify_executable(path: &Path, expected: &Build) -> Result<(), ServerError> {
    #[cfg(test)]
    let bytes = if test_support::active() {
        test_support::native_build(&expected.identity)
    } else {
        executable_build(path, &expected.identity)?
    };
    #[cfg(not(test))]
    let bytes = executable_build(path, &expected.identity)?;
    let reported = Build::parse(&bytes, &expected.identity)?;
    if reported != *expected {
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok(())
}
fn executable_build(path: &Path, identity: &str) -> Result<Vec<u8>, ServerError> {
    let layout = Layout::host(identity).ok_or(ServerError::RuntimeUnavailable)?;
    let bytes = execute(
        Command::new(path.join(layout.executable()))
            .args(["server", "provision", "build-info"])
            .env_clear()
            .current_dir(path),
    )
    .map_err(|_| ServerError::RuntimeUnavailable)?;
    if bytes.len() > 4096 {
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok(bytes)
}

pub(super) fn describe(path: &Path, identity: &str) -> Result<(String, String), ServerError> {
    verify(path, identity)?;
    let directory = owned_directory(path)?;
    let info = Build::parse(
        &read_file(&directory, Path::new("build.json"), 4096)?,
        identity,
    )?;
    Ok((info.version, info.commit))
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Build {
    schema: u32,
    identity: String,
    target: String,
    commit: String,
    version: String,
    protocol_major: u16,
    capabilities: Vec<String>,
    source_sha256: Option<String>,
}
impl From<Build> for CandidateMetadata {
    fn from(info: Build) -> Self {
        Self {
            version: info.version,
            commit: info.commit,
            protocol_major: info.protocol_major,
            capabilities: info.capabilities,
        }
    }
}
impl Build {
    /// `None` accepts any protocol major.
    fn require_compatibility(
        &self,
        required_major: Option<u16>,
        required_capabilities: &[&str],
    ) -> Result<(), ServerError> {
        if required_major.is_some_and(|major| self.protocol_major != major) {
            return Err(ServerError::IncompatibleProtocol);
        }
        if required_capabilities
            .iter()
            .any(|required| !self.capabilities.iter().any(|value| value == required))
        {
            return Err(ServerError::MissingRequiredCapability);
        }
        Ok(())
    }

    fn parse(bytes: &[u8], identity: &str) -> Result<Self, ServerError> {
        Self::parse_for_target(bytes, identity, TARGET)
    }
    fn parse_for_target(bytes: &[u8], identity: &str, target: &str) -> Result<Self, ServerError> {
        if !matches!(target, "aarch64-apple-darwin" | "x86_64-unknown-linux-gnu") {
            return Err(ServerError::RuntimeUnavailable);
        }
        if bytes.len() > 4096 {
            return Err(ServerError::RuntimeUnavailable);
        }
        let info: Self =
            serde_json::from_slice(bytes).map_err(|_| ServerError::RuntimeUnavailable)?;
        if info.commit.len() != 40
            || !info
                .commit
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        {
            return Err(ServerError::RuntimeUnavailable);
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
        if info.schema != 1
            || info.identity != identity
            || info.target != target
            || info.version.len() > 64
            || !version
            || info
                .source_sha256
                .as_deref()
                .is_some_and(|value| !digest_valid(value))
            || info.protocol_major == 0
            || info.capabilities.len() > 32
            || info.capabilities.iter().any(|value| {
                value.is_empty()
                    || value.len() > 64
                    || !value.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
            || info.capabilities.iter().collect::<BTreeSet<_>>().len() != info.capabilities.len()
        {
            return Err(ServerError::RuntimeUnavailable);
        }
        Ok(info)
    }
}

/// One parser owns the signed layout for both extraction and retained-content verification.
fn contents(
    bytes: &[u8],
    identity: &str,
    visit: impl FnMut(&Path, bool, u32, &[u8]) -> Result<(), ServerError>,
) -> Result<Build, ServerError> {
    contents_for_target(bytes, identity, TARGET, visit)
}
fn contents_for_target(
    bytes: &[u8],
    identity: &str,
    target: &str,
    mut visit: impl FnMut(&Path, bool, u32, &[u8]) -> Result<(), ServerError>,
) -> Result<Build, ServerError> {
    let layout = Layout::for_target(target, identity).ok_or(ServerError::RuntimeUnavailable)?;
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
        .map_err(|_| ServerError::RuntimeUnavailable)?
    {
        let item = item.map_err(|_| ServerError::RuntimeUnavailable)?;
        let raw = item.path_bytes();
        let path = item
            .path()
            .map_err(|_| ServerError::RuntimeUnavailable)?
            .into_owned();
        let directory = item.header().entry_type().is_dir();
        let canonical_path: PathBuf = path.components().collect();
        let canonical = canonical_path.as_os_str().as_bytes();
        if raw.as_ref() != canonical && !(directory && raw.strip_suffix(b"/") == Some(canonical)) {
            return Err(ServerError::RuntimeUnavailable);
        }
        let path = path
            .strip_prefix("server")
            .map_err(|_| ServerError::RuntimeUnavailable)?;
        if path
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
            || names.len() >= 128
            || !names.insert(path.to_owned())
            || (!directory && !item.header().entry_type().is_file())
            || (path.as_os_str().is_empty() && !directory)
        {
            return Err(ServerError::RuntimeUnavailable);
        }
        let mode = item
            .header()
            .mode()
            .map_err(|_| ServerError::RuntimeUnavailable)?;
        let expected_mode = if directory || path == main {
            0o755
        } else {
            0o644
        };
        if mode != expected_mode {
            return Err(ServerError::RuntimeUnavailable);
        }
        if directory {
            directories.insert(path.to_owned());
        } else {
            files.insert(path.to_owned());
        }
        let size = item.size();
        expanded = expanded
            .checked_add(size)
            .ok_or(ServerError::RuntimeUnavailable)?;
        if expanded > MAX_EXPANDED || (directory && size != 0) {
            return Err(ServerError::RuntimeUnavailable);
        }
        let mut signed = Vec::new();
        item.take(size + 1)
            .read_to_end(&mut signed)
            .map_err(|_| ServerError::RuntimeUnavailable)?;
        if signed.len() as u64 != size {
            return Err(ServerError::RuntimeUnavailable);
        }
        if path == Path::new("build.json") {
            manifest = Some(Build::parse_for_target(&signed, identity, target)?);
        }
        if path == main && !directory {
            executable = true;
        }
        visit(path, directory, mode, &signed)?;
    }
    let mut required: BTreeSet<PathBuf> = serde_json::from_str::<Vec<PathBuf>>(include_str!(
        "../../../../packaging/server-resources.json"
    ))
    .map_err(|_| ServerError::RuntimeUnavailable)?
    .iter()
    .map(|listed| layout.resource(listed))
    .collect::<Option<_>>()
    .ok_or(ServerError::RuntimeUnavailable)?;
    required.extend([PathBuf::from("build.json"), main]);
    required.extend(layout.bundle_files());
    let terminfo_entries = ["terminfo/x/xterm-spaceterm", "terminfo/78/xterm-spaceterm"]
        .map(|entry| layout.resources().join(entry));
    let terminfo: Vec<_> = files
        .iter()
        .filter(|path| terminfo_entries.contains(path))
        .cloned()
        .collect();
    if terminfo.is_empty() {
        return Err(ServerError::RuntimeUnavailable);
    }
    required.extend(terminfo);
    let required_directories: BTreeSet<_> = files
        .iter()
        .flat_map(|path| path.ancestors().skip(1).map(Path::to_owned))
        .collect();
    if !executable || files != required || !directories.is_subset(&required_directories) {
        return Err(ServerError::RuntimeUnavailable);
    }
    manifest.ok_or(ServerError::RuntimeUnavailable)
}
fn verify_contents(directory: &File, bytes: &[u8], identity: &str) -> Result<Build, ServerError> {
    let mut expected = BTreeSet::from([
        PathBuf::from("artifact.tar.gz"),
        PathBuf::from("artifact.sig"),
    ]);
    let info = contents(bytes, identity, |path, is_directory, mode, signed| {
        if !path.as_os_str().is_empty() {
            let file = open_relative(directory, path, is_directory)?;
            let metadata = file
                .metadata()
                .map_err(|_| ServerError::StorageUnavailable)?;
            if metadata.is_dir() != is_directory || metadata.mode() & 0o7777 != mode {
                return Err(ServerError::RuntimeUnavailable);
            }
            if !is_directory && read_file(directory, path, signed.len() as u64)? != signed {
                return Err(ServerError::RuntimeUnavailable);
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
    ) -> Result<(), ServerError> {
        for name in files::names(directory)? {
            let path = prefix.join(&name);
            if names.len() >= 256 {
                return Err(ServerError::RuntimeUnavailable);
            }
            names.insert(path.clone());
            let entry = open_relative(directory, Path::new(&name), false)?;
            if entry
                .metadata()
                .map_err(|_| ServerError::StorageUnavailable)?
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
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok(info)
}
fn read_file(directory: &File, path: &Path, maximum: u64) -> Result<Vec<u8>, ServerError> {
    bounded(open_relative(directory, path, false)?, maximum)
}
fn bounded(file: File, maximum: u64) -> Result<Vec<u8>, ServerError> {
    let metadata = file
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > maximum {
        return Err(ServerError::RuntimeUnavailable);
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ServerError::StorageUnavailable)?;
    if bytes.len() as u64 > maximum {
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok(bytes)
}
fn source(path: &Path, maximum: u64) -> Result<Vec<u8>, ServerError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| ServerError::RuntimeUnavailable)?;
    bounded(file, maximum)
}
fn provision(
    parent: &Path,
    identity: &str,
    archive: &Path,
    signature: &Path,
    target: &str,
    commit: &str,
    version: &str,
) -> Result<RetainedInstallation, ServerError> {
    let bytes = source(archive, MAX_ARCHIVE)?;
    let signature = source(signature, 64)?;
    authenticate(&bytes, &signature, identity)?;
    let info = contents(&bytes, identity, |_, _, _, _| Ok(()))?;
    if info.target != target || info.commit != commit || info.version != version {
        return Err(ServerError::RuntimeUnavailable);
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
    required_capabilities: &[&str],
) -> Result<RetainedInstallation, ServerError> {
    let (bytes, signature, info) = candidate(
        identity,
        archive,
        signature,
        expected_sha256,
        required_major,
        required_capabilities,
    )?;
    retain(parent, identity, &bytes, &signature, &info)
}

fn update_candidate(
    identity: &str,
    archive: &Path,
    signature: &Path,
    expected_sha256: &str,
) -> Result<(Vec<u8>, Vec<u8>, Build), ServerError> {
    let bytes = source(archive, MAX_ARCHIVE)?;
    let signature = source(signature, 64)?;
    authenticate(&bytes, &signature, identity)?;
    if !digest_valid(expected_sha256) || digest(&bytes) != expected_sha256 {
        return Err(ServerError::CandidateChanged);
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
    required_capabilities: &[&str],
) -> Result<(Vec<u8>, Vec<u8>, Build), ServerError> {
    candidate_for_target(
        identity,
        archive,
        signature,
        expected_sha256,
        TARGET,
        required_major,
        required_capabilities,
    )
}
fn candidate_for_target(
    identity: &str,
    archive: &Path,
    signature: &Path,
    expected_sha256: &str,
    target: &str,
    required_major: Option<u16>,
    required_capabilities: &[&str],
) -> Result<(Vec<u8>, Vec<u8>, Build), ServerError> {
    let bytes = source(archive, MAX_ARCHIVE)?;
    let signature = source(signature, 64)?;
    let info = verified_candidate_build(
        identity,
        &bytes,
        &signature,
        expected_sha256,
        target,
        required_major,
        required_capabilities,
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
    required_capabilities: &[&str],
) -> Result<Build, ServerError> {
    if archive.len() as u64 > MAX_ARCHIVE || signature.len() > 64 {
        return Err(ServerError::RuntimeUnavailable);
    }
    authenticate(archive, signature, identity)?;
    if !digest_valid(expected_sha256) || digest(archive) != expected_sha256 {
        return Err(ServerError::CandidateChanged);
    }
    let info = contents_for_target(archive, identity, target, |_, _, _, _| Ok(()))?;
    info.require_compatibility(required_major, required_capabilities)?;
    Ok(info)
}

fn retain(
    parent: &Path,
    identity: &str,
    bytes: &[u8],
    signature: &[u8],
    info: &Build,
) -> Result<RetainedInstallation, ServerError> {
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
            return Err(ServerError::RuntimeUnavailable);
        }
    } else {
        let prepared = files::Preparation::new(&directory)?;
        contents(bytes, identity, |path, is_directory, mode, data| {
            if is_directory {
                files::create_directories(&prepared.directory, path)?;
            } else {
                files::create_file(&prepared.directory, path, data, mode)?;
            }
            Ok(())
        })?;
        files::create_file(
            &prepared.directory,
            Path::new("artifact.tar.gz"),
            bytes,
            0o644,
        )?;
        files::create_file(
            &prepared.directory,
            Path::new("artifact.sig"),
            signature,
            0o644,
        )?;
        verify_contents(&prepared.directory, bytes, identity)?;
        let prepared_path = prepared.path(parent);
        verify_directory_path(&prepared.directory, &prepared_path)?;
        verify_native(&prepared_path, identity)?;
        verify_executable(&prepared_path, info)?;
        verify_directory_path(&directory, parent)?;
        prepared
            .directory
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)?;
        prepared.publish(&name)?;
    }
    verify_directory_path(&directory, parent)?;
    Ok(RetainedInstallation {
        directory: destination,
        version: version.clone(),
        commit: commit.clone(),
    })
}
