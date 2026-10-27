//! Existing Worktree mutation, original-outcome and removal-review helpers for native checks.
use super::repository::inventory;
use spaceterm_server::{
    ServerConnection, ServerError,
    connection::link::ServerLink,
    hierarchy::{Mutation, RetainedOutcome},
    worktree::{Operation, Read, ReadReply, read_worktree},
};
use std::{error::Error, path::Path};

/// The settled outcome of the edit `sequence`. The server replies once the write settles.
pub fn finish(link: &ServerLink, sequence: u64) -> Result<RetainedOutcome, Box<dyn Error>> {
    Ok(super::support::resolve(link, sequence)?)
}
pub fn write(link: &ServerLink, operation: Operation) -> Result<RetainedOutcome, Box<dyn Error>> {
    let sequence = inventory(link)?.1;
    let outcome = super::support::mutate(link, sequence, Mutation::Worktree(operation))?;
    link.acknowledge(sequence).map_err(ServerError::from)?;
    Ok(outcome)
}
pub fn review(link: &ServerLink, root: &Path, path: &Path) -> Result<Operation, Box<dyn Error>> {
    let review = match read_worktree(
        link,
        Read::CheckRemoval {
            root: root.into(),
            path: path.into(),
        },
    )? {
        Ok(ReadReply::Removal(review)) => review,
        Err(error) => {
            return Err(format!("removal review failed: {error:?}").into());
        }
        _ => return Err("removal review response".into()),
    };
    Ok(Operation::Remove {
        root: root.into(),
        path: path.into(),
        boot: review.boot,
        confirmation: review.confirmation,
    })
}

/// Keeps an uncertain original in this client's existing journal until its exact outcome settles.
pub fn journaled_mutation(
    server: &ServerConnection,
    link: &ServerLink,
    mutation: Mutation,
) -> Result<RetainedOutcome, Box<dyn Error>> {
    let mut journal = server.shared_journal()?;
    if journal.pending().is_some() {
        return Err("fixture client has an unresolved original; refusing new work".into());
    }
    let sequence = inventory(link)?.1;
    journal.submit(sequence, mutation.clone())?;
    let result = super::support::mutate(link, sequence, mutation.clone())?;
    if result.sequence != sequence || result.mutation != mutation {
        return Err("fixture original identity changed".into());
    }
    journal.retire(result.clone())?;
    link.acknowledge(sequence).map_err(ServerError::from)?;
    Ok(result)
}
