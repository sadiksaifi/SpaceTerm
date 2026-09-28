//! The Themes section: a preview of the Terminal Theme in use and a gallery of installed themes.
//!
//! Choosing is clicking, as it is for a desktop picture: a tile applies its theme to the
//! appearance the gallery is showing. Under Auto the preview shows the Light and Dark slots side by
//! side, and selecting one points the gallery at it. A tile's context menu applies or removes it.

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, Font, SharedString, StyledText, Window, div, px, relative};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ContextMenu, MenuEntry, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalId, SearchField, TextInput, TextInputEscapeBehavior,
    TextInputEvent, TextInputReturnBehavior, TextInputVariant, fuzzy_filter,
};

use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AvailableFonts, Color, ResetTarget,
    SystemAppearance, TerminalColors, ThemeCatalog, ThemeId, ThemeSummary,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color, prepared_font};
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

use super::SettingsWindow;
use super::controls::action_button;

/// Width over height of every theme preview, large or small, so a tile reads as the same picture.
const PREVIEW_ASPECT: f32 = 1.6;
/// The width of the preview of the theme in use when one appearance is fixed.
const CURRENT_PREVIEW_WIDTH: f32 = 272.0;
/// The width of each slot's preview under Auto, small enough that the gallery stays in view.
const SLOT_PREVIEW_WIDTH: f32 = 216.0;
const GALLERY_COLUMNS: u16 = 4;
/// A gallery shorter than this is scanned by eye, so it offers no search field.
const SEARCHABLE_GALLERY: usize = 8;
/// The space between a selected preview and the ring around it.
const RING_GAP: f32 = 3.0;
const RING_WIDTH: f32 = 2.0;

pub(super) const GALLERY_SEARCH_SELECTOR: &str = "settings-theme-gallery-search";
pub(super) const GET_MORE_SELECTOR: &str = "settings-get-more-themes";

/// What a tile's context menu offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TileCommand {
    Use,
    Remove,
    RemoveExtension,
}

/// The choice a removal confirmation returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemovalChoice {
    Remove,
    Cancel,
}

/// Owns the gallery's search and the slot Auto is choosing a theme for.
pub(super) struct ThemeGallery {
    search: Entity<TextInput>,
    query: SharedString,
    /// The slot the gallery edits under Auto. Unset, it follows the appearance on screen.
    auto_slot: Option<Appearance>,
}

impl ThemeGallery {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let search = cx.new(|cx| {
            TextInput::new(
                GALLERY_SEARCH_SELECTOR,
                "Search themes",
                String::new(),
                window,
                cx,
            )
            .placeholder("Search")
            .variant(TextInputVariant::Bare)
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .input_length_limit(Some(128))
            .emit_programmatic_changes(true)
            .debug_selector(GALLERY_SEARCH_SELECTOR)
        });
        cx.subscribe(&search, |settings, search, event: &TextInputEvent, cx| {
            if matches!(event, TextInputEvent::ValueChanged(_)) {
                settings.theme_gallery.query = SharedString::from(search.read(cx).value().to_owned());
                cx.notify();
            }
        })
        .detach();
        Self {
            search,
            query: SharedString::default(),
            auto_slot: None,
        }
    }
}

/// What the preview of one slot shows: the theme chosen for it and the colors panes paint.
struct SlotPreview {
    slot: Appearance,
    /// The chosen theme, absent when it is no longer installed.
    summary: Option<ThemeSummary>,
    requested: ThemeId,
    /// The colors terminal panes use for this slot, including the person's overrides and any
    /// fallback.
    colors: TerminalColors,
    /// The theme panes fall back to when the chosen one is missing.
    effective_name: String,
}

impl SettingsWindow {
    /// The appearance whose themes the gallery shows and a click assigns.
    pub(super) fn theme_slot(&self, cx: &App) -> Appearance {
        match self.editor.document().preferences.mode {
            AppearanceMode::Light => Appearance::Light,
            AppearanceMode::Dark => Appearance::Dark,
            AppearanceMode::Auto => self.theme_gallery.auto_slot.unwrap_or_else(|| {
                crate::ui::appearance_runtime::current(cx).terminal.appearance
            }),
        }
    }

