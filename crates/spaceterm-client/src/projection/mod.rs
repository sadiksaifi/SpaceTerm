//! Transient personal terminal state. No projection value is journal or view-document data.

pub mod budget;
mod copy;
mod eligibility;
mod find;
mod graphics;
pub use crate::content_cache::{ResidentPage, SemanticFrame};
pub use copy::{CopyPreparation, PreparedCopyLease};
pub(crate) use eligibility::{Eligibility, stale};
pub use find::{FindCursor, FindNavigation, FindProjection, FindWindow};
pub use graphics::{GraphicsAssetLease, GraphicsPreparation};

use crate::{
    content_cache::ContentCache,
    failure::{ClientFailure, exhausted, invalid_peer},
};
use budget::Residency;
use spaceterm_core::{
    authority::EffectFence,
    content::{ContentBase, ContentPage, RowId},
    content_queries::{ContentSelection, CopyLogicalLimits, CopyRequest, QueryStamp},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone)]
pub struct ProjectionBudgets {
    pub semantic: Arc<Residency>,
    pub prepared_copy: Arc<Residency>,
    pub graphics: Arc<Residency>,
}
impl ProjectionBudgets {
    fn copy_maximum() -> Result<u64, ClientFailure> {
        let limits = CopyLogicalLimits::supported().map_err(|_| exhausted())?;
        limits
            .plain()
            .checked_add(limits.html())
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(exhausted)
    }
    pub fn supported() -> Result<Self, ClientFailure> {
        let maximum = Self::copy_maximum()?;
        Ok(Self {
            semantic: Residency::new(16 * 1024 * 1024, 64 * 1024 * 1024)?,
            prepared_copy: Residency::new(maximum, maximum)?,
            graphics: Residency::new(
                spaceterm_protocol::limits::MAX_DECODED_GRAPHICS_BYTES as u64,
                spaceterm_protocol::limits::MAX_DECODED_GRAPHICS_BYTES as u64,
            )?,
        })
    }
    pub fn validate(&self) -> Result<(), ClientFailure> {
        let maximum = Self::copy_maximum()?;
        if Arc::ptr_eq(&self.semantic, &self.prepared_copy)
            || Arc::ptr_eq(&self.semantic, &self.graphics)
            || Arc::ptr_eq(&self.prepared_copy, &self.graphics)
            || !self.semantic.within(16 * 1024 * 1024, 64 * 1024 * 1024)
            || !self.prepared_copy.within(maximum, maximum)
            || !self.graphics.within(
                spaceterm_protocol::limits::MAX_DECODED_GRAPHICS_BYTES as u64,
                spaceterm_protocol::limits::MAX_DECODED_GRAPHICS_BYTES as u64,
            )
        {
            return Err(crate::failure::resource_limit());
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct FrameLease {
    frame: Arc<SemanticFrame>,
    fence: EffectFence,
    eligibility: Eligibility,
    paint: Eligibility,
}
impl FrameLease {
    pub fn is_paintable(&self) -> bool {
        self.paint.allows(self.fence, true)
    }
    pub fn permits_native_content_use(&self) -> bool {
        self.paint.allows(self.fence, false) && self.eligibility.allows(self.fence, false)
    }
    pub fn frame(&self) -> Option<&SemanticFrame> {
        self.is_paintable().then_some(&self.frame)
    }
    pub fn frozen(&self) -> bool {
        self.is_paintable() && self.paint.frozen()
    }
    /// Offline adapters may describe the displayed screen, but may not fetch or act.
    pub fn accessibility(&self) -> Option<AccessibilityFacts<'_>> {
        self.frame().map(|frame| AccessibilityFacts {
            frame,
            frozen: self.frozen(),
            live_actions: self.permits_native_content_use(),
        })
    }
}
pub struct AccessibilityFacts<'a> {
    pub frame: &'a SemanticFrame,
    pub frozen: bool,
    pub live_actions: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Viewport {
    pub anchor: Option<RowId>,
    pub rows: u16,
}

pub struct TerminalProjection {
    cache: ContentCache,
    eligibility: Eligibility,
    paint: Eligibility,
    frame: Option<FrameLease>,
    page_request: Option<QueryStamp>,
    selection: Option<ContentSelection>,
    viewport: Viewport,
    find: FindProjection,
    prepared: Option<PreparedCopyLease>,
    copy_generation: Arc<AtomicU64>,
    copy_budget: Arc<Residency>,
    graphics_budget: Arc<Residency>,
    graphics: BTreeMap<spaceterm_core::content::GraphicsResourceKey, GraphicsAssetLease>,
}
impl TerminalProjection {
    pub fn new(budgets: ProjectionBudgets) -> Result<Self, ClientFailure> {
        budgets.validate()?;
        let eligibility = Eligibility::default();
        let find = FindProjection::new(Arc::clone(&budgets.semantic), eligibility.clone());
        Ok(Self {
            cache: ContentCache::new(budgets.semantic),
            eligibility,
            paint: Eligibility::default(),
            frame: None,
            page_request: None,
            selection: None,
            viewport: Viewport {
                anchor: None,
                rows: 1,
            },
            find,
            prepared: None,
            copy_generation: Arc::new(AtomicU64::new(1)),
            copy_budget: budgets.prepared_copy,
            graphics_budget: budgets.graphics,
            graphics: BTreeMap::new(),
        })
    }
    pub(crate) fn authority(
        &mut self,
        fence: EffectFence,
        base: Option<ContentBase>,
        presentation: bool,
        native: bool,
    ) -> Result<(), ClientFailure> {
        if self.eligibility.fence() != Some(fence) {
            self.clear_transient()?;
            self.cache.clear();
        }
        if self.frame.as_ref().is_some_and(|frame| {
            !frame.is_paintable()
                || frame.fence.session() != fence.session()
                || frame.fence.grant() != fence.grant()
        }) {
            self.paint.revoke();
            self.frame = None;
            self.graphics.clear();
        }
        if let Some(frame) = &mut self.frame {
            if base == Some(frame.frame.base()) {
                if frame.fence != fence {
                    self.graphics.clear();
                }
                frame.fence = fence;
                self.paint.online(fence, presentation, native);
            } else {
                // A newer base revokes content actions, not the displayed screen.
                // Its paint-only lease survives until complete replacement or revocation.
                self.paint.disable_native();
            }
        }
        self.eligibility.online(fence, presentation, native);
        Ok(())
    }
    pub(crate) fn freeze(&mut self) -> Result<(), ClientFailure> {
        self.eligibility.freeze();
        self.paint.freeze();
        self.cache.clear();
        self.clear_transient()
    }
    pub(crate) fn revoke(&mut self) -> Result<(), ClientFailure> {
        self.eligibility.revoke();
        self.paint.revoke();
        self.frame = None;
        self.graphics.clear();
        self.cache.clear();
        self.viewport.anchor = None;
        self.clear_transient()
    }
    fn clear_transient(&mut self) -> Result<(), ClientFailure> {
        self.page_request = None;
        self.selection = None;
        self.find.clear()?;
        self.invalidate_copy()
    }
    fn invalidate_copy(&mut self) -> Result<(), ClientFailure> {
        self.prepared = None;
        self.copy_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| exhausted())?;
        Ok(())
    }
    pub(crate) fn bind_admission(&self, gate: Arc<dyn Fn() -> bool + Send + Sync>) {
        self.eligibility.bind_admission(Arc::clone(&gate));
        self.paint.bind_admission(gate);
    }
    pub fn current_frame(&self) -> Option<FrameLease> {
        self.frame
            .as_ref()
            .filter(|lease| lease.is_paintable())
            .cloned()
    }
    pub fn needs_content_resync(&self) -> bool {
        self.cache.needs_resync()
    }
    /// Read admission revokes older query completions before awaiting the worker.
    pub(crate) fn expect_page(&mut self, stamp: QueryStamp) -> Result<(), ClientFailure> {
        if !self.eligibility.allows_presentation(stamp.fence()) {
            return Err(stale());
        }
        if self.page_request == Some(stamp) {
            return Ok(());
        }
        if self.page_request.is_some_and(|old| {
            stamp.request() <= old.request() || stamp.generation() < old.generation()
        }) {
            return Err(stale());
        }
        self.cache.clear();
        self.page_request = Some(stamp);
        Ok(())
    }
    /// All pages of one projected frame retain its original query stamp.
    pub fn accept_page(
        &mut self,
        stamp: QueryStamp,
        page: ContentPage,
    ) -> Result<bool, ClientFailure> {
        if !self.eligibility.allows_presentation(stamp.fence()) || stamp.base() != page.parts().base
        {
            return Err(stale());
        }
        if self.page_request.is_some_and(|old| {
            stamp.request() < old.request() || stamp.generation() < old.generation()
        }) {
            return Err(stale());
        }
        if self.page_request != Some(stamp) && !self.cache.is_staging() {
            self.expect_page(stamp)?;
        }
        if let Some(frame) = self.cache.push(stamp, page)? {
            self.paint.revoke();
            self.graphics.clear();
            self.paint = self.eligibility.fresh()?;
            self.paint.online(
                stamp.fence(),
                true,
                self.eligibility.allows(stamp.fence(), false),
            );
            self.frame = Some(FrameLease {
                frame,
                fence: stamp.fence(),
                eligibility: self.eligibility.clone(),
                paint: self.paint.clone(),
            });
            return Ok(true);
        }
        Ok(false)
    }
    pub fn selection(&self) -> Option<ContentSelection> {
        self.eligibility.current().ok().and(self.selection)
    }
    pub fn set_selection(
        &mut self,
        selection: Option<ContentSelection>,
    ) -> Result<(), ClientFailure> {
        let current = self.eligibility.current()?;
        if selection.is_some_and(|value| {
            value.start().session != current.session()
                || value.end().session != current.session()
                || self.frame.as_ref().is_none_or(|frame| {
                    !frame.frame.base().contains(value.start().row)
                        || !frame.frame.base().contains(value.end().row)
                })
        }) {
            return Err(invalid_peer());
        }
        if selection != self.selection {
            self.invalidate_copy()?;
            self.selection = selection;
        }
        Ok(())
    }
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }
    /// Scroll is local stable-row navigation, never a terminal runtime command.
    pub fn set_viewport(&mut self, viewport: Viewport) -> Result<(), ClientFailure> {
        if viewport.rows == 0
            || viewport.anchor.is_some_and(|row| {
                self.frame
                    .as_ref()
                    .is_none_or(|frame| !frame.frame.base().contains(row))
            })
        {
            return Err(invalid_peer());
        }
        self.viewport = viewport;
        Ok(())
    }
    pub fn find(&self) -> &FindProjection {
        &self.find
    }
    pub fn find_mut(&mut self) -> Result<&mut FindProjection, ClientFailure> {
        self.eligibility.current()?;
        Ok(&mut self.find)
    }
    pub fn begin_copy(&mut self, request: CopyRequest) -> Result<CopyPreparation, ClientFailure> {
        if self.selection != Some(request.selection()) {
            return Err(stale());
        }
        self.invalidate_copy()?;
        CopyPreparation::new(
            request,
            &self.copy_budget,
            self.eligibility.clone(),
            Arc::clone(&self.copy_generation),
        )
    }
    pub fn install_copy(&mut self, lease: PreparedCopyLease) -> Result<(), ClientFailure> {
        if !lease.is_current()
            || !lease.belongs_to(&self.copy_generation)
            || self.selection != Some(lease.request().selection())
        {
            return Err(stale());
        }
        self.prepared = Some(lease);
        Ok(())
    }
    pub fn prepared_copy(&self) -> Option<PreparedCopyLease> {
        self.prepared
            .as_ref()
            .filter(|lease| lease.is_current())
            .cloned()
    }
    pub fn begin_graphics(
        &self,
        request: spaceterm_core::content_queries::GraphicsRequest,
    ) -> Result<GraphicsPreparation, ClientFailure> {
        if self.frame.as_ref().is_none_or(|frame| {
            !frame
                .frame
                .pages()
                .iter()
                .any(|page| page.page().parts().resources.contains(&request.resource()))
        }) {
            return Err(stale());
        }
        GraphicsPreparation::new(
            request,
            &self.graphics_budget,
            self.eligibility.clone(),
            self.paint.clone(),
        )
    }
    pub fn install_graphics(&mut self, lease: GraphicsAssetLease) -> Result<(), ClientFailure> {
        if !lease.eligible(&self.eligibility, &self.paint) {
            return Err(stale());
        }
        self.graphics.insert(lease.key(), lease);
        Ok(())
    }
    pub fn graphics(
        &self,
        key: spaceterm_core::content::GraphicsResourceKey,
    ) -> Option<GraphicsAssetLease> {
        self.graphics
            .get(&key)
            .filter(|lease| lease.is_paintable())
            .cloned()
    }
    pub fn content_base(&self) -> Option<ContentBase> {
        self.current_frame().map(|frame| frame.frame.base())
    }
}
