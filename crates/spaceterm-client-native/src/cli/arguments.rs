//! Side-effect-free bounded command-line parsing.

use std::{ffi::OsString, fmt, net::SocketAddr};

use spaceterm_client::management_cli::AssessmentToken;
use spaceterm_core::{identity::DeviceId, operations::OperationId};
use spaceterm_host::startup::InstallationId;

use super::CliFailure;

pub const MAX_ARGUMENTS: usize = 128;
pub const MAX_ARGUMENT_BYTES: usize = 8192;
pub const MAX_TOTAL_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliInvocation {
    pub json: bool,
    pub non_interactive: bool,
    pub command: CliCommand,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfirmationMode {
    Interactive,
    Assess,
    Token(AssessmentToken),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliCommand {
    Help,
    Version,
    Serve,
    Status {
        operation: Option<RequestSelector>,
    },
    Install {
        version: Option<StableVersion>,
        source: Option<crate::native::artifacts::LocalArtifactSource>,
    },
    Start,
    Restart {
        confirmation: ConfirmationMode,
    },
    Uninstall {
        confirmation: ConfirmationMode,
    },
    Stop {
        confirmation: ConfirmationMode,
    },
    PairEnroll {
        fingerprint: Option<PublicFingerprint>,
        recover_account: bool,
    },
    PairAuthorize {
        request: PublicRequestId,
        fingerprint: PublicFingerprint,
    },
    PairList,
    Unpair {
        device: DeviceId,
        confirmation: ConfirmationMode,
    },
    RetryPendingUnpair,
    RemoteAccess {
        endpoints: Vec<SocketAddr>,
        confirmation: ConfirmationMode,
    },
    EndAllSessions {
        confirmation: ConfirmationMode,
    },
    UpdateCheck,
    UpdateStage {
        version: Option<StableVersion>,
        source: Option<crate::native::artifacts::LocalArtifactSource>,
    },
    UpdateActivate {
        installation: InstallationId,
        confirmation: ConfirmationMode,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestSelector {
    Protocol(OperationId),
    Native(spaceterm_host::native_deployment::contracts::NativeRequestId),
}

/// A public pairing-request comparison identity, resolved by the pairing port.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PublicRequestId([u8; 16]);
impl PublicRequestId {
    pub fn bytes(self) -> [u8; 16] {
        self.0
    }
}
impl fmt::Debug for PublicRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublicRequestId(..)")
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PublicFingerprint([u8; 32]);
impl PublicFingerprint {
    pub(crate) fn from_public_key(key: [u8; 32]) -> Self {
        Self(key)
    }
    pub fn bytes(self) -> [u8; 32] {
        self.0
    }
}
impl fmt::Debug for PublicFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublicFingerprint(..)")
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableVersion(String);
impl StableVersion {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Arguments exclude argv[0]. No native, credential, or transport call occurs.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<CliInvocation, CliFailure> {
    let mut owned = Vec::new();
    let mut total = 0usize;
    for argument in arguments {
        let bytes = argument.as_encoded_bytes().len();
        if owned.len() == MAX_ARGUMENTS
            || bytes > MAX_ARGUMENT_BYTES
            || bytes > MAX_TOTAL_ARGUMENT_BYTES - total
        {
            return Err(CliFailure::LimitExceeded);
        }
        total += bytes;
        owned.push(argument);
    }
    let args = owned
        .iter()
        .map(|arg| arg.to_str().ok_or(CliFailure::Usage))
        .collect::<Result<Vec<_>, _>>()?;
    let mut invocation = CliInvocation {
        json: false,
        non_interactive: false,
        command: CliCommand::Help,
    };
    let mut start = 0;
    while let Some(flag) = args.get(start) {
        match *flag {
            "--json" if !invocation.json => invocation.json = true,
            "--non-interactive" if !invocation.non_interactive => invocation.non_interactive = true,
            "--json" | "--non-interactive" => return Err(CliFailure::Usage),
            _ => break,
        }
        start += 1;
    }
    let args = &args[start..];
    let confirmation_flags = [("--assess", Flag::Switch), ("--confirm", Flag::Value)];
    invocation.command = match args {
        [] | ["--help"] | ["help"] => CliCommand::Help,
        ["--version"] => CliCommand::Version,
        ["serve"] => {
            if invocation.json || invocation.non_interactive {
                return Err(CliFailure::Usage);
            }
            CliCommand::Serve
        }
        ["status", rest @ ..] => {
            let options = Options::new(rest, &[("--operation", Flag::Value)])?;
            CliCommand::Status { operation: options.value("--operation").map(|value| {
                if let Some(bytes) = value.strip_prefix("native:") {
                    return spaceterm_host::native_deployment::contracts::NativeRequestId::new(parse_hex(bytes)?).map(RequestSelector::Native).map_err(|_| CliFailure::Usage);
                }
                parse_operation(value).map(RequestSelector::Protocol)
            }).transpose()? }
        }
        ["install", rest @ ..] => {
            let options = Options::new(
                rest,
                &[
                    ("--version", Flag::Value),
                    ("--feed", Flag::Value),
                    ("--archive", Flag::Value),
                ],
            )?;
            let source = options.artifact_source()?;
            CliCommand::Install {
                version: options.value("--version").map(parse_version).transpose()?,
                source,
            }
        }
        ["start"] => CliCommand::Start,
        ["restart", rest @ ..] => CliCommand::Restart {
            confirmation: Options::new(rest, &confirmation_flags)?
                .confirmation(invocation.non_interactive)?,
        },
        ["uninstall", rest @ ..] => CliCommand::Uninstall {
            confirmation: Options::new(rest, &confirmation_flags)?
                .confirmation(invocation.non_interactive)?,
        },
        ["stop", rest @ ..] => CliCommand::Stop {
            confirmation: Options::new(rest, &confirmation_flags)?
                .confirmation(invocation.non_interactive)?,
        },
        ["end-all-sessions", rest @ ..] => CliCommand::EndAllSessions {
            confirmation: Options::new(rest, &confirmation_flags)?
                .confirmation(invocation.non_interactive)?,
        },
        ["pair", "enroll", rest @ ..] => {
            let options = Options::new(
                rest,
                &[
                    ("--server-fingerprint", Flag::Value),
                    ("--recover-account", Flag::Switch),
                ],
            )?;
            let fingerprint = options
                .value("--server-fingerprint")
                .map(|s| parse_hex(s).map(PublicFingerprint))
                .transpose()?;
            if invocation.non_interactive && fingerprint.is_none() {
                return Err(CliFailure::ConfirmationRequired);
            }
            CliCommand::PairEnroll {
                fingerprint,
                recover_account: options.present("--recover-account"),
            }
        }
        ["pair", "authorize", request, rest @ ..] => {
            let options = Options::new(rest, &[("--device-fingerprint", Flag::Value)])?;
            CliCommand::PairAuthorize {
                request: PublicRequestId(parse_hex(request)?),
                fingerprint: PublicFingerprint(parse_hex(
                    options.required("--device-fingerprint")?,
                )?),
            }
        }
        ["pair", "list"] => CliCommand::PairList,
        ["unpair", rest @ ..] => {
            let options = Options::new(
                rest,
                &[
                    ("--device", Flag::Value),
                    ("--retry-pending", Flag::Switch),
                    ("--assess", Flag::Switch),
                    ("--confirm", Flag::Value),
                ],
            )?;
            if options.present("--retry-pending") {
                if options.entries.len() != 1 {
                    return Err(CliFailure::Usage);
                }
                CliCommand::RetryPendingUnpair
            } else {
                CliCommand::Unpair {
                    device: DeviceId::new(parse_hex(options.required("--device")?)?),
                    confirmation: options.confirmation(invocation.non_interactive)?,
                }
            }
        }
        ["remote-access", direction, rest @ ..] => {
            let enable = match *direction {
                "enable" => true,
                "disable" => false,
                _ => return Err(CliFailure::Usage),
            };
            let options = Options::new(
                rest,
                &[
                    ("--bind", Flag::RepeatedValue),
                    ("--assess", Flag::Switch),
                    ("--confirm", Flag::Value),
                ],
            )?;
            let mut endpoints = Vec::new();
            for value in options
                .entries
                .iter()
                .filter_map(|(key, value)| (*key == "--bind").then_some(*value).flatten())
            {
                let endpoint: SocketAddr = value.parse().map_err(|_| CliFailure::Usage)?;
                if !enable || endpoints.contains(&endpoint) {
                    return Err(CliFailure::Usage);
                }
                if endpoints.len() == spaceterm_core::remote_access::MAX_REMOTE_BIND_ENDPOINTS {
                    return Err(CliFailure::LimitExceeded);
                }
                endpoints.push(endpoint);
            }
            if enable && endpoints.is_empty() {
                return Err(CliFailure::Usage);
            }
            CliCommand::RemoteAccess {
                endpoints,
                confirmation: options.confirmation(invocation.non_interactive)?,
            }
        }
        ["update", "check"] => CliCommand::UpdateCheck,
        ["update", "stage", rest @ ..] => {
            let options = Options::new(
                rest,
                &[
                    ("--version", Flag::Value),
                    ("--feed", Flag::Value),
                    ("--archive", Flag::Value),
                ],
            )?;
            let version = options.value("--version").map(parse_version).transpose()?;
            CliCommand::UpdateStage {
                version,
                source: options.artifact_source()?,
            }
        }
        ["update", "activate", rest @ ..] => {
            let options = Options::new(
                rest,
                &[
                    ("--installation", Flag::Value),
                    ("--assess", Flag::Switch),
                    ("--confirm", Flag::Value),
                ],
            )?;
            let installation = InstallationId(
                std::num::NonZeroU64::new(decimal(options.required("--installation")?)?)
                    .ok_or(CliFailure::Usage)?,
            );
            CliCommand::UpdateActivate {
                installation,
                confirmation: options.confirmation(invocation.non_interactive)?,
            }
        }
        _ => return Err(CliFailure::Usage),
    };
    Ok(invocation)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Flag {
    Switch,
    Value,
    RepeatedValue,
}
struct Options<'a> {
    entries: Vec<(&'a str, Option<&'a str>)>,
}
impl<'a> Options<'a> {
    fn new(args: &[&'a str], allowed: &[(&str, Flag)]) -> Result<Self, CliFailure> {
        let mut entries = Vec::new();
        let mut args = args.iter();
        while let Some(&key) = args.next() {
            let kind = allowed
                .iter()
                .find_map(|(name, flag)| (*name == key).then_some(*flag))
                .ok_or(CliFailure::Usage)?;
            if kind != Flag::RepeatedValue && entries.iter().any(|(old, _)| *old == key) {
                return Err(CliFailure::Usage);
            }
            let value = if kind == Flag::Switch {
                None
            } else {
                let &value = args.next().ok_or(CliFailure::Usage)?;
                if value.starts_with("--") {
                    return Err(CliFailure::Usage);
                }
                Some(value)
            };
            entries.push((key, value));
        }
        Ok(Self { entries })
    }
    fn present(&self, key: &str) -> bool {
        self.entries.iter().any(|(name, _)| *name == key)
    }
    fn value(&self, key: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .find_map(|(name, value)| (*name == key).then_some(*value).flatten())
    }
    fn required(&self, key: &str) -> Result<&'a str, CliFailure> {
        self.value(key).ok_or(CliFailure::Usage)
    }
    fn artifact_source(
        &self,
    ) -> Result<Option<crate::native::artifacts::LocalArtifactSource>, CliFailure> {
        match (self.value("--feed"), self.value("--archive")) {
            (None, None) => Ok(None),
            (Some(feed), Some(archive)) => Ok(Some(
                crate::native::artifacts::LocalArtifactSource::new(feed.into(), archive.into())?,
            )),
            _ => Err(CliFailure::Usage),
        }
    }
    fn confirmation(&self, non_interactive: bool) -> Result<ConfirmationMode, CliFailure> {
        match (self.present("--assess"), self.value("--confirm")) {
            (true, Some(_)) => Err(CliFailure::Usage),
            (true, None) => Ok(ConfirmationMode::Assess),
            (false, Some(token)) => Ok(ConfirmationMode::Token(AssessmentToken::new(
                token.to_owned(),
            )?)),
            (false, None) if non_interactive => Err(CliFailure::ConfirmationRequired),
            _ => Ok(ConfirmationMode::Interactive),
        }
    }
}
fn parse_version(value: &str) -> Result<StableVersion, CliFailure> {
    spaceterm_distribution::StableVersion::parse(value).map_err(|_| CliFailure::Usage)?;
    Ok(StableVersion(value.to_owned()))
}
fn decimal(value: &str) -> Result<u64, CliFailure> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CliFailure::Usage);
    }
    value.parse().map_err(|_| CliFailure::Usage)
}
pub(crate) fn parse_hex<const N: usize>(value: &str) -> Result<[u8; N], CliFailure> {
    if value.len() != N * 2 {
        return Err(CliFailure::Usage);
    }
    let mut bytes = [0; N];
    for (output, pair) in bytes.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        let nibble = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(CliFailure::Usage),
        };
        *output = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    if bytes == [0; N] {
        return Err(CliFailure::Usage);
    }
    Ok(bytes)
}
fn parse_operation(value: &str) -> Result<OperationId, CliFailure> {
    use spaceterm_core::{
        identity::{ClientId, OperationStreamId, OperationStreamKey, PairingId, ServerId},
        operations::OperationSequence,
    };
    let parts = value.split(':').collect::<Vec<_>>();
    let [server, pairing, client, stream, sequence] = parts.as_slice() else {
        return Err(CliFailure::Usage);
    };
    let server = ServerId::new(parse_hex(server)?);
    let pairing = PairingId::new(server, decimal(pairing)?).map_err(|_| CliFailure::Usage)?;
    let client = ClientId::new(server, decimal(client)?).map_err(|_| CliFailure::Usage)?;
    let key = OperationStreamKey::new(server, decimal(stream)?).map_err(|_| CliFailure::Usage)?;
    let stream = OperationStreamId::new(key, pairing, client).map_err(|_| CliFailure::Usage)?;
    Ok(OperationId::new(
        stream,
        OperationSequence::new(decimal(sequence)?).map_err(|_| CliFailure::Usage)?,
    ))
}
