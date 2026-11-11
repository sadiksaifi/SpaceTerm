//! The CLI's lint over its person-facing update strings, the same rules as the desktop's copy
//! lint (ADR 0065): a version not running yet is "downloaded", an action names the software it
//! changes and never the computer, and "nothing running" stands for "idle".
use super::{lifecycle, status, update};
use crate::contract::{DeviceRef, MachineRef, UpdateFailedDetails};
use crate::notice::{ASK_AFTER, INSIST_AFTER, line};
use regex::Regex;
use spaceterm_core::service::lane::{LaneActivation, LaneEffects, LaneVersion};
use spaceterm_core::update::continuation::Continuation;
use spaceterm_core::update::facts::{
    CheckFailure, FailureKind, InstallFailure, Operation, PendingVersion, Platform, Protocols,
    StartFailure, StrandedDevice, UpdateCheck, UpdateFacts, UpdateFailure, Version,
};

const INSTALLED: u64 = 1_793_697_120;
const MACHINES: [&str; 3] = ["This Mac", "This Computer", "penguin"];
const RESTART: &str = "spaceterm --machine local restart";

/// The words and forms the update strings refuse, each with the reason, compiled once.
fn rules() -> &'static [(Regex, &'static str)] {
    static RULES: std::sync::OnceLock<Vec<(Regex, &'static str)>> = std::sync::OnceLock::new();
    RULES.get_or_init(compile)
}

fn compile() -> Vec<(Regex, &'static str)> {
    let machines = MACHINES.join("|");
    [
        (r"(?i)\binstall(ed|ing)?\b", INSTALLED_WORD),
        (r"(?i)\bready\b", READY),
        (r"(?i)\bstaged\b", STAGED),
        (r"(?i)\bpending\b", PENDING),
        (r"(?i)\bidle\b", IDLE),
        (&format!(r"\bUpdate ({machines})\b"), UPDATE_MACHINE),
        (r"\bUpdat(e with|ing)\b", UPDATE_RESTART),
        (r"\b0 Terminal Sessions\b", ZERO),
        // The `--machine` flag of a copied command line is not prose.
        (r"(^|[^-])\bmachine\b", LOWERCASE_MACHINE),
        (r"\bin Progress\b", IN_PROGRESS),
        (r"[\u{2018}\u{2019}]", APOSTROPHE),
        (r"\u{2014}", EM_DASH),
    ]
    .into_iter()
    .map(|(pattern, why)| (Regex::new(pattern).unwrap(), why))
    .collect()
}

const INSTALLED_WORD: &str = "a version not running yet is downloaded, not installed";
const READY: &str = "a version not running yet is downloaded, not ready";
const STAGED: &str = "a version not running yet is downloaded, not staged";
const PENDING: &str = "a version not running yet is downloaded, not pending";
const IDLE: &str = "person-facing text says nothing running";
const UPDATE_MACHINE: &str = "an action never names the computer";
const UPDATE_RESTART: &str = "the Core's step is a restart";
const ZERO: &str = "no consequence carries a zero count";
const LOWERCASE_MACHINE: &str = "Machine is a proper term";
const IN_PROGRESS: &str = "statuses use sentence case";
const APOSTROPHE: &str = "update text uses straight apostrophes";
const EM_DASH: &str = "no em dash";
const BARE_CORE: &str = "\"Core\" alone names nothing";

/// Every rule `text` breaks, and "Core" without "SpaceTerm" outside the restart labels.
fn violations(text: &str) -> Vec<&'static str> {
    static CORE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let mut broken: Vec<_> = rules()
        .iter()
        .filter(|(rule, _)| rule.is_match(text))
        .map(|(_, why)| *why)
        .collect();
    let core = CORE.get_or_init(|| Regex::new(r"\bCore\b").unwrap());
    let bare = core.find_iter(text).any(|core| {
        let before = &text[..core.start()];
        !["SpaceTerm ", "Restart ", "Restarting "]
            .iter()
            .any(|allowed| before.ends_with(allowed))
    });
    if bare {
        broken.push(BARE_CORE);
    }
    broken
}

