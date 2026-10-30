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
    self, Code, Confirmation, Envelope, Failure, Receipt, ReceiptState, ServerRef, ServiceAction,
    UpdateCheck, UpdateEffects, UpdateKind, UpdateOperation, Version, Warning, WarningCode,
    WarningDetails,
};
use base64::Engine;
use spaceterm_server::service::activation::{ActivationPhase, ActivationStatus};
use spaceterm_server::service::lane::client::within;
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
        Err(error) => return Reply::routing_failure(COMMAND, error),
    };
    let pending = match Pending::open(context, &routed.route) {
        Ok(pending) => pending,
        Err(failure) => return Reply::failure(COMMAND, Some(routed.server), failure),
    };
    settle_first(routed.local, check, &mut || {
        let _ = Service::for_identity(context.identity).and_then(|v| v.recover_activation());
    });
    let opened = super::open_lane(context, &routed.connection, &routed.route, &pending);
    match opened {
        Ok(opened) => running(context, routed, opened, &pending, check, files, token),
        Err(_) if routed.local => stopped(context, routed, check, files, token),
        Err(failure) => Reply::failure(COMMAND, Some(routed.server), failure),
    }
}

/// An update on the server's own machine settles an interrupted activation first. Recovery
/// never repeats a dispatched step. `--check` is passive, so it never recovers.
fn settle_first(local: bool, check: bool, recover: &mut dyn FnMut()) {
    if local && !check {
        recover();
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

/// The envelope of `server update --check`.
pub(crate) fn check_envelope(
    server: ServerRef,
    data: UpdateCheck,
    recovered: Vec<contract::RecoveredOperation>,
) -> Envelope<UpdateCheck> {
    super::with_recovered(Envelope::ok(COMMAND, Some(server), data), recovered)
}

pub(crate) fn lane_check(check: spaceterm_server::service::lane::LaneCheck) -> UpdateCheck {
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
                Reply::new(&check_envelope(server, data, opened.recovered), text)
            }
            Ok(_) => fail(Failure::new(Code::ProtocolError), opened.recovered),
            Err(error) => fail(super::lane_failure(error), opened.recovered),
        };
    }
    // When the settled state allows return to the previous runtime, the return is the offered
    // action instead of an update.
    let probe = opened.session.request(
        &LaneRequest::Preview {
            action: LaneAction::Return,
        },
        deadline(),
    );
    match offered_return(probe) {
        Ok(Some((token_now, expires_at, effects))) => {
            let Some(to) = effects.to.as_ref().map(version) else {
                return fail(Failure::new(Code::ProtocolError), opened.recovered);
            };
            return match token {
                None => preview(
                    context,
                    routed,
                    opened,
                    pending,
                    (LaneAction::Return, to),
                    token_now,
                    expires_at,
                    &effects,
                ),
                Some(given) => execute(
                    context,
                    routed,
                    opened,
                    pending,
                    LaneAction::Return,
                    given.to_owned(),
                    to,
                ),
            };
        }
        Ok(None) => {}
        Err(failure) => return fail(failure, opened.recovered),
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

/// The return a server offers, from its reply to `Preview { Return }`. `return_unavailable`
/// means no return applies, so the update proceeds.
fn offered_return(
    reply: Result<LaneReply, spaceterm_server::ServerError>,
) -> Result<Option<(String, String, LaneEffects)>, Failure> {
    match reply {
        Ok(LaneReply::Preview {
            token,
            expires_at,
            effects,
        }) if effects.kind == Some(LaneUpdateKind::Return) => {
            Ok(Some((token, expires_at, effects)))
        }
        Ok(LaneReply::Error { code }) if code == "return_unavailable" => Ok(None),
        Ok(LaneReply::Error { code }) => Err(contract::from_lane_code(&code)),
        Ok(_) => Err(Failure::new(Code::ProtocolError)),
        Err(error) => Err(contract::from_server_error(error)),
    }
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
        let warnings: Vec<_> = major_warning(effects.to_major)
            .into_iter()
            .chain(super::recovered_warning(&opened.recovered))
            .collect();
        contract::write_warnings(&warnings, &mut std::io::stderr().lock());
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
    let text = format!(
        "{summary}Confirm with: {}\n",
        context.command_line(&routed.server, &format!("server update --confirm {token}"))
    );
    Reply::new(
        &preview_envelope(
            routed.server,
            token,
            expires_at,
            data,
            effects.to_major,
            opened.recovered,
        ),
        text,
    )
}

/// The confirmation envelope of an update or return preview.
pub(crate) fn preview_envelope(
    server: ServerRef,
    token: String,
    expires_at: String,
    effects: UpdateEffects,
    to_major: Option<u16>,
    recovered: Vec<contract::RecoveredOperation>,
) -> Envelope<contract::ConfirmationData<UpdateEffects>> {
    let mut envelope = Envelope::confirmation(
        COMMAND,
        Some(server),
        Confirmation {
            token,
            expires_at,
            effects,
        },
    );
    if let Some(warning) = major_warning(to_major) {
        envelope.warnings.push(warning);
    }
    super::with_recovered(envelope, recovered)
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
        &crate::interrupt::settle_by,
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
        receipt,
        failure,
        acknowledged,
        ..
    } = lifecycle::settle(ServiceAction::Update, outcome);
    if acknowledged {
        let _ = pending.remove(&receipt.operation);
    }
    finish(routed, receipt, failure, Some(candidate), recovered)
}

