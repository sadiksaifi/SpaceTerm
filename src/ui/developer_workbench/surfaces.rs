//! The Floating Surfaces section: menus, pickers, the command palette, and tooltips over hostile
//! synthetic backdrop content, so a capture shows whether they stay legible.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Entity, Image, ImageFormat, ObjectFit, Window, div, img, px, rgba};
use spaceterm_ui::{
    Button, ButtonSize, ComboBox, ComboBoxItem, CommandPalette, ContextMenu, FloatingRole, Menu,
    MenuEntry, MenuSize, Picker, PickerOption, SegmentedControl, SegmentedOption, Tooltip,
};

use super::DeveloperWorkbench;
use super::modals::DialogBody;
use crate::ui::appearance::gpui_color;
use crate::ui::appearance::settings::SettingsAppearance;
use crate::ui::sidebar_window::form::{FormGroup, FormRow, FormRowLayout};

const BACKDROP_PROBE_TEXT: &str =
    "Persistent floating probe. Compare the detail behind it with Blur off and on.";
const SYNTHETIC_TEXT: &str =
    "SYNTHETIC 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ · fine text behind a floating surface · ";

/// What stands behind the floating probes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Backdrop {
    Patterned,
    Photographic,
    /// Nothing: the window's own material.
    Window,
}

pub(super) struct FloatingSurfaces {
    backdrop: Backdrop,
    picker_value: u8,
    combo_value: Option<u8>,
    interaction_probe: Entity<DialogBody>,
    photograph: Arc<Image>,
}

impl FloatingSurfaces {
    pub(super) fn new(window: &mut Window, cx: &mut Context<DeveloperWorkbench>) -> Self {
        Self {
            backdrop: Backdrop::Patterned,
            picker_value: 1,
            combo_value: Some(1),
            interaction_probe: cx.new(|cx| DialogBody::new(window, cx)),
            photograph: Arc::new(Image::from_bytes(
                ImageFormat::Jpeg,
                include_bytes!("../../../assets/developer-workbench/blue-marble-2012.jpg").to_vec(),
            )),
        }
    }

    pub(super) fn render(
        &self,
        palette: &Entity<CommandPalette<u8>>,
        surface: &SettingsAppearance,
        window: &Window,
        cx: &mut Context<DeveloperWorkbench>,
    ) -> Vec<AnyElement> {
        let appearance = &surface.chrome;
        let owner = cx.weak_entity();
        let backdrop = SegmentedControl::new(
            "workbench-backdrop",
            "Backdrop",
            &self.backdrop,
            vec![
                SegmentedOption::new(Backdrop::Patterned, "Pattern")
                    .debug_selector("workbench-backdrop-patterned"),
                SegmentedOption::new(Backdrop::Photographic, "Photograph")
                    .debug_selector("workbench-backdrop-photographic"),
                SegmentedOption::new(Backdrop::Window, "Window")
                    .debug_selector("workbench-backdrop-window"),
            ],
        )
        .expect("three backdrops are within the bounded option set")
        .on_change(move |change, _, cx| {
            let backdrop = *change.requested();
            let _ = owner.update(cx, |workbench, cx| {
                workbench.surfaces.backdrop = backdrop;
                cx.notify();
            });
        });
        let owner = cx.weak_entity();
        let picker = Picker::new(
            "workbench-picker",
            "Picker fixture",
            self.picker_value,
            vec![
                PickerOption::new(1, "First option"),
                PickerOption::new(2, "Second option"),
                PickerOption::new(3, "Unavailable option").disabled(true),
            ],
        )
        .expect("the picker's value is one of its options")
        .on_change(move |change, _, cx| {
            let value = *change.value();
            let _ = owner.update(cx, |workbench, cx| {
                workbench.surfaces.picker_value = value;
                cx.notify();
            });
        });
        let owner = cx.weak_entity();
        let combo = ComboBox::new(
            "workbench-surface-combo",
            "Combo box fixture",
            self.combo_value,
            "Choose a result",
            vec![
                ComboBoxItem::new(1, "First result").description("Selected description"),
                ComboBoxItem::new(2, "Second result").description("Another description"),
                ComboBoxItem::new(3, "Unavailable result").disabled(true),
            ],
        )
        .on_accept(move |accepted, _, cx| {
            let value = *accepted.item_id();
            let _ = owner.update(cx, |workbench, cx| {
                workbench.surfaces.combo_value = Some(value);
                cx.notify();
            });
        });
        let palette = palette.clone();
        let menus = div()
            .flex()
            .flex_wrap()
            .gap(appearance.spacing(8.0))
            .children(
                [
                    ("workbench-menu-small", "Small", MenuSize::Small),
                    ("workbench-menu-regular", "Regular", MenuSize::Regular),
                    ("workbench-menu-wide", "Wide", MenuSize::Wide),
                ]
                .into_iter()
                .map(|(id, label, size)| {
                    Menu::new(id, label, menu_entries())
                        .size(size)
                        .debug_selector(id)
                        .on_activate(|_, _, _| {})
                }),
            )
            .child(
                ContextMenu::new(
                    "workbench-context-menu",
                    "Context menu fixture",
                    Button::new("workbench-context-target", "Secondary-Click Here")
                        .size(ButtonSize::Small),
                    menu_entries(),
                )
                .on_activate(|_, _, _| {}),
            );
        let rows = vec![
            FormRow::new("workbench-row-surfaces-backdrop", "Backdrop", backdrop)
                .description("Compare material over detail with Blur off and on.")
                .render(appearance, window, cx)
                .into_any_element(),
            FormRow::new("workbench-row-surfaces-menus", "Menus", menus)
                .render(appearance, window, cx)
                .into_any_element(),
            FormRow::new("workbench-row-surfaces-picker", "Picker", picker)
                .render(appearance, window, cx)
                .into_any_element(),
            FormRow::new("workbench-row-surfaces-combo", "Combo box", combo)
                .render(appearance, window, cx)
                .into_any_element(),
            FormRow::new(
                "workbench-row-surfaces-palette",
                "Command palette",
                Button::new("workbench-surfaces-open-palette", "Open Palette")
                    .size(ButtonSize::Small)
                    .on_activate(move |_, window, cx| {
                        palette.update(cx, |palette, cx| palette.open(window, cx));
                    }),
            )
            .render(appearance, window, cx)
            .into_any_element(),
            FormRow::new(
                "workbench-row-surfaces-tooltip",
                "Tooltip",
                Button::new("workbench-tooltip-target", "Hover Here")
                    .size(ButtonSize::Small)
                    .on_activate(|_, _, _| {})
                    .tooltip(
                        Tooltip::new("workbench-tooltip", "Tooltip over detailed content")
                            .detail("The foreground and edge must stay legible.")
                            .debug_selector("workbench-floating-tooltip"),
                    ),
            )
            .render(appearance, window, cx)
            .into_any_element(),
        ];
        vec![
            FormGroup::new(
                "workbench-group-surfaces-controls".to_owned(),
                "Controls",
                rows,
            )
            .render(surface)
            .into_any_element(),
            FormGroup::new(
                "workbench-group-surfaces-stage".to_owned(),
                "Stage",
                vec![
                    FormRow::new(
                        "workbench-row-surfaces-stage",
                        "Stage",
                        self.render_stage(surface),
                    )
                    .layout(FormRowLayout::Full)
                    .render(appearance, window, cx)
                    .into_any_element(),
                ],
            )
            .render(surface)
            .into_any_element(),
        ]
    }

