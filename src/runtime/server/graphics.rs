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
    pub(super) fn graphics_chunk(&self, key: ImageKey, offset: usize) -> Result<Chunk, Error> {
        // Pull from the current owned screen. No per-client queue or transfer residency is retained.
        // The caller checks the exact authenticated lease before reaching this operation.
        let image = self
            .screen
            .as_ref()
            .and_then(|screen| screen.graphics.images.iter().find(|image| image.key == key))
            .ok_or(Error::Missing)?;
        let descriptor = Image {
            key,
            width: image.width,
            height: image.height,
        };
        if descriptor.byte_len()? != image.rgba.len()
            || offset >= image.rgba.len()
            || !offset.is_multiple_of(CHUNK_BYTES)
        {
            return Err(Error::Malformed);
        }
        let end = offset.saturating_add(CHUNK_BYTES).min(image.rgba.len());
        Ok(Chunk {
            image: key,
            offset,
            bytes: image.rgba[offset..end].to_vec(),
        })
    }
}
