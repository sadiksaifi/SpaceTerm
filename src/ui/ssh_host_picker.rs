#[cfg(test)]
use spaceterm_ui::CommandPaletteReplacementFocus;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Render, Window};
use spaceterm_ui::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteAction, CommandPaletteActivationPolicy,
    CommandPaletteCloseReason, CommandPaletteEmpty, CommandPaletteEmptyAction, CommandPaletteEvent,
    CommandPaletteItem, CommandPaletteLifecycleEvent, CommandPaletteMatching, FuzzyTarget, Icon,
    IconName, fuzzy_filter,
};

use super::chrome_icons::IconRole;
use crate::domain::SshDestination;
use crate::ssh::destination::{
    DestinationQueryResolution, SshHostAlias, resolve_destination_query,
};
use crate::ssh::host_config::{DiscoveredSshHost, HostConfigIssueKind, HostDiscovery};

const MAXIMUM_DESTINATION_BYTES: usize = 1024;
const ADD_HOST_ACTION: &str = "ssh-host-picker-add";
const ADD_HOST_LABEL: &str = "Add SSH Host";
const HEADER_ADD_HOST_SELECTOR: &str = "ssh-host-picker-header-add";
const EMPTY_ADD_HOST_SELECTOR: &str = "ssh-host-picker-empty-add";
const DISCOVERY_WARNING_SELECTOR: &str = "ssh-host-picker-discovery-warning";
const HOST_ROW_SELECTOR: &str = "ssh-host-picker-row";
const MAXIMUM_DISCOVERY_WARNING_BYTES: usize = 256;
const HOST_DISCOVERY_ISSUE_CLASS_COUNT: usize = 4;

