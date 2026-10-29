use super::*;

/// Environment context is valid only for this executable's identity and selected server.
pub(super) fn from_context(
    snapshot: &Snapshot,
    executable_identity: &str,
    identity: Option<&str>,
    server: Option<&str>,
    pane: Option<&str>,
) -> Option<Current> {
    if identity != Some(executable_identity) {
        return None;
    }
    let server = full_id(server?)?;
    let pane = full_id(pane?)?;
    if server != snapshot.server
        || !snapshot
            .inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .any(|p| p.id == pane)
    {
        return None;
    }
    Some(Current { server, pane })
}

fn full_id(text: &str) -> Option<Id> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut id = [0; 32];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{fixture, id};
    use super::*;

    #[test]
    fn environment_context_requires_identity_server_and_existing_pane() {
        let snapshot = fixture();
        let server = "09".repeat(32);
        let pane = "16".repeat(32);
        assert_eq!(
            from_context(
                &snapshot,
                "spaceterm-development",
                Some("spaceterm-development"),
                Some(&server),
                Some(&pane)
            ),
            Some(Current {
                server: id(9),
                pane: id(22)
            })
        );
        for identity in [None, Some("another-identity")] {
            assert_eq!(
                from_context(
                    &snapshot,
                    "spaceterm-development",
                    identity,
                    Some(&server),
                    Some(&pane)
                ),
                None
            );
        }
        for server in [
            None,
            Some("090909"),
            Some("invalid"),
            Some("0101010101010101010101010101010101010101010101010101010101010101"),
        ] {
            assert_eq!(
                from_context(
                    &snapshot,
                    "spaceterm-development",
                    Some("spaceterm-development"),
                    server,
                    Some(&pane)
                ),
                None
            );
        }
        assert_eq!(
            from_context(
                &snapshot,
                "spaceterm-development",
                Some("spaceterm-development"),
                Some(&server),
                Some(&"ff".repeat(32))
            ),
            None
        );
    }
}
