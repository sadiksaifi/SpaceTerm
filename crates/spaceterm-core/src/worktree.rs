pub use spaceterm_protocol::worktree::*;

/// One bounded read on `link`. Its reply arrives when the read settles; one that outlasts the
/// limit is cancelled. Uncertain requests are never retried.
#[cfg(feature = "client")]
pub fn read_worktree(
    link: &crate::connection::link::CoreLink,
    read: Read,
) -> Result<Result<ReadReply, Error>, crate::CoreError> {
    let pending = link.request(crate::protocol::Operation::WorktreeRead(read))?;
    match pending.wait_for(READ_LIMIT) {
        Some(Ok(crate::protocol::ReplyBody::Worktree(result))) => Ok(result),
        Some(Ok(_)) => Err(crate::CoreError::Protocol),
        Some(Err(error)) => Err(error.into()),
        None => {
            pending.cancel();
            Ok(Err(Error::Unavailable))
        }
    }
}
/// How long a Worktree read may run before the client abandons it.
#[cfg(feature = "client")]
const READ_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
