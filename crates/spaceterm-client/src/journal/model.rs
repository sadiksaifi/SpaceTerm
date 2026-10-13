use super::*;
use crate::failure::{exhausted, resource_limit};
use spaceterm_core::{
    operation_ledger_contracts::{
        CanonicalOperationRequest, MAX_CANONICAL_REQUEST_BYTES, MAX_COMPACT_OUTCOME_BYTES,
    },
    operations::{Command, OperationFailure, Outcome},
    pairing::{PairingCommand, PairingTarget, PairingTargetParts},
};
use std::collections::BTreeSet;

const MAX_HEADERS: usize = 4096;
const RELEASE_RESERVATION: usize = 2048;
// One slot covers fixed cleanup metadata, including the Workspace suppression
// marker and detach task. Their per-grant lists use the reserved cleanup slots.
const CLEANUP_BOOKKEEPING_RESERVATION: usize = 256;
const REVOCATION_RESERVATION: usize = super::pairing_projection::PAIRING_RECORD_RESERVATION;

pub(super) fn validate(
    state: &RecoveredClientState,
    codec: &dyn LedgerCodec,
    verifier: &dyn ReceiptVerifier,
    limits: JournalLimits,
) -> Result<(), ClientFailure> {
    limits.validate()?;
    if state.schema != CLIENT_JOURNAL_SCHEMA {
        return Err(JournalFailure::UnknownSchema.into());
    }
    for (count, ceiling) in [
        (state.bindings.len(), limits.saved_servers),
        (state.streams.len(), MAX_HEADERS),
        (state.generations.len(), MAX_HEADERS),
        (state.tombstones.len(), MAX_HEADERS),
        (state.suppressed_workspaces.len(), MAX_HEADERS),
        (state.detaches.len(), MAX_HEADERS),
        (state.suppressed_attachments.len(), MAX_HEADERS),
        (state.revocations.len(), limits.pending_revocations),
        (state.grants.len(), limits.cleanup_grants),
    ] {
        if count > ceiling {
            return Err(resource_limit());
        }
    }
    let mut attachments = BTreeSet::new();
    for marker in &state.suppressed_attachments {
        marker.validate()?;
        if marker.incarnation > state.attachment_incarnation_high_water
            || !generation_retained(state, &marker.local_pairing)
            || !attachments.insert((marker.workspace, marker.client, marker.incarnation))
        {
            return Err(invalid());
        }
    }
    let mut generations = BTreeSet::new();
    for mark in &state.generations {
        if !generations.insert(mark.server) {
            return Err(invalid());
        }
    }
    let mut tombstones = BTreeSet::new();
    for key in &state.tombstones {
        key.validate().map_err(|_| invalid())?;
        if !tombstones.insert(key_tuple(key)) || !generation_retained(state, key) {
            return Err(invalid());
        }
    }
    let mut servers = BTreeSet::new();
    let mut reserved_streams = BTreeSet::new();
    if state
        .bindings
        .len()
        .checked_add(state.revocations.len())
        .is_none_or(|n| n > limits.pending_revocations)
    {
        return Err(resource_limit());
    }
    let permanent = state
        .streams
        .len()
        .checked_add(state.tombstones.len())
        .and_then(|n| n.checked_add(state.generations.len()))
        .ok_or_else(resource_limit)?;
    let mut resident = charge(
        permanent.checked_mul(256).ok_or_else(resource_limit)?,
        state.pairing.validate(state, verifier, limits)?,
    )?;
    for saved in &state.bindings {
        saved.validate(verifier)?;
        let binding = saved.binding()?;
        let target = binding.target();
        if !servers.insert(target.server())
            || !state.ordinary_use_allowed(&binding)
            || !generation_retained(state, &target.local_pairing()?)
            || !reserved_streams.insert(saved.streams.parts().self_revocation.key())
            || state
                .stream(saved.streams.parts().ordinary)
                .is_none_or(|header| header.purpose != StreamPurpose::Ordinary)
            || state
                .stream(saved.streams.parts().self_revocation)
                .is_none_or(|header| {
                    header.purpose != StreamPurpose::SelfRevocation || header.high_water != 0
                })
        {
            return Err(invalid());
        }
        resident = charge(resident, REVOCATION_RESERVATION)?;
    }
    let mut stream_keys = BTreeSet::new();
    for stream in &state.streams {
        if !generation_retained(state, &stream.local_pairing)
            || stream.local_pairing.parts().pairing != stream.stream.pairing()
            || !stream_keys.insert(stream.stream.key())
            || stream.confirmed_prefix > stream.durable_prefix
            || stream.durable_prefix > stream.high_water
        {
            return Err(invalid());
        }
        if stream.purpose == StreamPurpose::SelfRevocation {
            if stream.high_water > 1 || (stream.high_water == 1 && !stream.retired) {
                return Err(invalid());
            }
            let pending = state
                .revocations
                .iter()
                .find(|record| record.task.parts().original.operation().stream() == stream.stream);
            if stream.high_water == 1 && pending.is_none() && stream.confirmed_prefix != 1 {
                return Err(invalid());
            }
            if let Some(record) = pending {
                let durable = u64::from(matches!(record.phase, RevocationPhase::CleanupPending(_)));
                if stream.durable_prefix != durable {
                    return Err(invalid());
                }
            }
            continue;
        }
        if stream.purpose != StreamPurpose::Ordinary {
            return Err(invalid());
        }
        let mut entries: Vec<_> = state
            .operations
            .iter()
            .filter(|entry| entry.binding.id.stream() == stream.stream)
            .collect();
        entries.sort_by_key(|entry| entry.binding.id.sequence());
        let mut expected = stream.confirmed_prefix;
        let mut prefix = stream.confirmed_prefix;
        let mut gap = false;
        for entry in entries
            .iter()
            .filter(|entry| entry.binding.id.sequence().get() > stream.confirmed_prefix)
        {
            expected = expected.checked_add(1).ok_or_else(invalid)?;
            if entry.binding.id.sequence().get() != expected {
                return Err(invalid());
            }
            if !gap && matches!(entry.state, RetainedOperationState::Completed { .. }) {
                prefix = expected;
            } else {
                gap = true;
            }
        }
        if expected != stream.high_water || prefix != stream.durable_prefix {
            return Err(invalid());
        }
        let ordinary: Vec<_> = entries.iter().filter(|entry| !entry.cleanup).collect();
        if ordinary.len() > limits.operations_per_stream {
            return Err(resource_limit());
        }
        let request_bytes = ordinary
            .iter()
            .try_fold(0usize, |sum, entry| charge(sum, entry.request.len()))?;
        if request_bytes > limits.request_bytes_per_stream {
            return Err(resource_limit());
        }
    }
    let mut operations = BTreeSet::new();
    let mut grant_keys = BTreeSet::new();
    let mut cleanup_workspaces: BTreeSet<_> = state
        .suppressed_workspaces
        .iter()
        .map(|marker| (key_tuple(&marker.local_pairing), marker.workspace))
        .chain(
            state
                .suppressed_attachments
                .iter()
                .map(|marker| (key_tuple(&marker.local_pairing), marker.workspace)),
        )
        .collect();
    let mut unknown_workspaces = 0;
    for grant in &state.grants {
        cleanup_workspaces.insert((key_tuple(&grant.local_pairing), grant.workspace));
        let header = state
            .streams
            .iter()
            .find(|header| {
                header.local_pairing == grant.local_pairing
                    && header.purpose == StreamPurpose::Ordinary
            })
            .ok_or_else(invalid)?;
        if grant.control.grant().client() != header.stream.client()
            || grant.workspace.server() != header.stream.server()
            || grant.publication_revision.get() == 0
            || !grant_keys.insert((
                key_tuple(&grant.local_pairing),
                grant.control.epoch().get(),
                grant.control.grant().pane(),
                grant.control.grant().generation().get(),
            ))
        {
            return Err(invalid());
        }
    }
    let mut reserved_grants = grant_keys;
    let mut unresolved_grants = 0;
    let mut release_requests = 0;
    let mut release_request_bytes = 0;
    for header in &state.streams {
        if header.purpose == StreamPurpose::Ordinary {
            release_request_bytes =
                release_request_bytes.max(release_request_reservation(header.stream, codec)?);
        }
    }
    let release_capacity = charge(RELEASE_RESERVATION, release_request_bytes)?;
    for entry in &state.operations {
        let stream = state
            .stream(entry.binding.id.stream())
            .ok_or_else(invalid)?;
        if stream.purpose != StreamPurpose::Ordinary
            || !operations.insert(entry.binding.id)
            || entry.binding.id.sequence().get() > stream.high_water
            || (entry.binding.id.sequence().get() <= stream.durable_prefix
                && !matches!(entry.state, RetainedOperationState::Completed { .. }))
        {
            return Err(invalid());
        }
        let request =
            CanonicalOperationRequest::decode(&entry.request, codec).map_err(|_| invalid())?;
        request
            .verify_binding(entry.binding)
            .map_err(|_| invalid())?;
        if request.encode(codec).map_err(|_| invalid())? != entry.request
            || entry
                .workspace
                .is_some_and(|workspace| workspace.server() != entry.binding.id.stream().server())
            || entry.reserves_grant != grant_producing(request.command())
            || (entry.workspace.is_none()
                && starts_or_grants(request.command())
                && !matches!(request.command(), Command::CreateWorkspace { .. }))
            || matches!(request.command(), Command::CreateTab { workspace, .. } | Command::RenameWorkspace { workspace, .. } if Some(*workspace) != entry.workspace)
        {
            return Err(invalid());
        }
        // Self-revocation/enrollment cannot be smuggled into the ordinary stream.
        if matches!(
            request.command(),
            Command::Pairing(PairingCommand::SelfRevoke { .. } | PairingCommand::Enroll { .. })
        ) {
            return Err(invalid());
        }
        if entry.cleanup && !matches!(request.command(), Command::ReleaseControl(_)) {
            return Err(invalid());
        }
        if entry.reserves_grant && !matches!(entry.state, RetainedOperationState::Completed { .. })
        {
            unresolved_grants = charge(unresolved_grants, 1)?;
            if let Some(workspace) = entry.workspace {
                cleanup_workspaces.insert((key_tuple(&stream.local_pairing), workspace));
            } else {
                // A pending CreateWorkspace may allocate a distinct Workspace.
                // Its eventual identity cannot borrow an existing marker's slot.
                unknown_workspaces = charge(unknown_workspaces, 1)?;
            }
        }
        let outcome_charge = if entry.cleanup {
            if entry.request.len() > release_request_bytes {
                return Err(resource_limit());
            }
            release_requests = charge(release_requests, 1)?;
            // The resident release replaces its dormant request/result slot.
            // Ordinary requests cannot spend either form of this capacity.
            resident = charge(resident, release_capacity)?;
            RELEASE_RESERVATION
        } else {
            resident = charge(
                resident,
                charge(entry.request.len(), MAX_COMPACT_OUTCOME_BYTES)?,
            )?;
            MAX_COMPACT_OUTCOME_BYTES
        };
        if let RetainedOperationState::Completed {
            outcome,
            publication_revision,
            ..
        } = &entry.state
        {
            if publication_revision.get() == 0 || outcome.encoded_len() > outcome_charge {
                return Err(invalid());
            }
            let decoded = outcome.outcome(codec).map_err(|_| invalid())?;
            decoded
                .validate_scope(entry.binding.id.stream())
                .map_err(|_| invalid())?;
            validate_result(entry, &decoded, codec)?;
        }
    }
    for server in state
        .streams
        .iter()
        .map(|stream| stream.stream.server())
        .collect::<BTreeSet<_>>()
    {
        let count = state
            .operations
            .iter()
            .filter(|entry| entry.binding.id.stream().server() == server && !entry.cleanup)
            .count();
        if count
            .checked_mul(MAX_COMPACT_OUTCOME_BYTES)
            .is_none_or(|n| n > limits.outcome_bytes_per_server)
        {
            return Err(resource_limit());
        }
    }
    let mut suppressed = BTreeSet::new();
    for marker in &state.suppressed_workspaces {
        if !generation_retained(state, &marker.local_pairing)
            || marker.workspace.server() != marker.local_pairing.parts().server
            || !suppressed.insert((key_tuple(&marker.local_pairing), marker.workspace))
        {
            return Err(invalid());
        }
    }
    let mut detached = BTreeSet::new();
    for task in &state.detaches {
        if !detached.insert((key_tuple(&task.local_pairing), task.workspace))
            || (!suppressed.contains(&(key_tuple(&task.local_pairing), task.workspace))
                && !state.suppressed_attachments.iter().any(|marker| {
                    marker.local_pairing == task.local_pairing && marker.workspace == task.workspace
                }))
        {
            return Err(invalid());
        }
        validate_detach(state, task, codec)?;
        let (grants, unresolved) = captured_detach_grants(state, task, codec)?;
        if charge(grants.len(), unresolved)? > limits.cleanup_grants {
            return Err(resource_limit());
        }
        for grant in grants {
            reserved_grants.insert((
                key_tuple(&task.local_pairing),
                grant.epoch().get(),
                grant.grant().pane(),
                grant.grant().generation().get(),
            ));
        }
    }
    for entry in state.operations.iter().filter(|entry| entry.cleanup) {
        let request =
            CanonicalOperationRequest::decode(&entry.request, codec).map_err(|_| invalid())?;
        let Command::ReleaseControl(release) = request.command() else {
            return Err(invalid());
        };
        let header = state
            .stream(entry.binding.id.stream())
            .ok_or_else(invalid)?;
        reserved_grants.remove(&(
            key_tuple(&header.local_pairing),
            release.epoch.get(),
            release.grant.pane(),
            release.grant.generation().get(),
        ));
    }
    // A release owns its slot until acknowledgement and detach settlement allow
    // compaction. Later grant admission cannot borrow a retained release's slot.
    if charge(
        charge(reserved_grants.len(), unresolved_grants)?,
        release_requests,
    )? > limits.cleanup_grants
    {
        return Err(resource_limit());
    }
    // A grant-producing request reserves its future marker/task before sending.
    // BeginDetach transfers that slot to resident metadata without another charge.
    // Settled markers remain charged; a later grant in a different Workspace must
    // reserve another slot when those markers have spent the dormant capacity.
    let bookkeeping_slots =
        charge(cleanup_workspaces.len(), unknown_workspaces)?.max(limits.cleanup_grants);
    if bookkeeping_slots > MAX_HEADERS {
        return Err(resource_limit());
    }
    resident = charge(
        resident,
        bookkeeping_slots
            .checked_mul(CLEANUP_BOOKKEEPING_RESERVATION)
            .ok_or_else(resource_limit)?,
    )?;
    // Request/result capacity separately transfers to resident exact releases.
    let dormant = limits
        .cleanup_grants
        .checked_sub(release_requests)
        .ok_or_else(resource_limit)?;
    resident = charge(
        resident,
        dormant
            .checked_mul(release_capacity)
            .ok_or_else(resource_limit)?,
    )?;
    let mut revocations = BTreeSet::new();
    for record in &state.revocations {
        validate_revocation(record, verifier)?;
        let original = record.task.parts().original;
        let header = state
            .stream(original.operation().stream())
            .ok_or_else(invalid)?;
        if header.purpose != StreamPurpose::SelfRevocation
            || header.local_pairing != record.task.parts().local_pairing
            || header.high_water != 1
            || original.operation().sequence().get() != 1
        {
            return Err(invalid());
        }
        let target = PairingTarget::new(PairingTargetParts {
            server: original.server(),
            device: original.device(),
            pairing: original.pairing(),
        })
        .map_err(|_| invalid())?;
        let request = CanonicalOperationRequest::new(
            original.operation(),
            Command::Pairing(PairingCommand::SelfRevoke { target }),
            codec,
        )
        .map_err(|_| invalid())?;
        if request.binding().digest != record.task.parts().request_digest {
            return Err(invalid());
        }
        if !revocations.insert(record.task.parts().original.operation())
            || !state
                .tombstones
                .contains(&record.task.parts().local_pairing)
        {
            return Err(invalid());
        }
        resident = charge(resident, REVOCATION_RESERVATION)?;
    }
    if resident > limits.retained_bytes {
        return Err(resource_limit());
    }
    Ok(())
}

