//! Schema-1 output has a typed whitelist, never raw outcomes or native errors.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::{Value, json};
use spaceterm_client::management_cli::{AssessmentEvidence, ManagementAssessment};
use spaceterm_core::{
    management::{
        ManagementProgress, ManagementProgressState, ManagementRequestReference, ServerStatus,
    },
    operations::OperationId,
};
use spaceterm_host::native_management::controller::{
    ManagementPhase, ManagementProgress as NativeProgress, ProgressState,
};

use super::{
    CliFailure,
    bindings::{NativeObservation, PublicPairedDevice, RevocationState, UnpairReport},
    runner::CliIo,
};

pub const MAX_RESULT_BYTES: usize = 64 * 1024;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalStatus {
    pub native: Option<NativeObservation>,
    pub native_failure: Option<CliFailure>,
    pub authenticated: Option<ServerStatus>,
    pub negotiated_protocol_major: Option<u32>,
    pub authentication_failure: Option<CliFailure>,
}
#[derive(Clone, Debug)]
pub enum CliOutput {
    Status(LocalStatus),
    Assessment(ManagementAssessment),
    Progress(ManagementProgress),
    NativeProgress(NativeProgress),
    ServiceAssessment(crate::native::NativeActionPrompt),
    ServiceProgress(spaceterm_host::account_service::ServiceProgress),
    Unpair(UnpairReport),
    Devices(Vec<PublicPairedDevice>),
    Update {
        available_version: Option<String>,
    },
    Enrolled {
        server: spaceterm_core::identity::ServerId,
        client: spaceterm_core::identity::ClientId,
    },
}
impl CliOutput {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Progress(p) if p.state == ManagementProgressState::Unavailable => 69,
            Self::Progress(p) if p.state != ManagementProgressState::Completed => 75,
            Self::NativeProgress(p) if p.state != ProgressState::Complete => 75,
            Self::ServiceProgress(p) => match &p.state {
                spaceterm_host::account_service::ServiceRequestState::Completed(_) => 0,
                spaceterm_host::account_service::ServiceRequestState::NotApplied(failure) => {
                    crate::native::classify_service(*failure).exit_code()
                }
                _ => 75,
            },
            Self::Unpair(p) if p.revocation != RevocationState::Confirmed => 75,
            _ => 0,
        }
    }
    fn validate(&self) -> Result<(), CliFailure> {
        match self {
            Self::Devices(devices) if devices.len() > 128 => Err(CliFailure::LimitExceeded),
            Self::Update {
                available_version: Some(value),
            } if value.len() > 128 => Err(CliFailure::LimitExceeded),
            Self::NativeProgress(p)
                if p.request == [0; 16]
                    || (p.state == ProgressState::Complete
                        && p.phase != ManagementPhase::Completed) =>
            {
                Err(CliFailure::UnsafeNativeState)
            }
            Self::ServiceProgress(p)
                if matches!(
                    p.state,
                    spaceterm_host::account_service::ServiceRequestState::Completed(_)
                ) && p.phase != spaceterm_host::account_service::ServicePhase::Completed =>
            {
                Err(CliFailure::UnsafeNativeState)
            }
            Self::ServiceAssessment(a) if a.affected_sessions > 64 => {
                Err(CliFailure::LimitExceeded)
            }
            Self::Progress(p) if p.completed_units > p.total_units => {
                Err(CliFailure::UnsafeNativeState)
            }
            Self::Enrolled { server, client }
                if server.bytes() == [0; 16] || client.server() != *server =>
            {
                Err(CliFailure::UnsafeNativeState)
            }
            Self::Unpair(p)
                if p.server.bytes() == [0; 16]
                    || p.device.bytes() == [0; 16]
                    || p.pairing.server() != p.server =>
            {
                Err(CliFailure::UnsafeNativeState)
            }
            _ => Ok(()),
        }
    }
    fn value(&self) -> Value {
        let body = match self {
            Self::Status(status) => json!({
                "kind": "status",
                "native": status.native.as_ref().map(|n| json!({
                    "registration": n.availability,
                    "desired_state": n.desired,
                    "process": n.process,
                    "verified": n.verified,
                    "approved_installation": n.approved.map(|id| id.0.get()),
                    "staged_installation": n.staged.map(|id| id.0.get())
                })),
                "native_failure": status.native_failure,
                "authenticated": status.authenticated.as_ref().map(status_value),
                "negotiated_protocol_major": status.negotiated_protocol_major,
                "authentication_failure": status.authentication_failure
            }),
            Self::Assessment(a) => {
                json!({"kind":"assessment","server":hex(&a.action().server().bytes()),"action":a.action().name(),"token":a.token().as_str(),"description":assessment_description(a)})
            }
            Self::Progress(p) => {
                json!({"kind":"operation","request":match p.request {ManagementRequestReference::Operation(b)=>json!({"operation":operation_text(b.id)}),ManagementRequestReference::OperationReference(id)=>json!({"operation":operation_text(id)}),ManagementRequestReference::Native(bytes)=>json!({"native_request":hex(&bytes)})},"state":p.state,"completed_units":p.completed_units,"total_units":p.total_units})
            }
            Self::NativeProgress(p) => {
                json!({"kind":"native_operation","native_request":hex(&p.request),"state":format!("{:?}",p.state),"phase":format!("{:?}",p.phase)})
            }
            Self::ServiceAssessment(a) => {
                json!({"kind":"native_assessment","native_request":hex(a.request.as_bytes()),"action":a.action,"token":a.token.as_str(),"affected_sessions":a.affected_sessions,"description":a.description()})
            }
            Self::ServiceProgress(p) => {
                // Serialize only bounded lifecycle facts, never installation paths.
                let mut result = json!({"kind":"native_operation","native_request":hex(p.request.as_bytes()),"phase":p.phase,"state":service_state_name(&p.state),"failure":match p.state {spaceterm_host::account_service::ServiceRequestState::NotApplied(failure)=>Some(crate::native::classify_service(failure)),_=>None},"outcome":service_outcome_name(&p.state)});
                if let Some(operation) = starting_service_operation(p) {
                    result["follow_up"] = json!({"command":"status", "operation":operation});
                }
                result
            }
            Self::Unpair(p) => {
                json!({"kind":"unpair","server":hex(&p.server.bytes()),"device":hex(&p.device.bytes()),"pairing":p.pairing.local(),"local_connection_removed":p.local_connection_removed,"revocation":p.revocation})
            }
            Self::Devices(devices) => {
                json!({"kind":"paired_devices","devices":devices.iter().map(|p|json!({"server":hex(&p.server.bytes()),"device":hex(&p.device.bytes()),"pairing":p.pairing.local()})).collect::<Vec<_>>()})
            }
            Self::Update { available_version } => {
                json!({"kind":"update","available_version":available_version})
            }
            Self::Enrolled { server, client } => {
                json!({"kind":"enrolled","server":hex(&server.bytes()),"client":client.local()})
            }
        };
        json!({"schema":1,"result":body})
    }
}

