//! Routes operating-system invitation deliveries to Add Server without granting authority.
//!
//! The sheet validates the complete invitation and requires Join. This boundary only admits
//! the application URL route and bounds transient input; it never logs or retains the link.

const MAXIMUM_LINK_BYTES: usize = 16 * 1024;

pub(crate) fn is_pairing_link(value: &str) -> bool {
    value.len() <= MAXIMUM_LINK_BYTES
        && value.starts_with("spaceterm://pair/")
        && value.len() > "spaceterm://pair/".len()
        && !value.chars().any(char::is_control)
}

/// A desktop launch opens at most one invitation sheet. No link enters process diagnostics.
pub(crate) fn from_arguments(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<String> {
    arguments
        .into_iter()
        .filter_map(|argument| argument.into_string().ok())
        .find(|argument| is_pairing_link(argument))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_arguments_select_an_invitation_without_treating_options_as_links() {
        let arguments = [
            "--new-instance",
            "https://example.com/",
            "spaceterm://pair/claim",
        ];
        assert_eq!(
            from_arguments(arguments.map(std::ffi::OsString::from)).as_deref(),
            Some("spaceterm://pair/claim")
        );
    }

    #[test]
    fn other_routes_and_unbounded_or_control_input_cannot_open_an_invitation() {
        for value in [
            "spaceterm://pair/",
            "spaceterm://other/claim",
            "https://pair/claim",
            "spaceterm://pair/claim\n",
            &format!("spaceterm://pair/{}", "a".repeat(MAXIMUM_LINK_BYTES)),
        ] {
            assert!(!is_pairing_link(value));
        }
    }
}
