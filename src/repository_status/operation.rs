//! Unfinished git operations from the marker files in a git directory.

use super::{
    ApplyMarkers, MAXIMUM_MARKER_BYTES, OperationMarkers, OperationStep, RepositoryOperation,
    StepMarkers,
};

/// The operation the markers describe, in Starship's `git_state` precedence.
pub(crate) fn repository_operation(markers: &OperationMarkers) -> Option<RepositoryOperation> {
    if let Some(rebase) = &markers.rebase_merge {
        return Some(RepositoryOperation::Rebasing {
            step: operation_step(rebase),
        });
    }
    if let Some(ApplyMarkers {
        step,
        rebasing,
        applying,
    }) = &markers.rebase_apply
    {
        let step = operation_step(step);
        return Some(if *applying && !*rebasing {
            RepositoryOperation::Applying { step }
        } else {
            RepositoryOperation::Rebasing { step }
        });
    }
    [
        (markers.merge_head, RepositoryOperation::Merging),
        (markers.revert_head, RepositoryOperation::Reverting),
        (markers.cherry_pick_head, RepositoryOperation::CherryPicking),
        (markers.bisect_log, RepositoryOperation::Bisecting),
    ]
    .into_iter()
    .find_map(|(present, operation)| present.then_some(operation))
}

/// A step is shown only when both numbers are positive and the current one is within the total.
fn operation_step(markers: &StepMarkers) -> Option<OperationStep> {
    let current = step_number(markers.current.as_deref())?;
    let total = step_number(markers.total.as_deref())?;
    (current <= total).then_some(OperationStep { current, total })
}

fn step_number(contents: Option<&[u8]>) -> Option<u32> {
    let contents = contents.filter(|contents| contents.len() <= MAXIMUM_MARKER_BYTES)?;
    let digits = contents.trim_ascii();
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits)
        .ok()?
        .parse()
        .ok()
        .filter(|number| *number > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(current: &str, total: &str) -> StepMarkers {
        StepMarkers {
            current: Some(current.as_bytes().to_vec()),
            total: Some(total.as_bytes().to_vec()),
        }
    }

    fn apply(rebasing: bool, applying: bool) -> ApplyMarkers {
        ApplyMarkers {
            step: step("2\n", "5\n"),
            rebasing,
            applying,
        }
    }

    fn all_markers() -> OperationMarkers {
        OperationMarkers {
            rebase_merge: Some(step("3\n", "7\n")),
            rebase_apply: Some(apply(false, true)),
            merge_head: true,
            revert_head: true,
            cherry_pick_head: true,
            bisect_log: true,
        }
    }

    #[test]
    fn no_markers_should_mean_no_operation() {
        assert_eq!(repository_operation(&OperationMarkers::default()), None);
    }

    #[test]
    fn markers_should_follow_starship_precedence() {
        let removals: [fn(&mut OperationMarkers); 6] = [
            |markers| markers.rebase_merge = None,
            |markers| markers.rebase_apply = None,
            |markers| markers.merge_head = false,
            |markers| markers.revert_head = false,
            |markers| markers.cherry_pick_head = false,
            |markers| markers.bisect_log = false,
        ];
        let mut markers = all_markers();
        let mut seen = Vec::new();
        for remove in removals {
            seen.extend(repository_operation(&markers));
            remove(&mut markers);
        }

        let applying_step = Some(OperationStep {
            current: 2,
            total: 5,
        });
        assert_eq!(
            seen,
            [
                RepositoryOperation::Rebasing {
                    step: Some(OperationStep {
                        current: 3,
                        total: 7
                    })
                },
                RepositoryOperation::Applying { step: applying_step },
                RepositoryOperation::Merging,
                RepositoryOperation::Reverting,
                RepositoryOperation::CherryPicking,
                RepositoryOperation::Bisecting,
            ]
        );
    }

    #[test]
    fn rebase_apply_should_distinguish_rebase_from_am() {
        let step = Some(OperationStep {
            current: 2,
            total: 5,
        });
        for (rebasing, applying, expected) in [
            (true, false, RepositoryOperation::Rebasing { step }),
            (false, true, RepositoryOperation::Applying { step }),
            (false, false, RepositoryOperation::Rebasing { step }),
            (true, true, RepositoryOperation::Rebasing { step }),
        ] {
            let markers = OperationMarkers {
                rebase_apply: Some(apply(rebasing, applying)),
                ..OperationMarkers::default()
            };

            assert_eq!(
                repository_operation(&markers),
                Some(expected),
                "rebasing {rebasing}, applying {applying}"
            );
        }
    }

    #[test]
    fn steps_should_trim_ascii_whitespace() {
        let markers = OperationMarkers {
            rebase_merge: Some(step(" \t12\r\n", "\n40 ")),
            ..OperationMarkers::default()
        };

        assert_eq!(
            repository_operation(&markers),
            Some(RepositoryOperation::Rebasing {
                step: Some(OperationStep {
                    current: 12,
                    total: 40
                })
            })
        );
    }

    #[test]
    fn invalid_steps_should_drop_the_step_but_keep_the_operation() {
        let oversized = "1".repeat(MAXIMUM_MARKER_BYTES + 1);
        for (current, total) in [
            ("0", "3"),
            ("1", "0"),
            ("4", "3"),
            ("x", "3"),
            ("1", "three"),
            ("-1", "3"),
            ("+1", "3"),
            ("1 2", "3"),
            ("", "3"),
            ("   ", "3"),
            ("99999999999", "99999999999"),
            ("1", oversized.as_str()),
            ("\u{0661}", "3"),
        ] {
            let markers = OperationMarkers {
                rebase_merge: Some(step(current, total)),
                ..OperationMarkers::default()
            };

            assert_eq!(
                repository_operation(&markers),
                Some(RepositoryOperation::Rebasing { step: None }),
                "{current:?} of {total:?}"
            );
        }
    }

    #[test]
    fn missing_step_files_should_drop_the_step() {
        let markers = OperationMarkers {
            rebase_apply: Some(ApplyMarkers {
                step: StepMarkers {
                    current: Some(b"1".to_vec()),
                    total: None,
                },
                rebasing: false,
                applying: true,
            }),
            ..OperationMarkers::default()
        };

        assert_eq!(
            repository_operation(&markers),
            Some(RepositoryOperation::Applying { step: None })
        );
    }

    #[test]
    fn a_step_equal_to_the_total_should_be_kept() {
        let markers = OperationMarkers {
            rebase_merge: Some(step("7", "7")),
            ..OperationMarkers::default()
        };

        assert_eq!(
            repository_operation(&markers),
            Some(RepositoryOperation::Rebasing {
                step: Some(OperationStep {
                    current: 7,
                    total: 7
                })
            })
        );
    }
}
