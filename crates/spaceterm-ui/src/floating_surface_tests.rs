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
const SETTINGS_FIELD: gpui::Rgba = gpui::Rgba {
    r: 0.8,
    g: 0.7,
    b: 0.6,
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
        let controls = SurfaceControlThemes::new(
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

struct FloatingShellFixture {
    role: FloatingRole,
}

impl Render for FloatingShellFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let shell = FloatingSurfaceTheme::default().shell(self.role);
        div()
            .p(px(20.0))
            .child(shell.mount(div().w(px(200.0)).h(px(80.0))))
    }
}

#[gpui::test]
fn floating_shell_keeps_its_rounded_edge_without_a_straight_top_hairline(cx: &mut TestAppContext) {
    let (root, cx) = cx.add_window_view(|_, _| FloatingShellFixture {
        role: FloatingRole::Popover,
    });
    for role in [
        FloatingRole::Popover,
        FloatingRole::Command,
        FloatingRole::Modal,
        FloatingRole::Tooltip,
        FloatingRole::Notice,
        FloatingRole::Readout,
    ] {
        root.update(cx, |root, cx| {
            root.role = role;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, _| {
            let top = px(20.0).scale(window.scale_factor());
            let top_strip_bottom = px(22.0).scale(window.scale_factor());
            let hairline = px(1.0).scale(window.scale_factor());
            let minimum_line_width = px(100.0).scale(window.scale_factor());
            let quads = window.painted_quads();
            let straight_top_edges: Vec<_> = quads
                .iter()
                .filter(|quad| {
                    quad.bounds.intersect(&quad.content_mask.bounds).origin.y >= top
                        && quad.bounds.intersect(&quad.content_mask.bounds).bottom()
                            <= top_strip_bottom
                        && quad.bounds.intersect(&quad.content_mask.bounds).size.height == hairline
                        && quad.bounds.intersect(&quad.content_mask.bounds).size.width
                            >= minimum_line_width
                })
                .collect();
            assert!(
                straight_top_edges.is_empty(),
                "{role:?} must not add a straight top hairline: {straight_top_edges:?}"
            );
            assert!(
                quads
                    .iter()
                    .any(|quad| quad.border_color == rgba(0xffffff26).into()),
                "{role:?} must retain the shell's rounded outer edge"
            );
        });
    }
}

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

#[derive(Clone, Debug, PartialEq)]
struct ActivityPhaseObservation {
    phase: Phase,
    field_fill: Option<Fill>,
    button_normal: gpui::Rgba,
    button_hovered: gpui::Rgba,
    button_disabled: gpui::Rgba,
    focus_border: gpui::Rgba,
    focus_ring_width: Pixels,
    activity: ControlWindowActivity,
}

type ActivityObservations = Rc<RefCell<Vec<ActivityPhaseObservation>>>;

struct ActivityProbe {
    content: AnyElement,
    observations: ActivityObservations,
}

impl ActivityProbe {
    fn record(&self, phase: Phase, cx: &App) {
        let mut field = field_surface("activity-field", FieldState::default(), cx);
        let button = ControlHost::Window.button_theme(cx);
        let paints = button.paints(ButtonVariant::Secondary);
        self.observations
            .borrow_mut()
            .push(ActivityPhaseObservation {
                phase,
                field_fill: field.style().background.clone(),
                button_normal: paints.normal().background(),
                button_hovered: paints.hovered().background(),
                button_disabled: paints.disabled().background(),
                focus_border: button.focus_border(),
                focus_ring_width: button.resolved_focus_ring_width(),
                activity: ControlWindowActivity::current(),
            });
    }
}

impl IntoElement for ActivityProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ActivityProbe {
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.record(Phase::Layout, cx);
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
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
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.record(Phase::Paint, cx);
        self.content.paint(window, cx);
    }
}

struct ActivityFixture {
    activity: ControlWindowActivity,
    constructed: Rc<RefCell<Vec<ControlWindowActivity>>>,
    observations: ActivityObservations,
}

struct ThemeScopeFixture {
    scope: ControlThemeScope,
    observations: PhaseObservations,
}

