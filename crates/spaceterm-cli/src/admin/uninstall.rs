//! `uninstall [--delete-data]`: remove SpaceTerm Core's account service, its managed CLI
//! launcher and every installed version from this machine. Workspaces, Machine Settings,
//! pairings and saved machines remain unless `--delete-data` is given. Then SpaceTerm Core's
//! data root, this CLI's state and its credential items go too. While SpaceTerm Core runs, a
//! preview names the Terminal Sessions that end and takes a confirmation first.
use super::{Context, Reply};
use crate::contract::{self, Code, Confirmation, Empty, Envelope, Failure, ServiceEffects};
use spaceterm_connections::{
    self as connections,
    credentials::{CredentialProvider, Interaction},
};
use spaceterm_core::service::{CoreUninstall, Data, Service};

const COMMAND: &str = "uninstall";
const KEPT: &str = "Workspaces, Machine Settings, pairings and saved machines remain.";
const DELETED: &str =
    "Workspaces, Machine Settings, pairings, saved machines and their credentials are deleted.";

pub fn run(context: &Context<'_>, delete_data: bool, token: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    if let Err(failure) = crate::interrupt::dispatch() {
        return Reply::failure(COMMAND, None, failure);
    }
    let choice = if delete_data {
        Data::Delete
    } else {
        Data::Keep
    };
    let what = if delete_data { DELETED } else { KEPT };
    let flag = if delete_data { " --delete-data" } else { "" };
    let service = match Service::for_identity(context.identity) {
        Ok(service) => service,
        Err(error) => return Reply::failure(COMMAND, None, contract::from_core_error(error)),
    };
    // The data root holds the CLI's saved machines, so read their credential items before
    // uninstall deletes the records that name them.
    let credentials = if delete_data {
        match saved_credentials(context) {
            Ok(credentials) => credentials,
            Err(failure) => return Reply::failure(COMMAND, None, failure),
        }
    } else {
        Vec::new()
    };
    let result = service
        .uninstall_core(token, choice)
        .map_err(contract::from_core_error);
    if matches!(result, Ok(CoreUninstall::Completed))
        && let Err(failure) = remove_credentials(context, &credentials)
    {
        return Reply::failure(COMMAND, None, failure);
    }
    match result {
        Ok(CoreUninstall::Completed) => Reply::new(
            &Envelope::ok(COMMAND, None, Empty {}),
            format!("SpaceTerm Core is uninstalled from this machine. {what}\n"),
        ),
        Ok(CoreUninstall::Preview {
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
                "Uninstall SpaceTerm Core from this machine? Every Terminal Session on it ends, for every client. {what}\nConfirm with: spaceterm uninstall{flag} --confirm {token}\n"
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

/// The credential item of every saved and pending machine of this CLI.
fn saved_credentials(
    context: &Context<'_>,
) -> Result<Vec<(connections::StoreKind, [u8; 32])>, Failure> {
    let root =
        spaceterm_core::cli_state_root(context.identity).map_err(contract::from_core_error)?;
    let store = connections::ConnectionStore::new(root);
    let mut rows = store.load().map_err(crate::connection::store_failure)?;
    rows.extend(store.pending().map_err(crate::connection::store_failure)?);
    Ok(rows
        .into_iter()
        .map(|row| (row.credential_store, row.credential))
        .collect())
}

fn remove_credentials(
    context: &Context<'_>,
    credentials: &[(connections::StoreKind, [u8; 32])],
) -> Result<(), Failure> {
    for (kind, key) in credentials {
        context
            .credentials
            .open(*kind)
            .and_then(|store| store.remove(key, Interaction::Quiet))
            .map_err(crate::connection::credential_failure)?;
    }
    Ok(())
}
