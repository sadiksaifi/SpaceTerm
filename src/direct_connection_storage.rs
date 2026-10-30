//! Desktop filesystem authority for Saved Connections.
use crate::platform::{
    app_directories::AppDirectoryRoot,
    app_paths::AppPaths,
    secure_filesystem::{SecureCommitOutcome, SecureEntryIdentity},
};
use spaceterm_connections::records::{Durability, RecordFile, SaveError};
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
const FILE: &str = "direct-device-connections.json";
const MAX_BYTES: usize = 512 * 1024;
#[derive(Default)]
struct WriteAuthority {
    identity: Option<SecureEntryIdentity>,
    /// A commit error may follow publication or a failed rollback. Only an explicit
    /// load can establish a new observed snapshot; retries must not guess its identity.
    blocked: bool,
}

pub(crate) struct DesktopRecordFile {
    paths: Arc<AppPaths>,
    expected: Mutex<WriteAuthority>,
}

impl DesktopRecordFile {
    pub(crate) fn new(paths: Arc<AppPaths>) -> Self {
        Self {
            paths,
            expected: Mutex::default(),
        }
    }
}

impl RecordFile for DesktopRecordFile {
    fn read(
        &self,
        accept: &mut dyn FnMut(&[u8]) -> Result<(), spaceterm_server::direct::DirectError>,
    ) -> Result<(), spaceterm_server::direct::DirectError> {
        let mut expected = self
            .expected
            .lock()
            .map_err(|_| spaceterm_server::direct::DirectError::StorageUnavailable)?;
        expected.blocked = true;
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| spaceterm_server::direct::DirectError::StorageUnavailable)?
        else {
            *expected = WriteAuthority::default();
            return Ok(());
        };
        let snapshot = self
            .paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(FILE), MAX_BYTES)
            .map_err(|_| spaceterm_server::direct::DirectError::StorageUnavailable)?;
        let Some(snapshot) = snapshot else {
            *expected = WriteAuthority::default();
            return Ok(());
        };
        accept(&snapshot.bytes)?;
        *expected = WriteAuthority {
            identity: Some(snapshot.identity),
            blocked: false,
        };
        Ok(())
    }

    fn commit(&self, bytes: &[u8]) -> Result<Durability, SaveError> {
        let mut expected = self.expected.lock().map_err(|_| SaveError::NotPublished)?;
        if expected.blocked {
            return Err(SaveError::PublicationIndeterminate);
        }
        let directory = self
            .paths
            .ensure_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| SaveError::NotPublished)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| SaveError::NotPublished)?;
        let prepared = self
            .paths
            .filesystem()
            .prepare_private_file(&directory, OsStr::new(FILE), bytes, nonce)
            .map_err(|_| SaveError::NotPublished)?;
        let result = match self
            .paths
            .filesystem()
            .commit_private_file(prepared, expected.identity.as_ref())
        {
            Ok(result) => result,
            Err(_) => {
                expected.blocked = true;
                return Err(SaveError::PublicationIndeterminate);
            }
        };
        let durability = match result.outcome {
            SecureCommitOutcome::Conflict => return Err(SaveError::NotPublished),
            SecureCommitOutcome::Committed => Durability::Synchronized,
            SecureCommitOutcome::CommittedButUnsynced => Durability::Uncertain,
        };
        expected.blocked = result.published_identity.is_none();
        expected.identity = result.published_identity;
        Ok(durability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::app_directories::AppDirectoryEnvironment;
    use crate::platform::testing::{RecordingFilesystem, resolve_app_paths};
    use spaceterm_connections::records::{Document, PrivateRecords, Records};

    fn storage() -> (PrivateRecords, Arc<RecordingFilesystem>) {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = resolve_app_paths(
            &AppDirectoryEnvironment {
                home: Some("/home/test".into()),
                ..Default::default()
            },
            Some("/runtime".into()),
            200,
            filesystem.clone(),
        )
        .unwrap();
        (
            PrivateRecords::new(Arc::new(DesktopRecordFile::new(Arc::new(paths)))),
            filesystem,
        )
    }

    #[test]
    fn an_uncertain_publication_retains_its_identity_for_the_next_synchronized_write() {
        let (records, filesystem) = storage();
        let mut document = Document::default();
        assert_eq!(records.save(&document), Ok(Durability::Synchronized));
        document
            .routes
            .push(spaceterm_connections::tests::saved(1, 5));
        filesystem.files.lock().unwrap().commit_outcome =
            Some(SecureCommitOutcome::CommittedButUnsynced);

        assert_eq!(records.save(&document), Ok(Durability::Uncertain));

        filesystem.files.lock().unwrap().commit_outcome = None;
        document.routes[0].route.label = "Saved again".to_owned();
        assert_eq!(records.save(&document), Ok(Durability::Synchronized));
        assert_eq!(records.load().unwrap().routes[0].route.label, "Saved again");
    }

    #[test]
    fn an_uncertain_publication_never_captures_a_competing_successors_identity() {
        let (records, filesystem) = storage();
        let mut document = Document::default();
        document
            .routes
            .push(spaceterm_connections::tests::saved(1, 5));
        {
            let mut files = filesystem.files.lock().unwrap();
            files.commit_outcome = Some(SecureCommitOutcome::CommittedButUnsynced);
            files.successor_after_commit = Some(serde_json::to_vec(&document).unwrap());
        }
        assert_eq!(records.save(&document), Ok(Durability::Uncertain));
        filesystem.files.lock().unwrap().commit_outcome = None;
        document.routes[0].route.label = "Must not replace the successor".to_owned();

        assert_eq!(records.save(&document), Err(SaveError::NotPublished));
        assert_eq!(records.load().unwrap().routes[0].route.label, "Server 1");
    }
    #[test]
    fn a_failed_load_never_grants_write_authority_over_malformed_saved_connections() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = resolve_app_paths(
            &AppDirectoryEnvironment {
                home: Some("/home/test".into()),
                ..Default::default()
            },
            Some("/runtime".into()),
            200,
            filesystem,
        )
        .unwrap();
        let file = Arc::new(DesktopRecordFile::new(Arc::new(paths)));
        file.commit(b"invalid saved connections").unwrap();
        let records = PrivateRecords::new(file);
        assert!(records.load().is_err());
        assert_eq!(
            records.save(&Document::default()),
            Err(SaveError::PublicationIndeterminate)
        );
    }
}
