#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Host Picker lands before its Workspace Manager integration"
    )
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Render, SharedString, Window};
use spaceterm_ui::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteActivationPolicy,
    CommandPaletteCloseReason, CommandPaletteEvent, CommandPaletteItem,
    CommandPaletteLifecycleEvent, CommandPaletteMatching, CommandPaletteReplacementFocus,
    MenuEntry,
};

use crate::domain::SshDestination;
use crate::ssh::destination::{
    DestinationQueryResolution, SshHostAlias, resolve_destination_query,
};
use crate::ssh::host_config::{
    DiscoveredSshHost, HostConfigIssueKind, HostConfigSource, HostDiscovery,
};

const MAXIMUM_DESTINATION_BYTES: usize = 1024;
const ADD_HOST_ACTION: &str = "ssh-host-picker-add";
const EDIT_HOST_ACTION: &str = "ssh-host-picker-edit";
const DELETE_HOST_ACTION: &str = "ssh-host-picker-delete";
const DISCOVERY_WARNING_SELECTOR: &str = "ssh-host-picker-discovery-warning";
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
            .trailing(CommandPaletteAccessory::Status("Action needed".into()))
            .debug_selector(DISCOVERY_WARNING_SELECTOR)
    }
}

fn no_results_text(discovery: &HostDiscovery, query: &str) -> &'static str {
    if query.is_empty() && discovery.hosts.is_empty() && discovery.issues.is_empty() {
        "No SSH hosts configured"
    } else {
        "No matching SSH hosts"
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostPickerRow {
    id: SshHostPickerItemId,
    destination: SshDestination,
    label: String,
    subtitle: String,
    managed: bool,
    synthetic: bool,
}

impl HostPickerRow {
    fn label(&self) -> &str {
        &self.label
    }

    fn into_palette_item(self) -> CommandPaletteItem<SshHostPickerItemId> {
        let status = if self.synthetic {
            "User override"
        } else if self.managed {
            "Managed"
        } else {
            "Read-only"
        };
        CommandPaletteItem::new(self.id, self.label)
            .description(self.subtitle)
            .trailing(CommandPaletteAccessory::Status(status.into()))
            .debug_selector(format!("ssh-host-picker-row-{}", self.destination.as_str()))
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
    let folded_query = query.to_lowercase();
    let mut rows = hosts
        .iter()
        .filter(|host| {
            query.is_empty()
                || host
                    .alias()
                    .as_str()
                    .to_lowercase()
                    .starts_with(&folded_query)
        })
        .filter_map(configured_host_row)
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
                managed: false,
                synthetic: true,
            },
        );
    }
    rows
}

fn configured_host_row(host: &&DiscoveredSshHost) -> Option<HostPickerRow> {
    let destination = SshDestination::new(host.alias().as_str().to_owned()).ok()?;
    Some(HostPickerRow {
        id: SshHostPickerItemId::Configured(host.alias().clone()),
        destination,
        label: host.alias().as_str().to_owned(),
        subtitle: host.subtitle(),
        managed: host
            .provenance()
            .is_some_and(|provenance| provenance.source() == HostConfigSource::Managed),
        synthetic: false,
    })
}

