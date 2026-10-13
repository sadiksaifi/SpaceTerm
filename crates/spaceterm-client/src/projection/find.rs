use super::{
    budget::{Charge, Residency},
    eligibility::Eligibility,
    stale,
};
use crate::failure::{ClientFailure, exhausted, invalid_peer, resource_limit};
use spaceterm_core::content_queries::*;
use std::sync::Arc;

/// Only one bounded result window is resident. Navigation outside it asks the
/// adapter to resume or rescan to that chunk, rather than capping total matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindNavigation {
    Match(FindMatch),
    LoadChunk { index: u64 },
    NoMatches,
}
/// Cursor coordinates remain exact even when chunks contain different match counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FindCursor {
    pub chunk: u64,
    pub index: usize,
}
/// Borrowed presentation of the one resident, protocol-bounded match chunk.
pub struct FindWindow<'a> {
    pub chunk: u64,
    pub matches: &'a [FindMatch],
    pub cursor: Option<usize>,
}
pub struct FindProjection {
    generation: u64,
    request: Option<FindRequest>,
    window: Option<Box<FindChunk>>,
    cursor: Option<usize>,
    next_chunk: u64,
    complete: bool,
    total_matches: u64,
    eligibility: Eligibility,
    residency: Arc<Residency>,
    query_charge: Option<Charge>,
    window_charge: Option<Charge>,
}
impl FindProjection {
    pub(crate) fn new(residency: Arc<Residency>, eligibility: Eligibility) -> Self {
        Self {
            generation: 0,
            request: None,
            window: None,
            cursor: None,
            next_chunk: 0,
            complete: false,
            total_matches: 0,
            eligibility,
            residency,
            query_charge: None,
            window_charge: None,
        }
    }
    /// Clear before changing the query, then use generation() in the new request.
    pub fn clear(&mut self) -> Result<(), ClientFailure> {
        self.generation = self.generation.checked_add(1).ok_or_else(exhausted)?;
        self.request = None;
        self.window = None;
        self.query_charge = None;
        self.window_charge = None;
        self.cursor = None;
        self.next_chunk = 0;
        self.complete = false;
        self.total_matches = 0;
        Ok(())
    }
    pub fn generation(&self) -> RequestGeneration {
        RequestGeneration::new(self.generation)
    }
    pub fn begin(&mut self, request: FindRequest) -> Result<(), ClientFailure> {
        if request.stamp().generation() != self.generation()
            || self.request.is_some()
            || !self.eligibility.allows(request.stamp().fence(), false)
        {
            return Err(stale());
        }
        let bytes = request
            .query()
            .as_str()
            .len()
            .checked_add(4096)
            .ok_or_else(resource_limit)?;
        self.query_charge = Some(
            self.residency
                .reserve(request.stamp().base().session.pane.server(), bytes as u64)?,
        );
        self.request = Some(request);
        self.window = None;
        self.cursor = None;
        self.next_chunk = 0;
        self.complete = false;
        self.total_matches = 0;
        Ok(())
    }
    pub fn request(&self) -> Option<&FindRequest> {
        self.request
            .as_ref()
            .filter(|request| self.eligibility.allows(request.stamp().fence(), false))
    }
    /// Count received so far; final only when is_complete() is true.
    pub fn total_matches(&self) -> u64 {
        if self.request().is_some() {
            self.total_matches
        } else {
            0
        }
    }
    pub fn is_complete(&self) -> bool {
        self.request().is_some() && self.complete
    }
    pub fn current_window(&self) -> Option<FindWindow<'_>> {
        self.request()?;
        let chunk = self.window.as_ref()?;
        Some(FindWindow {
            chunk: chunk.index(),
            matches: chunk.matches(),
            cursor: self.cursor,
        })
    }
    pub fn current_cursor(&self) -> Option<FindCursor> {
        let window = self.current_window()?;
        Some(FindCursor {
            chunk: window.chunk,
            index: window.cursor?,
        })
    }
    pub fn accept(&mut self, item: FindStreamItem) -> Result<(), ClientFailure> {
        let stamp = match &item {
            FindStreamItem::Chunk(chunk) => chunk.stamp(),
            FindStreamItem::Complete { stamp, .. } | FindStreamItem::Failed { stamp, .. } => *stamp,
        };
        if self
            .request()
            .is_none_or(|request| request.stamp() != stamp)
        {
            return Err(stale());
        }
        let result = self.accept_current(item);
        if result.is_err() {
            self.clear()?;
        }
        result
    }
    fn accept_current(&mut self, item: FindStreamItem) -> Result<(), ClientFailure> {
        match item {
            FindStreamItem::Chunk(chunk) => {
                if self.complete || chunk.index() != self.next_chunk {
                    return Err(invalid_peer());
                }
                self.total_matches = self
                    .total_matches
                    .checked_add(chunk.matches().len() as u64)
                    .ok_or_else(exhausted)?;
                // The initial window stays visible while the scan progresses. A
                // requested navigation window can be installed with load_window.
                if self.window.is_none() {
                    self.install_window(chunk)?;
                }
                self.next_chunk = self.next_chunk.checked_add(1).ok_or_else(exhausted)?;
                Ok(())
            }
            FindStreamItem::Complete { chunks, .. } => {
                if self.complete || chunks != self.next_chunk {
                    return Err(invalid_peer());
                }
                self.complete = true;
                Ok(())
            }
            FindStreamItem::Failed { .. } => Err(stale()),
        }
    }
    fn install_window(&mut self, chunk: Box<FindChunk>) -> Result<(), ClientFailure> {
        let bytes = chunk
            .matches()
            .len()
            .checked_mul(128)
            .and_then(|bytes| bytes.checked_add(4096))
            .ok_or_else(resource_limit)?;
        let charge = self
            .residency
            .reserve(chunk.stamp().base().session.pane.server(), bytes as u64)?;
        self.window = Some(chunk);
        self.window_charge = Some(charge);
        self.cursor = None;
        Ok(())
    }
    /// The adapter rescans server-owned content when navigating backwards beyond
    /// the bounded window. No local content matcher or unbounded cursor list exists.
    pub fn load_window(&mut self, chunk: Box<FindChunk>) -> Result<(), ClientFailure> {
        if self
            .request()
            .is_none_or(|request| request.stamp() != chunk.stamp())
            || chunk.index() >= self.next_chunk
        {
            return Err(stale());
        }
        self.install_window(chunk)
    }
    pub fn navigate(&mut self, forward: bool) -> FindNavigation {
        if self.request().is_none() {
            return FindNavigation::NoMatches;
        }
        let Some(chunk) = &self.window else {
            return FindNavigation::NoMatches;
        };
        let count = chunk.matches().len();
        let next = match (self.cursor, forward) {
            (None, true) => Some(0),
            (None, false) => count.checked_sub(1),
            (Some(index), true) => index.checked_add(1),
            (Some(index), false) => index.checked_sub(1),
        };
        if let Some(index) = next
            && let Some(value) = chunk.matches().get(index)
        {
            self.cursor = Some(index);
            return FindNavigation::Match(*value);
        }
        let index = if forward {
            match chunk.index().checked_add(1) {
                Some(next) if next < self.next_chunk || !self.complete => next,
                _ => 0,
            }
        } else if chunk.index() != 0 {
            chunk.index() - 1
        } else if self.complete {
            self.next_chunk.saturating_sub(1)
        } else {
            return FindNavigation::NoMatches;
        };
        if index == chunk.index() {
            if count == 0 {
                return FindNavigation::NoMatches;
            }
            let cursor = if forward { 0 } else { count - 1 };
            self.cursor = Some(cursor);
            FindNavigation::Match(chunk.matches()[cursor])
        } else {
            FindNavigation::LoadChunk { index }
        }
    }
}
