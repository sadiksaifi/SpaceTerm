//! Get More Themes: a sheet that finds Zed extensions and installs their themes, or imports a Zed
//! theme file.
//!
//! SpaceTerm contacts the registry only when the sheet opens, and only once per window: the
//! listing is small, searched locally, and discarded with the window. Installing downloads one
//! extension, translates its themes, and adds them to the installed themes without applying any.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, FocusHandle, SharedString, WeakEntity, Window, div, px};
use spaceterm_ui::{
    Dialog, DialogCloseDecision, DialogInitialFocus, DialogSize, FrameSpinner, FuzzyTarget, Icon,
    IconName, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId, ProgressSize,
    SearchField, TextInput, TextInputEscapeBehavior, TextInputEvent, TextInputReturnBehavior,
    TextInputVariant, fuzzy_filter,
};

use crate::appearance::{CatalogError, ImportError, ThemePackage, ZedExtension};
use crate::settings::{SettingsError, ThemeImport};
use crate::theme_registry::{RegistryError, RegistryExtension, ZedThemeRegistry};
use crate::ui::appearance::{ChromeAppearance, gpui_color, shared_chrome};
use crate::ui::chrome_geometry::HAIRLINE;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

use super::SettingsWindow;
use super::controls::action_button;
use super::import::{ImportError as ThemeReadError, read_theme_document};

/// Results beyond this many ask for a narrower search instead of growing the list.
const MAX_RESULTS: usize = 40;

pub(super) const SEARCH_SELECTOR: &str = "settings-theme-store-search";
pub(super) const IMPORT_SELECTOR: &str = "settings-theme-store-import";
pub(super) const DONE_SELECTOR: &str = "settings-theme-store-done";

/// What the sheet knows about the registry listing.
#[derive(Clone, Debug)]
pub(super) enum Listing {
    /// The sheet has not opened, so SpaceTerm has not contacted the registry.
    NotRequested,
    Loading,
    Loaded(Arc<[RegistryExtension]>),
    Failed(RegistryError),
}

/// What one extension's row offers, derived from what is installed and in flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExtensionAction {
    Install,
    Installing,
    /// Installed at an older version.
    Update,
    /// Installed at the listed version, so the row offers removal instead.
    Installed,
}

/// The sheet's body: owns the injected registry, the listing, and installs in flight. Installed
/// themes belong to the Settings Window that owns this store.
pub(super) struct ThemeStore {
    owner: WeakEntity<SettingsWindow>,
    registry: Option<ZedThemeRegistry>,
    listing: Listing,
    installing: BTreeSet<String>,
    /// What the last install or import did, until the sheet next opens.
    status: Option<SharedString>,
    search: Entity<TextInput>,
    query: SharedString,
}