pub(super) trait HostDiscoveryProvider: Send + Sync {
    fn discover(&self) -> HostDiscovery;
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SshHostPickerItemId {
    DiscoveryWarning,
    Configured(SshHostAlias),
    UserOverride {
        destination: SshDestination,
        alias: SshHostAlias,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HostDiscoveryIssueClass {
    Unreadable,
    Malformed,
    IncludeCycle,
    SafetyLimit,
}

impl HostDiscoveryIssueClass {
    const fn action(self) -> &'static str {
        match self {
            Self::Unreadable => "check SSH config file permissions",
            Self::Malformed => "fix malformed SSH config directives",
            Self::IncludeCycle => "remove recursive Include directives",
            Self::SafetyLimit => "narrow Include patterns or reduce config size",
        }
    }
}

fn issue_class(kind: HostConfigIssueKind) -> HostDiscoveryIssueClass {
    match kind {
        HostConfigIssueKind::Read => HostDiscoveryIssueClass::Unreadable,
        HostConfigIssueKind::InvalidUtf8 | HostConfigIssueKind::MalformedLine => {
            HostDiscoveryIssueClass::Malformed
        }
        HostConfigIssueKind::IncludeCycle => HostDiscoveryIssueClass::IncludeCycle,
        HostConfigIssueKind::IncludeDepthLimit
        | HostConfigIssueKind::FileLimit
        | HostConfigIssueKind::TotalByteLimit
        | HostConfigIssueKind::FileByteLimit
        | HostConfigIssueKind::GlobLimit
        | HostConfigIssueKind::ResultLimit
        | HostConfigIssueKind::DeclarationLimit
        | HostConfigIssueKind::TokenLimit => HostDiscoveryIssueClass::SafetyLimit,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostDiscoveryDiagnostic {
    title: &'static str,
    description: String,
}

impl HostDiscoveryDiagnostic {
    fn for_discovery(discovery: &HostDiscovery) -> Option<Self> {
        let mut classes = BTreeSet::new();
        for issue in &discovery.issues {
            classes.insert(issue_class(issue.kind()));
            if classes.len() == HOST_DISCOVERY_ISSUE_CLASS_COUNT {
                break;
            }
        }
        if classes.is_empty() {
            return None;
        }
        let actions = classes
            .into_iter()
            .map(HostDiscoveryIssueClass::action)
            .collect::<Vec<_>>()
            .join("; ");
        let description = format!("To load all hosts, {actions}, then refresh.");
        debug_assert!(description.len() <= MAXIMUM_DISCOVERY_WARNING_BYTES);
        Some(Self {
            title: if discovery.hosts.is_empty() {
                "No safe SSH hosts were found"
            } else {
                "SSH host list is incomplete"
            },
            description,
        })
    }

    fn into_palette_item(self) -> CommandPaletteItem<SshHostPickerItemId> {
        CommandPaletteItem::new(SshHostPickerItemId::DiscoveryWarning, self.title)
            .description(self.description)
            .section("SSH Config Warning")
            .disabled(true)
            .leading_icon(|foreground, size| {
                Icon::new(IconName::TriangleAlert, size, foreground).into_any_element()
            })
            .trailing(CommandPaletteAccessory::Status("Action needed".into()))
            .debug_selector(DISCOVERY_WARNING_SELECTOR)
    }
}

fn empty_state(discovery: &HostDiscovery, query: &str) -> CommandPaletteEmpty {
    let empty = if query.is_empty() && discovery.hosts.is_empty() && discovery.issues.is_empty() {
        CommandPaletteEmpty::new("No SSH hosts configured")
            .description("Add a host to connect to it from SpaceTerm.")
    } else {
        CommandPaletteEmpty::new("No matching SSH hosts")
            .description("Check the host name, or add it as a new SSH host.")
    };
    empty.action(
        CommandPaletteEmptyAction::new(ADD_HOST_ACTION, ADD_HOST_LABEL)
            .debug_selector(EMPTY_ADD_HOST_SELECTOR),
    )
}

/// Builds the always-available Add SSH Host control at the current appearance's icon size.
fn add_host_header_action(cx: &gpui::App) -> CommandPaletteAction {
    let icon_size = super::appearance::chrome(cx)
        .icons
        .metrics(IconRole::Control)
        .glyph_size;
    CommandPaletteAction::new(ADD_HOST_ACTION, ADD_HOST_LABEL, move |tint| {
        Icon::new(IconName::Plus, icon_size, tint).into_any_element()
    })
    .debug_selector(HEADER_ADD_HOST_SELECTOR)
}

#[derive(Clone, Eq, PartialEq)]
struct HostPickerRow {
    id: SshHostPickerItemId,
    destination: SshDestination,
    label: String,
    subtitle: String,
    label_matched_indices: Vec<usize>,
    subtitle_matched_indices: Vec<usize>,
}

impl fmt::Debug for HostPickerRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HostPickerRow(<redacted>)")
    }
}

impl HostPickerRow {
    #[cfg(test)]
    fn label(&self) -> &str {
        &self.label
    }

    fn into_palette_item(self) -> CommandPaletteItem<SshHostPickerItemId> {
        CommandPaletteItem::new(self.id, self.label)
            .description(self.subtitle)
            .matched_indices(self.label_matched_indices)
            .matched_description_indices(self.subtitle_matched_indices)
            .debug_selector(HOST_ROW_SELECTOR)
    }
}

fn host_rows_for_query(discovery: &HostDiscovery, query: &str) -> Vec<HostPickerRow> {
    let mut seen = BTreeSet::new();
    let mut hosts = discovery
        .hosts
        .iter()
        .filter(|host| seen.insert(host.alias().as_str().to_owned()))
        .collect::<Vec<_>>();
    hosts.sort_by(|left, right| {
        left.alias()
            .as_str()
            .to_lowercase()
            .cmp(&right.alias().as_str().to_lowercase())
            .then_with(|| left.alias().as_str().cmp(right.alias().as_str()))
    });
    let configured_rows = hosts
        .iter()
        .filter_map(|host| configured_host_row(host))
        .collect::<Vec<_>>();
    let mut rows = fuzzy_filter(&configured_rows, query, |row| {
        FuzzyTarget::new(&row.label).field(&row.subtitle)
    })
    .into_iter()
    .map(|matched| {
        let mut row = configured_rows[matched.item_index()].clone();
        row.label_matched_indices = matched.field_highlight_indices(0);
        row.subtitle_matched_indices = matched.field_highlight_indices(1);
        row
    })
    .collect::<Vec<_>>();

    let aliases = hosts
        .iter()
        .map(|host| host.alias().clone())
        .collect::<Vec<_>>();
    if let Ok(DestinationQueryResolution::Configured {
        destination,
        alias,
        explicit_user: Some(user),
    }) = resolve_destination_query(query, &aliases, MAXIMUM_DESTINATION_BYTES)
    {
        rows.insert(
            0,
            HostPickerRow {
                id: SshHostPickerItemId::UserOverride {
                    destination: destination.clone(),
                    alias: alias.clone(),
                },
                destination,
                label: query.to_owned(),
                subtitle: format!("Connect as {user} through {}", alias.as_str()),
                label_matched_indices: (0..query.chars().count()).collect(),
                subtitle_matched_indices: Vec::new(),
            },
        );
    }
    rows
}

fn configured_host_row(host: &DiscoveredSshHost) -> Option<HostPickerRow> {
    let destination = SshDestination::new(host.alias().as_str().to_owned()).ok()?;
    Some(HostPickerRow {
        id: SshHostPickerItemId::Configured(host.alias().clone()),
        destination,
        label: host.alias().as_str().to_owned(),
        subtitle: host.subtitle(),
        label_matched_indices: Vec::new(),
        subtitle_matched_indices: Vec::new(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SshHostPickerLifecycleEvent {
    Opened,
    Closed(CommandPaletteCloseReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SshHostPickerEvent {
    Lifecycle(SshHostPickerLifecycleEvent),
    SelectDestination(SshDestination),
    RequestAddHost,
}

pub(super) struct SshHostPicker {
    palette: Entity<CommandPalette<SshHostPickerItemId>>,
    discovery_provider: Arc<dyn HostDiscoveryProvider>,
    discovery: HostDiscovery,
    rows: Vec<HostPickerRow>,
    open: bool,
    refresh_generation: u64,
    retained_query: String,
    retained_selection: Option<SshHostPickerItemId>,
    observed_selection: Option<SshHostPickerItemId>,
}

impl SshHostPicker {
    pub(super) fn new(
        discovery_provider: Arc<dyn HostDiscoveryProvider>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new("Connect to SSH Host", Vec::new(), window, cx);
            palette.set_matching(CommandPaletteMatching::Caller, cx);
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |picker, _, event: &CommandPaletteEvent<SshHostPickerItemId>, window, cx| {
                picker.reduce_palette_event(event, window, cx);
            },
        )
        .detach();
        cx.observe(&palette, |picker, palette, cx| {
            let selected = palette.read(cx).selected_item_id().cloned();
            picker.observe_selection(selected);
        })
        .detach();

        Self {
            palette,
            discovery_provider,
            discovery: HostDiscovery::default(),
            rows: Vec::new(),
            open: false,
            refresh_generation: 0,
            retained_query: String::new(),
            retained_selection: None,
            observed_selection: None,
        }
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.palette
            .update(cx, |palette, cx| palette.open(window, cx))
    }

    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.palette
            .update(cx, |palette, cx| palette.dismiss(window, cx))
    }

    #[cfg(test)]
    pub(super) fn dismiss_for_replacement(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<CommandPaletteReplacementFocus> {
        self.capture_selection(cx);
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_for_replacement(window, cx)
        })
    }

    #[cfg(test)]
    pub(super) fn open_replacing(
        &mut self,
        replacement: CommandPaletteReplacementFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.palette.update(cx, |palette, cx| {
            palette.open_replacing(replacement, window, cx)
        })
    }

    pub(super) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.capture_selection(cx);
        self.start_refresh(window, cx);
    }

    #[cfg(test)]
    pub(super) const fn is_open(&self) -> bool {
        self.open
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<SshHostPickerItemId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                self.open = true;
                let add_host = add_host_header_action(cx);
                self.palette.update(cx, |palette, cx| {
                    palette.set_header_actions(vec![add_host], cx);
                    palette.set_items(Vec::new(), cx);
                    palette.set_preferred_item(self.retained_selection.clone(), cx);
                    if palette.query() != self.retained_query {
                        palette.set_query(self.retained_query.clone(), cx);
                    }
                });
                self.start_refresh(window, cx);
                cx.emit(SshHostPickerEvent::Lifecycle(
                    SshHostPickerLifecycleEvent::Opened,
                ));
                cx.notify();
            }
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(reason)) => {
                self.capture_selection(cx);
                self.open = false;
                self.refresh_generation = self.refresh_generation.wrapping_add(1);
                cx.emit(SshHostPickerEvent::Lifecycle(
                    SshHostPickerLifecycleEvent::Closed(*reason),
                ));
                cx.notify();
            }
            CommandPaletteEvent::QueryChanged(query) => {
                let query_changed = self.retained_query != query.text();
                self.retained_query = query.text().to_owned();
                if query_changed {
                    self.retained_selection = None;
                }
                self.rebuild_rows(cx);
            }
            CommandPaletteEvent::Activated(activation) => {
                if let Some(row) = self.rows.iter().find(|row| &row.id == activation.item_id()) {
                    cx.emit(SshHostPickerEvent::SelectDestination(
                        row.destination.clone(),
                    ));
                    cx.notify();
                }
            }
            CommandPaletteEvent::HeaderAction(action) | CommandPaletteEvent::EmptyAction(action)
                if action.as_ref() == ADD_HOST_ACTION =>
            {
                cx.emit(SshHostPickerEvent::RequestAddHost);
                cx.notify();
            }
            _ => {}
        }
    }

    fn start_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        let generation = self.refresh_generation;
        self.palette
            .update(cx, |palette, cx| palette.set_loading(true, cx));
        let provider = Arc::clone(&self.discovery_provider);
        let background = cx.background_spawn(async move { provider.discover() });
        cx.spawn_in(window, async move |picker, cx| {
            let discovery = background.await;
            let _ = picker.update_in(cx, |picker, _, cx| {
                picker.apply_discovery(generation, discovery, cx);
            });
        })
        .detach();
    }

