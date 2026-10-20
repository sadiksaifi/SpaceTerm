//! Private connection intent, separate from shipped Settings and SSH configuration.
use std::{collections::BTreeSet, ffi::OsStr, sync::Arc};

use serde::{Deserialize, Serialize};

use super::server_connection::{
    RetainedSshConnection, SshConnectionIntentStore, SshIntentStoreError, SshServerContinuity,
};
use crate::{
    domain::SshDestination,
    platform::{
        app_directories::AppDirectoryRoot,
        app_paths::AppPaths,
        secure_filesystem::{PrivateFileSnapshot, SecureCommitOutcome, SecureFilesystemError},
    },
};

const FILE: &str = "ssh-server-connections.json";
const MAX_CONNECTIONS: usize = 128;
const MAX_BYTES: usize = 128 * 1024;

/// Construction is inert. Only explicit registry operations read or retain connection intent.
pub(crate) struct PrivateSshConnectionIntents {
    paths: Arc<AppPaths>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u8,
    connections: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    destination: String,
    continuity: Option<SshServerContinuity>,
    active: bool,
}

impl PrivateSshConnectionIntents {
    pub(crate) fn new(paths: Arc<AppPaths>) -> Self {
        Self { paths }
    }

    fn snapshot(&self) -> Result<Option<PrivateFileSnapshot>, SshIntentStoreError> {
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| SshIntentStoreError::Unavailable)?
        else {
            return Ok(None);
        };
        self.paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(FILE), MAX_BYTES)
            .map_err(|_| SshIntentStoreError::Unavailable)
    }

    fn decode(
        snapshot: Option<&PrivateFileSnapshot>,
    ) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
        let Some(snapshot) = snapshot else {
            return Ok(Vec::new());
        };
        if snapshot.bytes.len() > MAX_BYTES {
            return Err(SshIntentStoreError::Malformed);
        }
        let document: Document =
            serde_json::from_slice(&snapshot.bytes).map_err(|_| SshIntentStoreError::Malformed)?;
        if document.schema != 1 || document.connections.len() > MAX_CONNECTIONS {
            return Err(SshIntentStoreError::Malformed);
        }
        let mut destinations = BTreeSet::new();
        document
            .connections
            .into_iter()
            .map(|record| {
                if !destinations.insert(record.destination.clone()) {
                    return Err(SshIntentStoreError::Malformed);
                }
                let destination = SshDestination::new(record.destination)
                    .map_err(|_| SshIntentStoreError::Malformed)?;
                let connection = RetainedSshConnection {
                    destination,
                    continuity: record.continuity,
                    active: record.active,
                };
                Self::validate(&connection)?;
                Ok(connection)
            })
            .collect()
    }

    fn validate(connection: &RetainedSshConnection) -> Result<(), SshIntentStoreError> {
        if connection.destination.as_str().len() > 255 {
            return Err(SshIntentStoreError::Malformed);
        }
        if let Some(pin) = &connection.continuity
            && (pin.route == [0; 32]
                || pin.server == [0; 32]
                || pin.account == 0
                || pin.build.is_empty()
                || pin.build.len() > 128
                || !pin
                    .build
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        {
            return Err(SshIntentStoreError::Malformed);
        }
        Ok(())
    }

    fn update(
        &self,
        mut change: impl FnMut(&mut Vec<RetainedSshConnection>),
    ) -> Result<(), SshIntentStoreError> {
        for _ in 0..8 {
            let snapshot = self.snapshot()?;
            let mut connections = Self::decode(snapshot.as_ref())?;
            change(&mut connections);
            if connections.len() > MAX_CONNECTIONS {
                return Err(SshIntentStoreError::Unavailable);
            }
            connections.sort_by(|a, b| a.destination.cmp(&b.destination));
            let document = Document {
                schema: 1,
                connections: connections
                    .into_iter()
                    .map(|record| Record {
                        destination: record.destination.as_str().to_owned(),
                        continuity: record.continuity,
                        active: record.active,
                    })
                    .collect(),
            };
            let bytes =
                serde_json::to_vec(&document).map_err(|_| SshIntentStoreError::Unavailable)?;
            if bytes.len() > MAX_BYTES {
                return Err(SshIntentStoreError::Unavailable);
            }
            let directory = self
                .paths
                .ensure_secure_root(AppDirectoryRoot::Data)
                .map_err(|_| SshIntentStoreError::Unavailable)?;
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| SshIntentStoreError::Unavailable)?;
            let prepared = match self.paths.filesystem().prepare_private_file(
                &directory,
                OsStr::new(FILE),
                &bytes,
                nonce,
            ) {
                Ok(prepared) => prepared,
                Err(SecureFilesystemError::AlreadyExists) => continue,
                Err(_) => return Err(SshIntentStoreError::Unavailable),
            };
            let result = self
                .paths
                .filesystem()
                .commit_private_file(prepared, snapshot.as_ref().map(|value| &value.identity))
                .map_err(|_| SshIntentStoreError::Unavailable)?;
            match result.outcome {
                SecureCommitOutcome::Committed => return Ok(()),
                SecureCommitOutcome::Conflict => continue,
                SecureCommitOutcome::CommittedButUnsynced => {
                    return Err(SshIntentStoreError::Unavailable);
                }
            }
        }
        Err(SshIntentStoreError::Unavailable)
    }
}

