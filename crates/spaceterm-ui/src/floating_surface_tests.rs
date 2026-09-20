use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyView, App, Bounds, Context, Element, ElementId, Entity, Fill, GlobalElementId,
    InspectorElementId, LayoutId, Pixels, Render, StyleRefinement, TestAppContext, Window, div, px,
    rgba,
};

use crate::*;

const ROOT_FIELD: gpui::Rgba = gpui::Rgba {
    r: 0.1,
    g: 0.2,
    b: 0.3,
    a: 1.0,
};
const HOST_FIELD: gpui::Rgba = gpui::Rgba {
    r: 0.6,
    g: 0.5,
    b: 0.4,
    a: 1.0,
};

fn input_theme(background: gpui::Rgba, caret_width: Pixels) -> TextInputTheme {
    let text = rgba(0xffffffff);
    let accent = rgba(0x5599ffff);
    let paint = TextInputPaint::new(text, text, accent, text, text, accent);
    TextInputTheme::new(
        TextInputVariants::new(paint, paint),
        TextInputMetrics::new(caret_width, px(2.0), Duration::from_millis(16), px(20.0)),
    )
    .field_frame(FieldFrameTheme::new(
        background, accent, accent, accent, background, accent,
    ))
}

fn test_catalog(hosted: bool, generation: u64, host_fill: gpui::Rgba) -> ControlThemeCatalog {
    let mut catalog = crate::catalog_tests::catalog(generation);
    catalog.text_input = input_theme(ROOT_FIELD, px(1.0));
    catalog.floating = None;
    catalog.floating_controls = None;
    if hosted {
        let controls = FloatingControlThemes::new(
            catalog.button,
            catalog.toggle,
            catalog.progress,
            catalog.segmented_control,
            catalog.search_field,
            input_theme(host_fill, px(6.0)),
        );
        catalog = catalog.floating(FloatingSurfaceTheme::default(), controls);
    }
    catalog
}

type FieldObservations = Rc<RefCell<Vec<(&'static str, Option<Fill>)>>>;

struct RetainedField {
    id: &'static str,
    input: Entity<TextInput>,
    observations: FieldObservations,
}

impl Render for RetainedField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut frame = field_frame(
            self.id,
            &self.input.read(cx).focus_handle(),
            FieldState::default(),
            cx,
        );
        self.observations
            .borrow_mut()
            .push((self.id, frame.style().background.clone()));
        frame
            .debug_selector({
                let id = self.id;
                move || id.to_owned()
            })
            .w(px(200.0))
            .h(px(32.0))
            .child(self.input.clone())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Layout,
    Prepaint,
    Paint,
}

type PhaseObservations = Rc<RefCell<Vec<(Phase, Option<Fill>)>>>;

/// Custom elements may resolve their control presentation in any rendering phase.
struct PhaseField {
    content: AnyElement,
    observations: PhaseObservations,
}

impl PhaseField {
    fn record(&self, phase: Phase, cx: &App) {
        let mut field = field_surface("phase-field", FieldState::default(), cx);
        self.observations
            .borrow_mut()
            .push((phase, field.style().background.clone()));
    }
}

impl IntoElement for PhaseField {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PhaseField {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.record(Phase::Layout, cx);
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.record(Phase::Prepaint, cx);
        self.content.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.record(Phase::Paint, cx);
        self.content.paint(window, cx);
    }
}

struct HostFixture {
    before: Entity<RetainedField>,
    hosted: Entity<RetainedField>,
    after: Entity<RetainedField>,
    phases: PhaseObservations,
}

impl HostFixture {
    fn new(
        fields: FieldObservations,
        phases: PhaseObservations,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut field = |id| {
            cx.new(|cx| RetainedField {
                id,
                input: cx.new(|cx| TextInput::new(id, id, "", window, cx)),
                observations: Rc::clone(&fields),
            })
        };
        Self {
            before: field("root-before"),
            hosted: field("hosted-field"),
            after: field("root-after"),
            phases,
        }
    }
}

