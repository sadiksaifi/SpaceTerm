use gpui::prelude::*;
use gpui::{AnyElement, Font, Pixels, SharedString, TextRun, Window, canvas, div, px};
use spaceterm_ui::{Icon, IconName};

use super::workspace_manager::gpui_color;
use crate::theme::ACTIVE_THEME;

const NAME_SIZE: f32 = 13.0;
const DETAIL_SIZE: f32 = 12.0;
const GAP: f32 = 8.0;
const PIN_WIDTH: f32 = 16.0;

pub(super) fn title(
    name: SharedString,
    name_element: AnyElement,
    machine: Option<SharedString>,
    id: u64,
) -> AnyElement {
    canvas(
        move |bounds, window, cx| {
            let name_width = measure(&name, NAME_SIZE, window);
            let machine = machine.and_then(|machine| {
                let full_width = measure(&machine, DETAIL_SIZE, window);
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
                    row.gap(px(GAP)).child(
                        div()
                            .id(("workspace-machine", id))
                            .debug_selector(move || format!("workspace-machine-{id}"))
                            .w(width)
                            .flex_shrink_0()
                            .truncate()
                            .text_size(px(DETAIL_SIZE))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
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
    .h(px(22.0))
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
    warning: bool,
    selector: Option<String>,
    id: u64,
) -> AnyElement {
    canvas(
        move |bounds, window, cx| {
            let counts_width = measure(&counts, DETAIL_SIZE, window).ceil();
            let pin_width = if pinned { px(PIN_WIDTH) } else { px(0.0) };
            let available = (bounds.size.width - counts_width - px(GAP) - pin_width).max(px(0.0));
            let fitted = if warning {
                text
            } else {
                fit_trailing_path(&text, available, |value| {
                    measure(value, DETAIL_SIZE, window)
                })
                .into()
            };
            let path = div()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .when(pinned, |path| {
                    path.child(
                        div()
                            .w(px(PIN_WIDTH))
                            .flex_shrink_0()
                            .id(("workspace-row-pin", id))
                            .debug_selector(move || format!("workspace-row-pin-{id}"))
                            .child(Icon::new(
                                IconName::Pin,
                                px(12.0),
                                gpui_color(ACTIVE_THEME.icon),
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
                        .text_color(gpui_color(if warning {
                            ACTIVE_THEME.warning
                        } else {
                            ACTIVE_THEME.text_muted
                        }))
                        .child(fitted),
                );
            let mut content = div()
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .gap(px(GAP))
                .text_size(px(DETAIL_SIZE))
                .child(path)
                .child(
                    div()
                        .w(counts_width)
                        .flex_shrink_0()
                        .whitespace_nowrap()
                        .id(("workspace-counts", id))
                        .debug_selector(move || format!("workspace-counts-{id}"))
                        .text_color(gpui_color(ACTIVE_THEME.text_muted))
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
    .h(px(18.0))
    .into_any_element()
}

fn measure(text: &str, size: f32, window: &Window) -> Pixels {
    let style = window.text_style();
    let run = TextRun {
        len: text.len(),
        font: Font {
            family: style.font_family,
            features: style.font_features,
            fallbacks: style.font_fallbacks,
            weight: style.font_weight,
            style: style.font_style,
        },
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.to_owned().into(), px(size), &[run], None)
        .width
}

fn fit_trailing_path(text: &str, available: Pixels, measure: impl Fn(&str) -> Pixels) -> String {
    if measure(text) <= available {
        return text.to_owned();
    }
    if measure("…") > available {
        return String::new();
    }
    let boundaries: Vec<_> = text
        .char_indices()
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
