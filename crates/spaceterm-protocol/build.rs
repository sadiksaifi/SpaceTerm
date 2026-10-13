#[expect(
    dead_code,
    reason = "build admission uses only constants relevant to schema fields"
)]
#[path = "src/limits/hard.rs"]
mod hard;

use std::{collections::BTreeMap, env, fs, path::PathBuf};

use prost_types::{
    DescriptorProto, FieldDescriptorProto,
    field_descriptor_proto::{Label, Type},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let files = [
        "proto/common.proto",
        "proto/v1.proto",
        "proto/v2.proto",
        "proto/pairing.proto",
        "proto/pairing_restricted.proto",
        "proto/git_v1.proto",
        "proto/git_v2.proto",
        "proto/directories_v1.proto",
        "proto/directories_v2.proto",
    ];
    for file in files {
        println!("cargo:rerun-if-changed={file}");
    }
    // Both parsing and Rust generation are pinned Rust dependencies. No protoc,
    // network access, or host compiler installation participates in generation.
    let descriptors = protox::compile(files, ["proto"])?;
    let mut config = prost_build::Config::new();
    // Box only additive payload fields. This changes Rust storage, not protobuf
    // tags or retained legacy profile meaning.
    for major in [1, 2] {
        for field in [
            "Outcome.result.pairing_result",
            "OperationLookupResult.disposition.completed",
            "FindEvent.item.chunk",
            "Message.kind.canonical_semantic_page",
            "Message.kind.content_read_response",
            "ContentReadResponse.result.page",
            "PairingOutcome.operation",
            "PairingOutcome.reservation",
            "PairingOutcome.enrollment_receipt",
            "PairingOutcome.client_binding_receipt",
            "PairingOutcome.unpair_assessment",
            "PairingOutcome.authorization_delivery",
            "PairingOutcome.revocation_proof",
            "SemanticPage.stamp",
            "SemanticPage.page_continuation",
            "ContentQuery.stamp",
            "ContentQuery.page_continuation",
            "ContentQuery.find_continuation",
            "ResourceRequest.stamp",
            "ResourceChunk.stamp",
        ] {
            config.boxed(format!(".spaceterm.v{major}.{field}"));
        }
        config.boxed(format!(
            ".spaceterm.directories.v{major}.DirectoryQueryResult.result.page"
        ));
        config.boxed(format!(
            ".spaceterm.git.v{major}.GitAssessmentPage.creation_preview"
        ));
    }
    config.compile_fds(descriptors.clone())?;

    // Only startup compares a never-started Session (generation zero). Keep
    // every running-Session descriptor strict, including fences and resources.
    let mut admission_descriptors = descriptors.clone();
    for file in &mut admission_descriptors.file {
        if file.package.as_deref() == Some("spaceterm.common") {
            let mut session = file
                .message_type
                .iter()
                .find(|m| m.name() == "Session")
                .ok_or("Session admission descriptor is absent")?
                .clone();
            session.name = Some("StartupSessionComparison".into());
            let mut fence = file
                .message_type
                .iter()
                .find(|m| m.name() == "EffectFence")
                .ok_or("EffectFence admission descriptor is absent")?
                .clone();
            fence.name = Some("StartupFenceComparison".into());
            fence
                .field
                .iter_mut()
                .find(|f| f.name() == "session")
                .ok_or("EffectFence Session field is absent")?
                .type_name = Some(".spaceterm.common.StartupSessionComparison".into());
            file.message_type.extend([session, fence]);
        } else if matches!(
            file.package.as_deref(),
            Some("spaceterm.v1" | "spaceterm.v2")
        ) {
            let startup = file
                .message_type
                .iter_mut()
                .find(|m| m.name() == "StartupIntent")
                .ok_or("StartupIntent admission descriptor is absent")?;
            startup
                .field
                .iter_mut()
                .find(|f| f.name() == "fence")
                .ok_or("StartupIntent fence field is absent")?
                .type_name = Some(".spaceterm.common.StartupFenceComparison".into());
        }
    }
    let mut messages = BTreeMap::new();
    let mut enums = BTreeMap::new();
    for file in &admission_descriptors.file {
        let package = file.package.as_deref().unwrap_or_default();
        for message in &file.message_type {
            collect_messages(&mut messages, &format!(".{package}"), message);
        }
        for enumeration in &file.enum_type {
            enums.insert(format!(".{package}.{}", enumeration.name()), enumeration);
        }
    }
    let indices: BTreeMap<_, _> = messages
        .keys()
        .enumerate()
        .map(|(i, name)| (name.clone(), i))
        .collect();
    let mut output =
        String::from("// Generated from the same descriptors as the prost bindings.\n");
    for (name, index) in &indices {
        output.push_str(&format!(
            "pub(crate) const {}: usize = {index};\n",
            constant(name)
        ));
    }
    output.push_str("pub(crate) static SCHEMAS: &[Schema] = &[\n");
    for (name, message) in &messages {
        assert!(
            message.field.len() <= 64,
            "preflight stack counter capacity"
        );
        let semantic = match message.name() {
            "SemanticRow" | "CanonicalSemanticRow" => "Row",
            "StyledCell" | "Grapheme" => "Cell",
            _ => "Other",
        };
        output.push_str(&format!(
            "Schema {{ semantic: Semantic::{semantic}, fields: &[\n"
        ));
        for field in &message.field {
            let repeated = field.label() == Label::Repeated;
            let kind = match field.r#type() {
                Type::Message => format!("Kind::Message({})", indices[field.type_name()]),
                Type::Enum => {
                    let values = enums[field.type_name()]
                        .value
                        .iter()
                        .map(|v| v.number().to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("Kind::Enum(&[{values}])")
                }
                Type::String => format!(
                    "Kind::String({})",
                    bytes_limit(message.name(), field.name())
                ),
                Type::Bytes => {
                    let exact = match field.name() {
                        "signature"
                        | "device_proof"
                        | "device_identity_proof"
                        | "pairing_authentication_proof" => 64,
                        "device_identity_public_key"
                        | "pairing_authentication_public_key"
                        | "claim_nonce"
                        | "claim_digest"
                        | "request_digest"
                        | "enrollment_request_digest"
                        | "assessment_digest"
                        | "authentication_key_digest"
                        | "single_use_authorization"
                        | "plain_digest"
                        | "html_digest"
                        | "query_fingerprint"
                        | "random" => 32,
                        "ticket_nonce" | "revocation_event" | "native_request" => 16,
                        "nonce"
                        | "connection_nonce"
                        | "binding"
                        | "transcript"
                        | "public_key"
                        | "digest"
                        | "verified_fingerprint" => 32,
                        "value" if message.name() == "Identity" => 16,
                        _ => 0,
                    };
                    format!(
                        "Kind::Bytes({}, {exact})",
                        bytes_limit(message.name(), field.name())
                    )
                }
                Type::Bool => "Kind::Varint(1)".into(),
                Type::Uint32 | Type::Sint32 | Type::Int32 => {
                    let maximum = match (message.name(), field.name()) {
                        ("StyledCell" | "Grapheme", "width") => 2,
                        ("Color", "rgb")
                        | ("ColorOverride", "rgb")
                        | (
                            "ColorState",
                            "default_foreground" | "default_background" | "cursor_color",
                        ) => 0x00ff_ffff,
                        ("Color", "palette_index") | ("ColorOverride", "slot") => 255,
                        ("Geometry" | "MeasuredGrid" | "CellPixelMetrics", _) => u16::MAX as u32,
                        ("GraphicsResourceReference", "rgba_bytes") => {
                            hard::MAX_GRAPHICS_TRANSMISSION_BYTES as u32
                        }
                        ("Key" | "Pointer", "modifiers" | "consumed_modifiers") => 1023,
                        ("Key", "unshifted_codepoint") => 0x10_ffff,
                        ("PhysicalRowId", "page_row")
                        | ("PageContinuation", "columns" | "next_column")
                        | (
                            "SemanticRow" | "CanonicalSemanticRow",
                            "start_column" | "end_column" | "columns",
                        ) => u16::MAX as u32,
                        ("PhysicalRowId" | "QueryStamp" | "ContentBase", "screen") => 2,
                        ("RemoteEndpoint", "port") => u16::MAX as u32,
                        ("CapabilityStatus", "availability") | ("ListenerStatus", "state") => 4,
                        ("ManagementProgress", "state") => 6,
                        _ => u32::MAX,
                    };
                    format!("Kind::Varint({maximum})")
                }
                Type::Uint64 => "Kind::Varint(u64::MAX)".into(),
                _ => panic!("unsupported field type in {name}"),
            };
            let oneof = field.oneof_index.map_or(0, |i| i + 1);
            let max_count = repeated_limit(message.name(), field.name());
            let required = required(message.name(), field);
            let content_limit = match (message.name(), field.name()) {
                ("Paste" | "UnsafePasteRequest", "text") => "Paste",
                ("ClipboardRequest", "text") | ("NativeCompletion", "clipboard_text") => {
                    "Clipboard"
                }
                ("ResourceChunk" | "CopyChunk", "data") => "Resource",
                _ => "Other",
            };
            output.push_str(&format!("Field {{ number: {}, kind: {kind}, repeated: {repeated}, oneof: {oneof}, max_count: {max_count}, required: {required}, content_limit: ContentLimit::{content_limit} }},\n", field.number()));
        }
        let required_oneofs = message
            .field
            .iter()
            .filter(|field| !field.proto3_optional())
            .filter_map(|field| field.oneof_index)
            .fold(0u64, |mask, index| mask | (1 << index));
        // Canonical directory and remote-policy values are additive alternatives
        // to the frozen current_value oneof. Semantics validates that a value exists.
        let required_oneofs = if message.name() == "FieldConflict" {
            0
        } else {
            required_oneofs
        };
        output.push_str(&format!("], required_oneofs: {required_oneofs} }},\n"));
    }
    output.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("preflight.rs"),
        output,
    )?;
    Ok(())
}

