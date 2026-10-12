use spaceterm_core::{
    authority::ServerEpoch,
    identity::{ClientId, PairingId, ServerId},
    organization::ServerDirectory,
    server_directories::*,
};
use spaceterm_protocol::{ProtocolError, convert::directories};

fn context() -> DirectoryContext {
    let server = ServerId::new([1; 16]);
    DirectoryContext::new(DirectoryContextParts {
        server,
        epoch: ServerEpoch::new(7),
        pairing: PairingId::new(server, 3).unwrap(),
        client: ClientId::new(server, 4).unwrap(),
    })
    .unwrap()
}

#[test]
fn directory_queries_preserve_non_utf8_and_exact_principal_in_both_majors() {
    let context = context();
    let directory = ServerDirectory::new(context.server(), b"/server/\xff".to_vec()).unwrap();
    for major in [1, 2] {
        for action in [
            DirectoryQueryAction::Home,
            DirectoryQueryAction::Browse {
                directory: directory.clone(),
                cursor: None,
            },
            DirectoryQueryAction::Validate {
                directory: directory.clone(),
                purpose: DirectoryPurpose::PaneRepair,
            },
        ] {
            let query = DirectoryQuery::new(DirectoryQueryParts { context, action }).unwrap();
            let request = directories::DirectoryRequest::new(1, 2, query).unwrap();
            assert_eq!(
                directories::decode_query(
                    &directories::encode_query(major, &request).unwrap(),
                    context
                )
                .unwrap(),
                request
            );
        }
    }
}

#[test]
fn directory_context_is_compared_to_authenticated_connection() {
    let request = directories::DirectoryRequest::new(
        1,
        2,
        DirectoryQuery::new(DirectoryQueryParts {
            context: context(),
            action: DirectoryQueryAction::Home,
        })
        .unwrap(),
    )
    .unwrap();
    let mut parts = context().parts();
    parts.client = ClientId::new(parts.server, 9).unwrap();
    let other = DirectoryContext::new(parts).unwrap();
    for major in [1, 2] {
        assert_eq!(
            directories::decode_query(&directories::encode_query(major, &request).unwrap(), other),
            Err(ProtocolError::PairingDenied)
        );
    }
}

