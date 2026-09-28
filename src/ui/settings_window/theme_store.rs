//! The Themes section's Zed extension browser: search the registry, then install or update.
//!
//! SpaceTerm contacts the registry only after the person asks to browse it, and only once per
//! window: the listing is small, searched locally, and discarded with the window. Installing
//! downloads one extension, translates its themes, and installs them without selecting any.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Entity, SharedString, Window, div};
use spaceterm_ui::{FuzzyTarget, SearchField, TextInput, TextInputEvent, fuzzy_filter};

use crate::appearance::{ThemePackage, ZedExtension};
use crate::settings::ThemeImport;
use crate::theme_registry::{RegistryError, RegistryExtension, ZedThemeRegistry};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

use super::SettingsWindow;
use super::controls::action_button;
use crate::ui::appearance::gpui_color;
use super::themes::{import_failure_message, new_list_search};

/// Results beyond this many ask for a narrower search instead of growing the page.
const MAX_RESULTS: usize = 40;

pub(super) const SEARCH_SELECTOR: &str = "settings-zed-extensions-search";
pub(super) const BROWSE_SELECTOR: &str = "settings-zed-extensions-browse";

/// What the window knows about the registry listing.
#[derive(Clone, Debug)]
pub(super) enum Listing {
    /// The person has not asked to browse, so SpaceTerm has not contacted the registry.
    NotRequested,
    Loading,
    Loaded(Arc<[RegistryExtension]>),
    Failed(RegistryError),
}

/// What one extension's action offers, derived from what is installed and in flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExtensionAction {
    Install,
    Installing,
    Update,
    Installed,
}

/// Owns the injected registry, the listing, installs in flight, and the last outcome.
pub(super) struct ZedExtensionsBrowser {
    registry: Option<ZedThemeRegistry>,
    listing: Listing,
    installing: BTreeSet<String>,
    status: Option<SharedString>,
    search: Entity<TextInput>,
    query: SharedString,
}

