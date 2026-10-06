use std::{rc::Rc, sync::Arc};

use gpui::{Modifiers, TestAppContext, VisualTestContext, px, size};

use super::*;
use crate::appearance::{Appearance, AppearanceMode, ChromeDensity};
use crate::platform::appearance::AppearancePlatform as _;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::Settings;
use crate::settings::storage::testing::MemoryStorage;

#[derive(Default)]
struct RecordingMovement(Rc<RecordingOperatingSystemWindowDragPlatform>);

impl WindowMovementFactory for RecordingMovement {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        self.0.clone()
    }
}

/// Installs appearance over empty settings, with the system in Dark.
fn install(cx: &mut TestAppContext) -> (Settings, RecordingAppearancePlatform, Arc<MemoryStorage>) {
    let storage = Arc::new(MemoryStorage::default());
    let settings = Settings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    platform.set_native_window_opacity_supported(true);
    cx.update(|cx| {
        appearance_runtime::install(settings.clone(), Rc::new(platform.clone()), cx).unwrap();
        crate::ui::init(cx).unwrap();
    });
    (settings, platform, storage)
}

fn open_workbench_window(
    cx: &mut TestAppContext,
) -> (Entity<DeveloperWorkbench>, &mut VisualTestContext) {
    let (workbench, cx) = cx.add_window_view(DeveloperWorkbench::new);
    cx.simulate_resize(size(px(1600.0), px(2400.0)));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (workbench, cx)
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn status(workbench: &Entity<DeveloperWorkbench>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| workbench.read(cx).status.to_string())
}

#[gpui::test]
fn unavailable_window_effects_show_defaults_and_refuse_preview_edits(cx: &mut TestAppContext) {
    let (_, platform, _) = install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    workbench.update(cx, |workbench, cx| {
        workbench.apply(|preview| preview.set_opacity(0.2), "Fixture opacity", cx);
        workbench.apply(|preview| preview.set_blur(false), "Fixture blur", cx);
    });
    cx.run_until_parked();
    for show_borders in [false, true] {
        platform.set_show_borders(show_borders);
        cx.run_until_parked();
        for (opacity, blur) in [(false, false), (true, false), (false, true)] {
            platform.set_native_window_opacity_supported(opacity);
            platform.set_native_window_blur_supported(blur);
            cx.run_until_parked();
            let before = workbench.read_with(cx, |workbench, _| workbench.preview.document());
            let before_status = status(&workbench, cx);
            let track = cx.debug_bounds("workbench-blur-indicator").unwrap();
            let thumb = cx.debug_bounds("workbench-blur-thumb").unwrap();
            assert!(
                thumb.center().x > track.center().x,
                "Blur must show its effective default"
            );
            // The disabled selected segment still presents the effective Default stop.
            let default_bounds = cx.debug_bounds("workbench-opacity-default").unwrap();
            let minimum_bounds = cx.debug_bounds("workbench-opacity-minimum").unwrap();
            cx.update(|window, cx| {
                let surface = crate::ui::appearance::settings::shared(cx);
                let appearance = &surface.chrome;
                let theme = crate::ui::control_theme::segmented_control::prepared(
                    &appearance.card_controls.segmented,
                    &appearance.typography,
                    appearance.capabilities.show_borders,
                );
                let fill = |bounds: gpui::Bounds<gpui::Pixels>| {
                    window
                        .painted_quads()
                        .iter()
                        .find(|quad| quad.bounds == bounds.scale(window.scale_factor()))
                        .map(|quad| quad.background)
                };
                assert_eq!(
                    fill(default_bounds).expect("the selected segment must paint"),
                    theme.paint(true, false, false, false).background().into()
                );
                assert_eq!(
                    fill(minimum_bounds).map(|fill| gpui::Rgba::from(fill.as_solid().unwrap()).a),
                    show_borders.then(|| theme.paint(false, false, false, false).background().a)
                );
            });
            for selector in [
                "workbench-opacity-opaque",
                "workbench-opacity-minimum",
                "workbench-blur",
            ] {
                click(selector, cx);
            }
            assert_eq!(
                workbench.read_with(cx, |workbench, _| workbench.preview.document()),
                before
            );
            assert_eq!(status(&workbench, cx), before_status);
        }
    }
    // Once both effects return, the controls present and edit the retained choices again.
    platform.set_native_window_opacity_supported(true);
    platform.set_native_window_blur_supported(true);
    cx.run_until_parked();
    let track = cx.debug_bounds("workbench-blur-indicator").unwrap();
    let thumb = cx.debug_bounds("workbench-blur-thumb").unwrap();
    assert!(thumb.center().x < track.center().x);
    click("workbench-opacity-minimum", cx);
    click("workbench-blur", cx);
    let preferences = workbench.read_with(cx, |workbench, _| {
        workbench.preview.document().appearance.window
    });
    assert_eq!(preferences.opacity, 0.0);
    assert!(preferences.blur);
    for (selector, opacity) in [
        ("workbench-opacity-opaque", 1.0),
        ("workbench-opacity-default", 0.65),
        ("workbench-opacity-minimum", 0.0),
    ] {
        click(selector, cx);
        assert_eq!(
            workbench.read_with(cx, |workbench, _| {
                workbench.preview.document().appearance.window.opacity
            }),
            opacity
        );
    }
}

