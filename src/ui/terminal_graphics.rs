use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{App, Bounds, ContentMask, Pixels, RenderImage, Window, point, px, size};
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use crate::terminal::{
    ActiveScreenSnapshot, GraphicsSnapshot, ImageKey, ImagePlacementSnapshot, ImageSnapshot,
};

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
    source: Arc<ImageSnapshot>,
    render_image: Arc<RenderImage>,
    width: u32,
    height: u32,
    _reservation: GpuReservation,
}

#[derive(Default)]
pub(crate) struct TerminalGraphicsCache {
    images: HashMap<ImageKey, Arc<CachedImage>>,
    presented: PreparedGraphics,
    staged: Option<StagedGraphics>,
    next_attempt: u64,
    fail_next_sync: bool,
    fail_after_staging: bool,
    injected_rollback: Option<GraphicsRollbackProof>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct GraphicsRollbackProof {
    pub(crate) staged_count: u64,
    pub(crate) staged_bytes: u64,
    pub(crate) rolled_back_count: u64,
    pub(crate) rolled_back_bytes: u64,
}

struct StagedGraphics {
    token: GraphicsAttemptToken,
    additions: HashMap<ImageKey, Arc<CachedImage>>,
    prepared: PreparedGraphics,
    retain: Option<HashSet<ImageKey>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GraphicsAttemptToken {
    id: u64,
    screen: ActiveScreenSnapshot,
    generation: u64,
    placement_generation: u64,
}

pub(crate) struct GraphicsPreparation {
    pub(crate) token: GraphicsAttemptToken,
    pub(crate) graphics: PreparedGraphics,
}

impl TerminalGraphicsCache {
    pub(crate) fn sync(
        &mut self,
        active_screen: ActiveScreenSnapshot,
        snapshot: &GraphicsSnapshot,
        current_window: &mut Window,
        cx: &mut App,
    ) -> Result<GraphicsPreparation, GraphicsResourceError> {
        self.rollback_any_staged(Some(current_window), cx);
        if std::mem::take(&mut self.fail_next_sync) {
            return Err(GraphicsResourceError::Injected);
        }

        if self.presented.matches(active_screen, snapshot) {
            let token = self.next_token(active_screen, snapshot)?;
            self.staged = Some(StagedGraphics {
                token,
                additions: HashMap::new(),
                prepared: self.presented.clone(),
                retain: None,
            });
            return Ok(GraphicsPreparation {
                token,
                graphics: self.presented.clone(),
            });
        }

        let same_screen = self.presented.active_screen() == active_screen;
        let retain = snapshot
            .images
            .iter()
            .map(|image| image.key)
            .collect::<HashSet<_>>();
        if retain.len() != snapshot.images.len()
            || snapshot
                .placements
                .iter()
                .any(|placement| !retain.contains(&placement.image))
        {
            return Err(GraphicsResourceError::MissingImage);
        }
        let token = self.next_token(active_screen, snapshot)?;

        let mut additions = HashMap::new();
        for image in snapshot.images.iter() {
            let reusable = same_screen
                .then(|| self.images.get(&image.key))
                .flatten()
                .is_some_and(|cached| cached.matches(image));
            if !reusable {
                let cached = upload_image(Arc::clone(image)).inspect_err(|_| {
                    drop_staged_images(std::mem::take(&mut additions), current_window, cx);
                })?;
                additions.insert(image.key, Arc::new(cached));
            }
        }

        let mut placements = Vec::with_capacity(snapshot.placements.len());
        for placement in snapshot.placements.iter() {
            let image = additions.get(&placement.image).or_else(|| {
                same_screen
                    .then(|| self.images.get(&placement.image))
                    .flatten()
            });
            let Some(image) = image else {
                drop(placements);
                drop_staged_images(additions, current_window, cx);
                return Err(GraphicsResourceError::MissingImage);
            };
            placements.push(PreparedPlacement {
                placement: placement.clone(),
                image: Arc::clone(image),
            });
        }
        let prepared = PreparedGraphics::new(active_screen, snapshot, Arc::from(placements));
        self.staged = Some(StagedGraphics {
            token,
            additions,
            prepared: prepared.clone(),
            retain: Some(retain),
        });
        if self.fail_after_staging && !self.staged_additions_are_empty() {
            self.fail_after_staging = false;
            let proof = self.staged_rollback_proof();
            self.rollback(token, Some(current_window), cx);
            self.injected_rollback = proof;
            return Err(GraphicsResourceError::InjectedAfterStaging);
        }
        Ok(GraphicsPreparation {
            token,
            graphics: prepared,
        })
    }

