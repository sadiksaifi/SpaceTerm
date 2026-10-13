use super::{ServiceFailure, archive::Tree};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

const PUBLICATION_NAME: &str = "selection-publication";
const PUBLICATION_LIMIT: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationVersion {
    identity: Identity,
    sha256: [u8; 32],
}
impl PublicationVersion {
    fn of(record: &Record) -> Self {
        Self {
            identity: record.identity,
            sha256: Sha256::digest(&record.bytes).into(),
        }
    }
    fn matches(&self, record: &Record) -> bool {
        self.identity == record.identity
            && self.sha256 == <[u8; 32]>::from(Sha256::digest(&record.bytes))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Publication {
    schema: u16,
    previous: Option<PublicationVersion>,
    next: PublicationVersion,
    temporary: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    pub device: u64,
    pub inode: u64,
}
impl Identity {
    pub fn of(file: &File) -> Result<Self, ServiceFailure> {
        let facts = file.metadata().map_err(super::archive::classify)?;
        Ok(Self {
            device: facts.dev(),
            inode: facts.ino(),
        })
    }
    pub fn tuple(self) -> (u64, u64) {
        (self.device, self.inode)
    }
}

pub(super) struct Record {
    pub bytes: Vec<u8>,
    pub identity: Identity,
    // Holding the original inode prevents reuse while comparing or removing it.
    file: File,
}
impl Tree {
    pub(super) fn record(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Option<Record>, ServiceFailure> {
        let file = match self.read_file(name, 0o600) {
            Ok(file) => file,
            Err(ServiceFailure::Missing) => return Ok(None),
            Err(error) => return Err(error),
        };
        let before = file.metadata().map_err(super::archive::classify)?;
        if before.len() > limit as u64 {
            return Err(ServiceFailure::LimitExceeded);
        }
        let mut bytes = Vec::new();
        (&file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(super::archive::classify)?;
        let after = file.metadata().map_err(super::archive::classify)?;
        let identity = Identity::of(&file)?;
        if bytes.len() > limit
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || self.entry_identity(name)? != Some(identity)
        {
            return Err(ServiceFailure::UnsafeStorage);
        }
        Ok(Some(Record {
            bytes,
            identity,
            file,
        }))
    }
    /// Makes the validated recovered bytes and their exact directory entry durable.
    /// The caller holds the installation lock; no publication is repeated.
    pub(super) fn stabilize_record(
        &self,
        name: &str,
        record: &Record,
    ) -> Result<(), ServiceFailure> {
        self.expect_record(name, Some(record))?;
        super::archive::sync(&record.file)?;
        self.sync()?;
        self.expect_record(name, Some(record))
    }
    /// Stabilizes only the original publication's exact old or new selection.
    /// The caller holds the installation lock. No selection rename is repeated.
    pub(super) fn reconcile_selection(&self) -> Result<(), ServiceFailure> {
        let Some(intent) = self.record(PUBLICATION_NAME, PUBLICATION_LIMIT)? else {
            return Ok(());
        };
        let publication: Publication =
            serde_json::from_slice(&intent.bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
        let nonce = publication.temporary.strip_prefix(".cleanup-");
        if publication.schema != 1
            || publication.next.identity.inode == 0
            || publication.previous.as_ref().is_some_and(|previous| {
                previous.identity.inode == 0 || previous.identity == publication.next.identity
            })
            || nonce.is_none_or(|nonce| {
                nonce.len() != 32
                    || !nonce
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        let selection = self.record("selection", super::STATE_LIMIT)?;
        let permitted = match (&selection, &publication.previous) {
            (None, None) => true,
            (Some(record), previous) => {
                publication.next.matches(record)
                    || previous
                        .as_ref()
                        .is_some_and(|previous| previous.matches(record))
            }
            _ => false,
        };
        if !permitted {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        let temporary = self.record(&publication.temporary, super::STATE_LIMIT)?;
        if temporary
            .as_ref()
            .is_some_and(|record| !publication.next.matches(record))
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if let Some(record) = &selection {
            super::archive::sync(&record.file)?;
        }
        // The visible permitted selection must survive power loss before the
        // original publication intent can be forgotten or startup can use it.
        self.sync()?;
        self.expect_record("selection", selection.as_ref())?;
        if let Some(temporary) = &temporary {
            self.remove_record(&publication.temporary, temporary)?;
        }
        self.remove_record(PUBLICATION_NAME, &intent)
    }
    /// Retains exact old/new evidence until selection publication is durable.
    pub(super) fn publish_selection(
        &self,
        bytes: &[u8],
        expected: Option<&Record>,
    ) -> Result<(), ServiceFailure> {
        self.publish_record_inner("selection", bytes, expected, true)
    }
    /// A synchronized complete file precedes atomic publication.
    pub(super) fn publish_record(
        &self,
        name: &str,
        bytes: &[u8],
        expected: Option<&Record>,
    ) -> Result<(), ServiceFailure> {
        self.publish_record_inner(name, bytes, expected, false)
    }
    fn publish_record_inner(
        &self,
        name: &str,
        bytes: &[u8],
        expected: Option<&Record>,
        selection: bool,
    ) -> Result<(), ServiceFailure> {
        if selection && self.record(PUBLICATION_NAME, PUBLICATION_LIMIT)?.is_some() {
            return Err(ServiceFailure::Indeterminate);
        }
        let temporary = super::archive::quarantine_name()?;
        let mut file = self.create_file(&temporary, 0o600)?;
        let identity = Identity::of(&file)?;
        let mut publication_started = false;
        let mut intent = None;
        let result = (|| {
            file.write_all(bytes).map_err(super::archive::classify)?;
            super::archive::sync(&file)?;
            if selection {
                let publication = Publication {
                    schema: 1,
                    previous: expected.map(PublicationVersion::of),
                    next: PublicationVersion {
                        identity,
                        sha256: Sha256::digest(bytes).into(),
                    },
                    temporary: temporary.clone(),
                };
                let bytes =
                    serde_json::to_vec(&publication).map_err(|_| ServiceFailure::InvalidRecord)?;
                if bytes.len() > PUBLICATION_LIMIT {
                    return Err(ServiceFailure::LimitExceeded);
                }
                // This directory sync also establishes the complete candidate
                // inode before a selection rename can create uncertainty.
                publication_started = true;
                self.publish_record(PUBLICATION_NAME, &bytes, None)?;
                intent = Some(
                    self.record(PUBLICATION_NAME, PUBLICATION_LIMIT)?
                        .ok_or(ServiceFailure::Indeterminate)?,
                );
                #[cfg(feature = "test-support")]
                super::test_support::cleanup_boundary(15);
            }
            self.expect_record(name, expected)?;
            if expected.is_some() {
                self.rename(&temporary, name)?;
            } else {
                self.rename_exclusive(&temporary, name)?;
            }
            #[cfg(feature = "test-support")]
            if selection {
                super::test_support::published_selection();
                super::test_support::publication_sync()?;
            } else {
                super::test_support::cleanup_boundary(16);
            }
            self.sync()?;
            if self.entry_identity(name)? != Some(identity) {
                return Err(ServiceFailure::Indeterminate);
            }
            if let Some(intent) = &intent {
                self.remove_record(PUBLICATION_NAME, intent)?;
            }
            Ok(())
        })();
        // An interrupted selection keeps its recorded temporary for original
        // publication reconciliation. Generic records never remove replacements.
        if !publication_started && self.entry_identity(&temporary)? == Some(identity) {
            self.unlink_expected(&temporary, false, identity.tuple())?;
            self.sync()?;
        }
        result
    }
    fn expect_record(&self, name: &str, expected: Option<&Record>) -> Result<(), ServiceFailure> {
        let found = self.record(name, expected.map_or(0, |record| record.bytes.len()))?;
        match (found, expected) {
            (None, None) => Ok(()),
            (Some(found), Some(expected))
                if found.identity == expected.identity && found.bytes == expected.bytes =>
            {
                Ok(())
            }
            _ => Err(ServiceFailure::RequestConflict),
        }
    }
    pub(super) fn remove_record(
        &self,
        name: &str,
        expected: &Record,
    ) -> Result<(), ServiceFailure> {
        self.expect_record(name, Some(expected))?;
        self.unlink_expected(name, false, expected.identity.tuple())?;
        self.sync()
    }
}
