//! Owning-server reads. No operation here grants local file authority or Pane control.
use crate::{
    contract::*,
    list::PaneFacts,
    scope::{Resolved, Snapshot},
};
use spaceterm_server::{
    connection::link::ServerLink,
    hierarchy::{Id, Inventory},
    pane::directories,
    protocol::{Operation, ReplyBody},
    worktree::{Read as WorktreeRead, ReadReply},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub trait Read {
    fn inventory(&mut self) -> Result<Inventory, Failure>;
    fn home(&mut self) -> Result<PathBuf, Failure>;
    fn worktree(&mut self, read: WorktreeRead) -> Result<ReadReply, Failure>;
    fn panes(&mut self, snapshot: &Snapshot, target: &Resolved) -> Result<Vec<PaneFacts>, Failure>;
}

pub struct Connected {
    pub link: ServerLink,
    pub actor: Id,
    pub deadline: Instant,
}

impl Connected {
    fn request(&self, operation: Operation) -> Result<ReplyBody, Failure> {
        let request = self
            .link
            .request(operation)
            .map_err(|e| from_server_error(e.into()))?;
        match request.wait_for(self.deadline.saturating_duration_since(Instant::now())) {
            Some(reply) => reply.map_err(|e| from_server_error(e.into())),
            None => {
                request.cancel();
                Err(Failure::new(Code::Timeout))
            }
        }
    }
}

impl Read for Connected {
    fn inventory(&mut self) -> Result<Inventory, Failure> {
        loop {
            if let Some((_, revision)) = self.link.inventory() {
                return Ok(revision.inventory.clone());
            }
            if let spaceterm_server::connection::link::Health::Closed(error) = self.link.health() {
                return Err(from_server_error(error.into()));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Failure::new(Code::Timeout));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
        }
    }

    fn home(&mut self) -> Result<PathBuf, Failure> {
        match self.request(Operation::DirectoryRead(directories::Operation::Home))? {
            ReplyBody::Directory(Ok(directories::Value::Directory(path))) => Ok(path),
            ReplyBody::Directory(Err(_)) => Err(Failure::new(Code::DirectoryUnavailable)),
            _ => Err(Failure::new(Code::ProtocolError)),
        }
    }

    fn worktree(&mut self, read: WorktreeRead) -> Result<ReadReply, Failure> {
        match self.request(Operation::WorktreeRead(read))? {
            ReplyBody::Worktree(Ok(reply)) => Ok(reply),
            ReplyBody::Worktree(Err(error)) => Err(from_worktree_error(error)),
            _ => Err(Failure::new(Code::ProtocolError)),
        }
    }

    fn panes(&mut self, snapshot: &Snapshot, target: &Resolved) -> Result<Vec<PaneFacts>, Failure> {
        crate::list::collect(snapshot, target, self.actor, &self.link, self.deadline)
    }
}
