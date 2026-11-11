//! `update`: download a SpaceTerm Core update beside the running version. Downloading ends
//! nothing, so it asks nothing; `restart` moves SpaceTerm Core to the new version.
//!
//! Without files, SpaceTerm Core installs the release its own signed feed names. Preflight and
//! Development builds have no feed, so they take only `--archive` with `--signature`, which this
//! command pushes over the service lane like the desktop pushes its embedded build. A stopped
//! SpaceTerm Core on this machine installs directly.
use super::{Context, Opened, Pending, Reply, Routed};
use crate::contract::{self, Code, CoreUpdate, Envelope, Failure, MachineRef};
use spaceterm_core::service::Service;
use spaceterm_core::service::lane::Offline;
use spaceterm_core::service::lane::admin::{self, PushArchive};
use spaceterm_core::service::updates::InstallError;
use spaceterm_core::update::continuation::Continuation;
use spaceterm_core::update::facts::UpdateFacts;
use spaceterm_core::update::sources::SourceError;
use std::io::Read;
use std::path::Path;

const COMMAND: &str = "update";

pub fn run(context: &Context<'_>, files: Option<(&Path, &Path)>) -> Reply {
    let routed = match context.routed() {
        Ok(routed) => routed,
        Err(error) => return Reply::routing_failure(COMMAND, error),
    };
    let pending = match Pending::open(context, &routed.route) {
        Ok(pending) => pending,
        Err(failure) => return Reply::failure(COMMAND, Some(routed.machine), failure),
    };
    match super::open_lane(context, &routed.connection, &routed.route, &pending) {
        Ok(opened) => running(context, routed, opened, files),
        Err(failure) if routed.local && offline(&failure) => stopped(context, routed, files),
        Err(failure) => Reply::failure(COMMAND, Some(routed.machine), failure),
    }
}

/// Whether a lane failure means SpaceTerm Core on this machine is not running, so the CLI
/// installs directly. A running Core that refuses or fails answers for itself instead, because
/// an install outside its engine would stay invisible to it.
pub(crate) fn offline(failure: &Failure) -> bool {
    matches!(failure.code, Code::NotRunning | Code::Unreachable)
}

/// A running SpaceTerm Core installs through its service lane, on this machine or a paired one.
fn running(
    context: &Context<'_>,
    routed: Routed,
    mut opened: Opened,
    files: Option<(&Path, &Path)>,
) -> Reply {
    let installed = match files {
        Some((archive, signature)) => {
            push(context, &mut opened.session, archive, signature).map(|facts| (true, facts))
        }
        None => admin::install_from_feed(&mut opened.session).map_err(super::lane_failure),
    };
    match installed {
        Ok((installed, facts)) => {
            // A Core of service 1.2 says how the version it holds finishes.
            let detailed = super::UpdateLane::detailed_facts(&mut opened.session)
                .ok()
                .flatten();
            let continuation = crate::notice::reported_for(detailed.as_ref(), &facts);
            finish(
                context,
                routed.machine,
                installed,
                &facts,
                continuation,
                opened.recovered,
            )
        }
        Err(failure) => Reply::new(
            &super::with_recovered(
                Envelope::error(COMMAND, Some(routed.machine), failure, contract::Empty {}),
                opened.recovered,
            ),
            String::new(),
        ),
    }
}

/// A stopped SpaceTerm Core on this machine installs directly. It starts on the new version.
fn stopped(context: &Context<'_>, routed: Routed, files: Option<(&Path, &Path)>) -> Reply {
    let offline = match Offline::new(context.identity) {
        Ok(offline) => offline,
        Err(error) => {
            return Reply::failure(
                COMMAND,
                Some(routed.machine),
                contract::from_core_error(error),
            );
        }
    };
    let installed = match files {
        Some((archive, signature)) => digest(archive).and_then(|(sha256, _)| {
            offline
                .install_archive(archive, signature, &sha256)
                .map(|facts| (true, facts))
                .map_err(|error| source_failure(&error))
        }),
        None => offline
            .install_from_feed()
            .map_err(|error| source_failure(&error)),
    };
    match installed {
        Ok((installed, facts)) => {
            finish(context, routed.machine, installed, &facts, None, Vec::new())
        }
        Err(failure) => Reply::failure(COMMAND, Some(routed.machine), failure),
    }
}

