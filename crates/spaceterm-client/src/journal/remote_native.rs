//! Pending original remote native requests in the canonical client journal.
//! Encoded public native DTOs are comparison facts, never native or pairing authority.

use super::*;
use crate::failure::resource_limit;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum encoded public scope, intent, assessment or terminal progress size.
pub const MAX_REMOTE_NATIVE_BYTES: usize = 32 * 1024;
/// Pending requests are never evicted to admit another request.
pub const MAX_REMOTE_NATIVE_REQUESTS: usize = 256;
// JSON byte arrays need at most four bytes per byte. Reserve all four fields
// before the first effect, including future intent, assessment and terminal progress.
const RETENTION_RESERVATION: usize = 4 * MAX_REMOTE_NATIVE_BYTES * 4 + 1024;

/// One guest-issued original native request for one exact local pairing/client.
///
/// The client-native caller encodes only closed public native scope/capture,
/// original action intent, assessment and terminal progress DTOs and revalidates
/// them on recovery.
/// Credentials, tokens, environment values and paths must never enter these bytes.
/// Retention does not authorize replay, refresh confirmation or extend expiry.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteNativeRequest {
    local_pairing: LocalPairingKey,
    client: ClientId,
    request: [u8; 16],
    #[serde(deserialize_with = "native_bytes")]
    original_scope: Vec<u8>,
    #[serde(default, deserialize_with = "optional_native_bytes")]
    intent: Option<Vec<u8>>,
    #[serde(deserialize_with = "optional_native_bytes")]
    assessment: Option<Vec<u8>>,
    submitted: bool,
    #[serde(deserialize_with = "optional_native_bytes")]
    result: Option<Vec<u8>>,
}
impl std::fmt::Debug for RemoteNativeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteNativeRequest(..)")
    }
}
impl RemoteNativeRequest {
    pub fn new(
        local_pairing: LocalPairingKey,
        client: ClientId,
        request: [u8; 16],
        original_scope: Vec<u8>,
    ) -> Result<Self, ClientFailure> {
        let record = Self {
            local_pairing,
            client,
            request,
            original_scope,
            intent: None,
            assessment: None,
            submitted: false,
            result: None,
        };
        record.validate()?;
        Ok(record)
    }
    pub fn local_pairing(&self) -> &LocalPairingKey {
        &self.local_pairing
    }
    pub fn client(&self) -> ClientId {
        self.client
    }
    pub fn request_id(&self) -> [u8; 16] {
        self.request
    }
    pub fn original_scope(&self) -> &[u8] {
        &self.original_scope
    }
    /// The immutable public action intent retained before native Capture.
    pub fn intent(&self) -> Option<&[u8]> {
        self.intent.as_deref()
    }
    pub fn assessment(&self) -> Option<&[u8]> {
        self.assessment.as_deref()
    }
    /// Durable foreground confirmation/submission intent, not current authority.
    pub fn submitted(&self) -> bool {
        self.submitted
    }
    /// The exact terminal native progress DTO retained before guest acknowledgement.
    pub fn result(&self) -> Option<&[u8]> {
        self.result.as_deref()
    }
    fn validate(&self) -> Result<(), ClientFailure> {
        validate_identity(&self.local_pairing, self.client, self.request)?;
        validate_bytes(&self.original_scope)?;
        for bytes in [
            self.intent.as_deref(),
            self.assessment.as_deref(),
            self.result.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_bytes(bytes)?;
        }
        Ok(())
    }
    fn matches(&self, pairing: &LocalPairingKey, client: ClientId, request: [u8; 16]) -> bool {
        &self.local_pairing == pairing && self.client == client && self.request == request
    }
}

/// Why a submitted original has no usable canonical recovery references.
/// These classifications do not authorize reconstruction or another pairing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteNativeOriginalBlocker {
    MissingIntent,
    MissingAssessment,
    MissingOriginalTarget,
    ConflictingOriginalTarget,
    MissingOriginalCredential,
    OriginalCredentialMismatch,
    MissingOriginalSshCapture,
    UnsupportedOriginalRoute,
}

/// Presence of canonical references only, not native DTO, proof, SSH configuration,
/// credential availability, connection readiness or admission validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteNativeOriginalState {
    Available,
    Blocked(RemoteNativeOriginalBlocker),
}

