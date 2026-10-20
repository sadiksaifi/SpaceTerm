//! Conservative proof for a frozen direct software-key route. Unsupported configuration defers.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::cancellation::SshCancellationToken;
use super::host_config::{HostConfigFilesystem, HostConfigFilesystemError};
use super::process::{SshProbeEnvironment, SshProcessAdapter, run_probe_process};
use crate::platform::{
    app_directories::AppDirectoryRoot,
    app_paths::AppPaths,
    secure_filesystem::{SecureCommitOutcome, SecureDirectory, SecureEntryIdentity},
};

#[derive(Clone, Eq, PartialEq)]
struct Dependency {
    path: PathBuf,
    digest: Option<[u8; 32]>,
}
impl Dependency {
    fn read(filesystem: &dyn HostConfigFilesystem, path: PathBuf) -> Option<Self> {
        let digest = match filesystem.file_digest_limited(&path, 4 * 1024 * 1024) {
            Ok(digest) => Some(digest),
            Err(HostConfigFilesystemError::Missing) => None,
            Err(_) => return None,
        };
        Some(Self { path, digest })
    }
    fn unchanged(&self, filesystem: &dyn HostConfigFilesystem) -> bool {
        Self::read(filesystem, self.path.clone()).as_ref() == Some(self)
    }
}
/// A deliberately limited scanner: literal includes only, no Match or glob expansion.
/// Rejection does not change OpenSSH configuration or requested authentication.
pub(super) struct ConfigProof {
    dependencies: Vec<Dependency>,
}
impl ConfigProof {
    pub(super) fn capture(
        filesystem: &dyn HostConfigFilesystem,
        root: &Path,
        home: &Path,
    ) -> Option<Self> {
        let mut dependencies = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        let mut seen = BTreeSet::new();
        while let Some(path) = pending.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            if seen.len() > 16 {
                return None;
            }
            dependencies.push(Dependency::read(filesystem, path.clone())?);
            let bytes = match filesystem.read_file_limited(&path, 1024 * 1024) {
                Ok(bytes) => bytes,
                Err(HostConfigFilesystemError::Missing) => continue,
                Err(_) => return None,
            };
            if bytes.len() > 1024 * 1024 {
                return None;
            }
            for line in std::str::from_utf8(&bytes).ok()?.lines() {
                let line = line.split('#').next()?.trim();
                let mut parts = line.split_whitespace();
                let Some(key) = parts.next() else {
                    continue;
                };
                let normalized_key = key.split('=').next()?;
                if normalized_key.eq_ignore_ascii_case("match") {
                    return None;
                }
                if normalized_key.eq_ignore_ascii_case("include") {
                    if key.contains('=') {
                        return None;
                    }
                    let includes: Vec<_> = parts.collect();
                    if includes.is_empty() {
                        return None;
                    }
                    for include in includes {
                        if !include.starts_with('/') && !include.starts_with("~/") {
                            return None;
                        }
                        pending.push(literal_path(include, home, path.parent()?)?);
                    }
                }
            }
        }
        if dependencies
            .iter()
            .all(|dependency| dependency.unchanged(filesystem))
        {
            Some(Self { dependencies })
        } else {
            None
        }
    }
}
pub(super) struct QuietSshRoute {
    dependencies: Vec<Dependency>,
    config: FrozenConfig,
    filesystem: Arc<dyn HostConfigFilesystem>,
}
impl QuietSshRoute {
    pub(super) fn prove<A: SshProcessAdapter>(
        context: &super::server_process::NativeSshServerConfig<A>,
        effective: &[u8],
        proof: ConfigProof,
        cancellation: &SshCancellationToken,
    ) -> Option<Self> {
        let paths = Arc::clone(&context.paths);
        let filesystem = Arc::clone(&context.filesystem);
        let home = context.home.as_path();
        let startup = &context.startup;
        let adapter = &context.adapter;
        let bootstrap = context.bootstrap.as_ref();
        let options = eligible_options(effective)?;
        let environment = SshProbeEnvironment::new(home.to_path_buf(), startup).ok()?;
        let mut dependencies = proof.dependencies;
        let mut keys = 0;
        for (name, values) in &options {
            if !matches!(
                name.as_str(),
                "identityfile" | "userknownhostsfile" | "globalknownhostsfile"
            ) {
                continue;
            }
            for values in values {
                for value in values.split_whitespace() {
                    if value == "none" {
                        continue;
                    }
                    let path = literal_path(value, home, home)?;
                    let dependency = Dependency::read(filesystem.as_ref(), path.clone())?;
                    if name == "identityfile" && dependency.digest.is_some() {
                        let command = bootstrap.key_public(&path)?;
                        let output = run_probe_process(
                            adapter,
                            &command,
                            &environment,
                            8192,
                            cancellation,
                            Instant::now() + Duration::from_secs(5),
                        )
                        .ok()?;
                        if !output.exit.is_success() {
                            return None;
                        }
                        let algorithm = std::str::from_utf8(&output.stdout)
                            .ok()?
                            .split_whitespace()
                            .next()?;
                        if !matches!(
                            algorithm,
                            "ssh-ed25519"
                                | "ssh-rsa"
                                | "ecdsa-sha2-nistp256"
                                | "ecdsa-sha2-nistp384"
                                | "ecdsa-sha2-nistp521"
                        ) {
                            return None;
                        }
                        keys += 1;
                        dependencies.push(Dependency::read(
                            filesystem.as_ref(),
                            PathBuf::from(format!("{}.pub", path.to_str()?)),
                        )?);
                        dependencies.push(Dependency::read(
                            filesystem.as_ref(),
                            PathBuf::from(format!("{}-cert.pub", path.to_str()?)),
                        )?);
                    }
                    dependencies.push(dependency);
                }
            }
        }
        if keys == 0
            || !dependencies
                .iter()
                .all(|dependency| dependency.unchanged(filesystem.as_ref()))
        {
            return None;
        }
        let mut frozen = String::new();
        for line in std::str::from_utf8(effective).ok()?.lines() {
            let key = line.split_whitespace().next()?;
            let software_algorithms;
            let replacement = match key {
                "pubkeyacceptedalgorithms" => {
                    software_algorithms = line
                        .split_once(' ')?
                        .1
                        .split(',')
                        .filter(|algorithm| !algorithm.contains("sk-"))
                        .collect::<Vec<_>>()
                        .join(",");
                    Some(software_algorithms.as_str())
                }
                "batchmode" => Some("yes"),
                "stricthostkeychecking" => Some("yes"),
                "passwordauthentication"
                | "kbdinteractiveauthentication"
                | "gssapiauthentication"
                | "hostbasedauthentication" => Some("no"),
                _ => None,
            };
            if let Some(value) = replacement {
                frozen.push_str(&format!("{key} {value}\n"));
            } else {
                frozen.push_str(line);
                frozen.push('\n');
            }
        }
        let config = FrozenConfig::create(paths, frozen.as_bytes())?;
        Some(Self {
            dependencies,
            config,
            filesystem,
        })
    }
    pub(super) fn unchanged(&self) -> bool {
        self.dependencies
            .iter()
            .all(|dependency| dependency.unchanged(self.filesystem.as_ref()))
            && self.config.unchanged()
    }
    pub(super) fn config_path(&self) -> PathBuf {
        self.config.path.clone()
    }
}
fn eligible_options(bytes: &[u8]) -> Option<BTreeMap<String, Vec<String>>> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.len() > 256 * 1024 {
        return None;
    }
    let mut options: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line.split_once(' ')?;
        if value.chars().any(char::is_control) || key.is_empty() {
            return None;
        }
        options
            .entry(key.to_owned())
            .or_default()
            .push(value.trim().to_owned());
    }
    if options.iter().any(|(key, values)| {
        values.len() != 1
            && !matches!(
                key.as_str(),
                "identityfile" | "userknownhostsfile" | "globalknownhostsfile"
            )
    }) {
        return None;
    }
    let single = |name: &str| {
        options.get(name).and_then(|values| {
            if values.len() == 1 {
                Some(values[0].as_str())
            } else {
                None
            }
        })
    };
    if single("identityagent") != Some("none")
        || single("identitiesonly") != Some("yes")
        || !matches!(single("pubkeyauthentication"), Some("yes" | "true"))
    {
        return None;
    }
    if !matches!(
        single("stricthostkeychecking"),
        Some("true" | "yes" | "ask")
    ) {
        return None;
    }
    for name in [
        "pkcs11provider",
        "proxycommand",
        "proxyjump",
        "knownhostscommand",
        "localcommand",
        "remotecommand",
        "certificatefile",
    ] {
        if single(name).is_some_and(|value| value != "none") {
            return None;
        }
    }
    // OpenSSH builds without an internal SK provider dump this default environment reference.
    // StartupSshEnvironment never captures SSH_SK_PROVIDER, so OpenSSH disables it; no external
    // provider is selected. Keys are independently proved software-only before this route is used.
    if !matches!(
        single("securitykeyprovider"),
        None | Some("none" | "internal" | "$SSH_SK_PROVIDER")
    ) {
        return None;
    }
    for name in [
        "usekeychain",
        "permitlocalcommand",
        "forwardagent",
        "addkeystoagent",
        "enablesshkeysign",
        "gssapiauthentication",
        "hostbasedauthentication",
    ] {
        if single(name).is_some_and(|value| !matches!(value, "no" | "false")) {
            return None;
        }
    }
    if ["localforward", "remoteforward", "dynamicforward", "setenv"]
        .iter()
        .any(|key| options.contains_key(*key))
    {
        return None;
    }
    if options.get("hostname")?.len() != 1
        || options.get("user")?.len() != 1
        || options.get("port")?.len() != 1
    {
        return None;
    }
    Some(options)
}
fn literal_path(value: &str, home: &Path, relative: &Path) -> Option<PathBuf> {
    if value.is_empty()
        || value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '%' | '$' | '*' | '?' | '[' | ']' | '\'' | '"' | '\\'
                )
        })
    {
        return None;
    }
    let path = if let Some(suffix) = value.strip_prefix("~/") {
        home.join(suffix)
    } else {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            relative.join(path)
        }
    };
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(path)
}
struct FrozenConfig {
    paths: Arc<AppPaths>,
    directory: SecureDirectory,
    name: OsString,
    path: PathBuf,
    identity: SecureEntryIdentity,
    digest: [u8; 32],
}
impl FrozenConfig {
    fn create(paths: Arc<AppPaths>, bytes: &[u8]) -> Option<Self> {
        let directory = paths.ensure_secure_root(AppDirectoryRoot::Data).ok()?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).ok()?;
        let name: OsString = format!(
            "ssh-quiet-{}.conf",
            nonce
                .iter()
                .map(|value| format!("{value:02x}"))
                .collect::<String>()
        )
        .into();
        let prepared = paths
            .filesystem()
            .prepare_private_file(&directory, &name, bytes, nonce)
            .ok()?;
        let committed = paths
            .filesystem()
            .commit_private_file(prepared, None)
            .ok()?;
        if committed.outcome != SecureCommitOutcome::Committed {
            return None;
        }
        Some(Self {
            path: paths.directories().data.join(&name),
            paths,
            directory,
            name,
            identity: committed.published_identity?,
            digest: Sha256::digest(bytes).into(),
        })
    }
    fn unchanged(&self) -> bool {
        self.paths
            .filesystem()
            .read_private_file(&self.directory, &self.name, 256 * 1024)
            .ok()
            .flatten()
            .is_some_and(|snapshot| {
                <[u8; 32]>::from(Sha256::digest(&snapshot.bytes)) == self.digest
            })
    }
}
impl Drop for FrozenConfig {
    fn drop(&mut self) {
        let _ = self.paths.filesystem().remove_private_file(
            &self.directory,
            &self.name,
            &self.identity,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> String {
        "host host\nhostname example.test\nuser example\nport 22\nidentityagent none\nidentitiesonly yes\npubkeyauthentication yes\nstricthostkeychecking ask\nidentityfile /test/software\n".into()
    }
    #[test]
    fn rejects_agent_provider_proxy_commands_and_permissive_host_trust() {
        assert!(eligible_options(options().as_bytes()).is_some());
        for extra in [
            "identityagent /agent",
            "pkcs11provider /provider",
            "proxycommand helper",
            "proxyjump jump",
            "knownhostscommand helper",
            "securitykeyprovider /provider",
            "usekeychain yes",
            "localcommand helper",
            "remotecommand helper",
            "stricthostkeychecking no",
            "forwardagent yes",
        ] {
            assert!(
                eligible_options(format!("{}{extra}\n", options()).as_bytes()).is_none(),
                "accepted {extra}"
            );
        }
    }
    #[test]
    fn accepts_the_disabled_openssh_default_provider_reference() {
        let standard = format!("{}securitykeyprovider $SSH_SK_PROVIDER\n", options());
        assert!(eligible_options(standard.as_bytes()).is_some());
        let external = format!("{}securitykeyprovider $CUSTOM_PROVIDER\n", options());
        assert!(eligible_options(external.as_bytes()).is_none());
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/quiet_continuation.rs"]
mod unix_adapter_tests;
