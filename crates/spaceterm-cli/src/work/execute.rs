//! One command execution against an admitted server. Resolution uses one frozen snapshot;
//! accepted mutations refresh output facts without retargeting the command.
use super::{
    backend::Read,
    dispatch::Dispatch,
    output::{Output, value},
    *,
};
use crate::{
    args::{Command, Global, PaneCommand, TabCommand, WorkspaceCommand, WorktreeCommand},
    contract::*,
    list,
    scope::{self, ScopeArgs, Selector, TargetData},
};
use spaceterm_server::{
    hierarchy::{self, CloseTarget, Outcome},
    labels,
    worktree::{Read as WorktreeRead, ReadReply, Removal},
};
use std::path::Path;

pub struct Environment<'a> {
    pub identity: &'a str,
    pub local: bool,
    pub cwd: &'a Path,
    pub current_identity: Option<&'a str>,
    pub current_server: Option<&'a str>,
    pub current_pane: Option<&'a str>,
    pub agent: bool,
    pub now: i64,
    pub confirmation: Option<&'a str>,
}

pub fn handles(command: &Command) -> bool {
    matches!(
        command,
        Command::List(_)
            | Command::Workspace(_)
            | Command::Tab(_)
            | Command::Worktree(_)
            | Command::Pane(PaneCommand::Split { .. } | PaneCommand::Close { .. })
    )
}

/// Route-dependent usage errors precede admission, recovery and every effect.
pub fn validate(command: &Command, local: bool) -> Result<(), Failure> {
    fn path(path: Option<&Path>, local: bool, argument: &'static str) -> Result<(), Failure> {
        if let Some(path) = path
            && (path.as_os_str().is_empty()
                || path.as_os_str().as_encoded_bytes().contains(&0)
                || (!local
                    && !path.is_absolute()
                    && !path.as_os_str().as_encoded_bytes().starts_with(b"~/")))
        {
            return Err(Failure::with(
                Code::InvalidArguments,
                Details::Usage(UsageDetails {
                    argument: Some(argument.into()),
                    kind: UsageKind::RelativePath,
                }),
            ));
        }
        Ok(())
    }
    fn name(name: &str, argument: &'static str) -> Result<(), Failure> {
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(Failure::with(
                Code::InvalidArguments,
                Details::Usage(UsageDetails {
                    argument: Some(argument.into()),
                    kind: UsageKind::Other,
                }),
            ));
        }
        Ok(())
    }
    match command {
        Command::Workspace(WorkspaceCommand::Create { name: n, directory }) => {
            name(n, "<NAME>")?;
            path(directory.as_deref(), local, "--directory")?;
        }
        Command::Workspace(WorkspaceCommand::Rename { new_name, .. }) => {
            name(new_name, "<NEW_NAME>")?
        }
        Command::Worktree(WorktreeCommand::Create { path: p, .. }) => {
            path(p.as_deref(), local, "--path")?
        }
        _ => {}
    }
    let scope = command_scope(command);
    for (index, selector) in [&scope.workspace, &scope.worktree, &scope.tab, &scope.pane]
        .into_iter()
        .enumerate()
    {
        if let Some(Selector::Path(p)) = selector {
            if index != 1 {
                return Err(Failure::new(Code::InvalidArguments));
            }
            path(Some(p), local, "--worktree")?;
        }
        if index == 1 && matches!(selector, Some(Selector::Id(_))) {
            return Err(Failure::new(Code::InvalidArguments));
        }
    }
    Ok(())
}