#[test]
fn every_section_is_reachable_by_its_launch_argument() {
    for section in WorkbenchSection::ALL {
        assert_eq!(
            WorkbenchSection::from_argument(section.argument()),
            Some(section)
        );
    }
    assert_eq!(WorkbenchSection::from_argument("Appearance"), None);
    assert_eq!(WorkbenchSection::from_argument(""), None);
}

#[gpui::test]
fn opening_twice_keeps_one_window_and_selects_the_requested_section(cx: &mut TestAppContext) {
    install(cx);
    cx.update(|cx| {
        configure_window_chrome(Rc::new(RecordingMovement::default()), cx);
        open_or_activate(None, cx);
    });
    cx.run_until_parked();
    cx.update(|cx| open_or_activate(Some(WorkbenchSection::Modals), cx));
    cx.run_until_parked();

    let windows = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<DeveloperWorkbench>())
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 1);
    let section = windows[0]
        .read_with(cx, |workbench, _| workbench.active_section)
        .unwrap();
    assert_eq!(section, WorkbenchSection::Modals);
}

#[gpui::test]
fn sections_present_their_own_fixtures(cx: &mut TestAppContext) {
    install(cx);
    let (workbench, cx) = open_workbench_window(cx);

    for (section, navigation, fixture) in [
        (
            WorkbenchSection::Appearance,
            "workbench-navigation-workbench-section-appearance",
            "workbench-density",
        ),
        (
            WorkbenchSection::Controls,
            "workbench-navigation-workbench-section-controls",
            "workbench-button-0-0",
        ),
        (
            WorkbenchSection::FloatingSurfaces,
            "workbench-navigation-workbench-section-floating-surfaces",
            "workbench-backdrop-probe",
        ),
        (
            WorkbenchSection::Modals,
            "workbench-navigation-workbench-section-modals",
            "workbench-show-alert",
        ),
        (
            WorkbenchSection::Terminal,
            "workbench-navigation-workbench-section-terminal",
            "workbench-terminal-caption",
        ),
        (
            WorkbenchSection::Document,
            "workbench-navigation-workbench-section-document",
            "workbench-document-apply",
        ),
    ] {
        click(navigation, cx);
        cx.update(|_, cx| assert_eq!(workbench.read(cx).active_section, section));
        assert!(
            cx.debug_bounds(fixture).is_some(),
            "{section:?} should present {fixture}"
        );
    }
}

#[gpui::test]
fn diagnostics_repaint_for_shared_system_changes(cx: &mut TestAppContext) {
    let (settings, platform, _) = install(cx);
    let (_, cx) = open_workbench_window(cx);
    assert!(
        cx.debug_bounds("workbench-diagnostics-generation-0")
            .is_some()
    );

    let token = settings.begin_preview(0).unwrap();
    let mut candidate = crate::settings::SettingsDocument::default();
    candidate.appearance.mode = AppearanceMode::Auto;
    settings.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();
    platform.set_system_appearance(Some(Appearance::Light));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("workbench-diagnostics-generation-1")
            .is_some()
    );
    cx.update(|_, cx| {
        let appearance = appearance_runtime::current(cx);
        assert_eq!(appearance.chrome.appearance, Appearance::Light);
        assert_eq!(appearance.terminal.appearance, Appearance::Light);
    });
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("workbench-diagnostics-generation-2")
            .is_some()
    );
}

