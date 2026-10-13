//! Semantic pages retain one exact base and allocation charges through frame leases.

use crate::{
    failure::{ClientFailure, invalid_peer, resource_limit},
    projection::budget::{Charge, Residency},
};
use spaceterm_core::{
    content::{ContentBase, ContentPage, PagePosition, RowId},
    content_queries::QueryStamp,
};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug)]
pub struct ResidentPage {
    page: ContentPage,
    _charge: Charge,
}
impl ResidentPage {
    pub fn page(&self) -> &ContentPage {
        &self.page
    }
}

#[derive(Debug)]
pub struct SemanticFrame {
    base: ContentBase,
    pages: Vec<Arc<ResidentPage>>,
}
impl SemanticFrame {
    pub fn base(&self) -> ContentBase {
        self.base
    }
    pub fn pages(&self) -> &[Arc<ResidentPage>] {
        &self.pages
    }
}

pub(crate) struct ContentCache {
    residency: Arc<Residency>,
    pages: Vec<Arc<ResidentPage>>,
    rows: BTreeSet<RowId>,
    base: Option<ContentBase>,
    stamp: Option<QueryStamp>,
    resync: bool,
}
impl ContentCache {
    pub(crate) fn new(residency: Arc<Residency>) -> Self {
        Self {
            residency,
            pages: vec![],
            rows: BTreeSet::new(),
            base: None,
            stamp: None,
            resync: false,
        }
    }
    pub(crate) fn clear(&mut self) {
        self.pages.clear();
        self.rows.clear();
        self.base = None;
        self.stamp = None;
        self.resync = false;
    }
    pub(crate) fn is_staging(&self) -> bool {
        self.stamp.is_some()
    }
    pub(crate) fn needs_resync(&self) -> bool {
        self.resync
    }
    pub(crate) fn push(
        &mut self,
        stamp: QueryStamp,
        page: ContentPage,
    ) -> Result<Option<Arc<SemanticFrame>>, ClientFailure> {
        if self.resync {
            return Err(invalid_peer());
        }
        let parts = page.parts();
        let base = parts.base;
        let valid = self.stamp.is_none_or(|old| old == stamp)
            && self.base.is_none_or(|old| old == base)
            && (!self.pages.is_empty()
                || parts.rows.first().is_none_or(|row| row.start_column() == 0))
            && self.pages.last().is_none_or(|previous| {
                let old = previous.page.parts();
                old.columns == parts.columns
                    && old.cursor == parts.cursor
                    && old.modes == parts.modes
                    && old.colors == parts.colors
                    && old.continuation.is_some_and(|continuation| {
                        continuation.matches(base)
                            && match continuation.position {
                                PagePosition::WithinRow { .. } => parts
                                    .rows
                                    .first()
                                    .is_some_and(|row| continuation.continues_row(base, row)),
                                PagePosition::AfterRow { row, .. } => {
                                    parts.rows.first().is_some_and(|next| {
                                        next.id() != row && next.start_column() == 0
                                    })
                                }
                            }
                    })
            });
        if !valid {
            self.clear();
            self.resync = true;
            return Err(invalid_peer());
        }
        for row in &parts.rows {
            if self.rows.contains(&row.id()) && row.start_column() == 0 {
                self.clear();
                self.resync = true;
                return Err(invalid_peer());
            }
        }
        let charge = self
            .residency
            .reserve(base.session.pane.server(), page.accounted_bytes() as u64)?;
        self.pages.try_reserve(1).map_err(|_| resource_limit())?;
        self.rows.extend(parts.rows.iter().map(|row| row.id()));
        let complete = parts.continuation.is_none();
        self.base = Some(base);
        self.stamp = Some(stamp);
        self.pages.push(Arc::new(ResidentPage {
            page,
            _charge: charge,
        }));
        if !complete {
            return Ok(None);
        }
        let pages = std::mem::take(&mut self.pages);
        self.rows.clear();
        self.base = None;
        self.stamp = None;
        Ok(Some(Arc::new(SemanticFrame { base, pages })))
    }
}
