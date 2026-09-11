use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{Context, ParentElement as _, Render, TestAppContext, Window, div, px, rgba};

use crate::*;

fn catalog(generation: u64) -> ControlThemeCatalog {
    let clear = rgba(0x00000000);
    let text = rgba(0xffffffff);
    let surface = rgba(0x202024ff);
    let muted = rgba(0x808088ff);
    let accent = rgba(0x5599ffff);
    let button_paint = ButtonPaint::new(surface, text, clear);
    let button_variant = ButtonVariantStyle::new(
        button_paint,
        button_paint,
        button_paint,
        ButtonPaint::new(clear, muted, clear),
    );
    let button_metrics = ButtonMetrics::new(px(28.0));
    let button = ButtonTheme::new(
        ButtonVariants::new(
            button_variant,
            button_variant,
            button_variant,
            button_variant,
            button_variant,
            button_variant,
            button_variant,
        ),
        ButtonSizes::new(
            button_metrics,
            button_metrics,
            button_metrics,
            button_metrics,
        ),
        accent,
    );
    let toggle_value = ToggleValuePaints::new(
        TogglePaint::new(surface, text, muted, text),
        TogglePaint::new(accent, text, accent, text),
    );
    let toggle_metrics = ToggleMetrics::new(px(24.0), px(16.0), px(34.0), px(18.0));
    let toggle = ToggleTheme::new(
        TogglePaints::new(toggle_value, toggle_value, toggle_value, toggle_value),
        ToggleSizes::new(toggle_metrics, toggle_metrics),
        accent,
    );
    let menu_paint = MenuPaint::new(
        surface, surface, text, muted, muted, surface, text, accent, surface,
    );
    let menu_metrics = MenuMetrics::new(px(180.0), px(28.0));
    let menu = MenuTheme::new(
        menu_paint,
        MenuSizes::new(menu_metrics, menu_metrics, menu_metrics),
    );
    let text_input_paint = TextInputPaint::new(text, muted, accent, text, muted, accent);
    ControlThemeCatalog::new(
        button,
        toggle,
        ScrollbarTheme::new(muted, text, accent),
        ResizeHandleTheme::new(
            ResizeHandlePaint::new(muted, text, accent, accent, muted),
            ResizeHandleMetrics::new(px(1.0), px(8.0)),
        ),
        menu,
        CommandPaletteTheme::new(
            CommandPalettePaint::new(surface, surface, text, muted, muted, surface, text, accent),
            CommandPaletteMetrics::new(px(420.0), px(40.0)),
        ),
        ComboBoxTheme::new(
            ComboBoxPaint::new(
                surface, surface, text, muted, muted, surface, text, surface, surface, muted,
                accent,
            ),
            ComboBoxMetrics::new(px(240.0), px(40.0)),
        ),
        TextInputTheme::new(
            TextInputVariants::new(text_input_paint, text_input_paint),
            TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(20.0)),
        ),
        TooltipTheme::new(
            TooltipPaint::new(surface, surface, text, muted, muted),
            TooltipMetrics::new(px(320.0)),
        ),
        ModalTheme::new(
            ModalPaint::new(
                rgba(0x00000099),
                surface,
                muted,
                text,
                muted,
                muted,
                surface,
                accent,
                accent,
                surface,
                accent,
                surface,
                accent,
                surface,
            ),
            ModalMetrics::new(px(360.0), px(480.0), px(640.0)),
        ),
    )
    .generation(ControlThemeGeneration::new(generation))
}

struct CatalogObserver {
    renders: Rc<Cell<usize>>,
}

impl Render for CatalogObserver {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let _ = cx.global::<ButtonTheme>();
        let _ = cx.global::<ToggleTheme>();
        let _ = cx.global::<ScrollbarTheme>();
        let _ = cx.global::<ResizeHandleTheme>();
        let _ = cx.global::<MenuTheme>();
        let _ = cx.global::<CommandPaletteTheme>();
        let _ = cx.global::<ComboBoxTheme>();
        let _ = cx.global::<TextInputTheme>();
        let _ = cx.global::<TooltipTheme>();
        let _ = cx.global::<ModalTheme>();
        let _ = cx.global::<ControlThemeCatalog>();
        self.renders.set(self.renders.get() + 1);
        div().child(
            ComboBox::new(
                "catalog-observer-combo",
                "Catalog observer",
                None,
                "Choose",
                vec![ComboBoxItem::new(1_u8, "One")],
            )
            .debug_selector("catalog-observer-combo"),
        )
    }
}

#[gpui::test]
fn replacement_should_require_initialization(cx: &mut TestAppContext) {
    assert_eq!(
        cx.update(|cx| replace_control_theme_catalog(cx, catalog(1))),
        Err(ControlThemeReplacementError)
    );
}

#[gpui::test]
fn replacement_should_publish_all_families_and_refresh_observers(cx: &mut TestAppContext) {
    let initial = catalog(1);
    cx.update(|cx| init(cx, initial.clone()))
        .expect("control initialization should succeed");
    let renders = Rc::new(Cell::new(0));
    let observed_renders = Rc::clone(&renders);
    let (_, cx) = cx.add_window_view(move |_, _| CatalogObserver {
        renders: observed_renders,
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let trigger = cx
        .debug_bounds("catalog-observer-combo")
        .expect("combo trigger should render");
    cx.simulate_click(trigger.center(), gpui::Modifiers::none());
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    let before = renders.get();

    let replacement = initial
        .clone()
        .scale_metrics(1.5, 1.25)
        .generation(ControlThemeGeneration::new(2));
    assert_eq!(
        cx.update(|_, cx| replace_control_theme_catalog(cx, replacement.clone())),
        Ok(ControlThemeReplacement::Applied)
    );
    cx.run_until_parked();
    assert!(renders.get() > before);
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    cx.update(|_, cx| {
        assert_eq!(cx.global::<ButtonTheme>(), &replacement.button);
        assert_eq!(cx.global::<ToggleTheme>(), &replacement.toggle);
        assert_eq!(cx.global::<ScrollbarTheme>(), &replacement.scrollbar);
        assert_eq!(cx.global::<ResizeHandleTheme>(), &replacement.resize_handle);
        assert_eq!(cx.global::<MenuTheme>(), &replacement.menu);
        assert_eq!(
            cx.global::<CommandPaletteTheme>(),
            &replacement.command_palette
        );
        assert_eq!(cx.global::<ComboBoxTheme>(), &replacement.combo_box);
        assert_eq!(cx.global::<TextInputTheme>(), &replacement.text_input);
        assert_eq!(cx.global::<TooltipTheme>(), &replacement.tooltip);
        assert_eq!(cx.global::<ModalTheme>(), &replacement.modal);
        assert_eq!(cx.global::<ControlThemeCatalog>(), &replacement);
        assert_eq!(
            replace_control_theme_catalog(cx, replacement),
            Ok(ControlThemeReplacement::Unchanged)
        );
    });
}