#[gpui::test]
fn toolbar_mode_previews_without_saving_and_cancel_restores(cx: &mut TestAppContext) {
    let (settings, _, storage) = install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    let saved = settings.snapshot().committed.clone();

    click("workbench-appearance-mode-light", cx);

    cx.update(|_, cx| {
        assert!(workbench.read(cx).preview.is_open());
        assert_eq!(
            appearance_runtime::current(cx).chrome.appearance,
            Appearance::Light
        );
    });
    assert_eq!(settings.snapshot().committed, saved);

    click("workbench-cancel", cx);

    cx.update(|_, cx| {
        assert!(!workbench.read(cx).preview.is_open());
        assert_eq!(
            appearance_runtime::current(cx).chrome.appearance,
            Appearance::Dark
        );
    });
    assert_eq!(status(&workbench, cx), "Preview cancelled.");
    assert_eq!(storage.writes(), 0);
}

fn open_framed_workbench(
    movement: Rc<RecordingOperatingSystemWindowDragPlatform>,
    cx: &mut TestAppContext,
) -> WindowHandle<DeveloperWorkbench> {
    cx.update(|cx| {
        configure_window_chrome(Rc::new(RecordingMovement(movement)), cx);
        open_or_activate(None, cx);
        cx.global::<OpenWorkbench>().0
    })
}

#[gpui::test]
fn toolbar_and_window_controls_keep_separate_space_at_every_density(cx: &mut TestAppContext) {
    install(cx);
    let movement = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
    let window = open_framed_workbench(movement.clone(), cx);
    let workbench = window.root(cx).unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, _| window.activate_window());

    for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
        workbench.update(cx, |workbench, cx| {
            workbench.apply(
                |preview| preview.set_density(density),
                "Density changed",
                cx,
            );
        });
        for client in [false, true] {
            let inset = if client {
                crate::platform::window_chrome::CLIENT_FRAME_INSET * 2.0
            } else {
                0.0
            };
            cx.simulate_decorations(if client {
                gpui::Decorations::Client {
                    tiling: gpui::Tiling::default(),
                }
            } else {
                gpui::Decorations::Server
            });
            cx.simulate_resize(size(px(WINDOW_WIDTH + inset), px(WINDOW_HEIGHT + inset)));
            workbench.update(cx, |workbench, cx| {
                workbench.set_mode(AppearanceMode::Dark, cx)
            });
            cx.run_until_parked();

            let surface = cx.debug_bounds("workbench-window-surface").unwrap();
            let heading = cx.debug_bounds("workbench-detail-heading").unwrap();
            let drag = cx
                .debug_bounds("workbench-detail-drag-region-hitbox")
                .unwrap();
            let toolbar = cx.debug_bounds("workbench-toolbar").unwrap();
            let simulate = cx.debug_bounds("workbench-simulate").unwrap();
            let gutter = cx.update(|_, cx| super::super::appearance::chrome(cx).spacing(26.0));
            assert_eq!(surface.size, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
            assert!(drag.size.width > px(0.0));
            assert!(
                drag.right() <= toolbar.left(),
                "drag {drag:?} overlaps toolbar {toolbar:?}"
            );
            assert!(toolbar.right() <= heading.right());
            if client {
                let controls = cx.debug_bounds("workbench-window-controls").unwrap();
                let close = cx.debug_bounds("window-close").unwrap();
                assert!(toolbar.right() <= controls.left());
                assert!(
                    simulate.right() < close.left(),
                    "toolbar {simulate:?} overlaps Close {close:?}"
                );
                assert!(heading.contains(&close.center()));
                let edge_margin = cx.update(|_, cx| {
                    spaceterm_ui::DesktopWindowStyle::current(cx)
                        .control_metrics()
                        .edge_margin
                });
                assert!((heading.right() - close.right() - px(edge_margin)).abs() < px(0.5));
                assert!(cx.debug_bounds("window-minimize").is_none());
                assert!(cx.debug_bounds("window-maximize").is_none());
            } else {
                assert!(cx.debug_bounds("window-close").is_none());
                assert!(cx.debug_bounds("workbench-window-controls").is_none());
                assert_eq!(toolbar.right(), heading.right());
                assert!((heading.right() - simulate.right() - gutter).abs() < px(0.5));
            }

            click("workbench-appearance-mode-light", cx);
            cx.update(|_, cx| {
                assert_eq!(
                    appearance_runtime::current(cx).chrome.appearance,
                    Appearance::Light
                );
            });
            assert_eq!(
                movement.counts(),
                (0, 0, 0),
                "toolbar clicks must not move the window"
            );
        }
    }
}

