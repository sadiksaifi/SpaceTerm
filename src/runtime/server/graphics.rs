use super::*;
use pane::graphics::{CHUNK_BYTES, Chunk, Error, Image, ImageKey, Snapshot};

pub(super) fn encode(graphics: &crate::terminal::GraphicsSnapshot) -> Result<Snapshot, Error> {
    if graphics.images.len() > pane::graphics::MAX_IMAGES
        || graphics.placements.len() > pane::graphics::MAX_PLACEMENTS
    {
        return Err(Error::TooLarge);
    }
    let snapshot = Snapshot {
        generation: graphics.generation,
        placement_generation: graphics.placement_generation,
        images: graphics
            .images
            .iter()
            .map(|image| Image {
                key: image.key,
                width: image.width,
                height: image.height,
            })
            .collect(),
        placements: graphics.placements.to_vec(),
    };
    snapshot.validate()?;
    Ok(snapshot)
}

impl OwnedPane {
    pub(super) fn graphics_chunk(&mut self, key: ImageKey, offset: usize) -> Result<Chunk, Error> {
        if !offset.is_multiple_of(CHUNK_BYTES) {
            return Err(Error::Malformed);
        }
        // One pinned source lets a slow controller finish a coherent animation frame.
        // Its existing application reservation remains charged until the last Arc is released.
        // Lease invalidation, deletion, and active-screen changes discard this source.
        if offset == 0 {
            self.graphics_transfer = None;
            let screen = self.screen.as_ref().ok_or(Error::Missing)?;
            let image = screen
                .graphics
                .images
                .iter()
                .find(|image| image.key == key)
                .ok_or(Error::Missing)?;
            self.graphics_transfer = Some((screen.active_screen, Arc::clone(image)));
        }
        let (_, image) = self
            .graphics_transfer
            .as_ref()
            .filter(|(_, image)| image.key == key)
            .ok_or(Error::Missing)?;
        let descriptor = Image {
            key,
            width: image.width,
            height: image.height,
        };
        if descriptor.byte_len()? != image.rgba.len() || offset >= image.rgba.len() {
            return Err(Error::Malformed);
        }
        let end = offset.saturating_add(CHUNK_BYTES).min(image.rgba.len());
        let chunk = Chunk {
            image: key,
            offset,
            bytes: image.rgba[offset..end].to_vec(),
        };
        if end == image.rgba.len() {
            self.graphics_transfer = None;
        }
        Ok(chunk)
    }
}
