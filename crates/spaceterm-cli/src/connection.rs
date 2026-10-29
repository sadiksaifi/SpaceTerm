//! CLI-owned Saved Connections, pairing, and server route selection.
pub mod route;
mod terminal;
use crate::args::{ConnectionCommand, Global};
use crate::contract::{
    self, Code, ComparisonEvent, Confirmation, ConnectionAdd, ConnectionList, ConnectionRemove,
    Details, Empty, Envelope, Failure, ListedConnection, Revocation, UsageDetails, UsageKind,
    Warning, WarningCode, WarningDetails,
};
use crate::state::{CliState, RouteKey};
use connections::enrollment::{self, PairingClient, PairingNetwork};
use serde::Serialize;
use serde_json::Value;
use spaceterm_connections::{
    self as connections, ConnectionStore, CredentialError, CredentialStore, SavedConnection,
    StoreError,
};
use spaceterm_server::direct::{DirectError, Invitation};
use spaceterm_server::pane::control::{ClientName, MAX_CLIENT_NAME_CHARS};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// Credential access is constructed only for enrollment, recovery or a paired route.
pub trait CredentialProvider {
    fn open(&self) -> Result<Arc<dyn CredentialStore>, CredentialError>;
}
pub struct NativeCredentials<'a>(pub &'a str);
impl CredentialProvider for NativeCredentials<'_> {
    fn open(&self) -> Result<Arc<dyn CredentialStore>, CredentialError> {
        connections::native(self.0)
    }
}
/// Reading held outcomes never admits a connection or settles a journal.
pub trait HeldOutcomes {
    fn count(&self, route: &[u8; 32]) -> Result<u32, Failure>;
}
struct NativeHeld<'a>(&'a str);
impl HeldOutcomes for NativeHeld<'_> {
    fn count(&self, route: &[u8; 32]) -> Result<u32, Failure> {
        let state = CliState::open(self.0).map_err(contract::from_server_error)?;
        let held = state
            .held(&RouteKey::Paired(*route))
            .map_err(contract::from_server_error)?;
        Ok(held.into_iter().fold(0u32, |total, identity| {
            total
                .saturating_add(identity.operations)
                .saturating_add(identity.runs)
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputError {
    BrokenPipe,
    Failed,
}
impl From<std::io::Error> for OutputError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            Self::BrokenPipe
        } else {
            Self::Failed
        }
    }
}
pub trait InteractionIo {
    fn interactive(&self) -> bool;
    fn interrupted(&self) -> bool;
    fn invitation(&mut self, deadline: Instant) -> Result<Zeroizing<String>, Failure>;
    fn consent(&mut self, invitation: &Invitation, deadline: Instant) -> Result<bool, Failure>;
    fn confirm_removal(&mut self, name: &str, deadline: Instant) -> Result<bool, Failure>;
    fn comparison(&mut self, event: &ComparisonEvent, json: bool) -> Result<(), OutputError>;
    fn wait(&mut self, duration: Duration);
}

/// One final envelope. `connection add --json` emits its comparison event through `InteractionIo` first.
pub struct CommandOutput {
    pub envelope: Value,
    pub exit: u8,
    lines: Vec<String>,
    failure: Option<Failure>,
}
impl CommandOutput {
    fn envelope<D: Serialize>(
        envelope: Envelope<D>,
        lines: Vec<String>,
        failure: Option<Failure>,
    ) -> Self {
        let exit = envelope.exit();
        match serde_json::to_value(envelope) {
            Ok(envelope) => Self {
                envelope,
                exit,
                lines,
                failure,
            },
            Err(_) => Self::failure("", Code::OutputFailed.into()),
        }
    }
    fn failure(command: &str, failure: Failure) -> Self {
        Self::envelope(
            Envelope::error(command, None, failure.clone(), Empty {}),
            Vec::new(),
            Some(failure),
        )
    }
    pub fn render(&self, global: &Global, output: &mut impl Write, errors: &mut impl Write) -> u8 {
        if self.exit == contract::EXIT_BROKEN_PIPE {
            return self.exit;
        }
        let result = if global.json {
            serde_json::to_writer(&mut *output, &self.envelope)
                .map_err(std::io::Error::from)
                .and_then(|()| output.write_all(b"\n"))
                .and_then(|()| output.flush())
        } else if !global.quiet || self.exit != 0 {
            self.lines
                .iter()
                .try_for_each(|line| writeln!(output, "{line}"))
                .and_then(|()| output.flush())
        } else {
            Ok(())
        };
        if let Err(error) = result {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return contract::EXIT_BROKEN_PIPE;
            }
            let _ = writeln!(errors, "{}", Failure::new(Code::OutputFailed).text());
            return Code::OutputFailed.exit();
        }
        if !global.json {
            if let Some(failure) = &self.failure {
                let _ = writeln!(errors, "{}", failure.text());
                if let Details::Usage(details) = &failure.details
                    && let Some(argument) = &details.argument
                {
                    let _ = writeln!(errors, "argument: {argument}");
                }
            }
            if let Some(warnings) = self.envelope["warnings"].as_array() {
                for warning in warnings {
                    if let Some(message) = warning["message"].as_str() {
                        let _ = writeln!(errors, "warning: {message}");
                    }
                }
            }
        }
        self.exit
    }
}

