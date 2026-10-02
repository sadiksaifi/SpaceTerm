//! The Themes section: a preview of the Terminal Theme in use and a list of installed themes.
//!
//! A row's Use button applies its theme to the appearance the list is showing. Under Auto the
//! preview shows the Light and Dark slots side by side, and selecting one points the list at it.
//! A row's removal button removes every theme installed with it: its extension's themes, or the
//! themes of the family imported from its file.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Entity, FocusHandle, Font, KeyDownEvent, SharedString, StyledText, Window,
    div, px, relative,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, HoverFade, Icon, IconName, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalId, SearchField, TextInput, TextInputEscapeBehavior,
    TextInputEvent, TextInputReturnBehavior, TextInputVariant, fuzzy_filter,
};

use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AvailableFonts, Color,
    SystemAppearance, TerminalColors, ThemeCatalog, ThemeId, ThemeSummary,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color, prepared_font};
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

use super::SettingsWindow;
use crate::ui::sidebar_window::form::action_button;

/// Width over height of every theme preview, large or small, so a tile reads as the same picture.
const PREVIEW_ASPECT: f32 = 1.6;
/// The width of the preview of the theme in use when one appearance is fixed.
const CURRENT_PREVIEW_WIDTH: f32 = 272.0;
/// The width of each slot's preview under Auto, small enough that the gallery stays in view.
const SLOT_PREVIEW_WIDTH: f32 = 216.0;
/// The width of the preview leading each row of the installed themes.
const ROW_PREVIEW_WIDTH: f32 = 64.0;
/// The width of a row's Use button, wider than its label so the target is easy to hit.
const USE_BUTTON_WIDTH: f32 = 60.0;
/// The space between a selected preview and the ring around it.
const RING_GAP: f32 = 3.0;
const RING_WIDTH: f32 = 2.0;

pub(super) const GALLERY_SEARCH_SELECTOR: &str = "settings-theme-gallery-search";
pub(super) const GET_MORE_SELECTOR: &str = "settings-get-more-themes";

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
    slot_focus: [FocusHandle; 2],
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
            .placeholder("Search themes")
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
            slot_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
        }
    }
}

