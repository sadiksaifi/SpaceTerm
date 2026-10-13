use super::*;
use crate::view::{AttachmentIncarnation, AttachmentSuppression, WorkspaceAttachment};

/// Durable comparison facts, not a second attachment allocator.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalAttachment {
    pub(super) local_pairing: LocalPairingKey,
    pub(super) workspace: WorkspaceId,
    pub(super) client: ClientId,
    #[serde(with = "fixed_u64")]
    pub(super) incarnation: u64,
}
impl JournalAttachment {
    pub fn new(
        local_pairing: LocalPairingKey,
        attachment: WorkspaceAttachment,
    ) -> Result<Self, ClientFailure> {
        if local_pairing.parts().pairing != attachment.pairing() {
            return Err(invalid());
        }
        let marker = Self {
            local_pairing,
            workspace: attachment.workspace(),
            client: attachment.client(),
            incarnation: attachment.incarnation().get(),
        };
        marker.validate()?;
        Ok(marker)
    }
    pub(super) fn validate(&self) -> Result<(), ClientFailure> {
        self.local_pairing.validate().map_err(|_| invalid())?;
        if self.workspace.server() != self.local_pairing.parts().server
            || self.client.server() != self.workspace.server()
            || self.incarnation == 0
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn belongs_to_active_binding(&self, state: &RecoveredClientState) -> bool {
        state.bindings().iter().any(|saved| {
            saved.binding().is_ok_and(|binding| {
                state.ordinary_use_allowed(&binding)
                    && binding.target().client() == self.client
                    && binding
                        .target()
                        .local_pairing()
                        .is_ok_and(|key| key == self.local_pairing)
            })
        })
    }
    fn matches(&self, attachment: &WorkspaceAttachment) -> bool {
        self.workspace == attachment.workspace()
            && self.client == attachment.client()
            && self.local_pairing.parts().pairing == attachment.pairing()
            && attachment.incarnation().get() <= self.incarnation
    }
}
impl AttachmentSuppression for RecoveredClientState {
    fn suppresses(&self, attachment: &WorkspaceAttachment) -> bool {
        self.tombstones
            .iter()
            .any(|key| key.parts().pairing == attachment.pairing())
            || self
                .suppressed_attachments
                .iter()
                .any(|marker| marker.matches(attachment))
            || self.suppressed_workspaces.iter().any(|marker| {
                marker.workspace == attachment.workspace()
                    && marker.local_pairing.parts().pairing == attachment.pairing()
            })
    }
    fn incarnation_floor(&self) -> Option<AttachmentIncarnation> {
        AttachmentIncarnation::new(self.attachment_incarnation_high_water)
    }
}
impl JournalController {
    /// Persist the view owner's newly allocated incarnation before exposing a
    /// Pane lease. Old deferred-release barriers stay in force after reattach.
    pub async fn record_attachment(
        &self,
        pairing: LocalPairingKey,
        attachment: WorkspaceAttachment,
    ) -> Result<JournalReceipt, ClientFailure> {
        self.transact(vec![ClientJournalChange::RecordAttachment(
            JournalAttachment::new(pairing, attachment)?,
        )])
        .await
    }
}
