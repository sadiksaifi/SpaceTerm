//! Session-qualified immutable graphics. Native decoded storage is not cloned
//! into pages. Digest preparation and RGBA transfers use bounded quanta.
use super::pages::ContentScanBudget;
#[cfg(feature = "kitty-graphics")]
use super::pages::{native_failure, row_id};
#[cfg(feature = "kitty-graphics")]
use libghostty_vt::{
    content as vt,
    kitty::graphics::{Image, ImageFormat},
};
#[cfg(feature = "kitty-graphics")]
use sha2::{Digest, Sha256};
use spaceterm_core::{content::*, content_queries::*};
#[cfg(feature = "kitty-graphics")]
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
};

#[cfg(feature = "kitty-graphics")]
#[derive(Default)]
pub(crate) struct GraphicsCatalog {
    #[cfg(feature = "kitty-graphics")]
    ready: RefCell<BTreeMap<(u32, u64), GraphicsResourceRef>>,
    #[cfg(feature = "kitty-graphics")]
    pending: RefCell<VecDeque<Preparation>>,
    order: RefCell<VecDeque<(u32, u64)>>,
}
#[cfg(feature = "kitty-graphics")]
struct Preparation {
    id: u32,
    generation: u64,
    width: u32,
    height: u32,
    total: u32,
    offset: u32,
    digest: Sha256,
}
// Manifest residency is distinct from the 128-resource page ceiling. Charge
// each tree/queue entry at 512 bytes, including spare node/queue capacity.
#[cfg(feature = "kitty-graphics")]
const MANIFEST_CACHE_BYTES: usize = 256 * 1024;
#[cfg(feature = "kitty-graphics")]
const MANIFEST_ENTRY_BYTES: usize = 512;
#[cfg(feature = "kitty-graphics")]
impl GraphicsCatalog {
    fn make_room(&self, pending_count: usize) -> Result<(), ContentReadFailure> {
        let mut ready = self.ready.borrow_mut();
        let mut order = self.order.borrow_mut();
        while (ready.len() + pending_count + 1) * MANIFEST_ENTRY_BYTES > MANIFEST_CACHE_BYTES {
            let old = order.pop_front().ok_or(ContentReadFailure::Capacity)?;
            ready.remove(&old);
        }
        Ok(())
    }
    fn touch(&self, key: (u32, u64)) {
        let mut order = self.order.borrow_mut();
        if let Some(index) = order.iter().position(|value| *value == key) {
            order.remove(index);
        }
        order.push_back(key);
    }
}
/// Exact immutable asset request and streaming position. No borrowed image escapes.
pub struct GraphicsJob {
    request: GraphicsRequest,
    #[cfg(feature = "kitty-graphics")]
    offset: u32,
    done: bool,
}
#[expect(
    clippy::large_enum_variant,
    reason = "worker-local return values are boxed once at the runtime stream boundary"
)]
pub enum GraphicsStep {
    Item(GraphicsChunk),
    Complete,
}
impl crate::TerminalEmulator {
    /// Prepare at most one 256 KiB digest quantum. The native asset generation,
    /// not unrelated text output, determines whether preparation remains valid.
    pub fn prepare_graphics_quantum(&mut self) -> Result<bool, ContentReadFailure> {
        #[cfg(not(feature = "kitty-graphics"))]
        {
            Ok(false)
        }
        #[cfg(feature = "kitty-graphics")]
        {
            let placement_work = self
                .semantic_content
                .reader
                .prepare_placements(&self.terminal)
                .map_err(native_failure)?
                .progressed;
            let Some(mut pending) = self
                .semantic_content
                .graphics
                .pending
                .borrow_mut()
                .pop_front()
            else {
                return Ok(placement_work);
            };
            let graphics = self.terminal.kitty_graphics().map_err(native_failure)?;
            let Some(image) = graphics.image(pending.id) else {
                return Ok(true);
            };
            if image.generation().map_err(native_failure)? != pending.generation {
                return Ok(true);
            }
            let end = pending
                .offset
                .checked_add(MAX_RESOURCE_CHUNK_BYTES as u32)
                .ok_or(ContentReadFailure::Exhausted)?
                .min(pending.total);
            // Fixed scratch bounds RGB expansion without retaining another image.
            let mut scratch = [0; 4096];
            while pending.offset < end {
                let count = (end - pending.offset).min(scratch.len() as u32);
                copy_rgba(&image, pending.offset, &mut scratch[..count as usize])?;
                pending.digest.update(&scratch[..count as usize]);
                pending.offset += count;
            }
            if pending.offset == pending.total {
                let session = self
                    .semantic_content
                    .session
                    .ok_or(ContentReadFailure::InvalidRange)?;
                let resource = GraphicsResourceRef {
                    key: GraphicsResourceKey {
                        session,
                        id: GraphicsResourceId::new(u64::from(pending.id) + 1)
                            .map_err(|_| ContentReadFailure::Exhausted)?,
                        generation: pending.generation,
                    },
                    width: pending.width,
                    height: pending.height,
                    rgba_bytes: pending.total,
                    digest: pending.digest.finalize().into(),
                };
                let catalog = &self.semantic_content.graphics;
                catalog.make_room(catalog.pending.borrow().len())?;
                let key = (pending.id, pending.generation);
                catalog.ready.borrow_mut().insert(key, resource);
                catalog.touch(key);
            } else {
                self.semantic_content
                    .graphics
                    .pending
                    .borrow_mut()
                    .push_back(pending);
            }
            Ok(true)
        }
    }
    pub fn begin_graphics(
        &mut self,
        request: &GraphicsRequest,
    ) -> Result<GraphicsJob, ContentReadFailure> {
        self.semantic_content
            .check(&self.terminal, request.stamp())?;
        self.validate_graphics_resource(request.resource())?;
        Ok(GraphicsJob {
            request: *request,
            #[cfg(feature = "kitty-graphics")]
            offset: request.offset(),
            done: false,
        })
    }
    pub fn step_graphics(
        &mut self,
        job: &mut GraphicsJob,
        budget: ContentScanBudget,
    ) -> Result<GraphicsStep, ContentReadFailure> {
        if job.done {
            return Ok(GraphicsStep::Complete);
        }
        self.semantic_content
            .check(&self.terminal, job.request.stamp())?;
        self.validate_live_graphics_resource(job.request.resource())?;
        #[cfg(not(feature = "kitty-graphics"))]
        {
            let _ = budget;
            Err(ContentReadFailure::MissingResource)
        }
        #[cfg(feature = "kitty-graphics")]
        {
            let bytes = budget
                .bytes
                .min(MAX_RESOURCE_CHUNK_BYTES)
                .min((job.request.resource().rgba_bytes - job.offset) as usize);
            if bytes == 0 {
                return Err(ContentReadFailure::Capacity);
            }
            let id = image_id(job.request.resource())?;
            let graphics = self.terminal.kitty_graphics().map_err(native_failure)?;
            let image = graphics
                .image(id)
                .ok_or(ContentReadFailure::MissingResource)?;
            let mut output = Vec::new();
            output
                .try_reserve_exact(bytes)
                .map_err(|_| ContentReadFailure::Capacity)?;
            output.resize(bytes, 0);
            copy_rgba(&image, job.offset, &mut output)?;
            let offset = job.offset;
            job.offset = job
                .offset
                .checked_add(bytes as u32)
                .ok_or(ContentReadFailure::Exhausted)?;
            job.done = job.offset == job.request.resource().rgba_bytes;
            Ok(GraphicsStep::Item(
                GraphicsChunk::new(job.request, offset, output, job.done)
                    .map_err(|_| ContentReadFailure::InvalidRange)?,
            ))
        }
    }
    fn validate_live_graphics_resource(
        &self,
        resource: GraphicsResourceRef,
    ) -> Result<(), ContentReadFailure> {
        if Some(resource.key.session) != self.semantic_content.session {
            return Err(ContentReadFailure::MissingResource);
        }
        #[cfg(not(feature = "kitty-graphics"))]
        {
            let _ = resource;
            Err(ContentReadFailure::MissingResource)
        }
        #[cfg(feature = "kitty-graphics")]
        {
            let id = image_id(resource)?;
            let graphics = self.terminal.kitty_graphics().map_err(native_failure)?;
            let image = graphics
                .image(id)
                .ok_or(ContentReadFailure::MissingResource)?;
            if image.generation().map_err(native_failure)? != resource.key.generation {
                return Err(ContentReadFailure::MissingResource);
            }
            if image.width().map_err(native_failure)? != resource.width
                || image.height().map_err(native_failure)? != resource.height
            {
                return Err(ContentReadFailure::MissingResource);
            }
            Ok(())
        }
    }
    fn validate_graphics_resource(
        &self,
        resource: GraphicsResourceRef,
    ) -> Result<(), ContentReadFailure> {
        self.validate_live_graphics_resource(resource)?;
        #[cfg(feature = "kitty-graphics")]
        if self
            .semantic_content
            .graphics
            .ready
            .borrow()
            .get(&(image_id(resource)?, resource.key.generation))
            != Some(&resource)
        {
            return Err(ContentReadFailure::MissingResource);
        }
        Ok(())
    }
    pub(crate) fn semantic_graphics(
        &self,
        parts: &mut ContentPageParts,
        first: u32,
        last: u32,
    ) -> Result<(), ContentReadFailure> {
        #[cfg(not(feature = "kitty-graphics"))]
        {
            let _ = (parts, first, last);
            Ok(())
        }
        #[cfg(feature = "kitty-graphics")]
        {
            let graphics = self.terminal.kitty_graphics().map_err(native_failure)?;
            self.semantic_content
                .graphics
                .ready
                .borrow_mut()
                .retain(|(id, generation), _| {
                    graphics
                        .image(*id)
                        .is_some_and(|image| image.generation().ok() == Some(*generation))
                });
            self.semantic_content
                .graphics
                .order
                .borrow_mut()
                .retain(|key| {
                    self.semantic_content
                        .graphics
                        .ready
                        .borrow()
                        .contains_key(key)
                });
            let placements = self
                .semantic_content
                .reader
                .read_placements(&self.terminal, first, last)
                .map_err(native_failure)?;
            for placement in placements {
                let id = placement.image_id;
                let image = graphics
                    .image(id)
                    .ok_or(ContentReadFailure::MissingResource)?;
                let generation = image.generation().map_err(native_failure)?;
                let resource = if let Some(resource) = self
                    .semantic_content
                    .graphics
                    .ready
                    .borrow()
                    .get(&(id, generation))
                    .copied()
                {
                    self.semantic_content.graphics.touch((id, generation));
                    resource
                } else {
                    let mut pending = self.semantic_content.graphics.pending.borrow_mut();
                    if !pending
                        .iter()
                        .any(|value| value.id == id && value.generation == generation)
                    {
                        if pending.len() >= MAX_PAGE_RESOURCES {
                            return Err(ContentReadFailure::Capacity);
                        }
                        self.semantic_content.graphics.make_room(pending.len())?;
                        let width = image.width().map_err(native_failure)?;
                        let height = image.height().map_err(native_failure)?;
                        let total = width
                            .checked_mul(height)
                            .and_then(|count| count.checked_mul(4))
                            .filter(|count| *count > 0 && *count <= MAX_GRAPHICS_RESOURCE_BYTES)
                            .ok_or(ContentReadFailure::Capacity)?;
                        pending
                            .try_reserve(1)
                            .map_err(|_| ContentReadFailure::Capacity)?;
                        pending.push_back(Preparation {
                            id,
                            generation,
                            width,
                            height,
                            total,
                            offset: 0,
                            digest: Sha256::new(),
                        });
                    }
                    return Err(ContentReadFailure::Capacity);
                };
                if !parts.resources.contains(&resource) {
                    if parts.resources.len() >= MAX_PAGE_RESOURCES {
                        return Err(ContentReadFailure::Capacity);
                    }
                    parts
                        .resources
                        .try_reserve(1)
                        .map_err(|_| ContentReadFailure::Capacity)?;
                    parts.resources.push(resource);
                }
                let row = self
                    .semantic_content
                    .reader
                    .read_topology(
                        &self.terminal,
                        vt::Options {
                            row: placement.document_row,
                            max_rows: 1,
                            ..Default::default()
                        },
                    )
                    .map_err(native_failure)?
                    .rows
                    .into_iter()
                    .next()
                    .ok_or(ContentReadFailure::InvalidRange)?;
                parts
                    .placements
                    .try_reserve(1)
                    .map_err(|_| ContentReadFailure::Capacity)?;
                parts.placements.push(GraphicsPlacement {
                    resource: resource.key,
                    placement_id: placement.placement_id,
                    anchor: row_id(row.id),
                    column: placement.column,
                    columns: u16::try_from(placement.columns)
                        .map_err(|_| ContentReadFailure::Capacity)?,
                    rows: u16::try_from(placement.rows)
                        .map_err(|_| ContentReadFailure::Capacity)?,
                    pixel_offset_x: placement.offset_x,
                    pixel_offset_y: placement.offset_y,
                    source: ImageRect {
                        x: placement.source_x,
                        y: placement.source_y,
                        width: placement.source_width,
                        height: placement.source_height,
                    },
                    destination_width: placement.dest_width,
                    destination_height: placement.dest_height,
                    unicode_placeholder: placement.is_virtual,
                    z_index: placement.z,
                });
            }
            Ok(())
        }
    }
}
#[cfg(feature = "kitty-graphics")]
fn image_id(resource: GraphicsResourceRef) -> Result<u32, ContentReadFailure> {
    resource
        .key
        .id
        .get()
        .checked_sub(1)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ContentReadFailure::MissingResource)
}
#[cfg(feature = "kitty-graphics")]
fn copy_rgba(image: &Image<'_>, offset: u32, output: &mut [u8]) -> Result<(), ContentReadFailure> {
    let source = image
        .data_if_ready()
        .map_err(native_failure)?
        .ok_or(ContentReadFailure::Capacity)?;
    let pixels = image
        .width()
        .map_err(native_failure)?
        .checked_mul(image.height().map_err(native_failure)?)
        .ok_or(ContentReadFailure::Capacity)? as usize;
    match image.format().map_err(native_failure)? {
        ImageFormat::Rgba => {
            if source.len() != pixels.checked_mul(4).ok_or(ContentReadFailure::Capacity)? {
                return Err(ContentReadFailure::InvalidRange);
            }
            let end = (offset as usize)
                .checked_add(output.len())
                .ok_or(ContentReadFailure::Exhausted)?;
            output.copy_from_slice(
                source
                    .get(offset as usize..end)
                    .ok_or(ContentReadFailure::InvalidRange)?,
            );
        }
        ImageFormat::Rgb => {
            if source.len() != pixels.checked_mul(3).ok_or(ContentReadFailure::Capacity)? {
                return Err(ContentReadFailure::InvalidRange);
            }
            for (index, byte) in output.iter_mut().enumerate() {
                let rgba = (offset as usize)
                    .checked_add(index)
                    .ok_or(ContentReadFailure::Exhausted)?;
                *byte = if rgba % 4 == 3 {
                    255
                } else {
                    *source
                        .get((rgba / 4) * 3 + rgba % 4)
                        .ok_or(ContentReadFailure::InvalidRange)?
                };
            }
        }
        _ => return Err(ContentReadFailure::InvalidRange),
    }
    Ok(())
}
