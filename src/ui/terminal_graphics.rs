use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{App, Bounds, ContentMask, Pixels, RenderImage, Window, point, px, size};
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use crate::terminal::{GraphicsSnapshot, ImageKey, ImagePlacementSnapshot, ImageSnapshot};

const GPU_CACHE_LIMIT: usize = 384 * 1024 * 1024;
const VERY_NEGATIVE_Z_LIMIT: i32 = i32::MIN / 2;

static GPU_CACHE_BYTES: AtomicUsize = AtomicUsize::new(0);

struct GpuReservation(usize);

impl GpuReservation {
    fn acquire(bytes: usize) -> Option<Self> {
        GPU_CACHE_BYTES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= GPU_CACHE_LIMIT)
            })
            .ok()
            .map(|_| Self(bytes))
    }
}

impl Drop for GpuReservation {
    fn drop(&mut self) {
        GPU_CACHE_BYTES.fetch_sub(self.0, Ordering::AcqRel);
    }
}

struct CachedImage {
    render_image: Arc<RenderImage>,
    width: u32,
    height: u32,
    _reservation: GpuReservation,
}

#[derive(Default)]
pub(crate) struct TerminalGraphicsCache {
    images: HashMap<ImageKey, CachedImage>,
}

impl TerminalGraphicsCache {
    pub(crate) fn sync(
        &mut self,
        snapshot: &GraphicsSnapshot,
        window: &mut Window,
        cx: &mut App,
    ) -> PreparedGraphics {
        let current = snapshot
            .images
            .iter()
            .map(|image| image.key)
            .collect::<HashSet<_>>();
        let stale = self
            .images
            .keys()
            .filter(|key| !current.contains(key))
            .copied()
            .collect::<Vec<_>>();
        for key in stale {
            if let Some(image) = self.images.remove(&key) {
                cx.drop_image(image.render_image, Some(window));
            }
        }

        for image in snapshot.images.iter() {
            if self.images.contains_key(&image.key) {
                continue;
            }
            if let Some(cached) = upload_image(image) {
                self.images.insert(image.key, cached);
            }
        }

        let placements = snapshot
            .placements
            .iter()
            .filter_map(|placement| {
                let image = self.images.get(&placement.image)?;
                Some(PreparedPlacement {
                    placement: placement.clone(),
                    image: Arc::clone(&image.render_image),
                    image_width: image.width,
                    image_height: image.height,
                })
            })
            .collect();
        PreparedGraphics { placements }
    }

    pub(crate) fn clear(&mut self, cx: &mut App) {
        for (_, image) in self.images.drain() {
            cx.drop_image(image.render_image, None);
        }
    }
}

fn upload_image(image: &ImageSnapshot) -> Option<CachedImage> {
    let reservation = GpuReservation::acquire(image.rgba.len())?;
    let mut bgra = image.rgba.to_vec();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(image.width, image.height, bgra)?;
    let render_image = Arc::new(RenderImage::new(smallvec![Frame::new(buffer)]));
    Some(CachedImage {
        render_image,
        width: image.width,
        height: image.height,
        _reservation: reservation,
    })
}

#[derive(Clone)]
pub(crate) struct PreparedGraphics {
    placements: Vec<PreparedPlacement>,
}

