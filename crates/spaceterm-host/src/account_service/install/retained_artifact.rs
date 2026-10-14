//! Exact signed bytes retained before accepting a stage request. Recovery verifies
//! them again; cached bytes never grant installation or activation authority.

use super::{ServiceFailure, ServiceSupervisor, VerifiedServerArtifact, classify_storage};
use crate::{
    native_deployment::contracts::NativeRequestId,
    private_filesystem::Directory,
    storage::{CommitDisposition, Database, InitialCreation, Migration, StorageFailure},
};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::{
    BuildChannel, ServerArchiveVerifier, ServerPlatform, UpdateKey,
    server_feed::{MAX_ARCHIVE_BYTES, MAX_FEED_BYTES, MAX_FEED_SIGNATURE_BYTES},
    verify_account_server_feed,
};

// Hold this through archive reading, verification and redelivery. Concurrent
// resolvers cannot each allocate the maximum archive for the same request.
pub(super) static RECOVERY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type SignedBytes = (Vec<u8>, Vec<u8>, Vec<u8>);

const APPLICATION_ID: u32 = 0x53545341;
const MIGRATIONS: &[Migration<'_>] = &[Migration {
    sql: "CREATE TABLE artifacts(\
        request BLOB PRIMARY KEY CHECK(length(request)=16),\
        feed BLOB NOT NULL CHECK(length(feed) BETWEEN 1 AND 4096),\
        signature BLOB NOT NULL CHECK(length(signature) BETWEEN 1 AND 256),\
        archive BLOB NOT NULL CHECK(length(archive) BETWEEN 1 AND 536870912)\
    ) STRICT, WITHOUT ROWID",
}];

fn database(
    supervisor: &ServiceSupervisor,
    create: bool,
) -> Result<Option<Database>, ServiceFailure> {
    let path = supervisor.anchor.join("stage-artifacts");
    if create {
        let directory = Directory::create(&path)?;
        let mut binding = Sha256::new();
        binding.update(b"spaceterm-stage-artifacts-creation-v1\0");
        binding.update(
            serde_json::to_vec(&supervisor.key).map_err(|_| ServiceFailure::InvalidRecord)?,
        );
        // Cache retention precedes request acceptance. Retain the existing
        // creation authority in this cache directory, bound to its directory and
        // database inodes, rather than adopting an unowned empty database.
        // The cache belongs to the account/build, not to one staging request.
        let creation = InitialCreation::new(directory, binding.finalize().into());
        let database =
            Database::open_initial_for_application(&path, MIGRATIONS, APPLICATION_ID, &creation)
                .map_err(classify_storage)?;
        creation.complete().map_err(classify_storage)?;
        return Ok(Some(database));
    }
    if Directory::open(&path)?.is_none() {
        return Ok(None);
    }
    Database::open_existing_for_application(&path, MIGRATIONS, APPLICATION_ID)
        .map(Some)
        .map_err(classify_storage)
}
fn durable(result: CommitDisposition) -> Result<(), ServiceFailure> {
    match result {
        CommitDisposition::Durable(()) => Ok(()),
        CommitDisposition::NotApplied(failure) => Err(classify_storage(failure)),
        CommitDisposition::Indeterminate => Err(ServiceFailure::Indeterminate),
    }
}
fn trust(supervisor: &ServiceSupervisor) -> Result<UpdateKey, ServiceFailure> {
    #[cfg(feature = "test-support")]
    if let Some(key) = &supervisor.test_installation_trust {
        return Ok(key.clone());
    }
    #[cfg(not(feature = "test-support"))]
    let _ = supervisor;
    UpdateKey::release().ok_or(ServiceFailure::ArtifactRejected)
}

