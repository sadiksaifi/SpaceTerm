//! `server pair`: let another device pair with this server, on the server's own machine.
//!
//! It prints a one-use Invitation Link and, when stdout supports it, the same link as a QR
//! code, then waits for that invitation's claim and asks the person to type the Comparison
//! Code the joining device shows. A match approves the claim. A mismatch rejects it.
//! The invitation is withdrawn on end of input,
//! interrupt, timeout and every failure, so no link outlives the command.
mod qr;

use super::{Context, Reply};
use crate::contract::{self, Code, Envelope, Failure, Warning, WarningCode, WarningDetails};
use spaceterm_server::ServerConnection;
use spaceterm_server::direct::{ClaimId, PairingClaim};
use std::io::Write;
use std::time::{Duration, Instant};

const COMMAND: &str = "server.pair";
/// An invitation's lifetime, and the longest the command waits for its claim.
const LIFETIME: Duration = Duration::from_secs(5 * 60);
/// How often the command asks for its invitation's claim.
const POLL: Duration = Duration::from_secs(2);
/// The longest Comparison Code line the command reads.
const CODE_LIMIT: usize = 32;

/// What pairing needs from the server. The running server's account IPC provides it.
pub trait Invitations {
    fn invite(&self, advertise: Option<&str>) -> Result<(ClaimId, String, Duration), Failure>;
    fn claims(&self, invitation: &ClaimId) -> Result<Vec<PairingClaim>, Failure>;
    fn approve(&self, claim: &PairingClaim) -> Result<String, Failure>;
    fn reject(&self, claim: &ClaimId) -> Result<(), Failure>;
    fn withdraw(&self, invitation: &ClaimId) -> Result<(), Failure>;
}

impl Invitations for ServerConnection {
    fn invite(&self, advertise: Option<&str>) -> Result<(ClaimId, String, Duration), Failure> {
        let invitation = self
            .invite_device(advertise.map(str::to_owned))
            .map_err(super::local_direct)?;
        Ok((
            invitation.id(),
            invitation.display_text().to_string(),
            invitation.lifetime(),
        ))
    }
    fn claims(&self, invitation: &ClaimId) -> Result<Vec<PairingClaim>, Failure> {
        self.pairing_claims(Some(*invitation))
            .map_err(super::local_direct)
    }
    fn approve(&self, claim: &PairingClaim) -> Result<String, Failure> {
        self.approve_device(claim)
            .map(|device| device.name.as_str().to_owned())
            .map_err(super::local_direct)
    }
    fn reject(&self, claim: &ClaimId) -> Result<(), Failure> {
        self.reject_device(*claim).map_err(super::local_direct)
    }
    fn withdraw(&self, invitation: &ClaimId) -> Result<(), Failure> {
        self.withdraw_invitation(*invitation)
            .map_err(super::local_direct)
    }
}

/// The person at the terminal.
pub trait Person {
    /// Shows the Invitation Link and, when supported, its QR rendering on stdout.
    fn show_link(&mut self, link: &str) -> Result<(), Failure>;
    /// Says something on stderr.
    fn say(&mut self, text: &str);
    /// Reads the typed Comparison Code before `deadline`. `None` is end of input, an
    /// interrupt or the deadline.
    fn comparison_code(&mut self, deadline: Instant) -> Option<String>;
    fn interrupted(&self) -> bool;
    fn sleep(&mut self, duration: Duration);
}

struct Terminal;

