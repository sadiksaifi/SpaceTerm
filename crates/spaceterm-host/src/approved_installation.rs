//! Approved account installation metadata uses schema 2 without a launcher ABI.
//!
//! The historical privileged descriptor below remains a compatibility reader for
//! its existing consumers. Account installation never uses it for new artifacts.

// Keep the authenticated account representation owned by distribution.
pub use spaceterm_distribution::{
    AccountInstallationManifestV2, verify_account_installation_manifest,
};

use crate::native_deployment::{
    bindings::{self, digest, identity},
    contracts::*,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// Immutable installation comparison facts, authenticated by the exact descriptor digest.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedInstallationDescriptor {
    /// Persisted descriptor schema, currently one.
    pub schema_version: u16,
    /// Registered account/build namespace.
    pub key: AccountBuildKey,
    /// Protected monotonic installation allocation, never a truncated digest.
    pub installation: InstallationId,
    /// Canonical core ServerId bytes. Integration maps the core facade's identity type.
    #[serde(with = "identity")]
    pub server_id: [u8; 16],
    /// Native account incarnation, UID, GID, and build binding.
    #[serde(with = "digest")]
    pub account_binding_sha256: [u8; 32],
    /// Exact protected profile digest.
    #[serde(with = "digest")]
    pub registration_profile_sha256: [u8; 32],
    /// Fresh cryptographically generated publication nonce.
    #[serde(with = "digest")]
    pub publication_nonce: [u8; 32],
    /// Exact signed archive digest.
    #[serde(with = "digest")]
    pub archive_sha256: [u8; 32],
    /// Exact installation manifest digest.
    #[serde(with = "digest")]
    pub manifest_sha256: [u8; 32],
    /// SHA-256 of the exact admitted main server file, not its CDHash.
    #[serde(with = "digest")]
    pub server_artifact_sha256: [u8; 32],
    /// Authenticated launcher, manager, credential tool, interpreter, libraries and resources.
    #[serde(with = "digest")]
    pub execution_closure_sha256: [u8; 32],
    /// Required production exchange ABI, currently two.
    pub launcher_abi: u16,
}
impl ApprovedInstallationDescriptor {
    /// Checks facts without granting retained native custody or execution approval.
    pub fn validate(&self) -> Result<(), NativeFailure> {
        validate_key(self.key)?;
        if self.schema_version != 1 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        if self.launcher_abi != 2 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        nonzero(&self.server_id)?;
        for digest in [
            &self.account_binding_sha256,
            &self.registration_profile_sha256,
            &self.publication_nonce,
            &self.archive_sha256,
            &self.manifest_sha256,
            &self.server_artifact_sha256,
            &self.execution_closure_sha256,
        ] {
            nonzero(digest)?;
        }
        Ok(())
    }
    /// Encodes canonical strict schema-one JSON, at most 16 KiB.
    pub fn encode(&self) -> Result<Vec<u8>, NativeFailure> {
        self.validate()?;
        bindings::canonical(self, 16 * 1024)
    }
    /// Rejects duplicates, unknown fields, missing fields and noncanonical JSON.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeFailure> {
        let descriptor: Self = bindings::decode_canonical(bytes, 16 * 1024)?;
        descriptor.validate()?;
        Ok(descriptor)
    }
    /// Derives the complete 32-byte handoff binding. No digest is truncated to an ID.
    pub fn installation_binding(&self) -> Result<[u8; 32], NativeFailure> {
        self.validate()?;
        let mut h = Sha256::new();
        h.update(b"SpaceTerm approved installation binding v1\0");
        h.update([build_code(self.key.build_identity)]);
        h.update(self.key.account_uid.to_le_bytes());
        h.update(self.installation.0.get().to_le_bytes());
        h.update(self.server_id);
        for bytes in [
            self.account_binding_sha256,
            self.registration_profile_sha256,
            self.publication_nonce,
            self.archive_sha256,
            self.manifest_sha256,
            self.server_artifact_sha256,
            self.execution_closure_sha256,
        ] {
            h.update(bytes);
        }
        h.update(self.launcher_abi.to_le_bytes());
        let digest: [u8; 32] = h.finalize().into();
        nonzero(&digest)?;
        Ok(digest)
    }
    /// Verifies exact descriptor bytes against the protected head before interpreting them.
    pub fn decode_authenticated(
        bytes: &[u8],
        expected_sha256: &[u8; 32],
    ) -> Result<Self, NativeFailure> {
        if bytes.len() > 16 * 1024 {
            return Err(NativeFailure::LimitExceeded);
        }
        nonzero(expected_sha256)?;
        let observed: [u8; 32] = Sha256::digest(bytes).into();
        if &observed != expected_sha256 {
            return Err(NativeFailure::ArtifactRejected);
        }
        Self::decode(bytes)
    }
}
impl fmt::Debug for ApprovedInstallationDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApprovedInstallationDescriptor(..)")
    }
}

// Only the fresh publisher and native approved-selection reader can bind these handles.
// Descriptor decoding and permission changes cannot construct this authority.
pub(crate) struct InstallationCustody {
    publication: crate::native_deployment::publication::RetainedPublication,
}
impl InstallationCustody {
    pub(crate) fn from_publication(
        publication: crate::native_deployment::publication::RetainedPublication,
    ) -> Self {
        Self { publication }
    }
    pub(crate) fn revalidate(&self) -> Result<(), NativeFailure> {
        self.publication.verify()
    }
}

/// Noncloneable authority retaining authenticated code, resources and native custody through use.
pub struct RetainedApprovedInstallation {
    descriptor: ApprovedInstallationDescriptor,
    binding: [u8; 32],
    _custody: InstallationCustody,
}
impl RetainedApprovedInstallation {
    pub(crate) fn from_publication(
        descriptor: ApprovedInstallationDescriptor,
        custody: InstallationCustody,
    ) -> Result<Self, NativeFailure> {
        custody.revalidate()?;
        let binding = descriptor.installation_binding()?;
        Ok(Self {
            descriptor,
            binding,
            _custody: custody,
        })
    }
    /// Revalidates retained file identities, destination bytes, ancestor links and native custody.
    pub fn revalidate(&self) -> Result<(), NativeFailure> {
        self._custody.revalidate()
    }
    /// Borrows the exact read-only main executable after revalidation, never reopening a caller path.
    pub fn retained_server(&self) -> Result<&std::fs::File, NativeFailure> {
        self._custody.publication.server()
    }
    /// Borrows the exact protected credential tool after revalidation.
    pub fn retained_credential_tool(&self) -> Result<&std::fs::File, NativeFailure> {
        self._custody.publication.credential_tool()
    }
    /// Returns authenticated installation facts.
    pub fn descriptor(&self) -> &ApprovedInstallationDescriptor {
        &self.descriptor
    }
    /// Returns the complete production handoff binding.
    pub fn installation_binding(&self) -> &[u8; 32] {
        &self.binding
    }
}
impl fmt::Debug for RetainedApprovedInstallation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetainedApprovedInstallation(..)")
    }
}
