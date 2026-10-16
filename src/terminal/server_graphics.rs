use crate::terminal::graphics::{GraphicsReservation, GraphicsSnapshot, ImageSnapshot};
use pane::graphics::{CHUNK_BYTES, Error, Image, ImageKey, Snapshot};
use spaceterm_server::{
    ServerError,
    pane::{self, Lease, Reply, Request},
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

struct Transfer {
    image: Image,
    bytes: Vec<u8>,
    reservation: Arc<GraphicsReservation>,
}

/// One current manifest and one incremental transfer per Pane connection.
#[derive(Default)]
pub(crate) struct Graphics {
    manifest: Option<Snapshot>,
    alternate: bool,
    images: HashMap<u32, Arc<ImageSnapshot>>,
    valid: bool,
    transfer: Option<Transfer>,
    missing: HashSet<ImageKey>,
    reported: Option<Error>,
}

impl Graphics {
    pub(crate) fn update(
        &mut self,
        manifest: Option<Result<Snapshot, Error>>,
        alternate: bool,
    ) -> Result<(), Error> {
        let manifest = manifest.transpose()?;
        if let Some(manifest) = &manifest {
            manifest.validate()?;
        }
        if alternate != self.alternate {
            self.images.clear();
            self.transfer = None;
            self.missing.clear();
        }
        self.alternate = alternate;
        let current = |image: &Image| {
            manifest.as_ref().is_some_and(|m| {
                m.images.iter().any(|current| {
                    current.key.image_id == image.key.image_id
                        && current.width == image.width
                        && current.height == image.height
                })
            })
        };
        self.images.retain(|_, image| {
            current(&Image {
                key: image.key,
                width: image.width,
                height: image.height,
            })
        });
        if self.transfer.as_ref().is_some_and(|t| !current(&t.image)) {
            self.transfer = None;
        }
        if self.manifest != manifest {
            self.missing.clear();
        }
        self.manifest = manifest;
        self.valid = true;
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> GraphicsSnapshot {
        let Some(manifest) = &self.manifest else {
            return GraphicsSnapshot {
                failure: self.reported,
                ..GraphicsSnapshot::default()
            };
        };
        GraphicsSnapshot {
            failure: self.reported,
            generation: manifest.generation,
            placement_generation: manifest.placement_generation,
            images: manifest
                .images
                .iter()
                .filter_map(|image| self.images.get(&image.key.image_id).cloned())
                .collect(),
            placements: manifest
                .placements
                .iter()
                .filter_map(|p| {
                    self.images.get(&p.image.image_id).map(|image| {
                        let mut placement = p.clone();
                        placement.image = image.key;
                        placement
                    })
                })
                .collect(),
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Return at most one new recoverable failure per unchanged failure class.
    pub(crate) fn failure(&mut self, error: Error) -> bool {
        if self.reported == Some(error) {
            return false;
        }
        self.reported = Some(error);
        true
    }

    pub(crate) fn recovered(&mut self) -> bool {
        let ready = self.valid
            && self.manifest.as_ref().is_none_or(|manifest| {
                manifest
                    .images
                    .iter()
                    .all(|image| self.images.contains_key(&image.key.image_id))
            });
        ready && self.reported.take().is_some()
    }

    pub(crate) fn poll(
        &mut self,
        lease: Lease,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<Result<bool, Error>, ServerError> {
        if self.transfer.is_none() {
            let Some(image) = self
                .manifest
                .as_ref()
                .and_then(|manifest| {
                    manifest.images.iter().find(|image| {
                        self.images
                            .get(&image.key.image_id)
                            .is_none_or(|cached| cached.key != image.key)
                            && !self.missing.contains(&image.key)
                    })
                })
                .cloned()
            else {
                return Ok(Ok(false));
            };
            let length = match image.byte_len() {
                Ok(length) => length,
                Err(error) => return Ok(Err(error)),
            };
            // Include the bounded Vec-to-Arc copy; keep that conservative charge with the image.
            let Some(reservation) = GraphicsReservation::try_acquire(length * 2) else {
                return Ok(Err(Error::Capacity));
            };
            let mut bytes = Vec::new();
            if bytes.try_reserve_exact(length).is_err() {
                return Ok(Err(Error::Capacity));
            }
            self.transfer = Some(Transfer {
                image,
                bytes,
                reservation: Arc::new(reservation),
            });
        }
        let Some(transfer) = &mut self.transfer else {
            return Ok(Ok(false));
        };
        let reply = exchange(&Request::GraphicsChunk {
            lease,
            image: transfer.image.key,
            offset: transfer.bytes.len(),
        })?;
        let Reply::GraphicsChunk(result) = reply else {
            return Err(ServerError::Protocol);
        };
        let chunk = match result {
            Ok(chunk) => chunk,
            Err(error) => {
                self.missing.insert(transfer.image.key);
                self.transfer = None;
                return Ok(Err(error));
            }
        };
        let length = transfer
            .image
            .byte_len()
            .map_err(|_| ServerError::Protocol)?;
        if chunk.image != transfer.image.key
            || chunk.offset != transfer.bytes.len()
            || chunk.bytes.len() != CHUNK_BYTES.min(length - transfer.bytes.len())
        {
            self.missing.insert(transfer.image.key);
            self.transfer = None;
            return Ok(Err(Error::Malformed));
        }
        transfer.bytes.extend_from_slice(&chunk.bytes);
        if transfer.bytes.len() != length {
            return Ok(Ok(false));
        }
        let Some(transfer) = self.transfer.take() else {
            return Ok(Ok(false));
        };
        self.images.insert(
            transfer.image.key.image_id,
            Arc::new(ImageSnapshot {
                key: transfer.image.key,
                width: transfer.image.width,
                height: transfer.image.height,
                rgba: transfer.bytes.into(),
                reservation: Some(transfer.reservation),
            }),
        );
        Ok(Ok(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pane::graphics::{Chunk, Placement};

    fn lease() -> Lease {
        Lease {
            server: [1; 32],
            session: [2; 32],
            control: [3; 32],
            connection: [4; 32],
        }
    }
    fn manifest(width: u32, height: u32) -> Snapshot {
        let image = Image {
            key: ImageKey {
                image_id: 7,
                generation: 1,
            },
            width,
            height,
        };
        Snapshot {
            generation: 1,
            placement_generation: 1,
            images: vec![image.clone()],
            placements: vec![Placement {
                image: image.key,
                placement_id: 3,
                z: 0,
                viewport_col: 1,
                viewport_row: 1,
                cell_offset_x: 0,
                cell_offset_y: 0,
                source_x: 0,
                source_y: 0,
                source_width: width,
                source_height: height,
                destination_width: 26,
                destination_height: 27,
                unicode_placeholder: false,
            }],
        }
    }
    fn reply(request: &Request, total: usize) -> Result<Reply, ServerError> {
        let Request::GraphicsChunk {
            lease: actual,
            image,
            offset,
        } = request
        else {
            panic!("unexpected request")
        };
        assert_eq!(*actual, lease());
        Ok(Reply::GraphicsChunk(Ok(Chunk {
            image: *image,
            offset: *offset,
            bytes: vec![255; CHUNK_BYTES.min(total - offset)],
        })))
    }

    #[test]
    fn graphics_transfer_is_incremental_and_geometry_changes_reuse_completed_pixels() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut graphics = Graphics::default();
        let mut manifest = manifest(384, 256);
        graphics.update(Some(Ok(manifest.clone())), false).unwrap();
        assert_eq!(
            graphics
                .poll(lease(), &mut |request| reply(request, 393216))
                .unwrap(),
            Ok(false)
        );
        assert!(
            graphics.snapshot().placements.is_empty(),
            "partial resources cannot paint"
        );
        assert_eq!(
            graphics
                .poll(lease(), &mut |request| reply(request, 393216))
                .unwrap(),
            Ok(true)
        );
        let completed = graphics.snapshot();
        assert_eq!(completed.images[0].rgba.len(), 393216);
        assert_eq!(completed.placements[0].destination_width, 26);
        manifest.placement_generation += 1;
        manifest.placements[0].viewport_row = -1;
        manifest.placements[0].destination_width = 52;
        graphics.update(Some(Ok(manifest)), false).unwrap();
        graphics
            .poll(lease(), &mut |_| {
                panic!("resize and scroll must not download unchanged pixels")
            })
            .unwrap()
            .unwrap();
        let resized = graphics.snapshot();
        assert!(Arc::ptr_eq(&resized.images[0], &completed.images[0]));
        assert_eq!(resized.placements[0].viewport_row, -1);
        assert_eq!(resized.placements[0].destination_width, 52);
        graphics.clear();
        assert!(graphics.snapshot().images.is_empty());
        assert_eq!(
            completed.images[0].rgba.len(),
            393216,
            "a frozen frame owns bounded residency until released"
        );
    }

    #[test]
    fn invalid_chunks_and_missing_resources_are_recoverable_and_never_published() {
        let _guard = crate::terminal::graphics::test_lock();
        for error in [Error::Missing, Error::Malformed] {
            let mut graphics = Graphics::default();
            graphics.update(Some(Ok(manifest(1, 1))), false).unwrap();
            let result = graphics
                .poll(lease(), &mut |_| {
                    Ok(if error == Error::Missing {
                        Reply::GraphicsChunk(Err(Error::Missing))
                    } else {
                        Reply::GraphicsChunk(Ok(Chunk {
                            image: ImageKey {
                                image_id: 99,
                                generation: 1,
                            },
                            offset: 0,
                            bytes: vec![255; 4],
                        }))
                    })
                })
                .unwrap();
            assert_eq!(result, Err(error));
            assert!(graphics.snapshot().images.is_empty());
            assert!(graphics.snapshot().placements.is_empty());
            assert!(graphics.failure(error));
            assert_eq!(graphics.snapshot().failure, Some(error));
            assert!(!graphics.failure(error));
            assert_eq!(
                graphics
                    .poll(lease(), &mut |_| panic!("failed resource must not spin"))
                    .unwrap(),
                Ok(false)
            );
        }
    }

    #[test]
    fn replacement_manifests_and_active_screens_discard_partial_or_reused_keys() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut graphics = Graphics::default();
        graphics
            .update(Some(Ok(manifest(384, 256))), false)
            .unwrap();
        graphics
            .poll(lease(), &mut |request| reply(request, 393216))
            .unwrap()
            .unwrap();
        let replacement = manifest(1, 1);
        graphics
            .update(Some(Ok(replacement.clone())), false)
            .unwrap();
        assert_eq!(
            graphics
                .poll(lease(), &mut |request| reply(request, 4))
                .unwrap(),
            Ok(true)
        );
        assert_eq!(graphics.snapshot().images[0].rgba.len(), 4);
        graphics.update(Some(Ok(replacement)), true).unwrap();
        assert!(
            graphics.snapshot().images.is_empty(),
            "primary pixels cannot resolve alternate-screen keys"
        );
    }

    #[test]
    fn graphics_admission_uses_the_application_budget_and_recovers_after_release() {
        let _guard = crate::terminal::graphics::test_lock();
        let reservation =
            GraphicsReservation::try_acquire(crate::terminal::graphics::APPLICATION_DECODED_LIMIT)
                .unwrap();
        let mut graphics = Graphics::default();
        graphics.update(Some(Ok(manifest(1, 1))), false).unwrap();
        assert_eq!(
            graphics
                .poll(lease(), &mut |_| panic!(
                    "capacity must be reserved before transfer"
                ))
                .unwrap(),
            Err(Error::Capacity)
        );
        assert!(graphics.failure(Error::Capacity));
        assert_eq!(graphics.snapshot().failure, Some(Error::Capacity));
        drop(reservation);
        assert_eq!(
            graphics
                .poll(lease(), &mut |request| reply(request, 4))
                .unwrap(),
            Ok(true)
        );
        assert!(graphics.recovered());
        assert_eq!(graphics.snapshot().failure, None);
    }
}
