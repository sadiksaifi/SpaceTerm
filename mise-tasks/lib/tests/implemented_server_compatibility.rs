//! Test-only comparison linked to the protocol and organization store owners.

use std::io::{self, Read};
use std::process::ExitCode;

use serde_json::{Value, json};
use spaceterm_host::organization_store::schema;
use spaceterm_protocol::negotiation::SUPPORTED_MAJORS;

fn main() -> ExitCode {
    let mut bytes = Vec::new();
    if io::stdin().take(4097).read_to_end(&mut bytes).is_err() || bytes.len() > 4096 {
        return ExitCode::FAILURE;
    }
    let Ok(metadata) = serde_json::from_slice::<Value>(&bytes) else {
        return ExitCode::FAILURE;
    };
    // The store currently has one initialization migration for newly authorized
    // storage. It supplies no historical reader or existing-store upgrade. A
    // changed inventory requires an explicit compatibility decision, not a range
    // inferred from its schema number.
    if schema::SCHEMA_VERSION != 1
        || schema::migrations().len() != 1
        || schema::check_version(schema::SCHEMA_VERSION).is_err()
        || schema::check_version(schema::SCHEMA_VERSION + 1).is_ok()
    {
        return ExitCode::FAILURE;
    }
    let mut majors = SUPPORTED_MAJORS;
    majors.sort_unstable();
    if metadata.get("protocol_majors") == Some(&json!(majors))
        && metadata.get("storage")
            == Some(&json!({
                "read": [schema::SCHEMA_VERSION],
                "write": schema::SCHEMA_VERSION,
                "migrate_from": [],
            }))
    {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
