//! Opaque read cursors: `h1-` and the base64url of the postcard encoding of [`Cursor`].
//!
//! A cursor names one history row of one Terminal Session, by the leading 16 bytes of each ID.
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

const PREFIX: &str = "h1-";
/// Longer than any encoded cursor, so decoding never reads unbounded input.
const MAX_TEXT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub server: [u8; 16],
    pub pane: [u8; 16],
    pub session: [u8; 16],
    pub epoch: u64,
    pub row: u64,
}

/// The text is not a cursor this CLI wrote. It carries no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidCursor;

impl Cursor {
    pub fn encode(&self) -> String {
        let bytes = postcard::to_allocvec(self).expect("a cursor always encodes");
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn decode(text: &str) -> Result<Self, InvalidCursor> {
        if text.len() > MAX_TEXT {
            return Err(InvalidCursor);
        }
        let encoded = text.strip_prefix(PREFIX).ok_or(InvalidCursor)?;
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| InvalidCursor)?;
        match postcard::take_from_bytes::<Self>(&bytes) {
            Ok((cursor, [])) => Ok(cursor),
            _ => Err(InvalidCursor),
        }
    }

    /// The leading 16 bytes of an ID, as a cursor stores it.
    pub fn short(id: &[u8; 32]) -> [u8; 16] {
        let mut short = [0; 16];
        short.copy_from_slice(&id[..16]);
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_anything_else() {
        let cursor = Cursor {
            server: [1; 16],
            pane: [2; 16],
            session: [3; 16],
            epoch: 4,
            row: u64::MAX,
        };
        let text = cursor.encode();
        assert!(text.starts_with("h1-"));
        assert_eq!(Cursor::decode(&text), Ok(cursor));
        assert_eq!(Cursor::decode(&format!("{text}A")), Err(InvalidCursor));
        assert_eq!(Cursor::decode(&text[1..]), Err(InvalidCursor));
        assert_eq!(Cursor::decode("h1-"), Err(InvalidCursor));
        assert_eq!(Cursor::decode(&"h1-A".repeat(64)), Err(InvalidCursor));
        let mut trailing = postcard::to_allocvec(&cursor).unwrap();
        trailing.push(0);
        assert_eq!(
            Cursor::decode(&format!("h1-{}", URL_SAFE_NO_PAD.encode(trailing))),
            Err(InvalidCursor)
        );
    }
}
