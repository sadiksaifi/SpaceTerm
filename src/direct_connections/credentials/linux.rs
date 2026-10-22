use std::collections::HashMap;
use std::time::Duration;

use futures_lite::{StreamExt, future};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};
use zeroize::Zeroizing;

use super::{CredentialError, CredentialStore, Interaction, account};

const DESTINATION: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const ITEM_INTERFACE: &str = "org.freedesktop.Secret.Item";
const COLLECTION_INTERFACE: &str = "org.freedesktop.Secret.Collection";
const PROMPT_INTERFACE: &str = "org.freedesktop.Secret.Prompt";

// The session bus authenticates this account. Plain is Secret Service's native session
// encoding, not a storage fallback: only the selected persistent native collection stores it.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);
type Search = (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>);

pub(super) struct SecretService {
    service: String,
}

impl SecretService {
    pub(super) fn new(identity: &str) -> Self {
        Self {
            service: format!("io.github.sadiksaifi.{identity}.direct-network"),
        }
    }

    fn attributes<'a>(&'a self, account: &'a str) -> HashMap<&'a str, &'a str> {
        HashMap::from([
            ("application", "spaceterm"),
            ("service", self.service.as_str()),
            ("pairing", account),
        ])
    }

    async fn find(
        &self,
        service: &Proxy<'_>,
        account: &str,
    ) -> Result<Option<(OwnedObjectPath, bool)>, CredentialError> {
        let (mut unlocked, mut locked): Search = service
            .call("SearchItems", &(self.attributes(account),))
            .await
            .map_err(classify)?;
        if unlocked.len() + locked.len() > 1 {
            return Err(CredentialError::Unavailable);
        }
        Ok(unlocked
            .pop()
            .map(|path| (path, false))
            .or_else(|| locked.pop().map(|path| (path, true))))
    }

    async fn read_secret(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        let connection = connect().await?;
        let service = proxy(&connection, SERVICE_PATH, SERVICE_INTERFACE).await?;
        let Some((path, locked)) = self.find(&service, &account(key)).await? else {
            return Ok(None);
        };
        if locked {
            unlock(&connection, &service, &path, ITEM_INTERFACE, interaction).await?;
        }
        let session = open_session(&service).await?;
        let result = async {
            let item = proxy(&connection, path.as_str(), ITEM_INTERFACE).await?;
            let (returned_session, parameters, value, content_type): Secret = item
                .call("GetSecret", &(&session,))
                .await
                .map_err(classify)?;
            let value = Zeroizing::new(value);
            if returned_session != session
                || !parameters.is_empty()
                || content_type != "application/octet-stream"
            {
                return Err(CredentialError::Unavailable);
            }
            Ok(Some(value))
        }
        .await;
        close_session(&connection, &session).await;
        result
    }

    async fn insert_secret(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        let connection = connect().await?;
        let account = account(key);
        // Serialize the check-and-create across SpaceTerm processes. A busy name fails closed;
        // it is automatically released when this operation drops its bus connection.
        let name = format!("{}.credential.k{account}", self.service);
        let bus = Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        .map_err(classify)?;
        // D-Bus DoNotQueue = 4, PrimaryOwner = 1. No persistent helper or registration.
        let claimed: u32 = bus
            .call("RequestName", &(&name, 4_u32))
            .await
            .map_err(classify)?;
        if claimed != 1 {
            return Err(CredentialError::Unavailable);
        }
        let service = proxy(&connection, SERVICE_PATH, SERVICE_INTERFACE).await?;
        if self.find(&service, &account).await?.is_some() {
            return Err(CredentialError::Unavailable);
        }
        let collection_path: OwnedObjectPath = service
            .call("ReadAlias", &("default",))
            .await
            .map_err(classify)?;
        if collection_path.as_str() == "/" {
            return Err(CredentialError::Unavailable);
        }
        unlock(
            &connection,
            &service,
            &collection_path,
            COLLECTION_INTERFACE,
            Interaction::Requested,
        )
        .await?;
        let session = open_session(&service).await?;
        let result = async {
            let collection =
                proxy(&connection, collection_path.as_str(), COLLECTION_INTERFACE).await?;
            let attributes = self.attributes(&account);
            let properties = HashMap::from([
                (
                    "org.freedesktop.Secret.Item.Label",
                    Value::from("SpaceTerm paired device"),
                ),
                (
                    "org.freedesktop.Secret.Item.Attributes",
                    Value::from(attributes),
                ),
            ]);
            // Borrow the secret for serialization; no command, environment or file transports it.
            let encoded = (&session, &[] as &[u8], secret, "application/octet-stream");
            let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = collection
                .call("CreateItem", &(properties, encoded, false))
                .await
                .map_err(classify)?;
            if prompt.as_str() != "/" {
                let result = run_prompt(&connection, &prompt, Interaction::Requested).await?;
                let item =
                    OwnedObjectPath::try_from(result).map_err(|_| CredentialError::Unavailable)?;
                if item.as_str() == "/" {
                    return Err(CredentialError::Unavailable);
                }
            } else if item.as_str() == "/" {
                return Err(CredentialError::Unavailable);
            }
            Ok(())
        }
        .await;
        close_session(&connection, &session).await;
        result
    }

    async fn remove_secret(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<(), CredentialError> {
        let connection = connect().await?;
        let service = proxy(&connection, SERVICE_PATH, SERVICE_INTERFACE).await?;
        let Some((path, locked)) = self.find(&service, &account(key)).await? else {
            return Ok(());
        };
        if locked {
            unlock(&connection, &service, &path, ITEM_INTERFACE, interaction).await?;
        }
        let item = proxy(&connection, path.as_str(), ITEM_INTERFACE).await?;
        let prompt: OwnedObjectPath = item.call("Delete", &()).await.map_err(classify)?;
        if prompt.as_str() != "/" {
            run_prompt(&connection, &prompt, interaction).await?;
        }
        Ok(())
    }
}

