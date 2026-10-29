use super::*;
use crate::{
    args::Cli,
    contract::golden::assert_golden,
    scope::fixtures::{fixture, id},
};
use clap::Parser;
use spaceterm_server::{
    hierarchy::{CloseInventory, ConfirmedPane, Inventory},
    worktree::ReadReply,
};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

#[derive(Default)]
struct Script {
    inventories: VecDeque<Inventory>,
    replies: VecDeque<ReadReply>,
    outcomes: VecDeque<Result<Outcome, Failure>>,
    sent: Vec<Mutation>,
    events: Vec<&'static str>,
    recovery_failure: Option<Failure>,
    recovered: Vec<RecoveredOperation>,
}
#[derive(Clone)]
struct Double(Rc<RefCell<Script>>);
impl Dispatch for Double {
    fn server_id(&self) -> Id {
        id(9)
    }
    fn client_id(&self) -> Id {
        id(5)
    }
    fn resolve_leftovers(&mut self) -> Result<Vec<RecoveredOperation>, Failure> {
        let mut script = self.0.borrow_mut();
        script.events.push("recover");
        match &script.recovery_failure {
            Some(failure) => Err(failure.clone()),
            None => Ok(script.recovered.clone()),
        }
    }
    fn submit(&mut self, mutation: Mutation) -> Result<Outcome, Failure> {
        let mut script = self.0.borrow_mut();
        script.events.push("submit");
        script.sent.push(mutation);
        script.outcomes.pop_front().expect("unexpected dispatch")
    }
}
impl Read for Double {
    fn inventory(&mut self) -> Result<Inventory, Failure> {
        let mut s = self.0.borrow_mut();
        s.events.push("inventory");
        Ok(s.inventories.pop_front().expect("unexpected snapshot"))
    }
    fn home(&mut self) -> Result<std::path::PathBuf, Failure> {
        self.0.borrow_mut().events.push("home");
        Ok("/server/home".into())
    }
    fn worktree(&mut self, _read: WorktreeRead) -> Result<ReadReply, Failure> {
        self.0
            .borrow_mut()
            .replies
            .pop_front()
            .ok_or_else(|| Failure::new(Code::ProtocolError))
    }
    fn panes(
        &mut self,
        snapshot: &Snapshot,
        target: &Resolved,
    ) -> Result<Vec<list::PaneFacts>, Failure> {
        Ok(snapshot
            .inventory
            .workspaces
            .iter()
            .filter(|w| target.workspace.is_none_or(|id| w.id == id))
            .flat_map(|w| &w.tabs)
            .filter(|t| target.tab.is_none_or(|id| t.id == id))
            .flat_map(|t| &t.panes)
            .filter(|p| target.pane.is_none_or(|id| p.id == id))
            .map(|p| list::PaneFacts {
                pane: p.id,
                session: PaneSession {
                    id: None,
                    state: SessionStateLabel::NotStarted,
                },
                control: PaneControl {
                    state: ControlState::None,
                    client_name: None,
                },
            })
            .collect())
    }
}
fn env() -> Environment<'static> {
    Environment {
        identity: "test",
        local: true,
        cwd: Path::new("/client/cwd"),
        current_identity: None,
        current_server: None,
        current_pane: None,
        agent: true,
        now: 100,
        confirmation: None,
    }
}
fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("spaceterm").chain(args.iter().copied())).unwrap()
}
fn run_script(args: &[&str], env: &Environment<'_>, script: &Rc<RefCell<Script>>) -> Output {
    let cli = cli(args);
    run(
        &cli.command,
        &cli.global,
        env,
        &mut Double(script.clone()),
        &mut Double(script.clone()),
        "fixture",
        "local",
    )
}
fn live(mut inventory: Inventory) -> Inventory {
    inventory.close = Some(CloseInventory {
        boot: id(9),
        controls: Vec::new(),
        sessions: inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|p| ConfirmedPane {
                id: p.id,
                session: Some(p.id),
            })
            .collect(),
    });
    inventory
}