    pub(super) fn installed_themes_title(&self, cx: &App) -> &'static str {
        match self.theme_slot(cx) {
            Appearance::Light => "Light themes",
            Appearance::Dark => "Dark themes",
        }
    }

    fn select_auto_slot(&mut self, slot: Appearance, cx: &mut Context<Self>) {
        self.theme_gallery.auto_slot = Some(slot);
        cx.notify();
    }

    fn slot_preview(&self, slot: Appearance, summaries: &[ThemeSummary]) -> SlotPreview {
        let document = self.editor.document();
        let requested = document.preferences.terminal.themes.get(slot).clone();
        let catalog =
            ThemeCatalog::from_terminal_themes(&document.terminal_themes).unwrap_or_default();
        let mut preferences = document.preferences.clone();
        preferences.mode = slot.into();
        let resolved = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .ok();
        let summary = summaries
            .iter()
            .find(|summary| summary.id == requested)
            .cloned();
        let (colors, effective) = match resolved {
            Some(resolved) => (
                resolved.terminal.colors.clone(),
                resolved.terminal.effective_theme.clone(),
            ),
            None => (
                crate::appearance::builtin_terminal_base(slot),
                crate::appearance::builtin_fallback_theme(slot),
            ),
        };
        let effective_name = summaries
            .iter()
            .find(|summary| summary.id == effective)
            .map_or_else(|| effective.to_string(), |summary| summary.name.clone());
        SlotPreview {
            slot,
            summary,
            requested,
            colors,
            effective_name,
        }
    }

    /// The theme in use, large enough to judge: one preview when the appearance is fixed, and the
    /// Light and Dark slots side by side under Auto.
    pub(super) fn render_current_theme(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let summaries = self.editor.theme_summaries().unwrap_or_default();
        let font = crate::ui::appearance_runtime::current(cx)
            .terminal
            .typography
            .regular
            .clone();
        let font = prepared_font(&font);
        let content = div()
            .debug_selector(|| "settings-current-theme".to_owned())
            .w_full()
            .py(appearance.spacing(6.0));
        if self.editor.document().preferences.mode != AppearanceMode::Auto {
            let preview = self.slot_preview(self.theme_slot(cx), &summaries);
            return content
                .child(current_theme_details(&preview, font, appearance))
                .into_any_element();
        }
        let chosen = self.theme_slot(cx);
        let slots = [Appearance::Light, Appearance::Dark].map(|slot| {
            let preview = self.slot_preview(slot, &summaries);
            self.render_slot_card(preview, chosen == slot, font.clone(), appearance, cx)
        });
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        content
            .flex()
            .flex_col()
            .gap(appearance.spacing(12.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .gap(appearance.spacing(32.0))
                    .children(slots),
            )
            .child(
                div()
                    .text_center()
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(colors.text_secondary))
                    .whitespace_normal()
                    .child(
                        "Terminal panes follow the system appearance. Select Light or Dark, then \
                         choose its theme below.",
                    ),
            )
            .into_any_element()
    }

    /// One slot under Auto: its preview, its name, and the theme chosen for it.
    fn render_slot_card(
        &self,
        preview: SlotPreview,
        selected: bool,
        font: Font,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let slot = preview.slot;
        let label = match slot {
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        };
        let name = preview_name(&preview);
        let selector = format!("settings-theme-slot-{}", label.to_ascii_lowercase());
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .w(appearance.spacing(SLOT_PREVIEW_WIDTH))
            .gap(appearance.spacing(8.0))
            .child(selection_ring(
                terminal_preview(&preview.colors, font, PreviewSize::Small, appearance).w_full(),
                selected,
                RadiusRole::Card,
                appearance,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .min_w_0()
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                            .text_color(gpui_color(colors.text))
                            .child(label),
                    )
                    .child(
                        div()
                            .max_w_full()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(colors.text_secondary))
                            .child(name),
                    ),
            )
            .on_click(cx.listener(move |settings, _, _, cx| {
                settings.select_auto_slot(slot, cx);
            }))
            .into_any_element()
    }

    /// Every installed theme for the chosen slot, as tiles that apply their theme when clicked.
    pub(super) fn render_installed_themes(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slot = self.theme_slot(cx);
        let selected = self.editor.document().preferences.terminal.themes.get(slot).clone();
        let summaries = self
            .editor
            .theme_summaries()
            .unwrap_or_default()
            .into_iter()
            .filter(|summary| summary.appearance == slot)
            .collect::<Vec<_>>();
        let searchable = summaries.len() > SEARCHABLE_GALLERY;
        let query = if searchable {
            self.theme_gallery.query.clone()
        } else {
            SharedString::default()
        };
        let matches = fuzzy_filter(&summaries, &query, |summary| {
            let target = spaceterm_ui::FuzzyTarget::new(&summary.name);
            match &summary.family {
                Some(family) => target.field(family),
                None => target,
            }
        });
        let tiles = matches
            .iter()
            .map(|matched| {
                let summary = &summaries[matched.item_index()];
                self.render_theme_tile(summary, slot, summary.id == selected, appearance, cx)
            })
            .collect::<Vec<_>>();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let owner = cx.weak_entity();
        let secondary = |text: SharedString| {
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.text_secondary))
                .whitespace_normal()
                .child(text)
        };
        div()
            .debug_selector(|| "settings-installed-themes".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(14.0))
            .py(appearance.spacing(6.0))
            .when(searchable, |gallery| {
                gallery.child(
                    SearchField::new(
                        "settings-theme-gallery-search-frame",
                        self.theme_gallery.search.clone(),
                    )
                    .debug_selectors(
                        "settings-theme-gallery-search-frame",
                        "settings-theme-gallery-search-clear",
                    ),
                )
            })
            .when(tiles.is_empty(), |gallery| {
                gallery.child(secondary(SharedString::from(format!(
                    "No themes match “{query}”."
                ))))
            })
            .child(
                div()
                    .grid()
                    .grid_cols(GALLERY_COLUMNS)
                    .gap_x(appearance.spacing(16.0))
                    .gap_y(appearance.spacing(14.0))
                    .children(tiles),
            )
            // Getting more themes follows the themes it adds to, the way a list's add button does.
            .child(div().flex().justify_end().child(action_button(
                GET_MORE_SELECTOR,
                "Get More Themes…",
                self.editor.editable(),
                move |window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.open_theme_store(window, cx);
                    });
                },
            )))
            .into_any_element()
    }

    fn render_theme_tile(
        &self,
        summary: &ThemeSummary,
        slot: Appearance,
        selected: bool,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let editable = self.editor.editable();
        let id = summary.id.clone();
        let selector = format!("settings-theme-tile-{}", summary.id.as_str());
        let tile = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector({
                let selector = selector.clone();
                move || selector
            })
            .flex()
            .flex_col()
            .min_w_0()
            .gap(appearance.spacing(6.0))
            .child(selection_ring(
                theme_miniature(&summary.colors, appearance).w_full(),
                selected,
                RadiusRole::Control,
                appearance,
            ))
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .max_w_full()
                            .truncate()
                            .chrome_text(appearance.typography.style(if selected {
                                TextRole::BodyEmphasis
                            } else {
                                TextRole::Secondary
                            }))
                            .text_color(gpui_color(if selected {
                                colors.text
                            } else {
                                colors.text_secondary
                            }))
                            .child(SharedString::from(summary.name.clone())),
                    ),
            )
            .when(editable, |tile| {
                tile.on_click(cx.listener(move |settings, _, _, cx| {
                    settings.set_theme(slot, id.clone(), cx);
                }))
            });
        let mut entries = vec![
            MenuEntry::action("Use Theme", TileCommand::Use)
                .disabled(selected)
                .debug_selector(format!("{selector}-use")),
        ];
        if !summary.builtin {
            entries.push(MenuEntry::separator());
            entries.push(
                MenuEntry::action("Remove Theme", TileCommand::Remove)
                    .destructive(true)
                    .debug_selector(format!("{selector}-remove")),
            );
            if summary.package.is_some() {
                let label = match &summary.family {
                    Some(family) => format!("Remove All {family} Themes"),
                    None => String::from("Remove All Themes from This Extension"),
                };
                entries.push(
                    MenuEntry::action(label, TileCommand::RemoveExtension)
                        .destructive(true)
                        .debug_selector(format!("{selector}-remove-extension")),
                );
            }
        }
        let owner = cx.weak_entity();
        let target = summary.clone();
        ContextMenu::new(
            SharedString::from(format!("{selector}-menu")),
            SharedString::from(format!("{} options", summary.name)),
            tile,
            entries,
        )
        .disabled(!editable)
        .debug_selector(format!("{selector}-menu"))
        .on_activate(move |activation, window, cx| {
            let command = *activation.action();
            let target = target.clone();
            let _ = owner.update(cx, |settings, cx| match command {
                TileCommand::Use => settings.set_theme(slot, target.id.clone(), cx),
                TileCommand::Remove | TileCommand::RemoveExtension => {
                    settings.confirm_removal(&target, command, window, cx);
                }
            });
        })
        .into_any_element()
    }

    /// Asks before removing a theme, or every theme its extension installed, then removes them
    /// together. A slot that used a removed theme returns to its built-in theme.
    fn confirm_removal(
        &mut self,
        theme: &ThemeSummary,
        command: TileCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let summaries = self.editor.theme_summaries().unwrap_or_default();
        let ids = match (command, &theme.package) {
            (TileCommand::RemoveExtension, Some(package)) => summaries
                .iter()
                .filter(|summary| {
                    summary
                        .package
                        .as_ref()
                        .is_some_and(|installed| installed.id == package.id)
                })
                .map(|summary| summary.id.clone())
                .collect::<Vec<_>>(),
            _ => vec![theme.id.clone()],
        };
        let slots = self.editor.document().preferences.terminal.themes.clone();
        let in_use = [Appearance::Light, Appearance::Dark]
            .into_iter()
            .filter(|slot| ids.contains(slots.get(*slot)))
            .collect::<Vec<_>>();
        let (title, message) = match ids.len() {
            1 => (
                "Remove Theme",
                format!("Remove “{}”?", theme.name),
            ),
            count => (
                "Remove Themes",
                match &theme.family {
                    Some(family) => format!("Remove all {count} {family} themes?"),
                    None => format!("Remove all {count} themes from this extension?"),
                },
            ),
        };
        let one = ids.len() == 1;
        let detail = match (in_use.is_empty(), &theme.package) {
            (false, _) if one => "Terminal panes using it will switch to the built-in theme.",
            (false, _) => "Terminal panes using one of them will switch to the built-in theme.",
            (true, Some(_)) if one => "You can get it again from Get More Themes.",
            (true, Some(_)) => "You can get them again from Get More Themes.",
            (true, None) => "You can import it again from its file.",
        };
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-remove-theme"),
            title,
            title,
            message,
            vec![
                ModalAction::new(
                    RemovalChoice::Remove,
                    "Remove",
                    ModalActionRole::Affirmative,
                    "settings-remove-theme-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    RemovalChoice::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "settings-remove-theme-cancel",
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .detail(detail)
        .present(window, cx, move |outcome, cx| {
            let confirmed = matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: RemovalChoice::Remove,
                    ..
                }
            );
            if !confirmed {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                if settings.editor.remove_themes(&ids, cx).is_ok() {
                    for slot in &in_use {
                        settings.editor.reset(ResetTarget::TerminalTheme(*slot), cx);
                    }
                }
                cx.notify();
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm theme removal confirmation");
        }
    }
}

/// The theme in use beside its name, where it came from, and its sixteen colors.
fn current_theme_details(
    preview: &SlotPreview,
    font: Font,
    appearance: &ChromeAppearance,
) -> impl IntoElement {
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(appearance.spacing(22.0))
        .child(
            terminal_preview(&preview.colors, font, PreviewSize::Large, appearance)
                .rounded(RadiusRole::Card.pixels())
                .flex_none()
                .w(appearance.spacing(CURRENT_PREVIEW_WIDTH)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .gap(appearance.spacing(4.0))
                .child(
                    div()
                        .debug_selector(|| "settings-current-theme-name".to_owned())
                        .truncate()
                        .chrome_text(appearance.typography.style(TextRole::Section))
                        .text_color(gpui_color(colors.text))
                        .child(preview_name(preview)),
                )
                .child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.text_secondary))
                        .whitespace_normal()
                        .child(preview_origin(preview)),
                )
                .child(
                    div()
                        .pt(appearance.spacing(10.0))
                        .child(palette(&preview.colors, appearance)),
                ),
        )
}