    fn apply_discovery(
        &mut self,
        generation: u64,
        discovery: HostDiscovery,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.open || self.refresh_generation != generation {
            return false;
        }
        self.discovery = discovery;
        self.rebuild_rows(cx);
        true
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        self.rows = host_rows_for_query(&self.discovery, &self.retained_query);
        let mut items = Vec::with_capacity(self.rows.len().saturating_add(1));
        if let Some(diagnostic) = HostDiscoveryDiagnostic::for_discovery(&self.discovery) {
            items.push(diagnostic.into_palette_item());
        }
        items.extend(
            self.rows
                .iter()
                .cloned()
                .map(HostPickerRow::into_palette_item),
        );
        self.palette.update(cx, |palette, cx| {
            palette.set_empty(empty_state(&self.discovery, &self.retained_query), cx);
            palette.set_preferred_item(self.retained_selection.clone(), cx);
            palette.set_items(items, cx);
        });
    }

    fn capture_selection(&mut self, cx: &gpui::App) {
        if let Some(selected) = self.palette.read(cx).selected_item_id().cloned() {
            self.retained_selection = Some(selected);
        }
    }

    fn observe_selection(&mut self, selected: Option<SshHostPickerItemId>) {
        if self.observed_selection == selected {
            return;
        }
        self.observed_selection = selected.clone();
        if selected.is_some() {
            self.retained_selection = selected;
        }
    }