impl Render for HostFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .child(self.before.clone())
            .child(
                FloatingSurfaceTheme::default()
                    .shell(FloatingRole::Modal)
                    .mount(div().p(px(8.0)).child(PhaseField {
                        content: self.hosted.clone().into_any_element(),
                        observations: Rc::clone(&self.phases),
                    })),
            )
            .child(self.after.clone())
    }
}

struct CachedDebugBoundsChild {
    renders: Rc<Cell<usize>>,
}

impl Render for CachedDebugBoundsChild {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(|| "cached-debug-bounds-child".to_owned())
                    .w(px(40.0))
                    .h(px(30.0)),
            )
            .child(
                div()
                    .debug_selector(|| "duplicate-debug-bounds".to_owned())
                    .w(px(10.0))
                    .h(px(10.0)),
            )
    }
}

struct UncachedDebugBoundsSibling {
    width: Pixels,
}

impl Render for UncachedDebugBoundsSibling {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "duplicate-debug-bounds".to_owned())
            .w(self.width)
            .h(px(20.0))
    }
}

struct CachedDebugBoundsRoot {
    child: Entity<CachedDebugBoundsChild>,
    sibling: Entity<UncachedDebugBoundsSibling>,
    show_child: bool,
}

impl Render for CachedDebugBoundsRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .when(self.show_child, |root| {
                root.child(AnyView::from(self.child.clone()).cached(StyleRefinement {
                    size: gpui::SizeRefinement {
                        width: Some(px(40.0).into()),
                        height: Some(px(40.0).into()),
                    },
                    ..StyleRefinement::default()
                }))
            })
            .child(self.sibling.clone())
    }
}

#[test]
fn floating_theme_applies_one_sanitized_backdrop_alpha_limit_to_every_shell() {
    let theme = FloatingSurfaceTheme::default().backdrop_alpha_limit(0.15);
    for role in [
        FloatingRole::Popover,
        FloatingRole::Command,
        FloatingRole::Modal,
        FloatingRole::Tooltip,
        FloatingRole::Notice,
        FloatingRole::Readout,
    ] {
        let shell = theme.shell(role);
        assert_eq!(shell.backdrop_alpha_limit(), 0.15);
        let mut frame = shell.frame(div());
        assert_eq!(frame.style().backdrop_alpha_limit, Some(0.15));
    }

    assert_eq!(
        FloatingSurfaceTheme::default()
            .backdrop_alpha_limit(f32::NAN)
            .shell(FloatingRole::Popover)
            .backdrop_alpha_limit(),
        1.0
    );
    assert_eq!(
        FloatingSurfaceTheme::default()
            .backdrop_alpha_limit(-1.0)
            .shell(FloatingRole::Popover)
            .backdrop_alpha_limit(),
        0.0
    );
    assert_eq!(
        FloatingSurfaceTheme::default()
            .backdrop_alpha_limit(2.0)
            .shell(FloatingRole::Popover)
            .backdrop_alpha_limit(),
        1.0
    );
}