#[test]
fn directory_pages_preserve_cursor_observation_and_non_utf8_names() {
    use std::num::NonZeroU64;
    let context = context();
    let directory = ServerDirectory::new(context.server(), b"/server".to_vec()).unwrap();
    let cursor = DirectoryCursor::new(DirectoryCursorParts {
        context,
        scope: directory.clone(),
        observation: NonZeroU64::new(11).unwrap(),
        random: [9; 32],
    })
    .unwrap();
    let page = DirectoryPage::new(DirectoryPageParts {
        context,
        directory: directory.clone(),
        observation: NonZeroU64::new(11).unwrap(),
        entries: vec![DirectoryEntry {
            name: DirectoryEntryName::new(vec![255]).unwrap(),
            directory: ServerDirectory::new(context.server(), b"/server/\xff".to_vec()).unwrap(),
            selectable: true,
        }],
        next: Some(cursor.clone()),
    })
    .unwrap();
    let response = directories::DirectoryResponse {
        request: NonZeroU64::new(1).unwrap(),
        generation: NonZeroU64::new(2).unwrap(),
        result: Ok(DirectoryQueryResult::Page(Box::new(page))),
    };
    for major in [1, 2] {
        let payload = directories::encode_response(major, &response, context).unwrap();
        let bytes = payload.encode().unwrap();
        let decoded = spaceterm_protocol::codec::Payload::decode(major, &bytes).unwrap();
        assert_eq!(
            directories::decode_response(&decoded, context).unwrap(),
            response
        );
        let request = directories::DirectoryRequest::new(
            1,
            2,
            DirectoryQuery::new(DirectoryQueryParts {
                context,
                action: DirectoryQueryAction::Browse {
                    directory: directory.clone(),
                    cursor: Some(cursor.clone()),
                },
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            directories::decode_query(
                &directories::encode_query(major, &request).unwrap(),
                context
            )
            .unwrap(),
            request
        );
    }
}

#[test]
fn directory_validation_preserves_every_purpose_and_classification() {
    let context = context();
    for purpose in [
        DirectoryPurpose::WorkspacePin,
        DirectoryPurpose::SessionLaunch,
        DirectoryPurpose::PaneRepair,
        DirectoryPurpose::WorktreeCreateTarget,
        DirectoryPurpose::WorktreeFallback,
    ] {
        for class in [
            DirectoryValidationClass::AvailableDirectory,
            DirectoryValidationClass::AbsentCreateTarget,
            DirectoryValidationClass::EmptyCreateTarget,
            DirectoryValidationClass::Missing,
            DirectoryValidationClass::NotDirectory,
            DirectoryValidationClass::Unavailable,
            DirectoryValidationClass::IdentityChanged,
            DirectoryValidationClass::Unchecked,
        ] {
            let response = directories::DirectoryResponse {
                request: std::num::NonZeroU64::new(1).unwrap(),
                generation: std::num::NonZeroU64::new(2).unwrap(),
                result: Ok(DirectoryQueryResult::Validation(DirectoryValidation {
                    directory: ServerDirectory::new(context.server(), b"/validate".to_vec())
                        .unwrap(),
                    purpose,
                    class,
                })),
            };
            for major in [1, 2] {
                assert_eq!(
                    directories::decode_response(
                        &directories::encode_response(major, &response, context).unwrap(),
                        context
                    )
                    .unwrap(),
                    response
                );
            }
        }
    }
}

#[test]
fn mismatched_scope_or_observation_and_duplicate_entries_are_rejected() {
    macro_rules! check {
        ($p:ident) => {{
            use spaceterm_protocol::wire::directories::$p as w;
            let context = context();
            let directory = ServerDirectory::new(context.server(), b"/server".to_vec()).unwrap();
            let cursor = DirectoryCursor::new(DirectoryCursorParts {
                context,
                scope: directory.clone(),
                observation: std::num::NonZeroU64::new(11).unwrap(),
                random: [9; 32],
            })
            .unwrap();
            let mut page = w::DirectoryPage {
                context: Some(directories::$p::encode_cursor(&cursor).context.unwrap()),
                directory: Some(directories::$p::encode_directory(&directory)),
                observation: 11,
                entries: vec![w::DirectoryEntry {
                    name: Some(w::DirectoryEntryName {
                        value: b"child".to_vec(),
                    }),
                    directory: Some(directories::$p::encode_directory(
                        &ServerDirectory::new(context.server(), b"/server/child".to_vec()).unwrap(),
                    )),
                    selectable: true,
                }],
                next: Some(directories::$p::encode_cursor(&cursor)),
            };
            let response = |p: w::DirectoryPage| w::DirectoryQueryResult {
                request: 1,
                request_generation: 2,
                result: Some(w::directory_query_result::Result::Page(Box::new(p))),
            };
            assert!(directories::$p::decode_response(&response(page.clone()), context).is_ok());
            page.next.as_mut().unwrap().observation = 12;
            assert_eq!(
                directories::$p::decode_response(&response(page.clone()), context),
                Err(ProtocolError::Malformed)
            );
            page.next.as_mut().unwrap().observation = 11;
            page.entries.push(page.entries[0].clone());
            assert_eq!(
                directories::$p::decode_response(&response(page.clone()), context),
                Err(ProtocolError::Malformed)
            );
            page.entries.pop();
            page.next.as_mut().unwrap().scope.as_mut().unwrap().value = b"/another".to_vec();
            assert_eq!(
                directories::$p::decode_response(&response(page), context),
                Err(ProtocolError::Malformed)
            );
        }};
    }
    check!(v1);
    check!(v2);
}

#[test]
fn directory_count_byte_and_basename_bounds_reject_without_truncation() {
    macro_rules! check {
        ($p:ident,$major:ident) => {{
            use spaceterm_protocol::{
                codec::Payload,
                wire::{directories::$p as w, $p},
            };
            let request = directories::$p::encode_query(
                &directories::DirectoryRequest::new(
                    1,
                    2,
                    DirectoryQuery::new(DirectoryQueryParts {
                        context: context(),
                        action: DirectoryQueryAction::Home,
                    })
                    .unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
            let entries = |count: usize, name_length: usize| {
                (0..count)
                    .map(|i| w::DirectoryEntry {
                        name: Some(w::DirectoryEntryName {
                            value: format!("{i:03}{}", "x".repeat(name_length - 3)).into_bytes(),
                        }),
                        directory: Some(directories::$p::encode_directory(
                            &ServerDirectory::new(
                                context().server(),
                                format!("/server/{i}").into_bytes(),
                            )
                            .unwrap(),
                        )),
                        selectable: true,
                    })
                    .collect()
            };
            let payload = |entries| {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::ServerDirectoryResult(
                        w::DirectoryQueryResult {
                            request: 1,
                            request_generation: 2,
                            result: Some(w::directory_query_result::Result::Page(Box::new(
                                w::DirectoryPage {
                                    context: request.context.clone(),
                                    directory: Some(directories::$p::encode_directory(
                                        &ServerDirectory::new(
                                            context().server(),
                                            b"/server".to_vec(),
                                        )
                                        .unwrap(),
                                    )),
                                    observation: 11,
                                    entries,
                                    next: None,
                                },
                            ))),
                        },
                    )),
                })
            };
            assert!(payload(entries(128, 255)).encode().is_ok());
            assert_eq!(
                payload(entries(129, 255)).encode(),
                Err(ProtocolError::LimitExceeded)
            );
            assert_eq!(
                payload(entries(1, 256)).encode(),
                Err(ProtocolError::LimitExceeded)
            );
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn unavailable_directory_failures_never_become_authoritative_missing() {
    for major in [1, 2] {
        for remote in [DirectoryFailure::Unavailable, DirectoryFailure::Unchecked] {
            let response = directories::DirectoryResponse {
                request: std::num::NonZeroU64::new(1).unwrap(),
                generation: std::num::NonZeroU64::new(2).unwrap(),
                result: Err(remote),
            };
            let decoded = directories::decode_response(
                &directories::encode_response(major, &response, context()).unwrap(),
                context(),
            )
            .unwrap();
            assert_eq!(decoded.result, Err(DirectoryFailure::Unavailable));
        }
    }
}