impl Person for Terminal {
    fn show_link(&mut self, link: &str) -> Result<(), Failure> {
        let mut out = std::io::stdout().lock();
        qr::write_invitation(&mut out, link, qr::terminal_columns())
            .map_err(|_| Failure::new(Code::OutputFailed))
    }
    fn say(&mut self, text: &str) {
        let _ = write!(std::io::stderr().lock(), "{text}");
    }
    fn comparison_code(&mut self, deadline: Instant) -> Option<String> {
        super::read_line_until(CODE_LIMIT, Some(deadline))
    }
    fn interrupted(&self) -> bool {
        crate::interrupt::interrupted()
    }
    fn sleep(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while !crate::interrupt::interrupted() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// How a pairing ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paired {
    Approved { device: String },
    Failed { failure: Failure, withdrawn: bool },
}

/// The invitation flow, with the server and the person supplied.
pub fn pair(
    server: &dyn Invitations,
    person: &mut dyn Person,
    advertise: Option<&str>,
    limit: Duration,
) -> Paired {
    let (invitation, link, lifetime) = match server.invite(advertise) {
        Ok(invitation) => invitation,
        Err(failure) => {
            return Paired::Failed {
                failure,
                withdrawn: false,
            };
        }
    };
    let failed = |failure: Failure| {
        let withdrawn = server.withdraw(&invitation).is_ok();
        Paired::Failed { failure, withdrawn }
    };
    if let Err(failure) = person.show_link(&link) {
        return failed(failure);
    }
    person.say(
        "Open this Invitation Link on the device that joins. Waiting for it. Ctrl-C withdraws the invitation.\n",
    );
    let deadline = Instant::now() + limit.min(lifetime).min(LIFETIME);
    let claim = loop {
        if person.interrupted() {
            return failed(Failure::new(Code::Interrupted));
        }
        match server.claims(&invitation) {
            Ok(claims) => {
                if let Some(claim) = claims.into_iter().next() {
                    break claim;
                }
            }
            Err(failure) => return failed(failure),
        }
        if Instant::now() + POLL > deadline {
            return failed(Failure::new(Code::Timeout));
        }
        person.sleep(POLL);
    };
    person.say(&format!(
        "{} wants to pair. Type the Comparison Code it shows: ",
        claim.device_name.as_str()
    ));
    // The code is entered within the invitation's own lifetime.
    let Some(typed) = person.comparison_code(deadline) else {
        let _ = server.reject(&claim.id);
        let code = if person.interrupted() {
            Code::Interrupted
        } else if Instant::now() >= deadline {
            Code::Timeout
        } else {
            Code::InteractionRequired
        };
        return failed(Failure::new(code));
    };
    let typed: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
    if typed != claim.comparison_code().to_string() {
        let _ = server.reject(&claim.id);
        return failed(Failure::new(Code::ComparisonMismatch));
    }
    match server.approve(&claim) {
        Ok(device) => Paired::Approved { device },
        Err(failure) => failed(failure),
    }
}

/// A successful pairing's reply. The invitation was already shown, so the reply adds
/// nothing to stdout.
fn approved(server: Option<crate::contract::ServerRef>) -> Reply {
    Reply::new(
        &Envelope::ok(COMMAND, server, contract::Empty {}),
        String::new(),
    )
}

pub fn run(context: &Context<'_>, address: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    // Pairing needs a person at a terminal, so it creates no invitation without one.
    if !context.interactive() {
        return Reply::failure(COMMAND, None, Failure::new(Code::InteractionRequired));
    }
    if let Some(address) = address
        && spaceterm_server::direct::Endpoint::parse(address).is_err()
    {
        return Reply::failure(COMMAND, None, Failure::new(Code::InvalidAddress));
    }
    let server = super::local_ref(context.identity).ok();
    let connection = match context.local_connection() {
        Ok(connection) => connection,
        Err(failure) => return Reply::failure(COMMAND, server, failure),
    };
    // Ctrl-C withdraws the invitation, so while pairing runs it is only recorded.
    let _cooperation = match crate::interrupt::cooperate() {
        Ok(cooperation) => cooperation,
        Err(failure) => return Reply::failure(COMMAND, server, failure),
    };
    match pair(
        &connection,
        &mut Terminal,
        address,
        context.timeout(LIFETIME),
    ) {
        Paired::Approved { device } => {
            let _ = writeln!(std::io::stderr().lock(), "Paired {device}.");
            approved(server)
        }
        Paired::Failed { failure, withdrawn } => {
            let mut envelope = Envelope::error(COMMAND, server, failure, contract::Empty {});
            if withdrawn {
                envelope.warnings.push(Warning::new(
                    WarningCode::InvitationWithdrawn,
                    WarningDetails::None,
                ));
            }
            let text = if withdrawn {
                "The Invitation Link was withdrawn.\n".to_owned()
            } else {
                String::new()
            };
            let mut reply = Reply::new(&envelope, String::new());
            // The link already went to stdout; the rest of the story belongs on stderr.
            if !text.is_empty() {
                let _ = write!(std::io::stderr().lock(), "\n{text}");
            }
            reply.stdout.clear();
            reply
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Server {
        claims: Vec<PairingClaim>,
        log: RefCell<Vec<String>>,
    }
    impl Invitations for Server {
        fn invite(&self, advertise: Option<&str>) -> Result<(ClaimId, String, Duration), Failure> {
            self.log
                .borrow_mut()
                .push(format!("invite {}", advertise.unwrap_or("-")));
            Ok(([1; 32], "spaceterm://invite".into(), LIFETIME))
        }
        fn claims(&self, invitation: &ClaimId) -> Result<Vec<PairingClaim>, Failure> {
            assert_eq!(invitation, &[1; 32]);
            self.log.borrow_mut().push("claims".into());
            Ok(self.claims.clone())
        }
        fn approve(&self, claim: &PairingClaim) -> Result<String, Failure> {
            self.log.borrow_mut().push("approve".into());
            Ok(claim.device_name.as_str().to_owned())
        }
        fn reject(&self, _: &ClaimId) -> Result<(), Failure> {
            self.log.borrow_mut().push("reject".into());
            Ok(())
        }
        fn withdraw(&self, _: &ClaimId) -> Result<(), Failure> {
            self.log.borrow_mut().push("withdraw".into());
            Ok(())
        }
    }

    struct Typist {
        code: Option<String>,
        interrupt_after: Option<usize>,
        sleeps: usize,
        link: Option<String>,
    }
    impl Person for Typist {
        fn show_link(&mut self, link: &str) -> Result<(), Failure> {
            self.link = Some(link.to_owned());
            Ok(())
        }
        fn say(&mut self, _: &str) {}
        fn comparison_code(&mut self, _: Instant) -> Option<String> {
            self.code.clone()
        }
        fn interrupted(&self) -> bool {
            self.interrupt_after
                .is_some_and(|after| self.sleeps >= after)
        }
        fn sleep(&mut self, _: Duration) {
            self.sleeps += 1;
        }
    }

    fn claim() -> PairingClaim {
        PairingClaim {
            id: [2; 32],
            invitation_id: [1; 32],
            server: [3; 32],
            device_public_key: [4; 32],
            device_name: "Laptop".into(),
            expires_at_unix_seconds: u64::MAX,
            exchange_hash: [5; 32],
        }
    }

    fn typist(code: Option<String>) -> Typist {
        Typist {
            code,
            interrupt_after: None,
            sleeps: 0,
            link: None,
        }
    }

    #[test]
    fn a_matching_code_approves_the_invitations_own_claim() {
        let server = Server {
            claims: vec![claim()],
            log: RefCell::default(),
        };
        let mut person = typist(Some(format!(" {} ", claim().comparison_code())));
        let paired = pair(
            &server,
            &mut person,
            Some("build.example.test:9300"),
            LIFETIME,
        );
        assert_eq!(
            paired,
            Paired::Approved {
                device: "Laptop".into()
            }
        );
        assert_eq!(person.link.as_deref(), Some("spaceterm://invite"));
        assert_eq!(
            *server.log.borrow(),
            ["invite build.example.test:9300", "claims", "approve"]
        );
    }

    #[test]
    fn a_wrong_code_rejects_the_claim_and_withdraws_the_invitation() {
        let server = Server {
            claims: vec![claim()],
            log: RefCell::default(),
        };
        let wrong = if claim().comparison_code().to_string() == "000000" {
            "111111"
        } else {
            "000000"
        };
        let paired = pair(&server, &mut typist(Some(wrong.into())), None, LIFETIME);
        assert_eq!(
            paired,
            Paired::Failed {
                failure: Failure::new(Code::ComparisonMismatch),
                withdrawn: true
            }
        );
        assert_eq!(
            *server.log.borrow(),
            ["invite -", "claims", "reject", "withdraw"]
        );
    }

    #[test]
    fn a_successful_pairing_prints_nothing_more_on_stdout() {
        let reply = approved(None);
        assert!(reply.stdout.is_empty());
        assert_eq!(reply.exit(), 0);
    }

    #[test]
    fn a_code_not_entered_before_the_invitation_expires_rejects_and_withdraws() {
        let server = Server {
            claims: vec![claim()],
            log: RefCell::default(),
        };
        // The claim arrives at the end of the invitation's lifetime, and no code follows.
        assert_eq!(
            pair(&server, &mut typist(None), None, Duration::ZERO),
            Paired::Failed {
                failure: Failure::new(Code::Timeout),
                withdrawn: true
            }
        );
        assert_eq!(
            *server.log.borrow(),
            ["invite -", "claims", "reject", "withdraw"]
        );
    }

    #[test]
    fn interrupt_end_of_input_and_timeout_each_withdraw() {
        let waiting = || Server {
            claims: Vec::new(),
            log: RefCell::default(),
        };
        let server = waiting();
        let mut person = typist(None);
        person.interrupt_after = Some(2);
        assert_eq!(
            pair(&server, &mut person, None, LIFETIME),
            Paired::Failed {
                failure: Failure::new(Code::Interrupted),
                withdrawn: true
            }
        );
        assert_eq!(
            server.log.borrow().last().map(String::as_str),
            Some("withdraw")
        );
        let server = waiting();
        assert_eq!(
            pair(&server, &mut typist(None), None, Duration::from_secs(3)),
            Paired::Failed {
                failure: Failure::new(Code::Timeout),
                withdrawn: true
            }
        );
        let server = Server {
            claims: vec![claim()],
            log: RefCell::default(),
        };
        assert_eq!(
            pair(&server, &mut typist(None), None, LIFETIME),
            Paired::Failed {
                failure: Failure::new(Code::InteractionRequired),
                withdrawn: true
            }
        );
        assert_eq!(
            *server.log.borrow(),
            ["invite -", "claims", "reject", "withdraw"]
        );
    }
}
