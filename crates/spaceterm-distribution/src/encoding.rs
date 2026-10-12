use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serializer, de::Error};

pub(crate) fn hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2 || !value.bytes().all(|b| matches!(b, b'0'..=b'9'|b'a'..=b'f')) {
        return None;
    }
    let mut result = [0; N];
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(result)
}
macro_rules! hex_codec {
    ($name:ident, $size:expr) => {
        pub(crate) mod $name {
            use super::*;
            pub fn serialize<S: Serializer>(
                value: &[u8; $size],
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                serializer
                    .serialize_str(&value.iter().map(|b| format!("{b:02x}")).collect::<String>())
            }
            pub fn deserialize<'de, D: Deserializer<'de>>(
                deserializer: D,
            ) -> Result<[u8; $size], D::Error> {
                hex(&String::deserialize(deserializer)?)
                    .ok_or_else(|| D::Error::custom("invalid digest"))
            }
        }
    };
}
hex_codec!(sha256, 32);
hex_codec!(commit, 20);

pub(crate) mod signature {
    use super::*;
    pub fn serialize<S: Serializer>(value: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(value))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        let value = String::deserialize(deserializer)?;
        let bytes = STANDARD
            .decode(&value)
            .map_err(|_| D::Error::custom("invalid signature"))?;
        if STANDARD.encode(&bytes) != value {
            return Err(D::Error::custom("invalid signature"));
        }
        bytes
            .try_into()
            .map_err(|_| D::Error::custom("invalid signature"))
    }
}

// deserialize_with prevents Serde's implicit missing-Option default. The wire field
// must exist, even when its value is null.
pub(crate) fn required_optional<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}