pub fn run(
    command: &Command,
    global: &Global,
    env: &Environment<'_>,
    journal: &mut dyn Dispatch,
    reader: &mut dyn Read,
    server_name: &str,
    route: &str,
) -> Output {
    let path = command.path();
    if let Err(failure) = validate(command, env.local) {
        return Output::failed(path, None, failure, serde_json::json!({}), String::new());
    }
    let server = ServerRef {
        name: server_name.into(),
        route: route.into(),
        id: hex(&journal.server_id()),
    };
    let recovered = match journal.resolve_leftovers() {
        Ok(recovered) => recovered,
        Err(failure) => {
            return Output::failed(
                path,
                Some(server),
                failure,
                serde_json::json!({}),
                String::new(),
            );
        }
    };
    let result = execute(command, global, env, journal, reader, &server);
    let mut output = match result {
        Ok(output) => output,
        Err(error) => error.output(path, server),
    };
    if !recovered.is_empty() {
        output.envelope.warnings.push(Warning::new(
            WarningCode::RecoveredOperation,
            WarningDetails::Recovered {
                count: recovered.len().try_into().unwrap_or(u32::MAX),
            },
        ));
        let mut text = String::new();
        for recovered in &recovered {
            let identity = match &recovered.identity {
                RecoveredIdentity::Base => "base",
                RecoveredIdentity::Agent(name) => name.as_str(),
            };
            let state = match recovered.outcome.state {
                RecoveredState::Applied => "applied",
                RecoveredState::Refused => "refused",
                RecoveredState::Unknown => "unknown",
            };
            text.push_str(&format!(
                "Recovered operation {} ({identity}): {state}\n",
                recovered.operation
            ));
        }
        text.push_str(&output.text);
        output.text = text;
        output.envelope.data.recovered_operations = recovered;
    }
    output.diagnostics.push_str(match output.envelope.status {
        Status::Ok => "progress: completed\n",
        Status::ConfirmationRequired => "progress: waiting for confirmation\n",
        Status::Error => "progress: stopped\n",
    });
    output
}

struct Error {
    failure: Failure,
    data: Box<serde_json::Value>,
    text: String,
}
impl Error {
    fn output(self, command: &str, server: ServerRef) -> Output {
        Output::failed(command, Some(server), self.failure, *self.data, self.text)
    }
}
impl From<Failure> for Error {
    fn from(failure: Failure) -> Self {
        Self {
            failure,
            data: Box::new(serde_json::json!({})),
            text: String::new(),
        }
    }
}
impl From<Code> for Error {
    fn from(code: Code) -> Self {
        Failure::new(code).into()
    }
}
impl From<scope::TargetError> for Error {
    fn from(error: scope::TargetError) -> Self {
        let (failure, data) = error.into_parts();
        let text = scope::data_text(&data);
        let data = match data {
            TargetData::None => serde_json::json!({}),
            TargetData::Candidates(data) => value(data),
            TargetData::Mismatch(data) => value(data),
        };
        Self {
            failure,
            data: Box::new(data),
            text,
        }
    }
}
impl From<WorkError> for Error {
    fn from(e: WorkError) -> Self {
        match e {
            WorkError::Target(e) => e.into(),
            WorkError::Code(e) => e.into(),
        }
    }
}

fn snapshot(reader: &mut dyn Read, server: Id) -> Result<Snapshot, Error> {
    let inventory = reader.inventory()?;
    Ok(Snapshot {
        server,
        repositories: inventory.repositories.clone(),
        inventory,
    })
}
fn submit(journal: &mut dyn Dispatch, mutation: Mutation) -> Result<Id, Error> {
    let outcome = journal.submit(mutation)?;
    from_outcome(&outcome)?;
    match outcome {
        Outcome::Accepted { target } => Ok(target),
        _ => Err(Code::OutcomeUnconfirmed.into()),
    }
}

