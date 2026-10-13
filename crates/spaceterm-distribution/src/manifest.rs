//! An authenticated installation describes bytes, not native approval authority.
use crate::{
    BuildChannel, BuildVersion, DistributionFailure, ServerComponent, ServerPlatform,
    StorageCompatibility, VerifiedServerArchive,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerEntryPoints {
    pub server: String,
    pub launcher: String,
    pub manager: String,
    #[serde(deserialize_with = "crate::encoding::required_optional")]
    pub credential_tool: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationFile {
    pub name: String,
    pub mode: u32,
    pub size: u64,
    #[serde(with = "crate::encoding::sha256")]
    pub sha256: [u8; 32],
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundledDependency {
    pub name: String,
    pub version: String,
    pub source_identity: String,
    pub license: String,
    pub notice_file: String,
    pub corresponding_source: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationManifestV1 {
    pub schema_version: u16,
    pub component: ServerComponent,
    pub build_identity: BuildChannel,
    pub platform: ServerPlatform,
    pub version: BuildVersion,
    #[serde(with = "crate::encoding::commit")]
    pub build_commit: [u8; 20],
    pub protocol_majors: Vec<u16>,
    pub storage: StorageCompatibility,
    pub native_identifier: String,
    pub launcher_abi: u16,
    pub entry_points: ServerEntryPoints,
    pub files: Vec<InstallationFile>,
    pub dependencies: Vec<BundledDependency>,
}
pub struct VerifiedInstallationManifest {
    pub(crate) document: InstallationManifestV1,
}
impl VerifiedInstallationManifest {
    pub fn document(&self) -> &InstallationManifestV1 {
        &self.document
    }
}
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 4096;

pub(crate) fn relative_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('/')
        && !value.chars().any(|c| c.is_control() || c == '\\')
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

impl InstallationManifestV1 {
    /// Validates comparison facts only. A decoded manifest is not authenticated authority.
    pub fn validate(&self) -> Result<(), DistributionFailure> {
        use crate::server_feed::sorted_nonzero;
        if self.schema_version != 1 || self.launcher_abi != 2 {
            return Err(DistributionFailure::UnsupportedSchema);
        }
        if self.native_identifier != self.build_identity.native_identifier()
            || !sorted_nonzero(&self.protocol_majors, 8, false)
            || !self.storage.validate()
            || self.build_commit == [0; 20]
        {
            return Err(DistributionFailure::ManifestRejected);
        }
        if self.build_identity == BuildChannel::Release
            && crate::version::stable_version(self.version.as_str()).is_none()
        {
            return Err(DistributionFailure::ManifestRejected);
        }
        if self.version.as_str().starts_with("dev.") {
            let commit = self
                .build_commit
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect::<String>();
            let expected = format!("dev.{}", &commit[..12]);
            if self.version.as_str() != expected
                && self.version.as_str() != format!("{expected}.dirty")
            {
                return Err(DistributionFailure::ManifestRejected);
            }
        }
        let expected_server = match self.platform {
            ServerPlatform::LinuxX86_64 => "bin/spaceterm-server".to_owned(),
            ServerPlatform::DarwinArm64 => format!(
                "{}/Contents/MacOS/spaceterm-server",
                self.build_identity.bundle_name()
            ),
        };
        if self.entry_points.server != expected_server
            || self.entry_points.launcher != "libexec/spaceterm-server-launcher"
            || self.entry_points.manager != "libexec/spaceterm-server-manager"
            || self.entry_points.credential_tool.as_deref()
                != match self.platform {
                    ServerPlatform::LinuxX86_64 => Some("libexec/credential-tools/systemd-creds"),
                    ServerPlatform::DarwinArm64 => None,
                }
        {
            return Err(DistributionFailure::ManifestRejected);
        }
        if self.files.is_empty() || self.files.len() > MAX_FILES || self.dependencies.len() > 128 {
            return Err(DistributionFailure::LimitExceeded);
        }
        if !self.files.windows(2).all(|p| p[0].name < p[1].name) {
            return Err(DistributionFailure::ManifestRejected);
        }
        let names: std::collections::BTreeSet<_> =
            self.files.iter().map(|file| file.name.as_str()).collect();
        let mut total = 0_u64;
        for file in &self.files {
            if !relative_name(&file.name)
                || file.name == "installation.json"
                || file.mode & !0o777 != 0
                || file.mode & 0o022 != 0
                || file.mode & 0o400 == 0
                || file.size > MAX_EXPANDED_BYTES
            {
                return Err(DistributionFailure::ManifestRejected);
            }
            total = total
                .checked_add(file.size)
                .ok_or(DistributionFailure::LimitExceeded)?;
            // Check ancestors without generating a quadratic set of prefix strings.
            let mut name = file.name.as_str();
            while let Some((parent, _)) = name.rsplit_once('/') {
                if names.contains(parent) {
                    return Err(DistributionFailure::ManifestRejected);
                }
                name = parent;
            }
        }
        if total > MAX_EXPANDED_BYTES {
            return Err(DistributionFailure::LimitExceeded);
        }
        for name in [
            &self.entry_points.server,
            &self.entry_points.launcher,
            &self.entry_points.manager,
        ]
        .into_iter()
        .chain(self.entry_points.credential_tool.iter())
        {
            let file = self
                .files
                .iter()
                .find(|file| file.name == *name)
                .ok_or(DistributionFailure::ManifestRejected)?;
            if file.size == 0 || file.mode & 0o111 == 0 {
                return Err(DistributionFailure::ManifestRejected);
            }
            if self.platform == ServerPlatform::LinuxX86_64
                && name == &self.entry_points.server
                && file.mode != 0o501
            {
                return Err(DistributionFailure::ManifestRejected);
            }
        }
        let mut dependencies = std::collections::BTreeSet::new();
        for dependency in &self.dependencies {
            if !dependencies.insert(&dependency.name)
                || [
                    &dependency.name,
                    &dependency.version,
                    &dependency.source_identity,
                    &dependency.license,
                ]
                .iter()
                .any(|v| v.is_empty() || v.len() > 1024 || v.chars().any(char::is_control))
                || [&dependency.notice_file, &dependency.corresponding_source]
                    .iter()
                    .any(|v| !relative_name(v) || !self.files.iter().any(|f| f.name == **v))
            {
                return Err(DistributionFailure::ManifestRejected);
            }
        }
        Ok(())
    }
}

pub fn verify_installation_manifest(
    archive: &VerifiedServerArchive,
    manifest_bytes: &[u8],
) -> Result<VerifiedInstallationManifest, DistributionFailure> {
    use sha2::{Digest as _, Sha256};
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(DistributionFailure::LimitExceeded);
    }
    let digest: [u8; 32] = Sha256::digest(manifest_bytes).into();
    if digest != archive.document.archive.manifest_sha256 {
        return Err(DistributionFailure::ManifestRejected);
    }
    let document: InstallationManifestV1 =
        serde_json::from_slice(manifest_bytes).map_err(|_| DistributionFailure::InvalidRecord)?;
    document.validate()?;
    let feed = &archive.document;
    if document.component != feed.component
        || document.build_identity != feed.build_identity
        || document.platform != feed.platform
        || document.version.as_str() != feed.version.as_str()
        || document.protocol_majors != feed.protocol_majors
        || document.storage != feed.storage
    {
        return Err(DistributionFailure::ManifestRejected);
    }
    validate_archive(&archive.bytes, &document.files, manifest_bytes)?;
    Ok(VerifiedInstallationManifest { document })
}

/// Checks all expanded bytes without writing or executing archive contents.
/// GNU long names are bounded; PAX overrides and other special records are rejected.
pub(crate) fn validate_archive(
    bytes: &[u8],
    files: &[InstallationFile],
    manifest_bytes: &[u8],
) -> Result<(), DistributionFailure> {
    use sha2::{Digest as _, Sha256};
    use std::{
        collections::BTreeSet,
        io::{Cursor, Read},
    };
    let reject = DistributionFailure::ArchiveRejected;
    let decoder = flate2::read::MultiGzDecoder::new(Cursor::new(bytes));
    let mut reader = decoder.take(MAX_EXPANDED_BYTES + 1);
    let mut seen = BTreeSet::new();
    let mut long_name: Option<String> = None;
    let mut count = 0_usize;
    let mut entry_count = 0_usize;
    loop {
        let mut block = [0; 512];
        reader.read_exact(&mut block).map_err(|_| reject)?;
        if block == [0; 512] {
            if long_name.is_some() {
                return Err(reject);
            }
            let mut second = [0; 512];
            reader.read_exact(&mut second).map_err(|_| reject)?;
            if second != [0; 512] {
                return Err(reject);
            }
            let mut tail = [0; 8192];
            loop {
                let n = reader.read(&mut tail).map_err(|_| reject)?;
                if n == 0 {
                    break;
                }
                if tail[..n].iter().any(|b| *b != 0) {
                    return Err(reject);
                }
            }
            if reader.limit() == 0 {
                return Err(DistributionFailure::LimitExceeded);
            }
            break;
        }
        count += 1;
        if count > MAX_FILES * 2 + 2 {
            return Err(DistributionFailure::LimitExceeded);
        }
        let header = tar::Header::from_byte_slice(&block);
        let sum: u32 = block
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u32::from(*b)
                }
            })
            .sum();
        if header.cksum().map_err(|_| reject)? != sum {
            return Err(reject);
        }
        let size = header.size().map_err(|_| reject)?;
        if size > MAX_EXPANDED_BYTES || size > reader.limit() {
            return Err(DistributionFailure::LimitExceeded);
        }
        if header.entry_type().is_gnu_longname() {
            if long_name.is_some() || size == 0 || size > 1043 {
                return Err(reject);
            }
            let mut value = vec![0; size as usize];
            reader.read_exact(&mut value).map_err(|_| reject)?;
            if value.last() != Some(&0) {
                return Err(reject);
            }
            value.pop();
            long_name = Some(String::from_utf8(value).map_err(|_| reject)?);
        } else {
            entry_count += 1;
            if entry_count > MAX_FILES {
                return Err(DistributionFailure::LimitExceeded);
            }
            let path = header.path_bytes();
            let name = long_name
                .take()
                .unwrap_or(std::str::from_utf8(&path).map_err(|_| reject)?.to_owned());
            let mode = header.mode().map_err(|_| reject)?;
            if header.uid().map_err(|_| reject)? != 0
                || header.gid().map_err(|_| reject)? != 0
                || mode & !0o777 != 0
                || mode & 0o022 != 0
            {
                return Err(reject);
            }
            if header.entry_type().is_dir() {
                let name = name.strip_suffix('/').unwrap_or(&name);
                if !relative_name(name)
                    || size != 0
                    || !(name == "spaceterm-server"
                        || name
                            .strip_prefix("spaceterm-server/")
                            .is_some_and(|relative| {
                                let prefix = format!("{relative}/");
                                files.iter().any(|file| file.name.starts_with(&prefix))
                            }))
                    || !seen.insert(name.to_owned())
                {
                    return Err(reject);
                }
            } else if header.entry_type().is_file() {
                if !relative_name(&name) || !seen.insert(name.clone()) {
                    return Err(reject);
                }
                let name = name.strip_prefix("spaceterm-server/").ok_or(reject)?;
                if name == "installation.json" {
                    if size != manifest_bytes.len() as u64 || mode & 0o111 != 0 {
                        return Err(reject);
                    }
                    let mut value = vec![0; manifest_bytes.len()];
                    reader.read_exact(&mut value).map_err(|_| reject)?;
                    if value != manifest_bytes {
                        return Err(reject);
                    }
                } else {
                    let file = files
                        .binary_search_by(|f| f.name.as_str().cmp(name))
                        .ok()
                        .map(|i| &files[i])
                        .ok_or(reject)?;
                    if size != file.size || mode != file.mode {
                        return Err(reject);
                    }
                    let mut digest = Sha256::new();
                    let mut remaining = size;
                    let mut buffer = [0; 64 * 1024];
                    while remaining != 0 {
                        let n = remaining.min(buffer.len() as u64) as usize;
                        reader.read_exact(&mut buffer[..n]).map_err(|_| reject)?;
                        digest.update(&buffer[..n]);
                        remaining -= n as u64;
                    }
                    let actual: [u8; 32] = digest.finalize().into();
                    if actual != file.sha256 {
                        return Err(reject);
                    }
                }
            } else {
                return Err(reject);
            }
        }
        let padding = (512 - size % 512) % 512;
        let mut pad = [0; 512];
        reader
            .read_exact(&mut pad[..padding as usize])
            .map_err(|_| reject)?;
        if pad[..padding as usize].iter().any(|b| *b != 0) {
            return Err(reject);
        }
    }
    if !seen.contains("spaceterm-server/installation.json")
        || files
            .iter()
            .any(|f| !seen.contains(&format!("spaceterm-server/{}", f.name)))
    {
        return Err(reject);
    }
    Ok(())
}

