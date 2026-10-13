//! Strict executable dispatch. Metadata selection precedes account effects.

use std::ffi::OsString;

use spaceterm_distribution::{DistributionFailure, StorageCompatibility};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerInvocation {
    Serve,
    DistributionMetadata,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidServerArguments;

impl ServerInvocation {
    /// Accepts exactly "serve" for service startup, or one metadata flag.
    /// An empty invocation cannot start an unsupervised server.
    pub fn parse(
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, InvalidServerArguments> {
        let mut arguments = arguments.into_iter();
        let invocation = match arguments.next() {
            Some(argument) if argument == "serve" => Self::Serve,
            Some(argument) if argument == "--distribution-metadata" => Self::DistributionMetadata,
            _ => return Err(InvalidServerArguments),
        };
        if arguments.next().is_some() {
            return Err(InvalidServerArguments);
        }
        Ok(invocation)
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
