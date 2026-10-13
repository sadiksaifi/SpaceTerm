//! Strict executable dispatch. Queries never enter service startup or setup.

use std::ffi::OsString;

use spaceterm_distribution::{DistributionFailure, StorageCompatibility};

use crate::account_startup::StartupFailure;

pub mod owner;
pub mod setup;
mod stdio;

pub use stdio::{run_native, run_native_with_supervisor};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerInvocation {
    Serve,
    DistributionMetadata,
    Help,
    Version,
    Status,
    ReserveNativeRequest,
    OwnerEnrollment,
    SetupConfirmed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidServerArguments;

impl ServerInvocation {
    /// Accepts exactly "serve" for service startup, or one query operation.
    /// Status alone accepts unique leading bootstrap output/interaction flags.
    /// Mutations have distinct deliberate entry points. Setup requires confirmation.
    pub fn parse(
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, InvalidServerArguments> {
        let mut arguments = arguments.into_iter();
        let mut json = false;
        let mut non_interactive = false;
        let invocation = loop {
            match arguments.next() {
                Some(argument) if argument == "--json" && !json => json = true,
                Some(argument) if argument == "--non-interactive" && !non_interactive => {
                    non_interactive = true;
                }
                Some(argument) if argument == "status" => break Self::Status,
                Some(argument) if !json && !non_interactive => {
                    break match argument.to_str() {
                        Some("serve") => Self::Serve,
                        Some("reserve-native-request") => Self::ReserveNativeRequest,
                        Some("owner-enrollment") => Self::OwnerEnrollment,
                        Some("setup")
                            if arguments.next().as_deref()
                                == Some(std::ffi::OsStr::new("--confirm")) =>
                        {
                            Self::SetupConfirmed
                        }
                        Some("--distribution-metadata") => Self::DistributionMetadata,
                        Some("--help") => Self::Help,
                        Some("--version") => Self::Version,
                        _ => return Err(InvalidServerArguments),
                    };
                }
                _ => return Err(InvalidServerArguments),
            }
        };
        if arguments.next().is_some() {
            return Err(InvalidServerArguments);
        }
        Ok(invocation)
    }
}

pub const USAGE: &str = "usage: spaceterm-server serve | [--json] [--non-interactive] status | reserve-native-request | owner-enrollment | setup --confirm | --distribution-metadata | --help | --version";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryFailure {
    Distribution(DistributionFailure),
    Account(StartupFailure),
}
impl QueryFailure {
    pub const fn classification(self) -> &'static str {
        match self {
            Self::Distribution(_) => "server distribution metadata unavailable",
            Self::Account(failure) => failure.classification(),
        }
    }
}

/// Help, version and artifact metadata require no account or credential access.
/// Status only reads existing account registration and cannot enter serve/setup.
pub fn query_output(invocation: ServerInvocation) -> Result<Option<Vec<u8>>, QueryFailure> {
    match invocation {
        ServerInvocation::Serve
        | ServerInvocation::ReserveNativeRequest
        | ServerInvocation::OwnerEnrollment
        | ServerInvocation::SetupConfirmed => Ok(None),
        ServerInvocation::Status => crate::account_startup::production_status()
            .map(Some)
            .map_err(QueryFailure::Account),
        ServerInvocation::DistributionMetadata => distribution_metadata()
            .map(Some)
            .map_err(QueryFailure::Distribution),
        ServerInvocation::Help | ServerInvocation::Version => {
            let version =
                crate::distribution_metadata::version().map_err(QueryFailure::Distribution)?;
            let mut output = format!("spaceterm-server {}\n", version.as_str());
            if invocation == ServerInvocation::Help {
                output.push_str(USAGE);
                output.push('\n');
            }
            Ok(Some(output.into_bytes()))
        }
    }
}

pub fn distribution_metadata() -> Result<Vec<u8>, DistributionFailure> {
    let mut majors = spaceterm_protocol::negotiation::SUPPORTED_MAJORS
        .into_iter()
        .map(|major| u16::try_from(major).map_err(|_| DistributionFailure::InvalidRecord))
        .collect::<Result<Vec<_>, _>>()?;
    majors.sort_unstable();
    crate::distribution_metadata::encode(
        majors,
        StorageCompatibility {
            read: vec![spaceterm_host::organization_store::schema::SCHEMA_VERSION],
            write: spaceterm_host::organization_store::schema::SCHEMA_VERSION,
            migrate_from: Vec::new(),
        },
    )
}