#[test]
fn workspace_creation_reports_the_server_chosen_directory_after_journal_acknowledgement() {
    let initial = fixture().inventory;
    let mut updated = initial.clone();
    let mut created = updated.workspaces[1].clone();
    created.id = id(44);
    created.name = "new".into();
    created.root_tab = id(45);
    created.tabs[0].id = id(45);
    created.tabs[0].root_pane = id(46);
    created.tabs[0].panes[0].id = id(46);
    created.tabs[0].panes[0].starting_directory = "/server/chosen/new".into();
    created.pinned_directory = Some("/server/chosen/new".into());
    updated.workspaces.push(created);
    updated.revision = 8;
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial, updated]),
        outcomes: VecDeque::from([Ok(Outcome::Accepted { target: id(44) })]),
        ..Default::default()
    }));
    let output = run_script(
        &["workspace", "create", "new", "--directory", "~/new"],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.exit(), 0);
    assert_golden("work_workspace_created", &output.envelope);
    assert_eq!(output.envelope.data.body["directory"], "/server/chosen/new");
    let script = script.borrow();
    assert_eq!(
        script.events,
        ["recover", "inventory", "home", "submit", "inventory"]
    );
    assert!(
        matches!(&script.sent[0],Mutation::CreateWorkspace {directory:Some(p),control:CreateControl::Creator,..} if p==Path::new("/server/home/new"))
    );
}

#[test]
fn remote_relative_path_usage_fails_before_recovery_or_any_dispatch() {
    let script = Rc::new(RefCell::new(Script::default()));
    let mut env = env();
    env.local = false;
    let output = run_script(
        &[
            "workspace",
            "create",
            "private-title",
            "--directory",
            "private-path",
        ],
        &env,
        &script,
    );
    assert_eq!(output.envelope.exit(), 2);
    assert!(script.borrow().events.is_empty());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(output.write(true, false, true, &mut stdout, &mut stderr), 2);
    let encoded = String::from_utf8(stdout).unwrap();
    assert!(!encoded.contains("private-title"));
    assert!(!encoded.contains("private-path"));
    assert!(stderr.is_empty());
}

#[test]
fn workspace_kill_previews_exact_work_then_refuses_changed_sessions() {
    let initial = live(fixture().inventory);
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial.clone()]),
        ..Default::default()
    }));
    let output = run_script(
        &["workspace", "kill", "--workspace", "api"],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.exit(), 3);
    assert!(script.borrow().sent.is_empty());
    assert_golden("work_workspace_kill_preview", &output.envelope);
    let token = output.envelope.data.body["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut changed = initial;
    changed.close.as_mut().unwrap().sessions[0].session = Some(id(99));
    script.borrow_mut().inventories.push_back(changed);
    let mut confirmed = env();
    confirmed.confirmation = Some(&token);
    let output = run_script(
        &["workspace", "kill", "--workspace", "api"],
        &confirmed,
        &script,
    );
    assert_eq!(output.envelope.error.unwrap().code, Code::Changed);
    assert!(script.borrow().sent.is_empty());
}

#[test]
fn fallback_creation_must_be_accepted_before_a_fresh_close_preview() {
    let mut initial = live(fixture().inventory);
    initial.workspaces[0].tabs.remove(0);
    initial.workspaces[0].root_tab = id(12);
    initial
        .close
        .as_mut()
        .unwrap()
        .sessions
        .retain(|s| s.id != id(21));
    let mut updated = initial.clone();
    let mut outside = fixture().inventory.workspaces[0].tabs[0].clone();
    outside.id = id(40);
    outside.root_pane = id(41);
    outside.panes[0].id = id(41);
    updated.workspaces[0].tabs.push(outside);
    updated = live(updated);
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial, updated]),
        outcomes: VecDeque::from([Ok(Outcome::Accepted { target: id(40) })]),
        ..Default::default()
    }));
    let output = run_script(
        &[
            "tab",
            "close",
            "--workspace",
            "api",
            "--worktree",
            "feature/x",
            "--all",
        ],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.exit(), 3);
    let script = script.borrow();
    assert_eq!(script.sent.len(), 1);
    assert!(
        matches!(&script.sent[0],Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab {path,..}) if path==Path::new("/repo"))
    );
    let sessions = &output.envelope.data.body["confirmation"]["effects"]["sessions"];
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["pane"], hex(&id(22)));
}

#[test]
fn failed_fallback_keeps_all_original_tabs_and_dispatches_no_close() {
    let mut initial = live(fixture().inventory);
    initial.workspaces[0].tabs.remove(0);
    initial.workspaces[0].root_tab = id(12);
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial]),
        outcomes: VecDeque::from([Ok(Outcome::WorktreeFailed(
            spaceterm_server::worktree::Error::Missing,
        ))]),
        ..Default::default()
    }));
    let output = run_script(
        &[
            "tab",
            "close",
            "--workspace",
            "api",
            "--worktree",
            "feature/x",
            "--all",
        ],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.exit(), 4);
    assert_eq!(script.borrow().sent.len(), 1);
}

