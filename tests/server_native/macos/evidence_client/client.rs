//! Fixed shell-to-probe entries. Exec preserves the originating shell as parent.

use spaceterm_host::native_deployment::contracts::NativeFailure;
use std::{ffi::OsString, process::ExitCode};

#[cfg(target_os = "macos")]
#[path = "native.rs"]
mod native;

const PROBE: &str = "/Library/Application Support/SpaceTerm Native Evidence 520/capability-probe";

fn decimal(value: &str, maximum: u64) -> Result<u64, NativeFailure> {
    if value.is_empty()
        || value.len() > 20
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(NativeFailure::InvalidRecord);
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= maximum)
        .ok_or(NativeFailure::InvalidRecord)
}

fn probe_arguments(arguments: &[String], deadline: u64) -> Result<Vec<String>, NativeFailure> {
    match arguments {
        [mode, context, fd] if mode == "--environment" => {
            if !(1..=2).contains(&decimal(context, 2)?) {
                return Err(NativeFailure::InvalidRecord);
            }
            decimal(fd, i32::MAX as u64)?;
            Ok(arguments.to_vec())
        }
        [mode, case, context, targets, rest @ ..] if mode == "--probe" => {
            let case_number = decimal(case, 13)?;
            if !(1..=13).contains(&case_number)
                || !(1..=2).contains(&decimal(context, 2)?)
                || decimal(targets, i32::MAX as u64)? < 3
                || (case_number == 13) != (rest.len() == 1)
                || (case_number != 13 && !rest.is_empty())
            {
                return Err(NativeFailure::InvalidRecord);
            }
            if let Some(fd) = rest.first()
                && decimal(fd, i32::MAX as u64)? == decimal(targets, i32::MAX as u64)?
            {
                return Err(NativeFailure::InvalidRecord);
            }
            Ok([
                vec![
                    "--run".into(),
                    case.clone(),
                    context.clone(),
                    deadline.to_string(),
                    targets.clone(),
                ],
                rest.to_vec(),
            ]
            .concat())
        }
        _ => Err(NativeFailure::InvalidRecord),
    }
}

fn probe_environment(
    values: impl Iterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    // Only the values actually compared, plus the narrow SSH socket selector.
    // No signing inputs, loader values, private account credentials or Python state.
    values
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                matches!(name, "PATH" | "LANG" | "SSH_AUTH_SOCK") || name.starts_with("LC_")
            })
        })
        .collect()
}

pub fn entry() -> ExitCode {
    let result = run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("classification={failure}");
            ExitCode::from(failure as u8)
        }
    }
}

fn run() -> Result<(), NativeFailure> {
    let arguments: Vec<String> = std::env::args_os()
        .skip(1)
        .take(9)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| NativeFailure::InvalidRecord)
        })
        .collect::<Result<_, _>>()?;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: these effect-free native calls return the current process IDs.
        if unsafe { libc::getuid() == 0 || libc::getuid() != libc::geteuid() } {
            return Err(NativeFailure::IdentityMismatch);
        }
        if arguments.first().is_some_and(|value| value == "input") {
            return native::input(&arguments);
        }
        if arguments.first().is_some_and(|value| value == "targets") {
            return native::targets(&arguments);
        }
        let wait = if arguments.get(1).is_some_and(|value| value == "12") {
            60
        } else {
            5
        };
        let deadline = native::monotonic_ns()?
            .checked_add(wait * 1_000_000_000)
            .ok_or(NativeFailure::LimitExceeded)?;
        let arguments = probe_arguments(&arguments, deadline)?;
        native::exec_probe(PROBE, &arguments, probe_environment(std::env::vars_os()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (
            PROBE,
            probe_arguments(&arguments, 0)?,
            probe_environment(std::env::vars_os()),
        );
        Err(NativeFailure::UnsupportedHost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_shell_mode_descriptor_reaches_the_probe_unchanged() {
        let arguments = ["--environment", "2", "0"].map(str::to_owned);
        assert_eq!(probe_arguments(&arguments, 10).unwrap(), arguments);
        let arguments = ["--probe", "13", "1", "7", "0"].map(str::to_owned);
        assert_eq!(
            probe_arguments(&arguments, 10).unwrap(),
            ["--run", "13", "1", "10", "7", "0"]
        );
    }

    #[test]
    fn noncanonical_or_shared_descriptors_refuse_before_exec() {
        for arguments in [
            vec!["--environment", "1", "00"],
            vec!["--environment", "3", "0"],
            vec!["--probe", "13", "1", "3", "3"],
            vec!["--probe", "5", "1", "3", "0"],
        ] {
            assert_eq!(
                probe_arguments(
                    &arguments
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect::<Vec<_>>(),
                    10
                ),
                Err(NativeFailure::InvalidRecord)
            );
        }
    }

    #[test]
    fn only_comparison_values_and_the_agent_selector_reach_exec() {
        let values = [
            "PATH",
            "LANG",
            "LC_CTYPE",
            "SSH_AUTH_SOCK",
            "MACOS_SIGNING_CERTIFICATE_P12",
            "AWS_SESSION_TOKEN",
            "DYLD_INSERT_LIBRARIES",
            "PYTHONPATH",
            "HOME",
        ]
        .into_iter()
        .map(|name| (OsString::from(name), OsString::from("public-sentinel")));
        let actual: Vec<_> = probe_environment(values)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(
            actual,
            ["PATH", "LANG", "LC_CTYPE", "SSH_AUTH_SOCK"].map(OsString::from)
        );
    }
}