fn collect_messages<'a>(
    out: &mut BTreeMap<String, &'a DescriptorProto>,
    prefix: &str,
    message: &'a DescriptorProto,
) {
    let name = format!("{prefix}.{}", message.name());
    out.insert(name.clone(), message);
    for nested in &message.nested_type {
        collect_messages(out, &name, nested);
    }
}

fn constant(name: &str) -> String {
    let mut result = String::new();
    for ch in name.trim_start_matches('.').chars() {
        if ch == '.' {
            result.push('_');
        } else {
            result.push(ch.to_ascii_uppercase());
        }
    }
    result
}

fn repeated_limit(message: &str, field: &str) -> usize {
    match (message, field) {
        (_, "majors" | "capabilities") => 16,
        (_, "palette" | "color_overrides") => hard::MAX_PALETTE_COLORS,
        ("SemanticPage" | "CanonicalSemanticPage", "rows") => hard::MAX_SEMANTIC_ROWS,
        ("SemanticRow" | "CanonicalSemanticRow", "cells" | "runs") | ("StyledRun", "graphemes") => {
            hard::MAX_SEMANTIC_CELLS
        }
        ("SemanticPage" | "CanonicalSemanticPage", "graphics" | "hyperlinks" | "matches") => {
            hard::MAX_SEMANTIC_REFERENCES
        }
        ("CanonicalSemanticPage", "resources") => 128,
        ("OrganizationDelta" | "OrganizationSnapshot", "repositories" | "deleted_repositories") => {
            4096
        }
        ("Tab", "creation_replay_order") => usize::from(u16::MAX),
        ("UnpairAssessment", "clients") => 16,
        ("RemoteAccessPolicy", "endpoints") | ("ManagementStatus", "listeners" | "progress") => 8,
        ("GitInventoryPage" | "GitBranchPage", "records")
        | ("GitAssessmentPage", "tabs" | "panes" | "fallbacks")
        | ("DirectoryPage", "entries") => 128,
        ("GitStatusFacts", "changes") => 20,
        ("FindChunk", "matches") | ("TopologyPage", "rows") => 256,
        _ => 4096,
    }
}