#[test]
fn list_output_keeps_operational_titles_and_paths_out_of_diagnostics() {
    let mut initial = fixture().inventory;
    initial.workspaces[0].tabs[0].panes[0]
        .presentation
        .as_mut()
        .unwrap()
        .title = "private-title".into();
    initial.workspaces[0].tabs[0].panes[0].current_directory = Some("/private-path".into());
    let server_facts =
        spaceterm_server::worktree::ServerFacts::new("fixture", "test", "1").unwrap();
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial]),
        replies: VecDeque::from([ReadReply::ServerFacts(server_facts)]),
        ..Default::default()
    }));
    let output = run_script(&["list"], &env(), &script);
    assert_eq!(output.envelope.exit(), 0);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    output.write(false, false, true, &mut stdout, &mut stderr);
    let text = String::from_utf8(stdout).unwrap();
    let diagnostics = String::from_utf8(stderr).unwrap();
    assert!(text.contains("private-title"));
    assert!(text.contains("/private-path"));
    assert!(!diagnostics.contains("private-title"));
    assert!(!diagnostics.contains("/private-path"));
    assert!(script.borrow().sent.is_empty());
}

#[test]
fn recovery_failure_prevents_new_work() {
    let script = Rc::new(RefCell::new(Script {
        recovery_failure: Some(Failure::new(Code::OutcomeUnconfirmed)),
        ..Default::default()
    }));
    let output = run_script(&["list"], &env(), &script);
    assert_eq!(output.envelope.exit(), 7);
    assert_eq!(script.borrow().events, ["recover"]);
}

#[test]
fn a_broken_output_pipe_is_silent_and_exits_141() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = Output::failed(
        "list",
        None,
        Failure::new(Code::NotFound),
        serde_json::json!({}),
        "operational-data".into(),
    );
    let mut errors = Vec::new();
    assert_eq!(
        output.write(false, false, true, &mut Broken, &mut errors),
        141
    );
    assert!(errors.is_empty());
}

#[test]
fn an_anchor_that_changes_worktree_during_fallback_cannot_change_the_close_scope() {
    let mut initial = live(fixture().inventory);
    initial.workspaces[0].tabs.remove(0);
    initial.workspaces[0].root_tab = id(12);
    let mut updated = initial.clone();
    updated.workspaces[0].tabs[0].worktree = Some("/repo".into());
    let mut outside = fixture().inventory.workspaces[0].tabs[0].clone();
    outside.id = id(40);
    outside.root_pane = id(41);
    outside.panes[0].id = id(41);
    updated.workspaces[0].tabs.push(outside);
    updated = live(updated);
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial, updated]),
        outcomes: VecDeque::from([Ok(Outcome::Accepted { target: id(40) })]),
        ..Default::default()
    }));
    let output = run_script(
        &[
            "tab",
            "close",
            "--workspace",
            "api",
            "--worktree",
            "feature/x",
            "--all",
        ],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.error.unwrap().code, Code::Changed);
    assert_eq!(script.borrow().sent.len(), 1);
}

#[test]
fn renames_use_the_frozen_name_version_and_return_the_accepted_name() {
    let initial = fixture().inventory;
    let mut updated = initial.clone();
    updated.workspaces[0].name = "api-v2".into();
    updated.workspaces[0].name_version = 3;
    updated.revision = 8;
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial, updated]),
        outcomes: VecDeque::from([Ok(Outcome::Accepted { target: id(1) })]),
        ..Default::default()
    }));
    let output = run_script(
        &["workspace", "rename", "api-v2", "--workspace", "api"],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.exit(), 0);
    assert_golden("work_workspace_renamed", &output.envelope);
    assert!(
        matches!(&script.borrow().sent[0],Mutation::RenameWorkspace {workspace,version:2,name} if *workspace==id(1)&&name=="api-v2")
    );
}