pub(crate) fn source_failure(error: &SourceError) -> Failure {
    match error {
        SourceError::Install(InstallError::BelowFloor { floor }) => contract::below_floor(*floor),
        error => contract::from_lane_code(error.code()),
    }
}

/// The archive's SHA-256 as 64 lowercase hex digits, and its length.
fn digest(archive: &Path) -> Result<(String, u64), Failure> {
    let unreadable = || Failure::new(Code::NoCandidate);
    let mut file = std::fs::File::open(archive).map_err(|_| unreadable())?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 1 << 20];
    let mut length = 0u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| unreadable())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        length += count as u64;
    }
    Ok((super::hex(digest.finish().as_ref()), length))
}

/// Verifies a local archive as this build identity would, then pushes it. Verifying first
/// names the version, so SpaceTerm Core refuses one it would not install before any byte is
/// sent.
fn push(
    context: &Context<'_>,
    session: &mut spaceterm_core::service::lane::client::Session,
    archive: &Path,
    signature_path: &Path,
) -> Result<UpdateFacts, Failure> {
    let (sha256, length) = digest(archive)?;
    let signature =
        std::fs::read(signature_path).map_err(|_| Failure::new(Code::CandidateUnverified))?;
    let service = Service::for_identity(context.identity).map_err(contract::from_core_error)?;
    let metadata = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
        .iter()
        .find_map(|target| {
            service
                .inspect_candidate_for_target(
                    archive,
                    signature_path,
                    &sha256,
                    target,
                    spaceterm_core::protocol::MAJOR,
                )
                .ok()
        });
    let metadata = match metadata {
        Some(metadata) => metadata,
        None => service
            .inspect_update_candidate(archive, signature_path, &sha256)
            .map_err(contract::from_core_error)?,
    };
    let candidate = metadata
        .candidate()
        .map_err(|_| Failure::new(Code::CandidateUnverified))?;
    let mut file = std::fs::File::open(archive).map_err(|_| Failure::new(Code::NoCandidate))?;
    admin::push(
        session,
        &PushArchive {
            candidate: &candidate,
            length,
            sha256: &sha256,
            signature: &signature,
        },
        &mut file,
        &mut |_| {},
    )
    .map_err(super::lane_failure)
}

fn finish(
    context: &Context<'_>,
    machine: MachineRef,
    installed: bool,
    facts: &UpdateFacts,
    continuation: Option<Continuation>,
    recovered: Vec<contract::RecoveredOperation>,
) -> Reply {
    let restart = context.command_line(&machine, &["restart"]);
    let now = crate::notice::now();
    let text = text(
        installed,
        facts,
        continuation,
        super::display_name(&machine),
        &restart,
        now,
    );
    Reply::new(&envelope(machine, installed, facts, recovered, now), text)
}

/// The `update` envelope.
pub(crate) fn envelope(
    machine: MachineRef,
    installed: bool,
    facts: &UpdateFacts,
    recovered: Vec<contract::RecoveredOperation>,
    now: u64,
) -> Envelope<CoreUpdate> {
    let data = CoreUpdate {
        installed,
        running: facts.running.to_string(),
        pending: super::status::pending(facts, now),
        sessions: facts.sessions,
    };
    super::with_recovered(Envelope::ok(COMMAND, Some(machine), data), recovered)
}

