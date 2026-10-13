//! Identity-checked personal view publication, separate from client preferences.
//!
//! The client-view codec and `ViewPersistence` own document validation and recovery. This adapter
//! owns only private filesystem publication. Callers run it on a background executor, never GPUI.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "native desktop client composition consumes this store after its provider handoff"
    )
)]

use std::{ffi::OsStr, sync::Arc};

use spaceterm_client::view::{
    EncodedView, StoredView, ViewCommit, ViewStore, ViewStoreFailure, ViewStoreFailureClass,
    ViewStoreStage,
};

use super::{
    app_directories::AppDirectoryRoot,
    app_paths::AppPaths,
    secure_filesystem::{
        SecureCommitOutcome, SecureDirectory, SecureEntryIdentity, SecureFilesystemError,
    },
};

const DOCUMENT_NAME: &str = "client-view.bin";

pub(crate) struct NativeViewStore {
    paths: Arc<AppPaths>,
    directory: Option<SecureDirectory>,
    durable_directory: bool,
}

impl NativeViewStore {
    pub(crate) fn new(paths: Arc<AppPaths>) -> Self {
        Self {
            paths,
            directory: None,
            durable_directory: false,
        }
    }

    fn directory(
        &mut self,
        create: bool,
        stage: ViewStoreStage,
    ) -> Result<Option<SecureDirectory>, ViewStoreFailure> {
        let filesystem = self.paths.filesystem();
        if let Some(directory) = &self.directory {
            filesystem
                .verify_directory(directory)
                .map_err(|error| failure(error, stage))?;
            if !create || self.durable_directory {
                return Ok(Some(directory.clone()));
            }
        }
        let path = self.paths.directories().root(AppDirectoryRoot::State);
        let directory = if create {
            Some(
                filesystem
                    .ensure_durable_private_directory(path)
                    .map_err(|error| failure(error, stage))?,
            )
        } else {
            filesystem
                .open_private_directory(path)
                .map_err(|error| failure(error, stage))?
        };
        self.durable_directory = create;
        self.directory = directory.clone();
        Ok(directory)
    }
}

impl ViewStore for NativeViewStore {
    type Version = SecureEntryIdentity;

    fn load(
        &mut self,
        maximum_bytes: usize,
    ) -> Result<Option<StoredView<Self::Version>>, ViewStoreFailure> {
        let Some(directory) = self.directory(false, ViewStoreStage::Load)? else {
            return Ok(None);
        };
        let snapshot = self
            .paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(DOCUMENT_NAME), maximum_bytes)
            .map_err(|error| failure(error, ViewStoreStage::Load))?;
        snapshot
            .map(|snapshot| {
                if snapshot.bytes.len() > maximum_bytes {
                    return Err(failure(
                        SecureFilesystemError::TooLarge,
                        ViewStoreStage::Load,
                    ));
                }
                Ok(StoredView {
                    version: snapshot.identity,
                    bytes: snapshot.bytes,
                })
            })
            .transpose()
    }

    fn replace(
        &mut self,
        expected: Option<&Self::Version>,
        document: &EncodedView,
    ) -> ViewCommit<Self::Version> {
        let directory = match self.directory(true, ViewStoreStage::Write) {
            Ok(Some(directory)) => directory,
            Ok(None) => {
                return ViewCommit::NotApplied(failure(
                    SecureFilesystemError::Unavailable,
                    ViewStoreStage::Write,
                ));
            }
            Err(failure) => return ViewCommit::NotApplied(failure),
        };
        let mut nonce = [0; 16];
        if getrandom::fill(&mut nonce).is_err() {
            return ViewCommit::NotApplied(failure(
                SecureFilesystemError::Unavailable,
                ViewStoreStage::Write,
            ));
        }
        let filesystem = self.paths.filesystem();
        let prepared = match filesystem.prepare_private_file(
            &directory,
            OsStr::new(DOCUMENT_NAME),
            document.as_bytes(),
            nonce,
        ) {
            Ok(prepared) => prepared,
            Err(error) => return ViewCommit::NotApplied(failure(error, ViewStoreStage::Write)),
        };
        match filesystem.commit_private_file(prepared, expected) {
            Ok(result) => match result.outcome {
                SecureCommitOutcome::Committed => match result.published_identity {
                    Some(version) => ViewCommit::Durable(version),
                    None => ViewCommit::Indeterminate,
                },
                SecureCommitOutcome::Conflict => ViewCommit::NotApplied(failure(
                    SecureFilesystemError::Conflict,
                    ViewStoreStage::Publish,
                )),
                SecureCommitOutcome::CommittedButUnsynced => ViewCommit::Indeterminate,
            },
            // A native publication error can follow a rename and a failed rollback. A later
            // successful sync cannot establish which document survived. Recover before saving.
            Err(_) => ViewCommit::Indeterminate,
        }
    }
}

fn failure(error: SecureFilesystemError, stage: ViewStoreStage) -> ViewStoreFailure {
    ViewStoreFailure {
        class: match error {
            SecureFilesystemError::Conflict | SecureFilesystemError::AlreadyExists => {
                ViewStoreFailureClass::Conflict
            }
            SecureFilesystemError::TooLarge => ViewStoreFailureClass::TooLarge,
            SecureFilesystemError::Unsafe => ViewStoreFailureClass::Unsafe,
            SecureFilesystemError::Missing | SecureFilesystemError::Unavailable => {
                ViewStoreFailureClass::Unavailable
            }
        },
        stage,
    }
}

#[cfg(test)]
mod tests;