    fn next_token(
        &mut self,
        active_screen: ActiveScreenSnapshot,
        snapshot: &GraphicsSnapshot,
    ) -> Result<GraphicsAttemptToken, GraphicsResourceError> {
        self.next_attempt = self
            .next_attempt
            .checked_add(1)
            .ok_or(GraphicsResourceError::AttemptExhausted)?;
        Ok(GraphicsAttemptToken {
            id: self.next_attempt,
            screen: active_screen,
            generation: snapshot.generation,
            placement_generation: snapshot.placement_generation,
        })
    }

    pub(crate) fn last_presented(&self) -> PreparedGraphics {
        self.presented.clone()
    }

    pub(crate) fn mark_presented(&mut self, token: GraphicsAttemptToken, cx: &mut App) -> bool {
        let Some(mut staged) = self.staged.take() else {
            return false;
        };
        if staged.token != token {
            self.staged = Some(staged);
            return false;
        }
        self.presented = staged.prepared;
        for (key, image) in staged.additions.drain() {
            if let Some(replaced) = self.images.insert(key, image) {
                cx.drop_image(Arc::clone(&replaced.render_image), None);
            }
        }
        if let Some(retain) = staged.retain {
            self.images.retain(|key, image| {
                let keep = retain.contains(key);
                if !keep {
                    cx.drop_image(Arc::clone(&image.render_image), None);
                }
                keep
            });
        }
        true
    }

    pub(crate) fn rollback(
        &mut self,
        token: GraphicsAttemptToken,
        current_window: Option<&mut Window>,
        cx: &mut App,
    ) -> bool {
        if self.staged.as_ref().map(|staged| staged.token) != Some(token) {
            return false;
        }
        self.rollback_any_staged(current_window, cx);
        true
    }

    fn rollback_any_staged(&mut self, mut current_window: Option<&mut Window>, cx: &mut App) {
        let Some(staged) = self.staged.take() else {
            return;
        };
        drop(staged.prepared);
        for (_, image) in staged.additions {
            cx.drop_image(
                Arc::clone(&image.render_image),
                current_window.as_deref_mut(),
            );
        }
    }

    pub(crate) fn clear(&mut self, cx: &mut App) {
        self.rollback_any_staged(None, cx);
        for (_, image) in self.images.drain() {
            cx.drop_image(Arc::clone(&image.render_image), None);
        }
        self.presented = PreparedGraphics::default();
    }

    pub(crate) fn fail_next_sync(&mut self) {
        self.fail_next_sync = true;
    }

    pub(crate) fn fail_after_staging(&mut self) {
        self.fail_after_staging = true;
        self.injected_rollback = None;
    }

    pub(crate) fn take_injected_rollback(&mut self) -> Option<GraphicsRollbackProof> {
        self.injected_rollback.take()
    }

    fn staged_additions_are_empty(&self) -> bool {
        self.staged
            .as_ref()
            .is_none_or(|staged| staged.additions.is_empty())
    }

