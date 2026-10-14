//! Desktop and headless connections share checked organization publication conversion.
use super::*;

pub(super) fn decode(payload: Payload) -> Result<OrganizationPublication, ClientFailure> {
    spaceterm_client::organization_protocol::decode(payload)
}
