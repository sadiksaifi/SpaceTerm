//! Confirmed setup uses authenticated artifacts and the host's retained request.
//! Input file names are explicit local authority on this executable entry point.
//! Neither feed contents nor an ordinary remote request can select local roots.

use serde::Deserialize;
use spaceterm_distribution::{
    BuildChannel, ServerArchiveVerifier, ServerPlatform, UpdateKey,
    server_feed::{MAX_ARCHIVE_BYTES, MAX_FEED_BYTES, MAX_FEED_SIGNATURE_BYTES},
    verify_account_server_feed,
};
use spaceterm_host::{
    account_service::{
        ConfirmedServiceInstall, ServiceFailure, ServiceProgress, ServiceSupervisor,
    },
    native_deployment::contracts::NativeRequestId,
};
use std::{
    fs::OpenOptions,
    io::Read as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    pub request: NativeRequestId,
    pub feed: PathBuf,
    pub signature: PathBuf,
    pub archive: PathBuf,
}

/// The caller holds local confirmation and the original provider-issued request.
/// Unknown, retired or reused IDs remain host errors. There is no replacement ID.
pub async fn account_setup(
    supervisor: &ServiceSupervisor,
    request: NativeRequestId,
    confirmation: ConfirmedServiceInstall,
) -> Result<ServiceProgress, ServiceFailure> {
    supervisor.install(request, confirmation).await
}

/// Authentication covers exact feed bytes before any archive is interpreted.
/// Test trust roots cannot enter this constructor.
pub fn prepare_confirmed_setup(
    supervisor: &ServiceSupervisor,
    request: &SetupRequest,
) -> Result<ConfirmedServiceInstall, ServiceFailure> {
    let key = UpdateKey::release().ok_or(ServiceFailure::ArtifactRejected)?;
    let build = match supervisor.key().build_identity {
        spaceterm_host::startup::BuildIdentity::Release => BuildChannel::Release,
        spaceterm_host::startup::BuildIdentity::Preflight => BuildChannel::Preflight,
        spaceterm_host::startup::BuildIdentity::Development => BuildChannel::Development,
    };
    let platform = match option_env!("SPACETERM_SERVER_PLATFORM") {
        Some("darwin-arm64") => ServerPlatform::DarwinArm64,
        Some("linux-x86_64") => ServerPlatform::LinuxX86_64,
        _ => return Err(ServiceFailure::UnsupportedHost),
    };
    let feed = read_bounded(&request.feed, MAX_FEED_BYTES as u64)?;
    let signature = read_bounded(&request.signature, MAX_FEED_SIGNATURE_BYTES as u64)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ServiceFailure::SupervisorUnavailable)?
        .as_secs();
    let feed = verify_account_server_feed(&key, build, platform, &feed, &signature, now)
        .map_err(|_| ServiceFailure::ArtifactRejected)?;
    let bytes = read_bounded(&request.archive, MAX_ARCHIVE_BYTES)?;
    let artifact = ServerArchiveVerifier::new()
        .verify_account_owned(&key, feed, bytes)
        .map_err(|_| ServiceFailure::ArtifactRejected)?;
    supervisor
        .assess_install(artifact)
        .map(|assessment| assessment.confirm())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ServiceFailure> {
    if !path.is_absolute() {
        return Err(ServiceFailure::InvalidRecord);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ServiceFailure::ArtifactRejected)?;
    let metadata = file
        .metadata()
        .map_err(|_| ServiceFailure::ArtifactRejected)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(ServiceFailure::ArtifactRejected);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ServiceFailure::ArtifactRejected)?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(ServiceFailure::ArtifactRejected);
    }
    Ok(bytes)
}
