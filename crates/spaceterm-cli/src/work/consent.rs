//! An opaque copy of the exact server-issued review. It grants no authority beyond the
//! authenticated client's existing ability to submit that review to the server.
use crate::contract::{Code, Failure};
use serde::{Deserialize, Serialize};
use spaceterm_server::{
    hierarchy::{CloseConfirmation, CloseTarget, Id},
    worktree::Removal,
};

const MAX_TOKEN_BYTES: usize = 128 * 1024;
const LIFETIME: i64 = 300;

#[derive(Clone, Serialize, Deserialize)]
pub enum Review {
    Close {
        target: CloseTarget,
        confirmation: CloseConfirmation,
    },
    Removal(Removal),
}

#[derive(Serialize, Deserialize)]
struct Token {
    server: Id,
    client: Id,
    expires: i64,
    review: Review,
}

pub fn issue(
    server: Id,
    client: Id,
    review: Review,
    now: i64,
) -> Result<(String, String), Failure> {
    let expires = now
        .checked_add(LIFETIME)
        .ok_or_else(|| Failure::new(Code::StorageUnavailable))?;
    let bytes = postcard::to_allocvec(&Token {
        server,
        client,
        expires,
        review,
    })
    .map_err(|_| Failure::new(Code::ProtocolError))?;
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err(Failure::new(Code::Capacity));
    }
    let value: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let expires_at = jiff::Timestamp::from_second(expires)
        .map_err(|_| Failure::new(Code::ProtocolError))?
        .to_string();
    Ok((format!("c1-{value}"), expires_at))
}

pub fn verify(value: &str, server: Id, client: Id, now: i64) -> Result<Review, Failure> {
    let changed = || Failure::new(Code::Changed);
    let text = value.strip_prefix("c1-").ok_or_else(changed)?;
    if text.len() > MAX_TOKEN_BYTES * 2
        || !text.len().is_multiple_of(2)
        || !text.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(changed());
    }
    let bytes: Vec<_> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| changed()))
        .collect::<Result<_, _>>()?;
    let (token, rest): (Token, _) = postcard::take_from_bytes(&bytes).map_err(|_| changed())?;
    if !rest.is_empty()
        || token.server != server
        || token.client != client
        || now >= token.expires
        || token.expires > now.saturating_add(LIFETIME)
    {
        return Err(changed());
    }
    Ok(token.review)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::hierarchy::ConfirmedPane;

    #[test]
    fn exact_review_is_scoped_to_actor_server_and_expiry() {
        let target = CloseTarget::Workspace { workspace: [3; 32] };
        let review = Review::Close {
            target,
            confirmation: CloseConfirmation {
                boot: [4; 32],
                panes: vec![ConfirmedPane {
                    id: [5; 32],
                    session: Some([6; 32]),
                }],
                removes_tab: true,
                removes_workspace: true,
                own_controls: None,
            },
        };
        let (token, _) = issue([1; 32], [2; 32], review, 100).unwrap();
        assert!(
            matches!(verify(&token,[1;32],[2;32],101).unwrap(), Review::Close { target: t, .. } if t == target)
        );
        for (server, client, time) in [
            ([9; 32], [2; 32], 101),
            ([1; 32], [9; 32], 101),
            ([1; 32], [2; 32], 400),
        ] {
            assert!(matches!(
                verify(&token, server, client, time),
                Err(Failure {
                    code: Code::Changed,
                    ..
                })
            ));
        }
        assert!(verify(&(token + "00"), [1; 32], [2; 32], 101).is_err());
    }
}