    fn staged_rollback_proof(&self) -> Option<GraphicsRollbackProof> {
        let staged = self.staged.as_ref()?;
        let count = u64::try_from(staged.additions.len()).ok()?;
        let bytes = staged.additions.values().try_fold(0_u64, |total, image| {
            total.checked_add(u64::try_from(image._reservation.0).ok()?)
        })?;
        (count > 0 && bytes > 0).then_some(GraphicsRollbackProof {
            staged_count: count,
            staged_bytes: bytes,
            rolled_back_count: count,
            rolled_back_bytes: bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn cached_image_keys(&self) -> Vec<ImageKey> {
        let mut keys = self.images.keys().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        keys
    }

    #[cfg(test)]
    pub(crate) fn staged_image_keys(&self) -> Vec<ImageKey> {
        let mut keys = self
            .staged
            .iter()
            .flat_map(|staged| staged.additions.keys())
            .copied()
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys
    }

    #[cfg(test)]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.images
            .values()
            .map(|image| image._reservation.0)
            .sum::<usize>()
            + self
                .staged
                .iter()
                .flat_map(|staged| staged.additions.values())
                .map(|image| image._reservation.0)
                .sum::<usize>()
    }
}

impl CachedImage {
    fn matches(&self, candidate: &Arc<ImageSnapshot>) -> bool {
        self.source.key == candidate.key
            && self.source.width == candidate.width
            && self.source.height == candidate.height
            && Arc::ptr_eq(&self.source.rgba, &candidate.rgba)
    }
}

fn drop_staged_images(
    images: HashMap<ImageKey, Arc<CachedImage>>,
    window: &mut Window,
    cx: &mut App,
) {
    for (_, image) in images {
        cx.drop_image(Arc::clone(&image.render_image), Some(window));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphicsResourceError {
    Capacity,
    InvalidImage,
    MissingImage,
    AttemptExhausted,
    Paint,
    Injected,
    InjectedAfterStaging,
}

fn upload_image(image: Arc<ImageSnapshot>) -> Result<CachedImage, GraphicsResourceError> {
    let reservation =
        GpuReservation::acquire(image.rgba.len()).ok_or(GraphicsResourceError::Capacity)?;
    let width = image.width;
    let height = image.height;
    let mut bgra = image.rgba.to_vec();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(image.width, image.height, bgra)
        .ok_or(GraphicsResourceError::InvalidImage)?;
    let render_image = Arc::new(RenderImage::new(smallvec![Frame::new(buffer)]));
    Ok(CachedImage {
        source: image,
        render_image,
        width,
        height,
        _reservation: reservation,
    })
}

#[derive(Clone)]
pub(crate) struct PreparedGraphics {
    inner: Rc<PreparedGraphicsInner>,
}

struct PreparedGraphicsInner {
    active_screen: ActiveScreenSnapshot,
    generation: u64,
    placement_generation: u64,
    images: Arc<[Arc<ImageSnapshot>]>,
    placement_snapshot: Arc<[ImagePlacementSnapshot]>,
    placements: Arc<[PreparedPlacement]>,
    paint_plan: RefCell<Option<CachedGraphicsPaintPlan>>,
    #[cfg(test)]
    paint_plan_builds: std::cell::Cell<usize>,
}

struct PreparedPlacement {
    placement: ImagePlacementSnapshot,
    image: Arc<CachedImage>,
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
    _resource: Arc<CachedImage>,
}

#[derive(Clone, Default)]
pub(crate) struct GraphicsPaintPlan {
    paints: Arc<[ImagePaint]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GraphicsGeometryKey {
    grid_bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    scale_factor_bits: u32,
}

struct CachedGraphicsPaintPlan {
    key: GraphicsGeometryKey,
    plan: GraphicsPaintPlan,
}

impl Default for PreparedGraphics {
    fn default() -> Self {
        Self {
            inner: Rc::new(PreparedGraphicsInner {
                active_screen: ActiveScreenSnapshot::Primary,
                generation: 0,
                placement_generation: 0,
                images: Arc::from([]),
                placement_snapshot: Arc::from([]),
                placements: Arc::from([]),
                paint_plan: RefCell::new(None),
                #[cfg(test)]
                paint_plan_builds: std::cell::Cell::new(0),
            }),
        }
    }
}

impl PreparedGraphics {
    fn new(
        active_screen: ActiveScreenSnapshot,
        snapshot: &GraphicsSnapshot,
        placements: Arc<[PreparedPlacement]>,
    ) -> Self {
        Self {
            inner: Rc::new(PreparedGraphicsInner {
                active_screen,
                generation: snapshot.generation,
                placement_generation: snapshot.placement_generation,
                images: Arc::clone(&snapshot.images),
                placement_snapshot: Arc::clone(&snapshot.placements),
                placements,
                paint_plan: RefCell::new(None),
                #[cfg(test)]
                paint_plan_builds: std::cell::Cell::new(0),
            }),
        }
    }

    fn active_screen(&self) -> ActiveScreenSnapshot {
        self.inner.active_screen
    }

    fn matches(&self, active_screen: ActiveScreenSnapshot, snapshot: &GraphicsSnapshot) -> bool {
        self.inner.active_screen == active_screen
            && self.inner.generation == snapshot.generation
            && self.inner.placement_generation == snapshot.placement_generation
            && image_snapshots_match(&self.inner.images, &snapshot.images)
            && (Arc::ptr_eq(&self.inner.placement_snapshot, &snapshot.placements)
                || self.inner.placement_snapshot.as_ref() == snapshot.placements.as_ref())
    }

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
        let key = GraphicsGeometryKey {
            grid_bounds,
            cell_width,
            line_height,
            scale_factor_bits: scale.to_bits(),
        };
        if let Some(cached) = self.inner.paint_plan.borrow().as_ref()
            && cached.key == key
        {
            return cached.plan.clone();
        }

        let paints = self
            .inner
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
                        source_scale_x * prepared.image.width as f32,
                        source_scale_y * prepared.image.height as f32,
                    ),
                );
                Some(ImagePaint {
                    layer: layer_for_z(placement.z),
                    destination,
                    full_image,
                    image: Arc::clone(&prepared.image.render_image),
                    _resource: Arc::clone(&prepared.image),
                })
            })
            .collect::<Vec<_>>();
        let plan = GraphicsPaintPlan {
            paints: Arc::from(paints),
        };
        *self.inner.paint_plan.borrow_mut() = Some(CachedGraphicsPaintPlan {
            key,
            plan: plan.clone(),
        });
        #[cfg(test)]
        self.inner
            .paint_plan_builds
            .set(self.inner.paint_plan_builds.get().saturating_add(1));
        plan
    }

