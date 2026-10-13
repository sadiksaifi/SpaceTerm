//! Small created-resource inventory for interrupted native setup.
//!
//! Identities are comparison facts held in protected product storage. Neither this
//! inventory nor a JSONL report authorizes signals or deletion. The production
//! publisher, supervisor and credential-item owner retain and revalidate their
//! objects through those effects. Unrecorded resources need operator reconciliation.

use crate::native_deployment::{
    bindings::{self, digest, identity},
    contracts::{NativeFailure, NativeRequestId, PublicationDisposition},
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, fmt};

const INVENTORY_BYTES: usize = 16 * 1024;

/// Candidate and native-account binding of a single setup run.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryBinding {
    run: NativeRequestId,
    #[serde(with = "digest")]
    account_binding_sha256: [u8; 32],
    #[serde(with = "digest")]
    artifact_sha256: [u8; 32],
}
impl InventoryBinding {
    /// Checks facts. Only the product storage owner supplies protected authority.
    pub fn new(
        run: NativeRequestId,
        account_binding_sha256: [u8; 32],
        artifact_sha256: [u8; 32],
    ) -> Result<Self, NativeFailure> {
        if account_binding_sha256 == [0; 32] || artifact_sha256 == [0; 32] {
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self {
            run,
            account_binding_sha256,
            artifact_sha256,
        })
    }
}
impl fmt::Debug for InventoryBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InventoryBinding(..)")
    }
}

/// Fixed resources created by the bounded native setup sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InventoryResource {
    /// A product publisher's fresh protected installation.
    Installation,
    /// The product registration object.
    Registration,
    /// The credential owner's exact synthetic item.
    Credential,
    /// The supervisor's protected service configuration.
    Service,
    /// The exact startup process.
    Server,
    /// The exact one-shot retriever process.
    Retriever,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum EffectIdentity {
    Object {
        #[serde(with = "identity")]
        volume: [u8; 16],
        device: u64,
        inode: u64,
        generation: u64,
        birth_seconds: u64,
        birth_nanoseconds: u32,
        #[serde(with = "digest")]
        artifact_sha256: [u8; 32],
    },
    CredentialItem {
        persistent_reference: Vec<u8>,
        #[serde(with = "digest")]
        credential_binding_sha256: [u8; 32],
    },
    Process {
        #[serde(with = "identity")]
        boot_id: [u8; 16],
        pid: u32,
        start_seconds: u64,
        start_microseconds: u32,
        #[serde(with = "digest")]
        executable_sha256: [u8; 32],
    },
}
/// Exact native identity facts, not a retained handle or cleanup permission.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NativeEffectBinding(EffectIdentity);
impl NativeEffectBinding {
    /// Records the object identity observed by the production publisher.
    pub fn object(
        volume: [u8; 16],
        device: u64,
        inode: u64,
        generation: u64,
        birth_seconds: u64,
        birth_nanoseconds: u32,
        artifact_sha256: [u8; 32],
    ) -> Result<Self, NativeFailure> {
        Self::checked(EffectIdentity::Object {
            volume,
            device,
            inode,
            generation,
            birth_seconds,
            birth_nanoseconds,
            artifact_sha256,
        })
    }
    /// Records the persistent item reference returned by the credential owner.
    pub fn credential_item(
        persistent_reference: Vec<u8>,
        credential_binding_sha256: [u8; 32],
    ) -> Result<Self, NativeFailure> {
        Self::checked(EffectIdentity::CredentialItem {
            persistent_reference,
            credential_binding_sha256,
        })
    }
    /// Records boot-bound prior-process identity, never a PID alone.
    pub fn process(
        boot_id: [u8; 16],
        pid: u32,
        start_seconds: u64,
        start_microseconds: u32,
        executable_sha256: [u8; 32],
    ) -> Result<Self, NativeFailure> {
        Self::checked(EffectIdentity::Process {
            boot_id,
            pid,
            start_seconds,
            start_microseconds,
            executable_sha256,
        })
    }
    /// Borrows the protected persistent item reference for the production item owner.
    /// This is not credential data and is never emitted in diagnostics or JSONL.
    pub fn persistent_reference(&self) -> Option<&[u8]> {
        match &self.0 {
            EffectIdentity::CredentialItem {
                persistent_reference,
                ..
            } => Some(persistent_reference),
            _ => None,
        }
    }
    fn checked(value: EffectIdentity) -> Result<Self, NativeFailure> {
        let binding = Self(value);
        binding.validate()?;
        Ok(binding)
    }
    fn validate(&self) -> Result<(), NativeFailure> {
        let valid = match &self.0 {
            EffectIdentity::Object {
                volume,
                inode,
                birth_seconds,
                birth_nanoseconds,
                artifact_sha256,
                ..
            } => {
                *volume != [0; 16]
                    && *inode != 0
                    && *birth_seconds != 0
                    && *birth_nanoseconds < 1_000_000_000
                    && *artifact_sha256 != [0; 32]
            }
            EffectIdentity::CredentialItem {
                persistent_reference,
                credential_binding_sha256,
            } => {
                !persistent_reference.is_empty()
                    && persistent_reference.len() <= 4096
                    && *credential_binding_sha256 != [0; 32]
            }
            EffectIdentity::Process {
                boot_id,
                pid,
                start_seconds,
                start_microseconds,
                executable_sha256,
            } => {
                *boot_id != [0; 16]
                    && (1..=i32::MAX as u32).contains(pid)
                    && *start_seconds != 0
                    && *start_microseconds < 1_000_000
                    && *executable_sha256 != [0; 32]
            }
        };
        if valid {
            Ok(())
        } else {
            Err(NativeFailure::InvalidRecord)
        }
    }
    fn matches_resource(&self, resource: InventoryResource) -> bool {
        matches!(
            (&self.0, resource),
            (
                EffectIdentity::Object { .. },
                InventoryResource::Installation
                    | InventoryResource::Registration
                    | InventoryResource::Service
            ) | (
                EffectIdentity::CredentialItem { .. },
                InventoryResource::Credential
            ) | (
                EffectIdentity::Process { .. },
                InventoryResource::Server | InventoryResource::Retriever
            )
        )
    }
}
impl fmt::Debug for NativeEffectBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeEffectBinding(..)")
    }
}
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ResourceState {
    Pending,
    Created,
    Removed,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    resource: InventoryResource,
    state: ResourceState,
    identity: Option<NativeEffectBinding>,
}

