use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use libghostty_vt::kitty::graphics::{ImageFormat, ResolvedPlacement};
use libghostty_vt::{Error, Terminal};

pub(crate) const IMAGE_STORAGE_LIMIT: usize = 96 * 1024 * 1024;
pub(crate) const APC_TRANSMISSION_LIMIT: usize = 128 * 1024 * 1024;
// Bound resident native image/frame data and owned RGBA snapshots together. Transient
// parser and decoder allocations have their own transmission and image-size limits.
pub(crate) const APPLICATION_DECODED_LIMIT: usize = IMAGE_STORAGE_LIMIT * 4;

static DECODED_BUDGET: Mutex<DecodedBudget> = Mutex::new(DecodedBudget {
    bytes: 0,
    waiting: Vec::new(),
});
#[cfg(test)]
static GRAPHICS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    GRAPHICS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) struct GraphicsBudgetWake {
    pending: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl GraphicsBudgetWake {
    pub(crate) fn new(notify: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            pending: AtomicBool::new(false),
            notify: Box::new(notify),
        })
    }

    pub(crate) fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            (self.notify)();
        }
    }
}

struct DecodedBudget {
    bytes: usize,
    waiting: Vec<(Weak<GraphicsBudgetWake>, usize)>,
}

impl DecodedBudget {
    fn wait_for(&mut self, wake: &Arc<GraphicsBudgetWake>, bytes: usize) {
        self.waiting.retain(|(wake, _)| wake.strong_count() != 0);
        let weak = Arc::downgrade(wake);
        if let Some((_, required)) = self
            .waiting
            .iter_mut()
            .find(|(waiting, _)| waiting.ptr_eq(&weak))
        {
            *required = (*required).min(bytes);
        } else {
            self.waiting.push((weak, bytes));
        }
    }