pub fn execute(command: &ConnectionCommand, global: &Global, identity: &str) -> CommandOutput {
    let path = command_path(command);
    let environment = if global.server.is_none() {
        std::env::var_os("SPACETERM_SERVER")
    } else {
        None
    };
    let environment = match environment
        .as_ref()
        .map(|value| value.to_str().ok_or_else(|| usage("--server")))
        .transpose()
    {
        Ok(environment) => environment,
        Err(failure) => return CommandOutput::failure(path, failure),
    };
    match route::selected(global.server.as_deref(), environment) {
        Ok("local") => {}
        Ok(_) => return CommandOutput::failure(path, usage("--server")),
        Err(failure) => return CommandOutput::failure(path, failure),
    }
    let root = match spaceterm_server::cli_state_root(identity) {
        Ok(root) => root,
        Err(error) => return CommandOutput::failure(path, contract::from_server_error(error)),
    };
    let store = ConnectionStore::new(root);
    let mut io = match terminal::Terminal::new(!global.json && !global.no_input) {
        Ok(io) => io,
        Err(failure) => return CommandOutput::failure(path, failure),
    };
    Connections {
        identity,
        global,
        store: &store,
        credentials: &NativeCredentials(identity),
        held: &NativeHeld(identity),
        network: &enrollment::NativeNetwork,
        io: &mut io,
    }
    .run(command)
}

pub struct Connections<'a> {
    pub identity: &'a str,
    pub global: &'a Global,
    pub store: &'a ConnectionStore,
    pub credentials: &'a dyn CredentialProvider,
    pub held: &'a dyn HeldOutcomes,
    pub network: &'a dyn PairingNetwork,
    pub io: &'a mut dyn InteractionIo,
}

