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
    let files = ["proto/common.proto", "proto/v1.proto", "proto/v2.proto"];
    for file in files {
        println!("cargo:rerun-if-changed={file}");
    }
    // Both parsing and Rust generation are pinned Rust dependencies. No protoc,
    // network access, or host compiler installation participates in generation.
    let descriptors = protox::compile(files, ["proto"])?;
    prost_build::Config::new().compile_fds(descriptors.clone())?;

    let mut messages = BTreeMap::new();
    let mut enums = BTreeMap::new();
    for file in &descriptors.file {
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
            "SemanticRow" => "Row",
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
                        "signature" | "device_proof" => 64,
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
                        ("Geometry", _) => u16::MAX as u32,
                        ("Key" | "Pointer", "modifiers" | "consumed_modifiers") => 1023,
                        ("Key", "unshifted_codepoint") => 0x10_ffff,
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
                ("ResourceChunk", "data") => "Resource",
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
        ("SemanticPage", "rows") => hard::MAX_SEMANTIC_ROWS,
        ("SemanticRow", "cells" | "runs") | ("StyledRun", "graphemes") => hard::MAX_SEMANTIC_CELLS,
        ("SemanticPage", "graphics" | "hyperlinks" | "matches") => hard::MAX_SEMANTIC_REFERENCES,
        _ => 4096,
    }
}

fn bytes_limit(message: &str, field: &str) -> usize {
    match (message, field) {
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
        "ResourceId" => true,
        "ResourceChunk" => matches!(
            field.name(),
            "fence" | "resource" | "transfer" | "total_bytes" | "format"
        ),
        "Geometry" => true,
        "Failure" => field.name() == "classification",
        "TerminalMetadata" => field.name() == "version",
        "Key" => field.name() == "action",
        "ControlRequest" => matches!(field.name(), "pane" | "action" | "request"),
        "Scope" => field.name() == "kind",
        "ManagementRequest" => matches!(field.name(), "request" | "action"),
        "ManagementStatus" => field.name() == "state",
        _ => false,
    }
}
