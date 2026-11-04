//! `uninstall [--delete-data]`: remove SpaceTerm Core's account service, its managed CLI
//! launcher and every installed version from this machine, then the SpaceTerm app as
//! [`spaceterm::installed_app`] finds it. Workspaces, Machine Settings,
//! pairings and saved machines remain unless `--delete-data` is given. Then this CLI's
//! credential items go first, and SpaceTerm Core's data root, which holds this CLI's state, goes
//! after them. The SpaceTerm app's own settings and saved machines remain either way. Without
//! `--confirm`, it only previews, also while SpaceTerm Core is stopped: the preview names the
//! Terminal Sessions that end and the token that confirms it. While the SpaceTerm app runs, it
//! refuses before it previews or removes anything.
use super::{Context, Reply};
use crate::contract::{self, Code, Confirmation, Empty, Envelope, Failure, ServiceEffects};
use spaceterm::installed_app::{Account, AppIdentity, InstalledApp};
use spaceterm_connections::{
    self as connections,
    credentials::{CredentialProvider, Interaction},
};
use spaceterm_core::service::{CliUninstall, Data, Service};

const COMMAND: &str = "uninstall";
const KEPT: &str = "Workspaces, Machine Settings, pairings and saved machines remain.";
const DELETED: &str = "Workspaces, Machine Settings, pairings, and this CLI's saved machines and their credentials are deleted.";
/// What `--delete-data` leaves when it also removes the app, and where to delete that too.
const APP_DATA_KEPT: &str = " The SpaceTerm app's own settings and saved machines remain. Uninstall SpaceTerm in SpaceTerm's Settings deletes them too.";

/// What `uninstall` needs from this machine. Tests script it.
trait Removal {
    /// Whether an installed SpaceTerm app was found to remove.
    fn has_app(&self) -> bool;
    /// Whether one of the installed SpaceTerm apps runs now.
    fn app_running(&self) -> bool;
    /// Previews or confirms SpaceTerm Core's uninstall. A confirmed [`Data::Delete`] runs
    /// `forget` first, under the service lock, and removes nothing when it fails.
    fn uninstall(
        &self,
        token: Option<&str>,
        choice: Data,
        forget: &mut dyn FnMut() -> Result<(), Failure>,
    ) -> Result<CliUninstall, Failure>;
    /// Removes the credential item of every saved, unpairing and enrolling machine of this CLI.
    fn forget_credentials(&self) -> Result<(), Failure>;
    /// Removes every installed SpaceTerm app.
    fn remove_apps(&self) -> Result<(), Failure>;
}

pub fn run(context: &Context<'_>, delete_data: bool, token: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    if let Err(failure) = crate::interrupt::dispatch() {
        return Reply::failure(COMMAND, None, failure);
    }
    let service = match Service::for_identity(context.identity) {
        Ok(service) => service,
        Err(error) => return Reply::failure(COMMAND, None, contract::from_core_error(error)),
    };
    // Found before SpaceTerm Core goes, while its data root still marks what must stay.
    let apps = Account::current(context.identity).map_or_else(Vec::new, |account| {
        InstalledApp::installed(&AppIdentity::for_identity(context.identity), &account)
    });
    uninstall(
        &Native {
            context,
            service,
            apps,
        },
        delete_data,
        token,
    )
}