impl ZedExtensionsBrowser {
    pub(super) fn new(
        registry: Option<ZedThemeRegistry>,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let search = new_list_search(SEARCH_SELECTOR, "Search Zed themes", window, cx);
        cx.subscribe(&search, |settings, search, event: &TextInputEvent, cx| {
            if matches!(event, TextInputEvent::ValueChanged(_)) {
                settings.zed_extensions.query = SharedString::from(search.read(cx).value().to_owned());
                cx.notify();
            }
        })
        .detach();
        Self {
            registry,
            listing: Listing::NotRequested,
            installing: BTreeSet::new(),
            status: None,
            search,
            query: SharedString::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn listing(&self) -> &Listing {
        &self.listing
    }

    #[cfg(test)]
    pub(super) fn status(&self) -> Option<&SharedString> {
        self.status.as_ref()
    }

    /// The extensions matching the query, best match first, or the most downloaded first when
    /// there is no query.
    pub(super) fn matches(&self) -> (Vec<RegistryExtension>, usize) {
        let Listing::Loaded(extensions) = &self.listing else {
            return (Vec::new(), 0);
        };
        let matched = fuzzy_filter(extensions, &self.query, |extension| {
            FuzzyTarget::new(&extension.name)
                .field(&extension.id)
                .field(extension.authors.join(" "))
        });
        let total = matched.len();
        let shown = matched
            .into_iter()
            .take(MAX_RESULTS)
            .map(|matched| extensions[matched.item_index()].clone())
            .collect();
        (shown, total)
    }
}

impl SettingsWindow {
    /// Fetches the registry listing once, at the person's request.
    pub(super) fn browse_zed_extensions(&mut self, cx: &mut Context<Self>) {
        let Some(registry) = self.zed_extensions.registry.clone() else {
            return;
        };
        if matches!(
            self.zed_extensions.listing,
            Listing::Loading | Listing::Loaded(_)
        ) {
            return;
        }
        self.zed_extensions.listing = Listing::Loading;
        cx.notify();
        cx.spawn(async move |owner, cx| {
            let listing = cx
                .background_executor()
                .spawn(async move { registry.list() })
                .await;
            let _ = owner.update(cx, |settings, cx| {
                settings.zed_extensions.listing = match listing {
                    Ok(extensions) => Listing::Loaded(extensions.into()),
                    Err(error) => Listing::Failed(error),
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn install_zed_extension(
        &mut self,
        extension: RegistryExtension,
        cx: &mut Context<Self>,
    ) {
        let Some(registry) = self.zed_extensions.registry.clone() else {
            return;
        };
        if !self.editor.editable() || !self.zed_extensions.installing.insert(extension.id.clone())
        {
            return;
        }
        self.zed_extensions.status = None;
        cx.notify();
        cx.spawn(async move |owner, cx| {
            let download = cx
                .background_executor()
                .spawn({
                    let extension = extension.clone();
                    async move { registry.download(&extension) }
                })
                .await;
            let _ = owner.update(cx, |settings, cx| {
                settings.finish_zed_install(&extension, download, cx);
            });
        })
        .detach();
    }

    pub(super) fn finish_zed_install(
        &mut self,
        extension: &RegistryExtension,
        download: Result<ZedExtension, RegistryError>,
        cx: &mut Context<Self>,
    ) {
        self.zed_extensions.installing.remove(&extension.id);
        let message = match download {
            Err(error) => SharedString::from(registry_failure_message(error)),
            Ok(package) => match self.editor.import(ThemeImport::ZedExtension(&package), cx) {
                Ok(receipt) => SharedString::from(match receipt.installed.len() {
                    1 => format!("Installed 1 theme from {}.", extension.name),
                    count => format!("Installed {count} themes from {}.", extension.name),
                }),
                Err(error) => SharedString::from(import_failure_message(error)),
            },
        };
        self.zed_extensions.status = Some(message);
        cx.notify();
    }

    /// The installed version of every extension that contributed an installed theme.
    pub(super) fn installed_extensions(&self) -> BTreeMap<String, String> {
        self.editor
            .theme_summaries()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|summary| summary.package)
            .map(|ThemePackage { id, version }| (id, version))
            .collect()
    }

    pub(super) fn render_zed_extensions(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let secondary = |text: SharedString| {
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.text_secondary))
                .whitespace_normal()
                .child(text)
        };
        let mut content = div()
            .debug_selector(|| "settings-zed-extensions".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(8.0));
        if self.zed_extensions.registry.is_none() {
            return content
                .child(secondary("The Zed extension registry is unavailable.".into()))
                .into_any_element();
        }
        match self.zed_extensions.listing.clone() {
            Listing::NotRequested => {
                let owner = cx.weak_entity();
                content = content
                    .child(secondary(
                        "Browse the themes published to the Zed extension registry. SpaceTerm contacts the registry only when you browse it."
                            .into(),
                    ))
                    .child(
                        div().child(action_button(
                            BROWSE_SELECTOR,
                            "Browse Zed Themes",
                            true,
                            move |_, cx| {
                                let _ = owner.update(cx, |settings, cx| {
                                    settings.browse_zed_extensions(cx);
                                });
                            },
                        )),
                    );
            }
            Listing::Loading => {
                content = content.child(secondary("Loading the Zed extension registry…".into()));
            }
            Listing::Failed(error) => {
                let owner = cx.weak_entity();
                content = content
                    .child(secondary(registry_failure_message(error).into()))
                    .child(div().child(action_button(
                        "settings-zed-extensions-retry",
                        "Try Again",
                        true,
                        move |_, cx| {
                            let _ = owner.update(cx, |settings, cx| {
                                settings.browse_zed_extensions(cx);
                            });
                        },
                    )));
            }
            Listing::Loaded(_) => {
                let installed = self.installed_extensions();
                let (shown, total) = self.zed_extensions.matches();
                content = content.child(
                    SearchField::new(
                        "settings-zed-extensions-search-frame",
                        self.zed_extensions.search.clone(),
                    )
                    .debug_selectors(
                        "settings-zed-extensions-search-frame",
                        "settings-zed-extensions-search-clear",
                    ),
                );
                if shown.is_empty() {
                    content = content.child(secondary(
                        format!("No Zed themes match “{}”.", self.zed_extensions.query).into(),
                    ));
                }
                let rows = shown
                    .iter()
                    .map(|extension| {
                        let action =
                            extension_action(&self.zed_extensions.installing, extension, &installed);
                        self.render_extension_row(extension, action, appearance, cx)
                    })
                    .collect::<Vec<_>>();
                content = content.child(div().flex().flex_col().w_full().children(rows));
                if total > shown.len() {
                    content = content.child(secondary(
                        format!(
                            "Showing {} of {total} extensions. Refine your search to see others.",
                            shown.len()
                        )
                        .into(),
                    ));
                }
            }
        }
        content
            .children(
                self.zed_extensions
                    .status
                    .clone()
                    .map(|status| {
                        secondary(status)
                            .debug_selector(|| "settings-zed-extensions-status".to_owned())
                    }),
            )
            .into_any_element()
    }

    fn render_extension_row(
        &self,
        extension: &RegistryExtension,
        action: ExtensionAction,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let mut byline = Vec::new();
        if !extension.authors.is_empty() {
            byline.push(format!("by {}", extension.authors.join(", ")));
        }
        byline.push(download_count(extension.downloads));
        let label = match action {
            ExtensionAction::Install => "Install",
            ExtensionAction::Installing => "Installing…",
            ExtensionAction::Update => "Update",
            ExtensionAction::Installed => "Installed",
        };
        let enabled = matches!(action, ExtensionAction::Install | ExtensionAction::Update)
            && self.editor.editable();
        let owner = cx.weak_entity();
        let target = extension.clone();
        let selector = format!("settings-zed-extension-{}", extension.id);
        let action_selector = format!("settings-zed-extension-action-{}", extension.id);
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(appearance.spacing(10.0))
            .py(appearance.spacing(6.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Body))
                            .text_color(gpui_color(colors.text))
                            .child(SharedString::from(extension.name.clone())),
                    )
                    .children(extension.description.clone().map(|description| {
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(colors.text_secondary))
                            .child(SharedString::from(description))
                    }))
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(colors.text_muted))
                            .child(SharedString::from(byline.join(" · "))),
                    ),
            )
            .child(
                spaceterm_ui::Button::new(SharedString::from(action_selector.clone()), label)
                    .variant(spaceterm_ui::ButtonVariant::Outline)
                    .size(spaceterm_ui::ButtonSize::Small)
                    .disabled(!enabled)
                    .tab_stop(true)
                    .debug_selector(action_selector)
                    .on_activate(move |_, _, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.install_zed_extension(target.clone(), cx);
                        });
                    }),
            )
            .into_any_element()
    }
}

