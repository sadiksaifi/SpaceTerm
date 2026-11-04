//! The state of a Release Core's feed checks: the `check` of its update facts. The update engine
//! persists it with the rest of the update record, so a stopped Core reports the last check.
use super::facts::{CheckFailure, UpdateCheck};

/// The state after one more finished check: `Succeeded` at `at`, or `Failing` since the first
/// failure after the last success, with this failure's reason.
pub fn next(previous: &UpdateCheck, at: u64, outcome: Result<(), CheckFailure>) -> UpdateCheck {
    let Err(reason) = outcome else {
        return UpdateCheck::Succeeded { at };
    };
    let (since, last_success) = match *previous {
        UpdateCheck::Failing {
            since,
            last_success,
            ..
        } => (since, last_success),
        UpdateCheck::Succeeded { at: success } => (at, Some(success)),
        UpdateCheck::Never | UpdateCheck::Disabled => (at, None),
    };
    UpdateCheck::Failing {
        since,
        last_success,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_keep_their_first_time_and_the_last_success() {
        let never = UpdateCheck::Never;
        let failing = next(&never, 10, Err(CheckFailure::Network));
        assert_eq!(
            failing,
            UpdateCheck::Failing {
                since: 10,
                last_success: None,
                reason: CheckFailure::Network
            }
        );
        let still = next(&failing, 20, Err(CheckFailure::Proxy));
        assert_eq!(
            still,
            UpdateCheck::Failing {
                since: 10,
                last_success: None,
                reason: CheckFailure::Proxy
            },
            "the reason is the latest failure's"
        );
        let succeeded = next(&still, 30, Ok(()));
        assert_eq!(succeeded, UpdateCheck::Succeeded { at: 30 });
        assert_eq!(
            next(&succeeded, 40, Err(CheckFailure::Verification)),
            UpdateCheck::Failing {
                since: 40,
                last_success: Some(30),
                reason: CheckFailure::Verification
            }
        );
        assert_eq!(
            next(
                &UpdateCheck::Failing {
                    since: 40,
                    last_success: Some(30),
                    reason: CheckFailure::Verification
                },
                50,
                Err(CheckFailure::SystemTooOld)
            ),
            UpdateCheck::Failing {
                since: 40,
                last_success: Some(30),
                reason: CheckFailure::SystemTooOld
            }
        );
    }
}