fn starting_service_operation(
    progress: &spaceterm_host::account_service::ServiceProgress,
) -> Option<String> {
    (progress.phase == spaceterm_host::account_service::ServicePhase::Starting
        && progress.state == spaceterm_host::account_service::ServiceRequestState::Pending)
        .then(|| format!("native:{}", hex(progress.request.as_bytes())))
}

fn service_state_name(
    state: &spaceterm_host::account_service::ServiceRequestState,
) -> &'static str {
    use spaceterm_host::account_service::ServiceRequestState;
    match state {
        ServiceRequestState::Pending => "pending",
        ServiceRequestState::Completed(_) => "completed",
        ServiceRequestState::NotApplied(_) => "not_applied",
        ServiceRequestState::Indeterminate => "indeterminate",
    }
}
fn service_outcome_name(
    state: &spaceterm_host::account_service::ServiceRequestState,
) -> Option<&'static str> {
    use spaceterm_host::account_service::{ServiceOutcome, ServiceRequestState};
    let ServiceRequestState::Completed(outcome) = state else {
        return None;
    };
    Some(match outcome {
        ServiceOutcome::Installed { .. } => "installed",
        ServiceOutcome::Staged(_) => "staged",
        ServiceOutcome::Started => "started",
        ServiceOutcome::Stopped => "stopped",
        ServiceOutcome::Restarted => "restarted",
        ServiceOutcome::Activated(_) => "activated",
        ServiceOutcome::Uninstalled => "uninstalled",
    })
}
fn status_value(status: &ServerStatus) -> Value {
    let p = status.parts();
    json!({
        "server": hex(&p.server.bytes()),
        "epoch": p.epoch.get(),
        "storage_state": p.runtime.storage,
        "run_intent": p.runtime.intent,
        "revision": p.revision.get(),
        "storage_state": p.runtime.storage,
        "run_intent": p.runtime.intent,
        "running_version": p.running.as_ref().map(|v| v.as_str()),
        "approved_version": p.approved.as_ref().map(|v| v.as_str()),
        "staged_version": p.staged.as_ref().map(|v| v.as_str()),
        "native_verification": p.native_verification,
        "remote_access_enabled": p.remote_access.value.is_enabled(),
        "remote_access_version": p.remote_access.version.get(),
        "bind_scope": p.remote_access.value.endpoints(),
        "listener_count": p.listeners.len(),
        "listeners": p.listeners,
        "capabilities": p.capabilities,
        "progress": p.progress.iter().map(|progress| json!({
            "request": request_text(progress.request),
            "state": progress.state,
            "completed_units": progress.completed_units,
            "total_units": progress.total_units
        })).collect::<Vec<_>>()
    })
}
fn request_text(request: ManagementRequestReference) -> String {
    match request {
        ManagementRequestReference::Operation(binding) => operation_text(binding.id),
        ManagementRequestReference::OperationReference(id) => operation_text(id),
        ManagementRequestReference::Native(bytes) => format!("native:{}", hex(&bytes)),
    }
}