/// What one extension's action offers, given the installs in flight and installed versions.
fn extension_action(
    installing: &BTreeSet<String>,
    extension: &RegistryExtension,
    installed: &BTreeMap<String, String>,
) -> ExtensionAction {
    if installing.contains(&extension.id) {
        return ExtensionAction::Installing;
    }
    match installed.get(&extension.id) {
        None => ExtensionAction::Install,
        Some(version) if *version == extension.version => ExtensionAction::Installed,
        Some(_) => ExtensionAction::Update,
    }
}

/// A download count in the short form a gallery uses.
fn download_count(downloads: u64) -> String {
    let (value, unit) = match downloads {
        0..1_000 => return format!("{downloads} downloads"),
        1_000..1_000_000 => (downloads as f64 / 1_000.0, "K"),
        _ => (downloads as f64 / 1_000_000.0, "M"),
    };
    if value < 10.0 {
        format!("{value:.1}{unit} downloads")
    } else {
        format!("{value:.0}{unit} downloads")
    }
}

/// Content-free wording for one registry failure.
fn registry_failure_message(error: RegistryError) -> &'static str {
    match error {
        RegistryError::Unreachable => {
            "SpaceTerm could not reach the Zed extension registry. Check your connection and try again."
        }
        RegistryError::Refused => "The Zed extension registry refused the request.",
        RegistryError::TooLarge => "That download is larger than SpaceTerm accepts.",
        RegistryError::InvalidResponse | RegistryError::InvalidArchive => {
            "The Zed extension registry sent something SpaceTerm could not read."
        }
        RegistryError::NoThemes => "That extension contains no themes SpaceTerm can use.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(version: &str) -> RegistryExtension {
        RegistryExtension {
            id: String::from("sample"),
            name: String::from("Sample"),
            version: version.to_owned(),
            description: None,
            authors: Vec::new(),
            downloads: 0,
        }
    }

    #[test]
    fn the_action_follows_the_installed_version_and_installs_in_flight() {
        let installed = BTreeMap::from([(String::from("sample"), String::from("1.0.0"))]);
        let installing = BTreeSet::from([String::from("sample")]);
        let none = BTreeSet::new();

        assert_eq!(
            extension_action(&none, &listed("1.0.0"), &BTreeMap::new()),
            ExtensionAction::Install
        );
        assert_eq!(
            extension_action(&none, &listed("1.0.0"), &installed),
            ExtensionAction::Installed
        );
        assert_eq!(
            extension_action(&none, &listed("1.1.0"), &installed),
            ExtensionAction::Update
        );
        assert_eq!(
            extension_action(&installing, &listed("1.1.0"), &installed),
            ExtensionAction::Installing
        );
    }

    #[test]
    fn download_counts_use_short_units() {
        assert_eq!(download_count(950), "950 downloads");
        assert_eq!(download_count(1_240), "1.2K downloads");
        assert_eq!(download_count(48_000), "48K downloads");
        assert_eq!(download_count(1_147_221), "1.1M downloads");
    }
}
