-- Schema v1. The authority advances user_version in the migration transaction.
-- application_id must be checked/set by the authority before this migration.
-- Local IDs are qualified by store_metadata.server_id, never SQLite INTEGER.
-- Build tags: Release=0, Preflight=1, Development=2. Origins: desktop=0, headless=1, remote=2.
-- Seed decision: server default=0, imported legacy template=1. Presence records the one-time decision.
-- Pane kind: Initial=0, SplitOf=1. Axis: Horizontal=0, Vertical=1. Edge: Before=0, After=1.
-- Stream purpose: Ordinary=0, SelfRevocation=1, Enrollment=2. Status: Active=0, Retired=1.
-- Reservation: Ordinary=0, Safety=1, DedicatedSelfRevocation=2, DedicatedEnrollment=3.
-- Retirement: Explicit=0, Expired=1, PairingRevoked=2, SequenceExhausted=3.
-- Enrollment state: Reserved=0, Consumed=1, Invalidated=2. Pairing: Authorized=0, Revoked=1.
-- Receipt kind: Enrollment=0, ClientBinding=1, Revocation=2. Entry: Pending=0, Completed=1.
-- Pending kind: AtomicMutation=0, External=1. Git phase: Prepared=0, Started=1, ExternalFinished=2.
-- Native action: Stop=0, RestartApproved=1, ActivateUpdate=2. Binary subrecords use codec.rs framing.

CREATE TABLE store_metadata (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    application_id INTEGER NOT NULL CHECK (application_id = 1398034258),
    encoding_version INTEGER NOT NULL CHECK (encoding_version BETWEEN 1 AND 2147483647),
    server_id BLOB NOT NULL CHECK (typeof(server_id) = 'blob' AND length(server_id) = 16 AND server_id > zeroblob(16)),
    account_uid BLOB NOT NULL CHECK (typeof(account_uid) = 'blob' AND length(account_uid) = 8),
    build_identity INTEGER NOT NULL CHECK (build_identity BETWEEN 0 AND 2),
    server_public_key BLOB NOT NULL CHECK (typeof(server_public_key) = 'blob' AND length(server_public_key) = 32 AND server_public_key > zeroblob(32)),
    store_revision BLOB NOT NULL CHECK (typeof(store_revision) = 'blob' AND length(store_revision) = 8),
    organization_revision BLOB NOT NULL CHECK (typeof(organization_revision) = 'blob' AND length(organization_revision) = 8),
    last_epoch BLOB NOT NULL CHECK (typeof(last_epoch) = 'blob' AND length(last_epoch) = 8),
    initialization_origin INTEGER NOT NULL CHECK (initialization_origin BETWEEN 0 AND 2),
    legacy_seed_decision INTEGER NOT NULL CHECK (legacy_seed_decision BETWEEN 0 AND 1)
) STRICT, WITHOUT ROWID;

CREATE TABLE identity_high_water (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    workspace BLOB NOT NULL CHECK (typeof(workspace) = 'blob' AND length(workspace) = 8),
    tab BLOB NOT NULL CHECK (typeof(tab) = 'blob' AND length(tab) = 8),
    pane BLOB NOT NULL CHECK (typeof(pane) = 'blob' AND length(pane) = 8),
    worktree BLOB NOT NULL CHECK (typeof(worktree) = 'blob' AND length(worktree) = 8),
    repository BLOB NOT NULL CHECK (typeof(repository) = 'blob' AND length(repository) = 8),
    pairing BLOB NOT NULL CHECK (typeof(pairing) = 'blob' AND length(pairing) = 8),
    client BLOB NOT NULL CHECK (typeof(client) = 'blob' AND length(client) = 8),
    operation_stream BLOB NOT NULL CHECK (typeof(operation_stream) = 'blob' AND length(operation_stream) = 8),
    creation_ordinal BLOB NOT NULL CHECK (typeof(creation_ordinal) = 'blob' AND length(creation_ordinal) = 8)
) STRICT, WITHOUT ROWID;

