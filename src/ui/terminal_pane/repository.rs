//! A Pane's Repository Status Interest: what it reports to the store and what it presents.

use gpui::{App, Context, Entity, Subscription};

use super::{PaneTerminalState, TerminalFailure, TerminalPane, TerminalPaneEvent};
use crate::repository_status::scheduler::{Interest, InterestId, SourceDirectory};
use crate::repository_status::{RemoteMachineKey, RepositoryMachine, RepositoryView};
use crate::terminal::metadata::{CurrentDirectory, MetadataFreshness, TerminalMetadataContext};
use crate::ui::repository_status_store::{
    InstalledRepositoryStatus, RepositoryStatusStore, RepositoryViewsChanged,
};

#[derive(Default)]
pub(super) struct PaneRepositoryStatus {
    interest: Option<RegisteredInterest>,
    view: RepositoryView,
}

struct RegisteredInterest {
    store: Entity<RepositoryStatusStore>,
    id: InterestId,
    reported: PaneRepositoryFacts,
    _changes: Subscription,
}

/// Everything a Pane tells the store about its Interest.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PaneRepositoryFacts {
    machine: RepositoryMachine,
    directory: SourceDirectory,
    finished_commands: u64,
    visible: bool,
    available: bool,
    stopped: bool,
}

impl TerminalPane {
    /// What this Pane's Repository Status presents.
    pub(crate) fn repository_view(&self) -> &RepositoryView {
        &self.repository_status.view
    }

    /// Reports any changed fact to the store, registering on the first known directory.
    pub(super) fn sync_repository_status(&mut self, cx: &mut Context<Self>) {
        let Some(facts) = self.repository_facts() else {
            return;
        };
        let Some(interest) = &mut self.repository_status.interest else {
            self.register_repository_interest(facts, cx);
            return;
        };
        if interest.reported.machine != facts.machine {
            self.release_repository_status(cx);
            self.register_repository_interest(facts, cx);
            return;
        }
        let previous = std::mem::replace(&mut interest.reported, facts.clone());
        let id = interest.id;
        interest.store.update(cx, |store, cx| {
            // Availability and stopping first, so a new directory never reads over a down
            // connection or for an exited Pane.
            if previous.available != facts.available {
                store.set_available(id, facts.available, cx);
            }
            if previous.stopped != facts.stopped {
                store.set_stopped(id, facts.stopped, cx);
            }
            if previous.directory != facts.directory {
                store.set_source(id, facts.directory.clone(), cx);
            }
            if previous.finished_commands != facts.finished_commands {
                store.command_finished(id, facts.finished_commands, cx);
            }
            if previous.visible != facts.visible {
                store.set_visible(id, facts.visible, cx);
            }
        });
    }

    pub(super) fn release_repository_status(&mut self, cx: &mut App) {
        if let Some(interest) = self.repository_status.interest.take() {
            interest
                .store
                .update(cx, |store, cx| store.unregister(interest.id, cx));
        }
    }

    fn register_repository_interest(&mut self, facts: PaneRepositoryFacts, cx: &mut Context<Self>) {
        let Some(store) = InstalledRepositoryStatus::store(cx) else {
            return;
        };
        let id = store.update(cx, |store, cx| {
            let id = store.register(
                Interest {
                    machine: facts.machine.clone(),
                    directory: facts.directory.clone(),
                    visible: facts.visible,
                    available: facts.available,
                    finished_commands: facts.finished_commands,
                },
                cx,
            );
            if facts.stopped {
                store.set_stopped(id, true, cx);
            }
            id
        });
        let changes = cx.subscribe(&store, |pane, _, event: &RepositoryViewsChanged, cx| {
            if pane
                .repository_status
                .interest
                .as_ref()
                .is_some_and(|interest| event.contains(interest.id))
            {
                pane.refresh_repository_view(cx);
            }
        });
        self.repository_status.interest = Some(RegisteredInterest {
            store,
            id,
            reported: facts,
            _changes: changes,
        });
        self.refresh_repository_view(cx);
    }

    fn refresh_repository_view(&mut self, cx: &mut Context<Self>) {
        let view = self
            .repository_status
            .interest
            .as_ref()
            .map(|interest| interest.store.read(cx).view(interest.id))
            .unwrap_or_default();
        if view != self.repository_status.view {
            self.repository_status.view = view;
            cx.emit(TerminalPaneEvent::CaptionChanged);
            cx.notify();
        }
    }

    /// The facts to report, or `None` before the Pane knows a directory.
    fn repository_facts(&self) -> Option<PaneRepositoryFacts> {
        let metadata = self.terminal_session.metadata.as_deref()?;
        let stale = metadata.freshness == MetadataFreshness::Stale;
        // Stale facts keep the last directory: the Interest stops and presents it as last known.
        let directory = metadata
            .context
            .current_directory(&metadata.repository_directory)?;
        let (machine, directory) = match (&metadata.context, directory) {
            (TerminalMetadataContext::Local { .. }, CurrentDirectory::Local(path)) => {
                (RepositoryMachine::Local, SourceDirectory::Local(path))
            }
            (TerminalMetadataContext::Remote(remote), CurrentDirectory::Remote(directory)) => (
                RepositoryMachine::Remote(RemoteMachineKey::new(remote.destination().as_str())),
                SourceDirectory::Remote(directory.as_str().into()),
            ),
            _ => return None,
        };
        let focus = self.product_focus;
        let exited = matches!(self.pane_state, PaneTerminalState::Exited(_))
            || self
                .pane_state
                .failure()
                .is_some_and(TerminalFailure::is_fatal);
        Some(PaneRepositoryFacts {
            machine,
            directory,
            finished_commands: metadata.finished_commands,
            visible: focus.active_workspace && focus.active_tab && focus.pane_visible,
            available: !self.terminal_session.remote_input_blocked,
            stopped: stale || exited,
        })
    }
}