#[test]
fn tab_and_split_creation_report_server_directories_and_stable_root_sources() {
    for split in [false, true] {
        let initial = fixture().inventory;
        let mut updated = initial.clone();
        updated.revision = 8;
        let mut pane = updated.workspaces[1].tabs[0].panes[0].clone();
        pane.id = id(41);
        pane.starting_directory = "/server/chosen".into();
        pane.presentation = None;
        pane.creator = Some(id(5));
        let outcome = if split {
            pane.source = Some(id(23));
            pane.axis = hierarchy::Axis::Down;
            updated.workspaces[1].tabs[0].panes.push(pane);
            id(41)
        } else {
            let mut tab = updated.workspaces[1].tabs[0].clone();
            tab.id = id(40);
            tab.root_pane = id(41);
            tab.panes = vec![pane];
            updated.workspaces[1].tabs.push(tab);
            id(40)
        };
        let script = Rc::new(RefCell::new(Script {
            inventories: VecDeque::from([initial, updated]),
            outcomes: VecDeque::from([Ok(Outcome::Accepted { target: outcome })]),
            ..Default::default()
        }));
        let args = if split {
            vec!["pane", "split", "--tab", "id:0d0d0d", "--down"]
        } else {
            vec!["tab", "create", "--workspace", "tools"]
        };
        let output = run_script(&args, &env(), &script);
        assert_eq!(output.envelope.exit(), 0);
        assert_eq!(
            output.envelope.data.body["pane"]["directory"],
            "/server/chosen"
        );
        assert_eq!(output.envelope.data.body["directory"], "/server/chosen");
        assert_eq!(output.envelope.data.body["source_pane"], hex(&id(23)));
        assert_golden(
            if split {
                "work_pane_split"
            } else {
                "work_tab_created"
            },
            &output.envelope,
        );
    }
}

#[test]
fn confirmed_pane_and_tab_closes_submit_only_the_original_review() {
    for pane in [false, true] {
        let initial = live(fixture().inventory);
        let target = if pane {
            CloseTarget::Pane {
                workspace: id(1),
                tab: id(11),
                pane: id(21),
            }
        } else {
            CloseTarget::Tab {
                workspace: id(1),
                tab: id(11),
            }
        };
        let confirmation = initial.close_confirmation(target).unwrap();
        let (token, _) = consent::issue(
            id(9),
            id(5),
            consent::Review::Close {
                target,
                confirmation: confirmation.clone(),
            },
            100,
        )
        .unwrap();
        let mut updated = initial.clone();
        updated.workspaces[0].tabs.remove(0);
        updated.workspaces[0].root_tab = id(12);
        updated.revision = 8;
        let script = Rc::new(RefCell::new(Script {
            inventories: VecDeque::from([initial, updated]),
            outcomes: VecDeque::from([Ok(Outcome::Accepted { target: id(11) })]),
            ..Default::default()
        }));
        let args = if pane {
            vec!["pane", "close", "--pane", "id:151515"]
        } else {
            vec!["tab", "close", "--tab", "id:0b0b0b"]
        };
        let mut env = env();
        env.confirmation = Some(&token);
        let output = run_script(&args, &env, &script);
        assert_eq!(output.envelope.exit(), 0);
        assert_golden(
            if pane {
                "work_pane_closed"
            } else {
                "work_tab_closed"
            },
            &output.envelope,
        );
        assert_eq!(
            script.borrow().sent,
            [Mutation::Close {
                target,
                confirmation
            }]
        );
    }
}

#[test]
fn worktree_creation_uses_the_main_branch_and_opens_exactly_one_tab() {
    use spaceterm_server::worktree::{Branch, BranchName, Branches, Operation, Settings};
    for open in [false, true] {
        let initial = fixture().inventory;
        let mut updated = initial.clone();
        let mut tab = updated.workspaces[0].tabs[0].clone();
        tab.id = id(40);
        tab.root_pane = id(41);
        tab.worktree = Some("/server/canonical/feature-new".into());
        tab.panes[0].id = id(41);
        tab.panes[0].starting_directory = "/server/canonical/feature-new".into();
        tab.panes[0].presentation = None;
        tab.panes[0].creator = Some(id(5));
        updated.workspaces[0].tabs.push(tab);
        updated.revision = 8;
        let script = Rc::new(RefCell::new(Script {
            inventories: VecDeque::from([initial, updated]),
            replies: VecDeque::from([ReadReply::Context {
                home: "/server/home".into(),
                settings: Settings::default(),
                branches: Branches {
                    local: vec!["main".into()],
                    remote: Vec::new(),
                },
            }]),
            outcomes: VecDeque::from([Ok(Outcome::Accepted { target: id(40) })]),
            ..Default::default()
        }));
        let mut args = vec!["worktree", "create", "feature/new", "--workspace", "api"];
        if open {
            args.push("--open");
        }
        let output = run_script(&args, &env(), &script);
        assert_eq!(output.envelope.exit(), 0);
        assert_golden("work_worktree_created", &output.envelope);
        assert_eq!(
            output.envelope.data.body["worktree"]["path"],
            "/server/canonical/feature-new"
        );
        assert_eq!(
            output.envelope.data.body["directory"],
            "/server/canonical/feature-new"
        );
        assert_eq!(output.envelope.data.body["control"], "self");
        assert_eq!(output.envelope.data.body["tab"]["id"], hex(&id(40)));
        assert_eq!(script.borrow().sent.len(), 1);
        assert!(
            matches!(&script.borrow().sent[0],Mutation::Worktree(Operation::Create {branch:Branch::New {name,base:BranchName::Local(base)},path,..}) if name=="feature/new"&&base=="main"&&path==Path::new("/server/home/.worktrees/repo/feature-new"))
        );
    }
}