    fn ready(&mut self) -> Vec<Arc<GraphicsBudgetWake>> {
        let available = APPLICATION_DECODED_LIMIT.saturating_sub(self.bytes);
        let mut ready = Vec::new();
        self.waiting.retain(|(wake, required)| {
            let Some(wake) = wake.upgrade() else {
                return false;
            };
            if *required <= available {
                ready.push(wake);
                false
            } else {
                true
            }
        });
        ready
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct GraphicsReservation {
    bytes: usize,
}

impl GraphicsReservation {
    #[cfg(test)]
    pub(crate) fn try_acquire(bytes: usize) -> Option<Self> {
        Self::try_acquire_or_wait(bytes, None)
    }

    fn try_acquire_or_wait(bytes: usize, wake: Option<&Arc<GraphicsBudgetWake>>) -> Option<Self> {
        let mut budget = DECODED_BUDGET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next = budget.bytes.checked_add(bytes)?;
        if next > APPLICATION_DECODED_LIMIT {
            if let Some(wake) = wake {
                budget.wait_for(wake, bytes);
            }
            return None;
        }
        budget.bytes = next;
        Some(Self { bytes })
    }

    pub(crate) fn write(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        bytes: &[u8],
    ) -> Result<(), Error> {
        // A feed can decode images or change either screen. Serialize this bounded
        // operation so temporary capacity never causes another Session to evict live
        // images. Retain only actual residency after the feed has completed.
        let mut budget = DECODED_BUDGET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let resident = terminal.kitty_image_storage_bytes()?;
        let available = APPLICATION_DECODED_LIMIT.saturating_sub(budget.bytes);
        let limits = storage_limits(resident, available);
        terminal.set_kitty_image_storage_limits(limits[0], limits[1])?;
        terminal.vt_write(bytes);
        let residency = terminal.kitty_image_storage_bytes();
        // If a native query fails, retain the entire admitted capacity until the
        // failed Terminal Session is dropped rather than undercounting its data.
        let resident = residency.unwrap_or(limits);
        let resident_bytes = resident[0].saturating_add(resident[1]);
        budget.bytes = budget
            .bytes
            .saturating_sub(self.bytes)
            .saturating_add(resident_bytes);
        self.bytes = resident_bytes;
        let ready = budget.ready();
        drop(budget);
        for wake in ready {
            wake.notify();
        }
        residency.map(|_| ())
    }
}

impl Drop for GraphicsReservation {
    fn drop(&mut self) {
        let mut budget = DECODED_BUDGET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        budget.bytes = budget.bytes.saturating_sub(self.bytes);
        let ready = budget.ready();
        drop(budget);
        for wake in ready {
            wake.notify();
        }
    }
}

fn storage_limits(resident: [usize; 2], available: usize) -> [usize; 2] {
    let first_growth = (IMAGE_STORAGE_LIMIT.saturating_sub(resident[0])).min(available / 2);
    let second_growth = (IMAGE_STORAGE_LIMIT.saturating_sub(resident[1]))
        .min(available.saturating_sub(first_growth));
    // A nonzero limit keeps protocol queries working even when no pixel payload
    // fits. A one-byte allowance cannot admit the minimum RGB/RGBA image.
    [
        resident[0].saturating_add(first_growth).max(1),
        resident[1].saturating_add(second_growth).max(1),
    ]
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
    pub(crate) reservation: Option<Arc<GraphicsReservation>>,
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
    pub(crate) placement_generation: u64,
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
        wake: Option<&Arc<GraphicsBudgetWake>>,
    ) -> Result<GraphicsSnapshot, Error> {
        let graphics = terminal.kitty_graphics()?;
        let generation = graphics.generation()?;
        let resolved = terminal.resolved_kitty_placements()?;
        if generation != self.published.generation {
            // Release superseded payloads before admitting replacements. The UI may
            // still retain its own snapshot; a capacity-release notification retries
            // any replacement that remains deferred by that owner.
            self.image_cache.retain(|key, _| {
                graphics
                    .image(key.image_id)
                    .is_some_and(|image| image.generation().ok() == Some(key.generation))
            });
            if self
                .published
                .images
                .iter()
                .any(|image| !self.image_cache.contains_key(&image.key))
            {
                self.published.images = self
                    .published
                    .images
                    .iter()
                    .filter(|image| self.image_cache.contains_key(&image.key))
                    .cloned()
                    .collect();
            }
        }
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
                    let Some(image) = copy_image(key, &image, wake)? else {
                        current_keys.remove(&key);
                        continue;
                    };
                    Arc::new(image)
                };
                self.image_cache.insert(key, Arc::clone(&snapshot));
                images.push(snapshot);
            }
            placements.push(snapshot_placement(key, placement));
        }

        self.image_cache.retain(|key, _| current_keys.contains(key));
        images.sort_unstable_by_key(|image| image.key);
        placements.sort_unstable_by_key(|placement| (placement.z, placement.image.image_id));
        Ok(self.publish_collections(generation, images, placements))
    }

    pub(crate) fn published(&self) -> &GraphicsSnapshot {
        &self.published
    }

    fn publish_collections(
        &mut self,
        generation: u64,
        images: Vec<Arc<ImageSnapshot>>,
        placements: Vec<ImagePlacementSnapshot>,
    ) -> GraphicsSnapshot {
        let images = if image_snapshots_match(&self.published.images, &images) {
            Arc::clone(&self.published.images)
        } else {
            Arc::from(images)
        };
        let placements = if self.published.placements.as_ref() == placements.as_slice() {
            Arc::clone(&self.published.placements)
        } else {
            Arc::from(placements)
        };
        let placement_generation = if Arc::ptr_eq(&placements, &self.published.placements) {
            self.published.placement_generation
        } else {
            self.published.placement_generation.saturating_add(1)
        };
        let next = GraphicsSnapshot {
            generation,
            placement_generation,
            images,
            placements,
        };
        self.published = next.clone();
        next
    }
}

fn image_snapshots_match(
    published: &Arc<[Arc<ImageSnapshot>]>,
    candidate: &[Arc<ImageSnapshot>],
) -> bool {
    published.len() == candidate.len()
        && published
            .iter()
            .zip(candidate)
            .all(|(published, candidate)| Arc::ptr_eq(published, candidate))
}