impl ThemeStore {
    pub(super) fn new(
        owner: WeakEntity<SettingsWindow>,
        registry: Option<ZedThemeRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            TextInput::new(SEARCH_SELECTOR, "Search Zed themes", String::new(), window, cx)
                .placeholder("Search Zed themes")
                .variant(TextInputVariant::Bare)
                .return_behavior(TextInputReturnBehavior::Propagate)
                .escape_behavior(TextInputEscapeBehavior::Propagate)
                .input_length_limit(Some(128))
                .emit_programmatic_changes(true)
                .debug_selector(SEARCH_SELECTOR)
        });
        cx.subscribe(&search, |store, search, event: &TextInputEvent, cx| {
            if matches!(event, TextInputEvent::ValueChanged(_)) {
                store.query = SharedString::from(search.read(cx).value().to_owned());
                cx.notify();
            }
        })
        .detach();
        Self {
            owner,
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
    pub(super) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    fn search_focus(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle()
    }

    /// The extensions matching the query, best match first, or the most downloaded first when
    /// there is no query, with the number that matched.
    fn matches(&self) -> (Vec<RegistryExtension>, usize) {
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

    /// Fetches the registry listing unless it is loaded or loading.
    fn browse(&mut self, cx: &mut Context<Self>) {
        let Some(registry) = self.registry.clone() else {
            return;
        };
        if matches!(self.listing, Listing::Loading | Listing::Loaded(_)) {
            return;
        }
        self.listing = Listing::Loading;
        cx.notify();
        cx.spawn(async move |store, cx| {
            let listing = cx
                .background_executor()
                .spawn(async move { registry.list() })
                .await;
            let _ = store.update(cx, |store, cx| {
                store.listing = match listing {
                    Ok(extensions) => Listing::Loaded(extensions.into()),
                    Err(error) => Listing::Failed(error),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn install(&mut self, extension: RegistryExtension, cx: &mut Context<Self>) {
        let Some(registry) = self.registry.clone() else {
            return;
        };
        if !self.editable(cx) || !self.installing.insert(extension.id.clone()) {
            return;
        }
        self.status = None;
        cx.notify();
        cx.spawn(async move |store, cx| {
            let download = cx
                .background_executor()
                .spawn({
                    let extension = extension.clone();
                    async move { registry.download(&extension) }
                })
                .await;
            let _ = store.update(cx, |store, cx| {
                store.finish_install(&extension, download, cx);
            });
        })
        .detach();
    }

    pub(super) fn finish_install(
        &mut self,
        extension: &RegistryExtension,
        download: Result<ZedExtension, RegistryError>,
        cx: &mut Context<Self>,
    ) {
        self.installing.remove(&extension.id);
        let message = match download {
            Err(error) => SharedString::from(registry_failure_message(error)),
            Ok(package) => {
                let installed = self.owner.update(cx, |settings, cx| {
                    settings
                        .editor
                        .import(ThemeImport::ZedExtension(&package), cx)
                });
                match installed {
                    Ok(Ok(receipt)) => SharedString::from(match receipt.installed.len() {
                        1 => format!("Installed 1 theme from {}.", extension.name),
                        count => format!("Installed {count} themes from {}.", extension.name),
                    }),
                    Ok(Err(error)) => SharedString::from(import_failure_message(error)),
                    Err(_) => SharedString::from("Those themes could not be installed."),
                }
            }
        };
        self.status = Some(message);
        cx.notify();
    }

    /// Removes every theme the extension installed. The sheet cannot stack a confirmation, and Get
    /// sits in the same place to restore them.
    fn remove(&mut self, extension: &RegistryExtension, cx: &mut Context<Self>) {
        if !self.editable(cx) {
            return;
        }
        let removed = self.owner.update(cx, |settings, cx| {
            let ids = settings
                .editor
                .theme_summaries()
                .unwrap_or_default()
                .into_iter()
                .filter(|summary| {
                    summary
                        .package
                        .as_ref()
                        .is_some_and(|package| package.id == extension.id)
                })
                .map(|summary| summary.id)
                .collect::<Vec<_>>();
            settings
                .remove_installed_themes(&ids, cx)
                .then_some(ids.len())
        });
        self.status = Some(match removed {
            Ok(Some(1)) => format!("Removed 1 theme from {}.", extension.name).into(),
            Ok(Some(count)) => format!("Removed {count} themes from {}.", extension.name).into(),
            _ => SharedString::from("Those themes could not be removed."),
        });
        cx.notify();
    }

    /// Asks for a Zed theme file and installs every theme in it.
    fn begin_import(&mut self, cx: &mut Context<Self>) {
        let Some(opener) = cx
            .try_global::<crate::app::SelectedFileAccess>()
            .map(|access| Arc::clone(&access.0))
        else {
            self.status = Some("File import is unavailable.".into());
            cx.notify();
            return;
        };
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        self.status = None;
        cx.notify();
        cx.spawn(async move |store, cx| {
            let Ok(Ok(Some(paths))) = selection.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_executor()
                .spawn(async move { read_theme_document(&path, opener.as_ref()) })
                .await;
            let _ = store.update(cx, |store, cx| store.finish_import(read, cx));
        })
        .detach();
    }

    pub(super) fn finish_import(
        &mut self,
        read: Result<Vec<u8>, ThemeReadError>,
        cx: &mut Context<Self>,
    ) {
        let message = match read {
            Ok(bytes) => self
                .owner
                .update(cx, |settings, cx| {
                    import_family(&bytes, |source| settings.editor.import(source, cx))
                })
                .unwrap_or_else(|_| SharedString::from("Those themes could not be installed.")),
            Err(error) => SharedString::from(error.message()),
        };
        self.status = Some(message);
        cx.notify();
    }

    fn editable(&self, cx: &App) -> bool {
        self.owner
            .upgrade()
            .is_some_and(|settings| settings.read(cx).editor.editable())
    }

    /// The installed version of every extension that contributed an installed theme.
    fn installed_extensions(&self, cx: &App) -> BTreeMap<String, String> {
        let Some(settings) = self.owner.upgrade() else {
            return BTreeMap::new();
        };
        settings
            .read(cx)
            .editor
            .theme_summaries()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|summary| summary.package)
            .map(|ThemePackage { id, version }| (id, version))
            .collect()
    }

    fn render_listing(&self, appearance: &ChromeAppearance, cx: &mut Context<Self>) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let secondary = |text: SharedString| {
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.text_secondary))
                .whitespace_normal()
                .child(text)
        };
        // A state with no rows sits centered where the rows would be, so the sheet keeps its size.
        let placeholder = || {
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(appearance.spacing(10.0))
                .min_h(appearance.spacing(160.0))
                .text_center()
        };
        match self.listing.clone() {
            _ if self.registry.is_none() => placeholder()
                .child(secondary(
                    "The Zed extension registry is unavailable. You can still import a theme file."
                        .into(),
                ))
                .into_any_element(),
            Listing::NotRequested | Listing::Loading => placeholder()
                .child(
                    FrameSpinner::new("settings-theme-store-loading", "Loading themes")
                        .size(ProgressSize::Compact),
                )
                .child(secondary("Loading themes…".into()))
                .into_any_element(),
            Listing::Failed(error) => {
                let store = cx.weak_entity();
                placeholder()
                    .child(secondary(registry_failure_message(error).into()))
                    .child(action_button(
                        "settings-theme-store-retry",
                        "Try Again",
                        true,
                        move |_, cx| {
                            let _ = store.update(cx, |store, cx| store.browse(cx));
                        },
                    ))
                    .into_any_element()
            }
            Listing::Loaded(_) => {
                let installed = self.installed_extensions(cx);
                let editable = self.editable(cx);
                let (shown, total) = self.matches();
                if shown.is_empty() {
                    return placeholder()
                        .child(secondary(
                            format!("No Zed themes match “{}”.", self.query).into(),
                        ))
                        .into_any_element();
                }
                let divider = gpui_color(colors.border);
                let count = shown.len();
                let rows = shown.iter().enumerate().map(|(index, extension)| {
                    let action = extension_action(&self.installing, extension, &installed);
                    render_extension_row(extension, action, editable, appearance, cx)
                        .when(index + 1 < count, |row| {
                            row.border_b(px(HAIRLINE)).border_color(divider)
                        })
                });
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .children(rows)
                    .when(total > count, |list| {
                        list.child(div().pt(appearance.spacing(10.0)).child(secondary(
                            format!(
                                "Showing {count} of {total} extensions. Search to find others."
                            )
                            .into(),
                        )))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Render for ThemeStore {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = shared_chrome(cx);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let store = cx.weak_entity();
        div()
            .debug_selector(|| "settings-theme-store".to_owned())
            .chrome_text(appearance.typography.style(TextRole::Body))
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(12.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(10.0))
                    .child(
                        div().flex_1().min_w_0().child(
                            SearchField::new(
                                "settings-theme-store-search-frame",
                                self.search.clone(),
                            )
                            .debug_selectors(
                                "settings-theme-store-search-frame",
                                "settings-theme-store-search-clear",
                            ),
                        ),
                    )
                    .child(div().flex_none().child(action_button(
                        IMPORT_SELECTOR,
                        "Import from File…",
                        self.editable(cx),
                        move |_, cx| {
                            let _ = store.update(cx, |store, cx| store.begin_import(cx));
                        },
                    ))),
            )
            .child(self.render_listing(&appearance, cx))
            .children(self.status.clone().map(|status| {
                div()
                    .debug_selector(|| "settings-theme-store-status".to_owned())
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(colors.text_secondary))
                    .whitespace_normal()
                    .child(status)
            }))
    }
}

impl SettingsWindow {
    /// Opens Get More Themes. Opening it is the only thing that contacts the registry.
    pub(super) fn open_theme_store(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.theme_store.clone();
        let focus = store.update(cx, |store, cx| {
            store.status = None;
            store.browse(cx);
            store.search_focus(cx)
        });
        let dialog = Dialog::new(
            ModalId::new("settings-theme-store"),
            "Get more themes",
            "Get More Themes",
            vec![
                ModalAction::new((), "Done", ModalActionRole::Cancel, DONE_SELECTOR)
                    .with_emphasis(ModalActionEmphasis::Prominent),
            ],
            DialogInitialFocus::Body(focus),
        )
        .description(
            "Themes from the Zed extension registry. Installed themes appear in your list, ready \
             to choose.",
        )
        .size(DialogSize::Wide)
        .body(store);
        let presented = dialog.present(
            window,
            cx,
            |_, _, _| DialogCloseDecision::Allow,
            |_, _| {},
        );
        if presented.is_err() {
            eprintln!("failed to present the SpaceTerm Get More Themes sheet");
        }
    }
}

/// One extension, the way a store lists an app: what it is, who made it, and one action.
fn render_extension_row(
    extension: &RegistryExtension,
    action: ExtensionAction,
    editable: bool,
    appearance: &ChromeAppearance,
    cx: &mut Context<ThemeStore>,
) -> gpui::Div {
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
    let mut byline = Vec::new();
    if !extension.authors.is_empty() {
        byline.push(extension.authors.join(", "));
    }
    byline.push(download_count(extension.downloads));
    let selector = format!("settings-zed-extension-action-{}", extension.id);
    let remove = || {
        let store = cx.weak_entity();
        let target = extension.clone();
        let selector = format!("settings-zed-extension-remove-{}", extension.id);
        spaceterm_ui::Button::new(SharedString::from(selector.clone()), "Remove")
            .variant(spaceterm_ui::ButtonVariant::Outline)
            .size(spaceterm_ui::ButtonSize::Small)
            .disabled(!editable)
            .tab_stop(true)
            .debug_selector(selector)
            .on_activate(move |_, _, cx| {
                let _ = store.update(cx, |store, cx| store.remove(&target, cx));
            })
    };
    let button = |label: &'static str| {
        let store = cx.weak_entity();
        let target = extension.clone();
        spaceterm_ui::Button::new(SharedString::from(selector.clone()), label)
            .variant(spaceterm_ui::ButtonVariant::Outline)
            .size(spaceterm_ui::ButtonSize::Small)
            .disabled(!editable)
            .tab_stop(true)
            .debug_selector(selector.clone())
            .on_activate(move |_, _, cx| {
                let _ = store.update(cx, |store, cx| store.install(target.clone(), cx));
            })
            .into_any_element()
    };
    let trailing = match action {
        ExtensionAction::Install => button("Get"),
        ExtensionAction::Update => button("Update"),
        ExtensionAction::Installing => FrameSpinner::new(
            SharedString::from(format!("{selector}-progress")),
            "Installing",
        )
        .size(ProgressSize::Compact)
        .into_any_element(),
        ExtensionAction::Installed => div()
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(10.0))
            .child(
                div()
                    .debug_selector({
                        let selector = format!("settings-zed-extension-installed-{}", extension.id);
                        move || selector
                    })
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(4.0))
                    .text_color(gpui_color(colors.text_secondary))
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .child(Icon::new(
                        IconName::Check,
                        appearance.icons.metrics(IconRole::Caption).glyph_size,
                        gpui_color(colors.text_secondary),
                    ))
                    .child("Installed"),
            )
            .child(remove())
            .into_any_element(),
    };
    let row_selector = format!("settings-zed-extension-{}", extension.id);
    div()
        .debug_selector(move || row_selector)
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap(appearance.spacing(12.0))
        .py(appearance.spacing(10.0))
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
                        .chrome_text(appearance.typography.style(TextRole::Caption))
                        .text_color(gpui_color(colors.text_muted))
                        .child(SharedString::from(byline.join(" · "))),
                ),
        )
        .child(
            div()
                .flex_none()
                .min_w(appearance.spacing(72.0))
                .flex()
                .justify_end()
                .child(trailing),
        )
}

/// What one extension's row offers, given the installs in flight and installed versions.
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

/// A download count in the short form a store uses.
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

fn import_family<'a>(
    bytes: &'a [u8],
    install: impl FnOnce(ThemeImport<'a>) -> Result<crate::settings::ImportReceipt, SettingsError>,
) -> SharedString {
    match install(ThemeImport::ZedFamily(bytes)) {
        Ok(receipt) => installed_message(receipt.installed.len()),
        Err(error) => import_failure_message(error).into(),
    }
}

fn import_failure_message(error: SettingsError) -> &'static str {
    match error {
        SettingsError::Catalog(CatalogError::TooManyThemes) => {
            "There is no room for those themes. Remove some installed themes and try again."
        }
        SettingsError::Busy => "Settings are busy. Try again when saving finishes.",
        SettingsError::Stale | SettingsError::Catalog(CatalogError::RevisionConflict) => {
            "Settings changed before the install finished. Try again."
        }
        SettingsError::Import(ImportError::TooLarge) => "That theme file is too large.",
        SettingsError::Import(ImportError::InvalidThemeCount) => {
            "That source contains no themes SpaceTerm can use, or too many."
        }
        SettingsError::Import(_) => "That is not a Zed theme file SpaceTerm can read.",
        _ => "Those themes could not be installed.",
    }
}