fn finish(
    routed: Routed,
    receipt: Receipt,
    failure: Option<Failure>,
    version: Option<Version>,
    recovered: Vec<contract::RecoveredOperation>,
) -> Reply {
    let text = match failure {
        None => "Server updated.\n".to_owned(),
        Some(_) => String::new(),
    };
    Reply::new(
        &operation_envelope(routed.server, receipt, failure, version, recovered),
        text,
    )
}

/// The envelope of a confirmed update or return.
pub(crate) fn operation_envelope(
    server: ServerRef,
    receipt: Receipt,
    failure: Option<Failure>,
    version: Option<Version>,
    recovered: Vec<contract::RecoveredOperation>,
) -> Envelope<UpdateOperation> {
    let data = UpdateOperation {
        operation: receipt.operation.clone(),
        version: version.unwrap_or(Version {
            version: String::new(),
            commit: String::new(),
            sha256: String::new(),
        }),
        receipt,
    };
    let envelope = match failure {
        None => Envelope::ok(COMMAND, Some(server), data),
        Some(failure) => Envelope::error(COMMAND, Some(server), failure, data),
    };
    super::with_recovered(envelope, recovered)
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
                Reply::new(&check_envelope(server, data, Vec::new()), text)
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
        let observed = match approval.confirmation_token() {
            Ok(observed) => observed,
            Err(error) => return fail(contract::from_server_error(error)),
        };
        match confirmed(
            context,
            &routed.server.name,
            &effects,
            ("return", &observed),
            token,
        ) {
            Ok(true) => {}
            Ok(false) => return offline_preview(context, routed, effects, ("return", &observed)),
            Err(failure) => return fail(failure),
        }
        let to = effects.to.clone();
        if let Err(failure) = crate::interrupt::dispatch() {
            return fail(failure);
        }
        let returned = within(ACTIVATION, &crate::interrupt::settle_by, move || {
            service.return_to_previous(approval)
        });
        return offline_activated(routed, ServiceAction::Return, returned, to);
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
    let observed = match approval.confirmation_token() {
        Ok(observed) => observed,
        Err(error) => return fail(contract::from_server_error(error)),
    };
    match confirmed(
        context,
        &routed.server.name,
        &effects,
        ("update", &observed),
        token,
    ) {
        Ok(true) => {}
        Ok(false) => return offline_preview(context, routed, effects, ("update", &observed)),
        Err(failure) => return fail(failure),
    }
    let to = effects.to.clone();
    if let Err(failure) = crate::interrupt::dispatch() {
        return fail(failure);
    }
    let activated = within(ACTIVATION, &crate::interrupt::settle_by, move || {
        service.activate(approval)
    });
    offline_activated(routed, ServiceAction::Update, activated, to)
}