#[gpui::test]
fn simulations_apply_and_system_settings_ends_them(cx: &mut TestAppContext) {
    let (_, platform, _) = install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    let require_opaque_surfaces = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            appearance_runtime::current(cx)
                .chrome
                .composition
                .capabilities
                .require_opaque_surfaces
        })
    };

    workbench.update(cx, |workbench, cx| {
        workbench.simulate(
            Simulation::Accessibility(AccessibilityPreviewFact::RequireOpaqueSurfaces),
            cx,
        );
    });
    cx.run_until_parked();

    assert!(require_opaque_surfaces(cx));
    assert!(
        !platform
            .accessibility_display_options()
            .require_opaque_surfaces
    );
    assert!(status(&workbench, cx).contains("System Settings are unchanged"));

    workbench.update(cx, |workbench, cx| {
        workbench.simulate(Simulation::InactiveWindow, cx);
        workbench.simulate(Simulation::SystemSettings, cx);
    });
    cx.run_until_parked();

    assert!(!require_opaque_surfaces(cx));
    assert!(cx.update(|_, cx| !workbench.read(cx).simulate_inactive));
    assert_eq!(
        status(&workbench, cx),
        "Simulations are off. Accessibility follows System Settings."
    );
}

#[gpui::test]
fn differentiate_without_color_renders_two_status_cues_and_the_failed_glyph(
    cx: &mut TestAppContext,
) {
    install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    click("workbench-navigation-workbench-section-controls", cx);
    let shapes = |cx: &mut VisualTestContext| {
        (
            cx.debug_bounds("workspace-switcher-status-glyph").is_some(),
            cx.debug_bounds("workbench-status-terminal-1-progress-track")
                .is_some(),
        )
    };
    assert!(cx.debug_bounds("workbench-status-workspace-0").is_some());
    assert_eq!(shapes(cx), (false, false));

    workbench.update(cx, |workbench, cx| {
        workbench.simulate(
            Simulation::Accessibility(AccessibilityPreviewFact::DifferentiateWithoutColor),
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(shapes(cx), (true, true));
    // Every Workspace mark shares the glyph selector; the last one rendered is Failed's.
    let glyph = cx.debug_bounds("workspace-switcher-status-glyph").unwrap();
    let mark = cx.debug_bounds("workbench-status-workspace-5").unwrap();
    assert!(
        mark.contains(&glyph.center()),
        "{glyph:?} must sit in {mark:?}"
    );

    workbench.update(cx, |workbench, cx| {
        workbench.simulate(Simulation::SystemSettings, cx);
    });
    cx.run_until_parked();
    assert_eq!(shapes(cx), (false, false));
}

#[gpui::test]
fn closing_the_workbench_ends_its_preview_simulations_and_fixtures(cx: &mut TestAppContext) {
    let (settings, _, _) = install(cx);
    let window = cx.add_window(DeveloperWorkbench::new);
    window
        .update(cx, |workbench, _, cx| {
            workbench.set_mode(AppearanceMode::Light, cx);
            workbench.simulate(
                Simulation::Accessibility(AccessibilityPreviewFact::IncreaseContrast),
                cx,
            );
            terminal::set_caption_fixture(true, cx);
            terminal::set_link_preview_fixture(true, cx);
        })
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(caption_fixture(cx).is_some());
        assert!(link_preview_fixture(cx).is_some());
    });
    cx.run_until_parked();

    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let current = appearance_runtime::current(cx);
        assert_eq!(current.chrome.appearance, Appearance::Dark);
        assert!(!current.chrome.composition.capabilities.increase_contrast);
        assert!(caption_fixture(cx).is_none());
        assert!(link_preview_fixture(cx).is_none());
    });
    let revision = settings.snapshot().committed.revision;
    assert!(settings.begin_preview(revision).is_ok());
}