fn installed_message(installed: usize) -> SharedString {
    if installed == 1 {
        SharedString::from("Installed 1 theme.")
    } else {
        SharedString::from(format!("Installed {installed} themes."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
    use crate::settings::UserSettings;
    use crate::settings::storage::{SettingsStorage, StorageCommit, StorageError};

    struct EmptyStorage;

    impl SettingsStorage for EmptyStorage {
        fn quarantine(&self) -> Result<(), crate::settings::storage::StorageError> {
            Err(crate::settings::storage::StorageError::Unavailable)
        }
        fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
            Ok(None)
        }

        fn write(
            &self,
            _: &[u8],
            _: Option<&SecureEntryIdentity>,
        ) -> Result<StorageCommit, StorageError> {
            panic!("importing a preview must not write settings");
        }
    }

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

    #[test]
    fn reimporting_a_zed_family_replaces_its_themes() {
        let settings = UserSettings::load(std::sync::Arc::new(EmptyStorage));
        let token = settings.begin_preview(0).unwrap();
        let bytes = br##"{"themes":[{"name":"Sample","appearance":"dark","style":{"terminal.foreground":"#abcdef"}}]}"##;
        let install = || {
            import_family(bytes, |source| {
                settings.import_preview(&token, settings.snapshot().catalog_revision, source)
            })
        };

        assert_eq!(install(), installed_message(1));
        assert_eq!(install(), installed_message(1));
        assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 1);
    }

    #[test]
    fn a_malformed_zed_family_installs_nothing_and_a_corrected_retry_is_clean() {
        let settings = UserSettings::load(std::sync::Arc::new(EmptyStorage));
        let token = settings.begin_preview(0).unwrap();
        let invalid = br##"{"themes":[{"name":"First","appearance":"dark","style":{}},{"name":"Broken","appearance":"sepia","style":{}}]}"##;
        let corrected = br##"{"themes":[{"name":"First","appearance":"dark","style":{}},{"name":"Second","appearance":"light","style":{}}]}"##;
        let install = |bytes| {
            import_family(bytes, |source| {
                settings.import_preview(&token, settings.snapshot().catalog_revision, source)
            })
        };

        assert_eq!(
            install(invalid).as_ref(),
            "That is not a Zed theme file SpaceTerm can read."
        );
        assert!(settings.snapshot().candidate.terminal_themes.is_empty());

        assert_eq!(install(corrected), installed_message(2));
        assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 2);
    }

    #[test]
    fn install_failures_have_content_free_messages() {
        for (error, expected) in [
            (
                SettingsError::Busy,
                "Settings are busy. Try again when saving finishes.",
            ),
            (
                SettingsError::Catalog(CatalogError::TooManyThemes),
                "There is no room for those themes. Remove some installed themes and try again.",
            ),
            (
                SettingsError::Import(ImportError::InvalidThemeCount),
                "That source contains no themes SpaceTerm can use, or too many.",
            ),
        ] {
            let mut attempts = 0;
            let message = import_family(b"{}", |source| {
                assert!(matches!(source, ThemeImport::ZedFamily(_)));
                attempts += 1;
                Err(error)
            });
            assert_eq!(attempts, 1);
            assert_eq!(message.as_ref(), expected);
        }
    }
}
