//! `server update`: check the signed feed, stage a verified candidate, preview the change and
//! activate it after confirmation.
//!
//! A running server stages and activates through its service lane, which reaches it across
//! protocol majors. On the server's own machine a stopped server is updated directly, and an
//! interrupted activation is settled first. When the settled state allows return to the
//! previous runtime, the preview offers that return instead.
use super::lifecycle::{self, Settled};
use super::{Context, Opened, Pending, Reply, Routed};
use crate::contract::{
    self, Code, Confirmation, Envelope, Failure, Receipt, ReceiptState, ServiceAction, UpdateCheck,
    UpdateEffects, UpdateKind, UpdateOperation, Version, Warning, WarningCode, WarningDetails,
};
use base64::Engine;
use spaceterm_server::service::activation::ActivationPhase;
use spaceterm_server::service::lane::{
    LaneAction, LaneEffects, LaneReply, LaneRequest, LaneUpdateKind, LaneVersion, MAX_CHUNK,
    Offline,
};
use spaceterm_server::service::{RetainedInstallation, Service};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

const COMMAND: &str = "server.update";
/// How long the flow waits for the updated server to return.
const RETURN_WAIT: Duration = Duration::from_secs(180);
/// Each uploaded chunk's decoded size. Base64 keeps the frame under the lane's limit.
const CHUNK: usize = MAX_CHUNK;

pub fn run(
    context: &Context<'_>,
    check: bool,
    files: Option<(&Path, &Path)>,
    token: Option<&str>,
) -> Reply {
    let routed = match context.routed() {
        Ok(routed) => routed,
        Err((server, failure)) => return Reply::failure(COMMAND, server, failure),
    };
    let pending = match Pending::open(context, &routed.route) {
        Ok(pending) => pending,
        Err(failure) => return Reply::failure(COMMAND, Some(routed.server), failure),
    };
    if routed.local {
        // Settle an interrupted activation first. Recovery never repeats a dispatched step.
        let _ = Service::for_identity(context.identity).and_then(|v| v.recover_activation());
    }
    let opened = super::open_lane(context, &routed.connection, &routed.route, &pending);
    match opened {
        Ok(opened) => running(context, routed, opened, &pending, check, files, token),
        Err(_) if routed.local => stopped(context, routed, check, files, token),
        Err(failure) => Reply::failure(COMMAND, Some(routed.server), failure),
    }
}

fn version(value: &LaneVersion) -> Version {
    super::status::version(value)
}

fn installed(value: &RetainedInstallation) -> Version {
    Version {
        version: value.version.clone(),
        commit: value.commit.clone(),
        sha256: value
            .directory
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_owned(),
    }
}

fn check_text(check: &UpdateCheck) -> String {
    let mut text = format!("Installed: {}\n", check.approved.version);
    match &check.latest {
        Some(latest) if latest.sha256 == check.approved.sha256 => {
            text.push_str("Up to date.\n");
        }
        Some(latest) => text.push_str(&format!("Available: {}\n", latest.version)),
        None => text.push_str("No release feed for this build.\n"),
    }
    for staged in &check.staged {
        text.push_str(&format!("Staged: {}\n", staged.version));
    }
    text
}

fn lane_check(check: spaceterm_server::service::lane::LaneCheck) -> UpdateCheck {
    UpdateCheck {
        approved: version(&check.approved),
        latest: check.latest.as_ref().map(version),
        staged: check.staged.iter().map(version).collect(),
    }
}