/// Bounded protected inventory facts. No generic schema-2 cleanup authority is built.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeReceipt {
    schema: u16,
    scope: String,
    binding: InventoryBinding,
    entries: Vec<Entry>,
}
impl NativeReceipt {
    fn new(binding: InventoryBinding) -> Self {
        Self {
            schema: 1,
            scope: "native-created-resources".into(),
            binding,
            entries: Vec::new(),
        }
    }
    fn validate(&self) -> Result<(), NativeFailure> {
        if self.schema != 1 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        if self.scope != "native-created-resources" || self.entries.len() > 6 {
            return Err(NativeFailure::InvalidRecord);
        }
        InventoryBinding::new(
            self.binding.run,
            self.binding.account_binding_sha256,
            self.binding.artifact_sha256,
        )?;
        for (index, entry) in self.entries.iter().enumerate() {
            if self.entries[..index]
                .iter()
                .any(|old| old.resource == entry.resource)
            {
                return Err(NativeFailure::InvalidRecord);
            }
            if let Some(identity) = &entry.identity {
                identity.validate()?;
                if !identity.matches_resource(entry.resource) {
                    return Err(NativeFailure::InvalidRecord);
                }
            } else if entry.state != ResourceState::Pending {
                return Err(NativeFailure::InvalidRecord);
            }
        }
        Ok(())
    }
    fn encode(&self) -> Result<Vec<u8>, NativeFailure> {
        self.validate()?;
        bindings::canonical(self, INVENTORY_BYTES)
    }
    fn decode(bytes: &[u8]) -> Result<Self, NativeFailure> {
        let record: Self = bindings::decode_canonical(bytes, INVENTORY_BYTES)?;
        record.validate()?;
        Ok(record)
    }
}
impl fmt::Debug for NativeReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeReceipt(..)")
    }
}