    #[cfg(test)]
    fn paint_plan_builds(&self) -> usize {
        self.inner.paint_plan_builds.get()
    }

    #[cfg(test)]
    fn shares_identity_with(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

fn image_snapshots_match(
    cached: &Arc<[Arc<ImageSnapshot>]>,
    candidate: &Arc<[Arc<ImageSnapshot>]>,
) -> bool {
    Arc::ptr_eq(cached, candidate)
        || (cached.len() == candidate.len()
            && cached
                .iter()
                .zip(candidate.iter())
                .all(|(cached, candidate)| {
                    Arc::ptr_eq(cached, candidate)
                        || (cached.key == candidate.key
                            && cached.width == candidate.width
                            && cached.height == candidate.height
                            && Arc::ptr_eq(&cached.rgba, &candidate.rgba))
                }))
}

impl GraphicsPaintPlan {
    #[cfg(test)]
    fn shares_paints_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.paints, &other.paints)
    }

    pub(crate) fn image_count(&self) -> usize {
        self.paints.len()
    }

    pub(crate) fn paint_layer(
        &self,
        layer: GraphicsLayer,
        window: &mut Window,
    ) -> Result<(), GraphicsResourceError> {
        let mut failed = false;
        for paint in self.paints.iter().filter(|paint| paint.layer == layer) {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: paint.destination,
                }),
                |window| {
                    if window
                        .paint_image(
                            paint.full_image,
                            Default::default(),
                            Arc::clone(&paint.image),
                            0,
                            false,
                        )
                        .is_err()
                    {
                        failed = true;
                    }
                },
            );
        }
        if failed {
            Err(GraphicsResourceError::Paint)
        } else {
            Ok(())
        }
    }

    pub(crate) fn preflight_layer(
        &self,
        layer: GraphicsLayer,
        window: &mut Window,
    ) -> Result<(), GraphicsResourceError> {
        for paint in self.paints.iter().filter(|paint| paint.layer == layer) {
            window
                .paint_image(
                    Bounds::new(point(px(0.0), px(0.0)), size(px(0.0), px(0.0))),
                    Default::default(),
                    Arc::clone(&paint.image),
                    0,
                    false,
                )
                .map_err(|_| GraphicsResourceError::Paint)?;
        }
        Ok(())
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

    fn prepared_graphics() -> PreparedGraphics {
        let image_snapshot = Arc::new(ImageSnapshot {
            key: ImageKey {
                image_id: 1,
                generation: 2,
            },
            width: 100,
            height: 80,
            rgba: Arc::from(vec![0; 100 * 80 * 4]),
        });
        let image = Arc::new(upload_image(Arc::clone(&image_snapshot)).unwrap());
        let placement = ImagePlacementSnapshot {
            image: image_snapshot.key,
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
        };
        let snapshot = GraphicsSnapshot {
            generation: 3,
            placement_generation: 1,
            images: Arc::from([image_snapshot]),
            placements: Arc::from([placement.clone()]),
        };
        PreparedGraphics::new(
            ActiveScreenSnapshot::Primary,
            &snapshot,
            Arc::from([PreparedPlacement { placement, image }]),
        )
    }

    fn geometry() -> (Bounds<Pixels>, Pixels, Pixels) {
        (
            Bounds::new(point(px(5.0), px(7.0)), size(px(800.0), px(400.0))),
            px(10.0),
            px(20.0),
        )
    }

    fn token(id: u64) -> GraphicsAttemptToken {
        GraphicsAttemptToken {
            id,
            screen: ActiveScreenSnapshot::Primary,
            generation: 1,
            placement_generation: 1,
        }
    }

    #[test]
    fn z_layers_use_the_protocol_thresholds() {
        assert_eq!(layer_for_z(i32::MIN), GraphicsLayer::BelowBackground);
        assert_eq!(layer_for_z(i32::MIN / 2), GraphicsLayer::BelowText);
        assert_eq!(layer_for_z(-1), GraphicsLayer::BelowText);
        assert_eq!(layer_for_z(0), GraphicsLayer::AboveText);
    }

    #[test]
    fn source_crop_transforms_full_image_under_destination_mask() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();

        let plan = prepared.paint_plan(bounds, cell_width, line_height, 2.0);
        let paint = &plan.paints[0];
        assert_eq!(paint.destination.origin, point(px(27.0), px(28.0)));
        assert_eq!(paint.destination.size, size(px(50.0), px(40.0)));
        assert_eq!(paint.full_image.origin, point(px(17.0), px(8.0)));
        assert_eq!(paint.full_image.size, size(px(100.0), px(80.0)));
    }

    #[test]
    fn unchanged_geometry_reuses_the_arc_backed_paint_plan() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();

        let first = prepared.paint_plan(bounds, cell_width, line_height, 2.0);
        let second = prepared.paint_plan(bounds, cell_width, line_height, 2.0);

        assert!(first.shares_paints_with(&second));
        assert_eq!(prepared.paint_plan_builds(), 1);
    }

    #[test]
    fn invalid_backing_scales_share_the_canonical_one_x_plan() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let first = prepared.paint_plan(bounds, cell_width, line_height, f32::NAN);

        for scale in [f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0, 1.0] {
            let next = prepared.paint_plan(bounds, cell_width, line_height, scale);
            assert!(first.shares_paints_with(&next));
        }
        assert_eq!(prepared.paint_plan_builds(), 1);
    }

    #[test]
    fn backing_scale_change_rebuilds_geometry_without_replacing_the_image() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let one_x = prepared.paint_plan(bounds, cell_width, line_height, 1.0);
        let two_x = prepared.paint_plan(bounds, cell_width, line_height, 2.0);

        assert!(!one_x.shares_paints_with(&two_x));
        assert!(Arc::ptr_eq(&one_x.paints[0].image, &two_x.paints[0].image));
        assert_eq!(prepared.paint_plan_builds(), 2);
    }

    #[test]
    fn full_grid_bounds_are_geometry_cache_inputs() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let first = prepared.paint_plan(bounds, cell_width, line_height, 2.0);
        let resized = Bounds::new(bounds.origin, size(px(801.0), bounds.size.height));
        let second = prepared.paint_plan(resized, cell_width, line_height, 2.0);

        assert!(!first.shares_paints_with(&second));
        assert_eq!(prepared.paint_plan_builds(), 2);
    }

    #[test]
    fn prepared_graphics_clones_share_the_geometry_cache() {
        let prepared = prepared_graphics();

        assert!(prepared.shares_identity_with(&prepared.clone()));
    }

    #[test]
    fn upload_converts_rgba_to_gpui_bgra_once() {
        let snapshot = Arc::new(ImageSnapshot {
            key: ImageKey {
                image_id: 1,
                generation: 1,
            },
            width: 1,
            height: 1,
            rgba: Arc::from([10, 20, 30, 40]),
        });

        let uploaded = upload_image(snapshot).unwrap();

        assert_eq!(
            uploaded.render_image.as_bytes(0).unwrap(),
            &[30, 20, 10, 40]
        );
    }

    #[gpui::test]
    fn stale_attempt_cannot_commit_or_remove_the_current_stage(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut cache = TerminalGraphicsCache {
                staged: Some(StagedGraphics {
                    token: token(2),
                    additions: HashMap::new(),
                    prepared: PreparedGraphics::default(),
                    retain: Some(HashSet::new()),
                }),
                ..Default::default()
            };

            assert!(!cache.mark_presented(token(1), cx));
            assert_eq!(
                cache.staged.as_ref().map(|staged| staged.token),
                Some(token(2))
            );
        });
    }
}