fn bytes_limit(message: &str, field: &str) -> usize {
    match (message, field) {
        ("SignedReceipt", "canonical_body") => 1984,
        ("RemoteEndpoint", "address") => 16,
        ("DirectoryEntryName", "value") => 255,
        ("BranchText", "value") => 1024,
        ("CopyChunk", "data") => hard::MAX_RESOURCE_CHUNK_BYTES,
        ("Signed", "body") | ("AuthenticatedEnvelope", "payload") => hard::MAX_FRAME_BYTES,
        ("Paste" | "UnsafePasteRequest", "text") => hard::MAX_PASTE_BYTES,
        ("ClipboardRequest", "text") | ("NativeCompletion", "clipboard_text") => {
            hard::MAX_CLIPBOARD_BYTES
        }
        ("ResourceChunk", "data") => hard::MAX_RESOURCE_CHUNK_BYTES,
        ("SemanticPage", "copy_text") => hard::MAX_MANAGEMENT_BYTES,
        // The total frame bounds cell text; a separate small cap would split
        // or reject complete extended graphemes accepted by the VT engine.
        ("StyledCell", "grapheme") | ("Grapheme", "text") => hard::MAX_FRAME_BYTES,
        (
            _,
            "continuation"
            | "assessment"
            | "token"
            | "authorization"
            | "approval"
            | "single_use_authorization",
        ) => 1024,
        (_, "input_method_text") => hard::MAX_MANAGEMENT_BYTES,
        _ => 4096,
    }
}