#[gpui::test]
fn cached_view_debug_bounds_should_survive_reused_paint_and_clear_after_unmount(
    cx: &mut TestAppContext,
) {
    let renders = Rc::new(Cell::new(0));
    let child_renders = Rc::clone(&renders);
    let (root, cx) = cx.add_window_view(move |_, cx| CachedDebugBoundsRoot {
        child: cx.new(|_| CachedDebugBoundsChild {
            renders: child_renders,
        }),
        sibling: cx.new(|_| UncachedDebugBoundsSibling { width: px(20.0) }),
        show_child: true,
    });
    cx.run_until_parked();

    let initial = cx
        .debug_bounds("cached-debug-bounds-child")
        .expect("the cached child should publish its initial bounds");
    assert_eq!(renders.get(), 1);

    for sibling_width in [px(30.0), px(40.0)] {
        let sibling = root.read_with(cx, |root, _| root.sibling.clone());
        sibling.update(cx, |sibling, cx| {
            sibling.width = sibling_width;
            cx.notify();
        });
        cx.run_until_parked();

        assert_eq!(
            cx.debug_bounds("cached-debug-bounds-child"),
            Some(initial),
            "reusing cached paint must replay the visible child's selector bounds"
        );
        assert_eq!(
            renders.get(),
            1,
            "the child paint must actually stay cached"
        );
        assert_eq!(
            cx.debug_bounds("duplicate-debug-bounds")
                .expect("the later uncached sibling should win the duplicate selector")
                .size
                .width,
            sibling_width,
            "cached replay must preserve last-painted duplicate selector lookup"
        );
    }

    root.update(cx, |root, cx| {
        root.show_child = false;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("cached-debug-bounds-child").is_none(),
        "unmounting a cached child must remove its selector bounds"
    );
    assert_eq!(
        cx.debug_bounds("duplicate-debug-bounds")
            .expect("the remaining uncached sibling should retain its selector")
            .size
            .width,
        px(40.0)
    );
}

#[gpui::test]
fn mounted_surface_should_resolve_retained_fields_and_all_phases_against_its_host(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init(cx, test_catalog(true, 1, HOST_FIELD)))
        .expect("catalog should initialize");
    let fields = FieldObservations::default();
    let phases = PhaseObservations::default();
    let (root, cx) = cx.add_window_view(|window, cx| {
        HostFixture::new(Rc::clone(&fields), Rc::clone(&phases), window, cx)
    });
    let input = root.read_with(cx, |root, cx| root.hosted.read(cx).input.clone());
    cx.update(|window, cx| {
        window.activate_window();
        input.read(cx).focus_handle().focus(window);
    });
    cx.run_until_parked();

    for (id, expected) in [
        ("root-before", ROOT_FIELD),
        ("hosted-field", HOST_FIELD),
        ("root-after", ROOT_FIELD),
    ] {
        assert!(
            fields
                .borrow()
                .iter()
                .any(|(seen, fill)| { *seen == id && *fill == Some(expected.into()) }),
            "{id} must resolve its actual field frame against its own host: {:?}",
            fields.borrow()
        );
    }
    for phase in [Phase::Layout, Phase::Prepaint, Phase::Paint] {
        assert!(
            phases
                .borrow()
                .iter()
                .any(|(seen, fill)| { *seen == phase && *fill == Some(HOST_FIELD.into()) }),
            "host scope must remain entered during {phase:?}: {:?}",
            phases.borrow()
        );
    }
    assert!(
        cx.update(|window, _| {
            window
                .painted_quads_for_test()
                .iter()
                .any(|quad| quad.visible_bounds.size.width == px(6.0).scale(window.scale_factor()))
        }),
        "the retained TextInput must paint its host's six-pixel caret"
    );
}

#[gpui::test]
fn mounted_surface_without_host_catalog_should_keep_root_control_presentation(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init(cx, test_catalog(false, 1, HOST_FIELD)))
        .expect("legacy catalog should initialize");
    let fields = FieldObservations::default();
    let phases = PhaseObservations::default();
    let (_, cx) = cx.add_window_view(|window, cx| {
        HostFixture::new(Rc::clone(&fields), Rc::clone(&phases), window, cx)
    });
    cx.run_until_parked();

    assert!(
        fields
            .borrow()
            .iter()
            .any(|(id, fill)| { *id == "hosted-field" && *fill == Some(ROOT_FIELD.into()) })
    );
    assert!(
        phases
            .borrow()
            .iter()
            .all(|(_, fill)| *fill == Some(ROOT_FIELD.into()))
    );
}

