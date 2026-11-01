//! `open`, `show` and `diagnostics`: requests to the desktop on this machine over its
//! account-local channel. `open` needs no server connection; the desktop checks the Session
//! context against what it presents. `show` resolves its target in
//! [the work executor](crate::work::execute). `diagnostics` reads the desktop's Terminal
//! Diagnostics counters and changes nothing.
use crate::args::{Cli, Command, OpenArgs};
use crate::contract::*;
use crate::work::output::{Output, value};
use spaceterm::desktop_channel::{ChannelError, Refusal, Reply, Request, SessionContext};
use std::path::Path;
use std::time::{Duration, Instant};

/// The desktop on this machine. Tests inject a double so they never reach a running desktop.
pub trait Desktop {
    fn request(&self, request: &Request) -> Result<Reply, ChannelError>;
}

/// This account's desktop of one application identity, asked by `deadline`. An interrupt
/// brings the deadline to the interrupt's settlement instant.
pub struct LocalDesktop<'a> {
    pub identity: &'a str,
    pub deadline: Instant,
}

impl Desktop for LocalDesktop<'_> {
    fn request(&self, request: &Request) -> Result<Reply, ChannelError> {
        spaceterm::desktop_channel::request(self.identity, request, &|| {
            crate::interrupt::limit(self.deadline)
        })
    }
}

/// How long `open` and `diagnostics` wait without `--timeout`.
pub const DESKTOP_TIMEOUT: Duration = Duration::from_secs(10);

/// A desktop that commands other than `open`, `show` and `diagnostics` never ask.
#[cfg(test)]
pub(crate) struct NoDesktop;

#[cfg(test)]
impl Desktop for NoDesktop {
    fn request(&self, _: &Request) -> Result<Reply, ChannelError> {
        panic!("only open, show and diagnostics ask the desktop")
    }
}

/// Asks the desktop one request, mapping each refusal and channel failure to its fixed code.
/// The request is the command's dispatched effect: after it, an interrupt waits for the answer
/// within the interrupt grace, and an answer that does not arrive leaves the outcome
/// unconfirmed.
pub fn ask(desktop: &dyn Desktop, request: &Request) -> Result<(), Failure> {
    crate::interrupt::dispatch()?;
    match classify(desktop.request(request), crate::interrupt::interrupted()) {
        None => Ok(()),
        Some(code) => Err(Failure::new(code)),
    }
}

/// The fixed code of one exchange's result, or `None` when the desktop did what was asked.
fn classify(result: Result<Reply, ChannelError>, interrupted: bool) -> Option<Code> {
    Some(match result {
        Ok(Reply::Done) => return None,
        // Only a Diagnostics request is answered with counters.
        Ok(Reply::Diagnostics(_)) => Code::DesktopIncompatible,
        Ok(Reply::Refused(refusal)) => match refusal {
            Refusal::NotShown => Code::NotShown,
            Refusal::RemoteSession => Code::RemoteSession,
            Refusal::FileUnavailable => Code::FileUnavailable,
            Refusal::PreviewUnavailable => Code::PreviewUnavailable,
            Refusal::Busy => Code::DesktopBusy,
            Refusal::Incompatible => Code::DesktopIncompatible,
        },
        Err(error) => match error {
            ChannelError::Unavailable => Code::DesktopUnavailable,
            ChannelError::UnsafeEndpoint => Code::UnsafeStorage,
            ChannelError::AccountMismatch => Code::AuthenticationRefused,
            // The desktop acts on nothing after the deadline its request carried, unless an
            // interrupt ended the wait sooner.
            ChannelError::Timeout if interrupted => Code::OutcomeUnconfirmed,
            ChannelError::Timeout => Code::Timeout,
            ChannelError::Unanswered => Code::OutcomeUnconfirmed,
            ChannelError::Protocol => Code::DesktopIncompatible,
        },
    })
}