impl Connections<'_> {
    pub fn run(&mut self, command: &ConnectionCommand) -> CommandOutput {
        let path = command_path(command);
        if self
            .global
            .server
            .as_deref()
            .is_some_and(|server| server != "local")
        {
            return CommandOutput::failure(path, usage("--server"));
        }
        let duration =
            self.global
                .timeout
                .unwrap_or(if matches!(command, ConnectionCommand::Add { .. }) {
                    Duration::from_secs(300)
                } else {
                    Duration::from_secs(30)
                });
        let deadline = Instant::now() + duration;
        let result = match command {
            ConnectionCommand::List => self.list(),
            ConnectionCommand::Add {
                name,
                join,
                address,
            } => self.add(name.as_deref(), *join, address.as_deref(), deadline),
            ConnectionCommand::Remove { name, confirm } => {
                self.remove(name, confirm.as_deref(), deadline)
            }
        };
        result.unwrap_or_else(|failure| CommandOutput::failure(path, failure))
    }
    fn allows_input(&self) -> bool {
        !self.global.json && !self.global.no_input && self.io.interactive()
    }
    fn checkpoint(&self, deadline: Instant) -> Result<(), Failure> {
        if self.io.interrupted() {
            Err(Code::Interrupted.into())
        } else if Instant::now() >= deadline {
            Err(Code::Timeout.into())
        } else {
            Ok(())
        }
    }
    fn pause(&mut self, duration: Duration, deadline: Instant) -> Result<(), Failure> {
        let until = (Instant::now() + duration).min(deadline);
        while Instant::now() < until {
            self.checkpoint(deadline)?;
            self.io.wait(
                until
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100)),
            );
        }
        self.checkpoint(deadline)
    }
    fn list(&self) -> Result<CommandOutput, Failure> {
        let mut rows = self.store.load().map_err(store_failure)?;
        rows.extend(self.store.pending().map_err(store_failure)?);
        let mut connections = Vec::with_capacity(rows.len());
        for row in rows {
            connections.push(ListedConnection {
                name: row.name.clone(),
                route: format!("id:{}", hex(&row.route)),
                endpoint: row.endpoint.clone(),
                fingerprint: row.paired.pin.fingerprint(),
                store: platform_kind(),
                unpair_pending: row.unpair == connections::UnpairState::Pending,
                held_outcomes: self.held.count(&row.route)?,
            });
        }
        connections.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.route.cmp(&right.route))
        });
        let lines = connections
            .iter()
            .map(|row| {
                format!(
                    "{}\t{}\t{}\t{}\t{}{}\theld outcomes: {}",
                    row.name,
                    row.route,
                    row.endpoint,
                    row.fingerprint,
                    kind_name(row.store),
                    if row.unpair_pending {
                        "\tunpair pending"
                    } else {
                        ""
                    },
                    row.held_outcomes
                )
            })
            .collect();
        Ok(CommandOutput::envelope(
            Envelope::ok("connection.list", None, ConnectionList { connections }),
            lines,
            None,
        ))
    }
    fn add(
        &mut self,
        name: Option<&str>,
        join: bool,
        address: Option<&str>,
        deadline: Instant,
    ) -> Result<CommandOutput, Failure> {
        if let Some(name) = name
            && !connections::valid_name(name)
        {
            return Err(reserved_name());
        }
        if let Some(address) = address {
            spaceterm_server::direct::Endpoint::parse(address).map_err(|_| usage("--address"))?;
        }
        if !join && !self.allows_input() {
            return Err(Code::InteractionRequired.into());
        }
        self.checkpoint(deadline)?;
        let link = self.io.invitation(deadline)?;
        let mut invitation = Invitation::parse(&link).map_err(contract::from_direct_error)?;
        if invitation.server_pin().build != self.identity {
            return Err(contract::from_direct_error(DirectError::WrongServer));
        }
        if let Some(address) = address {
            invitation = invitation
                .with_address(address)
                .map_err(contract::from_direct_error)?;
        }
        let name = name
            .map(str::to_owned)
            .unwrap_or_else(|| slug(invitation.server_name()));
        if !connections::valid_name(&name) {
            return Err(reserved_name());
        }
        // Validate uniqueness before credential access. The guarded reservation rechecks it.
        if self
            .store
            .load()
            .map_err(store_failure)?
            .iter()
            .any(|row| row.name == name)
        {
            return Err(reserved_name());
        }
        if !join && !self.io.consent(&invitation, deadline)? {
            return Err(Code::InteractionRequired.into());
        }
        self.checkpoint(deadline)?;
        let credentials = self.credentials.open().map_err(credential_failure)?;
        self.checkpoint(deadline)?;
        let client = PairingClient {
            store: self.store,
            credentials: credentials.as_ref(),
            network: self.network,
        };
        let pending = client
            .recover_while(deadline, &|| self.io.interrupted())
            .map_err(pairing_failure)?;
        self.checkpoint(deadline)?;
        let computer = ClientName::this_computer();
        let computer: String = computer
            .as_str()
            .chars()
            .take(MAX_CLIENT_NAME_CHARS - 4)
            .collect();
        let mut joining = client
            .begin(
                &invitation,
                name,
                format!("{computer} CLI").into(),
                deadline,
            )
            .map_err(|error| match error {
                enrollment::Error::Store(StoreError::NameTaken) => Code::Changed.into(),
                other => pairing_failure(other),
            })?;
        let attempt = (|| -> Result<CommandOutput, Failure> {
            let comparison = loop {
                self.checkpoint(deadline)?;
                match joining.submit() {
                    Ok(code) => break code,
                    Err(enrollment::Error::Direct(DirectError::Transport)) => {
                        self.pause(Duration::from_secs(2), deadline)?
                    }
                    Err(error) => return Err(pairing_failure(error)),
                }
            };
            let event = ComparisonEvent::new(
                comparison.to_string(),
                timestamp(invitation.expires_at_unix_seconds())?,
            );
            match self.io.comparison(&event, self.global.json) {
                Ok(()) => {}
                Err(OutputError::BrokenPipe) => {
                    let mut result =
                        CommandOutput::failure("connection.add", Code::OutputFailed.into());
                    result.exit = contract::EXIT_BROKEN_PIPE;
                    return Ok(result);
                }
                Err(OutputError::Failed) => return Err(Code::OutputFailed.into()),
            }
            loop {
                self.checkpoint(deadline)?;
                match joining.finish() {
                    Ok(Some(connection)) => {
                        let data = ConnectionAdd {
                            connection: connection_data(
                                &connection,
                                store_kind(credentials.kind()),
                            ),
                        };
                        let lines = vec![format!(
                            "Saved {} ({}) at {}.",
                            connection.name,
                            hex(&connection.route),
                            connection.endpoint
                        )];
                        let failure = if self.io.interrupted() {
                            Some(Failure::new(Code::Interrupted))
                        } else if Instant::now() >= deadline {
                            Some(Failure::new(Code::Timeout))
                        } else {
                            None
                        };
                        let mut envelope = if let Some(failure) = &failure {
                            Envelope::error("connection.add", None, failure.clone(), data)
                        } else {
                            Envelope::ok("connection.add", None, data)
                        };
                        if pending > 0 {
                            envelope.warnings.push(pending_warning());
                        }
                        return Ok(CommandOutput::envelope(envelope, lines, failure));
                    }
                    Ok(None) | Err(enrollment::Error::Direct(DirectError::Transport)) => {
                        self.pause(Duration::from_secs(2), deadline)?
                    }
                    Err(error) => return Err(pairing_failure(error)),
                }
            }
        })();
        match attempt {
            Ok(result) if result.exit == 0 => Ok(result),
            Ok(result) => {
                let _ = joining.cancel(Instant::now() + Duration::from_secs(5));
                Ok(result)
            }
            Err(failure) => {
                if joining
                    .cancel(Instant::now() + Duration::from_secs(5))
                    .is_err()
                {
                    return Err(Code::OutcomeUnconfirmed.into());
                }
                Err(failure)
            }
        }
    }
    fn remove(
        &mut self,
        name: &str,
        confirm: Option<&str>,
        deadline: Instant,
    ) -> Result<CommandOutput, Failure> {
        if !connections::valid_name(name) {
            return Err(reserved_name());
        }
        let supplied = confirm.map(parse_token).transpose()?;
        self.checkpoint(deadline)?;
        let now = unix_now()?;
        let token = if let Some(token) = supplied {
            token
        } else {
            let preview = self
                .store
                .preview_remove(name, now)
                .map_err(store_failure)?;
            if self.allows_input() {
                if !self.io.confirm_removal(name, deadline)? {
                    return Err(Code::InteractionRequired.into());
                }
                preview.token
            } else {
                #[derive(Serialize)]
                struct Effects {
                    connection: contract::Connection,
                    unpairs_this_computer: bool,
                }
                let token = format!("c1-{}", hex(&preview.token));
                let lines = vec![
                    format!(
                        "Unpair this computer from {} at {}.",
                        preview.connection.name, preview.connection.endpoint
                    ),
                    format!("Repeat with --confirm {token}"),
                ];
                let envelope = Envelope::confirmation(
                    "connection.remove",
                    None,
                    Confirmation {
                        token,
                        expires_at: timestamp(preview.expires)?,
                        effects: Effects {
                            connection: connection_data(&preview.connection, platform_kind()),
                            unpairs_this_computer: true,
                        },
                    },
                );
                return Ok(CommandOutput::envelope(
                    envelope,
                    lines,
                    Some(Code::ConfirmationRequired.into()),
                ));
            }
        };
        self.checkpoint(deadline)?;
        let removed = self
            .store
            .confirm_remove(name, &token, unix_now()?)
            .map_err(|error| {
                if error == StoreError::Unavailable {
                    Failure::new(Code::OutcomeUnconfirmed)
                } else {
                    store_failure(error)
                }
            })?;
        if let Ok(credentials) = self.credentials.open() {
            let client = PairingClient {
                store: self.store,
                credentials: credentials.as_ref(),
                network: self.network,
            };
            let _ = client.recover_while(deadline, &|| self.io.interrupted());
        }
        let pending = self
            .store
            .pending()
            .map_err(|_| Failure::new(Code::OutcomeUnconfirmed))?
            .iter()
            .any(|row| row.route == removed.route);
        let data = ConnectionRemove {
            removed: removed.name.clone(),
            revocation: if pending {
                Revocation::Pending
            } else {
                Revocation::Confirmed
            },
        };
        let failure = self.io.interrupted().then(|| {
            Failure::new(if pending {
                Code::OutcomeUnconfirmed
            } else {
                Code::Interrupted
            })
        });
        let mut envelope = if let Some(failure) = &failure {
            Envelope::error("connection.remove", None, failure.clone(), data)
        } else {
            Envelope::ok("connection.remove", None, data)
        };
        if pending {
            envelope.warnings.push(pending_warning());
        }
        Ok(CommandOutput::envelope(
            envelope,
            vec![format!(
                "Removed {}. Revocation {}.",
                removed.name,
                if pending { "pending" } else { "confirmed" }
            )],
            failure,
        ))
    }
}