#[gpui::test]
fn replacing_catalog_should_refresh_retained_host_fields_without_leaking_to_root(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init(cx, test_catalog(true, 1, HOST_FIELD)))
        .expect("catalog should initialize");
    let fields = FieldObservations::default();
    let phases = PhaseObservations::default();
    let (_, cx) = cx.add_window_view(|window, cx| {
        HostFixture::new(Rc::clone(&fields), Rc::clone(&phases), window, cx)
    });
    cx.run_until_parked();
    fields.borrow_mut().clear();
    let replacement = rgba(0x8c357aff);
    cx.update(|_, cx| {
        replace_control_theme_catalog(cx, test_catalog(true, 2, replacement))
            .expect("catalog replacement should succeed");
    });
    cx.run_until_parked();

    assert!(
        fields
            .borrow()
            .iter()
            .any(|(id, fill)| { *id == "hosted-field" && *fill == Some(replacement.into()) })
    );
    assert!(
        fields
            .borrow()
            .iter()
            .filter(|(id, _)| *id != "hosted-field")
            .all(|(_, fill)| *fill == Some(ROOT_FIELD.into()))
    );
}

#[gpui::test]
fn replacing_catalog_without_floating_overrides_should_remove_stale_host_presentation(
    cx: &mut TestAppContext,
) {
    let mut initial = test_catalog(true, 1, HOST_FIELD);
    let material = FloatingSurfacePaint::new(rgba(0x35748cff), rgba(0x112233ff), rgba(0x445566ff));
    initial.floating = Some(FloatingSurfaceTheme::new(
        FloatingSurfacePaints::new(material, material),
        rgba(0x00000088).into(),
        rgba(0x00000044),
    ));
    cx.update(|cx| init(cx, initial))
        .expect("catalog should initialize");
    let fields = FieldObservations::default();
    let phases = PhaseObservations::default();
    let (_, cx) = cx.add_window_view(|window, cx| {
        HostFixture::new(Rc::clone(&fields), Rc::clone(&phases), window, cx)
    });
    cx.run_until_parked();
    fields.borrow_mut().clear();
    cx.update(|_, cx| {
        replace_control_theme_catalog(cx, test_catalog(false, 2, HOST_FIELD))
            .expect("catalog replacement should succeed");
    });
    cx.run_until_parked();

    assert!(
        fields
            .borrow()
            .iter()
            .any(|(id, fill)| { *id == "hosted-field" && *fill == Some(ROOT_FIELD.into()) }),
        "omitting floating overrides must restore the actual hosted field's root fallback"
    );
    cx.update(|_, cx| {
        assert_eq!(
            cx.try_global::<FloatingSurfaceTheme>()
                .copied()
                .unwrap_or_default(),
            FloatingSurfaceTheme::default(),
            "an omitted override must not preserve the prior catalog's floating material"
        );
    });
}

#[gpui::test]
fn scaling_catalog_should_preserve_role_materials_and_hairlines_while_growing_geometry(
    cx: &mut TestAppContext,
) {
    let initial = test_catalog(true, 1, HOST_FIELD);
    cx.update(|cx| init(cx, initial.clone()))
        .expect("catalog should initialize");
    let before = cx.update(|cx| *cx.global::<FloatingSurfaceTheme>());
    cx.update(|cx| {
        replace_control_theme_catalog(
            cx,
            initial
                .scale_metrics(1.5, 1.25)
                .generation(ControlThemeGeneration::new(2)),
        )
        .expect("scaled catalog should replace");
    });
    let after = cx.update(|cx| *cx.global::<FloatingSurfaceTheme>());

    for role in [
        FloatingRole::Popover,
        FloatingRole::Command,
        FloatingRole::Modal,
        FloatingRole::Tooltip,
        FloatingRole::Notice,
        FloatingRole::Readout,
    ] {
        let original = before.shell(role);
        let scaled = after.shell(role);
        assert_eq!(scaled.material(), original.material(), "{role:?}");
        assert_eq!(scaled.edge(), original.edge(), "{role:?}");
        assert_eq!(scaled.divider(), original.divider(), "{role:?}");
        assert_eq!(scaled.hairline(), original.hairline(), "{role:?}");
        assert_eq!(
            scaled.corner_radius(),
            original.corner_radius() * 1.25,
            "{role:?}"
        );
        assert_eq!(
            scaled.content_inset(),
            original.content_inset() * 1.25,
            "{role:?}"
        );
        assert_eq!(
            scaled.nested_radius(),
            original.nested_radius() * 1.25,
            "{role:?}"
        );
    }
}