impl Render for ThemeScopeFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.scope.with_scope(|| {
            self.scope.mount(PhaseField {
                content: div().size_full().into_any_element(),
                observations: Rc::clone(&self.observations),
            })
        })
    }
}

impl Render for ActivityFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.activity.with_scope(|| {
            self.constructed
                .borrow_mut()
                .push(ControlWindowActivity::current());
            self.activity.mount(ActivityProbe {
                content: div().size_full().into_any_element(),
                observations: Rc::clone(&self.observations),
            })
        })
    }
}

fn activity_button_theme(
    normal: gpui::Rgba,
    hovered: gpui::Rgba,
    disabled: gpui::Rgba,
    focus: gpui::Rgba,
) -> ButtonTheme {
    let text = rgba(0xffffffff);
    let clear = rgba(0x00000000);
    let state = ButtonVariantStyle::new(
        ButtonPaint::new(normal, text, clear),
        ButtonPaint::new(hovered, text, clear),
        ButtonPaint::new(hovered, text, clear),
        ButtonPaint::new(disabled, text, clear),
    );
    let metrics = ButtonMetrics::new(px(28.0));
    ButtonTheme::new(
        ButtonVariants::new(state, state, state, state, state, state, state),
        ButtonSizes::new(metrics, metrics, metrics, metrics),
        focus,
    )
}