impl SshConnectionIntentStore for PrivateSshConnectionIntents {
    fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
        Self::decode(self.snapshot()?.as_ref())
    }

    fn retain(&self, connection: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
        Self::validate(&connection)?;
        self.update(|connections| {
            connections.retain(|current| current.destination != connection.destination);
            connections.push(connection.clone());
        })
    }

    fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError> {
        self.update(|connections| connections.retain(|current| &current.destination != destination))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{
        app_directories::AppDirectoryEnvironment,
        testing::{RecordingFilesystem, resolve_app_paths},
    };

    fn fixture() -> (Arc<AppPaths>, Arc<RecordingFilesystem>) {
        let fs = Arc::new(RecordingFilesystem::default());
        let paths = resolve_app_paths(
            &AppDirectoryEnvironment {
                home: Some("/home/test".into()),
                ..Default::default()
            },
            Some("/runtime".into()),
            200,
            fs.clone(),
        )
        .unwrap();
        (Arc::new(paths), fs)
    }

    fn connection(name: &str, server: u8) -> RetainedSshConnection {
        RetainedSshConnection {
            destination: SshDestination::new(name.to_owned()).unwrap(),
            continuity: Some(SshServerContinuity {
                route: [2; 32],
                account: 1000,
                build: "spaceterm-development".into(),
                server: [server; 32],
            }),
            active: true,
        }
    }

    #[test]
    fn relaunch_preserves_continuity_and_removal_keeps_other_connections() {
        let (paths, _) = fixture();
        let store = PrivateSshConnectionIntents::new(paths.clone());
        store.retain(connection("penguin", 3)).unwrap();
        store.retain(connection("other", 4)).unwrap();
        let reopened = PrivateSshConnectionIntents::new(paths);
        reopened
            .remove(&SshDestination::new("other".into()).unwrap())
            .unwrap();
        let records = reopened.load().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].destination.as_str(), "penguin");
        assert_eq!(records[0].continuity.as_ref().unwrap().server, [3; 32]);
        reopened.remove(&records[0].destination).unwrap();
        assert!(reopened.load().unwrap().is_empty());
    }

    #[test]
    fn an_inactive_saved_connection_survives_relaunch_inactive() {
        let (paths, _) = fixture();
        let mut inactive = connection("penguin", 3);
        inactive.active = false;
        PrivateSshConnectionIntents::new(paths.clone())
            .retain(inactive)
            .unwrap();
        let records = PrivateSshConnectionIntents::new(paths).load().unwrap();
        assert_eq!(records.len(), 1);
        assert!(!records[0].active);
    }

    #[test]
    fn malformed_or_unavailable_storage_is_not_replaced_with_new_intent() {
        let (paths, fs) = fixture();
        let store = PrivateSshConnectionIntents::new(paths.clone());
        store.retain(connection("penguin", 3)).unwrap();
        let path = paths.directories().data.join(FILE);
        fs.files.lock().unwrap().values.get_mut(&path).unwrap().0 =
            br#"{"schema":99,"connections":[]}"#.to_vec();
        assert!(matches!(
            store.retain(connection("other", 4)),
            Err(SshIntentStoreError::Malformed)
        ));
        assert_eq!(
            fs.files.lock().unwrap().values[&path].0,
            br#"{"schema":99,"connections":[]}"#
        );
        fs.files.lock().unwrap().read_failure = Some(SecureFilesystemError::Unavailable);
        assert!(matches!(
            store.load(),
            Err(SshIntentStoreError::Unavailable)
        ));
    }

    #[test]
    fn rejected_records_preserve_readable_intent_and_allow_later_updates() {
        let (paths, fs) = fixture();
        let store = PrivateSshConnectionIntents::new(paths.clone());
        store.retain(connection("penguin", 3)).unwrap();
        let path = paths.directories().data.join(FILE);
        let original = fs.files.lock().unwrap().values[&path].0.clone();
        let mut invalid_pin = connection("other", 0);
        invalid_pin.continuity.as_mut().unwrap().account = 0;
        for rejected in [connection(&"a".repeat(256), 4), invalid_pin] {
            assert_eq!(store.retain(rejected), Err(SshIntentStoreError::Malformed));
            assert_eq!(fs.files.lock().unwrap().values[&path].0, original);
            let records = PrivateSshConnectionIntents::new(paths.clone())
                .load()
                .unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].destination.as_str(), "penguin");
        }
        let boundary = connection(&"a".repeat(255), 4);
        store.retain(boundary.clone()).unwrap();
        assert_eq!(store.load().unwrap().len(), 2);
        store.remove(&boundary.destination).unwrap();
        store
            .remove(&SshDestination::new("penguin".into()).unwrap())
            .unwrap();
        assert!(store.load().unwrap().is_empty());
    }
}