pub fn operation_text(id: OperationId) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        hex(&id.stream().server().bytes()),
        id.stream().pairing().local(),
        id.stream().client().local(),
        id.stream().key().local(),
        id.sequence().get()
    )
}
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}
pub fn assessment_description(assessment: &ManagementAssessment) -> String {
    let server = hex(&assessment.action().server().bytes());
    match assessment.evidence() {
        AssessmentEvidence::EndAllSessions(value) => format!(
            "End {} live Terminal Sessions on server {server} for every client. Clear their output and Scrollback. Keep the server, organization, grants and repository files.",
            value.summary().live_session_count()
        ),
        AssessmentEvidence::Unpair(value) => format!(
            "Revoke device {} pairing {} on server {server}. End its ordinary access without stopping Terminal Sessions.",
            hex(&value.parts().target.parts().device.bytes()),
            value.parts().target.parts().pairing.local()
        ),
        AssessmentEvidence::LocalSelfUnpair { .. } => format!(
            "Remove this device's saved connection to server {server} now. Retain a restricted revocation request until the server confirms. Keep Terminal Sessions running."
        ),
        AssessmentEvidence::RemoteAccess { .. } => {
            let spaceterm_client::management_cli::ManagementAction::RemoteAccess { policy } =
                assessment.action()
            else {
                return String::new();
            };
            if policy.is_enabled() {
                format!(
                    "Enable remote access on server {server} at {}.",
                    policy
                        .endpoints()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                format!("Disable remote listeners on server {server}. Keep private IPC available.")
            }
        }
        AssessmentEvidence::Enrollment => {
            format!("Authorize this exact enrollment request on server {server}.")
        }
    }
}
struct BoundedOutput(Vec<u8>);
impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RESULT_BYTES - self.0.len() {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded(value: &impl Serialize) -> Result<Vec<u8>, CliFailure> {
    let mut writer = BoundedOutput(Vec::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| CliFailure::LimitExceeded)?;
    writer
        .write_all(b"\n")
        .map_err(|_| CliFailure::LimitExceeded)?;
    Ok(writer.0)
}
pub fn emit(output: &CliOutput, json_output: bool, io: &mut dyn CliIo) -> Result<(), CliFailure> {
    output.validate()?;
    let value = output.value();
    validate_display_values(&value)?;
    let bytes = if json_output {
        encoded(&value)?
    } else {
        let text = human_output(output);
        if text.len() > MAX_RESULT_BYTES {
            return Err(CliFailure::LimitExceeded);
        }
        text.into_bytes()
    };
    io.write_stdout(&bytes)
}
fn validate_display_values(value: &Value) -> Result<(), CliFailure> {
    match value {
        Value::String(text) if text.chars().any(char::is_control) => {
            Err(CliFailure::UnsafeNativeState)
        }
        Value::Array(values) => values.iter().try_for_each(validate_display_values),
        Value::Object(values) => values.values().try_for_each(validate_display_values),
        _ => Ok(()),
    }
}
/// Announces a provider-issued durable reservation before polling any native
/// mutation or destructive assessment. Stderr leaves final stdout output intact.
/// This lookup reference is not completion acknowledgement or authority.
pub fn emit_native_request(
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    io: &mut dyn CliIo,
) -> Result<(), CliFailure> {
    io.write_stderr(&encoded(&json!({
        "schema": 1,
        "request": { "native_request": hex(request.as_bytes()) }
    }))?)
}

/// Raw authorization mode retains the original admitted request on failure.
/// This separate path leaves every normal schema-1 CLI envelope unchanged.
pub fn emit_authorization_failure(
    failure: &super::bindings::PairingAuthorizationFailure,
    json_output: bool,
    io: &mut dyn CliIo,
) -> Result<(), CliFailure> {
    io.write_authorization_diagnostic(&authorization_failure_bytes(failure, json_output)?)
}

/// Uses the same envelope while sharing flag ownership with the raw writer.
pub fn emit_authorization_failure_scoped(
    failure: &super::bindings::PairingAuthorizationFailure,
    json_output: bool,
    io: &mut dyn CliIo,
    signal: &super::runner::CliSignalContext,
) -> Result<(), CliFailure> {
    io.write_authorization_diagnostic_scoped(
        &authorization_failure_bytes(failure, json_output)?,
        signal,
    )
}

fn authorization_failure_bytes(
    failure: &super::bindings::PairingAuthorizationFailure,
    json_output: bool,
) -> Result<Vec<u8>, CliFailure> {
    let request_value = failure.request.as_deref().map(|&request| match request {
        ManagementRequestReference::Operation(binding) => {
            json!({"operation": operation_text(binding.id)})
        }
        ManagementRequestReference::OperationReference(id) => {
            json!({"operation": operation_text(id)})
        }
        ManagementRequestReference::Native(bytes) => json!({"native_request": hex(&bytes)}),
    });
    let bytes = if json_output {
        let mut envelope = json!({"schema": 1, "error": failure.failure});
        if let Some(request) = request_value {
            envelope["request"] = request;
        }
        encoded(&envelope)?
    } else if let Some(&request) = failure.request.as_deref() {
        format!("{} (request {})\n", failure.failure, request_text(request)).into_bytes()
    } else {
        format!("{}\n", failure.failure).into_bytes()
    };
    Ok(bytes)
}

pub fn emit_failure(
    failure: CliFailure,
    json_output: bool,
    io: &mut dyn CliIo,
) -> Result<(), CliFailure> {
    if json_output {
        io.write_stderr(&encoded(&json!({"schema":1,"error":failure}))?)
    } else {
        io.write_stderr(format!("{failure}\n").as_bytes())
    }
}

pub const USAGE: &str = "Usage: spaceterm-cli [--json] [--non-interactive] <command>\n\nstatus [--operation <server:pairing:client:stream:sequence|native:request>]\ninstall [--version <stable-version>] [--feed <local-json> --archive <local-tar-gz>]\nstart\nrestart [--assess | --confirm <assessment>]\nuninstall [--assess | --confirm <assessment>]\nstop [--assess | --confirm <assessment>]\npair enroll [--server-fingerprint <64 lowercase hex digits>] [--recover-account]\npair authorize <32 lowercase hex request digits> --device-fingerprint <64 lowercase hex digits>\npair authorize --claim-stdin --authorization-stdout --device-fingerprint <64 lowercase hex digits>\npair list\nunpair --device <32 lowercase hex digits> [--assess | --confirm <assessment>]\nunpair --retry-pending\nremote-access enable --bind <ip:port>... [--assess | --confirm <assessment>]\nremote-access disable [--assess | --confirm <assessment>]\nend-all-sessions [--assess | --confirm <assessment>]\nupdate check\nupdate stage [--version <stable-version>] [--feed <local-json> --archive <local-tar-gz>]\nupdate activate --installation <installation-id> [--assess | --confirm <assessment>]\n--help\n--version\nserve (requires the daemon's native-startup composition)\n\nRun as the owning account, not root. Noninteractive destructive actions require an exact, unexpired assessment. No discovery action installs, pairs, starts or updates a server. Local artifact actions read <local-json>.sig and requires the same release-key signatures as GitHub downloads.\n\nHeadless direct-pairing authorization: spaceterm-cli --non-interactive pair authorize --claim-stdin --authorization-stdout --device-fingerprint <64 lowercase hex digits>\nThe explicit pipe mode reads the exact JSON serialization of spaceterm_client_native::pairing_runtime::PublicDeviceClaim through a private anonymous stdin pipe until EOF (8192 bytes maximum). It writes only the existing opaque binary authorization delivery through a private anonymous stdout pipe (8192 bytes maximum), with no newline or JSON envelope, even with --json. Files, named FIFOs, sockets and terminals are rejected. Diagnostics use stderr; --json keeps typed failure envelopes and includes the original request after admission. The receiving client consumes the delivery with EnrollmentAuthorization::from_delivery and verifies it against its retained claim before enrollment in its own journal.\n";

pub fn emit_text(
    kind: &str,
    text: &str,
    json_output: bool,
    io: &mut dyn CliIo,
) -> Result<(), CliFailure> {
    if json_output {
        io.write_stdout(&encoded(&json!({"schema":1,kind:text}))?)
    } else {
        io.write_stdout(text.as_bytes())?;
        if !text.ends_with('\n') {
            io.write_stdout(b"\n")?;
        }
        Ok(())
    }
}
fn human_output(output: &CliOutput) -> String {
    match output {
        CliOutput::Status(s) => {
            let mut text = String::new();
            if let Some(n) = &s.native {
                text.push_str(&format!(
                    "Registration: {:?}\nDesired state: {:?}\nProcess observation: {:?} ({})\n",
                    n.availability,
                    n.desired,
                    n.process,
                    if n.verified { "verified" } else { "unverified" }
                ));
            }
            if let Some(f) = s.native_failure {
                text.push_str(&format!("Native status: {f}\n"));
            }
            if let Some(a) = &s.authenticated {
                let p = a.parts();
                let active = p
                    .listeners
                    .iter()
                    .filter(|listener| {
                        listener.state == spaceterm_core::management::ListenerState::Listening
                    })
                    .count();
                text.push_str(&format!("Authenticated server: {}\nRunning version: {}\nApproved version: {}\nStaged version: {}\nNative facts in server status: {:?}\nRemote-access policy: {}\nListening endpoints: {}\n", hex(&p.server.bytes()), p.running.as_ref().map_or("unavailable", |v| v.as_str()), p.approved.as_ref().map_or("unavailable", |v| v.as_str()), p.staged.as_ref().map_or("none", |v| v.as_str()), p.native_verification, if p.remote_access.value.is_enabled() { "enabled" } else { "disabled" }, active));
                text.push_str(&format!(
                    "Epoch: {}\nStorage state: {:?}\nRun intent: {:?}\n",
                    p.epoch.get(),
                    p.runtime.storage,
                    p.runtime.intent
                ));
                for listener in &p.listeners {
                    text.push_str(&format!(
                        "Listener {}: {:?}\n",
                        listener.endpoint, listener.state
                    ));
                }
                for progress in &p.progress {
                    text.push_str(&format!(
                        "Request {}: {:?} ({}/{})\n",
                        request_text(progress.request),
                        progress.state,
                        progress.completed_units,
                        progress.total_units
                    ));
                }
            } else {
                text.push_str("Authenticated details: unavailable\n");
            }
            if let Some(major) = s.negotiated_protocol_major {
                text.push_str(&format!("Negotiated protocol major: {major}\n"));
            }
            if let Some(f) = s.authentication_failure {
                text.push_str(&format!("Authentication: {f}\n"));
            }
            text
        }
        CliOutput::Assessment(a) => format!(
            "{}\nAssessment: {}\n",
            assessment_description(a),
            a.token().as_str()
        ),
        CliOutput::Progress(p) => format!(
            "Request: {}\nState: {:?}\nProgress: {}/{}\n",
            match p.request {
                ManagementRequestReference::Operation(b) => operation_text(b.id),
                ManagementRequestReference::OperationReference(id) => operation_text(id),
                ManagementRequestReference::Native(bytes) => format!("native:{}", hex(&bytes)),
            },
            p.state,
            p.completed_units,
            p.total_units
        ),
        CliOutput::NativeProgress(p) => format!(
            "Request: native:{}\nState: {:?}\nPhase: {:?}\n",
            hex(&p.request),
            p.state,
            p.phase
        ),
        CliOutput::ServiceAssessment(a) => format!(
            "{}\nRequest: native:{}\nAssessment: {}\n",
            a.description(),
            hex(a.request.as_bytes()),
            a.token.as_str()
        ),
        CliOutput::ServiceProgress(p) => {
            let mut result = format!(
                "Request: native:{}\nState: {}\nPhase: {:?}\nOutcome: {}\n",
                hex(p.request.as_bytes()),
                service_state_name(&p.state),
                p.phase,
                service_outcome_name(&p.state).unwrap_or("unresolved")
            );
            if let Some(operation) = starting_service_operation(p) {
                result.push_str(&format!(
                    "Server is still starting. Check status --operation {operation}.\n"
                ));
            }
            result
        }
        CliOutput::Unpair(p) => format!(
            "Server: {}\nDevice: {}\nLocal connection removed: {}\nServer revocation: {:?}\n",
            hex(&p.server.bytes()),
            hex(&p.device.bytes()),
            p.local_connection_removed,
            p.revocation
        ),
        CliOutput::Devices(devices) => {
            if devices.is_empty() {
                "No paired devices.\n".to_owned()
            } else {
                devices
                    .iter()
                    .map(|p| {
                        format!(
                            "Device: {}  Pairing: {}  Server: {}\n",
                            hex(&p.device.bytes()),
                            p.pairing.local(),
                            hex(&p.server.bytes())
                        )
                    })
                    .collect()
            }
        }
        CliOutput::Update { available_version } => format!(
            "Available server update: {}\n",
            available_version.as_deref().unwrap_or("none")
        ),
        CliOutput::Enrolled { server, client } => format!(
            "Enrolled CLI Client {} on server {}.\n",
            client.local(),
            hex(&server.bytes())
        ),
    }
}