/// A submitted original remains visible even after cleanup removes its recovery
/// references. Unsubmitted captures are not submitted-original pending work.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteNativeOriginalPending {
    local_pairing: LocalPairingKey,
    client: ClientId,
    request: [u8; 16],
    state: RemoteNativeOriginalState,
    result_retained: bool,
}
impl RemoteNativeOriginalPending {
    pub fn local_pairing(&self) -> &LocalPairingKey {
        &self.local_pairing
    }
    pub fn client(&self) -> ClientId {
        self.client
    }
    pub fn request(&self) -> [u8; 16] {
        self.request
    }
    pub fn state(&self) -> RemoteNativeOriginalState {
        self.state
    }
    /// Encoded result presence only. The native consumer validates terminality.
    pub fn result_retained(&self) -> bool {
        self.result_retained
    }
}
impl std::fmt::Debug for RemoteNativeOriginalPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteNativeOriginalPending(..)")
    }
}

/// Comparison-only custody of one submitted original in one canonical journal.
///
/// Construction is journal-owned. No actor, ordinary pairing authority, signer,
/// connection, request allocator, Capture, Assess or Admit is exposed. The native
/// consumer validates the closed original scope, intent, assessment and result
/// grammar and their account/build/pin/digest comparisons before using a transport
/// or persisting a result. This port never validates or sends a native effect.
///
/// Accessors describe the historical original, not a current paired connection.
/// Call validate/snapshot before and after each external recovery operation.
/// Clones share canonical custody; missing or changed facts invalidate every clone.
#[derive(Clone)]
pub struct HistoricalRemoteNativePort {
    journal: std::sync::Arc<JournalController>,
    target: crate::api::ConnectionTarget,
    credential: JournalCredentialSlot,
    original: RemoteNativeRequest,
}
impl std::fmt::Debug for HistoricalRemoteNativePort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoricalRemoteNativePort(..)")
    }
}
impl HistoricalRemoteNativePort {
    pub fn target(&self) -> &crate::api::ConnectionTarget {
        &self.target
    }
    pub fn credential(&self) -> &JournalCredentialSlot {
        &self.credential
    }
    pub fn ssh_capture(&self) -> &JournalSshCapture {
        self.credential
            .ssh_capture
            .as_ref()
            .expect("historical port construction requires its original SSH capture")
    }
    pub fn local_pairing(&self) -> &LocalPairingKey {
        self.original.local_pairing()
    }
    pub fn client(&self) -> ClientId {
        self.original.client()
    }
    pub fn request(&self) -> [u8; 16] {
        self.original.request_id()
    }

    /// Proves exact canonical owner identity without exposing a journal mutation port.
    pub fn validate_journal(
        &self,
        expected: &std::sync::Arc<JournalController>,
    ) -> Result<(), ClientFailure> {
        if !std::sync::Arc::ptr_eq(&self.journal, expected) {
            return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
        }
        self.validate()
    }
    pub fn validate(&self) -> Result<(), ClientFailure> {
        self.snapshot().map(|_| ())
    }
    /// Rechecks a fresh healthy snapshot. Only a previously absent exact result
    /// may appear; the original scope, intent, assessment and provenance cannot change.
    pub fn snapshot(&self) -> Result<RemoteNativeRequest, ClientFailure> {
        let state = self.journal.snapshot()?;
        self.validate_in(&state).cloned()
    }
    fn validate_in<'a>(
        &self,
        state: &'a RecoveredClientState,
    ) -> Result<&'a RemoteNativeRequest, ClientFailure> {
        let stale = || ClientFailure::new(ClientFailureClass::StaleAuthority);
        let record = state
            .remote_native_requests
            .iter()
            .find(|record| record.matches(self.local_pairing(), self.client(), self.request()))
            .ok_or_else(stale)?;
        let (target, credential) = historical_material(state, record).map_err(|_| stale())?;
        if target != self.target
            || credential != self.credential
            || record.original_scope != self.original.original_scope
            || record.intent != self.original.intent
            || record.assessment != self.original.assessment
            || record.submitted != self.original.submitted
            || self
                .original
                .result
                .as_ref()
                .is_some_and(|original| record.result.as_ref() != Some(original))
        {
            return Err(stale());
        }
        Ok(record)
    }
    /// Retains only the native-validated exact terminal result. This grants no
    /// network/ACK permission. Provenance is checked inside the serialized write.
    pub async fn retain_result(&self, result: &[u8]) -> Result<JournalReceipt, ClientFailure> {
        validate_bytes(result)?;
        let result = result.to_vec();
        let original = self.clone();
        self.journal
            .update(move |state| {
                let record = original.validate_in(state)?;
                Ok((
                    vec![ClientJournalChange::CompleteRemoteNativeRequest {
                        pairing: record.local_pairing.clone(),
                        client: record.client,
                        request: record.request,
                        original_scope: record.original_scope.clone(),
                        result,
                    }],
                    (),
                ))
            })
            .await
            .map(|(receipt, ())| receipt)
    }
    /// The native consumer calls this only after exact guest acknowledgement of
    /// a separately durable result. Never marks submission, admits or resends work.
    pub async fn retire_after_acknowledgement(
        &self,
        result: &[u8],
    ) -> Result<JournalReceipt, ClientFailure> {
        validate_bytes(result)?;
        let result = result.to_vec();
        let original = self.clone();
        self.journal
            .update(move |state| {
                let record = original.validate_in(state)?;
                if record.result.as_deref() != Some(result.as_slice()) {
                    return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
                }
                Ok((
                    vec![ClientJournalChange::RetireRemoteNativeRequest {
                        pairing: record.local_pairing.clone(),
                        client: record.client,
                        request: record.request,
                        original_scope: record.original_scope.clone(),
                        result,
                    }],
                    (),
                ))
            })
            .await
            .map(|(receipt, ())| receipt)
    }
}