fn required(message: &str, field: &FieldDescriptorProto) -> bool {
    match message {
        "StartupSessionComparison" => field.name() != "generation",
        "StartupFenceComparison" => true,
        "Identity"
        | "Entity"
        | "Session"
        | "EffectFence"
        | "OperationId"
        | "Signed"
        | "AuthenticatedEnvelope"
        | "Ready"
        | "Limits" => true,
        "ClientHello" | "ServerHello" => field.name() != "capabilities",
        "Operation" => field.name() == "id",
        "Outcome" => field.name() == "operation",
        "RevocationRequest" | "RevocationStatus" | "RevocationReceipt" => true,
        "SemanticRow" => matches!(field.name(), "id" | "revision"),
        "CanonicalSemanticRow" => matches!(field.name(), "id" | "columns"),
        "CanonicalSemanticPage" => matches!(field.name(), "stamp" | "geometry"),
        "SemanticPage" => matches!(
            field.name(),
            "fence"
                | "request_generation"
                | "content_revision"
                | "screen_generation"
                | "topology_revision"
        ),
        "RuntimeCommand"
        | "StartupIntent"
        | "ContentQuery"
        | "ResourceRequest"
        | "ResourceCancel"
        | "NativeRequest"
        | "NativeCompletion"
        | "ContentInvalidation" => field.name() == "fence",
        "CreatePane" => matches!(field.name(), "tab" | "source" | "edge"),
        "CreateTab" => field.name() == "workspace",
        "Style" => matches!(field.name(), "foreground" | "background"),
        "StyledCell" => field.name() == "style",
        "StyledRun" => matches!(field.name(), "style" | "graphemes"),
        "ResourceId" => matches!(field.name(), "session" | "local_id"),
        "ResourceChunk" => matches!(
            field.name(),
            "fence" | "resource" | "transfer" | "total_bytes" | "format"
        ),
        "Geometry"
        | "MeasuredGrid"
        | "HeldControl"
        | "StartupAwaitingMeasurementResult"
        | "GraphicsResourceReference"
        | "ContentFailure"
        | "ContentResyncRequired"
        | "CellPixelMetrics"
        | "NewBranchChoice"
        | "RemoteBranchChoice"
        | "WorktreeCreateDraft" => true,
        "LegacyCreatedWorkspaceResult" | "LegacyCreatedTabResult" => true,
        "LegacyControlAcquiredResult" => field.name() != "generation",
        "LegacyAssessedResult" => field.name() == "assessment",
        "OperationFailureResult" => field.name() == "kind",
        "WorktreeCreationPreview" => matches!(field.name(), "draft" | "directory"),
        "Failure" => field.name() == "classification",
        "ContentBase" => matches!(field.name(), "session" | "screen"),
        "TopologyPage" => field.name() == "stamp",
        "GitInventoryRequest" => field.name() == "workspace",
        "TerminalMetadata" => field.name() == "version",
        "Key" => field.name() == "action",
        "ControlRequest" => matches!(field.name(), "pane" | "action" | "request"),
        "Scope" => field.name() == "kind",
        "ManagementRequest" => matches!(field.name(), "request" | "action"),
        "ManagementStatus" => field.name() == "state",
        "Target"
        | "Keys"
        | "Binding"
        | "Ticket"
        | "Proposal"
        | "Reservation"
        | "IssueAuthorization"
        | "BindAdditionalClient"
        | "AssessUnpair"
        | "UnpairAssessment"
        | "UnpairConfirmed"
        | "AuthorizationDelivery"
        | "SignedReceipt"
        | "EnrollmentReceiptBody"
        | "ClientBindingReceiptBody"
        | "RevocationReceiptBody"
        | "SelfRevocation"
        | "ReceiptStatus"
        | "RestrictedAcknowledgement"
        | "EnrollmentHello"
        | "EnrollmentServerHello"
        | "EnrollmentProof"
        | "EnrollmentReady"
        | "EnrollmentSubmission"
        | "EnrollmentLookup"
        | "EnrollmentAcknowledgement" => true,
        "ReceiptBody" => field.name() == "schema",
        "RestrictedResult" => field.name() != "receipt",
        "EnrollmentResult" => matches!(field.name(), "operation" | "request_digest"),
        "GitAssessmentToken" => true,
        "DirectoryContext"
        | "ServerDirectory"
        | "ServerDirectoryValue"
        | "DirectoryEntryName"
        | "BranchText" => true,
        "DirectoryCursor" => true,
        "OperationStream" | "QualifiedOperationId" | "GrantedControl" | "PageLimits" => true,
        "QueryStamp" => matches!(field.name(), "fence" | "request" | "screen"),
        "AcceptedPaneCreation" => matches!(field.name(), "tab" | "pane" | "source" | "edge"),
        "PhysicalRowId" => field.name() == "screen",
        "OperationLookupRequest" | "OperationLookupResult" => {
            matches!(field.name(), "operation" | "request_digest")
        }
        "PublicationPart" => matches!(field.name(), "request" | "transfer" | "total" | "epoch"),
        _ => false,
    }
}
