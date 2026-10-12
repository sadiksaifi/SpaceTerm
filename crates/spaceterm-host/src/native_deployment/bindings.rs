//! Canonical scalar encodings used by native facts records.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::contracts::NativeFailure;

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 15)] as char);
    }
    out
}

pub(crate) fn unhex<const N: usize>(text: &str) -> Result<[u8; N], NativeFailure> {
    if text.len() != N * 2 {
        return Err(NativeFailure::InvalidRecord);
    }
    let nibble = |b| match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(NativeFailure::InvalidRecord),
    };
    let mut out = [0; N];
    for (i, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        out[i] = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Ok(out)
}

pub(crate) fn canonical<T: Serialize>(record: &T, limit: usize) -> Result<Vec<u8>, NativeFailure> {
    let mut value = serde_json::to_value(record).map_err(|_| NativeFailure::InvalidRecord)?;
    // Other workspace consumers may enable preserve_order through feature unification.
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|_| NativeFailure::InvalidRecord)?;
    if bytes.len() > limit {
        return Err(NativeFailure::LimitExceeded);
    }
    Ok(bytes)
}

pub(crate) fn decode_canonical<T: Serialize + DeserializeOwned>(
    bytes: &[u8],
    limit: usize,
) -> Result<T, NativeFailure> {
    if bytes.len() > limit {
        return Err(NativeFailure::LimitExceeded);
    }
    // Deserialize directly, not through Value: derives reject duplicate struct fields.
    let record: T = serde_json::from_slice(bytes).map_err(|_| NativeFailure::InvalidRecord)?;
    if canonical(&record, limit)? != bytes {
        return Err(NativeFailure::InvalidRecord);
    }
    Ok(record)
}

pub(crate) mod digest {
    use super::*;
    pub fn serialize<S: serde::Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex(bytes))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        unhex(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

pub(crate) mod identity {
    use super::*;
    pub fn serialize<S: serde::Serializer>(bytes: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex(bytes))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[u8; 16], D::Error> {
        unhex(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

pub(crate) mod optional_digest {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        bytes: &Option<[u8; 32]>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        bytes.as_ref().map(|b| hex(b)).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| unhex(&s).map_err(serde::de::Error::custom))
            .transpose()
    }
}