fn uninstall(removal: &dyn Removal, delete_data: bool, token: Option<&str>) -> Reply {
    // A running app would lose its files underneath it, and with them the SpaceTerm Core
    // package it reinstalls from.
    if removal.app_running() {
        return Reply::failure(COMMAND, None, Failure::new(Code::AppRunning));
    }
    let choice = if delete_data {
        Data::Delete
    } else {
        Data::Keep
    };
    let has_app = removal.has_app();
    let what = match (delete_data, has_app) {
        (false, _) => KEPT.to_owned(),
        (true, false) => DELETED.to_owned(),
        (true, true) => format!("{DELETED}{APP_DATA_KEPT}"),
    };
    let flag = if delete_data { " --delete-data" } else { "" };
    let (subject, verb) = if has_app {
        ("SpaceTerm and SpaceTerm Core", "are")
    } else {
        ("SpaceTerm Core", "is")
    };
    match removal.uninstall(token, choice, &mut || removal.forget_credentials()) {
        Ok(CliUninstall::Completed) => {
            if let Err(failure) = removal.remove_apps() {
                return Reply::failure(COMMAND, None, failure);
            }
            Reply::new(
                &Envelope::ok(COMMAND, None, Empty {}),
                format!("{subject} {verb} uninstalled from this machine. {what}\n"),
            )
        }
        Ok(CliUninstall::Preview {
            token,
            expires_at,
            sessions,
        }) => {
            let Some(expires_at) = i64::try_from(expires_at)
                .ok()
                .and_then(|value| jiff::Timestamp::from_second(value).ok())
            else {
                return Reply::failure(COMMAND, None, Failure::new(Code::StorageUnavailable));
            };
            let effects = ServiceEffects {
                ends_sessions: u32::try_from(sessions.len()).unwrap_or(u32::MAX),
                sessions: sessions.iter().map(|id| super::hex(id)).collect(),
                pending: None,
            };
            let text = format!(
                "Uninstall {subject} from this machine? Every Terminal Session on it ends, for every client. {what}\nConfirm with: spaceterm uninstall{flag} --confirm {token}\n"
            );
            Reply::new(
                &Envelope::confirmation(
                    COMMAND,
                    None,
                    Confirmation {
                        token,
                        expires_at: expires_at.to_string(),
                        effects,
                    },
                ),
                text,
            )
        }
        Err(failure) => Reply::failure(COMMAND, None, failure),
    }
}

/// This machine's SpaceTerm Core, this CLI's credential items and the installed apps.
struct Native<'a> {
    context: &'a Context<'a>,
    service: Service,
    apps: Vec<InstalledApp>,
}

impl Removal for Native<'_> {
    fn has_app(&self) -> bool {
        !self.apps.is_empty()
    }

    fn app_running(&self) -> bool {
        self.apps.iter().any(InstalledApp::is_running)
    }

    fn uninstall(
        &self,
        token: Option<&str>,
        choice: Data,
        forget: &mut dyn FnMut() -> Result<(), Failure>,
    ) -> Result<CliUninstall, Failure> {
        self.service
            .uninstall_cli_forgetting(token, choice, || forget().map_err(Forget::Failed))
            .map_err(|error| match error {
                Forget::Core(error) => contract::from_core_error(error),
                Forget::Failed(failure) => failure,
            })
    }

    fn forget_credentials(&self) -> Result<(), Failure> {
        let root = spaceterm_core::cli_state_root(self.context.identity)
            .map_err(contract::from_core_error)?;
        let items = connections::ConnectionStore::new(root)
            .credential_items()
            .map_err(crate::connection::store_failure)?;
        // A locked keyring asks to unlock when a person can answer.
        let interaction = if self.context.interactive() {
            Interaction::Requested
        } else {
            Interaction::Quiet
        };
        for (kind, key) in items {
            self.context
                .credentials
                .open(kind)
                .and_then(|store| store.remove(&key, interaction))
                .map_err(crate::connection::credential_failure)?;
        }
        Ok(())
    }

    fn remove_apps(&self) -> Result<(), Failure> {
        if self.apps.iter().filter(|app| app.remove().is_err()).count() > 0 {
            return Err(Failure::new(Code::AppNotRemoved));
        }
        Ok(())
    }
}

/// Why [`Service::uninstall_cli_forgetting`] failed: SpaceTerm Core, or removing a credential
/// item.
enum Forget {
    Core(spaceterm_core::CoreError),
    Failed(Failure),
}