/// What the preview of one slot shows: the theme chosen for it and the colors panes paint.
struct SlotPreview {
    slot: Appearance,
    /// The chosen theme, absent only when the catalog cannot be read.
    summary: Option<ThemeSummary>,
    /// The colors terminal panes use for this slot, including the person's overrides.
    colors: TerminalColors,
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
        let colors = resolved.map_or_else(
            || crate::appearance::builtin_terminal_base(slot),
            |resolved| resolved.terminal.colors.clone(),
        );
        SlotPreview {
            slot,
            summary,
            colors,
        }
    }

    /// The theme in use, large enough to judge: one preview when the appearance is fixed, and the
    /// Light and Dark slots side by side under Auto.
    pub(super) fn render_current_theme(
        &mut self,
        appearance: &ChromeAppearance,
        window: &mut Window,
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
            self.render_slot_card(preview, chosen == slot, font.clone(), appearance, window, cx)
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let slot = preview.slot;
        let label = match slot {
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        };
        let name = preview_name(&preview);
        let focus = &self.theme_gallery.slot_focus[usize::from(slot == Appearance::Dark)];
        let selector = format!("settings-theme-slot-{}", label.to_ascii_lowercase());
        let hover = HoverFade::new(SharedString::from(format!("{selector}-hover")), window, cx);
        let hover_level = hover.level(window, cx);
        let focused = focus.is_focused(window) && window.last_input_was_keyboard();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .track_focus(focus)
            .flex()
            .flex_col()
            .w(appearance.spacing(SLOT_PREVIEW_WIDTH))
            .gap(appearance.spacing(8.0))
            .child(selection_ring(
                terminal_preview(&preview.colors, font, PreviewSize::Small, appearance).w_full(),
                selected,
                &hover,
                hover_level,
                RadiusRole::Card,
                appearance,
                focused,
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
            .on_key_down(
                cx.listener(move |settings, event: &KeyDownEvent, window, cx| {
                    if keyboard_activation(event) {
                        settings.select_auto_slot(slot, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            )
            .into_any_element()
    }

    /// Every installed theme for the chosen slot, as rows offering to use or remove it, under a
    /// search field and the way to get more themes.
    pub(super) fn render_installed_themes(
        &self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slot = self.theme_slot(cx);
        let selected = self
            .editor
            .document()
            .preferences
            .terminal
            .themes
            .get(slot)
            .clone();
        let summaries = self
            .editor
            .theme_summaries()
            .unwrap_or_default()
            .into_iter()
            .filter(|summary| summary.appearance == slot)
            .collect::<Vec<_>>();
        let query = self.theme_gallery.query.clone();
        let matches = fuzzy_filter(&summaries, &query, |summary| {
            let target = spaceterm_ui::FuzzyTarget::new(&summary.name);
            match &summary.family {
                Some(family) => target.field(family),
                None => target,
            }
        });
        let rows = matches
            .iter()
            .map(|matched| {
                let summary = &summaries[matched.item_index()];
                self.render_theme_row(summary, slot, summary.id == selected, appearance, cx)
            })
            .collect::<Vec<_>>();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let separator = gpui_color(appearance.separator(spaceterm_ui::ControlHost::Card));
        let empty = rows.is_empty();
        let rows = rows.into_iter().enumerate().flat_map(|(index, row)| {
            let divider =
                (index > 0).then(|| div().h(px(HAIRLINE)).bg(separator).into_any_element());
            divider.into_iter().chain([row])
        });
        let owner = cx.weak_entity();
        div()
            .debug_selector(|| "settings-installed-themes".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(10.0))
            .py(appearance.spacing(6.0))
            // The header matches Get More Themes, which offers importing where this offers more.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(10.0))
                    .child(
                        div().flex_1().min_w_0().child(
                            SearchField::new(
                                "settings-theme-gallery-search-frame",
                                self.theme_gallery.search.clone(),
                            )
                            .debug_selectors(
                                "settings-theme-gallery-search-frame",
                                "settings-theme-gallery-search-clear",
                            ),
                        ),
                    )
                    .child(div().flex_none().child(action_button(
                        GET_MORE_SELECTOR,
                        "Get More Themes…",
                        self.editor.editable(),
                        move |window, cx| {
                            let _ = owner.update(cx, |settings, cx| {
                                settings.open_theme_store(window, cx);
                            });
                        },
                    ))),
            )
            .when(empty, |list| {
                list.child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.text_secondary))
                        .whitespace_normal()
                        .child(SharedString::from(format!("No themes match “{query}”."))),
                )
            })
            .child(div().flex().flex_col().children(rows))
            .into_any_element()
    }

    /// One installed theme: its preview, its name, and where it came from, then its actions. The
    /// theme in use says so where the others offer Use, and a theme SpaceTerm did not ship offers
    /// removal at the row's end.
    fn render_theme_row(
        &self,
        summary: &ThemeSummary,
        slot: Appearance,
        selected: bool,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let editable = self.editor.editable();
        let selector = format!("settings-theme-row-{}", summary.id.as_str());
        let action = if selected {
            div()
                .debug_selector({
                    let selector = format!("{selector}-in-use");
                    move || selector
                })
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(4.0))
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.text_secondary))
                .child(Icon::new(
                    IconName::Check,
                    appearance.icons.metrics(IconRole::Caption).glyph_size,
                    gpui_color(colors.text_secondary),
                ))
                .child("In Use")
                .into_any_element()
        } else {
            let owner = cx.weak_entity();
            let id = summary.id.clone();
            let use_selector = format!("{selector}-use");
            let button = spaceterm_ui::Button::new(SharedString::from(use_selector.clone()), "Use")
                .variant(spaceterm_ui::ButtonVariant::Outline)
                .size(spaceterm_ui::ButtonSize::Small)
                .full_width(true)
                .disabled(!editable)
                .tab_stop(true)
                .debug_selector(use_selector)
                .on_activate(move |activation, window, cx| {
                    // In Use replaces this button, so keyboard focus moves on before it goes.
                    if activation.source() != spaceterm_ui::ButtonActivationSource::Pointer {
                        window.focus_next(cx);
                    }
                    let _ = owner.update(cx, |settings, cx| {
                        settings.set_theme(slot, id.clone(), cx);
                    });
                });
            div()
                .flex_none()
                .w(appearance.spacing(USE_BUTTON_WIDTH))
                .child(button)
                .into_any_element()
        };
        let remove = (!summary.builtin).then(|| {
            let owner = cx.weak_entity();
            let target = summary.clone();
            let remove_selector = format!("{selector}-remove");
            let glyph = appearance.icons.metrics(IconRole::Control).glyph_size;
            spaceterm_ui::IconButton::new(
                SharedString::from(remove_selector.clone()),
                SharedString::from(format!("Remove {}", summary.name)),
                move |foreground| Icon::new(IconName::Trash2, glyph, foreground).into_any_element(),
            )
            .variant(spaceterm_ui::ButtonVariant::Ghost)
            .size(spaceterm_ui::ButtonSize::Small)
            .disabled(!editable)
            .tab_stop(true)
            .debug_selector(remove_selector.clone())
            .tooltip(spaceterm_ui::Tooltip::new(
                SharedString::from(format!("{remove_selector}-tooltip")),
                "Remove",
            ))
            .on_activate(move |_, window, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.confirm_removal(&target, window, cx);
                });
            })
        });
        div()
            .debug_selector({
                let selector = selector.clone();
                move || selector
            })
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(12.0))
            .py(appearance.spacing(8.0))
            .child(
                theme_miniature(&summary.colors, appearance)
                    .flex_none()
                    .w(appearance.spacing(ROW_PREVIEW_WIDTH))
                    .rounded(RadiusRole::Control.pixels()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .gap(appearance.spacing(1.0))
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                            .text_color(gpui_color(colors.text))
                            .child(SharedString::from(summary.name.clone())),
                    )
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(colors.text_secondary))
                            .child(theme_origin(summary)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(2.0))
                    .child(action)
                    .children(remove),
            )
            .into_any_element()
    }

    /// Asks before removing a theme and every theme installed with it, then removes them together.
    /// A slot that used a removed theme returns to its built-in theme.
    fn confirm_removal(
        &mut self,
        theme: &ThemeSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let removed = self
            .editor
            .theme_summaries()
            .unwrap_or_default()
            .into_iter()
            .filter(|summary| installed_together(summary, theme))
            .collect::<Vec<_>>();
        let ids = removed
            .iter()
            .map(|summary| summary.id.clone())
            .collect::<Vec<_>>();
        let slots = self.editor.document().preferences.terminal.themes.clone();
        let in_use = [Appearance::Light, Appearance::Dark]
            .into_iter()
            .filter(|slot| ids.contains(slots.get(*slot)))
            .collect::<Vec<_>>();
        let one = ids.len() == 1;
        let family = removed
            .iter()
            .all(|summary| summary.family == theme.family)
            .then_some(theme.family.as_ref())
            .flatten();
        let (title, message) = match (one, family) {
            (true, _) => ("Remove Theme", format!("Remove “{}”?", theme.name)),
            (false, Some(family)) => (
                "Remove Themes",
                format!("Remove all {} {family} themes?", ids.len()),
            ),
            (false, None) => (
                "Remove Themes",
                format!("Remove all {} themes from this extension?", ids.len()),
            ),
        };
        // The list shows one appearance, so name the themes of the other that go with it.
        let hidden = removed
            .iter()
            .filter(|summary| summary.appearance != theme.appearance)
            .count();
        let hidden = match (hidden, theme.appearance) {
            (0, _) => None,
            (1, Appearance::Light) => Some(String::from("This includes 1 dark theme.")),
            (1, Appearance::Dark) => Some(String::from("This includes 1 light theme.")),
            (count, Appearance::Light) => Some(format!("This includes {count} dark themes.")),
            (count, Appearance::Dark) => Some(format!("This includes {count} light themes.")),
        };
        let consequence = match (in_use.is_empty(), &theme.package) {
            (false, _) if one => "Terminal panes using it will switch to the built-in theme.",
            (false, _) => "Terminal panes using one of them will switch to the built-in theme.",
            (true, Some(_)) if one => "You can get it again from Get More Themes.",
            (true, Some(_)) => "You can get them again from Get More Themes.",
            (true, None) if one => "You can import it again from its file.",
            (true, None) => "You can import them again from their file.",
        };
        let detail = match hidden {
            Some(hidden) => format!("{hidden} {consequence}"),
            None => consequence.to_owned(),
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
                settings.remove_installed_themes(&ids, cx);
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm theme removal confirmation");
        }
    }

    /// Removes themes together; a slot that used one of them returns to its built-in theme.
    /// Returns whether the themes were removed.
    pub(super) fn remove_installed_themes(
        &mut self,
        ids: &[ThemeId],
        cx: &mut Context<Self>,
    ) -> bool {
        let removed = self.editor.remove_themes(ids, cx).is_ok();
        cx.notify();
        removed
    }
}