fn activity_catalog(
    generation: u64,
    field_fill: gpui::Rgba,
    normal: gpui::Rgba,
    hovered: gpui::Rgba,
    disabled: gpui::Rgba,
    focus: gpui::Rgba,
    focus_ring_width: Pixels,
) -> ControlThemeCatalog {
    let mut catalog = test_catalog(false, generation, field_fill);
    catalog.text_input = input_theme(field_fill, px(1.0));
    catalog.button =
        activity_button_theme(normal, hovered, disabled, focus).focus_ring_width(focus_ring_width);
    catalog
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

#[test]
fn window_activity_scope_is_reentrant_and_restores_its_caller() {
    assert_eq!(
        ControlWindowActivity::current(),
        ControlWindowActivity::Active
    );
    ControlWindowActivity::Inactive.with_scope(|| {
        assert_eq!(
            ControlWindowActivity::current(),
            ControlWindowActivity::Inactive
        );
        ControlWindowActivity::Active.with_scope(|| {
            assert_eq!(
                ControlWindowActivity::current(),
                ControlWindowActivity::Active
            );
        });
        assert_eq!(
            ControlWindowActivity::current(),
            ControlWindowActivity::Inactive
        );
    });
    assert_eq!(
        ControlWindowActivity::current(),
        ControlWindowActivity::Active
    );
}

#[test]
fn control_theme_scope_is_reentrant_and_restores_its_caller() {
    assert_eq!(ControlThemeScope::current(), ControlThemeScope::Application);
    ControlThemeScope::Settings.with_scope(|| {
        assert_eq!(ControlThemeScope::current(), ControlThemeScope::Settings);
        ControlThemeScope::Application.with_scope(|| {
            assert_eq!(ControlThemeScope::current(), ControlThemeScope::Application);
        });
        assert_eq!(ControlThemeScope::current(), ControlThemeScope::Settings);
    });
    assert_eq!(ControlThemeScope::current(), ControlThemeScope::Application);
}

#[gpui::test]
fn settings_catalog_scope_is_isolated_and_retained_in_every_rendering_phase(
    cx: &mut TestAppContext,
) {
    let application = activity_catalog(
        1,
        ROOT_FIELD,
        ROOT_FIELD,
        ROOT_FIELD,
        ROOT_FIELD,
        ROOT_FIELD,
        px(1.0),
    );
    let settings = activity_catalog(
        1,
        SETTINGS_FIELD,
        SETTINGS_FIELD,
        SETTINGS_FIELD,
        SETTINGS_FIELD,
        SETTINGS_FIELD,
        px(1.0),
    );
    cx.update(|cx| init(cx, application.clone())).unwrap();
    cx.update(|cx| {
        replace_scoped_control_theme_catalogs(
            cx,
            Box::new(application.clone()),
            Box::new(application),
            Box::new(settings.clone()),
            Box::new(settings),
        )
    })
    .unwrap();

    let application_observations = PhaseObservations::default();
    let settings_observations = PhaseObservations::default();
    let _application_window = cx.add_window({
        let observations = Rc::clone(&application_observations);
        move |_, _| ThemeScopeFixture {
            scope: ControlThemeScope::Application,
            observations,
        }
    });
    let _settings_window = cx.add_window({
        let observations = Rc::clone(&settings_observations);
        move |_, _| ThemeScopeFixture {
            scope: ControlThemeScope::Settings,
            observations,
        }
    });
    cx.run_until_parked();

    for phase in [Phase::Layout, Phase::Prepaint, Phase::Paint] {
        assert!(
            application_observations
                .borrow()
                .contains(&(phase, Some(ROOT_FIELD.into())))
        );
        assert!(
            settings_observations
                .borrow()
                .contains(&(phase, Some(SETTINGS_FIELD.into())))
        );
    }
}

#[gpui::test]
fn two_windows_keep_activity_catalogs_isolated_in_every_rendering_phase(cx: &mut TestAppContext) {
    let active_fill = rgba(0x182433ff);
    let inactive_fill = rgba(0x30343aff);
    let active_normal = rgba(0x204060ff);
    let active_hovered = rgba(0x306090ff);
    let inactive_normal = rgba(0x383838ff);
    let disabled = rgba(0x181818ff);
    let active_focus = rgba(0x5599ffff);
    let no_focus = rgba(0x00000000);
    let active = activity_catalog(
        1,
        active_fill,
        active_normal,
        active_hovered,
        disabled,
        active_focus,
        px(1.0),
    );
    let inactive = activity_catalog(
        1,
        inactive_fill,
        inactive_normal,
        inactive_normal,
        disabled,
        no_focus,
        px(2.0),
    );
    cx.update(|cx| init(cx, active.clone()))
        .expect("catalog should initialize");
    assert_eq!(
        cx.update(|cx| replace_control_theme_catalogs(cx, active, inactive)),
        Ok(ControlThemeReplacement::Applied)
    );

    let active_constructed = Rc::new(RefCell::new(Vec::new()));
    let inactive_constructed = Rc::new(RefCell::new(Vec::new()));
    let active_observations = ActivityObservations::default();
    let inactive_observations = ActivityObservations::default();
    let _active_window = cx.add_window({
        let constructed = Rc::clone(&active_constructed);
        let observations = Rc::clone(&active_observations);
        move |_, _| ActivityFixture {
            activity: ControlWindowActivity::Active,
            constructed,
            observations,
        }
    });
    let _inactive_window = cx.add_window({
        let constructed = Rc::clone(&inactive_constructed);
        let observations = Rc::clone(&inactive_observations);
        move |_, _| ActivityFixture {
            activity: ControlWindowActivity::Inactive,
            constructed,
            observations,
        }
    });
    cx.run_until_parked();

    assert!(!active_constructed.borrow().is_empty());
    assert!(!inactive_constructed.borrow().is_empty());
    assert!(
        active_constructed
            .borrow()
            .iter()
            .all(|activity| *activity == ControlWindowActivity::Active)
    );
    assert!(
        inactive_constructed
            .borrow()
            .iter()
            .all(|activity| *activity == ControlWindowActivity::Inactive)
    );
    for phase in [Phase::Layout, Phase::Prepaint, Phase::Paint] {
        assert!(active_observations.borrow().iter().any(|observation| {
            observation.phase == phase
                && observation.activity == ControlWindowActivity::Active
                && observation.field_fill == Some(active_fill.into())
                && observation.button_normal == active_normal
                && observation.button_hovered == active_hovered
                && observation.button_disabled == disabled
                && observation.focus_border == active_focus
                && observation.focus_ring_width == px(1.0)
        }));
        assert!(inactive_observations.borrow().iter().any(|observation| {
            observation.phase == phase
                && observation.activity == ControlWindowActivity::Inactive
                && observation.field_fill == Some(inactive_fill.into())
                && observation.button_normal == inactive_normal
                && observation.button_hovered == inactive_normal
                && observation.button_disabled == disabled
                && observation.focus_border == no_focus
                && observation.focus_ring_width == px(2.0)
        }));
    }
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
        input.read(cx).focus_handle().focus(window, cx);
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
            window.painted_quads().iter().any(|quad| {
                quad.bounds.intersect(&quad.content_mask.bounds).size.width
                    == px(6.0).scale(window.scale_factor())
            })
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
fn scaling_catalog_preserves_role_materials_radii_and_hairlines_while_growing_insets(
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
        assert_eq!(scaled.corner_radius(), original.corner_radius(), "{role:?}");
        assert_eq!(
            scaled.content_inset(),
            original.content_inset() * 1.25,
            "{role:?}"
        );
        assert_eq!(
            scaled.nested_radius(),
            (original.corner_radius() - scaled.content_inset()).max(px(4.0)),
            "{role:?}"
        );
    }
}

const PANEL_FIELD: gpui::Rgba = gpui::Rgba {
    r: 0.2,
    g: 0.6,
    b: 0.4,
    a: 1.0,
};
const TITLE_BAR_FIELD: gpui::Rgba = gpui::Rgba {
    r: 0.15,
    g: 0.25,
    b: 0.75,
    a: 1.0,
};
const CARD_FIELD: gpui::Rgba = gpui::Rgba {
    r: 0.7,
    g: 0.3,
    b: 0.2,
    a: 1.0,
};

fn surface_host_catalog(
    generation: u64,
    panel: gpui::Rgba,
    card: gpui::Rgba,
) -> ControlThemeCatalog {
    let catalog = test_catalog(true, generation, HOST_FIELD);
    let controls = |fill| {
        SurfaceControlThemes::new(
            catalog.button,
            catalog.toggle,
            catalog.progress,
            catalog.segmented_control,
            catalog.search_field,
            input_theme(fill, px(6.0)),
        )
    };
    let title_bar = controls(TITLE_BAR_FIELD);
    let panel = controls(panel);
    let card = controls(card);
    catalog
        .title_bar_controls(title_bar)
        .resting_controls(panel, card)
}

struct NestedHostFixture {
    fields: [Entity<RetainedField>; 8],
    phases: [PhaseObservations; 8],
}

impl NestedHostFixture {
    fn new(observations: FieldObservations, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            fields: [
                "window-before",
                "title-bar",
                "panel",
                "card",
                "floating",
                "window-override",
                "panel-after",
                "window-after",
            ]
            .map(|id| {
                cx.new(|cx| RetainedField {
                    id,
                    input: cx.new(|cx| TextInput::new(id, id, "", window, cx)),
                    observations: Rc::clone(&observations),
                })
            }),
            phases: std::array::from_fn(|_| PhaseObservations::default()),
        }
    }

    fn field(&self, index: usize) -> PhaseField {
        PhaseField {
            content: self.fields[index].clone().into_any_element(),
            observations: Rc::clone(&self.phases[index]),
        }
    }
}