/// What downloading found, and how SpaceTerm Core on `machine` finishes moving to the Pending
/// Version at `now`, Unix seconds, in the sentences of the notice. `installed` says whether
/// this command downloaded it. `continuation` is the one a Core of service 1.2 reported for
/// that version. `restart` is the command line that restarts SpaceTerm Core.
pub(crate) fn text(
    installed: bool,
    facts: &UpdateFacts,
    continuation: Option<Continuation>,
    machine: &str,
    restart: &str,
    now: u64,
) -> String {
    let running = &facts.running;
    let Some(pending) = &facts.pending else {
        return format!("SpaceTerm Core {running} is up to date.\n");
    };
    let what = if installed {
        format!("Downloaded SpaceTerm Core {}.", pending.version)
    } else {
        format!("SpaceTerm Core {} is already downloaded.", pending.version)
    };
    let how = crate::notice::finish(facts, continuation, machine, restart, now)
        .map(crate::notice::Finish::sentences)
        .unwrap_or_default();
    format!("{what} SpaceTerm Core {running} keeps running. {how}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::{ASK_AFTER, INSIST_AFTER};
    use spaceterm_core::update::facts::{
        PendingVersion, Platform, Protocols, UpdateCheck, Version,
    };

    const INSTALLED: u64 = 1_793_697_120;
    const RESTART: &str = "spaceterm --machine penguin restart";

    fn facts(sessions: u32, breaking: bool) -> UpdateFacts {
        UpdateFacts {
            revision: 4,
            platform: Platform::LinuxX86_64,
            running: Version::parse("0.7.0").unwrap(),
            pending: Some(PendingVersion {
                version: Version::parse("0.8.0").unwrap(),
                protocols: Protocols::CURRENT,
                breaking,
                installed_at: INSTALLED,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            sessions,
            operation: None,
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: None,
        }
    }

    #[test]
    fn update_names_the_downloaded_version_and_how_it_finishes() {
        let at = INSTALLED + 60;
        assert_eq!(
            text(true, &facts(2, false), None, "penguin", RESTART, at),
            "Downloaded SpaceTerm Core 0.8.0. SpaceTerm Core 0.7.0 keeps running. SpaceTerm Core \
             restarts into it when no Terminal Session is open. Restart now with: spaceterm \
             --machine penguin restart\n"
        );
        assert_eq!(
            text(false, &facts(0, false), None, "penguin", RESTART, at),
            "SpaceTerm Core 0.8.0 is already downloaded. SpaceTerm Core 0.7.0 keeps running. \
             SpaceTerm Core restarts into it in a moment. Restart now with: spaceterm --machine \
             penguin restart\n"
        );
        assert_eq!(
            text(true, &facts(2, true), None, "penguin", RESTART, at),
            "Downloaded SpaceTerm Core 0.8.0. SpaceTerm Core 0.7.0 keeps running. Restarting \
             ends 2 Terminal Sessions. Restart with: spaceterm --machine penguin restart\n",
            "a breaking version never restarts by itself"
        );
        assert_eq!(
            text(true, &facts(0, true), None, "penguin", RESTART, at),
            "Downloaded SpaceTerm Core 0.8.0. SpaceTerm Core 0.7.0 keeps running. No Terminal \
             Session is open. Restart with: spaceterm --machine penguin restart\n"
        );
    }

    #[test]
    fn update_names_how_long_an_update_has_waited_from_the_ask_stage() {
        assert_eq!(
            text(
                false,
                &facts(1, false),
                None,
                "penguin",
                RESTART,
                INSTALLED + ASK_AFTER
            ),
            "SpaceTerm Core 0.8.0 is already downloaded. SpaceTerm Core 0.7.0 keeps running. An \
             update has waited 2 days on penguin. Restarting into SpaceTerm Core 0.8.0 ends 1 \
             Terminal Session. Restart with: spaceterm --machine penguin restart\n"
        );
        assert_eq!(
            text(
                false,
                &facts(3, true),
                None,
                "penguin",
                RESTART,
                INSTALLED + INSIST_AFTER
            ),
            "SpaceTerm Core 0.8.0 is already downloaded. SpaceTerm Core 0.7.0 keeps running. An \
             update has waited 7 days on penguin. Restarting into SpaceTerm Core 0.8.0 ends 3 \
             Terminal Sessions. Restart with: spaceterm --machine penguin restart\n"
        );
    }

    #[test]
    fn update_says_what_a_1_2_core_reports_for_the_version_it_holds() {
        assert_eq!(
            text(
                false,
                &facts(2, false),
                Some(Continuation::WhenNothingRuns),
                "penguin",
                RESTART,
                INSTALLED + ASK_AFTER
            ),
            "SpaceTerm Core 0.8.0 is already downloaded. SpaceTerm Core 0.7.0 keeps running. \
             SpaceTerm Core restarts into it after an hour with nothing running. Restart now \
             with: spaceterm --machine penguin restart\n"
        );
    }

    #[test]
    fn update_with_no_newer_version_names_the_running_one() {
        let mut current = facts(2, false);
        current.pending = None;
        assert_eq!(
            text(false, &current, None, "penguin", RESTART, INSTALLED),
            "SpaceTerm Core 0.7.0 is up to date.\n"
        );
    }
}
