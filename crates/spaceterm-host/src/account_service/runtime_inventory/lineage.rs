//! Bounded signed history of immutable per-boot parent relationships.
//!
//! A missing intermediate process is not absence evidence. Checkpoints retain
//! its ancestry while any observed live process still refers to that ancestry.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

const DIRECTORY: &str = "runtime-history";
const ENTRY: &str = "lineage";
const LIMIT: usize = 4096;
const LINEAGE_DOMAIN: &[u8] = b"spaceterm-account-runtime-lineage-v1\0";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema: u16,
    instance: [u8; 32],
    owner: InstanceOwner,
    ancestors: Vec<[u64; 2]>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedLedger {
    body: String,
    signature: Vec<u8>,
}

fn bytes(body: &[u8]) -> Vec<u8> {
    [LINEAGE_DOMAIN, body].concat()
}

fn decode(registration: &RegisteredServer, record: &[u8]) -> Result<Ledger, ServiceFailure> {
    let signed: SignedLedger =
        serde_json::from_slice(record).map_err(|_| ServiceFailure::InvalidRecord)?;
    if signed.signature.len() != 64 || signed.body.len() > RECORD_LIMIT {
        return Err(ServiceFailure::InvalidRecord);
    }
    UnparsedPublicKey::new(&ED25519, registration.server_pin().public_key)
        .verify(&bytes(signed.body.as_bytes()), &signed.signature)
        .map_err(|_| ServiceFailure::IdentityMismatch)?;
    let ledger: Ledger = serde_json::from_slice(signed.body.as_bytes())
        .map_err(|_| ServiceFailure::InvalidRecord)?;
    if ledger.schema != 1
        || ledger.instance == [0; 32]
        || ledger.owner.boot == [0; 32]
        || ledger.owner.pid == 0
        || ledger.owner.pid > i32::MAX as u32
        || ledger.owner.birth == 0
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    validate(&ledger.ancestors)?;
    Ok(ledger)
}