pub(super) fn apply(
    state: &RecoveredClientState,
    tx: &ClientJournalTransaction,
    codec: &dyn LedgerCodec,
    verifier: &dyn ReceiptVerifier,
    limits: JournalLimits,
) -> Result<RecoveredClientState, ClientFailure> {
    if state.revision != tx.expected_revision || tx.changes.is_empty() || tx.changes.len() > 256 {
        return Err(invalid());
    }
    let mut next = state.clone();
    for change in &tx.changes {
        apply_change(&mut next, change, codec, verifier)?;
    }
    advance_prefixes(&mut next)?;
    compact(&mut next);
    next.revision = state.revision.checked_successor().ok_or_else(exhausted)?;
    validate(&next, codec, verifier, limits)?;
    Ok(next)
}

fn apply_change(
    state: &mut RecoveredClientState,
    change: &ClientJournalChange,
    codec: &dyn LedgerCodec,
    verifier: &dyn ReceiptVerifier,
) -> Result<(), ClientFailure> {
    if PairingJournalProjection::apply(state, change, verifier)? {
        if let ClientJournalChange::SaveEnrollment { binding, .. } = change {
            apply_change(
                state,
                &ClientJournalChange::SaveBinding(binding.clone()),
                codec,
                verifier,
            )?;
        }
        return Ok(());
    }
    match change {
        ClientJournalChange::RecordAttachment(marker) => {
            marker.validate()?;
            if marker.incarnation <= state.attachment_incarnation_high_water
                || !marker.belongs_to_active_binding(state)
            {
                return Err(invalid());
            }
            state.attachment_incarnation_high_water = marker.incarnation;
            // Explicit reattachment supersedes the legacy Workspace-wide marker,
            // never its pending exact-grant barrier or old-incarnation markers.
            state.suppressed_workspaces.retain(|old| {
                old.local_pairing != marker.local_pairing || old.workspace != marker.workspace
            });
        }
        ClientJournalChange::SuppressAttachment(marker) => {
            marker.validate()?;
            if !marker.belongs_to_active_binding(state) {
                return Err(invalid());
            }
            state.attachment_incarnation_high_water = state
                .attachment_incarnation_high_water
                .max(marker.incarnation);
            // The incarnation watermark replaces the legacy Workspace marker
            // inside this transaction and spends the same reserved metadata slot.
            state.suppressed_workspaces.retain(|old| {
                old.local_pairing != marker.local_pairing || old.workspace != marker.workspace
            });
            // A detach also suppresses every older file for this same local
            // attachment scope. One watermark bounds repeated detach lifetimes.
            if let Some(old) = state.suppressed_attachments.iter_mut().find(|old| {
                old.local_pairing == marker.local_pairing
                    && old.workspace == marker.workspace
                    && old.client == marker.client
            }) {
                old.incarnation = old.incarnation.max(marker.incarnation);
            } else {
                state.suppressed_attachments.push(marker.clone());
            }
        }
        ClientJournalChange::ReserveCredential(_)
        | ClientJournalChange::BindCredentialRoute { .. }
        | ClientJournalChange::RecordCredentialPublicKey { .. }
        | ClientJournalChange::PrepareEnrollment(_)
        | ClientJournalChange::SaveEnrollment { .. }
        | ClientJournalChange::FinishEnrollmentAcknowledgement(_) => return Err(invalid()),
        ClientJournalChange::SaveBinding(saved) => {
            saved.validate(verifier)?;
            let binding = saved.binding()?;
            let target = binding.target();
            if state
                .bindings
                .iter()
                .any(|b| b.target.server == target.server())
                || state.tombstones.iter().any(|key| {
                    key.parts().server == target.server() && key.parts().pairing == target.pairing()
                })
                || state.streams.iter().any(|s| {
                    s.stream.key() == saved.streams.parts().ordinary.key()
                        || s.stream.key() == saved.streams.parts().self_revocation.key()
                })
            {
                return Err(invalid());
            }
            if let Some(mark) = state
                .generations
                .iter_mut()
                .find(|mark| mark.server == target.server())
            {
                if target.incarnation().get() <= mark.generation.get() {
                    return Err(invalid());
                }
                mark.generation = target.incarnation();
            } else {
                state.generations.push(GenerationHighWater {
                    server: target.server(),
                    generation: target.incarnation(),
                });
            }
            state.pairing.retain_generation(target.incarnation());
            state.streams.push(JournalStream {
                stream: saved.streams.parts().ordinary,
                local_pairing: target.local_pairing()?,
                purpose: StreamPurpose::Ordinary,
                high_water: 0,
                durable_prefix: 0,
                confirmed_prefix: 0,
                retired: false,
            });
            state.streams.push(JournalStream {
                stream: saved.streams.parts().self_revocation,
                local_pairing: target.local_pairing()?,
                purpose: StreamPurpose::SelfRevocation,
                high_water: 0,
                durable_prefix: 0,
                confirmed_prefix: 0,
                retired: false,
            });
            if let Some(slot) = state
                .pairing
                .slots
                .iter_mut()
                .find(|slot| slot.reference == target.credential())
            {
                slot.route = None;
                state
                    .pairing
                    .credential_preparations
                    .retain(|reference| *reference != target.credential());
            }
            state.bindings.push(*saved.clone());
        }
        ClientJournalChange::Prepare(record) => {
            if !matches!(record.state, RetainedOperationState::Pending) {
                return Err(invalid());
            }
            let binding = state
                .bindings
                .iter()
                .find(|b| b.streams.parts().ordinary == record.binding.id.stream())
                .ok_or_else(invalid)?
                .binding()?;
            let key = binding.target().local_pairing()?;
            let request =
                CanonicalOperationRequest::decode(&record.request, codec).map_err(|_| invalid())?;
            if !state.ordinary_use_allowed(&binding)
                || (!record.cleanup
                    && record
                        .workspace
                        .is_some_and(|workspace| state.workspace_barrier(&key, workspace))
                    && starts_or_grants(request.command()))
            {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            let stream = state
                .streams
                .iter_mut()
                .find(|s| s.stream == record.binding.id.stream())
                .ok_or_else(invalid)?;
            if stream.retired
                || stream.high_water.checked_add(1) != Some(record.binding.id.sequence().get())
            {
                return Err(invalid());
            }
            stream.high_water = record.binding.id.sequence().get();
            state.operations.push(record.clone());
        }
        ClientJournalChange::Complete {
            binding,
            outcome,
            publication_revision,
        } => {
            let record = operation_mut(state, *binding)?;
            let result = outcome.outcome(codec).map_err(|_| invalid())?;
            result
                .validate_scope(binding.id.stream())
                .map_err(|_| invalid())?;
            validate_result(record, &result, codec)?;
            match &record.state {
                RetainedOperationState::Pending => {
                    record.state = RetainedOperationState::Completed {
                        outcome: outcome.clone(),
                        publication_revision: *publication_revision,
                        consumed: record.cleanup,
                    }
                }
                RetainedOperationState::Completed {
                    outcome: old,
                    publication_revision: revision,
                    ..
                } if old == outcome && revision == publication_revision => {}
                _ => return Err(crate::failure::invalid_peer()),
            }
            let workspace = record.workspace;
            let request =
                CanonicalOperationRequest::decode(&record.request, codec).map_err(|_| invalid())?;
            let key = state
                .stream(binding.id.stream())
                .ok_or_else(invalid)?
                .local_pairing
                .clone();
            let revoked = state.streams.iter().any(|header| {
                header.local_pairing == key
                    && header.purpose == StreamPurpose::SelfRevocation
                    && header.durable_prefix == 1
            });
            if let Some(control) = result.grant_provenance()
                && !revoked
            {
                let workspace = match (&result, workspace) {
                    (Outcome::CreatedWithControl(created), None) => match created.entities() {
                        spaceterm_core::operations::CreatedEntities::Workspace {
                            workspace,
                            ..
                        } => workspace,
                        _ => return Err(invalid()),
                    },
                    (_, Some(workspace)) => workspace,
                    _ => return Err(invalid()),
                };
                retain_grant(
                    state,
                    JournalGrant {
                        local_pairing: key.clone(),
                        workspace,
                        control,
                        publication_revision: *publication_revision,
                    },
                );
            }
            if let Command::ReleaseControl(release) = request.command()
                && matches!(
                    result,
                    Outcome::Applied
                        | Outcome::Failed(
                            OperationFailure::StaleAuthority | OperationFailure::TargetDeleted
                        )
                )
            {
                state.grants.retain(|grant| {
                    grant.local_pairing != key
                        || grant.control.epoch() != release.epoch
                        || grant.control.grant() != release.grant
                });
            }
        }
        ClientJournalChange::Retire(binding) => {
            let record = operation_mut(state, *binding)?;
            if matches!(record.state, RetainedOperationState::Pending) {
                record.state = RetainedOperationState::Retired;
            }
            state
                .streams
                .iter_mut()
                .find(|s| s.stream == binding.id.stream())
                .ok_or_else(invalid)?
                .retired = true;
        }
        ClientJournalChange::ConfirmAcknowledgement { stream, through } => {
            let record = state
                .streams
                .iter_mut()
                .find(|s| s.stream == *stream)
                .ok_or_else(invalid)?;
            if record.purpose != StreamPurpose::Ordinary || *through > record.durable_prefix {
                return Err(crate::failure::invalid_peer());
            }
            record.confirmed_prefix = record.confirmed_prefix.max(*through);
        }
        ClientJournalChange::ConsumeResult(id) => match &mut state
            .operations
            .iter_mut()
            .find(|r| r.binding.id == *id)
            .ok_or_else(invalid)?
            .state
        {
            RetainedOperationState::Completed { consumed, .. } => *consumed = true,
            _ => return Err(invalid()),
        },
        ClientJournalChange::ReconcileGrants {
            local_pairing,
            epoch,
            revision,
            grants,
        } => {
            let saved = state
                .bindings
                .iter()
                .find(|saved| {
                    saved.binding().is_ok_and(|binding| {
                        binding
                            .target()
                            .local_pairing()
                            .is_ok_and(|key| &key == local_pairing)
                    })
                })
                .ok_or_else(invalid)?
                .binding()?;
            if epoch.get() == 0 || revision.get() == 0 {
                return Err(invalid());
            }
            for (workspace, control) in grants {
                if workspace.server() != saved.target().server()
                    || control.epoch() != *epoch
                    || control.grant().client() != saved.target().client()
                {
                    return Err(invalid());
                }
            }
            state.grants.retain(|grant| {
                &grant.local_pairing != local_pairing
                    || grant.control.epoch().get() > epoch.get()
                    || (grant.control.epoch() == *epoch && grant.publication_revision > *revision)
            });
            for (workspace, control) in grants {
                retain_grant(
                    state,
                    JournalGrant {
                        local_pairing: local_pairing.clone(),
                        workspace: *workspace,
                        control: *control,
                        publication_revision: *revision,
                    },
                );
            }
        }
        ClientJournalChange::BeginDetach(task) => {
            if state.workspace_barrier(&task.local_pairing, task.workspace) {
                return Err(invalid());
            }
            let binding = state
                .bindings
                .iter()
                .find(|b| {
                    b.binding().is_ok_and(|binding| {
                        binding
                            .target()
                            .local_pairing()
                            .is_ok_and(|key| key == task.local_pairing)
                    })
                })
                .ok_or_else(invalid)?;
            if !task.releases.is_empty()
                || state.grants.iter().any(|grant| {
                    grant.local_pairing == task.local_pairing
                        && grant.workspace == task.workspace
                        && !task.grants.contains(&grant.control)
                })
            {
                return Err(invalid());
            }
            // Every unresolved grant-producing request in the Workspace must be
            // captured. Caller cancellation cannot remove a prepared dependency.
            for operation in &state.operations {
                if operation.binding.id.stream() == binding.streams.parts().ordinary
                    && (operation.workspace == Some(task.workspace)
                        || operation.workspace.is_none())
                    && operation.reserves_grant
                    && matches!(operation.state, RetainedOperationState::Pending)
                    && !task.dependencies.contains(&operation.binding)
                {
                    return Err(invalid());
                }
            }
            if !state
                .suppressed_workspaces
                .iter()
                .any(|m| m.local_pairing == task.local_pairing && m.workspace == task.workspace)
            {
                state.suppressed_workspaces.push(WorkspaceSuppression {
                    local_pairing: task.local_pairing.clone(),
                    workspace: task.workspace,
                });
            }
            state.detaches.push(task.clone());
        }
        ClientJournalChange::AddDetachRelease {
            local_pairing,
            workspace,
            operation,
        } => {
            let task = state
                .detaches
                .iter_mut()
                .find(|task| &task.local_pairing == local_pairing && task.workspace == *workspace)
                .ok_or_else(invalid)?;
            if task.releases.contains(operation) {
                return Err(invalid());
            }
            task.releases.push(*operation);
        }
        ClientJournalChange::FinishDetach {
            local_pairing,
            workspace,
        } => {
            let index = state
                .detaches
                .iter()
                .position(|task| {
                    &task.local_pairing == local_pairing && task.workspace == *workspace
                })
                .ok_or_else(invalid)?;
            let task = &state.detaches[index];
            let grants = detach_grants(state, task, codec)?;
            for grant in grants {
                if !task
                    .releases
                    .iter()
                    .any(|binding| release_settled(state, *binding, grant, codec))
                {
                    return Err(invalid());
                }
            }
            let exact = detach_grants(state, &state.detaches[index], codec)?;
            state.grants.retain(|grant| {
                &grant.local_pairing != local_pairing || !exact.contains(&grant.control)
            });
            state.detaches.remove(index);
        }
        ClientJournalChange::BeginRevocation(record) => {
            validate_revocation(record, verifier)?;
            if !matches!(record.phase, RevocationPhase::AwaitingConfirmation)
                || record.notice_dismissed
                || record.attempts != 0
            {
                return Err(invalid());
            }
            let key = &record.task.parts().local_pairing;
            let index = state
                .bindings
                .iter()
                .position(|b| b.target == record.target)
                .ok_or_else(invalid)?;
            let saved = &state.bindings[index];
            if record.task.parts().original.operation().stream()
                != saved.streams.parts().self_revocation
                || record.task.parts().original.operation().sequence().get() != 1
            {
                return Err(invalid());
            }
            let target = PairingTarget::new(PairingTargetParts {
                server: saved.target.server,
                device: saved.target.device,
                pairing: saved.target.pairing,
            })
            .map_err(|_| invalid())?;
            let request = CanonicalOperationRequest::new(
                record.task.parts().original.operation(),
                Command::Pairing(PairingCommand::SelfRevoke { target }),
                codec,
            )
            .map_err(|_| invalid())?;
            if request.binding().digest != record.task.parts().request_digest {
                return Err(invalid());
            }
            state.tombstones.push(key.clone());
            let ordinary = saved.streams.parts().ordinary;
            let restricted = saved.streams.parts().self_revocation;
            state
                .streams
                .iter_mut()
                .find(|s| s.stream == ordinary)
                .ok_or_else(invalid)?
                .retired = true;
            let header = state
                .streams
                .iter_mut()
                .find(|s| {
                    s.stream == restricted
                        && s.purpose == StreamPurpose::SelfRevocation
                        && s.high_water == 0
                })
                .ok_or_else(invalid)?;
            header.high_water = 1;
            header.retired = true;
            state.bindings.remove(index);
            state.revocations.push(*record.clone());
        }
        ClientJournalChange::DismissRevocation(id) => {
            revocation_mut(state, *id)?.notice_dismissed = true
        }
        ClientJournalChange::RevocationAttempt(id) => {
            let record = revocation_mut(state, *id)?;
            record.attempts = record.attempts.saturating_add(1);
        }
        ClientJournalChange::ConfirmRevocation { operation, receipt } => {
            let record = revocation_mut(state, *operation)?;
            if let RevocationPhase::CleanupPending(original) = &record.phase
                && original != receipt
            {
                return Err(crate::failure::invalid_peer());
            }
            record.phase = RevocationPhase::CleanupPending(receipt.clone());
            validate_revocation(record, verifier)?;
            let key = record.task.parts().local_pairing.clone();
            // Verified revocation settles all control of this original pairing.
            // It never settles a newer local incarnation.
            state.detaches.retain(|task| task.local_pairing != key);
            state.grants.retain(|grant| grant.local_pairing != key);
            let header = state
                .streams
                .iter_mut()
                .find(|header| {
                    header.stream == operation.stream()
                        && header.purpose == StreamPurpose::SelfRevocation
                })
                .ok_or_else(invalid)?;
            header.durable_prefix = 1;
        }
        ClientJournalChange::ConfirmRevocationAcknowledgement(id) => {
            if id.sequence().get() != 1 {
                return Err(crate::failure::invalid_peer());
            }
            let header = state
                .streams
                .iter_mut()
                .find(|header| {
                    header.stream == id.stream()
                        && header.purpose == StreamPurpose::SelfRevocation
                        && header.durable_prefix == 1
                })
                .ok_or_else(invalid)?;
            header.confirmed_prefix = 1;
        }
        ClientJournalChange::FinishCredentialCleanup(id) => {
            let index = state
                .revocations
                .iter()
                .position(|r| r.task.parts().original.operation() == *id)
                .ok_or_else(invalid)?;
            if !matches!(
                state.revocations[index].phase,
                RevocationPhase::CleanupPending(_)
            ) {
                return Err(invalid());
            }
            if state.stream(id.stream()).is_none_or(|header| {
                header.purpose != StreamPurpose::SelfRevocation || header.confirmed_prefix != 1
            }) {
                return Err(invalid());
            }
            // Never consult/remove the current saved binding or any shared key.
            state.revocations.remove(index);
        }
    }
    Ok(())
}

fn retain_grant(state: &mut RecoveredClientState, grant: JournalGrant) {
    let same_pane = |old: &JournalGrant| {
        old.local_pairing == grant.local_pairing
            && old.control.grant().pane() == grant.control.grant().pane()
    };
    if state.grants.iter().any(|old| {
        same_pane(old)
            && (old.control.epoch().get() > grant.control.epoch().get()
                || (old.control.epoch() == grant.control.epoch()
                    && old.control.grant().generation().get()
                        >= grant.control.grant().generation().get()))
    }) {
        return;
    }
    state.grants.retain(|old| !same_pane(old));
    state.grants.push(grant);
}
fn generation_retained(state: &RecoveredClientState, key: &LocalPairingKey) -> bool {
    state.generations.iter().any(|mark| {
        mark.server == key.parts().server && mark.generation.get() >= key.parts().generation.get()
    })
}
fn key_tuple(key: &LocalPairingKey) -> (ServerId, PairingId, u64) {
    let p = key.parts();
    (p.server, p.pairing, p.generation.get())
}
fn charge(value: usize, amount: usize) -> Result<usize, ClientFailure> {
    value.checked_add(amount).ok_or_else(resource_limit)
}

fn release_request_reservation(
    stream: OperationStreamId,
    codec: &dyn LedgerCodec,
) -> Result<usize, ClientFailure> {
    use spaceterm_core::{
        authority::{ControlGeneration, ControlGrant, ServerEpoch},
        control::ReleaseControlRequest,
        identity::PaneId,
        operations::OperationSequence,
    };
    // Exact releases contain only fixed identities and integer stamps. Reserve
    // their maximum encoded widths, including a later sequence, with the owning
    // canonical codec rather than assuming a protobuf or JSON request length.
    let id = OperationId::new(
        stream,
        OperationSequence::new(u64::MAX).map_err(|_| invalid())?,
    );
    let grant = ControlGrant::new(
        PaneId::new(stream.server(), u64::MAX).map_err(|_| invalid())?,
        stream.client(),
        ControlGeneration::new(u64::MAX),
    )
    .map_err(|_| invalid())?;
    let bytes = CanonicalOperationRequest::new(
        id,
        Command::ReleaseControl(ReleaseControlRequest {
            epoch: ServerEpoch::new(u64::MAX),
            grant,
        }),
        codec,
    )
    .and_then(|request| request.encode(codec))
    .map_err(|_| invalid())?;
    if bytes.is_empty() || bytes.len() > MAX_CANONICAL_REQUEST_BYTES {
        return Err(resource_limit());
    }
    Ok(bytes.len())
}
fn operation_mut(
    state: &mut RecoveredClientState,
    binding: OperationBinding,
) -> Result<&mut JournalOperation, ClientFailure> {
    state
        .operations
        .iter_mut()
        .find(|entry| entry.binding == binding)
        .ok_or_else(invalid)
}
fn revocation_mut(
    state: &mut RecoveredClientState,
    id: OperationId,
) -> Result<&mut JournalRevocation, ClientFailure> {
    state
        .revocations
        .iter_mut()
        .find(|record| record.task.parts().original.operation() == id)
        .ok_or_else(invalid)
}

pub(crate) fn grant_producing(command: &Command) -> bool {
    matches!(
        command,
        Command::CreateWorkspace { .. }
            | Command::CreateTab { .. }
            | Command::CreatePane { .. }
            | Command::CompareAcquire(_)
            | Command::TakeOver(_)
    )
}
fn starts_or_grants(command: &Command) -> bool {
    grant_producing(command)
        || matches!(
            command,
            Command::BeginStartup(_)
                | Command::CompleteStartup(_)
                | Command::CompleteMeasuredStartup(_)
        )
}
fn validate_result(
    record: &JournalOperation,
    outcome: &Outcome,
    codec: &dyn LedgerCodec,
) -> Result<(), ClientFailure> {
    if matches!(
        outcome,
        Outcome::ControlAcquired(_)
            | Outcome::CreatedWorkspace { .. }
            | Outcome::CreatedTab { .. }
            | Outcome::CreatedPane { .. }
    ) {
        return Err(crate::failure::invalid_peer());
    }
    let request =
        CanonicalOperationRequest::decode(&record.request, codec).map_err(|_| invalid())?;
    if let Outcome::Conflict(conflict) = outcome {
        use spaceterm_core::operations::FieldConflict;
        let same_field = match (request.command(), conflict) {
            (
                Command::RenameWorkspace { workspace, .. },
                FieldConflict::WorkspaceName {
                    workspace: target, ..
                },
            )
            | (
                Command::SetPinnedDirectory { workspace, .. },
                FieldConflict::PinnedDirectory {
                    workspace: target, ..
                },
            ) => workspace == target,
            (
                Command::RepairPaneDirectory { pane, .. }
                | Command::RetryPaneDirectory { pane, .. },
                FieldConflict::PaneDirectory { pane: target, .. },
            ) => pane == target,
            (
                Command::SetWorktreeLocation { server, .. },
                FieldConflict::WorktreeLocation { server: target, .. },
            )
            | (
                Command::SetRemoteAccess { server, .. },
                FieldConflict::RemoteAccess { server: target, .. },
            ) => server == target,
            _ => false,
        };
        if !same_field {
            return Err(crate::failure::invalid_peer());
        }
    }
    let Some(grant) = outcome.grant_provenance() else {
        if record.reserves_grant
            && !matches!(outcome, Outcome::Failed(_))
            && !matches!(
                (request.command(), outcome),
                (Command::CompareAcquire(_), Outcome::ControlUnavailable(_))
            )
        {
            return Err(crate::failure::invalid_peer());
        }
        return Ok(());
    };
    let valid = match (request.command(), outcome) {
        (Command::CompareAcquire(request), Outcome::ControlGranted(_)) => {
            grant.epoch() == request.epoch
                && grant.grant().pane() == request.pane
                && grant.grant().client() == request.client
        }
        (Command::TakeOver(request), Outcome::ControlGranted(_)) => {
            grant.epoch() == request.epoch
                && grant.grant().pane() == request.pane
                && grant.grant().client() == request.client
        }
        (Command::CreateWorkspace { .. }, Outcome::CreatedWithControl(created)) => matches!(
            created.entities(),
            spaceterm_core::operations::CreatedEntities::Workspace { .. }
        ),
        (Command::CreateTab { .. }, Outcome::CreatedWithControl(created)) => matches!(
            created.entities(),
            spaceterm_core::operations::CreatedEntities::Tab { .. }
        ),
        (Command::CreatePane { .. }, Outcome::CreatedWithControl(created)) => matches!(
            created.entities(),
            spaceterm_core::operations::CreatedEntities::Pane(_)
        ),
        _ => false,
    };
    if !record.reserves_grant || !valid {
        return Err(crate::failure::invalid_peer());
    }
    Ok(())
}
fn advance_prefixes(state: &mut RecoveredClientState) -> Result<(), ClientFailure> {
    for stream in &mut state.streams {
        if stream.purpose != StreamPurpose::Ordinary {
            continue;
        }
        let mut prefix = stream.durable_prefix.max(stream.confirmed_prefix);
        while let Some(next) = prefix.checked_add(1) {
            if !state.operations.iter().any(|entry| {
                entry.binding.id.stream() == stream.stream
                    && entry.binding.id.sequence().get() == next
                    && matches!(entry.state, RetainedOperationState::Completed { .. })
            }) {
                break;
            }
            prefix = next;
        }
        stream.durable_prefix = prefix;
    }
    Ok(())
}
fn compact(state: &mut RecoveredClientState) {
    state.operations.retain(|entry| {
        let acknowledged = state.streams.iter().any(|stream| {
            stream.stream == entry.binding.id.stream()
                && entry.binding.id.sequence().get() <= stream.confirmed_prefix
        });
        let consumed = matches!(
            entry.state,
            RetainedOperationState::Completed { consumed: true, .. }
        );
        let cleanup_dependency = state.detaches.iter().any(|task| {
            task.dependencies.contains(&entry.binding) || task.releases.contains(&entry.binding)
        });
        !acknowledged || !consumed || cleanup_dependency
    });
}

pub(crate) fn validate_revocation(
    record: &JournalRevocation,
    verifier: &dyn ReceiptVerifier,
) -> Result<(), ClientFailure> {
    record.task.validate().map_err(|_| invalid())?;
    let target = record.target.target()?;
    let original = record.task.parts().original;
    if target.local_pairing()? != record.task.parts().local_pairing
        || target.server() != original.server()
        || target.pairing() != original.pairing()
        || target.client() != original.client()
        || target.device() != original.device()
        || target.credential() != original.credential()
        || target.pin().public_key != record.task.parts().pinned_server_public_key
    {
        return Err(invalid());
    }
    if let RevocationPhase::CleanupPending(proof) = &record.phase {
        proof.validate().map_err(|_| invalid())?;
        verifier
            .verify(target.pin(), proof)
            .map_err(|_| crate::failure::invalid_peer())?;
        match &proof.parts().claims {
            PairingReceiptBody::Revocation { receipt, .. } if original.matches_receipt(receipt) => {
            }
            _ => return Err(crate::failure::invalid_peer()),
        }
    }
    Ok(())
}
fn validate_detach(
    state: &RecoveredClientState,
    task: &DetachTask,
    codec: &dyn LedgerCodec,
) -> Result<(), ClientFailure> {
    task.local_pairing.validate().map_err(|_| invalid())?;
    let principal = task.local_pairing.parts();
    if task.workspace.server() != principal.server
        || !generation_retained(state, &task.local_pairing)
    {
        return Err(invalid());
    }
    let mut grants = BTreeSet::new();
    let client = state
        .streams
        .iter()
        .find(|s| s.stream.pairing() == principal.pairing)
        .ok_or_else(invalid)?
        .stream
        .client();
    for grant in &task.grants {
        if grant.grant().client() != client
            || !grants.insert((
                grant.epoch().get(),
                grant.grant().pane(),
                grant.grant().generation().get(),
            ))
        {
            return Err(invalid());
        }
    }
    let mut dependencies = BTreeSet::new();
    for dependency in &task.dependencies {
        let entry = state.operation(dependency.id).ok_or_else(invalid)?;
        if entry.binding != *dependency
            || !entry.reserves_grant
            || (entry.workspace != Some(task.workspace) && entry.workspace.is_some())
            || dependency.id.stream().pairing() != principal.pairing
            || !dependencies.insert(dependency.id)
        {
            return Err(invalid());
        }
    }
    // Release identities are limited to the original capture and completed
    // original dependencies, never grants observed after local removal.
    let exact = if task.releases.is_empty() {
        vec![]
    } else {
        detach_grants(state, task, codec)?
    };
    let mut releases = BTreeSet::new();
    for release in &task.releases {
        let entry = state.operation(release.id).ok_or_else(invalid)?;
        let request =
            CanonicalOperationRequest::decode(&entry.request, codec).map_err(|_| invalid())?;
        if entry.binding != *release
            || !entry.cleanup
            || entry.workspace != Some(task.workspace)
            || release.id.stream().pairing() != principal.pairing
            || !releases.insert(release.id)
            || !matches!(request.command(), Command::ReleaseControl(release) if exact.iter().any(|grant| grant.epoch() == release.epoch && grant.grant() == release.grant))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(crate) fn detach_grants(
    state: &RecoveredClientState,
    task: &DetachTask,
    codec: &dyn LedgerCodec,
) -> Result<Vec<GrantedControl>, ClientFailure> {
    let (grants, unresolved) = captured_detach_grants(state, task, codec)?;
    if unresolved != 0 {
        return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
    }
    Ok(grants)
}

// Completed dependencies transfer their reservation to exact grant provenance.
// A retained result and its captured grant must not consume two cleanup slots.
fn captured_detach_grants(
    state: &RecoveredClientState,
    task: &DetachTask,
    codec: &dyn LedgerCodec,
) -> Result<(Vec<GrantedControl>, usize), ClientFailure> {
    let mut grants = task.grants.clone();
    let mut unresolved = 0;
    for dependency in &task.dependencies {
        let record = state.operation(dependency.id).ok_or_else(invalid)?;
        let RetainedOperationState::Completed { outcome, .. } = &record.state else {
            unresolved = charge(unresolved, 1)?;
            continue;
        };
        let outcome = outcome.outcome(codec).map_err(|_| invalid())?;
        if record.workspace.is_none() {
            match &outcome {
                Outcome::CreatedWithControl(created) if matches!(created.entities(), spaceterm_core::operations::CreatedEntities::Workspace { workspace, .. } if workspace == task.workspace) =>
                    {}
                _ => continue,
            }
        }
        if let Some(grant) = outcome.grant_provenance()
            && !grants.contains(&grant)
        {
            grants.push(grant);
        }
    }
    Ok((grants, unresolved))
}
pub(crate) fn release_settled(
    state: &RecoveredClientState,
    binding: OperationBinding,
    grant: GrantedControl,
    codec: &dyn LedgerCodec,
) -> bool {
    let Some(record) = state.operation(binding.id) else {
        return false;
    };
    let Ok(request) = CanonicalOperationRequest::decode(&record.request, codec) else {
        return false;
    };
    if !matches!(request.command(), Command::ReleaseControl(release) if release.epoch == grant.epoch() && release.grant == grant.grant())
    {
        return false;
    }
    match &record.state {
        RetainedOperationState::Completed { outcome, .. } => matches!(
            outcome.outcome(codec),
            Ok(Outcome::Applied
                | Outcome::Failed(
                    OperationFailure::StaleAuthority | OperationFailure::TargetDeleted
                ))
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::{
        authority::{ControlGeneration, ControlGrant, ServerEpoch},
        control::ReleaseControlRequest,
        identity::{OperationStreamKey, PaneId},
        operation_ledger::canonical::CanonicalCodec,
        operations::OperationSequence,
    };
    use spaceterm_protocol::{
        codec::Payload,
        pairing_adapter::operation_to_wire,
        wire::{common, v1, v2},
    };

    #[test]
    fn canonical_release_reservation_covers_both_major_exact_cleanup_requests() {
        let server = ServerId::new([255; 16]);
        let pairing = PairingId::new(server, u64::MAX).unwrap();
        let client = ClientId::new(server, u64::MAX).unwrap();
        let stream = OperationStreamId::new(
            OperationStreamKey::new(server, u64::MAX).unwrap(),
            pairing,
            client,
        )
        .unwrap();
        let request_reservation = release_request_reservation(stream, &CanonicalCodec).unwrap();
        let result = CompactOutcome::new(&Outcome::Applied, &CanonicalCodec).unwrap();
        let entity = |local| common::Entity {
            server: Some(common::Identity {
                value: server.bytes().to_vec(),
            }),
            local_id: local,
        };
        for value in [1, 127, 128, 65535, u64::MAX] {
            let id = OperationId::new(stream, OperationSequence::new(value).unwrap());
            let command = Command::ReleaseControl(ReleaseControlRequest {
                epoch: ServerEpoch::new(value),
                grant: ControlGrant::new(
                    PaneId::new(server, value).unwrap(),
                    client,
                    ControlGeneration::new(value),
                )
                .unwrap(),
            });
            let request =
                CanonicalOperationRequest::new(id, command.clone(), &CanonicalCodec).unwrap();
            let bytes = request.encode(&CanonicalCodec).unwrap();
            assert!(bytes.len() <= request_reservation);
            assert_eq!(
                CanonicalOperationRequest::decode(&bytes, &CanonicalCodec)
                    .unwrap()
                    .command(),
                &command
            );
            macro_rules! profile {
                ($version:ident, $variant:ident) => {{
                    let payload = Payload::$variant($version::Message {
                        kind: Some($version::message::Kind::Operation($version::Operation {
                            id: Some(operation_to_wire(id)),
                            action: Some($version::operation::Action::ReleaseControl(
                                $version::ReleaseControl {
                                    pane: Some(entity(value)),
                                    client: Some(entity(u64::MAX)),
                                    epoch: value,
                                    generation: value,
                                },
                            )),
                        })),
                    });
                    let wire = payload.encode().unwrap();
                    assert_eq!(Payload::decode(payload.major(), &wire).unwrap(), payload);
                    // Request, compact result and wire framing all fit the same
                    // dormant exact-release capacity used by the real journal.
                    assert!(
                        bytes.len() + result.encoded_len() + wire.len() + 4
                            <= request_reservation + RELEASE_RESERVATION
                    );
                }};
            }
            profile!(v1, Major1);
            profile!(v2, Major2);
        }
    }
}