fn preview_name(preview: &SlotPreview) -> SharedString {
    match &preview.summary {
        Some(summary) => SharedString::from(summary.name.clone()),
        None => SharedString::from(preview.requested.to_string()),
    }
}

/// Where the theme came from, in the words a person would use.
fn preview_origin(preview: &SlotPreview) -> SharedString {
    let Some(summary) = &preview.summary else {
        return SharedString::from(format!(
            "Not installed. Terminal panes use {}.",
            preview.effective_name
        ));
    };
    match (&summary.family, &summary.package) {
        _ if summary.builtin => SharedString::from("Built into SpaceTerm"),
        (Some(family), Some(_)) => SharedString::from(format!("{family} · Zed extension")),
        (Some(family), None) => SharedString::from(format!("{family} · Imported file")),
        (None, _) => SharedString::from("Imported file"),
    }
}

/// A ring in the accent color around a selected preview, held clear when unselected so selecting
/// never moves anything.
fn selection_ring(
    content: gpui::Div,
    selected: bool,
    radius: RadiusRole,
    appearance: &ChromeAppearance,
) -> gpui::Div {
    let accent = appearance.colors.border_focused;
    let hover = appearance.host_colors(spaceterm_ui::ControlHost::Card).border;
    div()
        .w_full()
        .p(px(RING_GAP))
        .rounded(radius.pixels() + px(RING_GAP + RING_WIDTH))
        .border(px(RING_WIDTH))
        .border_color(gpui_color(if selected {
            accent
        } else {
            Color::rgba(0x0000_0000)
        }))
        .when(!selected, |ring| {
            ring.hover(move |ring| ring.border_color(gpui_color(hover)))
        })
        .child(content.rounded(radius.pixels()))
}