fn facts(sessions: u32, breaking: bool, failure: Option<FailureKind>) -> UpdateFacts {
    UpdateFacts {
        revision: 1,
        platform: Platform::MacosArm64,
        running: Version::parse("0.11.3").unwrap(),
        pending: Some(PendingVersion {
            version: Version::parse("0.11.4").unwrap(),
            protocols: Protocols::CURRENT,
            breaking,
            installed_at: INSTALLED,
            overdue_since: None,
            stranded: if breaking {
                vec![StrandedDevice {
                    pairing: [0x9c; 32],
                    name: "old-mini".into(),
                }]
            } else {
                Vec::new()
            },
        }),
        sessions,
        operation: None,
        check: UpdateCheck::Never,
        skipped: None,
        last_failure: failure.map(|kind| UpdateFailure {
            id: 1,
            at: INSTALLED,
            version: Version::parse("0.11.4").unwrap(),
            kind,
        }),
    }
}

fn lane_version(version: &str) -> LaneVersion {
    LaneVersion {
        version: version.into(),
        commit: "a".repeat(40),
        sha256: "b".repeat(64),
    }
}

/// The status text of `facts` on `machine`, through the status renderer.
fn status_text(
    facts: UpdateFacts,
    continuation: Option<Continuation>,
    machine: &str,
    local: bool,
    unresolved: bool,
) -> String {
    let mut lane = super::golden_tests::lane_status(facts);
    if unresolved {
        lane.activation = LaneActivation::Unresolved;
    }
    let core = MachineRef {
        name: machine.into(),
        route: "penguin".into(),
        id: "5b".repeat(32),
    };
    let commands = status::Commands {
        restart: RESTART,
        local,
    };
    status::envelope(
        core,
        &lane,
        Vec::new(),
        Vec::new(),
        &commands,
        continuation,
        INSTALLED + ASK_AFTER,
    )
    .1
}

/// Every person-facing update string of the CLI, through the renderers the commands use: the
/// notice, the `update` result, the `status` text, the restart confirmation and its button, and
/// the restart failure text, for every continuation, ladder stage, session count, operation,
/// failure, check state and Machine.
fn corpus() -> Vec<String> {
    let reported = [
        None,
        Some(Continuation::Soon),
        Some(Continuation::WhenSessionsEnd),
        Some(Continuation::WhenNothingRuns),
        Some(Continuation::NeedsConfirmation),
        Some(Continuation::StartFailed),
        Some(Continuation::Unknown),
    ];
    let failures = [
        None,
        Some(FailureKind::Start(StartFailure::NotReady)),
        Some(FailureKind::Start(StartFailure::PortInUse)),
        Some(FailureKind::Start(StartFailure::DiskFull)),
        Some(FailureKind::Start(StartFailure::Interrupted)),
        Some(FailureKind::Install(InstallFailure::DiskFull)),
        Some(FailureKind::Install(InstallFailure::Download)),
        Some(FailureKind::Install(InstallFailure::Verification)),
    ];
    let version = Version::parse("0.11.4").unwrap();
    let operations = [
        None,
        Some(Operation::Checking),
        Some(Operation::Installing {
            version: version.clone(),
            received: 512,
            total: 2048,
        }),
        Some(Operation::Installing {
            version: version.clone(),
            received: 2048,
            total: 2048,
        }),
        Some(Operation::Restarting { to: version }),
        Some(Operation::RestartingSameVersion),
        Some(Operation::Stopping),
        Some(Operation::Uninstalling),
    ];
    let checks = [
        UpdateCheck::Disabled,
        UpdateCheck::Never,
        UpdateCheck::Succeeded { at: INSTALLED },
    ]
    .into_iter()
    .chain(
        [
            CheckFailure::Network,
            CheckFailure::Proxy,
            CheckFailure::Verification,
            CheckFailure::SystemTooOld,
            CheckFailure::ClockBehind,
            CheckFailure::SteppingStoneSkipped,
        ]
        .map(|reason| UpdateCheck::Failing {
            since: INSTALLED,
            last_success: None,
            reason,
        }),
    );
    let mut corpus = vec![
        lifecycle::Kind::Restart.confirm_label().to_owned(),
        lifecycle::Kind::Stop.confirm_label().to_owned(),
    ];
    for machine in MACHINES {
        for sessions in [0, 1, 3] {
            for breaking in [false, true] {
                for failure in failures {
                    let facts = facts(sessions, breaking, failure);
                    for waited in [0, ASK_AFTER, INSIST_AFTER + 3_600] {
                        let now = INSTALLED + waited;
                        for reported in reported {
                            corpus.extend(line(&facts, reported, machine, RESTART, now));
                            for installed in [false, true] {
                                corpus.push(update::text(
                                    installed, &facts, reported, machine, RESTART, now,
                                ));
                            }
                        }
                    }
                    for reported in reported {
                        corpus.push(status_text(facts.clone(), reported, machine, false, false));
                    }
                    let effects = LaneEffects {
                        ends_sessions: sessions,
                        sessions: Vec::new(),
                        start_requires_machine: false,
                        from: Some(lane_version("0.11.3")),
                        to: Some(lane_version("0.11.4")),
                        to_major: None,
                    };
                    let previewed = lifecycle::service_effects(&effects, Some(&facts));
                    corpus.push(lifecycle::summary(
                        lifecycle::Kind::Restart,
                        machine,
                        &previewed,
                    ));
                    let same = LaneEffects {
                        from: None,
                        to: None,
                        ..effects
                    };
                    let previewed = lifecycle::service_effects(&same, Some(&facts));
                    corpus.push(lifecycle::summary(
                        lifecycle::Kind::Restart,
                        machine,
                        &previewed,
                    ));
                    if let Some(kind) = failure {
                        let reason = status::failure_reason(kind).1;
                        for running in [None, Some("0.11.3".to_owned())] {
                            let details = UpdateFailedDetails {
                                version: "0.11.4".into(),
                                running,
                                reason: Some(reason),
                            };
                            corpus.push(lifecycle::update_failure_text(machine, &details, RESTART));
                        }
                    }
                }
            }
            let mut current = facts(sessions, false, None);
            current.pending = None;
            corpus.push(update::text(
                false, &current, None, machine, RESTART, INSTALLED,
            ));
        }
        for operation in &operations {
            let mut facts = facts(1, false, None);
            facts.operation = operation.clone();
            corpus.push(status_text(facts, None, machine, false, false));
        }
        for check in checks.clone() {
            let mut facts = facts(1, false, None);
            facts.check = check;
            facts.skipped = Some(Version::parse("0.11.2").unwrap());
            for local in [false, true] {
                corpus.push(status_text(facts.clone(), None, machine, local, true));
            }
        }
        let device = |name: &str| DeviceRef {
            id: "9c".repeat(32),
            name: name.into(),
        };
        for devices in [
            vec![],
            vec![device("old-mini")],
            vec![device("a"), device("b")],
        ] {
            corpus.push(status::stranded(&devices, machine));
        }
    }
    corpus
}

