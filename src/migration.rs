//! The desktop's two data roots and their Data Generations.
//!
//! Desktop config holds the Settings Document, whose `schema_version` repeats the root's
//! generation so an exported file carries it into an import. Desktop state holds Saved
//! Connections, presentation context and update history in the desktop's own directories, and
//! the desktop's Client View State, Workspace order and client records under SpaceTerm Core's
//! `clients/desktop*`.
use std::path::PathBuf;
use std::sync::Arc;

use crate::platform::app_directories::AppDirectories;
use spaceterm_connections::migration::StoreCredentials;
use spaceterm_core::migration::{
    self, DataKind, DirectoryStore, FileRule, Mount, Outcome, Refusal, Registry, generations,
};

/// The Settings Document's file.
pub(crate) const SETTINGS: &str = "settings.json";

const CONFIG_RULES: &[FileRule] = &[FileRule {
    name: SETTINGS,
    kind: DataKind::Settings,
    read: |value| {
        serde_json::to_vec(value).is_ok_and(|bytes| crate::settings::parse_settings(&bytes).is_ok())
    },
    stamp: Some("schema_version"),
}];

/// The desktop config root's generations, steps and files.
pub(crate) const CONFIG: Registry = Registry {
    generations: generations::DESKTOP_CONFIG,
    steps: &[],
    files: &[CONFIG_RULES],
};

const STATE_RULES: &[FileRule] = &[
    FileRule {
        name: crate::direct_connection_storage::FILE,
        kind: DataKind::SavedConnections,
        read: spaceterm_connections::records::readable,
        stamp: None,
    },
    FileRule {
        name: crate::core_context::FILE,
        kind: DataKind::View,
        read: crate::core_context::readable,
        stamp: None,
    },
    FileRule {
        name: crate::platform::update_history_file::HISTORY_NAME,
        kind: DataKind::UpdateHistory,
        read: crate::updates::policy::readable_history,
        stamp: None,
    },
];

/// The desktop state root's generations, steps and files.
pub(crate) const STATE: Registry = Registry {
    generations: generations::DESKTOP_STATE,
    steps: &[],
    files: &[STATE_RULES, migration::client_files::RULES],
};

/// The desktop config root: the Settings Document in the config directory `config`, through
/// the desktop's own filesystem authority.
pub(crate) fn config_store(
    filesystem: Arc<dyn crate::platform::secure_filesystem::SecureFilesystem>,
    config: PathBuf,
) -> secure_store::SecureStore {
    secure_store::SecureStore::new(filesystem, config, &[SETTINGS])
}

/// The desktop state root over the desktop's `data` and `state` directories. Its marker, backup
/// and reset areas live in `data`. `clients` is SpaceTerm Core's `clients` directory of the same
/// identity.
pub(crate) fn state_store(data: PathBuf, state: PathBuf, clients: PathBuf) -> DirectoryStore {
    DirectoryStore::new(
        data.clone(),
        vec![
            Mount::files(data).at("data"),
            Mount::files(state).at("state"),
            Mount::tree(clients)
                .at("core")
                .only(|name| name == "desktop" || name.starts_with("desktop-")),
        ],
    )
}

/// The credential stores the desktop's Saved Connections use.
pub(crate) fn credentials(identity: &'static str) -> StoreCredentials {
    StoreCredentials::new(move |kind| {
        let store = spaceterm_connections::native(identity)?;
        if store.kind() == kind {
            Ok(store)
        } else {
            Err(spaceterm_connections::CredentialError::Unavailable)
        }
    })
}

/// How the desktop state root stands when the desktop launches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StateCheck {
    /// Readable. `reset` names the rebuildable data a reset replaced that the person hasn't
    /// acknowledged yet.
    Ready {
        reset: Vec<DataKind>,
    },
    /// A newer SpaceTerm wrote it. Nothing in it changed.
    Newer,
    /// The data of `kinds` resets only after the person confirms.
    ResetRequired {
        reason: migration::ResetReason,
        kinds: Vec<DataKind>,
    },
    Unavailable,
}

/// Brings the desktop state root to this build's generation before anything reads it.
pub(crate) fn check_state(store: &DirectoryStore, identity: &'static str) -> StateCheck {
    match migration::run(&STATE, store, &mut credentials(identity), false) {
        Ok(Outcome::Current | Outcome::Migrated { .. } | Outcome::Restored | Outcome::Reset) => {
            StateCheck::Ready {
                reset: migration::pending_notice(store).unwrap_or_default(),
            }
        }
        Err(Refusal::Newer) => StateCheck::Newer,
        Err(Refusal::Reset { reason, kinds }) => StateCheck::ResetRequired { reason, kinds },
        Err(Refusal::Storage(_)) => StateCheck::Unavailable,
    }
}

/// Resets the desktop state that waits for a reset, after the person confirmed it.
pub(crate) fn reset_state(store: &DirectoryStore, identity: &'static str) -> bool {
    migration::reset_confirmed(&STATE, store, &mut credentials(identity)).is_ok()
}

/// This build's desktop Update Floor: the Stepping Stone release that still migrates data too
/// old for this one. None for a build that names a commit.
pub(crate) fn stepping_stone() -> Option<&'static str> {
    Some(env!("SPACETERM_DESKTOP_FLOOR")).filter(|floor| !floor.is_empty())
}

/// The desktop state root of this build's identity, or `None` when its directories are unknown.
pub(crate) fn current_state_store(directories: &AppDirectories) -> Option<Arc<DirectoryStore>> {
    let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
    let clients = spaceterm_core::core_data_root(identity)
        .ok()?
        .join("clients");
    Some(Arc::new(state_store(
        directories.data.clone(),
        directories.state.clone(),
        clients,
    )))
}

mod secure_store;
#[cfg(test)]
mod tests;