impl Render for NestedHostFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .child(self.field(0))
            .child(ControlHost::TitleBar.mount(self.field(1)))
            .child(
                ControlHost::Panel.mount(
                    div()
                        .flex()
                        .flex_col()
                        .child(self.field(2))
                        .child(
                            ControlHost::Card.mount(
                                div().flex().flex_col().child(self.field(3)).child(
                                    FloatingSurfaceTheme::default()
                                        .shell(FloatingRole::Popover)
                                        .host(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .child(self.field(4))
                                                .child(ControlHost::Window.mount(self.field(5))),
                                        ),
                                ),
                            ),
                        )
                        .child(self.field(6)),
                ),
            )
            .child(self.field(7))
    }
}

#[gpui::test]
fn control_hosts_resolve_nearest_material_in_all_phases_and_restore_siblings(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init(cx, surface_host_catalog(1, PANEL_FIELD, CARD_FIELD)))
        .unwrap();
    let observations = FieldObservations::default();
    let (root, cx) = cx
        .add_window_view(|window, cx| NestedHostFixture::new(Rc::clone(&observations), window, cx));
    cx.run_until_parked();
    let expected = [
        ROOT_FIELD,
        TITLE_BAR_FIELD,
        PANEL_FIELD,
        CARD_FIELD,
        HOST_FIELD,
        ROOT_FIELD,
        PANEL_FIELD,
        ROOT_FIELD,
    ];
    root.read_with(cx, |root, cx| {
        for (index, expected) in expected.into_iter().enumerate() {
            let id = root.fields[index].read(cx).id;
            assert!(
                observations
                    .borrow()
                    .iter()
                    .any(|(seen, fill)| { *seen == id && *fill == Some(expected.into()) }),
                "{id} must use its nearest material host"
            );
            for phase in [Phase::Layout, Phase::Prepaint, Phase::Paint] {
                assert!(
                    root.phases[index]
                        .borrow()
                        .iter()
                        .any(|(seen, fill)| { *seen == phase && *fill == Some(expected.into()) }),
                    "{id} must keep its material host during {phase:?}"
                );
            }
        }
    });
}