#[derive(Clone, Copy)]
enum PreviewSize {
    /// The theme in use when one appearance is fixed.
    Large,
    /// Each slot under Auto.
    Small,
}

/// A few lines of a shell session in the theme's colors and the person's terminal font.
fn terminal_preview(
    colors: &TerminalColors,
    font: Font,
    size: PreviewSize,
    appearance: &ChromeAppearance,
) -> gpui::Div {
    let (text_size, line_height, inset) = match size {
        PreviewSize::Large => (11.0, 16.0, 14.0),
        PreviewSize::Small => (8.5, 12.0, 10.0),
    };
    let edge = appearance.host_colors(spaceterm_ui::ControlHost::Card).border;
    let prompt = [
        ("~/spaceterm", colors.normal[4]),
        (" main", colors.normal[5]),
        (" ❯", colors.normal[2]),
    ];
    let line = |spans: &[(&str, Color)]| {
        let text = spans.iter().map(|(text, _)| *text).collect::<String>();
        let mut offset = 0;
        let highlights = spans
            .iter()
            .map(|(text, color)| {
                let range = offset..offset + text.len();
                offset = range.end;
                (range, gpui_color(*color).into())
            })
            .collect::<Vec<_>>();
        div()
            .whitespace_nowrap()
            .overflow_hidden()
            .child(StyledText::new(text).with_highlights(highlights))
    };
    let foreground = colors.foreground;
    div()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(1.0))
        .aspect_ratio(PREVIEW_ASPECT)
        .overflow_hidden()
        .px(px(inset))
        .border(px(HAIRLINE))
        .border_color(gpui_color(edge))
        .bg(gpui_color(colors.background))
        .font(font)
        .text_size(px(text_size))
        .line_height(px(line_height))
        .text_color(gpui_color(foreground))
        .child(line(&[prompt[0], prompt[1], prompt[2], (" ls", foreground)]))
        .child(line(&[
            ("docs  src  ", colors.bright[4]),
            ("Cargo.toml  README.md", foreground),
        ]))
        .child(line(&[
            prompt[0],
            prompt[1],
            prompt[2],
            (" git status -s", foreground),
        ]))
        .child(line(&[(" M ", colors.normal[1]), ("src/main.rs", foreground)]))
        .child(line(&[("A  ", colors.normal[2]), ("src/theme.rs", foreground)]))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .child(line(&[prompt[0], prompt[1], prompt[2], (" ", foreground)]))
                .child(
                    div()
                        .w(px(text_size * 0.6))
                        .h(px(line_height - 3.0))
                        .bg(gpui_color(colors.cursor)),
                ),
        )
}

