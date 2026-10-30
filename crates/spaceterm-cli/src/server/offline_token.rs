//! Confirmation tokens for actions confirmed on the server's own machine without a token
//! registry: an offline Update or Return, Unpair, or End All Sessions. Each token carries its
//! issue time, bound with the exact observation into its digest. It expires
//! after [`LIFETIME`], like a service-lane token (spec 6.3).
use crate::contract::{Code, Failure};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a token stays valid after it is issued.
pub const LIFETIME: Duration = Duration::from_secs(10 * 60);
const PREFIX: &str = "c1-";

/// A token and its RFC 3339 expiry.
pub struct Issued {
    pub token: String,
    pub expires_at: String,
}

fn seconds(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |v| v.as_secs())
}

fn digest(purpose: &str, observation: &[u8], issued: u64) -> String {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    context.update(b"spaceterm-cli-offline-token-1\0");
    context.update(purpose.as_bytes());
    context.update(b"\0");
    context.update(&issued.to_be_bytes());
    context.update(observation);
    super::hex(context.finish().as_ref())
}

/// Issues a token for `purpose` over `observation` at `now`.
pub fn issue(purpose: &str, observation: &[u8], now: SystemTime) -> Issued {
    let issued = seconds(now);
    let expires = issued + LIFETIME.as_secs();
    Issued {
        token: format!(
            "{PREFIX}{issued:016x}{}",
            digest(purpose, observation, issued)
        ),
        expires_at: jiff::Timestamp::from_second(i64::try_from(expires).unwrap_or(i64::MAX))
            .map(|v| v.to_string())
            .unwrap_or_default(),
    }
}

/// Accepts `token` only for the same purpose and observation, within its lifetime. Anything
/// else is `changed`: the person reviews the preview again.
pub fn verify(
    purpose: &str,
    observation: &[u8],
    token: &str,
    now: SystemTime,
) -> Result<(), Failure> {
    let changed = || Failure::new(Code::Changed);
    let body = token.strip_prefix(PREFIX).ok_or_else(changed)?;
    if body.len() != 16 + 64
        || !body
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(changed());
    }
    let issued = u64::from_str_radix(&body[..16], 16).map_err(|_| changed())?;
    let now = seconds(now);
    if issued > now || now - issued >= LIFETIME.as_secs() {
        return Err(changed());
    }
    // The digest binds a nonsecret observation, so comparing it needs no constant time.
    if body[16..] != digest(purpose, observation, issued) {
        return Err(changed());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_confirms_only_its_observation_within_its_lifetime() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let issued = issue("unpair", b"device", now);
        assert!(issued.token.starts_with("c1-"));
        assert_eq!(issued.expires_at, "2027-01-15T08:10:00Z");
        assert_eq!(verify("unpair", b"device", &issued.token, now), Ok(()));
        let later = now + LIFETIME - Duration::from_secs(1);
        assert_eq!(verify("unpair", b"device", &issued.token, later), Ok(()));
        let changed = Err(Failure::new(Code::Changed));
        // Expired, changed observation, another purpose, from the future, or malformed.
        assert_eq!(
            verify("unpair", b"device", &issued.token, now + LIFETIME),
            changed
        );
        assert_eq!(verify("unpair", b"other", &issued.token, now), changed);
        assert_eq!(verify("update", b"device", &issued.token, now), changed);
        assert_eq!(
            verify(
                "unpair",
                b"device",
                &issued.token,
                now - Duration::from_secs(5)
            ),
            changed
        );
        assert_eq!(verify("unpair", b"device", "c1-00", now), changed);
        let mut changed_case = issued.token.clone();
        changed_case.replace_range(3..19, &changed_case[3..19].to_ascii_uppercase());
        assert_ne!(changed_case, issued.token);
        assert_eq!(verify("unpair", b"device", &changed_case, now), changed);
        // The issue time is bound into the digest, so editing it is refused.
        let mut forged = issued.token.clone();
        forged.replace_range(3..19, &format!("{:016x}", 1_800_000_500u64));
        assert_eq!(
            verify("unpair", b"device", &forged, now + LIFETIME),
            changed
        );
    }
}