fn execute(
    command: &Command,
    _global: &Global,
    env: &Environment<'_>,
    journal: &mut dyn Dispatch,
    reader: &mut dyn Read,
    server: &ServerRef,
) -> Result<Output, Error> {
    let initial = snapshot(reader, journal.server_id())?;
    let current = scope::current_from_context(
        &initial,
        env.identity,
        env.current_identity,
        env.current_server,
        env.current_pane,
    );
    let home = reader.home()?;
    let paths = paths::Paths {
        local: env.local,
        cwd: env.cwd,
        home: &home,
    };
    let args = paths.scope(&command_scope(command))?;
    let control = create_control(env.agent, current.as_ref(), &initial);
    let path = command.path();
    match command {
        Command::List(_) => {
            let target = scope::resolve(
                &initial,
                &args,
                current.as_ref(),
                scope::Want::Server,
                scope::Effect::Passive,
            )?;
            let facts = reader.panes(&initial, &target)?;
            let ReadReply::ServerFacts(facts_server) =
                reader.worktree(WorktreeRead::ServerFacts)?
            else {
                return Err(Code::ProtocolError.into());
            };
            let sessions = initial.inventory.close.as_ref().map_or(0, |c| {
                c.sessions
                    .iter()
                    .filter(|s| s.session.is_some())
                    .count()
                    .try_into()
                    .unwrap_or(u32::MAX)
            });
            let data = list::build(
                &initial,
                &target,
                ListServer {
                    name: facts_server.machine,
                    os: facts_server.operating_system,
                    version: facts_server.version,
                    sessions,
                },
                &facts,
            )?;
            let text = list::text(&data);
            Ok(Output::ok(
                path,
                server.clone(),
                data,
                text,
                &scope::target_steps(&initial, &target),
            ))
        }
        Command::Workspace(WorkspaceCommand::Create { name, directory }) => {
            let directory = directory.as_deref().map(|p| paths.resolve(p)).transpose()?;
            let id = submit(
                journal,
                workspace::create(name.clone(), directory, control)?,
            )?;
            let updated = snapshot(reader, initial.server)?;
            let workspace = updated
                .inventory
                .workspaces
                .iter()
                .find(|w| w.id == id)
                .ok_or(Code::ProtocolError)?;
            let steps = vec![TargetStep {
                level: Level::Workspace,
                id: Some(hex(&id)),
                path: None,
                label: Some(workspace.name.clone()),
                source: Source::Created,
            }];
            let data = WorkspaceChange {
                target: steps.clone(),
                workspace: workspace_ref(workspace),
                directory: Some(
                    workspace
                        .tabs
                        .iter()
                        .find(|t| t.id == workspace.root_tab)
                        .and_then(|t| t.panes.iter().find(|p| p.id == t.root_pane))
                        .ok_or(Code::ProtocolError)?
                        .starting_directory
                        .to_string_lossy()
                        .into_owned(),
                ),
                revision: updated.inventory.revision,
            };
            let text = format!(
                "Created Workspace id:{}\nDirectory: {}\n",
                hex(&id),
                data.directory.as_deref().ok_or(Code::ProtocolError)?
            );
            Ok(Output::ok(path, server.clone(), data, text, &steps))
        }
        Command::Workspace(WorkspaceCommand::Rename { new_name, .. }) => {
            let plan = workspace::rename(&initial, &args, current.as_ref(), new_name.clone())?;
            let steps = scope::target_steps(&initial, &plan.target);
            let id = submit(journal, plan.mutation)?;
            let updated = snapshot(reader, initial.server)?;
            let workspace = updated
                .inventory
                .workspaces
                .iter()
                .find(|w| w.id == id)
                .ok_or(Code::ProtocolError)?;
            Ok(Output::ok(
                path,
                server.clone(),
                WorkspaceChange {
                    target: steps.clone(),
                    workspace: workspace_ref(workspace),
                    directory: None,
                    revision: updated.inventory.revision,
                },
                format!("Renamed Workspace id:{}\n", hex(&id)),
                &steps,
            ))
        }
        Command::Tab(TabCommand::Create { .. }) | Command::Pane(PaneCommand::Split { .. }) => {
            let plan = match command {
                Command::Tab(_) => tab::create(&initial, &args, current.as_ref(), control)?,
                Command::Pane(PaneCommand::Split { down, .. }) => pane::split(
                    &initial,
                    &args,
                    current.as_ref(),
                    if *down {
                        hierarchy::Axis::Down
                    } else {
                        hierarchy::Axis::Right
                    },
                    control,
                )?,
                _ => unreachable!(),
            };
            let source = match &plan.mutation {
                Mutation::CreateTab { source, .. }
                | Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab {
                    source,
                    ..
                }) => *source,
                Mutation::SplitPane { source, .. } => Some(*source),
                _ => None,
            };
            let steps = scope::target_steps(&initial, &plan.target);
            let id = submit(journal, plan.mutation)?;
            let updated = snapshot(reader, initial.server)?;
            let (tab, pane) = if matches!(command, Command::Tab(_)) {
                let tab = updated
                    .inventory
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.tabs)
                    .find(|t| t.id == id)
                    .ok_or(Code::ProtocolError)?;
                (
                    tab,
                    tab.panes
                        .iter()
                        .find(|p| p.id == tab.root_pane)
                        .ok_or(Code::ProtocolError)?,
                )
            } else {
                updated
                    .inventory
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.tabs)
                    .find_map(|t| t.panes.iter().find(|p| p.id == id).map(|p| (t, p)))
                    .ok_or(Code::ProtocolError)?
            };
            let data = Created {
                target: steps.clone(),
                directory: pane.starting_directory.to_string_lossy().into_owned(),
                tab: tab_ref(tab),
                pane: CreatedPane {
                    id: hex(&pane.id),
                    directory: pane.starting_directory.to_string_lossy().into_owned(),
                    control: created_control(control, pane),
                },
                source_pane: source.as_ref().map(hex),
                revision: updated.inventory.revision,
            };
            Ok(Output::ok(
                path,
                server.clone(),
                data,
                format!(
                    "Created Pane id:{}\nDirectory: {}\n",
                    hex(&pane.id),
                    pane.starting_directory.to_string_lossy()
                ),
                &steps,
            ))
        }
        Command::Workspace(WorkspaceCommand::Kill { confirm, .. }) => {
            let (target, close) = workspace::kill(&initial, &args, current.as_ref())?;
            close_work(
                path,
                &initial,
                CloseRequest {
                    target,
                    close,
                    token: confirm.as_deref().or(env.confirmation),
                    fallback: None,
                },
                env,
                journal,
                reader,
                server,
            )
        }
        Command::Pane(PaneCommand::Close { confirm, .. }) => {
            let (target, close) = pane::close(&initial, &args, current.as_ref())?;
            close_work(
                path,
                &initial,
                CloseRequest {
                    target,
                    close,
                    token: confirm.as_deref().or(env.confirmation),
                    fallback: None,
                },
                env,
                journal,
                reader,
                server,
            )
        }
        Command::Tab(TabCommand::Close { all, confirm, .. }) => {
            let mut plan = tab::close(&initial, &args, current.as_ref(), *all)?;
            let token = confirm.as_deref().or(env.confirmation);
            let mut reviewed = initial.clone();
            let mut fallback_reference = None;
            if let Some(mut fallback) = plan.fallback.take() {
                if token.is_some() {
                    return Err(Code::Changed.into());
                }
                if let Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab {
                    control: requested,
                    ..
                }) = &mut fallback
                {
                    *requested = control;
                }
                let Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab {
                    root: expected_root,
                    path: expected_path,
                    ..
                }) = &fallback
                else {
                    return Err(Code::ProtocolError.into());
                };
                let (expected_root, expected_path) = (expected_root.clone(), expected_path.clone());
                let id = submit(journal, fallback)?;
                reviewed = snapshot(reader, initial.server)?;
                let valid = reviewed
                    .inventory
                    .workspaces
                    .iter()
                    .find(|w| Some(w.id) == plan.target.workspace)
                    .is_some_and(|w| {
                        w.tabs.iter().any(|t| {
                            t.id == id
                                && t.worktree.as_ref() == Some(&expected_path)
                                && t.worktree_repository.as_ref() == Some(&expected_root)
                                && t.panes.iter().any(|p| p.id == t.root_pane)
                        })
                    });
                if !valid {
                    return Err(Code::NoFallbackWorktree.into());
                }
                if let CloseTarget::Worktree { workspace, tab } = plan.close {
                    let anchor = |inv: &hierarchy::Inventory| {
                        inv.workspaces
                            .iter()
                            .find(|w| w.id == workspace)
                            .and_then(|w| w.tabs.iter().find(|t| t.id == tab))
                            .map(|t| (t.worktree.clone(), t.worktree_repository.clone()))
                    };
                    if anchor(&reviewed.inventory) != anchor(&initial.inventory) {
                        return Err(Code::Changed.into());
                    }
                }
                if reviewed.inventory.close_confirmation(plan.close).is_none() {
                    return Err(Code::Changed.into());
                }
                fallback_reference = reviewed
                    .inventory
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.tabs)
                    .find(|t| t.id == id)
                    .map(tab_ref);
            }
            close_work(
                path,
                &reviewed,
                CloseRequest {
                    target: plan.target,
                    close: plan.close,
                    token,
                    fallback: fallback_reference,
                },
                env,
                journal,
                reader,
                server,
            )
        }
        Command::Worktree(WorktreeCommand::Create {
            branch,
            base,
            path: p,
            ..
        }) => {
            let target = scope::resolve(
                &initial,
                &args,
                current.as_ref(),
                scope::Want::Workspace,
                scope::Effect::Mutating,
            )?;
            let workspace = target.workspace.ok_or(Code::NotFound)?;
            let repos = initial.repositories.as_ref().ok_or(Code::NotFound)?;
            let root = spaceterm_server::organize::workspace_worktrees(
                &initial.inventory,
                repos,
                workspace,
            )
            .ok_or(Code::NotFound)?
            .main_root
            .clone();
            let ReadReply::Context {
                settings,
                home: server_home,
                branches,
            } = reader.worktree(WorktreeRead::Context { root })?
            else {
                return Err(Code::ProtocolError.into());
            };
            let paths = paths::Paths {
                local: env.local,
                cwd: env.cwd,
                home: &server_home,
            };
            let context = worktree::Context {
                branches: &branches,
                settings: &settings,
                paths: &paths,
            };
            let plan = worktree::create(
                &initial,
                &args,
                current.as_ref(),
                &worktree::CreateArgs {
                    name: branch,
                    base: base.as_deref(),
                    path: p.as_deref(),
                    control,
                },
                &context,
            )?;
            let Mutation::Worktree(spaceterm_server::worktree::Operation::Create {
                branch, ..
            }) = &plan.mutation
            else {
                unreachable!()
            };
            let branch = branch.name().to_owned();
            let id = submit(journal, plan.mutation)?;
            let updated = snapshot(reader, initial.server)?;
            let tab = updated
                .inventory
                .workspaces
                .iter()
                .find(|w| w.id == workspace)
                .and_then(|w| w.tabs.iter().find(|t| t.id == id))
                .ok_or(Code::ProtocolError)?;
            let actual_path = tab.worktree.as_ref().ok_or(Code::ProtocolError)?;
            let pane = tab
                .panes
                .iter()
                .find(|p| p.id == tab.root_pane)
                .ok_or(Code::ProtocolError)?;
            let mut steps = scope::target_steps(&initial, &plan.target);
            steps.push(TargetStep {
                level: Level::Worktree,
                id: None,
                path: Some(actual_path.to_string_lossy().into_owned()),
                label: Some(branch.clone()),
                source: Source::Created,
            });
            let data = WorktreeCreate {
                target: steps.clone(),
                directory: pane.starting_directory.to_string_lossy().into_owned(),
                control: created_control(control, pane),
                worktree: WorktreeRef {
                    path: actual_path.to_string_lossy().into_owned(),
                    branch: Some(branch),
                    main: false,
                    missing: false,
                },
                tab: Some(tab_ref(tab)),
            };
            let text = format!(
                "Created Worktree.\nDirectory: {}\nCreated Tab id:{}\nControl: {}\n",
                data.directory,
                hex(&tab.id),
                match data.control {
                    CreatedControl::Own => "self",
                    CreatedControl::Unclaimed => "unclaimed",
                    CreatedControl::Origin => "origin",
                }
            );
            Ok(Output::ok(path, server.clone(), data, text, &steps))
        }
        Command::Worktree(WorktreeCommand::Remove { confirm, .. }) => {
            let (target, root, tree) = worktree::remove(&initial, &args, current.as_ref())?;
            let token = confirm.as_deref().or(env.confirmation);
            let removal = if let Some(token) = token {
                let consent::Review::Removal(removal) =
                    consent::verify(token, initial.server, journal.client_id(), env.now)?
                else {
                    return Err(Code::Changed.into());
                };
                if removal.root != root
                    || removal.path != tree
                    || !removal.matches_inventory(&initial.inventory)
                {
                    return Err(Code::Changed.into());
                }
                removal
            } else {
                let ReadReply::Removal(removal) =
                    reader.worktree(WorktreeRead::CheckRemoval { root, path: tree })?
                else {
                    return Err(Code::ProtocolError.into());
                };
                let effects = removal_effects(&initial, &removal, journal.client_id())?;
                let (token, expires_at) = consent::issue(
                    initial.server,
                    journal.client_id(),
                    consent::Review::Removal(removal),
                    env.now,
                )?;
                let text = format!(
                    "{}Confirm with --confirm {}\n",
                    output::preview(&effects),
                    token
                );
                return Ok(Output::confirmation(
                    path,
                    server.clone(),
                    Confirmation {
                        token,
                        expires_at,
                        effects,
                    },
                    text,
                ));
            };
            let closed = removal
                .tabs
                .iter()
                .flat_map(|t| t.panes.iter().map(|p| hex(&p.id)))
                .collect();
            submit(journal, worktree::confirmed_removal(&removal))?;
            let updated = snapshot(reader, initial.server)?;
            let steps = scope::target_steps(&initial, &target);
            Ok(Output::ok(
                path,
                server.clone(),
                Closed {
                    target: steps.clone(),
                    closed,
                    revision: updated.inventory.revision,
                },
                "Removed Worktree.\n".into(),
                &steps,
            ))
        }
        _ => Err(Code::Unsupported.into()),
    }
}