fn copy_image(
    key: ImageKey,
    image: &libghostty_vt::kitty::graphics::Image<'_>,
    wake: Option<&Arc<GraphicsBudgetWake>>,
) -> Result<Option<ImageSnapshot>, Error> {
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
    let Some(source) = image.data_if_ready()? else {
        return Ok(None);
    };
    let rgba_len = pixel_count.checked_mul(4).ok_or(Error::OutOfMemory)?;
    let Some(reservation) = GraphicsReservation::try_acquire_or_wait(rgba_len, wake) else {
        return Ok(None);
    };
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
    Ok(Some(ImageSnapshot {
        key,
        width,
        height,
        rgba,
        reservation: Some(Arc::new(reservation)),
    }))
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

    fn image(image_id: u32) -> Arc<ImageSnapshot> {
        Arc::new(ImageSnapshot {
            key: ImageKey {
                image_id,
                generation: 1,
            },
            width: 1,
            height: 1,
            rgba: Arc::from([0, 0, 0, 255]),
            reservation: None,
        })
    }

    fn placement(image: &ImageSnapshot, viewport_row: i32) -> ImagePlacementSnapshot {
        ImagePlacementSnapshot {
            image: image.key,
            placement_id: 1,
            z: 0,
            viewport_col: 0,
            viewport_row,
            cell_offset_x: 0,
            cell_offset_y: 0,
            source_x: 0,
            source_y: 0,
            source_width: 1,
            source_height: 1,
            destination_width: 1,
            destination_height: 1,
            unicode_placeholder: false,
        }
    }

    #[test]
    fn recognizes_apc_across_worker_reads() {
        assert!(starts_apc(None, b"text\x1b_G"));
        assert!(starts_apc(Some(0x1b), b"_G"));
        assert!(!starts_apc(None, b"ordinary output"));
    }

    #[test]
    fn decoded_budget_is_bounded_across_sessions() {
        let _guard = test_lock();
        let first = GraphicsReservation::try_acquire(APPLICATION_DECODED_LIMIT - 16).unwrap();
        let second = GraphicsReservation::try_acquire(16).unwrap();
        assert!(GraphicsReservation::try_acquire(1).is_none());
        drop(first);
        assert!(GraphicsReservation::try_acquire(APPLICATION_DECODED_LIMIT - 16).is_some());
        drop(second);
    }

    #[test]
    fn snapshot_reservation_lives_until_the_last_owner_releases_pixels() {
        let _guard = test_lock();
        let reservation =
            Arc::new(GraphicsReservation::try_acquire(APPLICATION_DECODED_LIMIT).unwrap());
        let retained = Arc::clone(&reservation);
        drop(reservation);
        assert!(GraphicsReservation::try_acquire(1).is_none());
        drop(retained);
        assert!(GraphicsReservation::try_acquire(APPLICATION_DECODED_LIMIT).is_some());
    }

    #[test]
    fn deferred_pixel_capacity_wakes_once_after_a_sufficient_release() {
        let _guard = test_lock();
        let large = GraphicsReservation::try_acquire(APPLICATION_DECODED_LIMIT - 2).unwrap();
        let small = GraphicsReservation::try_acquire(2).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = GraphicsBudgetWake::new(move || {
            let _ = sender.send(());
        });
        assert!(GraphicsReservation::try_acquire_or_wait(4, Some(&wake)).is_none());
        assert!(GraphicsReservation::try_acquire_or_wait(4, Some(&wake)).is_none());
        drop(small);
        assert!(receiver.try_recv().is_err());
        drop(large);
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        assert!(wake.take());
        assert!(!wake.take());
    }

    #[test]
    fn deferred_pixel_capacity_does_not_retain_a_closed_terminal_session() {
        let _guard = test_lock();
        let reservation = GraphicsReservation::try_acquire(APPLICATION_DECODED_LIMIT).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = GraphicsBudgetWake::new(move || {
            let _ = sender.send(());
        });
        assert!(GraphicsReservation::try_acquire_or_wait(4, Some(&wake)).is_none());
        drop(wake);
        drop(reservation);
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn storage_growth_never_evicts_live_images_when_screens_have_unequal_usage() {
        assert_eq!(storage_limits([100, 4], 0), [100, 4]);
        assert_eq!(storage_limits([100, 4], 16), [108, 12]);
        assert_eq!(storage_limits([0, 0], 0), [1, 1]);
        assert_eq!(
            storage_limits([0, 0], APPLICATION_DECODED_LIMIT),
            [IMAGE_STORAGE_LIMIT; 2]
        );
    }

    #[test]
    fn unchanged_graphics_reuse_collection_arcs_and_placement_generation() {
        let image = image(1);
        let placement = placement(&image, 0);
        let mut state = GraphicsState::default();
        let first = state.publish_collections(1, vec![Arc::clone(&image)], vec![placement.clone()]);

        let second = state.publish_collections(1, vec![image], vec![placement]);

        assert!(Arc::ptr_eq(&first.images, &second.images));
        assert!(Arc::ptr_eq(&first.placements, &second.placements));
        assert_eq!(first.placement_generation, second.placement_generation);
    }

    #[test]
    fn geometry_change_advances_only_the_placement_generation() {
        let image = image(1);
        let mut state = GraphicsState::default();
        let first =
            state.publish_collections(8, vec![Arc::clone(&image)], vec![placement(&image, 0)]);

        let second = state.publish_collections(8, vec![image.clone()], vec![placement(&image, 1)]);

        assert_eq!(first.generation, second.generation);
        assert!(second.placement_generation > first.placement_generation);
        assert!(!Arc::ptr_eq(&first.placements, &second.placements));
    }
}