#[test]
fn every_update_string_of_the_cli_passes_the_copy_lint() {
    let corpus: std::collections::BTreeSet<String> = corpus().into_iter().collect();
    assert!(corpus.len() > 300, "{}", corpus.len());
    let broken: Vec<_> = corpus
        .iter()
        .filter_map(|text| {
            let why = violations(text);
            (!why.is_empty()).then(|| format!("{text:?}: {why:?}"))
        })
        .collect();
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

#[test]
fn each_rule_refuses_an_example_that_breaks_only_it() {
    for (text, why) in [
        (
            "SpaceTerm Core 0.11.4 is installed on penguin.",
            INSTALLED_WORD,
        ),
        ("SpaceTerm Core 0.11.4 is ready.", READY),
        ("SpaceTerm Core 0.11.4 is staged.", STAGED),
        ("SpaceTerm Core 0.11.4 is pending.", PENDING),
        (
            "SpaceTerm Core restarts once its Terminal Sessions are idle.",
            IDLE,
        ),
        ("Update This Mac\u{2026}", UPDATE_MACHINE),
        (
            "Restart with: spaceterm restart. Update with: spaceterm restart",
            UPDATE_RESTART,
        ),
        ("Restarting ends 0 Terminal Sessions.", ZERO),
        (
            "SpaceTerm Core on that machine restarts.",
            LOWERCASE_MACHINE,
        ),
        ("1 in Progress", IN_PROGRESS),
        ("SpaceTerm Core couldn\u{2019}t start.", APOSTROPHE),
        ("SpaceTerm Core couldn\u{2018}t start.", APOSTROPHE),
        ("SpaceTerm Core restarts \u{2014} soon.", EM_DASH),
        ("Core 0.11.4 is downloaded.", BARE_CORE),
    ] {
        assert_eq!(violations(text), [why], "{text:?}");
    }
    for allowed in [
        "SpaceTerm Core couldn't start.",
        "Restart Core\u{2026}",
        "Restarting Core\u{2026}",
        "SpaceTerm Core 0.11.4 is downloaded on This Mac.",
    ] {
        assert!(violations(allowed).is_empty(), "{allowed:?}");
    }
}
