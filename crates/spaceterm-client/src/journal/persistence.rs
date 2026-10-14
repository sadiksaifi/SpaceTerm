//! Borrowed serialization of the snapshot apart from its comparison history.

use super::*;

impl RecoveredClientState {
    /// Serializes the schema-1 document with empty comparison-history
    /// arrays. Storage supplies those arrays from the same atomic snapshot
    /// before decoding and validating it. This fragment is not recovered authority.
    pub fn persistence_metadata(&self) -> impl Serialize + '_ {
        #[derive(Serialize)]
        struct Metadata<'a> {
            schema: u32,
            revision: ClientJournalRevision,
            bindings: &'a [SavedBinding],
            tombstones: [LocalPairingKey; 0],
            generations: [GenerationHighWater; 0],
            streams: [JournalStream; 0],
            operations: &'a [JournalOperation],
            grants: &'a [JournalGrant],
            suppressed_workspaces: &'a [WorkspaceSuppression],
            detaches: &'a [DetachTask],
            revocations: &'a [JournalRevocation],
            pairing: PairingMetadata<'a>,
            #[serde(with = "fixed_u64")]
            attachment_incarnation_high_water: u64,
            suppressed_attachments: &'a [JournalAttachment],
            remote_native_requests: &'a [RemoteNativeRequest],
        }
        #[derive(Serialize)]
        struct PairingMetadata<'a> {
            #[serde(with = "fixed_u64")]
            generation_high_water: u64,
            slots: [JournalCredentialSlot; 0],
            credential_preparations: &'a [CredentialReference],
            prepared: &'a [JournalEnrollment],
            acknowledgements: &'a [JournalEnrollmentAcknowledgement],
        }
        Metadata {
            schema: self.schema,
            revision: self.revision,
            bindings: &self.bindings,
            tombstones: [],
            generations: [],
            streams: [],
            operations: &self.operations,
            grants: &self.grants,
            suppressed_workspaces: &self.suppressed_workspaces,
            detaches: &self.detaches,
            revocations: &self.revocations,
            pairing: PairingMetadata {
                generation_high_water: self.pairing.generation_high_water(),
                slots: [],
                credential_preparations: self.pairing.credential_preparations(),
                prepared: self.pairing.prepared(),
                acknowledgements: self.pairing.acknowledgements(),
            },
            attachment_incarnation_high_water: self.attachment_incarnation_high_water,
            suppressed_attachments: &self.suppressed_attachments,
            remote_native_requests: &self.remote_native_requests,
        }
    }
}