CREATE TABLE workspaces (
    id BLOB NOT NULL CHECK (typeof(id) = 'blob' AND length(id) = 8 AND id > zeroblob(8)),
    creation_ordinal BLOB NOT NULL CHECK (typeof(creation_ordinal) = 'blob' AND length(creation_ordinal) = 8 AND creation_ordinal > zeroblob(8)),
    name_mode INTEGER NOT NULL CHECK (name_mode BETWEEN 0 AND 1),
    display_name TEXT NOT NULL CHECK (typeof(display_name) = 'text' AND length(CAST(display_name AS BLOB)) BETWEEN 1 AND 1024),
    pinned_directory BLOB CHECK (pinned_directory IS NULL OR (typeof(pinned_directory) = 'blob' AND length(pinned_directory) BETWEEN 1 AND 4096 AND instr(pinned_directory, x'00') = 0)),
    root_tab BLOB NOT NULL CHECK (typeof(root_tab) = 'blob' AND length(root_tab) = 8 AND root_tab > zeroblob(8)),
    PRIMARY KEY (id),
    UNIQUE (creation_ordinal),
    FOREIGN KEY (id, root_tab) REFERENCES tabs (workspace_id, id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE workspace_field_versions (
    workspace_id BLOB NOT NULL CHECK (typeof(workspace_id) = 'blob' AND length(workspace_id) = 8 AND workspace_id > zeroblob(8)),
    name_version BLOB NOT NULL CHECK (typeof(name_version) = 'blob' AND length(name_version) = 8),
    pin_version BLOB NOT NULL CHECK (typeof(pin_version) = 'blob' AND length(pin_version) = 8),
    PRIMARY KEY (workspace_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE workspace_name_bases (
    workspace_id BLOB NOT NULL CHECK (typeof(workspace_id) = 'blob' AND length(workspace_id) = 8 AND workspace_id > zeroblob(8)),
    automatic_base TEXT NOT NULL CHECK (typeof(automatic_base) = 'text' AND length(CAST(automatic_base AS BLOB)) BETWEEN 1 AND 1024),
    assigned_automatic TEXT NOT NULL CHECK (typeof(assigned_automatic) = 'text' AND length(CAST(assigned_automatic AS BLOB)) BETWEEN 1 AND 1024),
    PRIMARY KEY (workspace_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE tabs (
    id BLOB NOT NULL CHECK (typeof(id) = 'blob' AND length(id) = 8 AND id > zeroblob(8)),
    workspace_id BLOB NOT NULL CHECK (typeof(workspace_id) = 'blob' AND length(workspace_id) = 8 AND workspace_id > zeroblob(8)),
    worktree_id BLOB CHECK (worktree_id IS NULL OR (typeof(worktree_id) = 'blob' AND length(worktree_id) = 8 AND worktree_id > zeroblob(8))),
    root_pane BLOB NOT NULL CHECK (typeof(root_pane) = 'blob' AND length(root_pane) = 8 AND root_pane > zeroblob(8)),
    creation_ordinal BLOB NOT NULL CHECK (typeof(creation_ordinal) = 'blob' AND length(creation_ordinal) = 8 AND creation_ordinal > zeroblob(8)),
    PRIMARY KEY (id),
    UNIQUE (workspace_id, id),
    UNIQUE (creation_ordinal),
    FOREIGN KEY (workspace_id) REFERENCES workspaces (id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (worktree_id) REFERENCES worktrees (id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (id, root_pane) REFERENCES panes (tab_id, id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE tab_field_versions (
    tab_id BLOB NOT NULL CHECK (typeof(tab_id) = 'blob' AND length(tab_id) = 8 AND tab_id > zeroblob(8)),
    membership_version BLOB NOT NULL CHECK (typeof(membership_version) = 'blob' AND length(membership_version) = 8),
    PRIMARY KEY (tab_id),
    FOREIGN KEY (tab_id) REFERENCES tabs (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE panes (
    id BLOB NOT NULL CHECK (typeof(id) = 'blob' AND length(id) = 8 AND id > zeroblob(8)),
    tab_id BLOB NOT NULL CHECK (typeof(tab_id) = 'blob' AND length(tab_id) = 8 AND tab_id > zeroblob(8)),
    creation_ordinal BLOB NOT NULL CHECK (typeof(creation_ordinal) = 'blob' AND length(creation_ordinal) = 8 AND creation_ordinal > zeroblob(8)),
    creation_kind INTEGER NOT NULL CHECK (creation_kind BETWEEN 0 AND 1),
    creation_source BLOB CHECK (creation_source IS NULL OR (typeof(creation_source) = 'blob' AND length(creation_source) = 8 AND creation_source > zeroblob(8))),
    split_axis INTEGER CHECK (split_axis IS NULL OR (split_axis BETWEEN 0 AND 1)),
    split_edge INTEGER CHECK (split_edge IS NULL OR (split_edge BETWEEN 0 AND 1)),
    starting_directory BLOB NOT NULL CHECK (typeof(starting_directory) = 'blob' AND length(starting_directory) BETWEEN 1 AND 4096 AND instr(starting_directory, x'00') = 0),
    restoration_directory BLOB CHECK (restoration_directory IS NULL OR (typeof(restoration_directory) = 'blob' AND length(restoration_directory) BETWEEN 1 AND 4096 AND instr(restoration_directory, x'00') = 0)),
    PRIMARY KEY (id),
    UNIQUE (tab_id, id),
    UNIQUE (creation_ordinal),
    CHECK ((creation_kind = 0 AND creation_source IS NULL AND split_axis IS NULL AND split_edge IS NULL) OR (creation_kind = 1 AND creation_source IS NOT NULL AND creation_source != id AND split_axis IS NOT NULL AND split_edge IS NOT NULL)),
    FOREIGN KEY (tab_id) REFERENCES tabs (id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (tab_id, creation_source) REFERENCES panes (tab_id, id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE pane_field_versions (
    pane_id BLOB NOT NULL CHECK (typeof(pane_id) = 'blob' AND length(pane_id) = 8 AND pane_id > zeroblob(8)),
    restoration_version BLOB NOT NULL CHECK (typeof(restoration_version) = 'blob' AND length(restoration_version) = 8),
    PRIMARY KEY (pane_id),
    FOREIGN KEY (pane_id) REFERENCES panes (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE pane_generation_high_water (
    pane_id BLOB NOT NULL CHECK (typeof(pane_id) = 'blob' AND length(pane_id) = 8 AND pane_id > zeroblob(8)),
    session_generation BLOB NOT NULL CHECK (typeof(session_generation) = 'blob' AND length(session_generation) = 8),
    control_generation BLOB NOT NULL CHECK (typeof(control_generation) = 'blob' AND length(control_generation) = 8),
    PRIMARY KEY (pane_id),
    FOREIGN KEY (pane_id) REFERENCES panes (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE tab_creation_replay (
    tab_id BLOB NOT NULL CHECK (typeof(tab_id) = 'blob' AND length(tab_id) = 8 AND tab_id > zeroblob(8)),
    position BLOB NOT NULL CHECK (typeof(position) = 'blob' AND length(position) = 8),
    pane_id BLOB NOT NULL CHECK (typeof(pane_id) = 'blob' AND length(pane_id) = 8 AND pane_id > zeroblob(8)),
    PRIMARY KEY (tab_id, position),
    UNIQUE (tab_id, pane_id),
    FOREIGN KEY (tab_id) REFERENCES tabs (id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (tab_id, pane_id) REFERENCES panes (tab_id, id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE repositories (
    id BLOB NOT NULL CHECK (typeof(id) = 'blob' AND length(id) = 8 AND id > zeroblob(8)),
    directory BLOB NOT NULL CHECK (typeof(directory) = 'blob' AND length(directory) BETWEEN 1 AND 4096 AND instr(directory, x'00') = 0),
    PRIMARY KEY (id)
) STRICT, WITHOUT ROWID;

CREATE TABLE repository_field_versions (
    repository_id BLOB NOT NULL CHECK (typeof(repository_id) = 'blob' AND length(repository_id) = 8 AND repository_id > zeroblob(8)),
    inventory_version BLOB NOT NULL CHECK (typeof(inventory_version) = 'blob' AND length(inventory_version) = 8),
    PRIMARY KEY (repository_id),
    FOREIGN KEY (repository_id) REFERENCES repositories (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE worktrees (
    id BLOB NOT NULL CHECK (typeof(id) = 'blob' AND length(id) = 8 AND id > zeroblob(8)),
    repository_id BLOB NOT NULL CHECK (typeof(repository_id) = 'blob' AND length(repository_id) = 8 AND repository_id > zeroblob(8)),
    directory BLOB NOT NULL CHECK (typeof(directory) = 'blob' AND length(directory) BETWEEN 1 AND 4096 AND instr(directory, x'00') = 0),
    is_main INTEGER NOT NULL CHECK (is_main BETWEEN 0 AND 1),
    missing INTEGER NOT NULL CHECK (missing BETWEEN 0 AND 1),
    PRIMARY KEY (id),
    FOREIGN KEY (repository_id) REFERENCES repositories (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE worktree_inventory (
    worktree_id BLOB NOT NULL CHECK (typeof(worktree_id) = 'blob' AND length(worktree_id) = 8 AND worktree_id > zeroblob(8)),
    listed INTEGER NOT NULL CHECK (listed BETWEEN 0 AND 1),
    inventory_version BLOB NOT NULL CHECK (typeof(inventory_version) = 'blob' AND length(inventory_version) = 8),
    PRIMARY KEY (worktree_id),
    FOREIGN KEY (worktree_id) REFERENCES worktrees (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE server_settings (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    worktree_location TEXT NOT NULL CHECK (typeof(worktree_location) = 'text' AND length(CAST(worktree_location AS BLOB)) BETWEEN 1 AND 4096),
    worktree_location_version BLOB NOT NULL CHECK (typeof(worktree_location_version) = 'blob' AND length(worktree_location_version) = 8),
    remote_access_version BLOB NOT NULL CHECK (typeof(remote_access_version) = 'blob' AND length(remote_access_version) = 8)
) STRICT, WITHOUT ROWID;

CREATE TABLE remote_bind_endpoints (
    position BLOB NOT NULL CHECK (typeof(position) = 'blob' AND length(position) = 8),
    address_family INTEGER NOT NULL CHECK (address_family BETWEEN 4 AND 6),
    address BLOB NOT NULL CHECK (typeof(address) = 'blob' AND length(address) BETWEEN 1 AND 16),
    port INTEGER NOT NULL CHECK (port BETWEEN 0 AND 65535),
    flowinfo INTEGER NOT NULL CHECK (flowinfo BETWEEN 0 AND 4294967295),
    scope_id INTEGER NOT NULL CHECK (scope_id BETWEEN 0 AND 4294967295),
    PRIMARY KEY (position),
    CHECK ((address_family = 4 AND length(address) = 4 AND flowinfo = 0 AND scope_id = 0) OR (address_family = 6 AND length(address) = 16)),
    UNIQUE (address_family, address, port, flowinfo, scope_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE pairings (
    id BLOB NOT NULL CHECK (typeof(id) = 'blob' AND length(id) = 8 AND id > zeroblob(8)),
    device_id BLOB NOT NULL CHECK (typeof(device_id) = 'blob' AND length(device_id) = 16 AND device_id > zeroblob(16)),
    device_identity_public_key BLOB NOT NULL CHECK (typeof(device_identity_public_key) = 'blob' AND length(device_identity_public_key) = 32 AND device_identity_public_key > zeroblob(32)),
    pairing_authentication_public_key BLOB NOT NULL CHECK (typeof(pairing_authentication_public_key) = 'blob' AND length(pairing_authentication_public_key) = 32 AND pairing_authentication_public_key > zeroblob(32)),
    authorization_version BLOB NOT NULL CHECK (typeof(authorization_version) = 'blob' AND length(authorization_version) = 8 AND authorization_version > zeroblob(8)),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 1),
    revoked_revision BLOB CHECK (revoked_revision IS NULL OR (typeof(revoked_revision) = 'blob' AND length(revoked_revision) = 8 AND revoked_revision > zeroblob(8))),
    PRIMARY KEY (id),
    CHECK (device_identity_public_key != pairing_authentication_public_key),
    CHECK ((state = 0 AND revoked_revision IS NULL) OR (state = 1 AND revoked_revision IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE TABLE paired_clients (
    id BLOB NOT NULL CHECK (typeof(id) = 'blob' AND length(id) = 8 AND id > zeroblob(8)),
    pairing_id BLOB NOT NULL CHECK (typeof(pairing_id) = 'blob' AND length(pairing_id) = 8 AND pairing_id > zeroblob(8)),
    ordinary_stream BLOB NOT NULL CHECK (typeof(ordinary_stream) = 'blob' AND length(ordinary_stream) = 8 AND ordinary_stream > zeroblob(8)),
    self_revocation_stream BLOB NOT NULL CHECK (typeof(self_revocation_stream) = 'blob' AND length(self_revocation_stream) = 8 AND self_revocation_stream > zeroblob(8)),
    PRIMARY KEY (id),
    UNIQUE (pairing_id, id),
    CHECK (ordinary_stream != self_revocation_stream),
    FOREIGN KEY (pairing_id) REFERENCES pairings (id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (ordinary_stream, pairing_id, id) REFERENCES operation_streams (stream_key, pairing_id, client_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (self_revocation_stream, pairing_id, id) REFERENCES operation_streams (stream_key, pairing_id, client_id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE enrollment_authorizations (
    ticket_nonce BLOB NOT NULL CHECK (typeof(ticket_nonce) = 'blob' AND length(ticket_nonce) = 16 AND ticket_nonce > zeroblob(16)),
    prospective_pairing_id BLOB NOT NULL CHECK (typeof(prospective_pairing_id) = 'blob' AND length(prospective_pairing_id) = 8 AND prospective_pairing_id > zeroblob(8)),
    device_id BLOB NOT NULL CHECK (typeof(device_id) = 'blob' AND length(device_id) = 16 AND device_id > zeroblob(16)),
    device_identity_public_key BLOB NOT NULL CHECK (typeof(device_identity_public_key) = 'blob' AND length(device_identity_public_key) = 32 AND device_identity_public_key > zeroblob(32)),
    pairing_authentication_public_key BLOB NOT NULL CHECK (typeof(pairing_authentication_public_key) = 'blob' AND length(pairing_authentication_public_key) = 32 AND pairing_authentication_public_key > zeroblob(32)),
    initial_client_id BLOB NOT NULL CHECK (typeof(initial_client_id) = 'blob' AND length(initial_client_id) = 8 AND initial_client_id > zeroblob(8)),
    enrollment_stream BLOB NOT NULL CHECK (typeof(enrollment_stream) = 'blob' AND length(enrollment_stream) = 8 AND enrollment_stream > zeroblob(8)),
    claim_digest BLOB NOT NULL CHECK (typeof(claim_digest) = 'blob' AND length(claim_digest) = 32),
    authorization_digest BLOB NOT NULL CHECK (typeof(authorization_digest) = 'blob' AND length(authorization_digest) = 32),
    enrollment_request_digest BLOB NOT NULL CHECK (typeof(enrollment_request_digest) = 'blob' AND length(enrollment_request_digest) = 32),
    issuer BLOB NOT NULL CHECK (typeof(issuer) = 'blob' AND length(issuer) BETWEEN 1 AND 160),
    issued_epoch BLOB NOT NULL CHECK (typeof(issued_epoch) = 'blob' AND length(issued_epoch) = 8 AND issued_epoch > zeroblob(8)),
    issued_revision BLOB NOT NULL CHECK (typeof(issued_revision) = 'blob' AND length(issued_revision) = 8 AND issued_revision > zeroblob(8)),
    issued_at_millis BLOB NOT NULL CHECK (typeof(issued_at_millis) = 'blob' AND length(issued_at_millis) = 8),
    expires_at_millis BLOB NOT NULL CHECK (typeof(expires_at_millis) = 'blob' AND length(expires_at_millis) = 8),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 2),
    consumed_sequence BLOB CHECK (consumed_sequence IS NULL OR (typeof(consumed_sequence) = 'blob' AND length(consumed_sequence) = 8 AND consumed_sequence > zeroblob(8))),
    consumed_digest BLOB CHECK (consumed_digest IS NULL OR (typeof(consumed_digest) = 'blob' AND length(consumed_digest) = 32)),
    PRIMARY KEY (ticket_nonce),
    UNIQUE (prospective_pairing_id),
    CHECK (device_identity_public_key != pairing_authentication_public_key),
    CHECK (expires_at_millis > issued_at_millis),
    CHECK ((state = 1 AND consumed_sequence IS NOT NULL AND consumed_digest IS NOT NULL AND consumed_digest = enrollment_request_digest) OR (state IN (0, 2) AND consumed_sequence IS NULL AND consumed_digest IS NULL)),
    FOREIGN KEY (enrollment_stream, prospective_pairing_id, initial_client_id) REFERENCES operation_streams (stream_key, pairing_id, client_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (enrollment_stream, consumed_sequence) REFERENCES pairing_receipts (stream_key, sequence) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE pairing_receipts (
    stream_key BLOB NOT NULL CHECK (typeof(stream_key) = 'blob' AND length(stream_key) = 8 AND stream_key > zeroblob(8)),
    sequence BLOB NOT NULL CHECK (typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > zeroblob(8)),
    pairing_id BLOB NOT NULL CHECK (typeof(pairing_id) = 'blob' AND length(pairing_id) = 8 AND pairing_id > zeroblob(8)),
    receipt_kind INTEGER NOT NULL CHECK (receipt_kind BETWEEN 0 AND 2),
    format_version INTEGER NOT NULL CHECK (format_version BETWEEN 1 AND 65535),
    canonical_body BLOB NOT NULL CHECK (typeof(canonical_body) = 'blob' AND length(canonical_body) BETWEEN 1 AND 1984),
    signature BLOB NOT NULL CHECK (typeof(signature) = 'blob' AND length(signature) = 64),
    PRIMARY KEY (stream_key, sequence),
    FOREIGN KEY (stream_key) REFERENCES operation_streams (stream_key) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (pairing_id) REFERENCES pairings (id) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE revocation_receipts (
    pairing_id BLOB NOT NULL CHECK (typeof(pairing_id) = 'blob' AND length(pairing_id) = 8 AND pairing_id > zeroblob(8)),
    operation_stream BLOB NOT NULL CHECK (typeof(operation_stream) = 'blob' AND length(operation_stream) = 8 AND operation_stream > zeroblob(8)),
    operation_sequence BLOB NOT NULL CHECK (typeof(operation_sequence) = 'blob' AND length(operation_sequence) = 8 AND operation_sequence > zeroblob(8)),
    operation_digest BLOB NOT NULL CHECK (typeof(operation_digest) = 'blob' AND length(operation_digest) = 32),
    PRIMARY KEY (pairing_id),
    FOREIGN KEY (pairing_id) REFERENCES pairings (id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (operation_stream, operation_sequence) REFERENCES pairing_receipts (stream_key, sequence) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (operation_stream) REFERENCES operation_streams (stream_key) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE operation_streams (
    stream_key BLOB NOT NULL CHECK (typeof(stream_key) = 'blob' AND length(stream_key) = 8 AND stream_key > zeroblob(8)),
    pairing_id BLOB NOT NULL CHECK (typeof(pairing_id) = 'blob' AND length(pairing_id) = 8 AND pairing_id > zeroblob(8)),
    client_id BLOB NOT NULL CHECK (typeof(client_id) = 'blob' AND length(client_id) = 8 AND client_id > zeroblob(8)),
    purpose INTEGER NOT NULL CHECK (purpose BETWEEN 0 AND 2),
    enrollment_claim_digest BLOB CHECK (enrollment_claim_digest IS NULL OR (typeof(enrollment_claim_digest) = 'blob' AND length(enrollment_claim_digest) = 32)),
    status INTEGER NOT NULL CHECK (status BETWEEN 0 AND 1),
    retirement_reason INTEGER CHECK (retirement_reason IS NULL OR (retirement_reason BETWEEN 0 AND 3)),
    admitted_through BLOB NOT NULL CHECK (typeof(admitted_through) = 'blob' AND length(admitted_through) = 8),
    acknowledged_through BLOB NOT NULL CHECK (typeof(acknowledged_through) = 'blob' AND length(acknowledged_through) = 8),
    dormant_class INTEGER CHECK (dormant_class IS NULL OR (dormant_class BETWEEN 0 AND 3)),
    dormant_bytes BLOB CHECK (dormant_bytes IS NULL OR (typeof(dormant_bytes) = 'blob' AND length(dormant_bytes) = 8)),
    request_format_version INTEGER NOT NULL CHECK (request_format_version BETWEEN 1 AND 2147483647),
    outcome_format_version INTEGER NOT NULL CHECK (outcome_format_version BETWEEN 1 AND 2147483647),
    quota_policy_version INTEGER NOT NULL CHECK (quota_policy_version BETWEEN 1 AND 2147483647),
    PRIMARY KEY (stream_key),
    UNIQUE (stream_key, pairing_id, client_id),
    CHECK (acknowledged_through <= admitted_through),
    CHECK ((purpose = 2 AND enrollment_claim_digest IS NOT NULL) OR (purpose IN (0, 1) AND enrollment_claim_digest IS NULL)),
    CHECK ((status = 0 AND retirement_reason IS NULL) OR (status = 1 AND retirement_reason IS NOT NULL)),
    CHECK ((dormant_class IS NULL AND dormant_bytes IS NULL) OR (dormant_class IS NOT NULL AND dormant_bytes IS NOT NULL AND dormant_bytes = x'0000000000010000')),
    CHECK ((purpose = 0 AND dormant_class IS NULL) OR (purpose IN (1, 2) AND admitted_through <= x'0000000000000001' AND ((admitted_through = zeroblob(8) AND dormant_class IS NOT NULL AND dormant_class = purpose + 1) OR (admitted_through = x'0000000000000001' AND dormant_class IS NULL))))
) STRICT, WITHOUT ROWID;

CREATE TABLE operation_entries (
    stream_key BLOB NOT NULL CHECK (typeof(stream_key) = 'blob' AND length(stream_key) = 8 AND stream_key > zeroblob(8)),
    sequence BLOB NOT NULL CHECK (typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > zeroblob(8)),
    request_digest BLOB NOT NULL CHECK (typeof(request_digest) = 'blob' AND length(request_digest) = 32),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 1),
    reservation_class INTEGER NOT NULL CHECK (reservation_class BETWEEN 0 AND 3),
    reservation_bytes BLOB NOT NULL CHECK (typeof(reservation_bytes) = 'blob' AND length(reservation_bytes) = 8),
    admitted_at BLOB NOT NULL CHECK (typeof(admitted_at) = 'blob' AND length(admitted_at) = 8 AND admitted_at > zeroblob(8)),
    admission_organization_revision BLOB CHECK (admission_organization_revision IS NULL OR (typeof(admission_organization_revision) = 'blob' AND length(admission_organization_revision) = 8)),
    progress_recorded_at BLOB CHECK (progress_recorded_at IS NULL OR (typeof(progress_recorded_at) = 'blob' AND length(progress_recorded_at) = 8 AND progress_recorded_at > zeroblob(8))),
    completed_at BLOB CHECK (completed_at IS NULL OR (typeof(completed_at) = 'blob' AND length(completed_at) = 8 AND completed_at > zeroblob(8))),
    pending_kind INTEGER CHECK (pending_kind IS NULL OR (pending_kind BETWEEN 0 AND 1)),
    external_progress BLOB CHECK (external_progress IS NULL OR (typeof(external_progress) = 'blob' AND length(external_progress) BETWEEN 1 AND 16777216)),
    compact_outcome BLOB CHECK (compact_outcome IS NULL OR (typeof(compact_outcome) = 'blob' AND length(compact_outcome) BETWEEN 1 AND 61440)),
    publication_revision BLOB CHECK (publication_revision IS NULL OR (typeof(publication_revision) = 'blob' AND length(publication_revision) = 8)),
    PRIMARY KEY (stream_key, sequence),
    CHECK (reservation_bytes = x'0000000000010000'),
    CHECK (progress_recorded_at >= admitted_at),
    CHECK ((state = 0 AND admission_organization_revision IS NOT NULL AND progress_recorded_at IS NOT NULL AND completed_at IS NULL AND pending_kind IS NOT NULL AND compact_outcome IS NULL AND publication_revision IS NULL AND ((pending_kind = 0 AND external_progress IS NULL AND progress_recorded_at = admitted_at) OR (pending_kind = 1 AND external_progress IS NOT NULL))) OR (state = 1 AND admission_organization_revision IS NULL AND progress_recorded_at IS NULL AND completed_at IS NOT NULL AND completed_at > admitted_at AND pending_kind IS NULL AND external_progress IS NULL AND compact_outcome IS NOT NULL AND publication_revision IS NOT NULL)),
    FOREIGN KEY (stream_key) REFERENCES operation_streams (stream_key) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE git_repository_locators (
    repository_id BLOB NOT NULL CHECK (typeof(repository_id) = 'blob' AND length(repository_id) = 8 AND repository_id > zeroblob(8)),
    main_root BLOB NOT NULL CHECK (typeof(main_root) = 'blob' AND length(main_root) BETWEEN 1 AND 4096 AND instr(main_root, x'00') = 0),
    common_directory BLOB NOT NULL CHECK (typeof(common_directory) = 'blob' AND length(common_directory) BETWEEN 1 AND 4096 AND instr(common_directory, x'00') = 0),
    PRIMARY KEY (repository_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE git_allocation_reservations (
    stream_key BLOB NOT NULL CHECK (typeof(stream_key) = 'blob' AND length(stream_key) = 8 AND stream_key > zeroblob(8)),
    sequence BLOB NOT NULL CHECK (typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > zeroblob(8)),
    request_digest BLOB NOT NULL CHECK (typeof(request_digest) = 'blob' AND length(request_digest) = 32),
    reservation BLOB NOT NULL CHECK (typeof(reservation) = 'blob' AND length(reservation) BETWEEN 1 AND 16777216),
    PRIMARY KEY (stream_key, sequence),
    FOREIGN KEY (stream_key) REFERENCES operation_streams (stream_key) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE git_write_intents (
    stream_key BLOB NOT NULL CHECK (typeof(stream_key) = 'blob' AND length(stream_key) = 8 AND stream_key > zeroblob(8)),
    sequence BLOB NOT NULL CHECK (typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > zeroblob(8)),
    request_digest BLOB NOT NULL CHECK (typeof(request_digest) = 'blob' AND length(request_digest) = 32),
    repository_id BLOB NOT NULL CHECK (typeof(repository_id) = 'blob' AND length(repository_id) = 8 AND repository_id > zeroblob(8)),
    action BLOB NOT NULL CHECK (typeof(action) = 'blob' AND length(action) BETWEEN 1 AND 16384),
    assessment_digest BLOB NOT NULL CHECK (typeof(assessment_digest) = 'blob' AND length(assessment_digest) = 32),
    phase INTEGER NOT NULL CHECK (phase BETWEEN 0 AND 2),
    affected_scope BLOB NOT NULL CHECK (typeof(affected_scope) = 'blob' AND length(affected_scope) BETWEEN 1 AND 16777216),
    external_result BLOB CHECK (external_result IS NULL OR (typeof(external_result) = 'blob' AND length(external_result) BETWEEN 1 AND 128)),
    PRIMARY KEY (stream_key, sequence),
    CHECK ((phase IN (0, 1) AND external_result IS NULL) OR (phase = 2 AND external_result IS NOT NULL)),
    FOREIGN KEY (repository_id) REFERENCES git_repository_locators (repository_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (stream_key) REFERENCES operation_streams (stream_key) DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE native_retirements (
    request_id BLOB NOT NULL CHECK (typeof(request_id) = 'blob' AND length(request_id) = 16 AND request_id > zeroblob(16)),
    request_digest BLOB NOT NULL CHECK (typeof(request_digest) = 'blob' AND length(request_digest) = 32),
    batch_epoch BLOB NOT NULL CHECK (typeof(batch_epoch) = 'blob' AND length(batch_epoch) = 8 AND batch_epoch > zeroblob(8)),
    batch_sequence BLOB NOT NULL CHECK (typeof(batch_sequence) = 'blob' AND length(batch_sequence) = 8 AND batch_sequence > zeroblob(8)),
    action INTEGER NOT NULL CHECK (action BETWEEN 0 AND 2),
    acceptance_revision BLOB NOT NULL CHECK (typeof(acceptance_revision) = 'blob' AND length(acceptance_revision) = 8),
    descriptor BLOB NOT NULL CHECK (typeof(descriptor) = 'blob' AND length(descriptor) BETWEEN 1 AND 16777216),
    PRIMARY KEY (request_id),
    UNIQUE (batch_epoch, batch_sequence)
) STRICT, WITHOUT ROWID;