fn add_destination_for_query(discovery: &HostDiscovery, query: &str) -> Option<SshDestination> {
    let aliases = discovery
        .hosts
        .iter()
        .map(|host| host.alias().clone())
        .collect::<Vec<_>>();
    match resolve_destination_query(query, &aliases, MAXIMUM_DESTINATION_BYTES).ok()? {
        DestinationQueryResolution::AddHost { destination } => Some(destination),
        DestinationQueryResolution::Configured { .. } => None,
    }
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
    RequestAddHost(SshDestination),
    RequestEditHost(SshHostAlias),
    RequestDeleteHost(SshHostAlias),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostPickerFooterAction {
    Add(SshDestination),
    Edit { alias: SshHostAlias, disabled: bool },
    Delete { alias: SshHostAlias, disabled: bool },
}

pub(super) struct SshHostPicker {
    palette: Entity<CommandPalette<SshHostPickerItemId>>,
    discovery_provider: Arc<dyn HostDiscoveryProvider>,
    host_in_active_use: Arc<dyn Fn(&SshHostAlias) -> bool + Send + Sync>,
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
        host_in_active_use: Arc<dyn Fn(&SshHostAlias) -> bool + Send + Sync>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new("Connect to SSH Host", Vec::new(), window, cx);
            palette.set_matching(CommandPaletteMatching::Caller, cx);
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette.set_no_results_text("No matching SSH hosts", cx);
            palette.set_actions_menu_label("Host Actions", cx);
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
            picker.observe_selection(selected, cx);
        })
        .detach();

        Self {
            palette,
            discovery_provider,
            host_in_active_use,
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
                self.palette.update(cx, |palette, cx| {
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
                self.retained_query = query.text().to_owned();
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
            CommandPaletteEvent::MenuAction(action) => self.activate_footer_action(action, cx),
            CommandPaletteEvent::HeaderAction(_) | CommandPaletteEvent::Confirmed => {}
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
            palette.set_no_results_text(no_results_text(&self.discovery, &self.retained_query), cx);
            palette.set_preferred_item(self.retained_selection.clone(), cx);
            palette.set_items(items, cx);
        });
        self.sync_footer(cx);
    }

    fn capture_selection(&mut self, cx: &gpui::App) {
        if let Some(selected) = self.palette.read(cx).selected_item_id().cloned() {
            self.retained_selection = Some(selected);
        }
    }

    fn observe_selection(&mut self, selected: Option<SshHostPickerItemId>, cx: &mut Context<Self>) {
        if self.observed_selection == selected {
            return;
        }
        self.observed_selection = selected.clone();
        if selected.is_some() {
            self.retained_selection = selected;
        }
        self.sync_footer(cx);
    }

    fn footer_actions(&self, cx: &gpui::App) -> Vec<HostPickerFooterAction> {
        let mut actions = Vec::new();
        if let Some(destination) = add_destination_for_query(&self.discovery, &self.retained_query)
        {
            actions.push(HostPickerFooterAction::Add(destination));
        }
        let Some(SshHostPickerItemId::Configured(alias)) = self.palette.read(cx).selected_item_id()
        else {
            return actions;
        };
        let managed = self
            .rows
            .iter()
            .any(|row| row.managed && row.id == SshHostPickerItemId::Configured(alias.clone()));
        if managed {
            let disabled = (self.host_in_active_use)(alias);
            actions.push(HostPickerFooterAction::Edit {
                alias: alias.clone(),
                disabled,
            });
            actions.push(HostPickerFooterAction::Delete {
                alias: alias.clone(),
                disabled,
            });
        }
        actions
    }

    fn sync_footer(&mut self, cx: &mut Context<Self>) {
        let entries = self
            .footer_actions(cx)
            .into_iter()
            .map(|action| match action {
                HostPickerFooterAction::Add(_) => {
                    MenuEntry::action("Add SSH Host", ADD_HOST_ACTION.into())
                        .debug_selector(ADD_HOST_ACTION)
                }
                HostPickerFooterAction::Edit { disabled, .. } => {
                    MenuEntry::action("Edit SSH Host", EDIT_HOST_ACTION.into())
                        .disabled(disabled)
                        .debug_selector(EDIT_HOST_ACTION)
                }
                HostPickerFooterAction::Delete { disabled, .. } => {
                    MenuEntry::action("Delete SSH Host", DELETE_HOST_ACTION.into())
                        .disabled(disabled)
                        .destructive(true)
                        .debug_selector(DELETE_HOST_ACTION)
                }
            })
            .collect();
        self.palette
            .update(cx, |palette, cx| palette.set_actions_menu(entries, cx));
    }

    fn activate_footer_action(&mut self, action: &SharedString, cx: &mut Context<Self>) {
        match action.as_ref() {
            ADD_HOST_ACTION => {
                if let Some(destination) =
                    add_destination_for_query(&self.discovery, &self.retained_query)
                {
                    cx.emit(SshHostPickerEvent::RequestAddHost(destination));
                    cx.notify();
                }
            }
            EDIT_HOST_ACTION | DELETE_HOST_ACTION => {
                let Some(SshHostPickerItemId::Configured(alias)) =
                    self.palette.read(cx).selected_item_id().cloned()
                else {
                    return;
                };
                let managed = self.rows.iter().any(|row| {
                    row.managed && row.id == SshHostPickerItemId::Configured(alias.clone())
                });
                if !managed || (self.host_in_active_use)(&alias) {
                    return;
                }
                if action.as_ref() == EDIT_HOST_ACTION {
                    cx.emit(SshHostPickerEvent::RequestEditHost(alias));
                } else {
                    cx.emit(SshHostPickerEvent::RequestDeleteHost(alias));
                }
                cx.notify();
            }
            _ => {}
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
    use std::io;
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
        HostConfigFilesystem, HostConfigRoots, HostDiscoveryLimits, discover_ssh_hosts,
    };

    struct MemoryHostConfigFilesystem {
        files: BTreeMap<PathBuf, Vec<u8>>,
        unreadable: BTreeSet<PathBuf>,
    }

    impl HostConfigFilesystem for MemoryHostConfigFilesystem {
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }

        fn read_file_limited(&self, path: &Path, maximum_bytes: usize) -> io::Result<Vec<u8>> {
            if self.unreadable.contains(path) {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let contents = self
                .files
                .get(path)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            Ok(contents
                .iter()
                .copied()
                .take(maximum_bytes.saturating_add(1))
                .collect())
        }

        fn read_directory_limited(&self, _: &Path, _: usize) -> io::Result<Vec<PathBuf>> {
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
            host_in_active_use: Arc<dyn Fn(&SshHostAlias) -> bool + Send + Sync>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let picker = cx.new(|cx| SshHostPicker::new(provider, host_in_active_use, window, cx));
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
        host_in_active_use: impl Fn(&SshHostAlias) -> bool + Send + Sync + 'static,
        cx: &'a mut TestAppContext,
    ) -> HostPickerWindow<'a> {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let injected_provider: Arc<dyn HostDiscoveryProvider> = provider;
        let active_use = Arc::new(host_in_active_use);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            SshHostPickerHarness::new(injected_provider, active_use, window, cx)
        });
        let (picker, events) = harness.read_with(cx, |harness, _| {
            (harness.picker.clone(), Rc::clone(&harness.events))
        });
        cx.update(|window, cx| {
            window.activate_window();
            harness.read(cx).prior_focus.focus(window);
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
    fn configured_rows_should_distinguish_managed_direct_and_read_only_provenance() {
        let rows = host_rows_for_query(
            &host_discovery(
                "Host work\n  HostName build.example\n  User deploy\n  Port 2222\n",
                "Host personal\n",
            ),
            "",
        );

        assert_eq!(
            rows.iter()
                .map(|row| (row.label(), row.subtitle.as_str(), row.managed))
                .collect::<Vec<_>>(),
            vec![
                ("personal", "/home/test/.ssh/config", false),
                ("work", "deploy@build.example:2222", true),
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
        let (_, picker, _, cx) = host_picker(provider, |_| false, cx);

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
        let (_, picker, _, cx) = host_picker(provider, |_| false, cx);

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
        let (_, picker, _, cx) = host_picker(provider, |_| false, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| no_results_text(&picker.discovery, "")),
            "No SSH hosts configured"
        );
        assert!(cx.debug_bounds(DISCOVERY_WARNING_SELECTOR).is_none());
        assert!(selected_item(&picker, cx).is_none());
    }

    #[gpui::test]
    fn partial_empty_discovery_should_explain_that_no_safe_hosts_were_found(
        cx: &mut TestAppContext,
    ) {
        let discovery = host_discovery("Host \"unterminated\n", "");
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([discovery]));
        let (_, picker, _, cx) = host_picker(provider, |_| false, cx);

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
        let (_, picker, _, cx) = host_picker(Arc::clone(&provider), |_| false, cx);
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
        let (_, picker, _, cx) = host_picker(provider, |_| false, cx);
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
    fn typing_should_only_filter_and_offer_safe_raw_add(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n  HostName work.example\n",
            "",
        )]));
        let (_, picker, events, cx) = host_picker(provider, |_| false, cx);
        events.borrow_mut().clear();

        set_query(&picker, "new-host", cx);

        assert!(cx.debug_bounds(ADD_HOST_ACTION).is_some());
        assert!(
            events.borrow().is_empty(),
            "typing emitted a host operation"
        );
        set_query(&picker, "work", cx);
        assert!(cx.debug_bounds(ADD_HOST_ACTION).is_none());
        set_query(&picker, "root@work", cx);
        assert!(
            picker.read_with(cx, |picker, cx| picker.footer_actions(cx).is_empty()),
            "a user override was offered as a raw host"
        );
    }

    #[gpui::test]
    fn raw_add_and_row_activation_should_emit_typed_events_without_closing(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n  HostName work.example\n",
            "",
        )]));
        let (_, picker, events, cx) = host_picker(provider, |_| false, cx);
        events.borrow_mut().clear();
        set_query(&picker, "new-host", cx);
        let add = cx.debug_bounds(ADD_HOST_ACTION).unwrap();
        cx.simulate_click(add.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(
            events
                .borrow()
                .contains(&SshHostPickerEvent::RequestAddHost(
                    SshDestination::new("new-host".to_owned()).unwrap()
                ))
        );

        events.borrow_mut().clear();
        set_query(&picker, "work", cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(
            events
                .borrow()
                .contains(&SshHostPickerEvent::SelectDestination(
                    SshDestination::new("work".to_owned()).unwrap()
                ))
        );
        assert!(picker.read_with(cx, |picker, _| picker.is_open()));
    }

    #[gpui::test]
    fn footer_should_offer_managed_actions_only_and_disable_them_while_active(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n  HostName work.example\n",
            "Host personal\n",
        )]));
        let (_, picker, _, cx) = host_picker(provider, |alias| alias.as_str() == "work", cx);

        set_query(&picker, "work", cx);
        assert_eq!(
            picker.read_with(cx, |picker, cx| picker.footer_actions(cx)),
            vec![
                HostPickerFooterAction::Edit {
                    alias: SshHostAlias::new("work".to_owned()).unwrap(),
                    disabled: true,
                },
                HostPickerFooterAction::Delete {
                    alias: SshHostAlias::new("work".to_owned()).unwrap(),
                    disabled: true,
                },
            ]
        );

        set_query(&picker, "personal", cx);
        assert!(picker.read_with(cx, |picker, cx| picker.footer_actions(cx).is_empty()));
    }

    #[gpui::test]
    fn managed_footer_actions_should_emit_typed_edit_and_delete_requests(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([host_discovery(
            "Host work\n  HostName work.example\n",
            "",
        )]));
        let (_, picker, events, cx) = host_picker(provider, |_| false, cx);
        set_query(&picker, "work", cx);
        events.borrow_mut().clear();

        picker.update(cx, |picker, cx| {
            picker.activate_footer_action(&EDIT_HOST_ACTION.into(), cx);
            picker.activate_footer_action(&DELETE_HOST_ACTION.into(), cx);
        });

        let alias = SshHostAlias::new("work".to_owned()).unwrap();
        assert_eq!(
            events.borrow().as_slice(),
            [
                SshHostPickerEvent::RequestEditHost(alias.clone()),
                SshHostPickerEvent::RequestDeleteHost(alias),
            ]
        );
    }

    #[gpui::test]
    fn refresh_should_preserve_query_and_stable_selection(cx: &mut TestAppContext) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            host_discovery("Host staging\nHost work\n", ""),
            host_discovery("Host stack\nHost staging\nHost work\n", ""),
        ]));
        let (_, picker, _, cx) = host_picker(Arc::clone(&provider), |_| false, cx);
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
    fn escape_should_emit_typed_close_restore_focus_and_reopen_with_fresh_discovery(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(ScriptedHostDiscoveryProvider::new([
            host_discovery("Host first\n", ""),
            host_discovery("Host second\n", ""),
        ]));
        let (harness, picker, events, cx) = host_picker(Arc::clone(&provider), |_| false, cx);
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
        let (harness, picker, events, cx) = host_picker(provider, |_| false, cx);
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