#[gpui::test]
fn clicking_client_close_ends_the_workbench_preview_simulations_and_fixtures(
    cx: &mut TestAppContext,
) {
    let (settings, _, _) = install(cx);
    let movement = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
    let window = open_framed_workbench(movement.clone(), cx);
    window
        .update(cx, |workbench, _, cx| {
            workbench.set_mode(AppearanceMode::Light, cx);
            workbench.simulate(
                Simulation::Accessibility(AccessibilityPreviewFact::IncreaseContrast),
                cx,
            );
            terminal::set_caption_fixture(true, cx);
            terminal::set_link_preview_fixture(true, cx);
        })
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            appearance_runtime::current(cx).chrome.appearance,
            Appearance::Light
        );
        assert!(
            appearance_runtime::current(cx)
                .chrome
                .composition
                .capabilities
                .increase_contrast
        );
        assert!(caption_fixture(cx).is_some());
        assert!(link_preview_fixture(cx).is_some());
    });

    {
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.simulate_decorations(gpui::Decorations::Client {
            tiling: gpui::Tiling::default(),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        click("window-close", cx);
    }

    assert!(!cx.windows().contains(&window.into()));
    assert_eq!(movement.counts(), (0, 0, 0));
    cx.update(|cx| {
        let current = appearance_runtime::current(cx);
        assert_eq!(current.chrome.appearance, Appearance::Dark);
        assert!(!current.chrome.composition.capabilities.increase_contrast);
        assert!(caption_fixture(cx).is_none());
        assert!(link_preview_fixture(cx).is_none());
    });
    let revision = settings.snapshot().committed.revision;
    assert!(settings.begin_preview(revision).is_ok());
}

#[gpui::test]
fn toggle_shortcut_previews_the_mode_while_the_palette_keeps_focus_and_query(
    cx: &mut TestAppContext,
) {
    install(cx);
    let window = cx.add_window(DeveloperWorkbench::new);
    cx.update(|cx| {
        init(cx);
        cx.set_global(OpenWorkbench(window));
        cx.bind_keys([gpui::KeyBinding::new(
            "cmd-alt-c",
            ToggleAppearancePreview,
            None,
        )]);
    });
    let workbench = window.root(cx).unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.simulate_resize(size(px(1600.0), px(2400.0)));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click("workbench-navigation-workbench-section-controls", cx);

    click("workbench-open-palette", cx);
    let palette = workbench.read_with(cx, |workbench, _| workbench.palette.clone());
    assert!(palette.read_with(cx, |palette, _| palette.is_open()));

    for expected in [Appearance::Light, Appearance::Dark] {
        cx.simulate_keystrokes("cmd-alt-c");
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                appearance_runtime::current(cx).chrome.appearance,
                expected,
                "{}",
                workbench.read(cx).status
            );
            assert!(palette.read(cx).is_open());
            assert_eq!(palette.read(cx).query(), "Open");
        });
        cx.simulate_input("!");
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.query().to_owned()),
            "Open!"
        );
        cx.simulate_keystrokes("backspace");
    }
}

#[gpui::test]
fn repeated_mode_toggles_keep_field_contents(cx: &mut TestAppContext) {
    install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    workbench.update(cx, |workbench, cx| {
        workbench.controls.fields[0].update(cx, |input, cx| {
            input.set_value("Retained fixture input", cx);
        });
    });

    for expected in [Appearance::Light, Appearance::Dark]
        .into_iter()
        .cycle()
        .take(10)
    {
        workbench.update(cx, |workbench, cx| workbench.toggle_mode(cx));
        cx.run_until_parked();
        cx.update(|_, cx| {
            let workbench = workbench.read(cx);
            assert_eq!(
                workbench.controls.fields[0].read(cx).value(),
                "Retained fixture input"
            );
            assert_eq!(
                appearance_runtime::current(cx).chrome.appearance,
                expected,
                "{}",
                workbench.status
            );
        });
    }
}