/// The Session context `spaceterm` inherited from the Terminal Session it runs in.
pub struct Session<'a> {
    pub identity: Option<&'a str>,
    pub server: Option<&'a str>,
    pub pane: Option<&'a str>,
    pub session: Option<&'a str>,
}

impl Session<'_> {
    /// The context, when it names this executable's identity and three full IDs.
    fn context(&self, executable_identity: &str) -> Option<SessionContext> {
        if self.identity != Some(executable_identity) {
            return None;
        }
        Some(SessionContext {
            server: full_id(self.server?)?,
            pane: full_id(self.pane?)?,
            session: full_id(self.session?)?,
        })
    }
}

fn full_id(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut id = [0; 32];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(id)
}

fn hex(id: &[u8; 32]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Process entry for `open` and `diagnostics`.
pub fn start(cli: &Cli) -> Option<Output> {
    let desktop = LocalDesktop {
        identity: env!("SPACETERM_CLI_IDENTITY"),
        deadline: Instant::now() + cli.global.timeout.unwrap_or(DESKTOP_TIMEOUT),
    };
    match &cli.command {
        Command::Open(args) => Some(start_open(args, &desktop)),
        Command::Diagnostics => Some(diagnostics(&desktop)),
        _ => None,
    }
}

fn start_open(args: &OpenArgs, desktop: &LocalDesktop<'_>) -> Output {
    let identity = std::env::var("SPACETERM_IDENTITY").ok();
    let server = std::env::var("SPACETERM_SERVER_ID").ok();
    let pane = std::env::var("SPACETERM_PANE").ok();
    let session = std::env::var("SPACETERM_SESSION").ok();
    let session = Session {
        identity: identity.as_deref(),
        server: server.as_deref(),
        pane: pane.as_deref(),
        session: session.as_deref(),
    };
    match std::env::current_dir() {
        Ok(cwd) => open(args, desktop.identity, &session, &cwd, desktop),
        Err(_) => failed(Failure::new(Code::DirectoryUnavailable)),
    }
}

/// Reads the desktop's Terminal Diagnostics counters. The request has no effect, so it marks no
/// dispatch, and an interrupt ends it as any read does.
pub fn diagnostics(desktop: &dyn Desktop) -> Output {
    let counters = match desktop.request(&Request::Diagnostics) {
        Ok(Reply::Diagnostics(counters)) => counters,
        result => {
            let code = classify(result, false).unwrap_or(Code::DesktopIncompatible);
            return Output::failed(
                "diagnostics",
                None,
                Failure::new(code),
                serde_json::json!({}),
                String::new(),
            );
        }
    };
    Output {
        envelope: Envelope::ok(
            "diagnostics",
            None,
            value(DesktopDiagnostics {
                terminal_counters: counters,
            }),
        ),
        text: counters.lines(),
        diagnostics: String::new(),
    }
}

/// Opens `args.path`, or its File Preview, in the desktop showing the current Session.
pub fn open(
    args: &OpenArgs,
    identity: &str,
    session: &Session<'_>,
    cwd: &Path,
    desktop: &dyn Desktop,
) -> Output {
    let usage = |kind| {
        failed(Failure::with(
            Code::InvalidArguments,
            Details::Usage(UsageDetails {
                argument: Some("<PATH>".into()),
                kind,
            }),
        ))
    };
    let bytes = args.path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return usage(UsageKind::Other);
    }
    let path = cwd.join(&args.path);
    let Some(text) = path.to_str().map(str::to_owned) else {
        return usage(UsageKind::InvalidUtf8);
    };
    let Some(context) = session.context(identity) else {
        return failed(Failure::new(Code::ContextUnavailable));
    };
    let request = Request::Open {
        session: context,
        path,
        preview: args.preview,
    };
    if let Err(failure) = ask(desktop, &request) {
        return failed(failure);
    }
    let server = ServerRef {
        name: "local".into(),
        route: "local".into(),
        id: hex(&context.server),
    };
    let text_output = if args.preview {
        "Showed File Preview in the desktop.\n"
    } else {
        "Opened in the desktop.\n"
    };
    Output {
        envelope: Envelope::ok(
            "open",
            Some(server),
            value(Opened {
                path: text,
                preview: args.preview,
                pane: hex(&context.pane),
                session: hex(&context.session),
            }),
        ),
        text: text_output.into(),
        diagnostics: String::new(),
    }
}