/// A tile-sized preview: the same session drawn as lines of color, legible at a glance.
fn theme_miniature(colors: &TerminalColors, appearance: &ChromeAppearance) -> gpui::Div {
    let edge = appearance.host_colors(spaceterm_ui::ControlHost::Card).border;
    let bar = |width: f32, color: Color| {
        div()
            .h(px(3.0))
            .w(relative(width))
            .rounded(px(1.5))
            .bg(gpui_color(color))
    };
    let row = |bars: Vec<gpui::Div>| div().flex().flex_row().gap(px(3.0)).children(bars);
    div()
        .flex()
        .flex_col()
        .justify_between()
        .aspect_ratio(PREVIEW_ASPECT)
        .overflow_hidden()
        .p(px(9.0))
        .border(px(HAIRLINE))
        .border_color(gpui_color(edge))
        .bg(gpui_color(colors.background))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(5.0))
                .child(row(vec![
                    bar(0.28, colors.normal[4]),
                    bar(0.14, colors.normal[5]),
                    bar(0.06, colors.normal[2]),
                    bar(0.12, colors.foreground),
                ]))
                .child(row(vec![bar(0.2, colors.bright[4]), bar(0.44, colors.foreground)]))
                .child(row(vec![bar(0.06, colors.normal[1]), bar(0.36, colors.foreground)]))
                .child(row(vec![bar(0.06, colors.normal[2]), bar(0.3, colors.foreground)])),
        )
        .child(
            div().flex().flex_row().gap(px(2.0)).children(
                colors.normal[1..7]
                    .iter()
                    .map(|color| div().flex_1().h(px(4.0)).rounded(px(1.0)).bg(gpui_color(*color))),
            ),
        )
}

/// The sixteen ANSI colors, normal above bright, as the palette a person scans to compare themes.
fn palette(colors: &TerminalColors, appearance: &ChromeAppearance) -> impl IntoElement {
    let edge = appearance.host_colors(spaceterm_ui::ControlHost::Card).border;
    let dot = move |color: Color| {
        div()
            .size(px(12.0))
            .rounded_full()
            .border(px(HAIRLINE))
            .border_color(gpui_color(edge))
            .bg(gpui_color(color))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(div().flex().flex_row().gap(px(6.0)).children(colors.normal.map(dot)))
        .child(div().flex().flex_row().gap(px(6.0)).children(colors.bright.map(dot)))
}