struct CloseRequest<'a> {
    target: Resolved,
    close: CloseTarget,
    token: Option<&'a str>,
    fallback: Option<Ref>,
}

fn close_work(
    command: &str,
    snapshot: &Snapshot,
    request: CloseRequest<'_>,
    env: &Environment<'_>,
    journal: &mut dyn Dispatch,
    reader: &mut dyn Read,
    server: &ServerRef,
) -> Result<Output, Error> {
    let CloseRequest {
        target,
        close,
        token,
        fallback,
    } = request;
    let mutation = if let Some(token) = token {
        let consent::Review::Close {
            target: reviewed,
            confirmation,
        } = consent::verify(token, snapshot.server, journal.client_id(), env.now)?
        else {
            return Err(Code::Changed.into());
        };
        if reviewed != close {
            return Err(Code::Changed.into());
        }
        close::confirmed(snapshot, close, confirmation)?
    } else {
        match close::review(
            snapshot,
            close,
            journal.client_id(),
            &close_facts(snapshot, journal.client_id()),
        )? {
            close::Review::Direct { mutation, .. } => mutation,
            close::Review::Confirm {
                confirmation,
                mut effects,
            } => {
                effects.fallback = fallback;
                let (token, expires_at) = consent::issue(
                    snapshot.server,
                    journal.client_id(),
                    consent::Review::Close {
                        target: close,
                        confirmation,
                    },
                    env.now,
                )?;
                let text = format!(
                    "{}Confirm with --confirm {}\n",
                    output::preview(&effects),
                    token
                );
                return Ok(Output::confirmation(
                    command,
                    server.clone(),
                    Confirmation {
                        token,
                        expires_at,
                        effects,
                    },
                    text,
                ));
            }
        }
    };
    let Mutation::Close { confirmation, .. } = &mutation else {
        unreachable!()
    };
    let closed = confirmation.panes.iter().map(|p| hex(&p.id)).collect();
    submit(journal, mutation)?;
    let updated = self::snapshot(reader, snapshot.server)?;
    let steps = scope::target_steps(snapshot, &target);
    Ok(Output::ok(
        command,
        server.clone(),
        Closed {
            target: steps.clone(),
            closed,
            revision: updated.inventory.revision,
        },
        "Closed terminal work.\n".into(),
        &steps,
    ))
}

