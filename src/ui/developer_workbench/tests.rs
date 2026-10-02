use std::{rc::Rc, sync::Arc};

use gpui::{Modifiers, TestAppContext, VisualTestContext, px, size};

use super::*;
use crate::appearance::{Appearance, AppearanceMode, ChromeDensity};
use crate::platform::appearance::AppearancePlatform as _;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::UserSettings;

/// Storage with no settings file that fails every write, so a test proves a preview never saves.
pub(super) struct ReadOnlyStorage;

impl crate::settings::storage::SettingsStorage for ReadOnlyStorage {
    fn quarantine(&self) -> Result<(), crate::settings::storage::StorageError> {
        Err(crate::settings::storage::StorageError::Unavailable)
    }

    fn read(
        &self,
    ) -> Result<
        Option<crate::platform::secure_filesystem::PrivateFileSnapshot>,
        crate::settings::storage::StorageError,
    > {
        Ok(None)
    }

    fn write(
        &self,
        _: &[u8],
        _: Option<&crate::platform::secure_filesystem::SecureEntryIdentity>,
    ) -> Result<crate::settings::storage::StorageCommit, crate::settings::storage::StorageError>
    {
        panic!("a Developer Workbench preview must not write")
    }
}

#[derive(Default)]
struct RecordingMovement(Rc<RecordingOperatingSystemWindowDragPlatform>);

impl WindowMovementFactory for RecordingMovement {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        self.0.clone()
    }
}

/// Installs appearance over read-only settings, with the system in Dark.
fn install(cx: &mut TestAppContext) -> (UserSettings, RecordingAppearancePlatform) {
    let settings = UserSettings::load(Arc::new(ReadOnlyStorage));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    platform.set_native_window_transparency_supported(true);
    cx.update(|cx| {
        appearance_runtime::install(settings.clone(), Rc::new(platform.clone()), cx).unwrap();
        crate::ui::init(cx).unwrap();
    });
    (settings, platform)
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
    let (settings, platform) = install(cx);
    let (_, cx) = open_workbench_window(cx);
    assert!(
        cx.debug_bounds("workbench-diagnostics-generation-0")
            .is_some()
    );

    let token = settings.begin_preview(0).unwrap();
    let mut candidate = crate::appearance::SettingsDocument::default();
    candidate.preferences.mode = AppearanceMode::Auto;
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
    let (settings, _) = install(cx);
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
}

#[gpui::test]
fn simulations_apply_and_system_settings_ends_them(cx: &mut TestAppContext) {
    let (_, platform) = install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    let reduce_transparency = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            appearance_runtime::current(cx)
                .chrome
                .composition
                .capabilities
                .reduce_transparency
        })
    };

    workbench.update(cx, |workbench, cx| {
        workbench.simulate(
            Simulation::Accessibility(AccessibilityPreviewFact::ReduceTransparency),
            cx,
        );
    });
    cx.run_until_parked();

    assert!(reduce_transparency(cx));
    assert!(!platform.accessibility_display_options().reduce_transparency);
    assert!(status(&workbench, cx).contains("System Settings are unchanged"));

    workbench.update(cx, |workbench, cx| {
        workbench.simulate(Simulation::InactiveWindow, cx);
        workbench.simulate(Simulation::SystemSettings, cx);
    });
    cx.run_until_parked();

    assert!(!reduce_transparency(cx));
    assert!(cx.update(|_, cx| !workbench.read(cx).simulate_inactive));
    assert_eq!(
        status(&workbench, cx),
        "Simulations are off. Accessibility follows System Settings."
    );
}

#[gpui::test]
fn simulating_differentiate_without_color_gives_status_fixtures_their_shapes(
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
    let (settings, _) = install(cx);
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
    let (settings, _) = install(cx);
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
    assert_eq!(movement.counts(), (0, 0, 0, 0));
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
    assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));

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
        assert!(cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));
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
        for transparency in [0.0, 0.35, 1.0] {
            workbench.update(cx, |workbench, cx| {
                workbench.preview.set_mode(mode).unwrap();
                workbench.preview.set_transparency(transparency).unwrap();
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
                                "{mode:?} {transparency} {activity:?} {role:?} crossed below L* 50 over {endpoint:?}: {lightness:.2}"
                            ),
                            Appearance::Dark => assert!(
                                lightness < 50.0,
                                "{mode:?} {transparency} {activity:?} {role:?} crossed above L* 50 over {endpoint:?}: {lightness:.2}"
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
    let (settings, _) = install(cx);
    let (workbench, cx) = open_workbench_window(cx);
    click("workbench-navigation-workbench-section-controls", cx);
    let preferences = settings.snapshot().candidate.preferences.clone();

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
    assert_eq!(settings.snapshot().candidate.preferences, preferences);

    simulate_inactive(cx);
    settle_focus_rings(cx);

    assert_eq!(hover_fill(cx), active_fill);
    assert!(paints_focus_ring(cx));
    assert_eq!(settings.snapshot().candidate.preferences, preferences);
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
    assert_eq!(
        menus.size.height, picker.size.height,
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