// Uses only one already-validated canonical snapshot. In particular, never select
// a newer binding/slot by server alone after the original binding was removed.
fn historical_material(
    state: &RecoveredClientState,
    record: &RemoteNativeRequest,
) -> Result<(crate::api::ConnectionTarget, JournalCredentialSlot), RemoteNativeOriginalBlocker> {
    use RemoteNativeOriginalBlocker::*;
    if record.intent.is_none() {
        return Err(MissingIntent);
    }
    if record.assessment.is_none() || !record.submitted {
        return Err(MissingAssessment);
    }
    let mut target = None;
    let mut original_route = None;
    for saved in &state.bindings {
        let candidate = saved.target.target().map_err(|_| MissingOriginalTarget)?;
        if candidate.client() != record.client
            || candidate.local_pairing().as_ref() != Ok(&record.local_pairing)
        {
            continue;
        }
        if target.replace(candidate).is_some() {
            return Err(ConflictingOriginalTarget);
        }
    }
    for revocation in &state.revocations {
        let task = revocation.task.parts();
        if task.local_pairing != record.local_pairing || task.original.client() != record.client {
            continue;
        }
        let candidate = revocation
            .target
            .target()
            .map_err(|_| MissingOriginalTarget)?;
        if candidate.local_pairing().as_ref() != Ok(&record.local_pairing)
            || candidate.client() != record.client
            || candidate.server() != task.original.server()
            || candidate.pairing() != task.original.pairing()
            || candidate.device() != task.original.device()
            || candidate.credential() != task.original.credential()
            || candidate.pin().public_key != task.pinned_server_public_key
        {
            return Err(ConflictingOriginalTarget);
        }
        if target.replace(candidate).is_some() {
            return Err(ConflictingOriginalTarget);
        }
        original_route = Some(task.original.route());
    }
    let target = target.ok_or(MissingOriginalTarget)?;
    if !matches!(target.route().parts(), crate::api::RouteParts::Ssh { .. }) {
        return Err(UnsupportedOriginalRoute);
    }
    let credential = state
        .pairing
        .slot(target.credential())
        .ok_or(MissingOriginalCredential)?;
    if credential.server != Some(target.server())
        || credential.generation != record.local_pairing.parts().generation
        || credential.public_key.is_none_or(|key| key == [0; 32])
        || credential.route_reference.is_none()
        || original_route.is_some_and(|route| credential.route_reference != Some(route))
        || credential
            .route
            .as_ref()
            .is_some_and(|route| route != target.route())
    {
        return Err(OriginalCredentialMismatch);
    }
    if credential.ssh_capture.is_none() {
        return Err(MissingOriginalSshCapture);
    }
    Ok((target, credential.clone()))
}

impl RecoveredClientState {
    pub fn remote_native_requests(&self) -> &[RemoteNativeRequest] {
        &self.remote_native_requests
    }
}