#[test]
fn worktree_removal_reuses_the_server_approval_without_requesting_a_new_one() {
    use spaceterm_server::worktree::{AffectedTab, Operation};
    let initial = live(fixture().inventory);
    let removal = Removal {
        boot: id(9),
        confirmation: id(70),
        root: "/repo".into(),
        path: "/trees/feature".into(),
        changes: false,
        submodules: false,
        tabs: vec![AffectedTab {
            workspace: id(1),
            tab: id(12),
            panes: vec![ConfirmedPane {
                id: id(22),
                session: Some(id(22)),
            }],
        }],
    };
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([initial.clone()]),
        replies: VecDeque::from([ReadReply::Removal(removal.clone())]),
        ..Default::default()
    }));
    let output = run_script(
        &[
            "worktree",
            "remove",
            "--workspace",
            "api",
            "--worktree",
            "feature/x",
        ],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.exit(), 3);
    assert_golden("work_worktree_remove_preview", &output.envelope);
    let token = output.envelope.data.body["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut updated = initial.clone();
    updated.workspaces[0].tabs.remove(1);
    updated.revision = 8;
    {
        let mut script = script.borrow_mut();
        script.inventories.extend([initial, updated]);
        script
            .outcomes
            .push_back(Ok(Outcome::Accepted { target: id(12) }));
    }
    let mut env = env();
    env.confirmation = Some(&token);
    let output = run_script(
        &[
            "worktree",
            "remove",
            "--workspace",
            "api",
            "--worktree",
            "feature/x",
        ],
        &env,
        &script,
    );
    assert_eq!(output.envelope.exit(), 0);
    assert_golden("work_worktree_removed", &output.envelope);
    assert!(
        matches!(&script.borrow().sent[0],Mutation::Worktree(Operation::Remove {confirmation,boot,..}) if *confirmation==id(70)&&*boot==id(9))
    );
}

#[test]
fn every_command_reports_settled_leftovers_and_a_bounded_warning() {
    let recovered = RecoveredOperation {
        operation: "7".into(),
        identity: RecoveredIdentity::Base,
        kind: RecoveredKind::Mutation,
        outcome: RecoveredOutcome {
            state: RecoveredState::Applied,
            code: None,
            target: Some(hex(&id(1))),
        },
    };
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([fixture().inventory]),
        replies: VecDeque::from([ReadReply::ServerFacts(
            spaceterm_server::worktree::ServerFacts::new("fixture", "test", "1").unwrap(),
        )]),
        recovered: vec![recovered.clone()],
        ..Default::default()
    }));
    let output = run_script(&["list"], &env(), &script);
    assert_eq!(output.envelope.exit(), 0);
    assert_eq!(output.envelope.data.recovered_operations, [recovered]);
    assert_eq!(
        output.envelope.warnings[0].code,
        WarningCode::RecoveredOperation
    );
}

#[test]
fn unconfirmed_worktree_creation_never_retries_or_opens_another_tab() {
    use spaceterm_server::worktree::{Branches, Settings};
    let script = Rc::new(RefCell::new(Script {
        inventories: VecDeque::from([fixture().inventory]),
        replies: VecDeque::from([ReadReply::Context {
            home: "/server/home".into(),
            settings: Settings::default(),
            branches: Branches {
                local: vec!["main".into()],
                remote: Vec::new(),
            },
        }]),
        outcomes: VecDeque::from([Err(Failure::new(Code::OutcomeUnconfirmed))]),
        ..Default::default()
    }));
    let output = run_script(
        &["worktree", "create", "new", "--workspace", "api", "--open"],
        &env(),
        &script,
    );
    assert_eq!(output.envelope.exit(), 7);
    assert_eq!(script.borrow().sent.len(), 1);
}
