use super::{
    budget::{Charge, Residency},
    eligibility::{Eligibility, stale},
};
use crate::failure::{ClientFailure, invalid_peer, resource_limit};
use sha2::{Digest, Sha256};
use spaceterm_core::{
    content::GraphicsResourceKey,
    content_queries::{GraphicsChunk, GraphicsRequest},
};
use std::sync::Arc;

#[derive(Clone)]
pub struct GraphicsAssetLease(Arc<Asset>);
struct Asset {
    request: GraphicsRequest,
    bytes: Vec<u8>,
    eligibility: Eligibility,
    paint: Eligibility,
    _charge: Charge,
}
impl GraphicsAssetLease {
    pub fn key(&self) -> GraphicsResourceKey {
        self.0.request.resource().key
    }
    pub fn is_paintable(&self) -> bool {
        self.0.paint.allows(self.0.request.stamp().fence(), true)
    }
    pub fn rgba(&self) -> Option<&[u8]> {
        self.is_paintable().then_some(self.0.bytes.as_slice())
    }
    pub(crate) fn eligible(&self, eligibility: &Eligibility, paint: &Eligibility) -> bool {
        self.0.eligibility.same(eligibility)
            && self.0.paint.same(paint)
            && self
                .0
                .eligibility
                .allows_presentation(self.0.request.stamp().fence())
    }
}
pub struct GraphicsPreparation {
    request: GraphicsRequest,
    bytes: Vec<u8>,
    eligibility: Eligibility,
    paint: Eligibility,
    charge: Option<Charge>,
    finished: bool,
}
impl GraphicsPreparation {
    pub(crate) fn new(
        request: GraphicsRequest,
        residency: &Arc<Residency>,
        eligibility: Eligibility,
        paint: Eligibility,
    ) -> Result<Self, ClientFailure> {
        // Complete assets, not resumed suffixes, are eligible for rendering.
        if request.offset() != 0 || !eligibility.allows_presentation(request.stamp().fence()) {
            return Err(stale());
        }
        Ok(Self {
            request,
            bytes: vec![],
            eligibility,
            paint,
            charge: Some(residency.reserve(request.resource().key.session.pane.server(), 0)?),
            finished: false,
        })
    }
    pub fn accept(
        &mut self,
        chunk: GraphicsChunk,
    ) -> Result<Option<GraphicsAssetLease>, ClientFailure> {
        let result = self.accept_current(chunk);
        if result.is_err() {
            self.finished = true;
            self.bytes = vec![];
            self.charge = None;
        }
        result
    }
    fn accept_current(
        &mut self,
        chunk: GraphicsChunk,
    ) -> Result<Option<GraphicsAssetLease>, ClientFailure> {
        if self.finished
            || !self
                .eligibility
                .allows_presentation(self.request.stamp().fence())
        {
            return Err(stale());
        }
        if chunk.request() != self.request || chunk.offset() as usize != self.bytes.len() {
            return Err(invalid_peer());
        }
        let desired = self
            .bytes
            .len()
            .checked_add(chunk.bytes().len())
            .ok_or_else(resource_limit)?;
        if desired > self.bytes.capacity() {
            let old = self.bytes.capacity();
            let charge = self.charge.as_mut().ok_or_else(stale)?;
            charge.grow((desired - old) as u64)?;
            self.bytes
                .try_reserve_exact(desired - self.bytes.len())
                .map_err(|_| resource_limit())?;
            if self.bytes.capacity() > desired {
                charge.grow((self.bytes.capacity() - desired) as u64)?;
            }
        }
        self.bytes.extend_from_slice(chunk.bytes());
        if !chunk.complete() {
            return Ok(None);
        }
        if self.bytes.len() != self.request.resource().rgba_bytes as usize
            || <[u8; 32]>::from(Sha256::digest(&self.bytes)) != self.request.resource().digest
        {
            return Err(invalid_peer());
        }
        self.finished = true;
        Ok(Some(GraphicsAssetLease(Arc::new(Asset {
            request: self.request,
            bytes: std::mem::take(&mut self.bytes),
            eligibility: self.eligibility.clone(),
            paint: self.paint.clone(),
            _charge: self.charge.take().ok_or_else(stale)?,
        }))))
    }
}
