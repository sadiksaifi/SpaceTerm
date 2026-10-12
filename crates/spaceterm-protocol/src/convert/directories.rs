//! Checked owning-server directory conversions. Native leases never cross this
//! boundary. Receive bytes through the authenticated bounded codec first.

use crate::{ProtocolError, codec::Payload, wire::common};
use prost::Message;
use spaceterm_core::{
    authority::ServerEpoch,
    identity::{ClientId, PairingId, ServerId},
    organization::ServerDirectory,
    server_directories::*,
};
use std::num::NonZeroU64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryRequest {
    pub request: NonZeroU64,
    pub generation: NonZeroU64,
    pub query: DirectoryQuery,
}
impl DirectoryRequest {
    pub fn new(
        request: u64,
        generation: u64,
        query: DirectoryQuery,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            request: nonzero(request)?,
            generation: nonzero(generation)?,
            query,
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryResponse {
    pub request: NonZeroU64,
    pub generation: NonZeroU64,
    pub result: Result<DirectoryQueryResult, DirectoryFailure>,
}

pub fn encode_query(major: u32, request: &DirectoryRequest) -> Result<Payload, ProtocolError> {
    match major {
        1 => Ok(Payload::Major1(crate::wire::v1::Message {
            kind: Some(crate::wire::v1::message::Kind::ServerDirectoryQuery(
                v1::encode_query(request)?,
            )),
        })),
        2 => Ok(Payload::Major2(crate::wire::v2::Message {
            kind: Some(crate::wire::v2::message::Kind::ServerDirectoryQuery(
                v2::encode_query(request)?,
            )),
        })),
        _ => Err(ProtocolError::Incompatible),
    }
}
pub fn decode_query(
    payload: &Payload,
    context: DirectoryContext,
) -> Result<DirectoryRequest, ProtocolError> {
    match payload {
        Payload::Major1(message) => match &message.kind {
            Some(crate::wire::v1::message::Kind::ServerDirectoryQuery(query)) => {
                v1::decode_query(query, context)
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        },
        Payload::Major2(message) => match &message.kind {
            Some(crate::wire::v2::message::Kind::ServerDirectoryQuery(query)) => {
                v2::decode_query(query, context)
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        },
    }
}
pub fn encode_response(
    major: u32,
    response: &DirectoryResponse,
    context: DirectoryContext,
) -> Result<Payload, ProtocolError> {
    match major {
        1 => Ok(Payload::Major1(crate::wire::v1::Message {
            kind: Some(crate::wire::v1::message::Kind::ServerDirectoryResult(
                v1::encode_response(response, context)?,
            )),
        })),
        2 => Ok(Payload::Major2(crate::wire::v2::Message {
            kind: Some(crate::wire::v2::message::Kind::ServerDirectoryResult(
                v2::encode_response(response, context)?,
            )),
        })),
        _ => Err(ProtocolError::Incompatible),
    }
}
pub fn decode_response(
    payload: &Payload,
    context: DirectoryContext,
) -> Result<DirectoryResponse, ProtocolError> {
    match payload {
        Payload::Major1(message) => match &message.kind {
            Some(crate::wire::v1::message::Kind::ServerDirectoryResult(response)) => {
                v1::decode_response(response, context)
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        },
        Payload::Major2(message) => match &message.kind {
            Some(crate::wire::v2::message::Kind::ServerDirectoryResult(response)) => {
                v2::decode_response(response, context)
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        },
    }
}

pub(crate) fn server(value: ServerId) -> common::Identity {
    common::Identity {
        value: value.bytes().to_vec(),
    }
}
pub(crate) fn decode_server(value: Option<&common::Identity>) -> Result<ServerId, ProtocolError> {
    Ok(ServerId::new(
        value
            .ok_or(ProtocolError::Malformed)?
            .value
            .as_slice()
            .try_into()
            .map_err(|_| ProtocolError::Malformed)?,
    ))
}
pub(crate) fn entity(server_id: ServerId, local_id: u64) -> common::Entity {
    common::Entity {
        server: Some(server(server_id)),
        local_id,
    }
}
pub(crate) fn decode_entity(
    value: Option<&common::Entity>,
    expected: ServerId,
) -> Result<u64, ProtocolError> {
    let value = value.ok_or(ProtocolError::Malformed)?;
    if decode_server(value.server.as_ref())? != expected || value.local_id == 0 {
        return Err(ProtocolError::Malformed);
    }
    Ok(value.local_id)
}
pub(crate) fn nonzero(value: u64) -> Result<NonZeroU64, ProtocolError> {
    NonZeroU64::new(value).ok_or(ProtocolError::Malformed)
}
pub(crate) fn bounded<T: Message>(value: &T, maximum: usize) -> Result<(), ProtocolError> {
    if value.encoded_len() > maximum {
        Err(ProtocolError::LimitExceeded)
    } else {
        Ok(())
    }
}
fn failure(value: DirectoryFailure) -> ProtocolError {
    match value {
        DirectoryFailure::Capacity => ProtocolError::LimitExceeded,
        _ => ProtocolError::Malformed,
    }
}

// The frozen directory profiles have only common.Failure. Preserve those closed
// wire classes, not a new numeric dialect. Several native classifications are
// deliberately coarsened; unavailable/unchecked can never become missing.
fn encode_failure(value: DirectoryFailure) -> common::Failure {
    use common::FailureClass as F;
    let classification = match value {
        DirectoryFailure::Unauthorized => F::Denied,
        DirectoryFailure::WrongServer
        | DirectoryFailure::InvalidDirectory
        | DirectoryFailure::NotDirectory => F::Invalid,
        DirectoryFailure::Unavailable | DirectoryFailure::Unchecked => F::Unavailable,
        DirectoryFailure::Missing => F::NotFound,
        DirectoryFailure::IdentityChanged => F::Conflict,
        DirectoryFailure::Capacity => F::Limit,
        DirectoryFailure::StaleCursor => F::Stale,
    };
    common::Failure {
        classification: classification as i32,
        retry_after_ms: 0,
    }
}
fn decode_failure(value: &common::Failure) -> Result<DirectoryFailure, ProtocolError> {
    use common::FailureClass as F;
    if value.retry_after_ms > 30_000 {
        return Err(ProtocolError::Malformed);
    }
    Ok(
        match F::try_from(value.classification).map_err(|_| ProtocolError::Malformed)? {
            F::Denied => DirectoryFailure::Unauthorized,
            F::Invalid => DirectoryFailure::InvalidDirectory,
            F::Unavailable => DirectoryFailure::Unavailable,
            F::NotFound => DirectoryFailure::Missing,
            F::Conflict => DirectoryFailure::IdentityChanged,
            F::Limit | F::Busy => DirectoryFailure::Capacity,
            F::Stale | F::ResyncRequired => DirectoryFailure::StaleCursor,
            _ => return Err(ProtocolError::Malformed),
        },
    )
}

macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::directories::$p as wire;

            pub fn encode_directory(value: &ServerDirectory) -> wire::ServerDirectory {
                wire::ServerDirectory {
                    server: Some(server(value.server())),
                    value: value.bytes().to_vec(),
                }
            }
            pub fn decode_directory(
                value: &wire::ServerDirectory,
                expected: ServerId,
            ) -> Result<ServerDirectory, ProtocolError> {
                if decode_server(value.server.as_ref())? != expected {
                    return Err(ProtocolError::Malformed);
                }
                ServerDirectory::new(expected, value.value.clone())
                    .map_err(|_| ProtocolError::Malformed)
            }
            fn encode_context(value: DirectoryContext) -> wire::DirectoryContext {
                let p = value.parts();
                wire::DirectoryContext {
                    server: Some(server(p.server)),
                    epoch: p.epoch.get(),
                    pairing: Some(entity(p.server, p.pairing.local())),
                    client: Some(entity(p.server, p.client.local())),
                }
            }
            fn decode_context(
                value: Option<&wire::DirectoryContext>,
                expected: DirectoryContext,
            ) -> Result<DirectoryContext, ProtocolError> {
                let value = value.ok_or(ProtocolError::Malformed)?;
                let server = decode_server(value.server.as_ref())?;
                let decoded = DirectoryContext::new(DirectoryContextParts {
                    server,
                    epoch: ServerEpoch::new(nonzero(value.epoch)?.get()),
                    pairing: PairingId::new(server, decode_entity(value.pairing.as_ref(), server)?)
                        .map_err(|_| ProtocolError::Malformed)?,
                    client: ClientId::new(server, decode_entity(value.client.as_ref(), server)?)
                        .map_err(|_| ProtocolError::Malformed)?,
                })
                .map_err(failure)?;
                if decoded != expected {
                    return Err(ProtocolError::PairingDenied);
                }
                Ok(decoded)
            }
            pub fn encode_cursor(value: &DirectoryCursor) -> wire::DirectoryCursor {
                let p = value.parts();
                wire::DirectoryCursor {
                    context: Some(encode_context(p.context)),
                    scope: Some(encode_directory(&p.scope)),
                    observation: p.observation.get(),
                    random: p.random.to_vec(),
                }
            }
            pub fn decode_cursor(
                value: &wire::DirectoryCursor,
                context: DirectoryContext,
            ) -> Result<DirectoryCursor, ProtocolError> {
                DirectoryCursor::new(DirectoryCursorParts {
                    context: decode_context(value.context.as_ref(), context)?,
                    scope: decode_directory(
                        value.scope.as_ref().ok_or(ProtocolError::Malformed)?,
                        context.server(),
                    )?,
                    observation: nonzero(value.observation)?,
                    random: value
                        .random
                        .as_slice()
                        .try_into()
                        .map_err(|_| ProtocolError::Malformed)?,
                })
                .map_err(failure)
            }
            fn encode_purpose(value: DirectoryPurpose) -> i32 {
                (match value {
                    DirectoryPurpose::WorkspacePin => wire::DirectoryPurpose::WorkspacePin,
                    DirectoryPurpose::SessionLaunch => wire::DirectoryPurpose::SessionLaunch,
                    DirectoryPurpose::PaneRepair => wire::DirectoryPurpose::PaneRepair,
                    DirectoryPurpose::WorktreeCreateTarget => {
                        wire::DirectoryPurpose::WorktreeCreateTarget
                    }
                    DirectoryPurpose::WorktreeFallback => wire::DirectoryPurpose::WorktreeFallback,
                }) as i32
            }
            fn decode_purpose(value: i32) -> Result<DirectoryPurpose, ProtocolError> {
                Ok(
                    match wire::DirectoryPurpose::try_from(value)
                        .map_err(|_| ProtocolError::Malformed)?
                    {
                        wire::DirectoryPurpose::WorkspacePin => DirectoryPurpose::WorkspacePin,
                        wire::DirectoryPurpose::SessionLaunch => DirectoryPurpose::SessionLaunch,
                        wire::DirectoryPurpose::PaneRepair => DirectoryPurpose::PaneRepair,
                        wire::DirectoryPurpose::WorktreeCreateTarget => {
                            DirectoryPurpose::WorktreeCreateTarget
                        }
                        wire::DirectoryPurpose::WorktreeFallback => {
                            DirectoryPurpose::WorktreeFallback
                        }
                        _ => return Err(ProtocolError::Malformed),
                    },
                )
            }
            fn encode_class(value: DirectoryValidationClass) -> i32 {
                (match value {
                    DirectoryValidationClass::AvailableDirectory => {
                        wire::DirectoryValidationClass::AvailableDirectory
                    }
                    DirectoryValidationClass::AbsentCreateTarget => {
                        wire::DirectoryValidationClass::AbsentCreateTarget
                    }
                    DirectoryValidationClass::EmptyCreateTarget => {
                        wire::DirectoryValidationClass::EmptyCreateTarget
                    }
                    DirectoryValidationClass::Missing => wire::DirectoryValidationClass::Missing,
                    DirectoryValidationClass::NotDirectory => {
                        wire::DirectoryValidationClass::NotDirectory
                    }
                    DirectoryValidationClass::Unavailable => {
                        wire::DirectoryValidationClass::Unavailable
                    }
                    DirectoryValidationClass::IdentityChanged => {
                        wire::DirectoryValidationClass::IdentityChanged
                    }
                    DirectoryValidationClass::Unchecked => {
                        wire::DirectoryValidationClass::Unchecked
                    }
                }) as i32
            }
            fn decode_class(value: i32) -> Result<DirectoryValidationClass, ProtocolError> {
                Ok(
                    match wire::DirectoryValidationClass::try_from(value)
                        .map_err(|_| ProtocolError::Malformed)?
                    {
                        wire::DirectoryValidationClass::AvailableDirectory => {
                            DirectoryValidationClass::AvailableDirectory
                        }
                        wire::DirectoryValidationClass::AbsentCreateTarget => {
                            DirectoryValidationClass::AbsentCreateTarget
                        }
                        wire::DirectoryValidationClass::EmptyCreateTarget => {
                            DirectoryValidationClass::EmptyCreateTarget
                        }
                        wire::DirectoryValidationClass::Missing => {
                            DirectoryValidationClass::Missing
                        }
                        wire::DirectoryValidationClass::NotDirectory => {
                            DirectoryValidationClass::NotDirectory
                        }
                        wire::DirectoryValidationClass::Unavailable => {
                            DirectoryValidationClass::Unavailable
                        }
                        wire::DirectoryValidationClass::IdentityChanged => {
                            DirectoryValidationClass::IdentityChanged
                        }
                        wire::DirectoryValidationClass::Unchecked => {
                            DirectoryValidationClass::Unchecked
                        }
                        _ => return Err(ProtocolError::Malformed),
                    },
                )
            }
            pub fn encode_query(
                value: &DirectoryRequest,
            ) -> Result<wire::DirectoryQuery, ProtocolError> {
                let p = value.query.parts();
                nonzero(p.context.parts().epoch.get())?;
                let action = match &p.action {
                    DirectoryQueryAction::Home => wire::directory_query::Action::Home(true),
                    DirectoryQueryAction::Browse { directory, cursor } => {
                        wire::directory_query::Action::Browse(wire::BrowseDirectory {
                            directory: Some(encode_directory(directory)),
                            cursor: cursor.as_ref().map(encode_cursor),
                        })
                    }
                    DirectoryQueryAction::Validate { directory, purpose } => {
                        wire::directory_query::Action::Validate(wire::ValidateDirectory {
                            directory: Some(encode_directory(directory)),
                            purpose: encode_purpose(*purpose),
                        })
                    }
                };
                let result = wire::DirectoryQuery {
                    request: value.request.get(),
                    request_generation: value.generation.get(),
                    context: Some(encode_context(p.context)),
                    action: Some(action),
                };
                bounded(&result, crate::limits::MAX_MANAGEMENT_BYTES)?;
                Ok(result)
            }
            pub fn decode_query(
                value: &wire::DirectoryQuery,
                context: DirectoryContext,
            ) -> Result<DirectoryRequest, ProtocolError> {
                bounded(value, crate::limits::MAX_MANAGEMENT_BYTES)?;
                decode_context(value.context.as_ref(), context)?;
                let action = match value.action.as_ref().ok_or(ProtocolError::Malformed)? {
                    wire::directory_query::Action::Home(true) => DirectoryQueryAction::Home,
                    wire::directory_query::Action::Browse(b) => DirectoryQueryAction::Browse {
                        directory: decode_directory(
                            b.directory.as_ref().ok_or(ProtocolError::Malformed)?,
                            context.server(),
                        )?,
                        cursor: b
                            .cursor
                            .as_ref()
                            .map(|c| decode_cursor(c, context))
                            .transpose()?,
                    },
                    wire::directory_query::Action::Validate(v) => DirectoryQueryAction::Validate {
                        directory: decode_directory(
                            v.directory.as_ref().ok_or(ProtocolError::Malformed)?,
                            context.server(),
                        )?,
                        purpose: decode_purpose(v.purpose)?,
                    },
                    _ => return Err(ProtocolError::Malformed),
                };
                DirectoryRequest::new(
                    value.request,
                    value.request_generation,
                    DirectoryQuery::new(DirectoryQueryParts { context, action })
                        .map_err(failure)?,
                )
            }
            pub fn encode_response(
                value: &DirectoryResponse,
                context: DirectoryContext,
            ) -> Result<wire::DirectoryQueryResult, ProtocolError> {
                let result = match &value.result {
                    Ok(DirectoryQueryResult::Home(directory)) => {
                        if directory.server() != context.server() {
                            return Err(ProtocolError::Malformed);
                        }
                        wire::directory_query_result::Result::Home(encode_directory(directory))
                    }
                    Ok(DirectoryQueryResult::Page(page)) => {
                        let p = page.parts();
                        if p.context != context {
                            return Err(ProtocolError::PairingDenied);
                        }
                        wire::directory_query_result::Result::Page(Box::new(wire::DirectoryPage {
                            context: Some(encode_context(p.context)),
                            directory: Some(encode_directory(&p.directory)),
                            observation: p.observation.get(),
                            entries: p
                                .entries
                                .iter()
                                .map(|e| wire::DirectoryEntry {
                                    name: Some(wire::DirectoryEntryName {
                                        value: e.name.bytes().to_vec(),
                                    }),
                                    directory: Some(encode_directory(&e.directory)),
                                    selectable: e.selectable,
                                })
                                .collect(),
                            next: p.next.as_ref().map(encode_cursor),
                        }))
                    }
                    Ok(DirectoryQueryResult::Validation(v)) => {
                        if v.directory.server() != context.server() {
                            return Err(ProtocolError::Malformed);
                        }
                        wire::directory_query_result::Result::Validation(
                            wire::DirectoryValidation {
                                directory: Some(encode_directory(&v.directory)),
                                purpose: encode_purpose(v.purpose),
                                classification: encode_class(v.class),
                            },
                        )
                    }
                    Err(f) => wire::directory_query_result::Result::Failure(encode_failure(*f)),
                };
                let response = wire::DirectoryQueryResult {
                    request: value.request.get(),
                    request_generation: value.generation.get(),
                    result: Some(result),
                };
                bounded(&response, crate::limits::MAX_MANAGEMENT_BYTES)?;
                Ok(response)
            }
            pub fn decode_response(
                value: &wire::DirectoryQueryResult,
                context: DirectoryContext,
            ) -> Result<DirectoryResponse, ProtocolError> {
                bounded(value, crate::limits::MAX_MANAGEMENT_BYTES)?;
                let result = match value.result.as_ref().ok_or(ProtocolError::Malformed)? {
                    wire::directory_query_result::Result::Home(d) => Ok(
                        DirectoryQueryResult::Home(decode_directory(d, context.server())?),
                    ),
                    wire::directory_query_result::Result::Validation(v) => {
                        Ok(DirectoryQueryResult::Validation(DirectoryValidation {
                            directory: decode_directory(
                                v.directory.as_ref().ok_or(ProtocolError::Malformed)?,
                                context.server(),
                            )?,
                            purpose: decode_purpose(v.purpose)?,
                            class: decode_class(v.classification)?,
                        }))
                    }
                    wire::directory_query_result::Result::Page(p) => {
                        if p.entries.len() > MAX_DIRECTORY_PAGE_ENTRIES {
                            return Err(ProtocolError::LimitExceeded);
                        }
                        bounded(p.as_ref(), MAX_DIRECTORY_PAGE_BYTES)?;
                        let page = DirectoryPage::new(DirectoryPageParts {
                            context: decode_context(p.context.as_ref(), context)?,
                            directory: decode_directory(
                                p.directory.as_ref().ok_or(ProtocolError::Malformed)?,
                                context.server(),
                            )?,
                            observation: nonzero(p.observation)?,
                            entries: p
                                .entries
                                .iter()
                                .map(|e| {
                                    Ok(DirectoryEntry {
                                        name: DirectoryEntryName::new(
                                            e.name
                                                .as_ref()
                                                .ok_or(ProtocolError::Malformed)?
                                                .value
                                                .clone(),
                                        )
                                        .map_err(failure)?,
                                        directory: decode_directory(
                                            e.directory.as_ref().ok_or(ProtocolError::Malformed)?,
                                            context.server(),
                                        )?,
                                        selectable: e.selectable,
                                    })
                                })
                                .collect::<Result<_, ProtocolError>>()?,
                            next: p
                                .next
                                .as_ref()
                                .map(|c| decode_cursor(c, context))
                                .transpose()?,
                        })
                        .map_err(failure)?;
                        Ok(DirectoryQueryResult::Page(Box::new(page)))
                    }
                    wire::directory_query_result::Result::Failure(f) => Err(decode_failure(f)?),
                };
                Ok(DirectoryResponse {
                    request: nonzero(value.request)?,
                    generation: nonzero(value.request_generation)?,
                    result,
                })
            }
        }
    };
}
profile!(v1);
profile!(v2);