impl JournalController {
    /// Recovers only comparison custody for this exact submitted original.
    /// All target/slot/capture facts come from one healthy canonical snapshot;
    /// callers cannot supply a binding, scope, pin, route or replacement capture.
    /// None means no usable port, including a blocked submitted original. The
    /// PendingTasks enumeration preserves blocked originals for presentation.
    /// Native consumers still validate every closed DTO and original digest.
    pub fn recover_remote_native_original(
        self: &std::sync::Arc<Self>,
        pairing: &LocalPairingKey,
        client: ClientId,
        request: [u8; 16],
    ) -> Result<Option<HistoricalRemoteNativePort>, ClientFailure> {
        validate_identity(pairing, client, request)?;
        let state = self.snapshot()?;
        let Some(original) = state
            .remote_native_requests
            .iter()
            .find(|record| record.matches(pairing, client, request) && record.submitted)
        else {
            return Ok(None);
        };
        let Ok((target, credential)) = historical_material(&state, original) else {
            return Ok(None);
        };
        Ok(Some(HistoricalRemoteNativePort {
            journal: self.clone(),
            target,
            credential,
            original: original.clone(),
        }))
    }

    pub(crate) fn pending_remote_native_originals(
        &self,
    ) -> Result<Vec<RemoteNativeOriginalPending>, ClientFailure> {
        let snapshot = self.snapshot()?;
        Ok(snapshot
            .remote_native_requests
            .iter()
            .filter(|record| record.submitted)
            .map(|record| {
                let state = match historical_material(&snapshot, record) {
                    Ok(_) => RemoteNativeOriginalState::Available,
                    Err(reason) => RemoteNativeOriginalState::Blocked(reason),
                };
                RemoteNativeOriginalPending {
                    local_pairing: record.local_pairing.clone(),
                    client: record.client,
                    request: record.request,
                    state,
                    result_retained: record.result.is_some(),
                }
            })
            .collect())
    }