impl CredentialStore for SecretService {
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        pollster::block_on(self.read_secret(key, interaction))
    }

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        pollster::block_on(self.insert_secret(key, secret))
    }

    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError> {
        pollster::block_on(self.remove_secret(key, interaction))
    }
}

async fn connect() -> Result<Connection, CredentialError> {
    zbus::connection::Builder::session()
        .map_err(classify)?
        .method_timeout(Duration::from_secs(30))
        .build()
        .await
        .map_err(classify)
}

async fn proxy<'a>(
    connection: &Connection,
    path: &'a str,
    interface: &'a str,
) -> Result<Proxy<'a>, CredentialError> {
    zbus::proxy::Builder::<Proxy<'a>>::new(connection)
        .destination(DESTINATION)
        .map_err(classify)?
        .path(path)
        .map_err(classify)?
        .interface(interface)
        .map_err(classify)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(classify)
}

async fn unlock(
    connection: &Connection,
    service: &Proxy<'_>,
    path: &OwnedObjectPath,
    interface: &str,
    interaction: Interaction,
) -> Result<(), CredentialError> {
    let object = proxy(connection, path.as_str(), interface).await?;
    let locked: bool = object.get_property("Locked").await.map_err(classify)?;
    if !locked {
        return Ok(());
    }
    if interaction == Interaction::Quiet {
        return Err(CredentialError::InteractionRequired);
    }
    let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service
        .call("Unlock", &(vec![path],))
        .await
        .map_err(classify)?;
    if prompt.as_str() != "/" {
        run_prompt(connection, &prompt, interaction).await?;
    }
    if object
        .get_property::<bool>("Locked")
        .await
        .map_err(classify)?
    {
        return Err(CredentialError::InteractionRequired);
    }
    Ok(())
}

async fn open_session(service: &Proxy<'_>) -> Result<OwnedObjectPath, CredentialError> {
    let (_, session): (OwnedValue, OwnedObjectPath) = service
        .call("OpenSession", &("plain", Value::from("")))
        .await
        .map_err(classify)?;
    if session.as_str() == "/" {
        return Err(CredentialError::Unavailable);
    }
    Ok(session)
}

async fn close_session(connection: &Connection, session: &OwnedObjectPath) {
    if let Ok(session) = proxy(
        connection,
        session.as_str(),
        "org.freedesktop.Secret.Session",
    )
    .await
    {
        let _ = session.call::<_, _, ()>("Close", &()).await;
    }
}

async fn run_prompt(
    connection: &Connection,
    path: &OwnedObjectPath,
    interaction: Interaction,
) -> Result<OwnedValue, CredentialError> {
    let prompt = proxy(connection, path.as_str(), PROMPT_INTERFACE).await?;
    if interaction == Interaction::Quiet {
        // Delete can return a prompt object. Dismiss it without ever invoking Prompt.
        let _ = prompt.call::<_, _, ()>("Dismiss", &()).await;
        return Err(CredentialError::InteractionRequired);
    }
    let mut completed = prompt.receive_signal("Completed").await.map_err(classify)?;
    prompt
        .call::<_, _, ()>("Prompt", &("",))
        .await
        .map_err(classify)?;
    let result = future::race(
        async {
            let message = completed.next().await.ok_or(CredentialError::Unavailable)?;
            let (dismissed, result): (bool, OwnedValue) = message
                .body()
                .deserialize()
                .map_err(|_| CredentialError::Unavailable)?;
            if dismissed {
                Err(CredentialError::InteractionRequired)
            } else {
                Ok(result)
            }
        },
        async {
            async_io::Timer::after(Duration::from_secs(120)).await;
            Err(CredentialError::InteractionRequired)
        },
    )
    .await;
    if result.is_err() {
        let _ = prompt.call::<_, _, ()>("Dismiss", &()).await;
    }
    result
}

fn classify(error: zbus::Error) -> CredentialError {
    match error {
        zbus::Error::MethodError(name, _, _)
            if matches!(
                name.as_str(),
                "org.freedesktop.Secret.Error.IsLocked" | "org.freedesktop.DBus.Error.AccessDenied"
            ) =>
        {
            CredentialError::InteractionRequired
        }
        _ => CredentialError::Unavailable,
    }
}
