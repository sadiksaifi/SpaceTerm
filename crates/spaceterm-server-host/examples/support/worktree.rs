//! Existing Worktree mutation, original-outcome and removal-review helpers for native checks.
use super::repository::inventory;
use spaceterm_server::{
    LocalServer,
    hierarchy::{Mutation, RetainedOutcome},
    pane::{PaneConnection, Reply, Request},
    worktree::{Operation, Read, ReadReply},
};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};

pub fn finish(c: &mut PaneConnection, sequence: u64) -> Result<RetainedOutcome, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(25);
    loop {
        match c.exchange(&Request::Resolve { sequence })? {
            Reply::Outcome(outcome) => return Ok(outcome),
            Reply::MutationPending => {}
            _ => return Err("original write outcome unavailable".into()),
        }
        if Instant::now() > until {
            return Err("write did not settle".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
pub fn write(
    c: &mut PaneConnection,
    operation: Operation,
) -> Result<RetainedOutcome, Box<dyn Error>> {
    let sequence = inventory(c)?.1;
    let reply = c.exchange(&Request::Mutate {
        sequence,
        mutation: Mutation::Worktree(operation),
    })?;
    let outcome = match reply {
        Reply::Outcome(o) => o,
        Reply::MutationPending => finish(c, sequence)?,
        _ => return Err("write response".into()),
    };
    c.exchange(&Request::Acknowledge { sequence })?;
    Ok(outcome)
}
pub fn review(
    c: &mut PaneConnection,
    root: &Path,
    path: &Path,
) -> Result<Operation, Box<dyn Error>> {
    let review = match c.read_worktree(Read::CheckRemoval {
        root: root.into(),
        path: path.into(),
    })? {
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
    server: &LocalServer,
    connection: &mut PaneConnection,
    mutation: Mutation,
) -> Result<RetainedOutcome, Box<dyn Error>> {
    let mut journal = server.shared_journal()?;
    if journal.pending().is_some() {
        return Err("fixture client has an unresolved original; refusing new work".into());
    }
    let sequence = inventory(connection)?.1;
    journal.submit(sequence, mutation.clone())?;
    let result = match connection.exchange(&Request::Mutate {
        sequence,
        mutation: mutation.clone(),
    })? {
        Reply::Outcome(outcome) => outcome,
        Reply::MutationPending => finish(connection, sequence)?,
        _ => return Err("fixture mutation outcome unavailable".into()),
    };
    if result.sequence != sequence || result.mutation != mutation {
        return Err("fixture original identity changed".into());
    }
    journal.complete(result.clone())?;
    connection.exchange(&Request::Acknowledge { sequence })?;
    journal.retire()?;
    Ok(result)
}