pub(super) fn retain(
    supervisor: &ServiceSupervisor,
    request: NativeRequestId,
    artifact: &VerifiedServerArtifact,
) -> Result<(), ServiceFailure> {
    if !artifact.feed().authenticated_by(&trust(supervisor)?) {
        return Err(ServiceFailure::ArtifactRejected);
    }
    let mut database = database(supervisor, true)?.ok_or(ServiceFailure::StorageUnavailable)?;
    // The caller holds the lifecycle lease and has proved no other accepted
    // request is pending. Only this request needs an archive, so abandoned
    // unsubmitted caches and completed requests cannot accumulate disk usage.
    durable(database.transaction(|tx| {
        tx.execute("DELETE FROM artifacts WHERE request<>?1", [request.as_bytes().as_slice()])?;
        tx.execute(
            "INSERT OR IGNORE INTO artifacts(request,feed,signature,archive) VALUES(?1,?2,?3,?4)",
            (request.as_bytes().as_slice(), artifact.feed().bytes(), artifact.feed().signature(), artifact.bytes()),
        )?;
        let exact: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifacts WHERE request=?1 AND feed=?2 AND signature=?3 AND archive=?4)",
            (request.as_bytes().as_slice(), artifact.feed().bytes(), artifact.feed().signature(), artifact.bytes()),
            |row| row.get(0),
        )?;
        if !exact { return Err(StorageFailure::InvalidState); }
        Ok(())
    }))
}

pub(super) fn load(
    supervisor: &ServiceSupervisor,
    request: NativeRequestId,
) -> Result<Option<VerifiedServerArtifact>, ServiceFailure> {
    let Some(database) = database(supervisor, false)? else {
        return Ok(None);
    };
    let bytes: Option<SignedBytes> = database
        .query_row(
            "SELECT (SELECT feed FROM artifacts WHERE request=?1),\
             (SELECT signature FROM artifacts WHERE request=?1),\
             (SELECT archive FROM artifacts WHERE request=?1)",
            [request.as_bytes().as_slice()],
            |row| {
                let feed: Option<Vec<u8>> = row.get(0)?;
                let signature: Option<Vec<u8>> = row.get(1)?;
                let archive: Option<Vec<u8>> = row.get(2)?;
                match (feed, signature, archive) {
                    (Some(feed), Some(signature), Some(archive)) => {
                        Ok(Some((feed, signature, archive)))
                    }
                    (None, None, None) => Ok(None),
                    _ => Err(rusqlite::Error::InvalidQuery),
                }
            },
        )
        .map_err(classify_storage)?;
    let Some((feed, signature, archive)) = bytes else {
        return Ok(None);
    };
    drop(database);
    if feed.len() > MAX_FEED_BYTES
        || signature.len() > MAX_FEED_SIGNATURE_BYTES
        || archive.len() as u64 > MAX_ARCHIVE_BYTES
    {
        return Err(ServiceFailure::LimitExceeded);
    }
    let build = match supervisor.key.build_identity {
        crate::startup::BuildIdentity::Release => BuildChannel::Release,
        crate::startup::BuildIdentity::Preflight => BuildChannel::Preflight,
        crate::startup::BuildIdentity::Development => BuildChannel::Development,
    };
    let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ServerPlatform::DarwinArm64
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ServerPlatform::LinuxX86_64
    } else {
        return Err(ServiceFailure::UnsupportedHost);
    };
    let key = trust(supervisor)?;
    let feed = verify_account_server_feed(&key, build, platform, &feed, &signature, u64::MAX)
        .map_err(|_| ServiceFailure::ArtifactRejected)?;
    ServerArchiveVerifier::new()
        .verify_account_owned(&key, feed, archive)
        .map(Some)
        .map_err(|_| ServiceFailure::ArtifactRejected)
}

pub(super) fn clear(
    supervisor: &ServiceSupervisor,
    request: NativeRequestId,
) -> Result<(), ServiceFailure> {
    let Some(mut database) = database(supervisor, false)? else {
        return Ok(());
    };
    durable(database.transaction(|tx| {
        tx.execute(
            "DELETE FROM artifacts WHERE request=?1",
            [request.as_bytes().as_slice()],
        )?;
        Ok(())
    }))
}
