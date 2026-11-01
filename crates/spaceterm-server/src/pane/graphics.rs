//! Bounded, renderer-independent RGBA resources. Keys are meaningful only within a lease.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_RESIDENT_BYTES: usize = 96 * 1024 * 1024;
pub const MAX_IMAGES: usize = 1024;
pub const MAX_PLACEMENTS: usize = 1024;
pub const CHUNK_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ImageKey {
    pub image_id: u32,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub image: ImageKey,
    pub placement_id: u32,
    pub z: i32,
    pub viewport_col: i32,
    pub viewport_row: i32,
    pub cell_offset_x: u32,
    pub cell_offset_y: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub destination_width: u32,
    pub destination_height: u32,
    pub unicode_placeholder: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub key: ImageKey,
    pub width: u32,
    pub height: u32,
}
impl Image {
    pub fn byte_len(&self) -> Result<usize, Error> {
        let bytes = u64::from(self.width) * u64::from(self.height);
        if bytes == 0 || bytes > (MAX_RESIDENT_BYTES / 4) as u64 {
            return Err(Error::TooLarge);
        }
        Ok(bytes as usize * 4)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub generation: u64,
    pub placement_generation: u64,
    pub images: Vec<Image>,
    pub placements: Vec<Placement>,
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), Error> {
        if self.images.len() > MAX_IMAGES || self.placements.len() > MAX_PLACEMENTS {
            return Err(Error::TooLarge);
        }
        let mut images = HashMap::new();
        let mut total = 0usize;
        for image in &self.images {
            total = total
                .checked_add(image.byte_len()?)
                .ok_or(Error::TooLarge)?;
            if total > MAX_RESIDENT_BYTES {
                return Err(Error::TooLarge);
            }
            if images.insert(image.key, image).is_some() {
                return Err(Error::Malformed);
            }
        }
        for placement in &self.placements {
            let image = images.get(&placement.image).ok_or(Error::Missing)?;
            if placement
                .source_x
                .checked_add(placement.source_width)
                .is_none_or(|end| end > image.width)
                || placement
                    .source_y
                    .checked_add(placement.source_height)
                    .is_none_or(|end| end > image.height)
            {
                return Err(Error::Malformed);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub image: ImageKey,
    pub offset: usize,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error)]
pub enum Error {
    #[error("malformed terminal graphics")]
    Malformed,
    #[error("terminal graphics exceed resource limits")]
    TooLarge,
    #[error("terminal graphics resource is no longer available")]
    Missing,
    #[error("terminal graphics capacity is unavailable")]
    Capacity,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifests_reject_overflow_duplicate_keys_and_unavailable_crop_sources() {
        let key = ImageKey {
            image_id: 1,
            generation: 1,
        };
        assert_eq!(
            Image {
                key,
                width: u32::MAX,
                height: u32::MAX
            }
            .byte_len(),
            Err(Error::TooLarge)
        );
        let image = Image {
            key,
            width: 1,
            height: 1,
        };
        let mut snapshot = Snapshot {
            generation: 1,
            placement_generation: 1,
            images: vec![image.clone(), image],
            placements: vec![],
        };
        assert_eq!(snapshot.validate(), Err(Error::Malformed));
        snapshot.images.pop();
        let placement: Placement = serde_json::from_str(r#"{"image":{"image_id":2,"generation":1},"placement_id":1,"z":0,"viewport_col":0,"viewport_row":0,"cell_offset_x":0,"cell_offset_y":0,"source_x":0,"source_y":0,"source_width":1,"source_height":1,"destination_width":1,"destination_height":1,"unicode_placeholder":false}"#).unwrap();
        snapshot.placements.push(placement);
        assert_eq!(snapshot.validate(), Err(Error::Missing));
        snapshot.placements[0].image = key;
        snapshot.placements[0].source_x = u32::MAX;
        assert_eq!(snapshot.validate(), Err(Error::Malformed));
    }
}