fn command_path(command: &ConnectionCommand) -> &'static str {
    match command {
        ConnectionCommand::Add { .. } => "connection.add",
        ConnectionCommand::List => "connection.list",
        ConnectionCommand::Remove { .. } => "connection.remove",
    }
}
pub(super) fn usage(argument: &str) -> Failure {
    Failure::with(
        Code::InvalidArguments,
        Details::Usage(UsageDetails {
            argument: Some(argument.to_owned()),
            kind: UsageKind::InvalidValue,
        }),
    )
}
fn reserved_name() -> Failure {
    Failure::with(
        Code::InvalidArguments,
        Details::Usage(UsageDetails {
            argument: Some("<NAME>".into()),
            kind: UsageKind::ReservedName,
        }),
    )
}
pub(super) fn store_failure(error: StoreError) -> Failure {
    match error {
        StoreError::Unavailable => Code::StorageUnavailable.into(),
        StoreError::Changed => Code::Changed.into(),
        StoreError::NameTaken => reserved_name(),
        StoreError::NotFound => Failure::with(
            Code::NotFound,
            Details::Target(contract::TargetDetails::level(contract::Level::Connection)),
        ),
    }
}
pub(super) fn credential_failure(error: CredentialError) -> Failure {
    match error {
        CredentialError::InteractionRequired => Code::CredentialInteractionRequired.into(),
        CredentialError::Unavailable => Code::CredentialUnavailable.into(),
    }
}
fn pairing_failure(error: enrollment::Error) -> Failure {
    match error {
        enrollment::Error::Credential(error) => credential_failure(error),
        enrollment::Error::Store(error) => store_failure(error),
        enrollment::Error::Direct(error) => contract::from_direct_error(error),
        enrollment::Error::Busy => Code::IdentityBusy.into(),
        enrollment::Error::Unconfirmed => Code::OutcomeUnconfirmed.into(),
    }
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn parse_token(text: &str) -> Result<[u8; 32], Failure> {
    let text = text
        .strip_prefix("c1-")
        .filter(|text| text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| usage("--confirm"))?;
    let mut token = [0; 32];
    for (byte, value) in token.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        let value = std::str::from_utf8(value).map_err(|_| usage("--confirm"))?;
        *byte = u8::from_str_radix(value, 16).map_err(|_| usage("--confirm"))?;
    }
    Ok(token)
}
fn slug(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() == 128 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() || slug == "local" {
        "server".into()
    } else {
        slug.into()
    }
}
fn timestamp(seconds: u64) -> Result<String, Failure> {
    let seconds = i64::try_from(seconds).map_err(|_| Failure::new(Code::ProtocolError))?;
    Ok(jiff::Timestamp::from_second(seconds)
        .map_err(|_| Failure::new(Code::ProtocolError))?
        .to_string())
}
fn unix_now() -> Result<u64, Failure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| Code::StorageUnavailable.into())
}
fn store_kind(kind: connections::StoreKind) -> contract::StoreKind {
    match kind {
        connections::StoreKind::Keychain => contract::StoreKind::Keychain,
        connections::StoreKind::SecretService => contract::StoreKind::SecretService,
    }
}
fn platform_kind() -> contract::StoreKind {
    #[cfg(target_os = "macos")]
    {
        contract::StoreKind::Keychain
    }
    #[cfg(not(target_os = "macos"))]
    {
        contract::StoreKind::SecretService
    }
}
fn kind_name(kind: contract::StoreKind) -> &'static str {
    match kind {
        contract::StoreKind::Keychain => "Keychain",
        contract::StoreKind::SecretService => "Secret Service",
    }
}
fn connection_data(
    connection: &SavedConnection,
    store: contract::StoreKind,
) -> contract::Connection {
    contract::Connection {
        name: connection.name.clone(),
        route: format!("id:{}", hex(&connection.route)),
        endpoint: connection.endpoint.clone(),
        fingerprint: connection.paired.pin.fingerprint(),
        store,
    }
}
fn pending_warning() -> Warning {
    Warning::new(WarningCode::UnpairPending, WarningDetails::None)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;
