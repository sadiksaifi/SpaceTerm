//! Real filesystem identity mechanics, selected only by native test composition.
use crate::local_filesystem;

#[path = "../../../src/platform/unix_local_identity.rs"]
mod unix_local_identity;

pub(super) fn identity() -> impl local_filesystem::LocalIdentitySource {
    unix_local_identity::UnixLocalIdentity
}