#[derive(Clone)]
struct PreparedPlacement {
    placement: ImagePlacementSnapshot,
    image: Arc<RenderImage>,
    image_width: u32,
    image_height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphicsLayer {
    BelowBackground,
    BelowText,
    AboveText,
}

#[derive(Clone)]
pub(crate) struct ImagePaint {
    pub(crate) layer: GraphicsLayer,
    pub(crate) destination: Bounds<Pixels>,
    pub(crate) full_image: Bounds<Pixels>,
    pub(crate) image: Arc<RenderImage>,
}

#[derive(Default)]
pub(crate) struct GraphicsPaintPlan {
    paints: Vec<ImagePaint>,
}

impl PreparedGraphics {
    pub(crate) fn paint_plan(
        &self,
        grid_bounds: Bounds<Pixels>,
        cell_width: Pixels,
        line_height: Pixels,
        scale_factor: f32,
    ) -> GraphicsPaintPlan {
        let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let paints = self
            .placements
            .iter()
            .filter_map(|prepared| {
                let placement = &prepared.placement;
                if placement.source_width == 0
                    || placement.source_height == 0
                    || placement.destination_width == 0
                    || placement.destination_height == 0
                {
                    return None;
                }
                let destination_width = px(placement.destination_width as f32 / scale);
                let destination_height = px(placement.destination_height as f32 / scale);
                let destination = Bounds::new(
                    point(
                        grid_bounds.left()
                            + cell_width * placement.viewport_col as f32
                            + px(placement.cell_offset_x as f32 / scale),
                        grid_bounds.top()
                            + line_height * placement.viewport_row as f32
                            + px(placement.cell_offset_y as f32 / scale),
                    ),
                    size(destination_width, destination_height),
                );
                let source_scale_x = destination_width / placement.source_width as f32;
                let source_scale_y = destination_height / placement.source_height as f32;
                let full_image = Bounds::new(
                    point(
                        destination.left() - source_scale_x * placement.source_x as f32,
                        destination.top() - source_scale_y * placement.source_y as f32,
                    ),
                    size(
                        source_scale_x * prepared.image_width as f32,
                        source_scale_y * prepared.image_height as f32,
                    ),
                );
                Some(ImagePaint {
                    layer: layer_for_z(placement.z),
                    destination,
                    full_image,
                    image: Arc::clone(&prepared.image),
                })
            })
            .collect();
        GraphicsPaintPlan { paints }
    }
}

impl GraphicsPaintPlan {
    pub(crate) fn paint_layer(&self, layer: GraphicsLayer, window: &mut Window) {
        for paint in self.paints.iter().filter(|paint| paint.layer == layer) {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: paint.destination,
                }),
                |window| {
                    if let Err(error) = window.paint_image(
                        paint.full_image,
                        Default::default(),
                        Arc::clone(&paint.image),
                        0,
                        false,
                    ) {
                        eprintln!("failed to paint terminal graphic: {error:#}");
                    }
                },
            );
        }
    }
}

const fn layer_for_z(z: i32) -> GraphicsLayer {
    if z < VERY_NEGATIVE_Z_LIMIT {
        GraphicsLayer::BelowBackground
    } else if z < 0 {
        GraphicsLayer::BelowText
    } else {
        GraphicsLayer::AboveText
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_layers_use_the_protocol_thresholds() {
        assert_eq!(layer_for_z(i32::MIN), GraphicsLayer::BelowBackground);
        assert_eq!(layer_for_z(i32::MIN / 2), GraphicsLayer::BelowText);
        assert_eq!(layer_for_z(-1), GraphicsLayer::BelowText);
        assert_eq!(layer_for_z(0), GraphicsLayer::AboveText);
    }

    #[test]
    fn source_crop_transforms_full_image_under_destination_mask() {
        let image = Arc::new(RenderImage::new(smallvec![Frame::new(RgbaImage::new(
            100, 80
        ))]));
        let prepared = PreparedGraphics {
            placements: vec![PreparedPlacement {
                placement: ImagePlacementSnapshot {
                    image: ImageKey {
                        image_id: 1,
                        generation: 2,
                    },
                    placement_id: 3,
                    z: 0,
                    viewport_col: 2,
                    viewport_row: 1,
                    cell_offset_x: 4,
                    cell_offset_y: 2,
                    source_x: 10,
                    source_y: 20,
                    source_width: 50,
                    source_height: 40,
                    destination_width: 100,
                    destination_height: 80,
                    unicode_placeholder: false,
                },
                image,
                image_width: 100,
                image_height: 80,
            }],
        };

        let plan = prepared.paint_plan(
            Bounds::new(point(px(5.0), px(7.0)), size(px(800.0), px(400.0))),
            px(10.0),
            px(20.0),
            2.0,
        );
        let paint = &plan.paints[0];
        assert_eq!(paint.destination.origin, point(px(27.0), px(28.0)));
        assert_eq!(paint.destination.size, size(px(50.0), px(40.0)));
        assert_eq!(paint.full_image.origin, point(px(17.0), px(8.0)));
        assert_eq!(paint.full_image.size, size(px(100.0), px(80.0)));
    }

    #[test]
    fn upload_converts_rgba_to_gpui_bgra_once() {
        let snapshot = ImageSnapshot {
            key: ImageKey {
                image_id: 1,
                generation: 1,
            },
            width: 1,
            height: 1,
            rgba: Arc::from([10, 20, 30, 40]),
        };

        let uploaded = upload_image(&snapshot).unwrap();

        assert_eq!(
            uploaded.render_image.as_bytes(0).unwrap(),
            &[30, 20, 10, 40]
        );
    }
}
