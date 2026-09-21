use gpui::prelude::*;
use gpui::{AnyElement, Pixels, SharedString, canvas, div, px};
use spaceterm_ui::{Icon, IconName};
use unicode_segmentation::UnicodeSegmentation;

use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt, TextRole};
use crate::ui::workspace_status::WorkspaceStatusPaint;

use super::gpui_color;

const GAP: f32 = 8.0;
const PIN_WIDTH: f32 = 16.0;

pub(super) fn title(
    name: SharedString,
    name_element: AnyElement,
    machine: Option<SharedString>,
    id: u64,
    appearance: ChromeAppearance,
) -> AnyElement {
    let height = appearance
        .typography
        .style(TextRole::Navigation)
        .line_height
        + appearance.spacing(super::SIDEBAR_ROW_TITLE_LINE_PADDING);
    canvas(
        move |bounds, window, cx| {
            let name_width = appearance
                .typography
                .measure(TextRole::Navigation, &name, window);
            let machine = machine.and_then(|machine| {
                let full_width =
                    appearance
                        .typography
                        .measure(TextRole::Secondary, &machine, window);
                machine_width(bounds.size.width, name_width, full_width)
                    .map(|width| (machine, width))
            });
            let mut content = div()
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .child(div().min_w_0().flex_1().child(name_element))
                .when_some(machine, |row, (machine, width)| {
                    row.gap(appearance.spacing(GAP)).child(
                        div()
                            .id(("workspace-machine", id))
                            .debug_selector(move || format!("workspace-machine-{id}"))
                            .w(width)
                            .flex_shrink_0()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(appearance.colors.row_secondary))
                            .group_hover(format!("workspace-row-state-{id}"), |style| {
                                style.text_color(gpui_color(appearance.colors.row_hover_secondary))
                            })
                            .child(machine),
                    )
                })
                .into_any_element();
            content.layout_as_root(bounds.size.map(gpui::AvailableSpace::Definite), window, cx);
            content.prepaint_at(bounds.origin, window, cx);
            content
        },
        |_, mut content, window, cx| content.paint(window, cx),
    )
    .w_full()
    .h(height)
    .into_any_element()
}

fn machine_width(available: Pixels, name: Pixels, machine: Pixels) -> Option<Pixels> {
    let remaining = (available - name - px(GAP)).min(machine);
    (remaining >= machine.min(px(48.0)) && remaining > px(0.0)).then_some(remaining)
}

