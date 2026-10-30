use super::*;
use crate::contract::TargetStep;
use crate::shell::quote;
use std::fmt::Write;

/// Diagnostics report only bounded classifications and IDs, even in verbose mode.
pub(super) fn verbose(steps: &[TargetStep]) -> String {
    let mut output = String::new();
    for step in steps {
        let level = name(step.level);
        let source = match step.source {
            Source::Flag => "flag",
            Source::Current => "current",
            Source::Only => "only",
            Source::Root => "root",
            Source::Created => "created",
        };
        let _ = write!(output, "target {level}: {source}");
        if let Some(id) = &step.id {
            let _ = write!(output, " id:{id}");
        }
        output.push('\n');
    }
    output
}

/// Candidate values belong on stdout. Error messages are rendered separately from `Failure`.
pub(super) fn data_text(data: &TargetData) -> String {
    let mut output = String::new();
    match data {
        TargetData::None => {}
        TargetData::Candidates(data) => {
            for candidate in &data.candidates {
                let _ = write!(output, "{}", candidate.label);
                if let Some(id) = &candidate.id {
                    let _ = write!(output, " --{} id:{id}", name(candidate.level));
                } else if let Some(path) = &candidate.path {
                    let _ = write!(
                        output,
                        " --{} {}",
                        name(candidate.level),
                        quote(&format!("path:{path}"))
                    );
                }
                if let Some(directory) = &candidate.directory {
                    let _ = write!(output, "\n  {directory}");
                }
                output.push('\n');
            }
        }
        TargetData::Mismatch(data) => {
            let _ = writeln!(
                output,
                "actual {}: {}",
                name(data.actual.level),
                data.actual.label
            );
            if let Some(id) = &data.actual.id {
                let _ = writeln!(output, "  --{} id:{id}", name(data.actual.level));
            }
            if let Some(path) = &data.actual.path {
                let _ = writeln!(
                    output,
                    "  --{} {}",
                    name(data.actual.level),
                    quote(&format!("path:{path}"))
                );
            }
        }
    }
    output
}

fn name(level: Level) -> &'static str {
    match level {
        Level::Workspace => "workspace",
        Level::Worktree => "worktree",
        Level::Tab => "tab",
        Level::Pane => "pane",
        _ => "server",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Envelope, ErrorBody};

    #[test]
    fn titles_and_paths_are_operational_data_in_text_and_json() {
        let marker = "private-title-and-path";
        let error = TargetError::Ambiguous {
            level: Level::Pane,
            candidates: vec![Candidate {
                level: Level::Pane,
                id: Some("ab".repeat(32)),
                path: None,
                label: marker.into(),
                directory: Some(format!("/{marker}")),
                worktree: Some(format!("/{marker}")),
                panes: 1,
            }],
        };
        let (failure, data) = error.into_parts();
        assert!(!failure.text().contains(marker));
        assert!(
            !serde_json::to_string(&ErrorBody::from(failure.clone()))
                .unwrap()
                .contains(marker)
        );
        assert!(data_text(&data).contains(marker));
        let TargetData::Candidates(data) = data else {
            panic!("expected candidates")
        };
        let envelope = Envelope::error("pane.close", None, failure, data);
        let json = serde_json::to_value(envelope).unwrap();
        assert!(!json["error"].to_string().contains(marker));
        assert!(json["data"].to_string().contains(marker));
        assert!(
            !verbose(&[TargetStep {
                level: Level::Worktree,
                id: None,
                path: Some(format!("/{marker}")),
                label: Some(marker.into()),
                source: Source::Flag
            }])
            .contains(marker)
        );
    }
}