#[gpui::test]
fn floating_tones_stay_on_their_appearance_side(cx: &mut TestAppContext) {
    install(cx);
    let (workbench, cx) = open_workbench_window(cx);

    for (mode, appearance) in [
        (AppearanceMode::Dark, Appearance::Dark),
        (AppearanceMode::Light, Appearance::Light),
    ] {
        for opacity in [1.0, 0.65, 0.0] {
            workbench.update(cx, |workbench, cx| {
                workbench.preview.set_mode(mode).unwrap();
                workbench.preview.set_opacity(opacity).unwrap();
                cx.notify();
            });
            cx.run_until_parked();

            for activity in [
                ControlWindowActivity::Active,
                ControlWindowActivity::Inactive,
            ] {
                let prepared = cx.update(|_, cx| {
                    activity.with_scope(|| crate::ui::appearance::shared_chrome(cx))
                });
                for role in [
                    spaceterm_ui::FloatingRole::Popover,
                    spaceterm_ui::FloatingRole::Command,
                    spaceterm_ui::FloatingRole::Modal,
                    spaceterm_ui::FloatingRole::Tooltip,
                    spaceterm_ui::FloatingRole::Notice,
                    spaceterm_ui::FloatingRole::Readout,
                ] {
                    let shell = prepared.floating_surfaces().shell(role);
                    let tone = crate::appearance::Color::rgba(u32::from(shell.backdrop_tone()));
                    let wash = crate::appearance::Color::rgba(u32::from(shell.material()));
                    for endpoint in [
                        crate::appearance::Color::BLACK,
                        crate::appearance::Color::WHITE,
                    ] {
                        let lightness = cie_lightness(wash.source_over(tone.source_over(endpoint)));
                        match appearance {
                            Appearance::Light => assert!(
                                lightness >= 50.0,
                                "{mode:?} {opacity} {activity:?} {role:?} crossed below L* 50 over {endpoint:?}: {lightness:.2}"
                            ),
                            Appearance::Dark => assert!(
                                lightness < 50.0,
                                "{mode:?} {opacity} {activity:?} {role:?} crossed above L* 50 over {endpoint:?}: {lightness:.2}"
                            ),
                        }
                    }
                }
            }
        }
    }
}

fn cie_lightness(color: crate::appearance::Color) -> f64 {
    let linear = |channel: u8| {
        let channel = f64::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b);
    if luminance <= 216.0 / 24_389.0 {
        luminance * 24_389.0 / 27.0
    } else {
        116.0 * luminance.cbrt() - 16.0
    }
}

#[gpui::test]
fn inactive_window_simulation_selects_inactive_control_states_without_editing_preferences(
    cx: &mut TestAppContext,
) {
    let (settings, _, _) = install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    click("workbench-navigation-workbench-section-controls", cx);
    let preferences = settings.snapshot().candidate.appearance.clone();

    // The pinned hover column's fill, which the active and inactive catalogs paint differently.
    let hover_fill = |cx: &mut VisualTestContext| {
        let bounds = cx
            .debug_bounds("workbench-button-0-1")
            .expect("the hover button must render");
        cx.update(|window, _| {
            let bounds = bounds.scale(window.scale_factor());
            window
                .painted_quads()
                .iter()
                .find(|quad| quad.bounds.intersect(&quad.content_mask.bounds) == bounds)
                .map(|quad| quad.background)
                .expect("the hover button must paint a fill")
        })
    };
    // Test windows have no frame loop, so the ring's entrance is finished by hand.
    let settle_focus_rings = |cx: &mut VisualTestContext| {
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
    };
    let paints_focus_ring = |cx: &mut VisualTestContext| {
        let bounds = cx
            .debug_bounds("workbench-button-0-4-keyboard-focus")
            .expect("the focus ring must render");
        cx.update(|window, _| {
            let bounds = bounds.scale(window.scale_factor());
            window
                .painted_quads()
                .iter()
                .any(|quad| quad.bounds == bounds && quad.border_color.a > 0.0)
        })
    };
    let simulate_inactive = |cx: &mut VisualTestContext| {
        workbench.update(cx, |workbench, cx| {
            workbench.simulate(Simulation::InactiveWindow, cx);
        });
        cx.run_until_parked();
    };

    settle_focus_rings(cx);
    let active_fill = hover_fill(cx);
    assert!(paints_focus_ring(cx));

    simulate_inactive(cx);

    assert_ne!(hover_fill(cx), active_fill);
    assert!(!paints_focus_ring(cx));
    assert_eq!(settings.snapshot().candidate.appearance, preferences);

    simulate_inactive(cx);
    settle_focus_rings(cx);

    assert_eq!(hover_fill(cx), active_fill);
    assert!(paints_focus_ring(cx));
    assert_eq!(settings.snapshot().candidate.appearance, preferences);
}

