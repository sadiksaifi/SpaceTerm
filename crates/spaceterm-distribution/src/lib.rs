//! Bounded client/server release metadata and exact-byte authentication.
//!
//! These records are comparison facts. They grant no native installation, custody,
//! execution, downgrade, or credential-release authority.

pub mod client_feed;
mod encoding;
mod key;
pub mod manifest;
pub mod server_feed;
mod version;

pub use key::UpdateKey;
pub use manifest::{
    BundledDependency, InstallationFile, InstallationManifestV1, ServerEntryPoints,
    VerifiedInstallationManifest, verify_installation_manifest,
};
pub use server_feed::ServerArchiveVerifier;
pub use server_feed::{
    BuildChannel, InstallationCompatibility, ServerArchiveV1, ServerComponent, ServerFeedV1,
    ServerPlatform, StorageCompatibility, VerifiedServerArchive, VerifiedServerFeed,
    verify_server_archive, verify_server_feed,
};
pub use version::{BuildVersion, StableVersion};

/// Content-free verification failures. No untrusted native/JSON text is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DistributionFailure {
    Verification,
    InvalidRecord,
    UnsupportedSchema,
    IdentityMismatch,
    LimitExceeded,
    ArchiveRejected,
    ManifestRejected,
}
