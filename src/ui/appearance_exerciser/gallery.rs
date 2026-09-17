//! Repeatable native acceptance fixtures using the production controls and appearance transaction.
use std::collections::BTreeSet;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, Render, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, px, rgba, size,
};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, Checkbox, CheckboxState, ComboBox, ComboBoxItem,
    CommandPalette, CommandPaletteItem, ControlPreviewState, DeterminateProgress, FieldState, Icon,
    IconButton, IconName, ModalLayer, OverlayScrollbar, ProgressBar, ProgressRing, ProgressSize,
    ProgressState, ResizeAxis, ResizeHandle, ScrollMetrics, SegmentedControl, SegmentedOption,
    Switch, TextInput, TooltipLayer,
};

use super::super::appearance_runtime::{self, AppearanceRuntime, WindowAppearanceOwner};
use crate::appearance::{Appearance, AppearanceMode, SchemeId, ZedImportKind};
use crate::settings::{PreviewToken, SchemeImport};

gpui::actions!(appearance_gallery, [NextGalleryFixture]);

pub(super) fn open(cx: &mut App) -> gpui::Result<()> {
    cx.bind_keys([gpui::KeyBinding::new(
        "ctrl-alt-n",
        NextGalleryFixture,
        Some("AppearanceGallery"),
    )]);
    let bounds = Bounds::centered(None, size(px(1120.0), px(830.0)), cx);
    cx.open_window(
        WindowOptions {
            window_background: appearance_runtime::window_background(cx),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Chrome Theme State Gallery".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            ..WindowOptions::default()
        },
        |window, cx| cx.new(|cx| Gallery::new(window, cx)),
    )?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fixture {
    Dark,
    Light,
    SparseDark,
    SparseLight,
    Adversarial,
}
impl Fixture {
    fn label(self) -> &'static str {
        match self {
            Self::Dark => "Built-in Dark",
            Self::Light => "Built-in Light",
            Self::SparseDark => "Sparse Zed Dark",
            Self::SparseLight => "Sparse Zed Light",
            Self::Adversarial => "Distinct authored roles",
        }
    }
    fn appearance(self) -> Appearance {
        match self {
            Self::Light | Self::SparseLight => Appearance::Light,
            _ => Appearance::Dark,
        }
    }
}
const FIXTURES: [Fixture; 5] = [
    Fixture::Dark,
    Fixture::Light,
    Fixture::SparseDark,
    Fixture::SparseLight,
    Fixture::Adversarial,
];
const STATES: [(&str, ControlPreviewState); 5] = [
    ("Normal", ControlPreviewState::Normal),
    ("Hover", ControlPreviewState::Hovered),
    ("Pressed / dragging", ControlPreviewState::Pressed),
    ("Disabled", ControlPreviewState::Normal),
    ("Keyboard focus", ControlPreviewState::Focused),
];