#[gpui::test]
fn default_window_separates_groups_and_keeps_row_labels_on_one_line(cx: &mut TestAppContext) {
    install(cx);
    let (_, cx) = cx.add_window_view(DeveloperWorkbench::new);
    cx.simulate_resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    let window_group = cx.debug_bounds("workbench-group-window").unwrap();
    let terminal_group = cx.debug_bounds("workbench-group-terminal").unwrap();
    let spacing = cx.update(|_, cx| group_spacing(crate::ui::appearance::chrome(cx)));
    assert_eq!(terminal_group.top() - window_group.bottom(), spacing);

    click(
        "workbench-navigation-workbench-section-floating-surfaces",
        cx,
    );
    let menus = cx
        .debug_bounds("workbench-row-surfaces-menus-label")
        .unwrap();
    let picker = cx
        .debug_bounds("workbench-row-surfaces-picker-label")
        .unwrap();
    let line_height = cx.update(|_, cx| {
        crate::ui::appearance::settings::shared(cx)
            .chrome
            .typography
            .style(crate::ui::chrome_typography::TextRole::Body)
            .line_height
    });
    assert_eq!(
        (menus.size.height, picker.size.height),
        (line_height, line_height),
        "the menu fixtures must leave their row label one line"
    );
}

#[gpui::test]
fn single_line_fields_center_their_text_in_the_frame(cx: &mut TestAppContext) {
    install(cx);
    let (_, cx) = open_workbench_window(cx);
    click("workbench-navigation-workbench-section-controls", cx);

    let field = cx.debug_bounds("workbench-field-1").unwrap();
    let frame = cx.debug_bounds("workbench-field-frame-1").unwrap();
    assert!(
        (field.center().y - frame.center().y).abs() < px(0.5),
        "field {field:?} must be centered in frame {frame:?}"
    );
}

#[gpui::test]
fn state_matrices_and_their_text_fit_the_default_window_at_every_density(cx: &mut TestAppContext) {
    install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    cx.simulate_resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
    click("workbench-navigation-workbench-section-controls", cx);

    for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
        workbench.update(cx, |workbench, cx| {
            workbench.apply(
                |preview| preview.set_density(density),
                "Density changed",
                cx,
            );
        });
        cx.run_until_parked();

        let card = cx.debug_bounds("workbench-group-buttons-card").unwrap();
        let first = cx.debug_bounds("workbench-button-0-0").unwrap();
        let last = cx.debug_bounds("workbench-button-0-4").unwrap();
        assert!(
            last.right() <= card.right(),
            "{density:?}: the last column {last:?} must stay inside the card {card:?}"
        );
        assert!(
            card.right() - last.right() < px(40.0),
            "{density:?}: the columns {first:?} to {last:?} must span the card {card:?}"
        );
        let frame = cx.debug_bounds("workbench-field-frame-4").unwrap();
        assert_eq!(
            (frame.left(), frame.size.width),
            (last.left(), last.size.width),
            "{density:?}: every matrix must share one set of columns"
        );
        let field = cx.debug_bounds("workbench-field-4").unwrap();
        for text in [controls::FIELD_SAMPLE, controls::FIELD_PLACEHOLDER] {
            let width = cx.update(|window, cx| {
                crate::ui::appearance::settings::shared(cx)
                    .chrome
                    .typography
                    .measure(TextRole::Body, text, window)
            });
            assert!(
                width <= field.size.width,
                "{density:?}: {text:?} ({width:?}) must fit the field {field:?}"
            );
        }
    }
}