fn running(
    context: &Context<'_>,
    routed: Routed,
    mut opened: Opened,
    pending: &Pending,
    check: bool,
    files: Option<(&Path, &Path)>,
    token: Option<&str>,
) -> Reply {
    let server = routed.server.clone();
    let fail = |failure: Failure, recovered| {
        Reply::new(
            &super::with_recovered(
                Envelope::error(COMMAND, Some(server.clone()), failure, contract::Empty {}),
                recovered,
            ),
            String::new(),
        )
    };
    let deadline = || Instant::now() + Duration::from_secs(30);
    if check {
        return match opened.session.call(&LaneRequest::Check, deadline()) {
            Ok(LaneReply::Check(check)) => {
                let data = lane_check(check);
                let text = check_text(&data);
                Reply::new(
                    &super::with_recovered(
                        Envelope::ok(COMMAND, Some(server), data),
                        opened.recovered,
                    ),
                    text,
                )
            }
            Ok(_) => fail(Failure::new(Code::ProtocolError), opened.recovered),
            Err(error) => fail(super::lane_failure(error), opened.recovered),
        };
    }
    // Stage the candidate. Local files stage directly on this machine; a remote server
    // receives them over the lane.
    let staged = match files {
        Some((archive, signature)) if routed.local => Offline::new(context.identity)
            .and_then(|offline| offline.admin().stage_files(archive, signature))
            .map(|v| installed(&v))
            .map_err(contract::from_server_error),
        Some((archive, signature)) => upload(&mut opened.session, archive, signature),
        None => match opened.session.call(
            &LaneRequest::StageFetch,
            Instant::now() + Duration::from_secs(30 * 60),
        ) {
            Ok(reply) => staged_version(reply),
            Err(error) => Err(super::lane_failure(error)),
        },
    };
    let candidate = match staged {
        Ok(candidate) => candidate,
        Err(failure) => return fail(failure, opened.recovered),
    };
    let action = LaneAction::Update {
        installation: candidate.sha256.clone(),
    };
    let Some(token) = token else {
        let reply = opened.session.call(
            &LaneRequest::Preview {
                action: action.clone(),
            },
            deadline(),
        );
        return match reply {
            Ok(LaneReply::Preview {
                token,
                expires_at,
                effects,
            }) => preview(
                context,
                routed,
                opened,
                pending,
                (action, candidate),
                token,
                expires_at,
                &effects,
            ),
            Ok(_) => fail(Failure::new(Code::ProtocolError), opened.recovered),
            Err(error) => fail(super::lane_failure(error), opened.recovered),
        };
    };
    execute(
        context,
        routed,
        opened,
        pending,
        action,
        token.to_owned(),
        candidate,
    )
}

fn staged_version(reply: LaneReply) -> Result<Version, Failure> {
    match reply {
        LaneReply::Staged {
            installation,
            version,
            commit,
        } => Ok(Version {
            version,
            commit,
            sha256: installation,
        }),
        _ => Err(Failure::new(Code::ProtocolError)),
    }
}

#[allow(clippy::too_many_arguments, reason = "one preview's parts, used once")]
fn preview(
    context: &Context<'_>,
    routed: Routed,
    opened: Opened,
    pending: &Pending,
    (action, candidate): (LaneAction, Version),
    token: String,
    expires_at: String,
    effects: &LaneEffects,
) -> Reply {
    let (Some(from), Some(to)) = (&effects.from, &effects.to) else {
        return Reply::failure(
            COMMAND,
            Some(routed.server),
            Failure::new(Code::ProtocolError),
        );
    };
    let data = UpdateEffects {
        from: version(from),
        to: version(to),
        kind: match effects.kind {
            Some(LaneUpdateKind::Return) => UpdateKind::Return,
            _ => UpdateKind::Update,
        },
        ends_sessions: effects.ends_sessions,
        sessions: effects.sessions.clone(),
    };
    let summary = summary(&routed.server.name, &data);
    if context.interactive() {
        eprint!("{summary}");
        if super::confirm("Update Server") {
            return execute(context, routed, opened, pending, action, token, candidate);
        }
        return Reply::failure(
            COMMAND,
            Some(routed.server),
            Failure::new(Code::ConfirmationRequired),
        );
    }
    let mut envelope = Envelope::confirmation(
        COMMAND,
        Some(routed.server),
        Confirmation {
            token: token.clone(),
            expires_at,
            effects: data,
        },
    );
    if let Some(warning) = major_warning(effects.to_major) {
        envelope.warnings.push(warning);
    }
    Reply::new(
        &super::with_recovered(envelope, opened.recovered),
        format!("{summary}Confirm with: spaceterm server update --confirm {token}\n"),
    )
}

/// After an update to another protocol major, work commands need a matching CLI.
fn major_warning(to_major: Option<u16>) -> Option<Warning> {
    let server_major = to_major?;
    let client_major = spaceterm_server::protocol::MAJOR;
    (server_major != client_major).then(|| {
        Warning::new(
            WarningCode::ClientMajorDiffers,
            WarningDetails::Major {
                client_major,
                server_major,
            },
        )
    })
}

fn summary(server: &str, effects: &UpdateEffects) -> String {
    let change = match effects.kind {
        UpdateKind::Update => format!(
            "Update the {server} server from {} to {}?",
            effects.from.version, effects.to.version
        ),
        UpdateKind::Return => format!(
            "Return the {server} server from {} to {}?",
            effects.from.version, effects.to.version
        ),
    };
    let ends = if effects.ends_sessions == 0 {
        "No Terminal Sessions end.".to_owned()
    } else {
        format!(
            "{} Terminal Sessions end for every client. Workspaces are kept.",
            effects.ends_sessions
        )
    };
    format!("{change}\n{ends}\n")
}

