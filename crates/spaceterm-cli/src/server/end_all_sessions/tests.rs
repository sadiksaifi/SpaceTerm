use super::*;
use crate::{
    args::Cli,
    contract::golden::assert_golden,
    scope::fixtures::{fixture, id},
};
use clap::Parser;
use spaceterm_server::hierarchy::{CloseInventory, ConfirmedPane, Id, Outcome};
use std::time::UNIX_EPOCH;

#[derive(Default)]
struct Journal(Vec<Mutation>, Option<Outcome>);
impl Dispatch for Journal {
    fn server_id(&self) -> Id {
        id(9)
    }
    fn client_id(&self) -> Id {
        id(5)
    }
    fn resolve_leftovers(&mut self) -> Result<Vec<contract::RecoveredOperation>, Failure> {
        Ok(Vec::new())
    }
    fn submit(&mut self, mutation: Mutation) -> Result<Outcome, Failure> {
        self.0.push(mutation);
        Ok(self
            .1
            .clone()
            .unwrap_or(Outcome::Accepted { target: id(9) }))
    }
}
fn live() -> Inventory {
    let mut inventory = fixture().inventory;
    inventory.close = Some(CloseInventory {
        boot: id(8),
        controls: Vec::new(),
        sessions: vec![
            ConfirmedPane {
                id: id(21),
                session: Some(id(31)),
            },
            ConfirmedPane {
                id: id(22),
                session: None,
            },
            ConfirmedPane {
                id: id(23),
                session: Some(id(33)),
            },
        ],
    });
    inventory
}
fn server() -> ServerRef {
    ServerRef {
        name: "local".into(),
        route: "local".into(),
        id: super::super::hex(&id(9)),
    }
}
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}
fn preview(inventory: &Inventory, journal: &mut dyn Dispatch) -> Reply {
    execute(inventory, journal, server(), None, now()).unwrap()
}
fn token(reply: &Reply) -> &str {
    reply.envelope.data.body["confirmation"]["token"]
        .as_str()
        .unwrap()
}

#[test]
fn preview_lists_only_live_sessions_without_dispatching() {
    let mut journal = Journal::default();
    let reply = preview(&live(), &mut journal);
    assert_eq!(reply.exit(), 3);
    assert!(journal.0.is_empty());
    assert_golden("server_end_all_sessions_preview", &reply.envelope);
    assert_eq!(
        reply.stdout,
        include_str!("../../../golden/server_end_all_sessions_preview.txt")
    );
}

#[test]
fn wrong_stale_and_expired_tokens_never_dispatch() {
    let inventory = live();
    let mut journal = Journal::default();
    let reply = preview(&inventory, &mut journal);
    let confirmed = token(&reply);
    for change in 0..7 {
        let mut changed = inventory.clone();
        let mut at = now();
        let mut given = confirmed.to_owned();
        match change {
            0 => given.push('0'),
            1 => changed.close.as_mut().unwrap().boot = id(99),
            2 => changed.close.as_mut().unwrap().sessions[0].session = Some(id(99)),
            3 => changed.close.as_mut().unwrap().sessions[1].session = Some(id(99)),
            4 => changed.close.as_mut().unwrap().sessions[0].session = None,
            5 => {
                changed.close.as_mut().unwrap().sessions.pop();
            }
            6 => at += offline_token::LIFETIME,
            _ => unreachable!(),
        }
        let Err(failure) = execute(&changed, &mut journal, server(), Some(&given), at) else {
            panic!("changed approval accepted");
        };
        assert_eq!(failure.code, Code::Changed);
        assert!(journal.0.is_empty());
    }
}

#[test]
fn confirmation_dispatches_the_exact_previewed_membership() {
    let inventory = live();
    let mut journal = Journal::default();
    let reply = preview(&inventory, &mut journal);
    let done = execute(
        &inventory,
        &mut journal,
        server(),
        Some(token(&reply)),
        now(),
    )
    .unwrap();
    assert_eq!(done.exit(), 0);
    assert_eq!(
        journal.0,
        vec![Mutation::EndAllSessions {
            confirmation: inventory.close.unwrap()
        }]
    );
    assert_eq!(
        done.envelope.data.body["ended"],
        reply.envelope.data.body["confirmation"]["effects"]["sessions"]
    );
    assert_golden("server_end_all_sessions_done", &done.envelope);
}

#[test]
fn paired_route_is_refused_before_local_state_or_transport_access() {
    let cli = Cli::try_parse_from([
        "spaceterm",
        "--server",
        "penguin",
        "server",
        "end-all-sessions",
        "--confirm",
        "invalid",
    ])
    .unwrap();
    assert_eq!(cli.command.path(), COMMAND);
    assert_eq!(
        crate::connection::route::validate_command(&cli.command, "penguin")
            .unwrap_err()
            .code,
        Code::LocalOnly
    );
    let reply = run(
        &Context {
            identity: "no-such-application",
            global: &cli.global,
        },
        Some("invalid"),
    );
    assert_eq!(reply.exit(), 5);
    assert_eq!(reply.envelope.error.as_ref().unwrap().code, Code::LocalOnly);
    assert_golden("server_end_all_sessions_local_only", &reply.envelope);
}

#[test]
fn server_refusal_after_review_is_changed_without_a_success_result() {
    let inventory = live();
    let mut journal = Journal(Vec::new(), Some(Outcome::StaleConfirmation));
    let reply = preview(&inventory, &mut journal);
    let Err(failure) = execute(
        &inventory,
        &mut journal,
        server(),
        Some(token(&reply)),
        now(),
    ) else {
        panic!("server refusal reported success");
    };
    assert_eq!(failure.code, Code::Changed);
}

#[test]
fn control_transfers_do_not_broaden_the_previewed_sessions() {
    let mut inventory = live();
    let mut journal = Journal::default();
    let reply = preview(&inventory, &mut journal);
    inventory.close.as_mut().unwrap().controls.push(
        spaceterm_server::hierarchy::ConfirmedControl {
            pane: id(21),
            session: Some(id(31)),
            owner: id(99),
            epoch: 2,
        },
    );
    assert_eq!(
        execute(
            &inventory,
            &mut journal,
            server(),
            Some(token(&reply)),
            now()
        )
        .unwrap()
        .exit(),
        0
    );
}