fn read(registration: &RegisteredServer) -> Result<Option<Ledger>, ServiceFailure> {
    use crate::private_filesystem::{Directory, Failure};
    let path = registration.data_directory().join(DIRECTORY);
    let directory = match Directory::open(&path) {
        Ok(Some(directory)) => directory,
        Ok(None) => return Ok(None),
        Err(Failure::Indeterminate) => {
            // A crash may interrupt the first checkpoint's directory admission.
            // Reconcile its original mkdir phase without creating a missing child.
            Directory::reconcile_initialization(&path)?;
            match Directory::open(&path) {
                Ok(Some(directory)) => directory,
                Ok(None) => return Ok(None),
                Err(Failure::Indeterminate) => {
                    // Replay only this fixed, exact signed checkpoint. An unrelated
                    // or malformed original directory phase remains unavailable.
                    Directory::reconcile_publication(&path, ENTRY, RECORD_LIMIT, |bytes| {
                        decode(registration, bytes)
                            .map(|_| ())
                            .map_err(|_| Failure::Unsafe)
                    })?;
                    Directory::open(&path)?.ok_or(ServiceFailure::CleanupUnresolved)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    directory
        .read(ENTRY, RECORD_LIMIT)?
        .map(|record| decode(registration, &record.bytes))
        .transpose()
}

fn validate(ancestors: &[[u64; 2]]) -> Result<(), ServiceFailure> {
    if ancestors.len() > LIMIT {
        return Err(ServiceFailure::LimitExceeded);
    }
    let mut previous = 0;
    for &[birth, parent] in ancestors {
        if birth <= previous || parent >= birth {
            return Err(ServiceFailure::InvalidRecord);
        }
        previous = birth;
    }
    Ok(())
}

pub(super) fn retained(
    registration: &RegisteredServer,
    state: &State,
) -> Result<Vec<[u64; 2]>, ServiceFailure> {
    let Some(ledger) = read(registration)? else {
        // Older shipped custody remains readable and retains conservative recovery.
        return Ok(Vec::new());
    };
    if ledger.instance != state.instance || Some(ledger.owner) != state.owner {
        return Ok(Vec::new());
    }
    Ok(ledger.ancestors)
}

pub(super) fn checkpoint(
    registration: &RegisteredServer,
    custody: &RuntimeCustody,
) -> Result<(), ServiceFailure> {
    let (state, _) = super::read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
    if state.instance != custody.state.instance || state.epoch != custody.state.epoch {
        return Err(ServiceFailure::IdentityMismatch);
    }
    if state.clean {
        return Ok(());
    }
    let owner = state.owner.ok_or(ServiceFailure::CleanupUnresolved)?;
    let current = super::super::native_processes::instance_lineage()?;
    let old = retained(registration, &state)?;
    let ancestors = merge(&old, &current)?;
    if ancestors == old {
        return Ok(());
    }
    let body = serde_json::to_string(&Ledger {
        schema: 1,
        instance: state.instance,
        owner,
        ancestors,
    })
    .map_err(|_| ServiceFailure::InvalidRecord)?;
    let signed = SignedLedger {
        signature: custody
            .signer
            .sign(&bytes(body.as_bytes()))
            .map_err(|_| ServiceFailure::StorageUnavailable)?
            .to_vec(),
        body,
    };
    let record = serde_json::to_vec(&signed).map_err(|_| ServiceFailure::InvalidRecord)?;
    if record.len() > RECORD_LIMIT {
        return Err(ServiceFailure::LimitExceeded);
    }
    let directory = crate::private_filesystem::Directory::create(
        &registration.data_directory().join(DIRECTORY),
    )?;
    let previous = directory.read(ENTRY, RECORD_LIMIT)?;
    directory.publish(ENTRY, &record, previous.as_ref())?;
    Ok(())
}

fn merge(old: &[[u64; 2]], current: &[[u64; 2]]) -> Result<Vec<[u64; 2]>, ServiceFailure> {
    validate(old)?;
    validate(current)?;
    let mut graph: BTreeMap<_, _> = old.iter().map(|&[birth, parent]| (birth, parent)).collect();
    for &[birth, parent] in current {
        if graph.insert(birth, parent).is_some_and(|old| old != parent) {
            return Err(ServiceFailure::IdentityMismatch);
        }
    }
    let mut needed = BTreeSet::new();
    for &[birth, _] in current {
        let mut ancestor = birth;
        while let Some(&parent) = graph.get(&ancestor) {
            if !needed.insert(ancestor) {
                break;
            }
            ancestor = parent;
        }
    }
    let retained: Vec<_> = graph
        .into_iter()
        .filter(|(birth, _)| needed.contains(birth))
        .map(|(birth, parent)| [birth, parent])
        .collect();
    validate(&retained)?;
    Ok(retained)
}

/// Only live processes must be discharged. Historical dead intermediates supply
/// ancestry, not liveness. Unknown ancestry remains unresolved.
#[cfg(any(test, target_os = "macos"))]
pub(in crate::account_service) fn absent(
    root: u64,
    live: &[[u64; 2]],
    retained: &[[u64; 2]],
) -> Result<(), ServiceFailure> {
    validate(live)?;
    validate(retained)?;
    let graph: BTreeMap<_, _> = retained
        .iter()
        .chain(live)
        .map(|&[birth, parent]| (birth, parent))
        .collect();
    for &[birth, parent] in retained {
        if graph.get(&birth) != Some(&parent) {
            return Err(ServiceFailure::IdentityMismatch);
        }
    }
    for &[birth, _] in live {
        if birth < root {
            continue;
        }
        let mut ancestor = birth;
        while ancestor >= root {
            if ancestor == root {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            ancestor = *graph
                .get(&ancestor)
                .ok_or(ServiceFailure::CleanupUnresolved)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_original_parent_disambiguates_an_unrelated_live_orphan() {
        assert_eq!(absent(100, &[[104, 103]], &[[103, 99]]), Ok(()));
    }

    #[test]
    fn retained_original_parent_never_discharges_a_live_descendant() {
        assert_eq!(
            absent(100, &[[104, 103]], &[[103, 100]]),
            Err(ServiceFailure::CleanupUnresolved)
        );
    }

    #[test]
    fn unknown_parent_and_conflicting_immutable_history_remain_unresolved() {
        assert_eq!(
            absent(100, &[[104, 103]], &[]),
            Err(ServiceFailure::CleanupUnresolved)
        );
        assert_eq!(
            absent(100, &[[104, 103]], &[[104, 99]]),
            Err(ServiceFailure::IdentityMismatch)
        );
    }

    #[test]
    fn checkpoint_keeps_dead_intermediates_until_their_last_live_descendant_exits() {
        assert_eq!(
            merge(&[[103, 99], [104, 103]], &[[104, 103]]).unwrap(),
            vec![[103, 99], [104, 103]]
        );
        assert!(merge(&[[103, 99], [104, 103]], &[]).unwrap().is_empty());
    }
}