    /// The backdrop with a persistent readout probe and an interactive popover probe over it.
    fn render_stage(&self, surface: &SettingsAppearance) -> AnyElement {
        let appearance = &surface.chrome;
        let pattern = div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .children((0..24).map(|row| {
                let (background, foreground) = if row % 2 == 0 {
                    (rgba(0xf5f5f5ff), rgba(0x111111ff))
                } else {
                    (rgba(0x111111ff), rgba(0xf5f5f5ff))
                };
                div()
                    .h(px(18.0))
                    .flex_none()
                    .bg(background)
                    .text_color(foreground)
                    .text_size(px(12.0))
                    .overflow_hidden()
                    .child(SYNTHETIC_TEXT.repeat(4))
            }));
        let backdrop = match self.backdrop {
            Backdrop::Patterned => Some(
                div()
                    .debug_selector(|| "workbench-backing-patterned".to_owned())
                    .absolute()
                    .inset_0()
                    .child(pattern)
                    .into_any_element(),
            ),
            Backdrop::Photographic => Some(
                div()
                    .debug_selector(|| "workbench-backing-photographic".to_owned())
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .child(
                        img(self.photograph.clone())
                            .size_full()
                            .object_fit(ObjectFit::Cover),
                    )
                    .into_any_element(),
            ),
            Backdrop::Window => None,
        };
        let readout = appearance
            .floating_surfaces()
            .shell(FloatingRole::Notice)
            .mount(
                div()
                    .absolute()
                    .top(px(96.0))
                    .left(px(24.0))
                    .w(px(300.0))
                    .p(appearance.spacing(14.0))
                    .text_color(gpui_color(appearance.floating_colors.text))
                    .whitespace_normal()
                    .debug_selector(|| "workbench-backdrop-probe".to_owned())
                    .child(BACKDROP_PROBE_TEXT),
            );
        let popover = appearance
            .floating_surfaces()
            .shell(FloatingRole::Popover)
            .mount(
                div()
                    .absolute()
                    .top(px(24.0))
                    .left(gpui::relative(0.45))
                    .right(px(24.0))
                    .p(appearance.spacing(14.0))
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(10.0))
                    .text_color(gpui_color(appearance.floating_colors.text))
                    .child("Interactive material over backdrop content")
                    .child(self.interaction_probe.clone()),
            );
        div()
            .relative()
            .w_full()
            .h(px(432.0))
            .overflow_hidden()
            .rounded(crate::ui::chrome_geometry::RadiusRole::Control.pixels())
            .children(backdrop)
            .child(readout)
            .child(popover)
            .into_any_element()
    }
}

fn menu_entries() -> Vec<MenuEntry<u8>> {
    vec![
        MenuEntry::action("Open item", 1),
        MenuEntry::checkbox("Checked item", true, 2),
        MenuEntry::separator(),
        MenuEntry::action("Unavailable item", 3).disabled(true),
        MenuEntry::submenu("Nested actions", vec![MenuEntry::action("Nested item", 4)]),
    ]
}
