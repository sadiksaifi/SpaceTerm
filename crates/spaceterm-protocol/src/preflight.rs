//! Allocation-free, descriptor-derived protobuf admission before prost decoding.

use crate::{
    ProtocolError,
    limits::{self, NegotiatedLimits},
};

enum Kind {
    Varint(u64),
    Enum(&'static [i32]),
    String(usize),
    Bytes(usize, usize),
    Message(usize),
}
enum ContentLimit {
    Other,
    Paste,
    Clipboard,
    Resource,
}
struct Field {
    number: i32,
    kind: Kind,
    repeated: bool,
    oneof: usize,
    max_count: usize,
    required: bool,
    content_limit: ContentLimit,
}
enum Semantic {
    Other,
    Row,
    Cell,
}
struct Schema {
    semantic: Semantic,
    fields: &'static [Field],
    required_oneofs: u64,
}
// Most descriptors are only reached recursively. Exported roots are referenced below.
#[expect(
    dead_code,
    reason = "only root descriptor constants are named by the decoder"
)]
mod generated {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/preflight.rs"));
}
pub(crate) use generated::{
    SPACETERM_COMMON_AUTHENTICATEDENVELOPE, SPACETERM_COMMON_CLIENTHELLO, SPACETERM_COMMON_READY,
    SPACETERM_COMMON_RESTRICTEDMESSAGE, SPACETERM_COMMON_SERVERHELLO, SPACETERM_COMMON_SIGNED,
    SPACETERM_V1_MESSAGE, SPACETERM_V2_MESSAGE,
};

struct Budget {
    fields: usize,
    repeated: usize,
    rows: usize,
    cells: usize,
    limits: NegotiatedLimits,
}

pub(crate) fn check(
    schema: usize,
    bytes: &[u8],
    limits: NegotiatedLimits,
) -> Result<(), ProtocolError> {
    if bytes.len() > limits.frame_bytes {
        return Err(ProtocolError::LimitExceeded);
    }
    let mut budget = Budget {
        fields: 0,
        repeated: 0,
        rows: 0,
        cells: 0,
        limits,
    };
    walk(schema, bytes, 0, &mut budget)
}

fn walk(
    schema: usize,
    mut bytes: &[u8],
    depth: usize,
    budget: &mut Budget,
) -> Result<(), ProtocolError> {
    if depth > limits::MAX_NESTING_DEPTH {
        return Err(ProtocolError::LimitExceeded);
    }
    let schema = &generated::SCHEMAS[schema];
    match schema.semantic {
        Semantic::Row => {
            budget.rows += 1;
            if budget.rows > budget.limits.semantic_rows {
                return Err(ProtocolError::LimitExceeded);
            }
        }
        Semantic::Cell => {
            budget.cells += 1;
            if budget.cells > budget.limits.semantic_cells {
                return Err(ProtocolError::LimitExceeded);
            }
        }
        Semantic::Other => {}
    }
    let mut counts = [0usize; 64];
    let mut oneofs = 0u64;
    while !bytes.is_empty() {
        budget.fields += 1;
        if budget.fields > limits::MAX_FIELDS {
            return Err(ProtocolError::LimitExceeded);
        }
        let tag = varint(&mut bytes)?;
        let number = tag >> 3;
        let wire = tag & 7;
        let (index, field) = schema
            .fields
            .iter()
            .enumerate()
            .find(|(_, field)| field.number as u64 == number)
            .ok_or(ProtocolError::Malformed)?;
        if field.oneof != 0 {
            let bit = 1 << (field.oneof - 1);
            if oneofs & bit != 0 {
                return Err(ProtocolError::Malformed);
            }
            oneofs |= bit;
        }
        if !field.repeated && counts[index] != 0 {
            return Err(ProtocolError::Malformed);
        }
        if field.repeated && wire == 2 && matches!(field.kind, Kind::Varint(_) | Kind::Enum(_)) {
            let mut packed = length_delimited(&mut bytes)?;
            while !packed.is_empty() {
                admit_count(field, &mut counts[index], budget)?;
                scalar(field, varint(&mut packed)?)?;
            }
            continue;
        }
        admit_count(field, &mut counts[index], budget)?;
        match (&field.kind, wire) {
            (Kind::Varint(_) | Kind::Enum(_), 0) => {
                scalar(field, varint(&mut bytes)?)?;
            }
            (Kind::String(max), 2) => {
                let value = length_delimited(&mut bytes)?;
                if value.len() > *max {
                    return Err(ProtocolError::LimitExceeded);
                }
                std::str::from_utf8(value).map_err(|_| ProtocolError::Malformed)?;
                if field.required && value.is_empty() {
                    return Err(ProtocolError::Malformed);
                }
            }
            (Kind::Bytes(max, exact), 2) => {
                let value = length_delimited(&mut bytes)?;
                let negotiated = match field.content_limit {
                    ContentLimit::Other => *max,
                    ContentLimit::Paste => budget.limits.paste_bytes,
                    ContentLimit::Clipboard => budget.limits.clipboard_bytes,
                    ContentLimit::Resource => budget.limits.resource_chunk_bytes,
                };
                if value.len() > (*max).min(negotiated) {
                    return Err(ProtocolError::LimitExceeded);
                }
                if (*exact != 0 && value.len() != *exact)
                    || (field.required && value.is_empty())
                    || (*exact == 16 && value.iter().all(|byte| *byte == 0))
                {
                    return Err(ProtocolError::Malformed);
                }
            }
            (Kind::Message(nested), 2) => {
                walk(*nested, length_delimited(&mut bytes)?, depth + 1, budget)?;
            }
            _ => return Err(ProtocolError::Malformed),
        }
    }
    if schema
        .fields
        .iter()
        .enumerate()
        .any(|(index, field)| field.required && counts[index] == 0)
        || (oneofs & schema.required_oneofs) != schema.required_oneofs
    {
        return Err(ProtocolError::Malformed);
    }
    Ok(())
}

