//! Attachment identifiers carried in Work requests.
use crate::hierarchy::Id;
use serde::{Deserialize, Serialize};

/// One attachment lifetime. Reattachment always receives a higher generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub workspace: Id,
    pub generation: u64,
}
