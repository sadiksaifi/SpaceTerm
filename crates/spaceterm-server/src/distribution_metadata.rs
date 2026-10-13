//! Content-free, bounded publisher metadata. Native installers never execute candidates as root.
use serde::Serialize;
use spaceterm_distribution::{
    BuildChannel, BuildVersion, DistributionFailure, ServerComponent, ServerPlatform,
    StorageCompatibility,
};

#[derive(Serialize)]
pub struct ServerBuildMetadata {
    pub schema_version: u16,
    pub component: ServerComponent,
    pub build_identity: BuildChannel,
    pub platform: ServerPlatform,
    pub version: BuildVersion,
    pub build_commit: String,
    pub protocol_majors: Vec<u16>,
    pub storage: StorageCompatibility,
    pub native_identifier: &'static str,
}

/// Owning protocol and store modules supply their implemented compatibility values.
/// The caller routes `--distribution-metadata` here before ordinary startup composition.
pub fn encode(
    protocol_majors: Vec<u16>,
    storage: StorageCompatibility,
) -> Result<Vec<u8>, DistributionFailure> {
    let build_identity = match option_env!("SPACETERM_SERVER_BUILD") {
        Some("release") => BuildChannel::Release,
        Some("preflight") => BuildChannel::Preflight,
        Some("development") => BuildChannel::Development,
        _ => return Err(DistributionFailure::IdentityMismatch),
    };
    let platform = match option_env!("SPACETERM_SERVER_PLATFORM") {
        Some("darwin-arm64") => ServerPlatform::DarwinArm64,
        Some("linux-x86_64") => ServerPlatform::LinuxX86_64,
        _ => return Err(DistributionFailure::IdentityMismatch),
    };
    let version = BuildVersion::parse(
        option_env!("SPACETERM_SERVER_VERSION").ok_or(DistributionFailure::InvalidRecord)?,
    )?;
    let build_commit =
        option_env!("SPACETERM_SERVER_COMMIT").ok_or(DistributionFailure::InvalidRecord)?;
    if build_commit.len() != 40
        || !build_commit
            .bytes()
            .all(|b| matches!(b,b'0'..=b'9'|b'a'..=b'f'))
        || protocol_majors.is_empty()
        || protocol_majors.len() > 8
        || protocol_majors.contains(&0)
        || !protocol_majors.windows(2).all(|p| p[0] < p[1])
        || storage.read.is_empty()
        || storage.read.len() > 16
        || storage.migrate_from.len() > 16
        || !storage.read.contains(&storage.write)
        || [storage.read.as_slice(), storage.migrate_from.as_slice()]
            .iter()
            .any(|list| list.contains(&0) || !list.windows(2).all(|p| p[0] < p[1]))
    {
        return Err(DistributionFailure::InvalidRecord);
    }
    let metadata = ServerBuildMetadata {
        schema_version: 2,
        component: ServerComponent::Server,
        build_identity,
        platform,
        version,
        build_commit: build_commit.to_owned(),
        protocol_majors,
        storage,
        native_identifier: build_identity.native_identifier(),
    };
    let bytes = serde_json::to_vec(&metadata).map_err(|_| DistributionFailure::InvalidRecord)?;
    if bytes.len() > 4096 {
        return Err(DistributionFailure::LimitExceeded);
    }
    Ok(bytes)
}