fn scalar(field: &Field, value: u64) -> Result<(), ProtocolError> {
    if field.required && value == 0 {
        return Err(ProtocolError::Malformed);
    }
    match &field.kind {
        Kind::Varint(max) if value <= *max => Ok(()),
        Kind::Enum(values) if i32::try_from(value).is_ok_and(|value| values.contains(&value)) => {
            Ok(())
        }
        _ => Err(ProtocolError::Malformed),
    }
}

fn admit_count(field: &Field, count: &mut usize, budget: &mut Budget) -> Result<(), ProtocolError> {
    *count += 1;
    if field.repeated {
        budget.repeated += 1;
        if *count > field.max_count || budget.repeated > limits::MAX_REPEATED_VALUES {
            return Err(ProtocolError::LimitExceeded);
        }
    }
    Ok(())
}

pub(crate) fn varint(bytes: &mut &[u8]) -> Result<u64, ProtocolError> {
    let mut result = 0u64;
    for shift in (0..70).step_by(7) {
        let (&byte, rest) = bytes.split_first().ok_or(ProtocolError::Malformed)?;
        *bytes = rest;
        if shift == 63 && byte > 1 {
            return Err(ProtocolError::Malformed);
        }
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            if shift != 0 && byte == 0 {
                return Err(ProtocolError::Malformed);
            }
            return Ok(result);
        }
    }
    Err(ProtocolError::Malformed)
}

pub(crate) fn length_delimited<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], ProtocolError> {
    let length = usize::try_from(varint(bytes)?).map_err(|_| ProtocolError::LimitExceeded)?;
    if length > bytes.len() {
        return Err(ProtocolError::Malformed);
    }
    let (value, rest) = bytes.split_at(length);
    *bytes = rest;
    Ok(value)
}

/// Returns the single top-level oneof tag without decoding or allocating.
pub(crate) fn payload_tag(mut bytes: &[u8]) -> Result<u32, ProtocolError> {
    let tag = varint(&mut bytes)?;
    if tag & 7 != 2 {
        return Err(ProtocolError::Malformed);
    }
    length_delimited(&mut bytes)?;
    if !bytes.is_empty() {
        return Err(ProtocolError::Malformed);
    }
    u32::try_from(tag >> 3).map_err(|_| ProtocolError::Malformed)
}