fn failed(failure: Failure) -> Output {
    Output::failed("open", None, failure, serde_json::json!({}), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::golden::assert_golden;
    use std::cell::RefCell;
    use std::path::PathBuf;

    const IDENTITY: &str = "spaceterm-development";

    struct Recorded {
        answer: Result<Reply, ChannelError>,
        requests: RefCell<Vec<Request>>,
    }

    impl Recorded {
        fn answering(answer: Result<Reply, ChannelError>) -> Self {
            Self {
                answer,
                requests: RefCell::default(),
            }
        }
    }

    impl Desktop for Recorded {
        fn request(&self, request: &Request) -> Result<Reply, ChannelError> {
            self.requests.borrow_mut().push(request.clone());
            self.answer
        }
    }

    fn session() -> (String, String, String) {
        ("01".repeat(32), "02".repeat(32), "03".repeat(32))
    }

    fn run(path: &str, preview: bool, desktop: &Recorded) -> Output {
        let (server, pane, session) = session();
        open(
            &OpenArgs {
                path: path.into(),
                preview,
            },
            IDENTITY,
            &Session {
                identity: Some(IDENTITY),
                server: Some(&server),
                pane: Some(&pane),
                session: Some(&session),
            },
            Path::new("/work/api"),
            desktop,
        )
    }

    fn code(output: &Output) -> Option<&'static str> {
        output
            .envelope
            .error
            .as_ref()
            .map(|error| error.code.as_str())
    }

    #[test]
    fn open_and_preview_send_the_absolute_path_and_session_to_the_desktop() {
        for preview in [false, true] {
            let desktop = Recorded::answering(Ok(Reply::Done));
            let output = run("notes/plan.md", preview, &desktop);
            assert_eq!(output.envelope.exit(), 0);
            assert_eq!(
                desktop.requests.into_inner(),
                vec![Request::Open {
                    session: SessionContext {
                        server: [1; 32],
                        pane: [2; 32],
                        session: [3; 32],
                    },
                    path: PathBuf::from("/work/api/notes/plan.md"),
                    preview,
                }]
            );
            assert_golden(
                if preview { "open_preview" } else { "open" },
                &output.envelope,
            );
        }
    }

    #[test]
    fn open_outside_a_session_of_this_identity_asks_no_desktop() {
        let desktop = Recorded::answering(Ok(Reply::Done));
        let (server, pane, session) = session();
        for context in [
            Session {
                identity: None,
                server: Some(&server),
                pane: Some(&pane),
                session: Some(&session),
            },
            Session {
                identity: Some("spaceterm"),
                server: Some(&server),
                pane: Some(&pane),
                session: Some(&session),
            },
            Session {
                identity: Some(IDENTITY),
                server: Some(&server),
                pane: Some(&pane),
                session: None,
            },
            Session {
                identity: Some(IDENTITY),
                server: Some("0102"),
                pane: Some(&pane),
                session: Some(&session),
            },
        ] {
            let output = open(
                &OpenArgs {
                    path: "/work/plan.md".into(),
                    preview: false,
                },
                IDENTITY,
                &context,
                Path::new("/work"),
                &desktop,
            );
            assert_eq!(code(&output), Some("context_unavailable"));
            assert_eq!(output.envelope.exit(), 4);
        }
        assert!(desktop.requests.borrow().is_empty());
    }

    #[test]
    fn desktop_failures_and_refusals_have_fixed_codes_and_exits() {
        for (answer, expected, exit) in [
            (Err(ChannelError::Unavailable), "desktop_unavailable", 5),
            (
                Err(ChannelError::AccountMismatch),
                "authentication_refused",
                3,
            ),
            (Err(ChannelError::UnsafeEndpoint), "unsafe_storage", 5),
            (Err(ChannelError::Timeout), "timeout", 6),
            (Err(ChannelError::Unanswered), "outcome_unconfirmed", 7),
            (Ok(Reply::Refused(Refusal::Busy)), "desktop_busy", 5),
            (Ok(Reply::Refused(Refusal::NotShown)), "not_shown", 4),
            (
                Ok(Reply::Refused(Refusal::RemoteSession)),
                "remote_session",
                4,
            ),
            (
                Ok(Reply::Refused(Refusal::FileUnavailable)),
                "file_unavailable",
                4,
            ),
            (
                Ok(Reply::Refused(Refusal::PreviewUnavailable)),
                "preview_unavailable",
                5,
            ),
            (
                Ok(Reply::Refused(Refusal::Incompatible)),
                "desktop_incompatible",
                5,
            ),
        ] {
            let output = run(
                "/private/secret-plan.md",
                true,
                &Recorded::answering(answer),
            );
            assert_eq!(code(&output), Some(expected));
            assert_eq!(output.envelope.exit(), exit, "{expected}");
            let rendered = serde_json::to_string(&output.envelope).unwrap();
            assert!(!rendered.contains("secret-plan"), "{rendered}");
        }
        assert_golden(
            "open_desktop_unavailable",
            &run(
                "plan.md",
                false,
                &Recorded::answering(Err(ChannelError::Unavailable)),
            )
            .envelope,
        );
    }

    #[test]
    fn an_interrupt_that_ends_the_wait_leaves_the_outcome_unconfirmed() {
        assert_eq!(
            classify(Err(ChannelError::Timeout), true),
            Some(Code::OutcomeUnconfirmed)
        );
        assert_eq!(
            classify(Err(ChannelError::Timeout), false),
            Some(Code::Timeout)
        );
        assert_eq!(classify(Ok(Reply::Done), true), None);
    }

    #[test]
    fn diagnostics_reports_the_desktops_counters() {
        let counters = spaceterm_server::diagnostics::TerminalCounterSnapshot {
            pty_read_attempts: 9,
            pty_data_reads: 4,
            pty_read_bytes: 12_000,
            frames_decoded: 7,
            frames_painted: 5,
            ..Default::default()
        };
        let desktop = Recorded::answering(Ok(Reply::Diagnostics(counters)));

        let output = diagnostics(&desktop);

        assert_eq!(desktop.requests.into_inner(), vec![Request::Diagnostics]);
        assert_eq!(output.envelope.exit(), 0);
        assert_eq!(output.text, counters.lines());
        assert_golden("diagnostics", &output.envelope);
    }

    #[test]
    fn diagnostics_without_counters_has_a_fixed_code() {
        for (answer, expected, exit) in [
            (Err(ChannelError::Unavailable), "desktop_unavailable", 5),
            (Err(ChannelError::Timeout), "timeout", 6),
            (Ok(Reply::Done), "desktop_incompatible", 5),
            (
                Ok(Reply::Refused(Refusal::Incompatible)),
                "desktop_incompatible",
                5,
            ),
        ] {
            let output = diagnostics(&Recorded::answering(answer));
            assert_eq!(code(&output), Some(expected));
            assert_eq!(output.envelope.exit(), exit, "{expected}");
        }
        assert_golden(
            "diagnostics_desktop_unavailable",
            &diagnostics(&Recorded::answering(Err(ChannelError::Unavailable))).envelope,
        );
    }

    #[test]
    fn an_empty_path_is_a_usage_error_before_any_request() {
        let desktop = Recorded::answering(Ok(Reply::Done));
        let output = run("", false, &desktop);
        assert_eq!(code(&output), Some("invalid_arguments"));
        assert!(desktop.requests.borrow().is_empty());
    }
}
