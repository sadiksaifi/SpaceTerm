//! The Core's admitted Client Name cache.
pub use spaceterm_protocol::pane::control::*;

/// This computer's bounded display name, using the native computer-name adapter.
#[cfg(feature = "client")]
pub fn this_computer() -> ClientName {
    use crate::computer_name::{ComputerName, SystemComputerName};
    ClientName::for_computer(SystemComputerName.computer_name().as_deref(), None)
}

/// How many Client Identities a server remembers a name for.
pub const MAX_CLIENT_NAMES: usize = 256;

/// The latest Client Name of each Client Identity a server admitted while it runs.
#[derive(Debug, Default)]
pub struct ClientNames(Vec<([u8; 32], ClientName)>);

impl ClientNames {
    /// Records the name a client supplied at admission. Once full, the name admitted least
    /// recently is forgotten, and a client it belonged to reads as the neutral fallback.
    pub fn admit(&mut self, client: [u8; 32], name: ClientName) {
        self.0.retain(|(known, _)| *known != client);
        if self.0.len() == MAX_CLIENT_NAMES {
            self.0.remove(0);
        }
        self.0.push((client, name));
    }

    /// The latest bounded Client Name, or the neutral fallback for a forgotten identity.
    pub fn name(&self, client: [u8; 32]) -> ClientName {
        self.0
            .iter()
            .find(|(known, _)| *known == client)
            .map_or_else(|| ClientName::from(""), |(_, name)| name.clone())
    }

    /// Who controls a Pane as `asking` sees it, given the controlling Client Identity.
    pub fn controller(&self, controlling: Option<[u8; 32]>, asking: [u8; 32]) -> Controller {
        let Some(controlling) = controlling else {
            return Controller::Nobody;
        };
        let name = self.name(controlling);
        Controller::Client {
            name,
            this_client: controlling == asking,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_name_of_a_bounded_set_of_clients_names_their_control() {
        let mut names = ClientNames::default();
        names.admit([1; 32], "First".into());
        names.admit([1; 32], "Renamed".into());
        assert_eq!(names.controller(None, [2; 32]), Controller::Nobody);
        assert_eq!(
            names.controller(Some([1; 32]), [2; 32]),
            Controller::Client {
                name: "Renamed".into(),
                this_client: false
            }
        );
        assert_eq!(
            names.controller(Some([1; 32]), [1; 32]),
            Controller::Client {
                name: "Renamed".into(),
                this_client: true
            }
        );
        for client in 2..=MAX_CLIENT_NAMES as u16 {
            let mut id = [0; 32];
            id[..2].copy_from_slice(&client.to_be_bytes());
            names.admit(id, format!("Client {client}").into());
        }
        names.admit([9; 32], "Newest".into());
        assert_eq!(
            names.controller(Some([1; 32]), [9; 32]),
            Controller::Client {
                name: ClientName::from(""),
                this_client: false
            },
            "the least recently admitted name is forgotten first"
        );
    }
}