#[gpui::test]
fn replacing_resting_host_catalog_refreshes_retained_fields_and_removes_omitted_hosts(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init(cx, surface_host_catalog(1, PANEL_FIELD, CARD_FIELD)))
        .unwrap();
    let observations = FieldObservations::default();
    let (_, cx) = cx
        .add_window_view(|window, cx| NestedHostFixture::new(Rc::clone(&observations), window, cx));
    cx.run_until_parked();
    observations.borrow_mut().clear();
    cx.update(|_, cx| {
        replace_control_theme_catalog(cx, surface_host_catalog(2, CARD_FIELD, PANEL_FIELD)).unwrap()
    });
    cx.run_until_parked();
    for (id, expected) in [
        ("title-bar", TITLE_BAR_FIELD),
        ("panel", CARD_FIELD),
        ("card", PANEL_FIELD),
        ("floating", HOST_FIELD),
        ("window-after", ROOT_FIELD),
    ] {
        assert!(
            observations
                .borrow()
                .iter()
                .any(|(seen, fill)| *seen == id && *fill == Some(expected.into())),
            "{id} did not refresh"
        );
    }

    observations.borrow_mut().clear();
    cx.update(|_, cx| {
        replace_control_theme_catalog(cx, test_catalog(true, 3, HOST_FIELD)).unwrap()
    });
    cx.run_until_parked();
    for id in ["title-bar", "panel", "card", "panel-after"] {
        assert!(
            observations
                .borrow()
                .iter()
                .any(|(seen, fill)| *seen == id && *fill == Some(ROOT_FIELD.into())),
            "{id} retained an omitted host override"
        );
    }
    cx.update(|_, cx| {
        let catalog = cx.global::<ControlThemeCatalog>();
        assert!(catalog.hosted_controls(ControlHost::TitleBar).is_none());
        assert!(catalog.hosted_controls(ControlHost::Panel).is_none());
        assert!(catalog.hosted_controls(ControlHost::Card).is_none());
        assert!(catalog.hosted_controls(ControlHost::Floating).is_some());
    });
}

#[gpui::test]
fn control_host_wrappers_do_not_paint_another_surface(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    for host in [
        ControlHost::Window,
        ControlHost::TitleBar,
        ControlHost::Panel,
        ControlHost::Card,
        ControlHost::Floating,
    ] {
        cx.draw(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(100.0), px(100.0)),
            |_, _| host.mount(div().size_full()),
        );
        assert_eq!(
            cx.update(|window, _| window.painted_quads().len()),
            0,
            "{host:?} must select control paints without adding fill, edge, shadow, or backdrop effects"
        );
    }
}

#[test]
fn resting_host_metrics_scale_once_with_the_complete_catalog() {
    let initial = surface_host_catalog(1, PANEL_FIELD, CARD_FIELD);
    let scaled = initial.clone().scale_metrics(1.5, 1.25);
    for host in [
        ControlHost::TitleBar,
        ControlHost::Panel,
        ControlHost::Card,
        ControlHost::Floating,
    ] {
        let original = initial.hosted_controls(host).unwrap();
        assert_ne!(scaled.hosted_controls(host), Some(original));
        assert_eq!(
            scaled.hosted_controls(host),
            Some(&original.clone().scale_metrics(1.5, 1.25))
        );
    }
    assert!(scaled.hosted_controls(ControlHost::Window).is_none());
    assert_eq!(
        scaled.installed_generation(),
        initial.installed_generation()
    );
}