fn execute(
    context: &Context<'_>,
    routed: Routed,
    opened: Opened,
    pending: &Pending,
    action: LaneAction,
    token: String,
    candidate: Version,
) -> Reply {
    let Opened {
        lane,
        session,
        recovered,
    } = opened;
    let outcome = lane.perform(
        session,
        action,
        token,
        context.timeout(RETURN_WAIT),
        &mut |operation| pending.add(operation),
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return Reply::new(
                &super::with_recovered(
                    Envelope::error(
                        COMMAND,
                        Some(routed.server),
                        super::lane_failure(error),
                        contract::Empty {},
                    ),
                    recovered,
                ),
                String::new(),
            );
        }
    };
    let Settled {
        receipt, failure, ..
    } = lifecycle::settle(ServiceAction::Update, outcome, pending);
    finish(routed, receipt, failure, Some(candidate), recovered)
}

fn finish(
    routed: Routed,
    receipt: Receipt,
    failure: Option<Failure>,
    version: Option<Version>,
    recovered: Vec<contract::RecoveredOperation>,
) -> Reply {
    let data = UpdateOperation {
        operation: receipt.operation.clone(),
        version: version.unwrap_or(Version {
            version: String::new(),
            commit: String::new(),
            sha256: String::new(),
        }),
        receipt,
    };
    let text = match failure {
        None => "Server updated.\n".to_owned(),
        Some(_) => String::new(),
    };
    let envelope = match failure {
        None => Envelope::ok(COMMAND, Some(routed.server), data),
        Some(failure) => Envelope::error(COMMAND, Some(routed.server), failure, data),
    };
    Reply::new(&super::with_recovered(envelope, recovered), text)
}

/// Sends a signed archive over the lane in order, then returns the staged installation.
fn upload(
    session: &mut spaceterm_server::service::lane::client::Session,
    archive: &Path,
    signature: &Path,
) -> Result<Version, Failure> {
    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let signature = std::fs::read(signature).map_err(|_| Failure::new(Code::NoCandidate))?;
    let mut file = std::fs::File::open(archive).map_err(|_| Failure::new(Code::NoCandidate))?;
    let length = file
        .metadata()
        .map_err(|_| Failure::new(Code::NoCandidate))?
        .len();
    let sha256 = {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buffer = vec![0; 1 << 20];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|_| Failure::new(Code::NoCandidate))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        super::hex(digest.finish().as_ref())
    };
    let deadline = || Instant::now() + Duration::from_secs(30);
    let reply = session
        .call(
            &LaneRequest::StageUpload {
                length,
                sha256,
                signature: encode(&signature),
            },
            deadline(),
        )
        .map_err(super::lane_failure)?;
    let LaneReply::UploadProgress { upload, .. } = reply else {
        return Err(Failure::new(Code::ProtocolError));
    };
    let mut file = std::fs::File::open(archive).map_err(|_| Failure::new(Code::NoCandidate))?;
    let mut buffer = vec![0; CHUNK];
    let mut offset = 0u64;
    loop {
        let count =
            read_full(&mut file, &mut buffer).map_err(|_| Failure::new(Code::NoCandidate))?;
        if count == 0 {
            return Err(Failure::new(Code::CandidateChanged));
        }
        let reply = session
            .call(
                &LaneRequest::UploadChunk {
                    upload: upload.clone(),
                    offset,
                    data: encode(&buffer[..count]),
                },
                deadline(),
            )
            .map_err(super::lane_failure)?;
        offset += count as u64;
        match reply {
            LaneReply::UploadProgress { next_offset, .. } if next_offset == offset => {}
            reply @ LaneReply::Staged { .. } if offset == length => return staged_version(reply),
            _ => return Err(Failure::new(Code::ProtocolError)),
        }
    }
}

fn read_full(file: &mut std::fs::File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    Ok(filled)
}