pub(super) fn detail(
    text: SharedString,
    counts: SharedString,
    pinned: bool,
    status_paint: Option<WorkspaceStatusPaint>,
    selector: Option<String>,
    id: u64,
    appearance: ChromeAppearance,
) -> AnyElement {
    let height = appearance.typography.style(TextRole::Secondary).line_height
        + appearance.spacing(super::SIDEBAR_ROW_DETAIL_LINE_PADDING);
    canvas(
        move |bounds, window, cx| {
            let counts_width = appearance
                .typography
                .measure(TextRole::Secondary, &counts, window)
                .ceil();
            let pin_width = if pinned {
                appearance.spacing(PIN_WIDTH)
            } else {
                px(0.0)
            };
            let available =
                (bounds.size.width - counts_width - appearance.spacing(GAP) - pin_width)
                    .max(px(0.0));
            let fitted = if status_paint.is_some() {
                text
            } else {
                fit_trailing_path(&text, available, |value| {
                    appearance
                        .typography
                        .measure(TextRole::Secondary, value, window)
                })
                .into()
            };
            let status_normal = status_paint.map(|paint| paint.normal);
            let status_hovered = status_paint.map(|paint| paint.hovered);
            let path = div()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .when(pinned, |path| {
                    path.child(
                        div()
                            .w(appearance.spacing(PIN_WIDTH))
                            .flex_shrink_0()
                            .id(("workspace-row-pin", id))
                            .debug_selector(move || format!("workspace-row-pin-{id}"))
                            .text_color(gpui_color(appearance.colors.row_secondary))
                            .group_hover(format!("workspace-row-state-{id}"), |style| {
                                style.text_color(gpui_color(appearance.colors.row_hover_secondary))
                            })
                            .child(Icon::inherited(
                                IconName::Pin,
                                appearance.icons.metrics(IconRole::Caption).glyph_size,
                            )),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .id(("workspace-row-detail", id))
                        .debug_selector(move || {
                            selector.unwrap_or_else(|| format!("workspace-row-path-{id}"))
                        })
                        .text_color(gpui_color(
                            status_normal.unwrap_or(appearance.colors.row_secondary),
                        ))
                        .group_hover(format!("workspace-row-state-{id}"), |style| {
                            style.text_color(gpui_color(
                                status_hovered.unwrap_or(appearance.colors.row_hover_secondary),
                            ))
                        })
                        .child(fitted),
                );
            let mut content = div()
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .gap(appearance.spacing(GAP))
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .child(path)
                .child(
                    div()
                        .w(counts_width)
                        .flex_shrink_0()
                        .whitespace_nowrap()
                        .id(("workspace-counts", id))
                        .debug_selector(move || format!("workspace-counts-{id}"))
                        .text_color(gpui_color(appearance.colors.row_secondary))
                        .group_hover(format!("workspace-row-state-{id}"), |style| {
                            style.text_color(gpui_color(appearance.colors.row_hover_secondary))
                        })
                        .child(counts),
                )
                .into_any_element();
            content.layout_as_root(bounds.size.map(gpui::AvailableSpace::Definite), window, cx);
            content.prepaint_at(bounds.origin, window, cx);
            content
        },
        |_, mut content, window, cx| content.paint(window, cx),
    )
    .w_full()
    .h(height)
    .into_any_element()
}

fn fit_trailing_path(text: &str, available: Pixels, measure: impl Fn(&str) -> Pixels) -> String {
    if measure(text) <= available {
        return text.to_owned();
    }
    if measure("…") > available {
        return String::new();
    }
    let boundaries: Vec<_> = text
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect();
    let mut low = 0;
    let mut high = boundaries.len() - 1;
    while low < high {
        let mid = low + (high - low) / 2;
        if measure(&format!("…{}", &text[boundaries[mid]..])) <= available {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    format!("…{}", &text[boundaries[low]..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_should_shrink_then_hide_before_name_shrinks() {
        let widths = [300.0, 220.0, 180.0, 100.0]
            .map(|width| machine_width(px(width), px(150.0), px(120.0)));
        assert_eq!(widths, [Some(px(120.0)), Some(px(62.0)), None, None]);
    }

    #[test]
    fn short_machine_should_fit_without_an_artificial_minimum() {
        assert_eq!(
            machine_width(px(178.0), px(150.0), px(20.0)),
            Some(px(20.0))
        );
        assert_eq!(machine_width(px(177.0), px(150.0), px(20.0)), None);
    }

    #[test]
    fn path_truncation_should_keep_combining_accents_with_their_base() {
        let measure = |text: &str| px(text.chars().count() as f32);
        let path = "/projects/e\u{301}x";
        assert_eq!(
            [3.0, 4.0].map(|width| fit_trailing_path(path, px(width), measure)),
            ["…x", "…e\u{301}x"]
        );
    }

    #[test]
    fn path_truncation_should_keep_joined_emoji_whole() {
        let measure = |text: &str| px(text.chars().count() as f32);
        let path = "/projects/👩‍💻x";
        assert_eq!(
            [4.0, 5.0].map(|width| fit_trailing_path(path, px(width), measure)),
            ["…x", "…👩‍💻x"]
        );
    }

    #[test]
    fn long_paths_should_keep_their_unicode_suffix_and_fit_the_remaining_width() {
        let measure = |text: &str| px(text.chars().count() as f32);
        assert_eq!(
            fit_trailing_path("~/projects/日本語/api", px(8.0), measure),
            "…日本語/api"
        );
        assert_eq!(fit_trailing_path("~/api", px(8.0), measure), "~/api");
        assert_eq!(fit_trailing_path("/api", px(0.0), measure), "");
    }
}