type Activated = Result<ActivationStatus, spaceterm_server::ServerError>;

/// The thread that runs a dispatched offline activation. Once `settle_by` reports an interrupt,
/// [`within`] waits for its result only until that instant. A result that did not arrive by then
/// is settled by a later `server update` or `server start`.
const ACTIVATION: &str = "spaceterm-activation";

/// The reply of an offline activation, or an unresolved one when its result never arrived.
fn offline_activated(
    routed: Routed,
    action: ServiceAction,
    activated: Option<Activated>,
    to: Version,
) -> Reply {
    match activated {
        Some(Ok(status)) => offline_finish(routed, action, &status, to),
        Some(Err(error)) => Reply::failure(
            COMMAND,
            Some(routed.server),
            contract::from_server_error(error),
        ),
        None => {
            let failure = Failure::new(Code::ActivationUnresolved);
            // The activation chose its operation ID, which this command never received.
            let receipt = Receipt {
                operation: String::new(),
                action,
                state: ReceiptState::Unresolved,
                code: Some(failure.code),
                boot: String::new(),
            };
            finish(routed, receipt, Some(failure), Some(to), Vec::new())
        }
    }
}

/// Whether the change is confirmed: by `--confirm` with a token for this exact observation
/// that has not expired, or by the person at a prompt. Any other token is `changed`.
fn confirmed(
    context: &Context<'_>,
    server: &str,
    effects: &UpdateEffects,
    (purpose, observed): (&str, &str),
    given: Option<&str>,
) -> Result<bool, Failure> {
    match given {
        Some(given) => super::offline_token::verify(
            purpose,
            observed.as_bytes(),
            given,
            std::time::SystemTime::now(),
        )
        .map(|()| true),
        None if context.interactive() => {
            eprint!("{}", summary(server, effects));
            Ok(super::confirm("Update Server"))
        }
        None => Ok(false),
    }
}

fn offline_preview(
    context: &Context<'_>,
    routed: Routed,
    effects: UpdateEffects,
    (purpose, observed): (&str, &str),
) -> Reply {
    let issued =
        super::offline_token::issue(purpose, observed.as_bytes(), std::time::SystemTime::now());
    let text = format!(
        "{}Confirm with: {}\n",
        summary(&routed.server.name, &effects),
        context.command_line(
            &routed.server,
            &format!("server update --confirm {}", issued.token)
        )
    );
    Reply::new(
        &preview_envelope(
            routed.server,
            issued.token,
            issued.expires_at,
            effects,
            None,
            Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_never_recovers_and_only_a_local_update_does() {
        for (local, check, recovers) in [
            (true, true, false),
            (true, false, true),
            (false, true, false),
            (false, false, false),
        ] {
            let mut recovered = false;
            settle_first(local, check, &mut || recovered = true);
            assert_eq!(recovered, recovers, "local {local}, check {check}");
        }
    }

    fn effects(kind: LaneUpdateKind) -> LaneEffects {
        LaneEffects {
            ends_sessions: 0,
            sessions: Vec::new(),
            start_requires_machine: false,
            from: None,
            to: None,
            kind: Some(kind),
            to_major: None,
        }
    }

    #[test]
    fn a_running_server_offers_return_only_when_its_preview_is_a_return() {
        let preview = |kind| {
            Ok(LaneReply::Preview {
                token: "c1-00".into(),
                expires_at: "2026-10-30T12:10:00Z".into(),
                effects: effects(kind),
            })
        };
        assert!(matches!(
            offered_return(preview(LaneUpdateKind::Return)),
            Ok(Some((token, _, _))) if token == "c1-00"
        ));
        assert_eq!(
            offered_return(Ok(LaneReply::Error {
                code: "return_unavailable".into()
            })),
            Ok(None)
        );
        assert_eq!(
            offered_return(Ok(LaneReply::Error {
                code: "not_installed".into()
            })),
            Err(Failure::new(Code::NotInstalled))
        );
        assert_eq!(
            offered_return(preview(LaneUpdateKind::Update)),
            Err(Failure::new(Code::ProtocolError))
        );
    }
}
