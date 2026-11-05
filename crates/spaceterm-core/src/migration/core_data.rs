//! SpaceTerm Core's data root: the records one Core version owns, directly in its data
//! directory.
//!
//! `service.json`, `updates.json` and `service-operations.json` stay outside it. The runtime an
//! activation replaces and the Pending Version it starts both read them, so their formats only
//! grow and never migrate. The clients' directories under `clients/` are the CLI's and the
//! desktop's own roots.
use super::{
    DataKind, DirectoryStore, FileRule, Mount, NoCredentials, Outcome, PendingReset, Refusal,
    Registry, StoreError, generations,
};
use crate::{CoreError, host::Paths, storage::Directory};
use std::path::Path;

const RULES: &[FileRule] = &[
    FileRule {
        name: "identity.json",
        kind: DataKind::MachineId,
        read: crate::storage::readable,
        stamp: None,
    },
    FileRule {
        name: crate::organization::FILE,
        kind: DataKind::WorkspaceOrganization,
        read: crate::organization::readable,
        stamp: None,
    },
    FileRule {
        name: "direct.json",
        kind: DataKind::Pairings,
        read: crate::direct::storage::readable,
        stamp: None,
    },
    FileRule {
        name: "paired-majors.json",
        kind: DataKind::Pairings,
        read: crate::direct::storage::readable_offered,
        stamp: None,
    },
    FileRule {
        name: crate::update::proxy::FILE,
        kind: DataKind::CoreRecords,
        read: crate::update::proxy::readable,
        stamp: None,
    },
];

/// The Core data root's generations, steps and records.
pub const REGISTRY: Registry = Registry {
    generations: generations::CORE_DATA,
    steps: &[],
    files: &[RULES],
};

/// The Core data root whose directory is `data`.
pub fn store(data: &Path) -> DirectoryStore {
    DirectoryStore::new(data.to_path_buf(), vec![Mount::files(data.to_path_buf())])
}

/// Brings the Core data root in `data` to this build's generation. `returned` says that an
/// activation returned to this runtime. The caller holds `service-intent.lock` and
/// `setup.lock`, which every other writer of the root's records takes.
pub(crate) fn run_locked(data: &Path, returned: bool) -> Result<Outcome, CoreError> {
    super::run(&REGISTRY, &store(data), &mut NoCredentials, returned).map_err(refusal)
}

/// Brings this Core's data to this build's generation before its runtime reads any of it.
pub(crate) fn prepare(paths: &Paths) -> Result<Outcome, CoreError> {
    let data = match Directory::open(&paths.data, false) {
        Ok(data) => data,
        Err(CoreError::NotSetUp) => return Ok(Outcome::Current),
        Err(error) => return Err(error),
    };
    let _intent = data.lock("service-intent.lock", false)?;
    let _setup = data.lock("setup.lock", false)?;
    run_locked(&paths.data, false)
}

/// The reset this Core's data waits for, as the last runtime that refused to read it recorded.
pub fn pending_reset(identity: &str) -> Result<Option<PendingReset>, CoreError> {
    let paths = Paths::for_identity(identity)?;
    super::pending(&store(&paths.data)).map_err(storage)
}

/// Resets the records of this Core's data that wait for a reset, after the person confirmed
/// it, and names the kinds of data it replaced. A root that no longer needs a reset is left
/// alone. The caller is this Core's approved runtime, and the Core is not running.
pub fn reset_confirmed(identity: &str) -> Result<Vec<DataKind>, CoreError> {
    let paths = Paths::for_identity(identity)?;
    let data = Directory::open(&paths.data, false)?;
    let _operation = data.lock("service.lock", false)?;
    let _intent = data.lock("service-intent.lock", false)?;
    let _setup = data.lock("setup.lock", false)?;
    // A running Core read its data, so a reset never races its runtime. Holding the instance
    // lock also keeps one from starting meanwhile.
    let runtime = Directory::open(&paths.runtime, true)?;
    let _instance = runtime
        .try_lock("server.lock")?
        .ok_or(CoreError::ServiceBusy)?;
    super::reset_confirmed(&REGISTRY, &store(&paths.data), &mut NoCredentials).map_err(refusal)
}

fn refusal(refusal: Refusal) -> CoreError {
    match refusal {
        Refusal::Newer => CoreError::IncompatibleState,
        Refusal::Reset { .. } => CoreError::DataResetRequired,
        Refusal::Storage(error) => storage(error),
    }
}

fn storage(error: StoreError) -> CoreError {
    match error {
        StoreError::Unavailable => CoreError::StorageUnavailable,
        StoreError::Unsafe => CoreError::UnsafeStorage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::fixtures;

    #[test]
    fn every_core_data_fixture_migrates_and_reads_strictly() {
        fixtures::check(&REGISTRY, &fixtures::directory("core-data"), &[], |root| {
            Box::new(store(root))
        });
    }

    /// `mise run migration:fixture core-data` writes this build's generation's fixture.
    #[test]
    #[ignore = "writes the fixture of the current generation"]
    fn capture_core_data_fixture() {
        let root = fixtures::scratch("core-data");
        let paths = Paths {
            identity: "spaceterm-development".into(),
            data: root.join("data"),
            runtime: root.join("runtime"),
        };
        let data = Directory::open(&paths.data, true).unwrap();
        prepare(&paths).unwrap();
        {
            let _setup = data.lock("setup.lock", false).unwrap();
            data.read_or_create().unwrap();
        }
        crate::organization::OrganizationStore(Directory::open(&paths.data, false).unwrap())
            .write(Some(
                &crate::hierarchy::State::first("/".as_ref())
                    .unwrap()
                    .envelope(),
            ))
            .unwrap();
        drop(crate::direct::host::State::open(&paths).unwrap());
        data.write_json(c"paired-majors.json", &serde_json::json!([]))
            .unwrap();
        crate::update::proxy::write(
            &paths.data,
            Some(&crate::update::proxy::Proxy::parse("http://127.0.0.1:3128").unwrap()),
        )
        .unwrap();
        fixtures::capture(
            &REGISTRY,
            &paths.data,
            &store(&paths.data),
            &fixtures::directory("core-data"),
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