/// Whether two installed themes arrived together, so removing one removes both: themes from the
/// same extension, or themes of the same family imported from a file.
fn installed_together(theme: &ThemeSummary, other: &ThemeSummary) -> bool {
    if theme.id == other.id {
        return true;
    }
    if theme.builtin || other.builtin {
        return false;
    }
    match (&theme.package, &other.package) {
        (Some(theme), Some(other)) => theme.id == other.id,
        (None, None) => theme.family.is_some() && theme.family == other.family,
        _ => false,
    }
}

fn keyboard_activation(event: &KeyDownEvent) -> bool {
    !event.is_held
        && !event.keystroke.modifiers.modified()
        && matches!(event.keystroke.key.as_str(), "enter" | "space")
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
    let name = preview.summary.as_ref().map(|summary| summary.name.clone());
    SharedString::from(name.unwrap_or_default())
}

/// Where the theme came from, in the words a person would use.
fn preview_origin(preview: &SlotPreview) -> SharedString {
    preview.summary.as_ref().map(theme_origin).unwrap_or_default()
}

/// Where an installed theme came from, in the words a person would use.
fn theme_origin(summary: &ThemeSummary) -> SharedString {
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
    hover: &HoverFade,
    hover_level: f32,
    radius: RadiusRole,
    appearance: &ChromeAppearance,
    focused: bool,
) -> impl IntoElement {
    let accent = appearance.colors.border_focused;
    let hover_ring = appearance.host_colors(spaceterm_ui::ControlHost::Card).border;
    let outer_radius = radius.pixels() + px(RING_GAP + RING_WIDTH);
    // Keyboard focus surrounds the selection ring so the two states stay distinct: the focus ring
    // treats the selection ring's outer edge as the control's edge.
    let focus_ring = focused.then(|| {
        spaceterm_ui::focus_ring(
            "focus-ring",
            gpui_color(appearance.colors.focus_ring),
            outer_radius,
            px(0.0),
        )
    });
    let tile = div()
        .relative()
        .w_full()
        .p(px(RING_GAP))
        .rounded(outer_radius)
        .border(px(RING_WIDTH))
        .border_color(gpui_color(if selected {
            accent
        } else {
            hover_ring.with_alpha(0).mix(hover_ring, f64::from(hover_level))
        }))
        .child(content.rounded(radius.pixels()))
        .when(!selected, |ring| ring.child(hover.tracker()));
    spaceterm_ui::Ringed::new(tile, focus_ring)
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

/// A row-sized preview: the same session drawn as lines of color, legible at a glance.
fn theme_miniature(colors: &TerminalColors, appearance: &ChromeAppearance) -> gpui::Div {
    let edge = appearance.host_colors(spaceterm_ui::ControlHost::Card).border;
    let bar = |width: f32, color: Color| {
        div()
            .h(px(2.0))
            .w(relative(width))
            .rounded(px(1.0))
            .bg(gpui_color(color))
    };
    let row = |bars: Vec<gpui::Div>| div().flex().flex_row().gap(px(2.0)).children(bars);
    div()
        .flex()
        .flex_col()
        .justify_between()
        .aspect_ratio(PREVIEW_ASPECT)
        .overflow_hidden()
        .p(px(6.0))
        .border(px(HAIRLINE))
        .border_color(gpui_color(edge))
        .bg(gpui_color(colors.background))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
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
                    .map(|color| div().flex_1().h(px(3.0)).rounded(px(1.0)).bg(gpui_color(*color))),
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
