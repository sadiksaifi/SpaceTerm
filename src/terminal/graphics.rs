use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libghostty_vt::kitty::graphics::{ImageFormat, ResolvedPlacement};
use libghostty_vt::{Error, Terminal};

pub(crate) const IMAGE_STORAGE_LIMIT: usize = 96 * 1024 * 1024;
pub(crate) const APC_TRANSMISSION_LIMIT: usize = 128 * 1024 * 1024;
const SESSION_RESERVATION: usize = IMAGE_STORAGE_LIMIT * 2;
const APPLICATION_DECODED_LIMIT: usize = SESSION_RESERVATION * 2;

static RESERVED_DECODED_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static GRAPHICS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    GRAPHICS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
pub(crate) struct GraphicsReservation;

impl GraphicsReservation {
    pub(crate) fn try_acquire() -> Option<Self> {
        RESERVED_DECODED_BYTES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
                reserved
                    .checked_add(SESSION_RESERVATION)
                    .filter(|next| *next <= APPLICATION_DECODED_LIMIT)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for GraphicsReservation {
    fn drop(&mut self) {
        RESERVED_DECODED_BYTES.fetch_sub(SESSION_RESERVATION, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ImageKey {
    pub(crate) image_id: u32,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImageSnapshot {
    pub(crate) key: ImageKey,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImagePlacementSnapshot {
    pub(crate) image: ImageKey,
    pub(crate) placement_id: u32,
    pub(crate) z: i32,
    pub(crate) viewport_col: i32,
    pub(crate) viewport_row: i32,
    pub(crate) cell_offset_x: u32,
    pub(crate) cell_offset_y: u32,
    pub(crate) source_x: u32,
    pub(crate) source_y: u32,
    pub(crate) source_width: u32,
    pub(crate) source_height: u32,
    pub(crate) destination_width: u32,
    pub(crate) destination_height: u32,
    pub(crate) unicode_placeholder: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GraphicsSnapshot {
    pub(crate) generation: u64,
    pub(crate) images: Arc<[Arc<ImageSnapshot>]>,
    pub(crate) placements: Arc<[ImagePlacementSnapshot]>,
}

#[derive(Default)]
pub(crate) struct GraphicsState {
    image_cache: HashMap<ImageKey, Arc<ImageSnapshot>>,
    published: GraphicsSnapshot,
}

impl GraphicsState {
    pub(crate) fn snapshot(
        &mut self,
        terminal: &Terminal<'_, '_>,
    ) -> Result<GraphicsSnapshot, Error> {
        let graphics = terminal.kitty_graphics()?;
        let generation = graphics.generation()?;
        let resolved = terminal.resolved_kitty_placements()?;
        let mut current_keys = HashSet::new();
        let mut images = Vec::new();
        let mut placements = Vec::with_capacity(resolved.len());

        for placement in resolved {
            let Some(image) = graphics.image(placement.image_id) else {
                continue;
            };
            let key = ImageKey {
                image_id: placement.image_id,
                generation: image.generation()?,
            };
            if current_keys.insert(key) {
                let snapshot = if let Some(cached) = self.image_cache.get(&key) {
                    Arc::clone(cached)
                } else {
                    Arc::new(copy_image(key, &image)?)
                };
                self.image_cache.insert(key, Arc::clone(&snapshot));
                images.push(snapshot);
            }
            placements.push(snapshot_placement(key, placement));
        }

        self.image_cache.retain(|key, _| current_keys.contains(key));
        images.sort_unstable_by_key(|image| image.key);
        placements.sort_unstable_by_key(|placement| (placement.z, placement.image.image_id));
        let next = GraphicsSnapshot {
            generation,
            images: Arc::from(images),
            placements: Arc::from(placements),
        };
        self.published = next.clone();
        Ok(next)
    }

    pub(crate) fn published(&self) -> &GraphicsSnapshot {
        &self.published
    }
}

fn copy_image(
    key: ImageKey,
    image: &libghostty_vt::kitty::graphics::Image<'_>,
) -> Result<ImageSnapshot, Error> {
    let width = image.width()?;
    let height = image.height()?;
    let pixel_count = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(Error::OutOfMemory)?;
    let source = image.data()?;
    let rgba = match image.format()? {
        ImageFormat::Rgba => {
            if source.len() != pixel_count.checked_mul(4).ok_or(Error::OutOfMemory)? {
                return Err(Error::InvalidValue);
            }
            Arc::from(source)
        }
        ImageFormat::Rgb => {
            if source.len() != pixel_count.checked_mul(3).ok_or(Error::OutOfMemory)? {
                return Err(Error::InvalidValue);
            }
            let mut rgba =
                Vec::with_capacity(pixel_count.checked_mul(4).ok_or(Error::OutOfMemory)?);
            for pixel in source.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], u8::MAX]);
            }
            Arc::from(rgba)
        }
        _ => return Err(Error::InvalidValue),
    };
    Ok(ImageSnapshot {
        key,
        width,
        height,
        rgba,
    })
}

fn snapshot_placement(key: ImageKey, placement: ResolvedPlacement) -> ImagePlacementSnapshot {
    ImagePlacementSnapshot {
        image: key,
        placement_id: placement.placement_id,
        z: placement.z,
        viewport_col: placement.viewport_col,
        viewport_row: placement.viewport_row,
        cell_offset_x: placement.cell_offset_x,
        cell_offset_y: placement.cell_offset_y,
        source_x: placement.source_x,
        source_y: placement.source_y,
        source_width: placement.source_width,
        source_height: placement.source_height,
        destination_width: placement.dest_width,
        destination_height: placement.dest_height,
        unicode_placeholder: placement.is_virtual,
    }
}

pub(crate) fn starts_apc(previous_byte: Option<u8>, bytes: &[u8]) -> bool {
    (previous_byte == Some(0x1b) && bytes.first() == Some(&b'_'))
        || bytes.windows(2).any(|window| window == b"\x1b_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_apc_across_worker_reads() {
        assert!(starts_apc(None, b"text\x1b_G"));
        assert!(starts_apc(Some(0x1b), b"_G"));
        assert!(!starts_apc(None, b"ordinary output"));
    }

    #[test]
    fn decoded_budget_is_bounded_across_sessions() {
        let _guard = test_lock();
        let first = GraphicsReservation::try_acquire().unwrap();
        let second = GraphicsReservation::try_acquire().unwrap();
        assert!(GraphicsReservation::try_acquire().is_none());
        drop(first);
        assert!(GraphicsReservation::try_acquire().is_some());
        drop(second);
    }
}
