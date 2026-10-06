use gpui::http_client::Url;

use crate::terminal::{DiagnosticBundle, TerminalFailure};

const ISSUE_URL: &str = "https://github.com/sadiksaifi/SpaceTerm/issues/new";
// Query escaping can triple the byte count. Keep the draft small enough for browsers and GitHub.
const MAX_DIAGNOSTIC_BYTES: usize = 2_000;

pub(super) fn issue_url(diagnostics: &DiagnosticBundle, failure: &TerminalFailure) -> String {
    let title = format!(
        "[Terminal failure] {}: {}",
        failure.class(),
        failure.operation()
    );
    let reason = failure
        .reason()
        .map_or_else(|| "Unspecified".to_owned(), |reason| format!("{reason:?}"));
    let body = format!(
        "## Terminal failure\n\n{failure}\n\nReason: {reason}\n\nAdd reproduction steps if you can.\n\n## Diagnostics\n\n```text\n{}```\n",
        diagnostics.report_text(MAX_DIAGNOSTIC_BYTES),
    );
    Url::parse_with_params(
        ISSUE_URL,
        [("title", title.as_str()), ("body", body.as_str())],
    )
    .map(|url| url.to_string())
    .unwrap_or_else(|_| ISSUE_URL.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_reports_keep_the_latest_failure_and_omit_whole_older_records() {
        let mut diagnostics = DiagnosticBundle::default();
        for _ in 0..200 {
            diagnostics.record_unhandled_key(crate::terminal::UnhandledKeyDiagnostic::new(
                crate::terminal::DiagnosticKeyEventKind::FlagsChanged,
                crate::terminal::KeyAction::Press,
                Some(55),
            ));
        }
        let failure = TerminalFailure::emulator_error(
            "produce-terminal-screen-snapshot",
            libghostty_vt::Error::OutOfMemory,
        );
        diagnostics.record(&failure);
        let url = issue_url(&diagnostics, &failure);
        assert!(url.len() <= 8_000);
        let url = Url::parse(&url).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query.len(), 2);
        assert_eq!(
            query.get("title").unwrap(),
            "[Terminal failure] Terminal Emulator: produce-terminal-screen-snapshot"
        );
        let body = query.get("body").unwrap();
        assert!(body.contains(&format!("build_version={}\n", env!("SPACETERM_VERSION"))));
        assert!(body.contains("network_telemetry=false\nterminal_content=false\n"));
        let omitted: usize = body
            .lines()
            .find_map(|line| line.strip_prefix("records_omitted="))
            .unwrap()
            .parse()
            .unwrap();
        assert!(omitted > 72);
        let records: Vec<_> = body
            .lines()
            .filter(|line| line.starts_with("sequence="))
            .collect();
        assert_eq!(records.len(), 201 - omitted);
        assert!(
            records
                .last()
                .unwrap()
                .starts_with("sequence=201 elapsed_ms=")
        );
        assert!(records.last().unwrap().ends_with("class=Emulator recoverability=Fatal operation=produce-terminal-screen-snapshot reason=OutOfMemory"));
        assert!(body.ends_with("\n```\n"));
    }
}