struct Gallery {
    window_appearance: WindowAppearanceOwner,
    preview: Option<PreviewToken>,
    fixture: Fixture,
    status: String,
    fields: Vec<Entity<TextInput>>,
    scrollbars: Vec<Entity<OverlayScrollbar<f32>>>,
    palette: Entity<CommandPalette<u8>>,
}
impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            if let Some(token) = &this.preview {
                let _ = cx
                    .global::<AppearanceRuntime>()
                    .settings
                    .cancel_preview(token);
            }
        })
        .detach();
        let mut window_appearance = WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        cx.observe_global_in::<appearance_runtime::InstalledAppearance>(
            window,
            |this, window, cx| {
                this.window_appearance.apply(window, cx);
                cx.notify();
            },
        )
        .detach();
        let fields = (0..5)
            .map(|index| {
                cx.new(|cx| {
                    TextInput::new(
                        SharedString::from(format!("gallery-field-{index}")),
                        "Field presentation fixture",
                        if index == 0 {
                            ""
                        } else {
                            "Selected glyph sample"
                        },
                        window,
                        cx,
                    )
                    .placeholder("Readable placeholder")
                    .enabled(index != 3)
                })
            })
            .collect();
        let scrollbars = STATES
            .iter()
            .enumerate()
            .map(|(index, (_, state))| {
                cx.new(|cx| {
                    let mut scrollbar = OverlayScrollbar::new(match index {
                        0 => "gallery-scroll-normal",
                        1 => "gallery-scroll-hover",
                        2 => "gallery-scroll-drag",
                        3 => "gallery-scroll-disabled",
                        _ => "gallery-scroll-focus",
                    })
                    .persistent()
                    .preview_state(*state);
                    scrollbar.sync(ScrollMetrics::for_pixels(0.0, 60.0, 240.0, 60.0), cx);
                    scrollbar
                })
            })
            .collect();
        let palette = cx.new(|cx| {
            CommandPalette::new(
                "Match labels and descriptions",
                vec![
                    CommandPaletteItem::new(1, "Open Workspace")
                        .description("Open a local folder")
                        .leading_icon(|color, size| {
                            Icon::new(IconName::Folder, size, color).into_any_element()
                        }),
                    CommandPaletteItem::new(2, "Open Remote Workspace")
                        .description("Open a remote folder")
                        .leading_icon(|color, size| {
                            Icon::new(IconName::Globe, size, color).into_any_element()
                        }),
                    CommandPaletteItem::new(3, "Unavailable Workspace")
                        .description("Disabled description")
                        .disabled(true),
                ],
                window,
                cx,
            )
        });
        let mut this = Self {
            window_appearance,
            preview: None,
            fixture: Fixture::Dark,
            status: "Ready. Fixture changes are preview-only; fields retain their contents.".into(),
            fields,
            scrollbars,
            palette,
        };
        this.select(Fixture::Dark, cx);
        this
    }

    fn select(&mut self, fixture: Fixture, cx: &mut Context<Self>) {
        let result: Result<(), &'static str> = (|| {
            let settings = cx.global::<AppearanceRuntime>().settings.clone();
            if self.preview.is_none() {
                self.preview = Some(
                    settings
                        .begin_preview(settings.snapshot().committed.revision)
                        .map_err(|_| "Cancel any other appearance preview first")?,
                );
            }
            let token = self.preview.as_ref().unwrap();
            let appearance = fixture.appearance();
            let id = if matches!(fixture, Fixture::SparseDark | Fixture::SparseLight) {
                let (mode, bg, text) = if appearance == Appearance::Dark {
                    ("dark", "#081a24", "#e8f5ee")
                } else {
                    ("light", "#fff4e5", "#30271e")
                };
                let bytes=serde_json::json!({"name":"SpaceTerm acceptance fixtures","author":"SpaceTerm","themes":[{"name":fixture.label(),"appearance":mode,"style":{"background":bg,"text":text}}]}).to_string();
                let incoming =
                    crate::appearance::import_zed(bytes.as_bytes(), 0, &[ZedImportKind::Chrome])
                        .map_err(|_| "Fixture source invalid")?
                        .into_iter()
                        .map(|scheme| scheme.id().clone())
                        .collect::<BTreeSet<_>>();
                let snapshot = settings.snapshot();
                let replace = snapshot
                    .candidate
                    .custom_schemes
                    .iter()
                    .map(|scheme| scheme.id().clone())
                    .filter(|id| incoming.contains(id))
                    .collect::<BTreeSet<_>>();
                settings
                    .import_preview(
                        token,
                        snapshot.catalog_revision,
                        SchemeImport::Zed {
                            bytes: bytes.as_bytes(),
                            candidate_index: 0,
                            kinds: &[ZedImportKind::Chrome],
                        },
                        &replace,
                    )
                    .map_err(|_| "Fixture import failed")?
                    .installed
                    .into_iter()
                    .next()
                    .ok_or("Fixture import was empty")?
            } else {
                SchemeId::new(if appearance == Appearance::Light {
                    "builtin.spaceterm.chrome.light"
                } else {
                    "builtin.spaceterm.chrome.dark"
                })
                .unwrap()
            };
            let mut candidate = (*settings.snapshot().candidate).clone();
            candidate.preferences.mode = AppearanceMode::from(appearance);
            candidate.preferences.chrome.overrides.clear();
            candidate
                .preferences
                .chrome
                .schemes
                .set(appearance, id.clone());
            if fixture == Fixture::Adversarial {
                let colors = serde_json::json!({
                    "primary_background":"#f7da60","primary_foreground":"#202535","primary_icon":"#643700","primary_hover_background":"#7ae1bc","primary_hover_foreground":"#173d28","primary_hover_icon":"#583375","primary_pressed_background":"#c7a3f1","primary_pressed_foreground":"#271045","primary_pressed_icon":"#083f4c",
                    "selection_background":"#335784","selection_foreground":"#fbf2c9","selection_hover_background":"#753969","selection_hover_foreground":"#d7ffe5",
                    "destructive_background":"#a2253f","destructive_foreground":"#fff4ef","destructive_hover_background":"#ffc5a1","destructive_hover_foreground":"#4c0e17","destructive_pressed_background":"#753944","destructive_pressed_foreground":"#f9e078",
                    "input_background":"#ffffff00","input_text":"#f6f3e9","input_selection_background":"#f6f3e9","input_selection_foreground":"#101b25",
                    "row_selected_background":"#e5d7fa","row_selected_foreground":"#25113d","row_selected_secondary":"#4b2153","row_selected_icon":"#075348","row_selected_match":"#941d43","row_hover_background":"#b6e9d7","row_hover_foreground":"#123b26","row_hover_secondary":"#573726","row_hover_icon":"#472174","row_hover_match":"#85323c",
                    "scrollbar_thumb_background":"#809dcd","scrollbar_thumb_hover_background":"#e2b067","scrollbar_thumb_active_background":"#9bd49c"
                });
                candidate.preferences.chrome.overrides.insert(
                    id,
                    serde_json::from_value(colors).map_err(|_| "Adversarial fixture invalid")?,
                );
            }
            settings
                .update_preview(token, candidate)
                .map_err(|_| "Fixture preview failed")?;
            Ok(())
        })();
        self.status = match result {
            Ok(()) => {
                self.fixture = fixture;
                format!(
                    "{} • production compiler • shared appearance • preview only",
                    fixture.label()
                )
            }
            Err(message) => message.to_owned(),
        };
        cx.notify();
    }
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::super::appearance::chrome(cx).clone();
        let weak = cx.weak_entity();
        let cell = |label: &'static str| div().w(px(170.0)).flex_none().child(label);
        let mut content = div()
            .id("gallery-scroll")
            .key_context("AppearanceGallery")
            .on_action(cx.listener(|this, _: &NextGalleryFixture, _, cx| {
                let index = FIXTURES
                    .iter()
                    .position(|fixture| *fixture == this.fixture)
                    .unwrap_or(0);
                this.select(FIXTURES[(index + 1) % FIXTURES.len()], cx);
            }))
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(9.0))
            .p(px(18.0))
            .bg(rgba(appearance.colors.background.rgba_hex()))
            .text_color(rgba(appearance.colors.text.rgba_hex()))
            .font(appearance.regular.clone())
            .text_size(px(12.0))
            .child(
                div()
                    .font(appearance.heading.clone())
                    .text_size(px(18.0))
                    .child("Chrome theme state gallery"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .children(FIXTURES.into_iter().enumerate().map(|(index, fixture)| {
                        let weak = weak.clone();
                        Button::new(("gallery-fixture", index), fixture.label())
                            .size(ButtonSize::Small)
                            .on_activate(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| this.select(fixture, cx));
                            })
                    })),
            )
            .child(
                div()
                    .text_color(rgba(appearance.colors.text_muted.rgba_hex()))
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(div().w(px(130.0)))
                    .children(STATES.iter().map(|(label, _)| cell(label))),
            );
        for (row_index, (label, variant)) in [
            ("Primary action", ButtonVariant::Primary),
            ("Destructive action", ButtonVariant::Destructive),
            ("Secondary action", ButtonVariant::Secondary),
        ]
        .into_iter()
        .enumerate()
        {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().w(px(130.0)).child(label))
                    .children(STATES.iter().enumerate().map(|(index, (_, state))| {
                        div().w(px(170.0)).child(
                            Button::new(("gallery-button", row_index * 5 + index), "Action")
                                .variant(variant)
                                .size(ButtonSize::Regular)
                                .disabled(index == 3)
                                .preview_state(*state)
                                .leading(|color| {
                                    Icon::new(IconName::Plus, px(14.0), color).into_any_element()
                                })
                                .on_activate(|_, _, _| {}),
                        )
                    })),
            );
        }
        content = content.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(div().w(px(130.0)).child("Persistent selection"))
                .children(STATES.iter().enumerate().map(|(index, (_, state))| {
                    div().w(px(170.0)).child(
                        SegmentedControl::new(
                            ("gallery-segment", index),
                            "Selection fixture",
                            &true,
                            vec![
                                SegmentedOption::new(false, "Off"),
                                SegmentedOption::new(true, "On"),
                            ],
                        )
                        .unwrap()
                        .disabled(index == 3)
                        .preview_state(*state)
                        .on_change(|_, _, _| {}),
                    )
                })),
        );
        for (row_index, (label, value)) in [
            ("Unchecked", CheckboxState::Unchecked),
            ("Checked", CheckboxState::Checked),
            ("Mixed", CheckboxState::Mixed),
        ]
        .into_iter()
        .enumerate()
        {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().w(px(130.0)).child(label))
                    .children(STATES.iter().enumerate().map(|(index, (_, state))| {
                        div().w(px(170.0)).child(
                            Checkbox::new(
                                ("gallery-checkbox", row_index * 5 + index),
                                "Choice",
                                value,
                            )
                            .disabled(index == 3)
                            .preview_state(*state)
                            .on_change(|_, _, _| {}),
                        )
                    })),
            );
        }
        content = content.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(div().w(px(130.0)).child("Switch on"))
                .children(STATES.iter().enumerate().map(|(index, (_, state))| {
                    div().w(px(170.0)).child(
                        Switch::new(("gallery-switch", index), "Enabled", true)
                            .disabled(index == 3)
                            .preview_state(*state)
                            .on_change(|_, _, _| {}),
                    )
                })),
        );
        content = content.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(div().w(px(130.0)).child("Fields"))
                .children(self.fields.iter().enumerate().map(|(index, input)| {
                    div()
                        .w(px(170.0))
                        .flex()
                        .flex_col()
                        .gap(px(5.0))
                        .child(
                            spaceterm_ui::field_frame(
                                ("gallery-field-frame", index),
                                &input.read(cx).focus_handle(),
                                FieldState::default()
                                    .disabled(index == 3)
                                    .invalid(index == 2 || index == 4)
                                    .preview_focus(index == 1 || index == 4),
                                cx,
                            )
                            .rounded(px(5.0))
                            .px(px(8.0))
                            .h(px(30.0))
                            .child(input.clone()),
                        )
                        .child(
                            [
                                "Placeholder",
                                "Focused",
                                "Invalid",
                                "Disabled",
                                "Invalid + focus",
                            ][index],
                        )
                })),
        );
        content = content.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(div().w(px(130.0)).child("Resize and scrollbar"))
                .children(STATES.iter().enumerate().map(|(index, (_, state))| {
                    div()
                        .relative()
                        .w(px(170.0))
                        .h(px(70.0))
                        .child(
                            div().h(px(60.0)).child(
                                ResizeHandle::new(
                                    ("gallery-resize", index),
                                    "Resize fixture",
                                    ResizeAxis::Horizontal,
                                    0.0,
                                )
                                .disabled(index == 3)
                                .preview_state(*state)
                                .on_event(|_, _, _| {}),
                            ),
                        )
                        .when(index != 3, |cell| {
                            cell.child(self.scrollbars[index].clone())
                        })
                })),
        );
        let determinate = |value: f64| {
            ProgressState::Determinate(
                DeterminateProgress::new(value).expect("gallery progress fixtures are finite"),
            )
        };
        let progress_columns = [
            ("Zero", determinate(0.0)),
            ("Partial", determinate(0.4)),
            ("Full", determinate(1.0)),
            ("Indeterminate", ProgressState::Indeterminate),
        ];
        content = content
            .child(
                div()
                    .text_color(rgba(appearance.colors.text_muted.rgba_hex()))
                    .child("Progress: determinate extents and the installed indeterminate motion"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(div().w(px(130.0)))
                    .children(progress_columns.iter().map(|(label, _)| cell(label))),
            );
        for (row_index, (label, size)) in [
            ("Bar compact", ProgressSize::Compact),
            ("Bar regular", ProgressSize::Regular),
        ]
        .into_iter()
        .enumerate()
        {
            content =
                content.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(div().w(px(130.0)).child(label))
                        .children(progress_columns.iter().enumerate().map(
                            |(index, (_, state))| {
                                div().w(px(170.0)).child(
                                    ProgressBar::new(
                                        ("gallery-progress-bar", row_index * 4 + index),
                                        "Gallery progress fixture",
                                        *state,
                                    )
                                    .size(size),
                                )
                            },
                        )),
                );
        }
        for (row_index, (label, size)) in [
            ("Ring compact", ProgressSize::Compact),
            ("Ring regular", ProgressSize::Regular),
        ]
        .into_iter()
        .enumerate()
        {
            content =
                content.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(div().w(px(130.0)).child(label))
                        .children(progress_columns.iter().enumerate().map(
                            |(index, (_, state))| {
                                div().w(px(170.0)).child(
                                    ProgressRing::new(
                                        ("gallery-progress-ring", row_index * 4 + index),
                                        "Gallery progress fixture",
                                        *state,
                                    )
                                    .size(size),
                                )
                            },
                        )),
                );
        }
        let palette = self.palette.clone();
        content = content.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    Button::new(
                        "gallery-open-palette",
                        "Open palette: icons, descriptions, matches",
                    )
                    .on_activate(move |_, window, cx| {
                        palette.update(cx, |palette, cx| {
                            palette.open(window, cx);
                            palette.set_query("Open", cx);
                        });
                    }),
                )
                .child(
                    ComboBox::new(
                        "gallery-combo",
                        "Row states",
                        Some(1_u8),
                        "Choose",
                        vec![
                            ComboBoxItem::new(1, "Selected row")
                                .description("Selected secondary text"),
                            ComboBoxItem::new(2, "Another row")
                                .description("Hovered secondary text"),
                        ],
                    )
                    .on_accept(|_, _, _| {}),
                )
                .child(
                    IconButton::new("gallery-icon", "Icon-only action", |color| {
                        Icon::new(IconName::Plus, px(14.0), color).into_any_element()
                    })
                    .variant(ButtonVariant::Primary)
                    .on_activate(|_, _, _| {}),
                ),
        );
        content=content.child(div().text_color(rgba(appearance.colors.text_muted.rgba_hex())).child("Columns pin visual states only; focus and drag handlers remain unarmed. Edit a field and press Ctrl+Alt+N to change fixtures while retaining focus or an open list. Real Workspace window verifies Tabs and opposite-scheme Pane Captions."));
        ModalLayer::new(TooltipLayer::new(content.child(self.palette.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{rc::Rc, sync::Arc};
    #[gpui::test]
    fn gallery_cycles_all_production_fixtures_repeatedly_without_losing_editor_state(
        cx: &mut TestAppContext,
    ) {
        let (settings, changed) = crate::settings::UserSettings::load(Arc::new(
            super::super::tests::ReadOnlyExerciserStorage,
        ));
        let platform = crate::platform::appearance::testing::RecordingAppearancePlatform::default();
        platform.set_system_appearance(Some(Appearance::Dark));
        cx.update(|cx| {
            appearance_runtime::install(settings.clone(), changed, Rc::new(platform), cx).unwrap();
            crate::ui::init(cx).unwrap();
        });
        let (gallery, cx) = cx.add_window_view(Gallery::new);
        gallery.update(cx, |gallery, cx| {
            gallery.fields[0].update(cx, |input, cx| {
                input.set_value("Retained fixture input", cx);
            });
        });
        for fixture in FIXTURES.into_iter().cycle().take(10) {
            gallery.update(cx, |gallery, cx| gallery.select(fixture, cx));
            cx.run_until_parked();
            gallery.read_with(cx, |gallery, cx| {
                assert_eq!(gallery.fixture, fixture, "{}", gallery.status);
                assert_eq!(gallery.fields[0].read(cx).value(), "Retained fixture input");
                assert_eq!(
                    appearance_runtime::current(cx).chrome.appearance,
                    fixture.appearance()
                );
            });
        }
        assert_eq!(settings.snapshot().candidate.custom_schemes.len(), 2);
    }
}