    #[cfg(test)]
    fn palette(&self) -> Entity<CommandPalette<SshHostPickerItemId>> {
        self.palette.clone()
    }
}

impl EventEmitter<SshHostPickerEvent> for SshHostPicker {}

impl Render for SshHostPicker {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.palette.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::{FocusHandle, TestAppContext, VisualTestContext, div};

    use super::*;

    type RecordedHostPickerEvents = Rc<RefCell<Vec<SshHostPickerEvent>>>;
    type HostPickerWindow<'a> = (
        Entity<SshHostPickerHarness>,
        Entity<SshHostPicker>,
        RecordedHostPickerEvents,
        &'a mut VisualTestContext,
    );
    use crate::ssh::host_config::{
        HostConfigFilesystem, HostConfigFilesystemError, HostConfigRoots, HostDiscoveryLimits,
        discover_ssh_hosts,
    };

    #[test]
    fn row_and_event_debug_should_redact_destination_and_display_values() {
        let destination = SshDestination::new("user@sensitive-host".to_owned()).unwrap();
        let row = HostPickerRow {
            id: SshHostPickerItemId::Configured(
                SshHostAlias::new("sensitive-host".to_owned()).unwrap(),
            ),
            destination: destination.clone(),
            label: "sensitive-host".to_owned(),
            subtitle: "/sensitive/config".to_owned(),
            label_matched_indices: Vec::new(),
            subtitle_matched_indices: Vec::new(),
        };
        let event = SshHostPickerEvent::SelectDestination(destination);

        let debug = format!("{row:?} {event:?}");
        assert!(!debug.contains("sensitive"));
        assert_eq!(HOST_ROW_SELECTOR, "ssh-host-picker-row");
    }

    struct MemoryHostConfigFilesystem {
        files: BTreeMap<PathBuf, Vec<u8>>,
        unreadable: BTreeSet<PathBuf>,
    }

    impl HostConfigFilesystem for MemoryHostConfigFilesystem {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf, HostConfigFilesystemError> {
            Ok(path.to_path_buf())
        }

        fn read_file_limited(
            &self,
            path: &Path,
            maximum_bytes: usize,
        ) -> Result<Vec<u8>, HostConfigFilesystemError> {
            if self.unreadable.contains(path) {
                return Err(HostConfigFilesystemError::Unavailable);
            }
            let contents = self
                .files
                .get(path)
                .ok_or(HostConfigFilesystemError::Missing)?;
            Ok(contents
                .iter()
                .copied()
                .take(maximum_bytes.saturating_add(1))
                .collect())
        }

        fn read_directory_limited(
            &self,
            _: &Path,
            _: usize,
        ) -> Result<Vec<PathBuf>, HostConfigFilesystemError> {
            Ok(Vec::new())
        }
    }

    fn host_discovery_with_limits(
        managed: &[u8],
        user: &[u8],
        limits: HostDiscoveryLimits,
    ) -> HostDiscovery {
        let roots = HostConfigRoots {
            managed: PathBuf::from("/managed/ssh_config"),
            user: PathBuf::from("/home/test/.ssh/config"),
            home: PathBuf::from("/home/test"),
        };
        let filesystem = MemoryHostConfigFilesystem {
            files: BTreeMap::from([
                (roots.managed.clone(), managed.to_vec()),
                (roots.user.clone(), user.to_vec()),
            ]),
            unreadable: BTreeSet::new(),
        };
        discover_ssh_hosts(&filesystem, &roots, limits)
    }

    fn host_discovery(managed: &str, user: &str) -> HostDiscovery {
        host_discovery_with_limits(
            managed.as_bytes(),
            user.as_bytes(),
            HostDiscoveryLimits::default(),
        )
    }

    fn unreadable_host_discovery() -> HostDiscovery {
        let roots = HostConfigRoots {
            managed: PathBuf::from("/managed/ssh_config"),
            user: PathBuf::from("/home/test/.ssh/config"),
            home: PathBuf::from("/home/test"),
        };
        let filesystem = MemoryHostConfigFilesystem {
            files: BTreeMap::from([
                (roots.managed.clone(), Vec::new()),
                (roots.user.clone(), b"Host safe-user-host\n".to_vec()),
            ]),
            unreadable: BTreeSet::from([roots.managed.clone()]),
        };
        discover_ssh_hosts(&filesystem, &roots, HostDiscoveryLimits::default())
    }

    struct ScriptedHostDiscoveryProvider {
        discoveries: Mutex<VecDeque<HostDiscovery>>,
        calls: AtomicUsize,
    }

    impl ScriptedHostDiscoveryProvider {
        fn new(discoveries: impl IntoIterator<Item = HostDiscovery>) -> Self {
            Self {
                discoveries: Mutex::new(discoveries.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl HostDiscoveryProvider for ScriptedHostDiscoveryProvider {
        fn discover(&self) -> HostDiscovery {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut discoveries = self.discoveries.lock().unwrap();
            if discoveries.len() > 1 {
                discoveries.pop_front().unwrap()
            } else {
                discoveries.front().cloned().unwrap_or_default()
            }
        }
    }

    struct SshHostPickerHarness {
        picker: Entity<SshHostPicker>,
        prior_focus: FocusHandle,
        events: Rc<RefCell<Vec<SshHostPickerEvent>>>,
    }

    impl SshHostPickerHarness {
        fn new(
            provider: Arc<dyn HostDiscoveryProvider>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let picker = cx.new(|cx| SshHostPicker::new(provider, window, cx));
            let events = Rc::new(RefCell::new(Vec::new()));
            let captured_events = Rc::clone(&events);
            cx.subscribe(&picker, move |_, _, event, _| {
                captured_events.borrow_mut().push(event.clone());
            })
            .detach();
            Self {
                picker,
                prior_focus: cx.focus_handle(),
                events,
            }
        }
    }

    impl Render for SshHostPickerHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.prior_focus)
                .child(self.picker.clone())
        }
    }

    fn host_picker<'a>(
        provider: Arc<ScriptedHostDiscoveryProvider>,
        cx: &'a mut TestAppContext,
    ) -> HostPickerWindow<'a> {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let injected_provider: Arc<dyn HostDiscoveryProvider> = provider;
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            SshHostPickerHarness::new(injected_provider, window, cx)
        });
        let (picker, events) = harness.read_with(cx, |harness, _| {
            (harness.picker.clone(), Rc::clone(&harness.events))
        });
        cx.update(|window, cx| {
            window.activate_window();
            let focus = harness.read(cx).prior_focus.clone();
            focus.focus(window, cx);
            picker.update(cx, |picker, cx| {
                picker.open(window, cx);
            });
        });
        cx.run_until_parked();
        (harness, picker, events, cx)
    }

    fn set_query(picker: &Entity<SshHostPicker>, query: &str, cx: &mut VisualTestContext) {
        picker.update(cx, |picker, cx| {
            picker
                .palette()
                .update(cx, |palette, cx| palette.set_query(query, cx));
        });
        cx.run_until_parked();
    }

    fn selected_item(
        picker: &Entity<SshHostPicker>,
        cx: &mut VisualTestContext,
    ) -> Option<SshHostPickerItemId> {
        picker.read_with(cx, |picker, cx| {
            picker.palette().read(cx).selected_item_id().cloned()
        })
    }

    #[test]
    fn configured_aliases_should_filter_by_case_insensitive_prefix() {
        let rows = host_rows_for_query(
            &host_discovery(
                "Host work\n  HostName work.example\nHost staging\n  HostName staging.example\n",
                "Host personal\n  HostName personal.example\n",
            ),
            "WO",
        );

        assert_eq!(
            rows.iter().map(|row| row.label()).collect::<Vec<_>>(),
            vec!["work"]
        );
    }

    #[test]
    fn configured_destination_subtitles_should_be_searchable() {
        let rows = host_rows_for_query(
            &host_discovery(
                "Host work\n  HostName build.example\n  User deploy\n  Port 2222\n",
                "Host personal\n  HostName personal.example\n",
            ),
            "build.example",
        );

        assert_eq!(
            rows.iter().map(|row| row.label()).collect::<Vec<_>>(),
            vec!["work"]
        );
        assert_eq!(
            rows[0].subtitle_matched_indices,
            (7..20).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multi_part_user_override_should_use_the_longest_alias_suffix() {
        let rows = host_rows_for_query(
            &host_discovery("Host orb\nHost fedora@orb\n", ""),
            "root@fedora@orb",
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label(), "root@fedora@orb");
        assert_eq!(rows[0].subtitle, "Connect as root through fedora@orb");
        assert!(matches!(
            &rows[0].id,
            SshHostPickerItemId::UserOverride { alias, .. }
                if alias.as_str() == "fedora@orb"
        ));
    }

    #[test]
    fn configured_rows_should_describe_destinations_or_their_config_source() {
        let rows = host_rows_for_query(
            &host_discovery(
                "Host work\n  HostName build.example\n  User deploy\n  Port 2222\n",
                "Host personal\n",
            ),
            "",
        );

        assert_eq!(
            rows.iter()
                .map(|row| (row.label(), row.subtitle.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("personal", "/home/test/.ssh/config"),
                ("work", "deploy@build.example:2222"),
            ]
        );
    }

    #[test]
    fn read_issues_should_produce_an_unreadable_diagnostic() {
        assert_eq!(
            issue_class(HostConfigIssueKind::Read),
            HostDiscoveryIssueClass::Unreadable
        );
    }

    #[gpui::test]
    fn unreadable_config_should_warn_without_discarding_safe_hosts(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            unreadable_host_discovery(),
        ]));
        let (_, picker, _, cx) = host_picker(provider, cx);

        let diagnostic = picker.read_with(cx, |picker, _| {
            HostDiscoveryDiagnostic::for_discovery(&picker.discovery).unwrap()
        });
        assert!(diagnostic.description.contains("file permissions"));
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.rows[0].label().to_owned()),
            "safe-user-host"
        );
        assert!(cx.debug_bounds(DISCOVERY_WARNING_SELECTOR).is_some());
    }

    #[test]
    fn invalid_text_and_directives_should_produce_a_malformed_diagnostic() {
        assert_eq!(
            [
                issue_class(HostConfigIssueKind::InvalidUtf8),
                issue_class(HostConfigIssueKind::MalformedLine),
            ],
            [
                HostDiscoveryIssueClass::Malformed,
                HostDiscoveryIssueClass::Malformed,
            ]
        );
    }

    #[test]
    fn include_cycles_should_produce_a_cycle_diagnostic() {
        assert_eq!(
            issue_class(HostConfigIssueKind::IncludeCycle),
            HostDiscoveryIssueClass::IncludeCycle
        );
    }

    #[test]
    fn every_scanner_bound_should_produce_a_safety_limit_diagnostic() {
        let classes = [
            HostConfigIssueKind::IncludeDepthLimit,
            HostConfigIssueKind::FileLimit,
            HostConfigIssueKind::TotalByteLimit,
            HostConfigIssueKind::FileByteLimit,
            HostConfigIssueKind::GlobLimit,
            HostConfigIssueKind::ResultLimit,
            HostConfigIssueKind::DeclarationLimit,
            HostConfigIssueKind::TokenLimit,
        ]
        .map(issue_class);

        assert_eq!([HostDiscoveryIssueClass::SafetyLimit; 8], classes);
    }

    #[gpui::test]
    fn partial_discovery_should_keep_safe_hosts_and_show_a_non_selectable_warning(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host \"unterminated\nHost work\n",
            "Host personal\n",
        )]));
        let (_, picker, _, cx) = host_picker(provider, cx);

        assert!(cx.debug_bounds(DISCOVERY_WARNING_SELECTOR).is_some());
        assert_eq!(
            picker.read_with(cx, |picker, _| picker
                .rows
                .iter()
                .map(|row| row.label().to_owned())
                .collect::<Vec<_>>()),
            ["personal".to_owned(), "work".to_owned()]
        );
        assert!(matches!(
            selected_item(&picker, cx),
            Some(SshHostPickerItemId::Configured(_))
        ));
        let diagnostic = picker.read_with(cx, |picker, _| {
            HostDiscoveryDiagnostic::for_discovery(&picker.discovery).unwrap()
        });
        assert!(!diagnostic.description.contains("/managed"));
        assert!(!diagnostic.description.contains("unterminated"));
    }

    #[test]
    fn truncated_discovery_warning_should_be_fixed_and_bounded() {
        let discovery = host_discovery_with_limits(
            b"Host first second third\n",
            b"",
            HostDiscoveryLimits {
                results: 1,
                ..HostDiscoveryLimits::default()
            },
        );

        let diagnostic = HostDiscoveryDiagnostic::for_discovery(&discovery).unwrap();

        assert!(diagnostic.description.contains("narrow Include patterns"));
        assert!(diagnostic.description.len() <= MAXIMUM_DISCOVERY_WARNING_BYTES);
    }

    #[gpui::test]
    fn genuine_empty_discovery_should_show_the_configured_empty_state_without_a_warning(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            HostDiscovery::default(),
        ]));
        let (_, picker, _, cx) = host_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| empty_state(&picker.discovery, "")
                .title()
                .to_owned()),
            "No SSH hosts configured"
        );
        assert!(cx.debug_bounds("command-palette-empty").is_some());
        assert!(cx.debug_bounds(EMPTY_ADD_HOST_SELECTOR).is_some());
        assert!(cx.debug_bounds(DISCOVERY_WARNING_SELECTOR).is_none());
        assert!(selected_item(&picker, cx).is_none());
    }

    #[gpui::test]
    fn partial_empty_discovery_should_explain_that_no_safe_hosts_were_found(
        cx: &mut TestAppContext,
    ) {
        let discovery = host_discovery("Host \"unterminated\n", "");
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([discovery]));
        let (_, picker, _, cx) = host_picker(provider, cx);

        let diagnostic = picker.read_with(cx, |picker, _| {
            HostDiscoveryDiagnostic::for_discovery(&picker.discovery).unwrap()
        });
        assert_eq!(diagnostic.title, "No safe SSH hosts were found");
        assert!(cx.debug_bounds(DISCOVERY_WARNING_SELECTOR).is_some());
        assert!(selected_item(&picker, cx).is_none());
    }

    #[gpui::test]
    fn refresh_should_update_then_clear_discovery_diagnostics(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            host_discovery("Host \"unterminated\nHost work\n", ""),
            host_discovery("Include /managed/ssh_config\nHost work\n", ""),
            host_discovery("Host work\n", ""),
        ]));
        let (_, picker, _, cx) = host_picker(Arc::clone(&provider), cx);
        assert!(
            picker
                .read_with(cx, |picker, _| HostDiscoveryDiagnostic::for_discovery(
                    &picker.discovery
                ))
                .unwrap()
                .description
                .contains("malformed")
        );

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.refresh(window, cx));
        });
        cx.run_until_parked();
        assert!(
            picker
                .read_with(cx, |picker, _| HostDiscoveryDiagnostic::for_discovery(
                    &picker.discovery
                ))
                .unwrap()
                .description
                .contains("recursive Include")
        );

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.refresh(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(provider.calls(), 3);
        assert!(picker.read_with(cx, |picker, _| {
            HostDiscoveryDiagnostic::for_discovery(&picker.discovery).is_none()
        }));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds(DISCOVERY_WARNING_SELECTOR).is_none());
    }

    #[gpui::test]
    fn stale_refresh_result_should_not_restore_an_old_diagnostic(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n",
            "",
        )]));
        let (_, picker, _, cx) = host_picker(provider, cx);
        let stale_generation =
            picker.read_with(cx, |picker, _| picker.refresh_generation.wrapping_sub(1));
        let stale_discovery = host_discovery("Host \"unterminated\n", "");

        let applied = picker.update(cx, |picker, cx| {
            picker.apply_discovery(stale_generation, stale_discovery, cx)
        });

        assert!(!applied);
        assert!(picker.read_with(cx, |picker, _| {
            HostDiscoveryDiagnostic::for_discovery(&picker.discovery).is_none()
        }));
        assert!(cx.debug_bounds(DISCOVERY_WARNING_SELECTOR).is_none());
    }

    #[gpui::test]
    fn the_header_add_action_should_stay_available_and_emit_an_add_request(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n  HostName work.example\n",
            "",
        )]));
        let (_, picker, events, cx) = host_picker(provider, cx);
        events.borrow_mut().clear();

        set_query(&picker, "work", cx);
        assert!(cx.debug_bounds(EMPTY_ADD_HOST_SELECTOR).is_none());
        let add = cx.debug_bounds(HEADER_ADD_HOST_SELECTOR).unwrap();
        cx.simulate_click(add.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            [SshHostPickerEvent::RequestAddHost]
        );
        assert!(picker.read_with(cx, |picker, _| picker.is_open()));
    }

    #[gpui::test]
    fn an_unmatched_query_should_offer_add_from_the_empty_state(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n  HostName work.example\n",
            "",
        )]));
        let (_, picker, events, cx) = host_picker(provider, cx);
        events.borrow_mut().clear();

        set_query(&picker, "new-host", cx);
        assert!(
            events.borrow().is_empty(),
            "typing emitted a host operation"
        );
        assert!(cx.debug_bounds(HEADER_ADD_HOST_SELECTOR).is_some());
        let add = cx.debug_bounds(EMPTY_ADD_HOST_SELECTOR).unwrap();
        cx.simulate_click(add.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [SshHostPickerEvent::RequestAddHost]
        );

        events.borrow_mut().clear();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [SshHostPickerEvent::RequestAddHost]
        );
        assert!(picker.read_with(cx, |picker, _| picker.is_open()));
    }

    #[gpui::test]
    fn row_activation_should_emit_the_destination_without_closing(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n  HostName work.example\n",
            "",
        )]));
        let (_, picker, events, cx) = host_picker(provider, cx);
        events.borrow_mut().clear();

        set_query(&picker, "work", cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            [SshHostPickerEvent::SelectDestination(
                SshDestination::new("work".to_owned()).unwrap()
            )]
        );
        assert!(picker.read_with(cx, |picker, _| picker.is_open()));
    }

    #[gpui::test]
    fn refresh_should_preserve_query_and_stable_selection(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            host_discovery("Host staging\nHost work\n", ""),
            host_discovery("Host stack\nHost staging\nHost work\n", ""),
        ]));
        let (_, picker, _, cx) = host_picker(Arc::clone(&provider), cx);
        set_query(&picker, "st", cx);
        assert_eq!(
            selected_item(&picker, cx),
            Some(SshHostPickerItemId::Configured(
                SshHostAlias::new("staging".to_owned()).unwrap()
            ))
        );

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.refresh(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(provider.calls(), 2);
        assert_eq!(
            picker.read_with(cx, |picker, cx| (
                picker.palette().read(cx).query().to_owned(),
                picker.palette().read(cx).selected_item_id().cloned(),
            )),
            (
                "st".to_owned(),
                Some(SshHostPickerItemId::Configured(
                    SshHostAlias::new("staging".to_owned()).unwrap()
                )),
            )
        );
    }

    #[gpui::test]
    fn query_change_should_select_the_new_first_fuzzy_result(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host projects\nHost remote-operation\n",
            "",
        )]));
        let (_, picker, _, cx) = host_picker(provider, cx);
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(
            selected_item(&picker, cx),
            Some(SshHostPickerItemId::Configured(
                SshHostAlias::new("remote-operation".to_owned()).unwrap()
            ))
        );

        set_query(&picker, "ro", cx);

        assert_eq!(
            selected_item(&picker, cx),
            Some(SshHostPickerItemId::Configured(
                SshHostAlias::new("projects".to_owned()).unwrap()
            ))
        );
    }

    #[gpui::test]
    fn escape_should_emit_typed_close_restore_focus_and_reopen_with_fresh_discovery(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            host_discovery("Host first\n", ""),
            host_discovery("Host second\n", ""),
        ]));
        let (harness, picker, events, cx) = host_picker(Arc::clone(&provider), cx);
        set_query(&picker, "f", cx);
        assert!(cx.update(|window, cx| {
            picker
                .read(cx)
                .palette()
                .read(cx)
                .editor_is_focused(window, cx)
        }));

        events.borrow_mut().clear();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(events.borrow().contains(&SshHostPickerEvent::Lifecycle(
            SshHostPickerLifecycleEvent::Closed(CommandPaletteCloseReason::Escape)
        )));
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.open(window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(provider.calls(), 2);
        assert_eq!(
            picker.read_with(cx, |picker, cx| picker
                .palette()
                .read(cx)
                .query()
                .to_owned()),
            "f"
        );
    }

    #[gpui::test]
    fn replacement_transfer_preserves_focus_chain_query_and_selection(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            host_discovery("Host staging\nHost work\n", ""),
            host_discovery("Host staging\nHost work\n", ""),
        ]));
        let (harness, picker, events, cx) = host_picker(provider, cx);
        set_query(&picker, "st", cx);
        let selected = selected_item(&picker, cx);
        events.borrow_mut().clear();

        cx.update(|window, cx| {
            let replacement = picker
                .update(cx, |picker, cx| picker.dismiss_for_replacement(window, cx))
                .expect("open host picker should transfer its original focus owner");
            assert!(!harness.read(cx).prior_focus.is_focused(window));
            assert!(picker.update(cx, |picker, cx| {
                picker.open_replacing(replacement, window, cx)
            }));
        });
        cx.run_until_parked();

        assert!(events.borrow().contains(&SshHostPickerEvent::Lifecycle(
            SshHostPickerLifecycleEvent::Closed(CommandPaletteCloseReason::Replaced)
        )));
        assert_eq!(
            picker.read_with(cx, |picker, cx| (
                picker.palette().read(cx).query().to_owned(),
                picker.palette().read(cx).selected_item_id().cloned(),
            )),
            ("st".to_owned(), selected)
        );
        assert!(cx.update(|window, cx| {
            picker
                .read(cx)
                .palette()
                .read(cx)
                .editor_is_focused(window, cx)
        }));
    }
}