/// The existing product authority stores this inventory beside its protected records.
///
/// Implementations authenticate the fixed storage and serialize compare/publication
/// with native management. They exclude account writes and persist an uncertain
/// publication for recreated consumers. Unprivileged output files never implement
/// this port in production. No directory path is selected by an evidence record.
pub trait ProtectedInventoryStorage {
    /// Reads exact protected inventory bytes. Pending publication refuses recovery.
    fn read(&self, run: NativeRequestId) -> Result<Vec<u8>, NativeFailure>;
    /// Publishes only when current bytes equal `previous`; None requires absence.
    /// Indeterminate must persist until reviewed reconciliation, not a later fsync.
    fn publish(
        &self,
        run: NativeRequestId,
        previous: Option<&[u8]>,
        bytes: &[u8],
    ) -> PublicationDisposition<()>;
}
/// Small per-run inventory. It cannot signal processes or delete objects or items.
pub struct ResourceInventory<S> {
    storage: S,
    record: NativeReceipt,
    uncertain: bool,
}
impl<S: ProtectedInventoryStorage> ResourceInventory<S> {
    /// Publishes durable empty intent before any setup effect.
    pub fn create(storage: S, binding: InventoryBinding) -> Result<Self, NativeFailure> {
        let record = NativeReceipt::new(binding);
        match storage.publish(binding.run, None, &record.encode()?) {
            PublicationDisposition::Durable(()) => Ok(Self {
                storage,
                record,
                uncertain: false,
            }),
            PublicationDisposition::NotApplied(failure) => Err(failure),
            PublicationDisposition::Indeterminate => Err(NativeFailure::Indeterminate),
        }
    }
    /// Loads protected inventory for the exact account, run and candidate.
    pub fn retain(storage: S, binding: InventoryBinding) -> Result<Self, NativeFailure> {
        let record = NativeReceipt::decode(&storage.read(binding.run)?)?;
        if record.binding != binding {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(Self {
            storage,
            record,
            uncertain: false,
        })
    }
    fn update(
        &mut self,
        change: impl FnOnce(&mut NativeReceipt) -> Result<(), NativeFailure>,
    ) -> Result<(), NativeFailure> {
        if self.uncertain {
            return Err(NativeFailure::Indeterminate);
        }
        let previous = self.record.encode()?;
        let mut next = self.record.clone();
        change(&mut next)?;
        match self
            .storage
            .publish(self.record.binding.run, Some(&previous), &next.encode()?)
        {
            PublicationDisposition::Durable(()) => {
                self.record = next;
                Ok(())
            }
            PublicationDisposition::NotApplied(failure) => Err(failure),
            PublicationDisposition::Indeterminate => {
                self.uncertain = true;
                Err(NativeFailure::Indeterminate)
            }
        }
    }
    /// Publishes pending intent. A preceding uncompleted creation prevents another effect.
    pub fn begin(&mut self, resource: InventoryResource) -> Result<(), NativeFailure> {
        self.update(|record| {
            if record
                .entries
                .iter()
                .any(|entry| entry.resource == resource)
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            if record
                .entries
                .iter()
                .any(|entry| entry.state == ResourceState::Pending)
            {
                return Err(NativeFailure::OutcomePending);
            }
            record.entries.push(Entry {
                resource,
                state: ResourceState::Pending,
                identity: None,
            });
            Ok(())
        })
    }
    /// Records the native owner's exact observed identity before another setup effect.
    pub fn record_identity(
        &mut self,
        resource: InventoryResource,
        identity: NativeEffectBinding,
    ) -> Result<(), NativeFailure> {
        identity.validate()?;
        if !identity.matches_resource(resource) {
            return Err(NativeFailure::InvalidRecord);
        }
        self.update(|record| {
            let entry = record
                .entries
                .iter_mut()
                .find(|entry| entry.resource == resource)
                .ok_or(NativeFailure::UnsafeNativeState)?;
            if entry.state != ResourceState::Pending || entry.identity.is_some() {
                return Err(NativeFailure::UnsafeNativeState);
            }
            entry.identity = Some(identity);
            Ok(())
        })
    }
    /// Records the native owner's observed completion, never completion from utility exit.
    pub fn completed(&mut self, resource: InventoryResource) -> Result<(), NativeFailure> {
        self.update(|record| {
            let entry = record
                .entries
                .iter_mut()
                .find(|entry| entry.resource == resource)
                .ok_or(NativeFailure::UnsafeNativeState)?;
            if entry.state != ResourceState::Pending {
                return Err(NativeFailure::UnsafeNativeState);
            }
            if entry.identity.is_none() {
                return Err(NativeFailure::CleanupUnresolved);
            }
            entry.state = ResourceState::Created;
            Ok(())
        })
    }
    /// Records removal after the production owner retained, revalidated and removed the exact resource.
    pub fn removed(&mut self, resource: InventoryResource) -> Result<(), NativeFailure> {
        self.update(|record| {
            let entry = record
                .entries
                .iter_mut()
                .find(|entry| entry.resource == resource)
                .ok_or(NativeFailure::UnsafeNativeState)?;
            if entry.identity.is_none() {
                return Err(NativeFailure::CleanupUnresolved);
            }
            entry.state = ResourceState::Removed;
            Ok(())
        })
    }
    /// Returns prior identity facts for revalidation by the production object/item owner.
    /// Equality alone never authorizes an effect or protects against replacement races.
    pub fn identity(
        &self,
        resource: InventoryResource,
    ) -> Result<&NativeEffectBinding, NativeFailure> {
        if self.uncertain {
            return Err(NativeFailure::Indeterminate);
        }
        self.record
            .entries
            .iter()
            .find(|entry| entry.resource == resource && entry.state != ResourceState::Removed)
            .and_then(|entry| entry.identity.as_ref())
            .ok_or(NativeFailure::CleanupUnresolved)
    }
    /// Counts completed creations for bounded status, not cleanup proof.
    pub fn created_resources(&self) -> usize {
        self.record
            .entries
            .iter()
            .filter(|entry| entry.state == ResourceState::Created)
            .count()
    }
}

// Frozen seed exports remain unavailable for old consumers. The judged scope
// does not build their generic cross-process receipt/cleanup authority.
/// Unavailable legacy cross-process cleanup authority. Inventory facts cannot construct it.
pub struct NativeCleanupBinding {
    _unavailable: Infallible,
}
impl fmt::Debug for NativeCleanupBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeCleanupBinding(..)")
    }
}
/// Legacy seed port, not the created-resource inventory or product cleanup owner.
pub trait NativeReceiptStore: Send + Sync {
    /// No generic receipt authority is issued in the judged scope.
    fn publish(&self, request: NativeRequestId) -> PublicationDisposition<NativeReceipt>;
    /// No generic receipt authority is loaded in the judged scope.
    fn retain(&self, request: NativeRequestId) -> Result<NativeReceipt, NativeFailure>;
    /// Use the bounded product-stored inventory for recorded identity facts instead.
    fn record_identity(
        &self,
        receipt: &mut NativeReceipt,
        identity: NativeEffectBinding,
    ) -> PublicationDisposition<()>;
    /// No inventory or output record can issue cross-process cleanup authority.
    fn retain_cleanup(
        &self,
        receipt: &NativeReceipt,
    ) -> Result<NativeCleanupBinding, NativeFailure>;
}
/// Fail-closed compatibility with the frozen seed declarations.
#[derive(Default)]
pub struct UnavailableNativeReceiptStore;
impl NativeReceiptStore for UnavailableNativeReceiptStore {
    fn publish(&self, _request: NativeRequestId) -> PublicationDisposition<NativeReceipt> {
        PublicationDisposition::NotApplied(NativeFailure::NativeUnavailable)
    }
    fn retain(&self, _request: NativeRequestId) -> Result<NativeReceipt, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
    fn record_identity(
        &self,
        _receipt: &mut NativeReceipt,
        _identity: NativeEffectBinding,
    ) -> PublicationDisposition<()> {
        PublicationDisposition::NotApplied(NativeFailure::NativeUnavailable)
    }
    fn retain_cleanup(
        &self,
        _receipt: &NativeReceipt,
    ) -> Result<NativeCleanupBinding, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}
