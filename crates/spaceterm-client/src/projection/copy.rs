use super::{
    budget::{Charge, Residency},
    eligibility::{Eligibility, stale},
};
use crate::failure::{ClientFailure, invalid_peer, resource_limit};
use sha2::{Digest, Sha256};
use spaceterm_core::content_queries::{CopyPart, CopyRequest, CopyStreamItem, ResourceTransferId};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// A complete result has separate residency and revocable Copy/Services eligibility.
#[derive(Clone)]
pub struct PreparedCopyLease(Arc<PreparedCopy>);
struct PreparedCopy {
    request: CopyRequest,
    plain: String,
    html: Option<String>,
    eligibility: Eligibility,
    generation: Arc<AtomicU64>,
    captured: u64,
    _charge: Charge,
}
impl PreparedCopyLease {
    pub fn is_current(&self) -> bool {
        self.0.generation.load(Ordering::Acquire) == self.0.captured
            && self
                .0
                .eligibility
                .allows(self.0.request.stamp().fence(), false)
    }
    pub fn plain(&self) -> Option<&str> {
        self.is_current().then_some(self.0.plain.as_str())
    }
    pub fn html(&self) -> Option<&str> {
        if self.is_current() {
            self.0.html.as_deref()
        } else {
            None
        }
    }
    pub fn request(&self) -> CopyRequest {
        self.0.request
    }
    pub(crate) fn belongs_to(&self, generation: &Arc<AtomicU64>) -> bool {
        Arc::ptr_eq(&self.0.generation, generation)
    }
}
impl std::fmt::Debug for PreparedCopyLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedCopyLease(..)")
    }
}

pub struct CopyPreparation {
    request: CopyRequest,
    transfer: Option<ResourceTransferId>,
    plain: Vec<u8>,
    html: Vec<u8>,
    totals: [Option<u64>; 2],
    charge: Option<Charge>,
    eligibility: Eligibility,
    generation: Arc<AtomicU64>,
    captured: u64,
    finished: bool,
}
impl CopyPreparation {
    pub(crate) fn new(
        request: CopyRequest,
        residency: &Arc<Residency>,
        eligibility: Eligibility,
        generation: Arc<AtomicU64>,
    ) -> Result<Self, ClientFailure> {
        if !eligibility.allows(request.stamp().fence(), false) {
            return Err(stale());
        }
        Ok(Self {
            request,
            transfer: None,
            plain: vec![],
            html: vec![],
            totals: [None, None],
            charge: Some(residency.reserve(request.stamp().base().session.pane.server(), 0)?),
            eligibility,
            captured: generation.load(Ordering::Acquire),
            generation,
            finished: false,
        })
    }
    pub fn request(&self) -> CopyRequest {
        self.request
    }
    pub fn accept(
        &mut self,
        item: CopyStreamItem,
    ) -> Result<Option<PreparedCopyLease>, ClientFailure> {
        let result = self.accept_current(item);
        if result.is_err() {
            self.finished = true;
            self.plain = vec![];
            self.html = vec![];
            self.charge = None;
        }
        result
    }
    fn accept_current(
        &mut self,
        item: CopyStreamItem,
    ) -> Result<Option<PreparedCopyLease>, ClientFailure> {
        if self.finished
            || self.captured != self.generation.load(Ordering::Acquire)
            || !self.eligibility.allows(self.request.stamp().fence(), false)
        {
            return Err(stale());
        }
        match item {
            CopyStreamItem::Failed { .. } => Err(stale()),
            CopyStreamItem::Chunk(chunk) => {
                if chunk.stamp() != self.request.stamp()
                    || self.transfer.is_some_and(|value| value != chunk.transfer())
                {
                    return Err(invalid_peer());
                }
                let (index, buffer) = match chunk.part() {
                    CopyPart::Plain => (0, &mut self.plain),
                    CopyPart::Html => (1, &mut self.html),
                };
                if (index == 1 && !self.request.options().include_html)
                    || chunk.offset() != buffer.len() as u64
                    || self.totals[index].is_some_and(|total| total != chunk.total())
                {
                    return Err(invalid_peer());
                }
                let desired = buffer
                    .len()
                    .checked_add(chunk.bytes().len())
                    .ok_or_else(resource_limit)?;
                if desired > buffer.capacity() {
                    let old = buffer.capacity();
                    let charge = self.charge.as_mut().ok_or_else(stale)?;
                    charge.grow((desired - old) as u64)?;
                    buffer
                        .try_reserve_exact(desired - buffer.len())
                        .map_err(|_| resource_limit())?;
                    if buffer.capacity() > desired {
                        charge.grow((buffer.capacity() - desired) as u64)?;
                    }
                }
                buffer.extend_from_slice(chunk.bytes());
                self.transfer = Some(chunk.transfer());
                self.totals[index] = Some(chunk.total());
                Ok(None)
            }
            CopyStreamItem::Complete(completion) => {
                if completion.stamp() != self.request.stamp()
                    || self
                        .transfer
                        .is_some_and(|value| value != completion.transfer())
                    || completion.html().is_some() != self.request.options().include_html
                    || completion.plain_bytes() != self.plain.len() as u64
                    || self.totals[0].is_some_and(|value| value != completion.plain_bytes())
                    || <[u8; 32]>::from(Sha256::digest(&self.plain)) != completion.plain_digest()
                {
                    return Err(invalid_peer());
                }
                if let Some((length, digest)) = completion.html()
                    && (length != self.html.len() as u64
                        || self.totals[1].is_some_and(|value| value != length)
                        || <[u8; 32]>::from(Sha256::digest(&self.html)) != digest)
                {
                    return Err(invalid_peer());
                }
                let plain = String::from_utf8(std::mem::take(&mut self.plain))
                    .map_err(|_| invalid_peer())?;
                let html = if completion.html().is_some() {
                    Some(
                        String::from_utf8(std::mem::take(&mut self.html))
                            .map_err(|_| invalid_peer())?,
                    )
                } else {
                    None
                };
                self.finished = true;
                Ok(Some(PreparedCopyLease(Arc::new(PreparedCopy {
                    request: self.request,
                    plain,
                    html,
                    eligibility: self.eligibility.clone(),
                    generation: Arc::clone(&self.generation),
                    captured: self.captured,
                    _charge: self.charge.take().ok_or_else(stale)?,
                }))))
            }
        }
    }
}