fn close_facts(snapshot: &Snapshot, actor: Id) -> Vec<list::PaneFacts> {
    snapshot
        .inventory
        .close
        .iter()
        .flat_map(|c| &c.sessions)
        .map(|p| list::PaneFacts {
            pane: p.id,
            session: PaneSession {
                id: p.session.as_ref().map(hex),
                state: if p.session.is_some() {
                    SessionStateLabel::Running
                } else {
                    SessionStateLabel::NotStarted
                },
            },
            control: match snapshot
                .inventory
                .close
                .as_ref()
                .and_then(|c| c.controls.iter().find(|c| c.pane == p.id))
            {
                Some(c) => PaneControl {
                    state: if c.owner == actor {
                        ControlState::Own
                    } else {
                        ControlState::Other
                    },
                    client_name: None,
                },
                None => PaneControl {
                    state: ControlState::None,
                    client_name: None,
                },
            },
        })
        .collect()
}
fn removal_effects(
    snapshot: &Snapshot,
    removal: &Removal,
    actor: Id,
) -> Result<CloseEffects, Error> {
    let mut effects = CloseEffects {
        ends_sessions: 0,
        sessions: Vec::new(),
        controlled_by_other_clients: Vec::new(),
        removes_tab: !removal.tabs.is_empty(),
        removes_workspace: false,
        fallback: None,
    };
    for tab in &removal.tabs {
        let target = CloseTarget::Tab {
            workspace: tab.workspace,
            tab: tab.tab,
        };
        let review = close::review(snapshot, target, actor, &close_facts(snapshot, actor))?;
        let additional = match review {
            close::Review::Confirm { effects, .. } | close::Review::Direct { effects, .. } => {
                effects
            }
        };
        effects.sessions.extend(additional.sessions);
        effects
            .controlled_by_other_clients
            .extend(additional.controlled_by_other_clients);
        effects.ends_sessions += additional.ends_sessions;
    }
    effects.removes_workspace = snapshot.inventory.workspaces.iter().any(|w| {
        !w.tabs.is_empty()
            && w.tabs.iter().all(|t| {
                removal
                    .tabs
                    .iter()
                    .any(|removed| removed.workspace == w.id && removed.tab == t.id)
            })
    });
    Ok(effects)
}
fn workspace_ref(workspace: &hierarchy::Workspace) -> Ref {
    Ref {
        id: hex(&workspace.id),
        label: workspace.name.clone(),
        line: workspace.name.clone(),
    }
}
fn created_control(control: CreateControl, pane: &hierarchy::Pane) -> CreatedControl {
    match control {
        CreateControl::Creator => CreatedControl::Own,
        CreateControl::Unclaimed => CreatedControl::Unclaimed,
        CreateControl::OriginController { .. } if pane.creator.is_some() => CreatedControl::Origin,
        CreateControl::OriginController { .. } => CreatedControl::Unclaimed,
    }
}

