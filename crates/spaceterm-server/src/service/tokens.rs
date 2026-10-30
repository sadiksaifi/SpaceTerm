//! Confirmation tokens for service-lane previews. A token binds one action, one actor and the
//! exact observation its preview showed. It lives only in the memory of the server that issued
//! it, so a server restart invalidates every token.
use super::operations::Actor;
use crate::ServerError;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// How long a preview's token stays valid.
pub const LIFETIME: Duration = Duration::from_secs(10 * 60);
/// The most tokens one server holds. Issuing past it forgets the oldest.
pub const MAX_TOKENS: usize = 16;
const PREFIX: &str = "c1-";

/// A token as the lane carries it, with its wall-clock expiry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issued {
    pub token: String,
    pub expires_at: SystemTime,
}

struct Entry {
    token: [u8; 32],
    /// The action this token confirms, in a form only this module compares.
    action: Vec<u8>,
    observation: [u8; 32],
    actor: Actor,
    expires: Instant,
}

#[derive(Default)]
pub struct Tokens {
    entries: Mutex<Vec<Entry>>,
}

impl Tokens {
    /// Issues a token for `action`, as observed by `observation`, to `actor`.
    pub fn issue(
        &self,
        action: &[u8],
        observation: [u8; 32],
        actor: Actor,
        now: Instant,
    ) -> Result<Issued, ServerError> {
        let token = crate::protocol::random()?;
        let mut entries = self.entries.lock().map_err(|_| ServerError::Protocol)?;
        entries.retain(|v| v.expires > now);
        if entries.len() >= MAX_TOKENS {
            entries.remove(0);
        }
        entries.push(Entry {
            token,
            action: action.to_vec(),
            observation,
            actor,
            expires: now + LIFETIME,
        });
        Ok(Issued {
            token: format!("{PREFIX}{}", hex(&token)),
            expires_at: SystemTime::now() + LIFETIME,
        })
    }

    /// Consumes a token and returns the observation it was issued for. An unknown, expired or
    /// foreign token, or one for another action, is `Changed`: the person reviews again.
    pub fn redeem(
        &self,
        token: &str,
        action: &[u8],
        actor: &Actor,
        now: Instant,
    ) -> Result<[u8; 32], ServerError> {
        let token = parse(token).ok_or(ServerError::Changed)?;
        let mut entries = self.entries.lock().map_err(|_| ServerError::Protocol)?;
        entries.retain(|v| v.expires > now);
        let index = entries
            .iter()
            .position(|v| v.token == token)
            .ok_or(ServerError::Changed)?;
        if entries[index].actor != *actor || entries[index].action != action {
            return Err(ServerError::Changed);
        }
        Ok(entries.remove(index).observation)
    }
}

fn parse(text: &str) -> Option<[u8; 32]> {
    let hex = text.strip_prefix(PREFIX)?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return None;
    }
    let mut token = [0; 32];
    for (index, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(token)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR: Actor = Actor {
        pairing: None,
        client: [1; 32],
    };

    #[test]
    fn a_token_confirms_once_for_its_own_action_actor_and_lifetime() {
        let tokens = Tokens::default();
        let now = Instant::now();
        let issued = tokens.issue(b"stop", [5; 32], ACTOR, now).unwrap();
        assert!(issued.token.starts_with("c1-") && issued.token.len() == 67);
        let other = Actor {
            pairing: Some([2; 32]),
            client: [1; 32],
        };
        assert_eq!(
            tokens.redeem(&issued.token, b"stop", &other, now),
            Err(ServerError::Changed)
        );
        assert_eq!(
            tokens.redeem(&issued.token, b"restart", &ACTOR, now),
            Err(ServerError::Changed)
        );
        assert_eq!(
            tokens.redeem(&issued.token, b"stop", &ACTOR, now),
            Ok([5; 32])
        );
        assert_eq!(
            tokens.redeem(&issued.token, b"stop", &ACTOR, now),
            Err(ServerError::Changed)
        );
        let issued = tokens.issue(b"stop", [5; 32], ACTOR, now).unwrap();
        assert_eq!(
            tokens.redeem(&issued.token, b"stop", &ACTOR, now + LIFETIME),
            Err(ServerError::Changed)
        );
        assert_eq!(
            tokens.redeem("c1-zz", b"stop", &ACTOR, now),
            Err(ServerError::Changed)
        );
    }

    #[test]
    fn issuing_past_the_limit_forgets_the_oldest_token() {
        let tokens = Tokens::default();
        let now = Instant::now();
        let first = tokens.issue(b"stop", [0; 32], ACTOR, now).unwrap();
        let rest: Vec<_> = (0..MAX_TOKENS)
            .map(|_| tokens.issue(b"stop", [1; 32], ACTOR, now).unwrap())
            .collect();
        assert_eq!(
            tokens.redeem(&first.token, b"stop", &ACTOR, now),
            Err(ServerError::Changed)
        );
        assert_eq!(
            tokens.redeem(&rest[0].token, b"stop", &ACTOR, now),
            Ok([1; 32])
        );
    }
}