/// A stopped server on this machine: stage, preview and activate through the service itself.
fn stopped(
    context: &Context<'_>,
    routed: Routed,
    check: bool,
    files: Option<(&Path, &Path)>,
    token: Option<&str>,
) -> Reply {
    let server = routed.server.clone();
    let fail = |failure: Failure| Reply::failure(COMMAND, Some(server.clone()), failure);
    let offline = match Offline::new(context.identity) {
        Ok(offline) => offline,
        Err(error) => return fail(contract::from_server_error(error)),
    };
    let admin = offline.admin();
    if check {
        return match admin.check() {
            Ok(check) => {
                let data = lane_check(check);
                let text = check_text(&data);
                Reply::new(&Envelope::ok(COMMAND, Some(server), data), text)
            }
            Err(error) => fail(contract::from_server_error(error)),
        };
    }
    let service = match Service::for_identity(context.identity) {
        Ok(service) => service,
        Err(error) => return fail(contract::from_server_error(error)),
    };
    // A candidate that never became ready can return to the previous runtime instead.
    if let Ok(approval) = service.prepare_return() {
        let effects = UpdateEffects {
            from: installed(approval.candidate()),
            to: installed(approval.previous()),
            kind: UpdateKind::Return,
            ends_sessions: 0,
            sessions: Vec::new(),
        };
        let token_now = match approval.confirmation_token() {
            Ok(token) => format!("c1-{token}"),
            Err(error) => return fail(contract::from_server_error(error)),
        };
        if token.is_some_and(|given| given != token_now) {
            return fail(Failure::new(Code::Changed));
        }
        if !confirmed(context, &routed.server.name, &effects, &token_now, token) {
            return offline_preview(routed, effects, token_now);
        }
        let to = effects.to.clone();
        return match service.return_to_previous(approval) {
            Ok(status) => offline_finish(routed, ServiceAction::Return, &status, to),
            Err(error) => fail(contract::from_server_error(error)),
        };
    }
    let staged = match files {
        Some((archive, signature)) => admin.stage_files(archive, signature),
        None => admin.stage_fetch(),
    };
    let candidate = match staged {
        Ok(candidate) => candidate,
        Err(error) => return fail(contract::from_server_error(error)),
    };
    let approval = match service.prepare_activation(&candidate) {
        Ok(Some(approval)) => approval,
        Ok(None) => return fail(Failure::new(Code::NoCandidate)),
        Err(error) => return fail(contract::from_server_error(error)),
    };
    let effects = UpdateEffects {
        from: installed(approval.previous()),
        to: installed(approval.candidate()),
        kind: UpdateKind::Update,
        ends_sessions: 0,
        sessions: Vec::new(),
    };
    let token_now = match approval.confirmation_token() {
        Ok(token) => format!("c1-{token}"),
        Err(error) => return fail(contract::from_server_error(error)),
    };
    if token.is_some_and(|given| given != token_now) {
        return fail(Failure::new(Code::Changed));
    }
    if !confirmed(context, &routed.server.name, &effects, &token_now, token) {
        return offline_preview(routed, effects, token_now);
    }
    let to = effects.to.clone();
    match service.activate(approval) {
        Ok(status) => offline_finish(routed, ServiceAction::Update, &status, to),
        Err(error) => fail(contract::from_server_error(error)),
    }
}

/// Whether the change is confirmed: by a matching `--confirm`, or by the person at a prompt.
/// A token that no longer matches the observation is `changed`, which the preview reports.
fn confirmed(
    context: &Context<'_>,
    server: &str,
    effects: &UpdateEffects,
    current: &str,
    given: Option<&str>,
) -> bool {
    match given {
        Some(given) => given == current,
        // A stale token is refused by the caller as `changed`.
        None if context.interactive() => {
            eprint!("{}", summary(server, effects));
            super::confirm("Update Server")
        }
        None => false,
    }
}

fn offline_preview(routed: Routed, effects: UpdateEffects, token: String) -> Reply {
    let text = format!(
        "{}Confirm with: spaceterm server update --confirm {token}\n",
        summary(&routed.server.name, &effects)
    );
    let expires = std::time::SystemTime::now() + Duration::from_secs(600);
    let expires_at = jiff::Timestamp::try_from(expires)
        .map(|v| {
            jiff::Timestamp::from_second(v.as_second())
                .map(|v| v.to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Reply::new(
        &Envelope::confirmation(
            COMMAND,
            Some(routed.server),
            Confirmation {
                token,
                expires_at,
                effects,
            },
        ),
        text,
    )
}

fn offline_finish(
    routed: Routed,
    action: ServiceAction,
    status: &spaceterm_server::service::activation::ActivationStatus,
    to: Version,
) -> Reply {
    let (state, failure) = match status.phase {
        ActivationPhase::Completed { .. } => (ReceiptState::Done, None),
        ActivationPhase::Failed { .. } => (
            ReceiptState::Failed,
            Some(Failure::new(Code::StartupFailed)),
        ),
        ActivationPhase::Activating | ActivationPhase::Uncertain => (
            ReceiptState::Unresolved,
            Some(Failure::new(Code::ActivationUnresolved)),
        ),
    };
    let receipt = Receipt {
        operation: super::hex(&status.operation[..16]),
        action,
        state,
        code: failure.as_ref().map(|v| v.code),
        boot: String::new(),
    };
    finish(routed, receipt, failure, Some(to), Vec::new())
}
