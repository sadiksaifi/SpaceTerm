//! Desktop provisioning of the shared headless client journal. Pairing and
//! operation tracking use this same database, outside the Settings Document.

use super::{
    app_directories::AppDirectoryRoot, app_paths::AppPaths,
    secure_filesystem::SecureFilesystemError,
};
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalController, JournalLimits, ReceiptVerifier},
};
use spaceterm_client_native::journal::NativeClientJournal;
use spaceterm_core::operation_ledger_contracts::LedgerCodec;
use std::{ffi::OsStr, sync::Arc};

#[derive(Clone, Copy)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "desktop C3 installs the native journal after the pairing/codec handoff"
    )
)]
pub(crate) enum JournalOpenMode {
    Initialize,
    Existing,
}

/// Only identity-scoped AppPaths establish this local path. Saved routes and
/// server-supplied paths cannot provision client storage. Existing mode never
/// creates a missing directory or database. SQLite work runs off the GUI thread.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "desktop C3 installs the native journal after the pairing/codec handoff"
    )
)]
pub(crate) async fn open(
    paths: &AppPaths,
    mode: JournalOpenMode,
    codec: Arc<dyn LedgerCodec>,
    verifier: Arc<dyn ReceiptVerifier>,
    limits: JournalLimits,
) -> Result<Arc<JournalController>, ClientFailure> {
    let filesystem = Arc::clone(paths.filesystem());
    let state = paths
        .directories()
        .root(AppDirectoryRoot::State)
        .to_path_buf();
    let connections = state.join("client-connections");
    let desktop = connections.join("desktop");
    let provision = Arc::clone(&filesystem);
    let directory = desktop.clone();
    let (parent, child) = tokio::task::spawn_blocking(move || match mode {
        JournalOpenMode::Initialize => {
            provision
                .ensure_private_directory(&state)
                .map_err(classify)?;
            let parent = provision
                .ensure_private_directory(&connections)
                .map_err(classify)?;
            let child = provision
                .create_private_child(&parent, OsStr::new("desktop"))
                .map_err(classify)?;
            Ok::<_, ClientFailure>((parent, child))
        }
        JournalOpenMode::Existing => {
            let parent = provision
                .open_private_directory(&connections)
                .map_err(classify)?
                .ok_or_else(|| ClientFailure::new(ClientFailureClass::StorageUnavailable))?;
            let child = provision
                .open_private_directory(&directory)
                .map_err(classify)?
                .ok_or_else(|| ClientFailure::new(ClientFailureClass::StorageUnavailable))?;
            Ok::<_, ClientFailure>((parent, child))
        }
    })
    .await
    .map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))??;
    let journal = match mode {
        JournalOpenMode::Initialize => {
            NativeClientJournal::create(desktop, Arc::clone(&codec), Arc::clone(&verifier), limits)
                .await?
        }
        JournalOpenMode::Existing => {
            NativeClientJournal::open_existing(
                desktop,
                Arc::clone(&codec),
                Arc::clone(&verifier),
                limits,
            )
            .await?
        }
    };
    tokio::task::spawn_blocking(move || {
        filesystem.verify_directory(&parent).map_err(classify)?;
        filesystem.verify_directory(&child).map_err(classify)
    })
    .await
    .map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))??;
    Ok(Arc::new(
        JournalController::recover(Arc::new(journal), codec, verifier, limits).await?,
    ))
}
fn classify(error: SecureFilesystemError) -> ClientFailure {
    ClientFailure::new(match error {
        SecureFilesystemError::Unsafe
        | SecureFilesystemError::Conflict
        | SecureFilesystemError::AlreadyExists => ClientFailureClass::UnsafeStorage,
        _ => ClientFailureClass::StorageUnavailable,
    })
}

#[cfg(test)]
mod tests;

/// Uses the operation controller and its existing fence and scheduler claim.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "desktop composition consumes the canonical pairing facade"
    )
)]
pub(crate) fn pairing_store(
    journal: Arc<JournalController>,
) -> spaceterm_client::pairing::journal::JournalPairingStore<JournalController> {
    spaceterm_client::pairing::journal::JournalPairingStore::new(journal)
}