impl From<spaceterm_core::CoreError> for Forget {
    fn from(error: spaceterm_core::CoreError) -> Self {
        Self::Core(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Removal steps whose answers a test sets and whose calls it records.
    struct Scripted {
        has_app: bool,
        running: bool,
        credentials: Result<(), Failure>,
        log: RefCell<Vec<String>>,
    }

    impl Scripted {
        fn new(has_app: bool, running: bool) -> Self {
            Self {
                has_app,
                running,
                credentials: Ok(()),
                log: RefCell::default(),
            }
        }
    }

    impl Removal for Scripted {
        fn has_app(&self) -> bool {
            self.has_app
        }
        fn app_running(&self) -> bool {
            self.running
        }
        fn uninstall(
            &self,
            token: Option<&str>,
            choice: Data,
            forget: &mut dyn FnMut() -> Result<(), Failure>,
        ) -> Result<CliUninstall, Failure> {
            let Some(token) = token else {
                self.log.borrow_mut().push(format!("preview {choice:?}"));
                return Ok(CliUninstall::Preview {
                    token: "c1-token".into(),
                    expires_at: 1_800_000_000,
                    sessions: Vec::new(),
                });
            };
            assert_eq!(token, "c1-token");
            // As SpaceTerm Core does: `forget` first, and nothing removed when it fails.
            if choice == Data::Delete {
                forget()?;
            }
            self.log.borrow_mut().push(format!("uninstall {choice:?}"));
            Ok(CliUninstall::Completed)
        }
        fn forget_credentials(&self) -> Result<(), Failure> {
            self.log.borrow_mut().push("forget credentials".into());
            self.credentials.clone()
        }
        fn remove_apps(&self) -> Result<(), Failure> {
            self.log.borrow_mut().push("remove apps".into());
            Ok(())
        }
    }

    fn code(reply: &Reply) -> Option<String> {
        let envelope = &reply.envelope;
        envelope
            .error
            .as_ref()
            .map(|error| error.code.as_str().to_owned())
    }

    #[test]
    fn a_running_app_refuses_before_anything_is_previewed_or_removed() {
        for (delete_data, token) in [(false, None), (true, None), (true, Some("c1-token"))] {
            let removal = Scripted::new(true, true);
            let reply = uninstall(&removal, delete_data, token);
            assert_eq!(code(&reply).as_deref(), Some("app_running"));
            assert!(removal.log.borrow().is_empty());
        }
        assert_eq!(
            Code::AppRunning.message(),
            "SpaceTerm is running. Quit SpaceTerm, then run this again."
        );
    }

    #[test]
    fn delete_data_removes_credentials_before_the_core_and_then_the_app() {
        let removal = Scripted::new(true, false);
        let reply = uninstall(&removal, true, Some("c1-token"));
        assert_eq!(code(&reply), None);
        assert_eq!(
            *removal.log.borrow(),
            ["forget credentials", "uninstall Delete", "remove apps"]
        );
    }

    #[test]
    fn a_credential_failure_removes_nothing_so_a_rerun_can_finish() {
        let mut removal = Scripted::new(true, false);
        removal.credentials = Err(Failure::new(Code::CredentialInteractionRequired));
        let reply = uninstall(&removal, true, Some("c1-token"));
        assert_eq!(
            code(&reply).as_deref(),
            Some("credential_interaction_required")
        );
        assert_eq!(*removal.log.borrow(), ["forget credentials"]);
        removal.credentials = Ok(());
        removal.log.borrow_mut().clear();
        assert_eq!(code(&uninstall(&removal, true, Some("c1-token"))), None);
        assert_eq!(
            *removal.log.borrow(),
            ["forget credentials", "uninstall Delete", "remove apps"]
        );
    }

    #[test]
    fn keeping_data_leaves_credentials_alone() {
        let removal = Scripted::new(false, false);
        assert_eq!(code(&uninstall(&removal, false, Some("c1-token"))), None);
        assert_eq!(*removal.log.borrow(), ["uninstall Keep", "remove apps"]);
    }

    /// `--delete-data` deletes SpaceTerm Core's data and this CLI's saved machines, not the
    /// app's own, and the text says so.
    #[test]
    fn delete_data_names_only_what_it_deletes() {
        let with_app = uninstall(&Scripted::new(true, false), true, None);
        assert_eq!(
            with_app.stdout,
            "Uninstall SpaceTerm and SpaceTerm Core from this machine? Every Terminal Session on it ends, for every client. Workspaces, Machine Settings, pairings, and this CLI's saved machines and their credentials are deleted. The SpaceTerm app's own settings and saved machines remain. Uninstall SpaceTerm in SpaceTerm's Settings deletes them too.\nConfirm with: spaceterm uninstall --delete-data --confirm c1-token\n"
        );
        let done = uninstall(&Scripted::new(false, false), true, Some("c1-token"));
        assert_eq!(
            done.stdout,
            "SpaceTerm Core is uninstalled from this machine. Workspaces, Machine Settings, pairings, and this CLI's saved machines and their credentials are deleted.\n"
        );
    }
}
