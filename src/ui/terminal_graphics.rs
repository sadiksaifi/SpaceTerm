use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{App, Bounds, ContentMask, Pixels, RenderImage, Size, Window, point, px, size};
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use crate::terminal::geometry::{BackingScale, LogicalCellSize, backing_cell_size};
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
    #[cfg(test)]
    fail_next_sync: bool,
    #[cfg(test)]
    fail_after_staging: bool,
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
        #[cfg(test)]
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
            #[cfg(test)]
            if std::mem::take(&mut self.fail_after_staging) {
                self.rollback(token, Some(current_window), cx);
                return Err(GraphicsResourceError::InjectedAfterStaging);
            }
            return Ok(GraphicsPreparation {
                token,
                graphics: self.presented.clone(),
            });
        }

        let same_screen = self.presented.active_screen() == active_screen;
        let available = snapshot
            .images
            .iter()
            .map(|image| image.key)
            .collect::<HashSet<_>>();
        if available.len() != snapshot.images.len()
            || snapshot
                .placements
                .iter()
                .any(|placement| !available.contains(&placement.image))
        {
            return Err(GraphicsResourceError::MissingImage);
        }
        let token = self.next_token(active_screen, snapshot)?;
        let retain = snapshot
            .placements
            .iter()
            .map(|placement| placement.image)
            .collect::<HashSet<_>>();

        let mut additions = HashMap::new();
        for image in snapshot
            .images
            .iter()
            .filter(|image| retain.contains(&image.key))
        {
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
        #[cfg(test)]
        if std::mem::take(&mut self.fail_after_staging) {
            self.rollback(token, Some(current_window), cx);
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
            if self.images.is_empty() {
                self.images = HashMap::new();
            }
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
        for (_, image) in std::mem::take(&mut self.images) {
            cx.drop_image(Arc::clone(&image.render_image), None);
        }
        self.presented = PreparedGraphics::default();
    }

    #[cfg(test)]
    pub(crate) fn fail_next_sync(&mut self) {
        self.fail_next_sync = true;
    }

    #[cfg(test)]
    pub(crate) fn fail_after_staging(&mut self) {
        self.fail_after_staging = true;
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
    #[cfg(test)]
    Injected,
    #[cfg(test)]
    InjectedAfterStaging,
}

/// Uploads the image with its edge texels repeated once on every side.
///
/// GPUI samples atlas tiles bilinearly and packs them without padding. A magnified image would
/// otherwise blend its edges with neighbouring tiles, which can hold another Pane's image or
/// texels left by an image that has since been removed. The repeated border lies outside the
/// destination mask, so the visible edge samples only the image's own texels.
fn upload_image(image: Arc<ImageSnapshot>) -> Result<CachedImage, GraphicsResourceError> {
    let width = image.width;
    let height = image.height;
    let (columns, rows) = (width as usize, height as usize);
    if columns == 0 || rows == 0 || image.rgba.len() != columns * rows * 4 {
        return Err(GraphicsResourceError::InvalidImage);
    }
    let padded_width = width
        .checked_add(2)
        .ok_or(GraphicsResourceError::InvalidImage)?;
    let padded_height = height
        .checked_add(2)
        .ok_or(GraphicsResourceError::InvalidImage)?;
    let padded_bytes = (columns + 2)
        .checked_mul(rows + 2)
        .and_then(|texels| texels.checked_mul(4))
        .ok_or(GraphicsResourceError::InvalidImage)?;
    let reservation =
        GpuReservation::acquire(padded_bytes).ok_or(GraphicsResourceError::Capacity)?;
    let source = image.rgba.as_chunks::<4>().0;
    let mut bgra = Vec::with_capacity(padded_bytes);
    for row in 0..rows + 2 {
        let row = row.saturating_sub(1).min(rows - 1);
        for column in 0..columns + 2 {
            let [red, green, blue, alpha] =
                source[row * columns + column.saturating_sub(1).min(columns - 1)];
            bgra.extend_from_slice(&[blue, green, red, alpha]);
        }
    }
    let buffer = RgbaImage::from_raw(padded_width, padded_height, bgra)
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
    /// The uploaded image with its repeated edge texels, drawn under the `destination` mask.
    pub(crate) padded_image: Bounds<Pixels>,
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
    measured_cell: (u32, u32),
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

    /// Places images on the drawn grid. `cell_width` and `line_height` are the drawn cell;
    /// `nominal_cell` is the cell the emulator measured in whole device pixels, which image
    /// placements are sized in.
    pub(crate) fn paint_plan(
        &self,
        grid_bounds: Bounds<Pixels>,
        cell_width: Pixels,
        line_height: Pixels,
        nominal_cell: Size<Pixels>,
        scale_factor: f32,
    ) -> GraphicsPaintPlan {
        let scale = BackingScale::new(scale_factor).unwrap_or(BackingScale::ONE);
        let measured = backing_cell_size(
            LogicalCellSize::new(
                f32::from(nominal_cell.width),
                f32::from(nominal_cell.height),
            ),
            scale,
        );
        let key = GraphicsGeometryKey {
            grid_bounds,
            cell_width,
            line_height,
            measured_cell: (measured.width, measured.height),
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
                // One measured device pixel spans this much of the drawn grid.
                let column_pixel = cell_width / measured.width as f32;
                let row_pixel = line_height / measured.height as f32;
                let destination_width = column_pixel * placement.destination_width as f32;
                let destination_height = row_pixel * placement.destination_height as f32;
                let destination = Bounds::new(
                    point(
                        grid_bounds.left()
                            + cell_width * placement.viewport_col as f32
                            + column_pixel * placement.cell_offset_x as f32,
                        grid_bounds.top()
                            + line_height * placement.viewport_row as f32
                            + row_pixel * placement.cell_offset_y as f32,
                    ),
                    size(destination_width, destination_height),
                );
                let source_scale_x = destination_width / placement.source_width as f32;
                let source_scale_y = destination_height / placement.source_height as f32;
                let padded_image = Bounds::new(
                    point(
                        destination.left() - source_scale_x * (placement.source_x as f32 + 1.0),
                        destination.top() - source_scale_y * (placement.source_y as f32 + 1.0),
                    ),
                    size(
                        source_scale_x * (prepared.image.width as f32 + 2.0),
                        source_scale_y * (prepared.image.height as f32 + 2.0),
                    ),
                );
                Some(ImagePaint {
                    layer: layer_for_z(placement.z),
                    destination,
                    padded_image,
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
                            paint.padded_image,
                            paint.padded_image,
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
    use gpui::AppContext as _;
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingImageAtlas {
        inner: gpui::HeadlessAtlas,
        image_lookups: AtomicUsize,
        failed_image: gpui::ImageId,
    }

    impl gpui::PlatformAtlas for FailingImageAtlas {
        fn get_or_insert_with<'a>(
            &self,
            key: gpui::AtlasKey,
            build: &mut dyn FnMut() -> anyhow::Result<
                Option<(gpui::Size<gpui::DevicePixels>, Cow<'a, [u8]>)>,
            >,
        ) -> anyhow::Result<Option<gpui::AtlasTile>> {
            if let gpui::AtlasKey::Image(params) = &key {
                self.image_lookups.fetch_add(1, Ordering::Relaxed);
                if params.image_id == self.failed_image {
                    anyhow::bail!("injected image atlas failure");
                }
            }
            self.inner.get_or_insert_with(key, build)
        }

        fn remove(&self, key: &gpui::AtlasKey) {
            self.inner.remove(key);
        }

        fn contains(&self, key: &gpui::AtlasKey) -> bool {
            self.inner.contains(key)
        }
    }

    struct FailingImageRenderer(Arc<FailingImageAtlas>);

    impl gpui::PlatformHeadlessRenderer for FailingImageRenderer {
        fn render_scene_to_image(
            &mut self,
            _scene: &gpui::Scene,
            _size: gpui::Size<gpui::DevicePixels>,
        ) -> anyhow::Result<RgbaImage> {
            Ok(RgbaImage::new(1, 1))
        }

        fn render_scene(
            &mut self,
            _scene: &gpui::Scene,
            _size: gpui::Size<gpui::DevicePixels>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn sprite_atlas(&self) -> Arc<dyn gpui::PlatformAtlas> {
            self.0.clone()
        }
    }

    struct FailingImagePaint {
        plan: GraphicsPaintPlan,
        result: Arc<std::sync::Mutex<Option<Result<(), GraphicsResourceError>>>>,
    }

    impl gpui::Render for FailingImagePaint {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            let plan = self.plan.clone();
            let result = Arc::clone(&self.result);
            gpui::canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    let outcome = window.with_content_mask(
                        Some(ContentMask {
                            bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(0.0), px(0.0))),
                        }),
                        |window| plan.paint_layer(GraphicsLayer::AboveText, window),
                    );
                    if outcome.is_err() {
                        window.paint_quad(gpui::fill(
                            Bounds::new(point(px(0.0), px(0.0)), size(px(40.0), px(20.0))),
                            gpui::rgba(0x22_33_44_ff),
                        ));
                    }
                    *result.lock().unwrap() = Some(outcome);
                },
            )
        }
    }

    fn prepared_graphics() -> PreparedGraphics {
        let image_snapshot = Arc::new(ImageSnapshot {
            key: ImageKey {
                image_id: 1,
                generation: 2,
            },
            width: 100,
            height: 80,
            rgba: Arc::from(vec![0; 100 * 80 * 4]),
            reservation: None,
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
            failure: None,
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

    #[gpui::test]
    fn server_graphics_reach_the_desktop_image_atlas_and_paint_measured_geometry(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::server_graphics::Graphics;
        use gpui::{Bounds, point, px, size};
        use spaceterm_server::pane::{
            Lease, Reply, Request,
            graphics::{Chunk, Image, ImageKey, Placement, Snapshot},
        };
        let _guard = crate::terminal::graphics::test_lock();
        let mut graphics = Graphics::default();
        let key = ImageKey {
            image_id: 7,
            generation: 1,
        };
        let manifest = Snapshot {
            generation: 1,
            placement_generation: 1,
            images: vec![Image {
                key,
                width: 1,
                height: 1,
            }],
            placements: vec![Placement {
                image: key,
                placement_id: 3,
                z: 0,
                viewport_col: 1,
                viewport_row: 1,
                cell_offset_x: 0,
                cell_offset_y: 0,
                source_x: 0,
                source_y: 0,
                source_width: 1,
                source_height: 1,
                destination_width: 26,
                destination_height: 27,
                unicode_placeholder: false,
            }],
        };
        graphics.update(Some(Ok(manifest)), false).unwrap();
        graphics
            .poll(
                Lease {
                    server: [1; 32],
                    session: [2; 32],
                    control: [3; 32],
                    connection: [4; 32],
                },
                &mut |request| {
                    let Request::GraphicsChunk { image, offset, .. } = request else {
                        panic!("unexpected request")
                    };
                    Ok(Reply::GraphicsChunk(Ok(Chunk {
                        image: *image,
                        offset: *offset,
                        bytes: vec![255; 4],
                    })))
                },
            )
            .unwrap()
            .unwrap();
        let snapshot = graphics.snapshot();
        let cx = cx.add_empty_window();
        let plan = cx.update(|window, cx| {
            let mut cache = TerminalGraphicsCache::default();
            let prepared = cache
                .sync(ActiveScreenSnapshot::Primary, &snapshot, window, cx)
                .unwrap();
            prepared.graphics.paint_plan(
                Bounds::new(point(px(0.), px(0.)), size(px(104.), px(108.))),
                px(13. / window.scale_factor()),
                px(27. / window.scale_factor()),
                size(
                    px(13. / window.scale_factor()),
                    px(27. / window.scale_factor()),
                ),
                window.scale_factor(),
            )
        });
        cx.draw(point(px(0.), px(0.)), size(px(104.), px(108.)), |_, _| {
            gpui::canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    plan.paint_layer(GraphicsLayer::AboveText, window).unwrap();
                },
            )
        });
        cx.update(|window, _| {
            let sprites = window.painted_polychrome_sprites();
            assert_eq!(sprites.len(), 1);
            let mask = sprites[0].content_mask.bounds;
            assert_eq!((mask.origin.x.0, mask.origin.y.0), (13., 27.));
            assert_eq!((mask.size.width.0, mask.size.height.0), (26., 27.));
            // One repeated edge texel on every side lies outside the measured cells.
            assert_eq!(sprites[0].bounds.origin.x.0, -13.);
            assert_eq!(sprites[0].bounds.origin.y.0, 0.);
            assert_eq!(sprites[0].bounds.size.width.0, 78.);
            assert_eq!(sprites[0].bounds.size.height.0, 81.);
        });
    }

    #[test]
    fn z_layers_use_the_protocol_thresholds() {
        assert_eq!(layer_for_z(i32::MIN), GraphicsLayer::BelowBackground);
        assert_eq!(layer_for_z(i32::MIN / 2), GraphicsLayer::BelowText);
        assert_eq!(layer_for_z(-1), GraphicsLayer::BelowText);
        assert_eq!(layer_for_z(0), GraphicsLayer::AboveText);
    }

    /// The emulator measures images in whole device-pixel cells from the nominal cell, while the
    /// grid spreads leftover width across its cells. An image placed over twelve cells covers
    /// exactly those twelve fitted cells.
    #[test]
    fn cell_sized_images_cover_their_fitted_grid_cells() {
        let image_snapshot = Arc::new(ImageSnapshot {
            key: ImageKey {
                image_id: 1,
                generation: 1,
            },
            width: 24,
            height: 24,
            rgba: Arc::from(vec![0; 24 * 24 * 4]),
            reservation: None,
        });
        let image = Arc::new(upload_image(Arc::clone(&image_snapshot)).unwrap());
        // A 10.8 x 20 nominal cell measures 11 x 20 device pixels at 1x; the grid draws 10.84.
        let placement = ImagePlacementSnapshot {
            image: image_snapshot.key,
            placement_id: 1,
            z: 0,
            viewport_col: 35,
            viewport_row: 3,
            cell_offset_x: 0,
            cell_offset_y: 0,
            source_x: 0,
            source_y: 0,
            source_width: 24,
            source_height: 24,
            destination_width: 12 * 11,
            destination_height: 6 * 20,
            unicode_placeholder: false,
        };
        let snapshot = GraphicsSnapshot {
            failure: None,
            generation: 1,
            placement_generation: 1,
            images: Arc::from([image_snapshot]),
            placements: Arc::from([placement.clone()]),
        };
        let prepared = PreparedGraphics::new(
            ActiveScreenSnapshot::Primary,
            &snapshot,
            Arc::from([PreparedPlacement { placement, image }]),
        );
        let grid = Bounds::new(point(px(0.0), px(0.0)), size(px(1670.0), px(960.0)));
        let cell = px(1670.0 / 154.0);

        let plan = prepared.paint_plan(grid, cell, px(20.0), size(px(10.8), px(20.0)), 1.0);

        let destination = plan.paints[0].destination;
        assert!((destination.left() - cell * 35.0).abs() < px(0.001));
        assert!((destination.size.width - cell * 12.0).abs() < px(0.001));
        assert_eq!(destination.size.height, px(120.0));
    }

    #[test]
    fn source_crop_transforms_padded_image_under_destination_mask() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();

        let plan = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );
        let paint = &plan.paints[0];
        assert_eq!(paint.destination.origin, point(px(27.0), px(28.0)));
        assert_eq!(paint.destination.size, size(px(50.0), px(40.0)));
        // The 100x80 image fills 100x80 pixels from (17, 8); its repeated edge texels add one
        // source texel on every side.
        assert_eq!(paint.padded_image.origin, point(px(16.0), px(7.0)));
        assert_eq!(paint.padded_image.size, size(px(102.0), px(82.0)));
    }

    #[gpui::test]
    fn image_preflight_fills_atlas_without_adding_a_scene_primitive(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let plan = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );
        let image = Arc::clone(&plan.paints[0].image);
        cx.draw(
            point(px(0.0), px(0.0)),
            size(px(800.0), px(400.0)),
            move |_, _| {
                gpui::canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        window.with_content_mask(
                            Some(ContentMask {
                                bounds: Bounds::new(
                                    point(px(0.0), px(0.0)),
                                    size(px(0.0), px(0.0)),
                                ),
                            }),
                            |window| {
                                plan.paint_layer(GraphicsLayer::AboveText, window)
                                    .expect("image preparation should reach the atlas");
                            },
                        );
                    },
                )
            },
        );
        cx.update(|window, _| {
            assert!(window.has_image_atlas_entry(&image));
            assert!(window.painted_polychrome_sprites().is_empty());
        });
    }

    #[test]
    fn image_atlas_failure_during_preflight_paints_only_retained_surface() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let first_plan = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );
        let first = first_plan.paints[0].clone();
        let mut second = first.clone();
        second.image = Arc::new(RenderImage::new(smallvec![Frame::new(RgbaImage::new(
            1, 1
        ))]));
        let failed_image = second.image.id;
        let plan = GraphicsPaintPlan {
            paints: Arc::from([first.clone(), second]),
        };
        let atlas = Arc::new(FailingImageAtlas {
            inner: gpui::HeadlessAtlas::default(),
            image_lookups: AtomicUsize::new(0),
            failed_image,
        });
        let mut cx = gpui::HeadlessAppContext::with_platform(
            Arc::new(gpui::NoopTextSystem),
            Arc::new(()),
            {
                let atlas = Arc::clone(&atlas);
                move || Ok(Some(Box::new(FailingImageRenderer(Arc::clone(&atlas)))))
            },
        );
        let result = Arc::new(std::sync::Mutex::new(None));
        let handle = cx
            .open_window(size(px(800.0), px(400.0)), {
                let result = Arc::clone(&result);
                move |_, cx| cx.new(|_| FailingImagePaint { plan, result })
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();

        assert!(atlas.image_lookups.load(Ordering::Relaxed) >= 2);
        assert!(matches!(
            *result.lock().unwrap(),
            Some(Err(GraphicsResourceError::Paint))
        ));
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.has_image_atlas_entry(&first.image));
            assert!(window.painted_polychrome_sprites().is_empty());
            assert_eq!(window.painted_quads().len(), 1);
        })
        .unwrap();
    }

    #[gpui::test]
    fn kitty_placeholder_protocol_preserves_image_crops_across_text_gaps_and_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::geometry::{
            BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
        };
        use crate::terminal::testing::{TerminalEmulator, graphics_test_lock};

        let _guard = graphics_test_lock();
        let mut emulator = TerminalEmulator::new(TerminalGeometry::from_grid(
            CellGridSize::new(8, 4),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::new(2.0).unwrap(),
        ))
        .unwrap();
        let image = format!(
            "\x1b_Ga=T,t=d,f=24,i=42,s=4,v=4,U=1,c=4,r=2,q=2;{}\x1b\\",
            "AAAA".repeat(16)
        );
        emulator.feed(image.as_bytes());
        emulator.feed(
            concat!(
                "\x1b[38;5;42m",
                "\u{10eeee}\u{305}\u{305}\u{10eeee}\u{305}\u{30d}",
                "x\u{10eeee}\u{305}\u{310}\r\n",
                "\u{10eeee}\u{30d}\u{305}\u{10eeee}\u{10eeee}\u{10eeee}",
                "\x1b[39m",
            )
            .as_bytes(),
        );
        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.graphics.placements.len(), 3);

        let test_window = cx.add_window(|_, _| gpui::EmptyView);
        test_window
            .update(cx, |_, window, cx| {
                let mut cache = TerminalGraphicsCache::default();
                let preparation = cache
                    .sync(snapshot.active_screen, &snapshot.graphics, window, cx)
                    .unwrap();
                let bounds = Bounds::new(point(px(5.0), px(7.0)), size(px(80.0), px(80.0)));
                let plan = preparation.graphics.paint_plan(
                    bounds,
                    px(10.0),
                    px(20.0),
                    size(px(10.0), px(20.0)),
                    2.0,
                );

                assert_eq!(
                    plan.paints
                        .iter()
                        .map(|paint| paint.destination)
                        .collect::<Vec<_>>(),
                    [
                        Bounds::new(point(px(5.0), px(7.0)), size(px(20.0), px(20.0))),
                        Bounds::new(point(px(35.0), px(7.0)), size(px(10.0), px(20.0))),
                        Bounds::new(point(px(5.0), px(27.0)), size(px(40.0), px(20.0))),
                    ]
                );
                let padded_image = Bounds::new(point(px(-5.0), px(-3.0)), size(px(60.0), px(60.0)));
                assert!(
                    plan.paints
                        .iter()
                        .all(|paint| paint.padded_image == padded_image)
                );
                assert!(
                    plan.paints
                        .iter()
                        .all(|paint| paint.layer == GraphicsLayer::BelowText)
                );
                cache.rollback(preparation.token, Some(window), cx);
            })
            .expect("the test window should remain available");
    }

    #[test]
    fn unchanged_geometry_reuses_the_arc_backed_paint_plan() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();

        let first = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );
        let second = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );

        assert!(first.shares_paints_with(&second));
        assert_eq!(prepared.paint_plan_builds(), 1);
    }

    #[test]
    fn invalid_backing_scales_share_the_canonical_one_x_plan() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let first = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            f32::NAN,
        );

        for scale in [f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0, 1.0] {
            let next = prepared.paint_plan(
                bounds,
                cell_width,
                line_height,
                size(cell_width, line_height),
                scale,
            );
            assert!(first.shares_paints_with(&next));
        }
        assert_eq!(prepared.paint_plan_builds(), 1);
    }

    #[test]
    fn backing_scale_change_rebuilds_geometry_without_replacing_the_image() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let one_x = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            1.0,
        );
        let two_x = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );

        assert!(!one_x.shares_paints_with(&two_x));
        assert!(Arc::ptr_eq(&one_x.paints[0].image, &two_x.paints[0].image));
        assert_eq!(prepared.paint_plan_builds(), 2);
    }

    #[test]
    fn full_grid_bounds_are_geometry_cache_inputs() {
        let prepared = prepared_graphics();
        let (bounds, cell_width, line_height) = geometry();
        let first = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );
        let resized = Bounds::new(bounds.origin, size(px(801.0), bounds.size.height));
        let second = prepared.paint_plan(
            resized,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );

        assert!(!first.shares_paints_with(&second));
        assert_eq!(prepared.paint_plan_builds(), 2);
    }

    #[test]
    fn prepared_graphics_clones_share_the_geometry_cache() {
        let prepared = prepared_graphics();

        let clone = prepared.clone();
        assert!(prepared.shares_identity_with(&clone));
        let (bounds, cell_width, line_height) = geometry();
        let original_plan = prepared.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );
        let clone_plan = clone.paint_plan(
            bounds,
            cell_width,
            line_height,
            size(cell_width, line_height),
            2.0,
        );
        assert!(original_plan.shares_paints_with(&clone_plan));
        assert_eq!(prepared.paint_plan_builds(), 1);
        assert_eq!(clone.paint_plan_builds(), 1);
    }

    #[test]
    fn upload_converts_rgba_to_gpui_bgra_and_repeats_edge_texels() {
        let snapshot = Arc::new(ImageSnapshot {
            key: ImageKey {
                image_id: 1,
                generation: 1,
            },
            width: 2,
            height: 1,
            rgba: Arc::from([10, 20, 30, 40, 50, 60, 70, 80]),
            reservation: None,
        });

        let uploaded = upload_image(snapshot).unwrap();

        let left = [30, 20, 10, 40];
        let right = [70, 60, 50, 80];
        let row = [left, left, right, right].concat();
        assert_eq!(uploaded.render_image.size(0), size(4.into(), 3.into()));
        assert_eq!(
            uploaded.render_image.as_bytes(0).unwrap(),
            [row.as_slice(), &row, &row].concat()
        );
        assert_eq!(uploaded._reservation.0, 4 * 3 * 4);
    }

    #[test]
    fn upload_rejects_images_without_texels() {
        let snapshot = Arc::new(ImageSnapshot {
            key: ImageKey {
                image_id: 1,
                generation: 1,
            },
            width: 0,
            height: 3,
            rgba: Arc::from([]),
            reservation: None,
        });

        assert!(matches!(
            upload_image(snapshot),
            Err(GraphicsResourceError::InvalidImage)
        ));
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

            assert!(!cache.rollback(token(1), None, cx));
            assert_eq!(
                cache.staged.as_ref().map(|staged| staged.token),
                Some(token(2))
            );
            assert!(!cache.mark_presented(token(1), cx));
            assert_eq!(
                cache.staged.as_ref().map(|staged| staged.token),
                Some(token(2))
            );
        });
    }
}