fn tab_ref(tab: &hierarchy::Tab) -> Ref {
    let label = labels::tab(tab);
    Ref {
        id: hex(&tab.id),
        label: label.activity,
        line: label.line,
    }
}
fn command_scope(command: &Command) -> ScopeArgs {
    match command {
        Command::List(args) => args.scope.args(),
        Command::Workspace(
            WorkspaceCommand::Rename { workspace, .. } | WorkspaceCommand::Kill { workspace, .. },
        ) => ScopeArgs {
            workspace: workspace.clone(),
            ..Default::default()
        },
        Command::Tab(TabCommand::Create {
            workspace,
            worktree,
            pane,
        }) => ScopeArgs {
            workspace: workspace.clone(),
            worktree: worktree.clone(),
            pane: pane.clone(),
            tab: None,
        },
        Command::Tab(TabCommand::Close {
            workspace,
            worktree,
            tab,
            ..
        }) => ScopeArgs {
            workspace: workspace.clone(),
            worktree: worktree.clone(),
            tab: tab.clone(),
            pane: None,
        },
        Command::Pane(PaneCommand::Split { scope, .. } | PaneCommand::Close { scope, .. }) => {
            scope.args()
        }
        Command::Worktree(WorktreeCommand::Create { workspace, .. }) => ScopeArgs {
            workspace: workspace.clone(),
            ..Default::default()
        },
        Command::Worktree(WorktreeCommand::Remove {
            workspace,
            worktree,
            ..
        }) => ScopeArgs {
            workspace: workspace.clone(),
            worktree: worktree.clone(),
            ..Default::default()
        },
        _ => ScopeArgs::default(),
    }
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
