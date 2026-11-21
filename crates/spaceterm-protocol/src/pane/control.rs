//! Which client controls a Pane, as bounded presentation metadata. None of it grants authority.
use serde::{Deserialize, Serialize};

/// The most characters a Client Name keeps.
pub const MAX_CLIENT_NAME_CHARS: usize = 48;
const FALLBACK: &str = "Another SpaceTerm";

/// A person-readable name a client supplies at admission, such as its computer's name.
///
/// Every value is printable text of at most [`MAX_CLIENT_NAME_CHARS`] characters, without control,
/// format, or bidirectional characters. Both peers apply these bounds to every value they receive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct ClientName(String);

impl ClientName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Composes the name from a device-name reading and optional development label.
    pub fn for_computer(computer: Option<&str>, development: Option<&str>) -> Self {
        match (computer, development) {
            (Some(computer), Some(development)) => format!("{computer} ({development})").into(),
            (Some(computer), None) => computer.into(),
            (None, Some(development)) => development.into(),
            (None, None) => "".into(),
        }
    }
}

impl ClientName {
    /// The CLI's name: `<computer> CLI`, or `<computer> CLI <agent>`. Only the computer's name is
    /// shortened to fit, so " CLI " and the agent name stay whole.
    pub fn for_cli(computer: Option<&str>, agent: Option<&crate::protocol::AgentName>) -> Self {
        let suffix = agent.map_or_else(|| " CLI".to_owned(), |agent| agent.name_suffix());
        let budget = MAX_CLIENT_NAME_CHARS - suffix.chars().count();
        let computer = computer
            .map(|name| Self::from(name).0)
            .filter(|name| name != FALLBACK)
            .unwrap_or_default();
        let mut kept: Vec<char> = computer.chars().collect();
        if kept.len() > budget {
            kept.truncate(budget - 1);
            while kept.last() == Some(&' ') {
                kept.pop();
            }
            kept.push('\u{2026}');
        }
        let computer: String = kept.into_iter().collect();
        Self::from(format!("{computer}{suffix}").trim_start())
    }
}

impl From<String> for ClientName {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<&str> for ClientName {
    fn from(value: &str) -> Self {
        let mut kept = Vec::with_capacity(MAX_CLIENT_NAME_CHARS + 1);
        let mut space = false;
        for c in value.chars() {
            if kept.len() > MAX_CLIENT_NAME_CHARS {
                break;
            }
            if c.is_whitespace() {
                space = !kept.is_empty();
            } else if !c.is_control() && !invisible(c) {
                if space {
                    kept.push(' ');
                    space = false;
                }
                kept.push(c);
            }
        }
        if kept.len() > MAX_CLIENT_NAME_CHARS {
            kept.truncate(MAX_CLIENT_NAME_CHARS - 1);
            while kept.last() == Some(&' ') {
                kept.pop();
            }
            kept.push('\u{2026}');
        }
        if kept.is_empty() {
            return Self(FALLBACK.to_owned());
        }
        Self(kept.into_iter().collect())
    }
}

impl From<ClientName> for String {
    fn from(value: ClientName) -> Self {
        value.0
    }
}

/// Format characters that reorder or hide text, which a name never needs. The zero-width
/// non-joiner and joiner stay, because Persian and Indic words and emoji sequences need them.
pub(super) fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200E}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// Who controls a Pane, as the server tells a client that does not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Controller {
    /// No client controls the Pane.
    Nobody,
    /// A client controls the Pane. `this_client` is whether it has the asking client's Client
    /// Identity, which holds control through an earlier Workspace Attachment.
    Client { name: ClientName, this_client: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn received(name: &str) -> String {
        let wire = serde_json::to_string(name).unwrap();
        serde_json::from_str::<ClientName>(&wire)
            .unwrap()
            .as_str()
            .to_owned()
    }

    #[test]
    fn a_received_name_keeps_printable_text_and_drops_controls_and_reordering() {
        assert_eq!(received("Sadik’s MacBook Pro"), "Sadik’s MacBook Pro");
        assert_eq!(
            received("  build\u{1b}[31m\tbox\n\u{202E}reversed\u{2066}\u{200B}  "),
            "build[31m box reversed"
        );
        assert_eq!(received("\u{7}\u{202E} \n"), "Another SpaceTerm");
    }

    /// Joiners shape Persian and Indic words and emoji sequences, so a name keeps them.
    #[test]
    fn a_received_name_keeps_joiners_that_shape_its_text() {
        assert_eq!(
            received("\u{645}\u{6CC}\u{200C}\u{62E}\u{648}\u{627}\u{647}\u{645}"),
            "\u{645}\u{6CC}\u{200C}\u{62E}\u{648}\u{627}\u{647}\u{645}"
        );
        assert_eq!(
            received("Lab \u{1F469}\u{200D}\u{1F4BB}"),
            "Lab \u{1F469}\u{200D}\u{1F4BB}"
        );
    }

    #[test]
    fn a_long_name_is_cut_to_the_bound_with_an_ellipsis() {
        let name = received(&"a".repeat(1000));
        assert_eq!(name.chars().count(), MAX_CLIENT_NAME_CHARS);
        assert!(name.ends_with('\u{2026}'));
        assert_eq!(
            received(&"é".repeat(MAX_CLIENT_NAME_CHARS)).chars().count(),
            48
        );
        assert!(!received(&"é".repeat(MAX_CLIENT_NAME_CHARS)).ends_with('\u{2026}'));
        let spaced = received(&format!("{} tail", "b".repeat(MAX_CLIENT_NAME_CHARS - 1)));
        assert_eq!(
            spaced,
            format!("{}\u{2026}", "b".repeat(MAX_CLIENT_NAME_CHARS - 1))
        );
    }

    #[test]
    fn a_cli_name_shortens_only_the_computer_and_keeps_the_agent_whole() {
        let agent: crate::protocol::AgentName = "a".repeat(32).parse().unwrap();
        let mac = Some("Sadik’s MacBook Pro");
        assert_eq!(
            ClientName::for_cli(mac, None).as_str(),
            "Sadik’s MacBook Pro CLI"
        );
        let name = ClientName::for_cli(mac, Some(&agent));
        assert_eq!(name.as_str().chars().count(), MAX_CLIENT_NAME_CHARS);
        assert_eq!(
            name.as_str(),
            format!("Sadik’s Ma\u{2026} CLI {}", "a".repeat(32))
        );
        assert_eq!(
            received(name.as_str()),
            name.as_str(),
            "the server keeps it unchanged"
        );
        let short: crate::protocol::AgentName = "agent-1".parse().unwrap();
        assert_eq!(
            ClientName::for_cli(None, Some(&short)).as_str(),
            "CLI agent-1"
        );
    }
}