/// The account service has one executable. It has no privileged helper or credential tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountEntryPoints {
    pub server: String,
}

/// Schema 2 installs into account-private storage, independently of client assets.
/// Schema 1 remains readable for retained server artifacts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountInstallationManifestV2 {
    pub schema_version: u16,
    pub component: ServerComponent,
    pub build_identity: BuildChannel,
    pub platform: ServerPlatform,
    pub version: BuildVersion,
    #[serde(with = "crate::encoding::commit")]
    pub build_commit: [u8; 20],
    pub protocol_majors: Vec<u16>,
    pub storage: StorageCompatibility,
    pub native_identifier: String,
    pub entry_points: AccountEntryPoints,
    pub files: Vec<InstallationFile>,
    pub dependencies: Vec<BundledDependency>,
}

impl AccountInstallationManifestV2 {
    /// Checks schema and layout, not signature authority.
    pub fn validate(&self) -> Result<(), DistributionFailure> {
        if self.schema_version != 2 {
            return Err(DistributionFailure::UnsupportedSchema);
        }
        let rejected = DistributionFailure::ManifestRejected;
        let server = match self.platform {
            ServerPlatform::LinuxX86_64 => "bin/spaceterm-server".to_owned(),
            ServerPlatform::DarwinArm64 => format!(
                "{}/Contents/MacOS/spaceterm-server",
                self.build_identity.bundle_name()
            ),
        };
        if self.native_identifier != self.build_identity.native_identifier()
            || self.entry_points.server != server
            || !crate::server_feed::sorted_nonzero(&self.protocol_majors, 8, false)
            || !self.storage.validate()
            || self.build_commit == [0; 20]
            || (self.build_identity == BuildChannel::Release
                && crate::version::stable_version(self.version.as_str()).is_none())
        {
            return Err(rejected);
        }
        if self.version.as_str().starts_with("dev.") {
            let commit: String = self
                .build_commit
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect();
            let expected = format!("dev.{}", &commit[..12]);
            if self.version.as_str() != expected
                && self.version.as_str() != format!("{expected}.dirty")
            {
                return Err(rejected);
            }
        }
        if self.files.is_empty() || self.files.len() > MAX_FILES || self.dependencies.len() > 128 {
            return Err(DistributionFailure::LimitExceeded);
        }
        if !self.files.windows(2).all(|p| p[0].name < p[1].name) {
            return Err(rejected);
        }
        let names: std::collections::BTreeSet<_> =
            self.files.iter().map(|f| f.name.as_str()).collect();
        let mut total = 0_u64;
        let mut entries = names.clone();
        for file in &self.files {
            if !relative_name(&file.name)
                || matches!(
                    file.name.as_str(),
                    "installation.json" | "feed.json" | "feed.sig"
                )
                || file.mode & !0o777 != 0
                || file.mode & 0o022 != 0
                || file.mode & 0o400 == 0
                || file.size > MAX_EXPANDED_BYTES
                || file.name.starts_with("libexec/")
                || file.name.split('/').count() > 32
                || file.name.split('/').any(|part| part.len() > 255)
            {
                return Err(rejected);
            }
            total = total
                .checked_add(file.size)
                .ok_or(DistributionFailure::LimitExceeded)?;
            let mut name = file.name.as_str();
            while let Some((parent, _)) = name.rsplit_once('/') {
                if names.contains(parent) {
                    return Err(rejected);
                }
                entries.insert(parent);
                name = parent;
            }
        }
        if entries.len() + 1 > MAX_FILES {
            return Err(DistributionFailure::LimitExceeded);
        }
        if total > MAX_EXPANDED_BYTES {
            return Err(DistributionFailure::LimitExceeded);
        }
        let executable = self
            .files
            .iter()
            .find(|f| f.name == server)
            .ok_or(rejected)?;
        if executable.size == 0 || executable.mode & 0o100 == 0 {
            return Err(rejected);
        }
        // The runtime owns these resources independently of any desktop install.
        let resources = match self.platform {
            ServerPlatform::LinuxX86_64 => "share/spaceterm-server".to_owned(),
            ServerPlatform::DarwinArm64 => {
                format!("{}/Contents/Resources", self.build_identity.bundle_name())
            }
        };
        for required in [
            "shell-integration/VERSION",
            "shell-integration/bash/spaceterm.bash",
            "shell-integration/sh/spaceterm.sh",
            "shell-integration/zsh/.zshenv",
            "shell-integration/zsh/spaceterm-integration",
            "THIRD-PARTY-NOTICES.txt",
        ] {
            let name = format!("{resources}/{required}");
            if !self.files.iter().any(|f| f.name == name && f.size != 0) {
                return Err(rejected);
            }
        }
        // ncurses may use the initial character or its hexadecimal encoding.
        if !["x", "78"].iter().any(|directory| {
            self.files.iter().any(|f| {
                f.name == format!("{resources}/terminfo/{directory}/xterm-spaceterm") && f.size != 0
            })
        }) {
            return Err(rejected);
        }
        let mut dependencies = std::collections::BTreeSet::new();
        for dependency in &self.dependencies {
            if !dependencies.insert(&dependency.name)
                || [
                    &dependency.name,
                    &dependency.version,
                    &dependency.source_identity,
                    &dependency.license,
                ]
                .iter()
                .any(|v| v.is_empty() || v.len() > 1024 || v.chars().any(char::is_control))
                || [&dependency.notice_file, &dependency.corresponding_source]
                    .iter()
                    .any(|v| !relative_name(v) || !names.contains(v.as_str()))
            {
                return Err(rejected);
            }
        }
        Ok(())
    }
}

/// The signed feed authenticates the manifest digest before decoding installed metadata.
pub fn verify_account_installation_manifest(
    feed: &crate::server_feed::VerifiedAccountServerFeed,
    bytes: &[u8],
) -> Result<AccountInstallationManifestV2, DistributionFailure> {
    use sha2::{Digest as _, Sha256};
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(DistributionFailure::LimitExceeded);
    }
    if <[u8; 32]>::from(Sha256::digest(bytes)) != feed.document().archive.manifest_sha256 {
        return Err(DistributionFailure::ManifestRejected);
    }
    let document: AccountInstallationManifestV2 =
        serde_json::from_slice(bytes).map_err(|_| DistributionFailure::InvalidRecord)?;
    document.validate()?;
    let expected = feed.document();
    if document.build_identity != expected.build_identity
        || document.platform != expected.platform
        || document.version != expected.version
        || document.protocol_majors != expected.protocol_majors
        || document.storage != expected.storage
        || document.component != expected.component
    {
        return Err(DistributionFailure::ManifestRejected);
    }
    Ok(document)
}