    /// Retains the original guest ID and scope before any network/native effect.
    /// Existing IDs accept only their original scope and preserve later facts.
    pub async fn retain_remote_native_request(
        &self,
        record: RemoteNativeRequest,
    ) -> Result<JournalReceipt, ClientFailure> {
        let controller = self.clone();
        self.update(move |state| {
            let binding = active_binding(state, &record.local_pairing, record.client)?;
            if !controller.ordinary_use_allowed_in(state, &binding)? {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            Ok((
                vec![ClientJournalChange::RetainRemoteNativeRequest(record)],
                (),
            ))
        })
        .await
        .map(|(receipt, ())| receipt)
    }
    /// Retains the original public action intent before native Capture.
    /// Only exact replay is accepted after the first intent is durable.
    /// A first intent cannot be added after assessment, submission or completion.
    pub async fn retain_remote_native_intent(
        &self,
        pairing: LocalPairingKey,
        client: ClientId,
        request: [u8; 16],
        original_scope: Vec<u8>,
        intent: Vec<u8>,
    ) -> Result<JournalReceipt, ClientFailure> {
        let controller = self.clone();
        self.update(move |state| {
            let binding = active_binding(state, &pairing, client)?;
            if !controller.ordinary_use_allowed_in(state, &binding)? {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            Ok((
                vec![ClientJournalChange::RetainRemoteNativeIntent {
                    pairing,
                    client,
                    request,
                    original_scope,
                    intent,
                }],
                (),
            ))
        })
        .await
        .map(|(receipt, ())| receipt)
    }
    /// Keeps the original assessment, including its original expiry, unchanged.
    pub async fn retain_remote_native_assessment(
        &self,
        pairing: LocalPairingKey,
        client: ClientId,
        request: [u8; 16],
        original_scope: Vec<u8>,
        assessment: Vec<u8>,
    ) -> Result<JournalReceipt, ClientFailure> {
        self.transact(vec![ClientJournalChange::RetainRemoteNativeAssessment {
            pairing,
            client,
            request,
            original_scope,
            assessment,
        }])
        .await
    }
    /// Retains foreground confirmation/submission before paired Admit. The caller
    /// must still validate the exact original public DTOs and live authority.
    pub async fn mark_remote_native_submitted(
        &self,
        pairing: LocalPairingKey,
        client: ClientId,
        request: [u8; 16],
        original_scope: Vec<u8>,
        assessment: Option<Vec<u8>>,
    ) -> Result<JournalReceipt, ClientFailure> {
        let controller = self.clone();
        self.update(move |state| {
            let binding = active_binding(state, &pairing, client)?;
            if !controller.ordinary_use_allowed_in(state, &binding)? {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            Ok((
                vec![ClientJournalChange::SubmitRemoteNativeRequest {
                    pairing,
                    client,
                    request,
                    original_scope,
                    assessment,
                }],
                (),
            ))
        })
        .await
        .map(|(receipt, ())| receipt)
    }
    /// Retains the exact terminal result before acknowledging the guest request.
    /// In-flight progress is not a terminal result and must not be stored here.
    pub async fn retain_remote_native_result(
        &self,
        pairing: LocalPairingKey,
        client: ClientId,
        request: [u8; 16],
        original_scope: Vec<u8>,
        result: Vec<u8>,
    ) -> Result<JournalReceipt, ClientFailure> {
        self.transact(vec![ClientJournalChange::CompleteRemoteNativeRequest {
            pairing,
            client,
            request,
            original_scope,
            result,
        }])
        .await
    }
    /// Recovers comparison records only for the exact original pairing/client.
    /// Records survive unpair; they cannot authorize a newer binding's effects.
    pub fn recover_remote_native_requests(
        &self,
        pairing: &LocalPairingKey,
        client: ClientId,
    ) -> Result<Vec<RemoteNativeRequest>, ClientFailure> {
        pairing.validate().map_err(|_| invalid())?;
        if client.server() != pairing.parts().server {
            return Err(invalid());
        }
        Ok(self
            .snapshot()?
            .remote_native_requests
            .into_iter()
            .filter(|record| &record.local_pairing == pairing && record.client == client)
            .collect())
    }
    /// Call only after guest acknowledgement of the retained terminal result.
    /// Both original scope and result must match an earlier durable snapshot.
    pub async fn retire_remote_native_request(
        &self,
        pairing: LocalPairingKey,
        client: ClientId,
        request: [u8; 16],
        original_scope: Vec<u8>,
        result: Vec<u8>,
    ) -> Result<JournalReceipt, ClientFailure> {
        self.transact(vec![ClientJournalChange::RetireRemoteNativeRequest {
            pairing,
            client,
            request,
            original_scope,
            result,
        }])
        .await
    }
}

pub(super) fn validate_retained(
    state: &RecoveredClientState,
    limits: JournalLimits,
) -> Result<usize, ClientFailure> {
    if state.remote_native_requests.len() > MAX_REMOTE_NATIVE_REQUESTS {
        return Err(resource_limit());
    }
    let mut originals = BTreeSet::new();
    let mut counts = BTreeMap::new();
    for record in &state.remote_native_requests {
        record.validate()?;
        let parts = record.local_pairing.parts();
        let key = (
            parts.server,
            parts.pairing,
            parts.generation.get(),
            record.client,
        );
        if !originals.insert((key, record.request))
            || !state.streams.iter().any(|header| {
                header.local_pairing == record.local_pairing
                    && header.stream.client() == record.client
                    && header.purpose == StreamPurpose::Ordinary
            })
        {
            return Err(invalid());
        }
        let count = counts.entry(key).or_insert(0usize);
        *count += 1;
        if *count > limits.operations_per_stream {
            return Err(resource_limit());
        }
    }
    state
        .remote_native_requests
        .len()
        .checked_mul(RETENTION_RESERVATION)
        .ok_or_else(resource_limit)
}

// Retirement cannot erase a result written earlier in the same transaction.
// The guest acknowledgement must follow a separately durable result commit.
pub(super) fn validate_retirements(
    state: &RecoveredClientState,
    changes: &[ClientJournalChange],
) -> Result<(), ClientFailure> {
    for change in changes {
        if let ClientJournalChange::RetireRemoteNativeRequest {
            pairing,
            client,
            request,
            original_scope,
            result,
        } = change
        {
            let record = find(state, pairing, *client, *request, original_scope)?;
            if record.result.as_ref() != Some(result) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

pub(super) fn apply(
    state: &mut RecoveredClientState,
    change: &ClientJournalChange,
) -> Result<bool, ClientFailure> {
    match change {
        ClientJournalChange::RetainRemoteNativeRequest(record) => {
            record.validate()?;
            if record.intent.is_some()
                || record.assessment.is_some()
                || record.submitted
                || record.result.is_some()
            {
                return Err(invalid());
            }
            active_binding(state, &record.local_pairing, record.client)?;
            if let Some(old) = state
                .remote_native_requests
                .iter()
                .find(|old| old.matches(&record.local_pairing, record.client, record.request))
            {
                if old.original_scope != record.original_scope {
                    return Err(invalid());
                }
            } else {
                state.remote_native_requests.push(record.clone());
            }
        }
        ClientJournalChange::RetainRemoteNativeIntent {
            pairing,
            client,
            request,
            original_scope,
            intent,
        } => {
            validate_bytes(intent)?;
            active_binding(state, pairing, *client)?;
            let record = find_mut(state, pairing, *client, *request, original_scope)?;
            if let Some(old) = &record.intent {
                if old != intent {
                    return Err(invalid());
                }
            } else {
                if record.assessment.is_some() || record.submitted || record.result.is_some() {
                    return Err(invalid());
                }
                record.intent = Some(intent.clone());
            }
        }
        ClientJournalChange::RetainRemoteNativeAssessment {
            pairing,
            client,
            request,
            original_scope,
            assessment,
        } => {
            validate_bytes(assessment)?;
            let record = find_mut(state, pairing, *client, *request, original_scope)?;
            if let Some(old) = &record.assessment {
                if old != assessment {
                    return Err(invalid());
                }
            } else {
                if record.submitted || record.result.is_some() {
                    return Err(invalid());
                }
                record.assessment = Some(assessment.clone());
            }
        }
        ClientJournalChange::SubmitRemoteNativeRequest {
            pairing,
            client,
            request,
            original_scope,
            assessment,
        } => {
            active_binding(state, pairing, *client)?;
            let record = find_mut(state, pairing, *client, *request, original_scope)?;
            if &record.assessment != assessment || record.result.is_some() {
                return Err(invalid());
            }
            record.submitted = true;
        }
        ClientJournalChange::CompleteRemoteNativeRequest {
            pairing,
            client,
            request,
            original_scope,
            result,
        } => {
            validate_bytes(result)?;
            let record = find_mut(state, pairing, *client, *request, original_scope)?;
            if let Some(old) = &record.result {
                if old != result {
                    return Err(invalid());
                }
            } else {
                record.result = Some(result.clone());
            }
        }
        ClientJournalChange::RetireRemoteNativeRequest {
            pairing,
            client,
            request,
            original_scope,
            result,
        } => {
            let record = find(state, pairing, *client, *request, original_scope)?;
            if record.result.as_ref() != Some(result) {
                return Err(invalid());
            }
            state
                .remote_native_requests
                .retain(|record| !record.matches(pairing, *client, *request));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn active_binding(
    state: &RecoveredClientState,
    pairing: &LocalPairingKey,
    client: ClientId,
) -> Result<PairedServerBinding, ClientFailure> {
    state
        .bindings
        .iter()
        .filter_map(|saved| saved.binding().ok())
        .find(|binding| {
            binding.target().client() == client
                && binding.target().local_pairing().as_ref() == Ok(pairing)
                && state.ordinary_use_allowed(binding)
        })
        .ok_or_else(|| ClientFailure::new(ClientFailureClass::StaleAuthority))
}
fn find<'a>(
    state: &'a RecoveredClientState,
    pairing: &LocalPairingKey,
    client: ClientId,
    request: [u8; 16],
    scope: &[u8],
) -> Result<&'a RemoteNativeRequest, ClientFailure> {
    state
        .remote_native_requests
        .iter()
        .find(|record| record.matches(pairing, client, request) && record.original_scope == scope)
        .ok_or_else(invalid)
}
fn find_mut<'a>(
    state: &'a mut RecoveredClientState,
    pairing: &LocalPairingKey,
    client: ClientId,
    request: [u8; 16],
    scope: &[u8],
) -> Result<&'a mut RemoteNativeRequest, ClientFailure> {
    state
        .remote_native_requests
        .iter_mut()
        .find(|record| record.matches(pairing, client, request) && record.original_scope == scope)
        .ok_or_else(invalid)
}
fn validate_identity(
    pairing: &LocalPairingKey,
    client: ClientId,
    request: [u8; 16],
) -> Result<(), ClientFailure> {
    pairing.validate().map_err(|_| invalid())?;
    if client.server() != pairing.parts().server || request == [0; 16] {
        return Err(invalid());
    }
    Ok(())
}
fn validate_bytes(bytes: &[u8]) -> Result<(), ClientFailure> {
    if bytes.is_empty() {
        return Err(invalid());
    }
    if bytes.len() > MAX_REMOTE_NATIVE_BYTES {
        return Err(resource_limit());
    }
    Ok(())
}
fn native_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    super::bounded::sequence::<D, u8, MAX_REMOTE_NATIVE_BYTES>(d)
}
fn optional_native_bytes<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<u8>>, D::Error> {
    #[derive(Deserialize)]
    struct Bytes(#[serde(deserialize_with = "native_bytes")] Vec<u8>);
    Option::<Bytes>::deserialize(d).map(|value| value.map(|bytes| bytes.0))
}
