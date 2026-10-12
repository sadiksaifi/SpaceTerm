//! A private old-clone probe, separate from read-only mount inspection.
//!
//! The result witnesses only the selected mount. It does not establish backing
//! storage, publication history, broker policy, or exclusion of writable aliases.

use crate::native_deployment::contracts::NativeFailure;
use std::fs::File;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
const MAX_ATTEMPTS: u32 = 128;
#[cfg(target_os = "linux")]
const MAX_INTERRUPTS: u32 = 4;
#[cfg(target_os = "linux")]
const DEADLINE: Duration = Duration::from_secs(2);

/// Descriptor-derived comparison identity of the original, never the clone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct OriginalIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mount: u64,
}

pub(super) struct OldCloneWitness {
    original: File,
    identity: OriginalIdentity,
}

impl OldCloneWitness {
    pub(super) fn original(&self) -> &File {
        &self.original
    }
    pub(super) fn identity(&self) -> OriginalIdentity {
        self.identity
    }
}

/// All methods operate on the same retained original. A successful syscall with
/// no actual nontrivial IDMAP application is not a successful peer response.
pub(super) trait ProbePeer {
    fn identity(&self, original: &File) -> Result<OriginalIdentity, NativeFailure>;
    fn apply_to_private_old_clone(&self, original: &File) -> Result<(), NativeFailure>;
}

pub(super) fn acquire(
    original: File,
    peer: &impl ProbePeer,
) -> Result<OldCloneWitness, NativeFailure> {
    let before = peer.identity(&original)?;
    if before.mount == 0 || before.inode == 0 {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    peer.apply_to_private_old_clone(&original)?;
    if peer.identity(&original)? != before {
        return Err(NativeFailure::LinuxMountWitnessUnavailable);
    }
    Ok(OldCloneWitness {
        original,
        identity: before,
    })
}

#[cfg(target_os = "linux")]
pub(super) fn acquire_native(original: File) -> Result<OldCloneWitness, NativeFailure> {
    acquire(
        original,
        &native::KernelProbe::new(Instant::now() + DEADLINE),
    )
}

#[cfg(target_os = "linux")]
mod native;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Peer {
        samples: Cell<u32>,
        changed: bool,
        outcome: Result<(), NativeFailure>,
    }
    impl ProbePeer for Peer {
        fn identity(&self, _: &File) -> Result<OriginalIdentity, NativeFailure> {
            let sample = self.samples.replace(self.samples.get() + 1);
            Ok(OriginalIdentity {
                device: 7,
                inode: 11,
                mount: if self.changed && sample > 0 { 18 } else { 17 },
            })
        }
        fn apply_to_private_old_clone(&self, _: &File) -> Result<(), NativeFailure> {
            self.outcome
        }
    }

    #[test]
    fn successful_old_clone_witness_retains_and_rechecks_the_original() {
        let file = tempfile::tempfile().unwrap();
        let peer = Peer {
            samples: Cell::new(0),
            changed: false,
            outcome: Ok(()),
        };
        let witness = acquire(file, &peer).unwrap();
        assert_eq!(
            witness.identity(),
            OriginalIdentity {
                device: 7,
                inode: 11,
                mount: 17
            }
        );
        assert!(witness.original().metadata().is_ok());
        assert_eq!(peer.samples.get(), 2);
    }

    #[test]
    fn old_clone_probe_failure_never_becomes_mapping_provenance() {
        for refusal in [
            NativeFailure::LinuxMountWitnessUnavailable,
            NativeFailure::TimedOut,
            NativeFailure::CleanupUnresolved,
            NativeFailure::LimitExceeded,
        ] {
            let peer = Peer {
                samples: Cell::new(0),
                changed: false,
                outcome: Err(refusal),
            };
            assert!(
                matches!(acquire(tempfile::tempfile().unwrap(), &peer), Err(e) if e == refusal)
            );
            assert_eq!(peer.samples.get(), 1);
        }
    }

    #[test]
    fn changed_original_cannot_be_replaced_by_the_successful_clone() {
        let peer = Peer {
            samples: Cell::new(0),
            changed: true,
            outcome: Ok(()),
        };
        assert!(matches!(
            acquire(tempfile::tempfile().unwrap(), &peer),
            Err(NativeFailure::LinuxMountWitnessUnavailable)
        ));
    }
}
