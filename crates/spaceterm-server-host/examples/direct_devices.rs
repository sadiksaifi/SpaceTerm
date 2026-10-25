//! Two real hosts, one existing Development server, and only scoped test-owned work.
//! The default prints the scope without connecting. The parent prepares/signs both native
//! examples and starts the foreground server before explicitly authorizing the mutation pass.

#[cfg(feature = "client")]
#[expect(
    dead_code,
    reason = "this exercise uses only the terminal command/output helpers"
)]
mod support;

#[cfg(feature = "client")]
#[path = "support/terminal_semantics.rs"]
mod terminal_semantics;

#[cfg(feature = "client")]
#[path = "support/presentation.rs"]
mod presentation;

#[cfg(feature = "client")]
mod exercise {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::os::fd::{AsRawFd, BorrowedFd};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use spaceterm_server::direct::{
        self, DeviceCredential, DirectConnection, DirectConnector, DirectError, Enrollment,
        Invitation, ListenerState, NetworkPolicy, PairedDevice,
    };
    use spaceterm_server::hierarchy::{CloseTarget, Id, Inventory, Mutation, Outcome};
    use spaceterm_server::pane::control::Controller;
    use spaceterm_server::pane::{
        Lease, PaneConnection, PaneSize, Reply, Request, Screen, directories,
    };
    use spaceterm_server::{ServerConnection, ServerError};

    type Result<T> = std::result::Result<T, &'static str>;
    const IDENTITY: &str = "spaceterm-development";
    const DEVICE_NAME: &str = "direct541-penguin";
    const MAX_PIPE_FRAME: usize = 16 * 1024;
    const PIPE_DEADLINE: Duration = Duration::from_secs(90);

    fn api<T, E>(result: std::result::Result<T, E>) -> Result<T> {
        result.map_err(|_| "native API refused or unavailable")
    }

    fn require(condition: bool, failure: &'static str) -> Result<()> {
        if condition { Ok(()) } else { Err(failure) }
    }

    fn reply_classification(reply: &std::result::Result<Reply, ServerError>) -> &'static str {
        match reply {
            Ok(_) => "unexpected-reply",
            Err(ServerError::Protocol) => "protocol",
            Err(ServerError::NotRunning) => "not-running",
            Err(ServerError::SessionUnavailable) => "session-unavailable",
            Err(ServerError::StaleControl) => "stale-control",
            Err(_) => "server-refusal",
        }
    }

    fn random() -> Result<[u8; 32]> {
        let mut bytes = [0; 32];
        // SAFETY: getentropy writes exactly 32 bytes to this live writable array.
        require(
            unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } == 0,
            "native randomness unavailable",
        )?;
        Ok(bytes)
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn id(text: &str) -> Result<Id> {
        require(
            text.len() == 64 && text.is_ascii(),
            "expected server must be 64 hex digits",
        )?;
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map_err(|_| "expected server must be 64 hex digits")?;
        }
        require(bytes != [0; 32], "expected server cannot be zero")?;
        Ok(bytes)
    }

    fn size() -> PaneSize {
        PaneSize {
            columns: 96,
            rows: 30,
            cell_width: 8,
            cell_height: 18,
        }
    }

    fn inventory(connection: &mut PaneConnection) -> Result<(Inventory, u64)> {
        match api(connection.exchange(&Request::Inventory))? {
            Reply::Inventory {
                inventory,
                next_sequence,
            } => Ok((inventory, next_sequence)),
            _ => Err("inventory response refused"),
        }
    }

    fn idle(inventory: &Inventory) -> Result<()> {
        require(
            inventory
                .close
                .as_ref()
                .is_some_and(|close| close.sessions.iter().all(|pane| pane.session.is_none())),
            "idle guard refused: a Terminal Session exists or session facts are unavailable",
        )
    }

    fn mutate(
        server: &ServerConnection,
        connection: &mut PaneConnection,
        mutation: Mutation,
    ) -> Result<Id> {
        let mut journal = api(server.shared_journal())?;
        require(
            journal.pending().is_none(),
            "resolve this test client's original mutation first",
        )?;
        let (_, sequence) = inventory(connection)?;
        api(journal.submit(sequence, mutation.clone()))?;
        let mut reply = api(connection.exchange(&Request::Mutate {
            sequence,
            mutation: mutation.clone(),
        }))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while matches!(reply, Reply::MutationPending) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(40));
            reply = api(connection.exchange(&Request::Resolve { sequence }))?;
        }
        let Reply::Outcome(outcome) = reply else {
            return Err("test mutation remains unconfirmed");
        };
        require(
            outcome.sequence == sequence && outcome.mutation == mutation,
            "test mutation identity changed",
        )?;
        api(journal.complete(outcome.clone()))?;
        api(connection.exchange(&Request::Acknowledge { sequence }))?;
        api(journal.retire())?;
        match outcome.outcome {
            Outcome::Accepted { target } => Ok(target),
            _ => Err("test mutation refused"),
        }
    }

    fn select(connection: &mut PaneConnection, pane: Id, explicit: bool) -> Result<Reply> {
        api(connection.exchange(&Request::SelectPane {
            pane,
            explicit,
            size: size(),
            style: None,
        }))
    }

    fn attached(reply: Reply) -> Result<Lease> {
        match reply {
            Reply::Attached { lease } => Ok(lease),
            _ => Err("expected an attached test Pane"),
        }
    }

    fn take(connection: &mut PaneConnection, pane: Id, named: &str) -> Result<Lease> {
        let Reply::Takeover {
            target,
            controller:
                Controller::Client {
                    name,
                    this_client: false,
                },
            ..
        } = select(connection, pane, false)?
        else {
            return Err("expected another Controlling Client");
        };
        require(
            name.as_str().ends_with(&format!("({named})")),
            "Controlling Client name did not reconcile",
        )?;
        attached(api(connection.exchange(&Request::TakeOver {
            target,
            size: size(),
            style: None,
        }))?)
    }

    fn reclaim_after_revocation(connection: &mut PaneConnection, pane: Id) -> Result<Lease> {
        let Reply::Takeover {
            target,
            controller: Controller::Nobody,
            session_ended: false,
        } = select(connection, pane, false)?
        else {
            return Err("revocation did not leave the running Pane without a controller");
        };
        attached(api(connection.exchange(&Request::TakeOver {
            target,
            size: size(),
            style: None,
        }))?)
    }

    fn command(connection: &mut PaneConnection, lease: Lease, text: &str) -> Result<()> {
        api(super::support::command(connection, lease, text))
    }

    fn output(connection: &mut PaneConnection, lease: Lease, marker: &str) -> Result<()> {
        output_snapshot(connection, lease, marker).map(|_| ())
    }

    fn output_snapshot(
        connection: &mut PaneConnection,
        lease: Lease,
        marker: &str,
    ) -> Result<Box<Screen>> {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if let Reply::Screen(screen) = api(connection.exchange(&Request::Poll { lease }))?
                && screen.rows.iter().any(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        == marker
                })
            {
                return Ok(screen);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Err("terminal output did not arrive")
    }

    fn pipe_only(fd: i32) -> Result<()> {
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes the live output on success; no file contents are read.
        require(
            unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } == 0,
            "pipe metadata unavailable",
        )?;
        // SAFETY: the successful fstat call initialized every field.
        let kind = unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT;
        require(
            kind == libc::S_IFIFO || kind == libc::S_IFSOCK,
            "invitation transport must be an anonymous pipe or SSH stream, never a file or terminal",
        )
    }

    fn read_exact<R: Read + AsRawFd>(
        reader: &mut R,
        mut bytes: &mut [u8],
        until: Instant,
        progress: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        while !bytes.is_empty() {
            require(Instant::now() < until, "pipe phase deadline elapsed")?;
            progress()?;
            let left = until.saturating_duration_since(Instant::now());
            require(!left.is_zero(), "pipe phase deadline elapsed")?;
            let mut poll = libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = left.as_millis().clamp(1, 1_000) as i32;
            // SAFETY: poll borrows one initialized pollfd for at most the bounded deadline.
            let ready = unsafe { libc::poll(&mut poll, 1, millis) };
            if ready < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            require(ready >= 0, "pipe phase unavailable")?;
            if ready == 0 {
                continue;
            }
            let read = api(reader.read(bytes))?;
            require(read != 0, "peer pipe closed before completing its phase")?;
            bytes = &mut bytes[read..];
        }
        Ok(())
    }

    fn receive<R: Read + AsRawFd>(reader: &mut R) -> Result<Vec<u8>> {
        receive_while(reader, &mut || Ok(()))
    }

    fn receive_while<R: Read + AsRawFd>(
        reader: &mut R,
        progress: &mut impl FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        pipe_only(reader.as_raw_fd())?;
        let until = Instant::now() + PIPE_DEADLINE;
        let mut length = [0; 4];
        read_exact(reader, &mut length, until, progress)?;
        let length = u32::from_be_bytes(length) as usize;
        require(
            length != 0 && length <= MAX_PIPE_FRAME,
            "pipe frame bound refused",
        )?;
        let mut bytes = vec![0; length];
        if let Err(error) = read_exact(reader, &mut bytes, until, progress) {
            wipe(&mut bytes);
            return Err(error);
        }
        Ok(bytes)
    }

    fn wipe(bytes: &mut [u8]) {
        for byte in bytes {
            // SAFETY: each byte is live and exclusively borrowed. Volatile writes retain the
            // erasure even when the compiler can prove the receiving frame is about to drop.
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }

    fn send<W: Write + AsRawFd>(writer: &mut W, bytes: &[u8]) -> Result<()> {
        pipe_only(writer.as_raw_fd())?;
        require(
            !bytes.is_empty() && bytes.len() <= MAX_PIPE_FRAME,
            "pipe frame bound refused",
        )?;
        api(writer.write_all(&(bytes.len() as u32).to_be_bytes()))?;
        api(writer.write_all(bytes))?;
        api(writer.flush())
    }

    fn send_json<W: Write + AsRawFd>(writer: &mut W, value: Value) -> Result<()> {
        send(writer, &api(serde_json::to_vec(&value))?)
    }

    fn receive_json<R: Read + AsRawFd>(reader: &mut R) -> Result<Value> {
        receive_json_while(reader, &mut || Ok(()))
    }

    fn receive_json_while<R: Read + AsRawFd>(
        reader: &mut R,
        progress: &mut impl FnMut() -> Result<()>,
    ) -> Result<Value> {
        let mut bytes = receive_while(reader, progress)?;
        let result = serde_json::from_slice(&bytes).map_err(|_| "peer phase response malformed");
        wipe(&mut bytes);
        result
    }

    fn receive_invitation<R: Read + AsRawFd>(reader: &mut R) -> Result<Invitation> {
        let mut bytes = receive(reader)?;
        let result = std::str::from_utf8(&bytes)
            .map_err(|_| "invitation pipe encoding refused")
            .and_then(|text| api(Invitation::parse(text)));
        wipe(&mut bytes);
        result
    }

    struct Peer {
        child: Child,
        input: ChildStdin,
        output: ChildStdout,
    }

    impl Peer {
        fn penguin() -> Result<Self> {
            let command = if super::terminal_semantics::input_timing_requested() {
                "SPACETERM_NATIVE_INPUT_TIMING=1 exec \"$HOME/Projects/SpaceTerm/target/debug/examples/direct_devices\" device-pipe"
            } else {
                "exec \"$HOME/Projects/SpaceTerm/target/debug/examples/direct_devices\" device-pipe"
            };
            let mut child = api(Command::new("ssh")
                .args([
                    "-T",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=yes",
                    "-o",
                    "UpdateHostKeys=no",
                    "-o",
                    "AddKeysToAgent=no",
                    "-o",
                    "UseKeychain=no",
                    "-o",
                    "LogLevel=ERROR",
                    "-o",
                    "ControlMaster=no",
                    "-o",
                    "ControlPath=none",
                    "penguin",
                    command,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn())?;
            let input = child.stdin.take().ok_or("SSH input pipe unavailable")?;
            let output = child.stdout.take().ok_or("SSH output pipe unavailable")?;
            Ok(Self {
                child,
                input,
                output,
            })
        }

        fn message(&mut self, value: Value) -> Result<Value> {
            self.message_while(value, || Ok(()))
        }

        fn message_while(
            &mut self,
            value: Value,
            mut progress: impl FnMut() -> Result<()>,
        ) -> Result<Value> {
            send_json(&mut self.input, value)?;
            receive_json_while(&mut self.output, &mut progress)
        }

        fn finish(&mut self) -> Result<()> {
            send_json(&mut self.input, json!({"done": true}))?;
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                if let Some(status) = api(self.child.try_wait())? {
                    return require(status.success(), "Penguin driver failed");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err("Penguin driver did not exit")
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FixtureWorkspace {
        workspace: Id,
        boot: Id,
        name: String,
        directory: std::path::PathBuf,
        pane: Option<Id>,
    }

    fn close_fixture_workspace(server: &ServerConnection, owned: &FixtureWorkspace) -> Result<()> {
        let mut connection = api(server.connect_pane())?;
        let current = inventory(&mut connection)?.0;
        require(
            current.close.as_ref().map(|close| close.boot) == Some(owned.boot),
            "cleanup refused replacement server boot",
        )?;
        let Some(work) = current
            .workspaces
            .iter()
            .find(|work| work.id == owned.workspace)
        else {
            return Ok(());
        };
        require(
            work.name == owned.name
                && work.pinned_directory.as_ref() == Some(&owned.directory)
                && work.tabs.len() == 1
                && work.tabs[0].panes.len() == 1
                && work.root_tab == work.tabs[0].id
                && work.tabs[0].root_pane == work.tabs[0].panes[0].id
                && owned.pane.is_none_or(|pane| work.tabs[0].root_pane == pane),
            "cleanup refused: test Workspace changed",
        )?;
        let target = CloseTarget::Workspace {
            workspace: owned.workspace,
        };
        let confirmation = current
            .close_confirmation(target)
            .ok_or("cleanup confirmation unavailable")?;
        mutate(
            server,
            &mut connection,
            Mutation::Close {
                target,
                confirmation,
            },
        )?;
        Ok(())
    }

    struct WorkspaceGuard<'a> {
        server: &'a ServerConnection,
        owned: Option<FixtureWorkspace>,
    }
    impl Drop for WorkspaceGuard<'_> {
        fn drop(&mut self) {
            if let Some(owned) = &self.owned
                && let Err(classification) = close_fixture_workspace(self.server, owned)
            {
                eprintln!(
                    "direct_devices: exact Workspace {} cleanup incomplete: {classification}",
                    hex(&owned.workspace)
                );
            }
        }
    }

    struct Scope {
        owner: ServerConnection,
        policy: NetworkPolicy,
        server: Id,
        baseline: Inventory,
        workspace: Option<FixtureWorkspace>,
        invitations: Vec<Id>,
        claims: Vec<Id>,
        keys: Vec<[u8; 32]>,
    }

    impl Scope {
        fn cleanup(&mut self) -> Result<()> {
            require(
                api(self.owner.direct_status())?.pin.server == self.server,
                "cleanup server identity changed",
            )?;
            for invitation in &self.invitations {
                api(self.owner.withdraw_invitation(*invitation))?;
            }
            for claim in api(self.owner.pairing_claims())? {
                if self.claims.contains(&claim.id) {
                    api(self.owner.reject_device(claim.id))?;
                }
            }
            let devices = api(self.owner.paired_devices())?;
            for device in &devices {
                if self.keys.contains(&device.public_key) {
                    api(self.owner.revoke_device(random()?, device.pairing))?;
                }
            }
            let mut connection = api(self.owner.connect_pane())?;
            if let Some(owned) = &self.workspace {
                require(
                    !self
                        .baseline
                        .workspaces
                        .iter()
                        .any(|work| work.id == owned.workspace),
                    "cleanup preserved a baseline Workspace",
                )?;
                close_fixture_workspace(&self.owner, owned)?;
                self.workspace = None;
            }
            require(
                api(self.owner.paired_devices())?.is_empty(),
                "cleanup preserved an unrelated pairing; exposure requires owner review",
            )?;
            let policy = api(self.owner.direct_status())?.policy;
            require(
                policy.as_ref() == Some(&self.policy) || policy.is_none(),
                "cleanup preserved a changed exposure policy",
            )?;
            if policy.is_some() {
                api(self.owner.set_direct_policy(None))?;
            }
            require(
                inventory(&mut connection)?.0.workspaces == self.baseline.workspaces,
                "baseline organization changed; it was preserved without rollback",
            )?;
            Ok(())
        }

        fn enroll(&mut self, peer: &mut Peer) -> Result<PairedDevice> {
            let invitation = api(self.owner.invite_device())?;
            require(
                invitation.server_pin().server == self.server,
                "invitation belongs to another server",
            )?;
            self.invitations.push(invitation.id());
            send(&mut peer.input, invitation.display_text().as_bytes())?;
            let response = receive_json(&mut peer.output)?;
            let claim_id: Id = api(serde_json::from_value(response["claim"].clone()))?;
            let claims = api(self.owner.pairing_claims())?;
            require(
                claims.len() == 1 && claims[0].id == claim_id,
                "exact claim retry created duplicate or concurrent owner claims",
            )?;
            let claim = claims
                .into_iter()
                .find(|claim| claim.id == claim_id)
                .ok_or("proved device claim missing")?;
            self.claims.push(claim.id);
            self.keys.push(claim.device_public_key);
            require(
                claim.device_fingerprint() == response["fingerprint"].as_str().unwrap_or_default()
                    && claim.device_name.as_str() == DEVICE_NAME,
                "claim fingerprint or identity substituted",
            )?;
            require(
                claim.comparison_code().to_string()
                    == response["comparison_code"].as_str().unwrap_or_default(),
                "owner and device comparison codes differ",
            )?;
            let mut wrong = claim.clone();
            wrong.server[0] ^= 1;
            require(
                matches!(
                    self.owner.approve_device(&wrong),
                    Err(DirectError::WrongServer)
                ),
                "wrong server binding was accepted",
            )?;
            let mut wrong = claim.clone();
            wrong.device_public_key[0] ^= 1;
            require(
                matches!(
                    self.owner.approve_device(&wrong),
                    Err(DirectError::Substituted)
                ),
                "substituted public claim was accepted",
            )?;
            let approved = api(self.owner.approve_device(&claim))?;
            require(
                matches!(
                    self.owner.approve_device(&claim),
                    Err(DirectError::AlreadyDecided)
                ),
                "approval replay was accepted",
            )?;
            let response = peer.message(json!({"approved": true}))?;
            let paired: PairedDevice = api(serde_json::from_value(response["paired"].clone()))?;
            require(
                paired.pin == *invitation.server_pin()
                    && paired.pairing == approved.pairing
                    && paired.client == approved.client,
                "approved device/server binding changed",
            )?;
            Ok(paired)
        }
    }

    fn owner(bind: SocketAddr, expected: Id, restart: bool) -> Result<()> {
        require(
            !bind.ip().is_loopback(),
            "two-host verification requires a reachable native host address",
        )?;
        let name = format!("direct541-owner-{}", &hex(&random()?)[..6]);
        let owner = api(ServerConnection::for_development_client(&name, 1))?;
        require(
            api(owner.refresh())?.protocol_major == 1,
            "expected initial protocol major 1",
        )?;
        let mut connection = api(owner.connect_pane())?;
        let baseline = inventory(&mut connection)?.0;
        idle(&baseline)?;
        let initial = api(owner.direct_status())?;
        require(
            initial.pin.server == expected && initial.pin.build == IDENTITY,
            "expected Development server identity changed",
        )?;
        require(
            initial.policy.is_none()
                && initial.listener == ListenerState::Off
                && api(owner.paired_devices())?.is_empty()
                && api(owner.pairing_claims())?.is_empty(),
            "scope guard refused: exposure, paired devices or pending claims already exist",
        )?;
        let policy = api(NetworkPolicy::new(bind))?;
        let mut scope = Scope {
            owner,
            policy,
            server: expected,
            baseline,
            workspace: None,
            invitations: Vec::new(),
            claims: Vec::new(),
            keys: Vec::new(),
        };
        println!("SCOPE named Development client {name}");
        let result = (|| {
            let status = api(scope.owner.set_direct_policy(Some(scope.policy.clone())))?;
            require(
                status.pin == initial.pin
                    && status.listener == ListenerState::Listening { addr: bind },
                "explicit exposure did not bind the requested endpoint",
            )?;
            if restart {
                idle(&inventory(&mut connection)?.0)?;
                drop(connection);
                api(spaceterm_server::service::Service::for_identity(IDENTITY))?
                    .restart()
                    .map_err(|_| "explicit idle service restart failed")?;
                let until = Instant::now() + Duration::from_secs(15);
                connection = loop {
                    match scope.owner.connect_pane() {
                        Ok(connection) => break connection,
                        Err(_) if Instant::now() < until => {
                            std::thread::sleep(Duration::from_millis(50))
                        }
                        Err(_) => return Err("server did not return after the explicit restart"),
                    }
                };
                require(
                    api(scope.owner.direct_status())? == status,
                    "restart did not retain exact network policy and server identity",
                )?;
                idle(&inventory(&mut connection)?.0)?;
                println!("PASS explicit idle restart retained exposure policy and server identity");
            }
            let mut peer = Peer::penguin()?;
            require(
                receive_json(&mut peer.output)?["os"].as_str() == Some("linux"),
                "second native host did not identify as Linux",
            )?;
            let paired = scope.enroll(&mut peer)?;
            println!(
                "PASS two-host claim proofs, exact retry, substitution refusal, approval and protocol major 1"
            );
            let failed = peer.message(json!({"create_workspace": format!("Direct devices cleanup {}", &hex(&random()?)[..12]), "fail_before_handoff": true}))?;
            scope.workspace = Some(api(serde_json::from_value(
                failed["workspace_owner"].clone(),
            ))?);
            require(
                failed["injected_failure"] == true,
                "post-creation failure witness missing",
            )?;
            require(
                inventory(&mut connection)?.0.workspaces == scope.baseline.workspaces,
                "creation failure before identity handoff left fixture organization",
            )?;
            scope.workspace = None;
            println!("PASS exact Workspace cleanup after creation failure before identity handoff");

            let response = peer.message(json!({"create_workspace": format!("Direct devices native {}", &hex(&random()?)[..12])}))?;
            scope.workspace = Some(api(serde_json::from_value(
                response["workspace_owner"].clone(),
            ))?);
            let owned = scope
                .workspace
                .as_ref()
                .ok_or("test Workspace ownership missing")?;
            let workspace = owned.workspace;
            require(
                owned.boot
                    == inventory(&mut connection)?
                        .0
                        .close
                        .ok_or("current server boot missing")?
                        .boot,
                "WSS-created Workspace changed server boot",
            )?;
            println!("SCOPE test Workspace {}", hex(&workspace));
            let response = peer.message(json!({"workspace_owned": workspace}))?;
            let pane: Id = api(serde_json::from_value(response["pane"].clone()))?;
            let pinned: std::path::PathBuf =
                api(serde_json::from_value(response["directory"].clone()))?;
            let owned = scope
                .workspace
                .as_mut()
                .ok_or("test Workspace ownership missing")?;
            require(
                pinned == owned.directory,
                "WSS-created Workspace directory changed during handoff",
            )?;
            owned.pane = Some(pane);
            let current = inventory(&mut connection)?.0;
            let work = current
                .workspaces
                .iter()
                .find(|work| work.id == workspace)
                .ok_or("test Workspace missing")?;
            require(
                work.pinned_directory.as_ref() == Some(&pinned),
                "WSS-created Workspace lost its owning-server directory",
            )?;
            require(
                work.tabs.first().is_some_and(|tab| tab.root_pane == pane),
                "WSS-created root Pane identity changed",
            )?;
            println!(
                "PASS WSS Workspace creation, directory browsing, physical matching and attachment"
            );
            let first = attached(select(&mut connection, pane, true)?)?;
            command(
                &mut connection,
                first,
                "stty -echo; printf '\\033[2J\\033[HDIRECT-LOCAL\\n'",
            )?;
            output(&mut connection, first, "DIRECT-LOCAL")?;
            let started = Instant::now();
            let mut next_health = started;
            let mut heartbeats = 0_u64;
            let response = peer.message_while(
                json!({"pane": pane, "owner": name, "session": first.session}),
                || {
                    if Instant::now() < next_health {
                        return Ok(());
                    }
                    // Keep this exact connection alive while the peer reads retained history.
                    // Capabilities observes the transport without acquiring Pane control.
                    let reply = connection.exchange(&Request::Capabilities);
                    if !matches!(reply, Ok(Reply::Capabilities { .. })) {
                        eprintln!(
                            "WSS fixture failure: stage=owner-keepalive, class={}, elapsed_ms={}, heartbeats={heartbeats}",
                            reply_classification(&reply),
                            started.elapsed().as_millis(),
                        );
                        return Err("original owner connection keepalive failed");
                    }
                    heartbeats += 1;
                    next_health = Instant::now() + Duration::from_secs(1);
                    Ok(())
                },
            )?;
            require(
                response["presentation_hidden"] == true,
                "hidden presentation phase missing",
            )?;
            let observer_name = format!("p612-observer-{}", &hex(&random()?)[..6]);
            let observer = api(ServerConnection::for_development_client(&observer_name, 1))?;
            let mut observer = api(observer.connect_pane())?;
            super::presentation::observe_hidden_inventory(&mut observer, pane).map_err(
                |failure| {
                    eprintln!("WSS inventory observer: {failure}");
                    "independent inventory observer did not see hidden metadata"
                },
            )?;
            let response = peer.message(json!({"presentation_observed": true}))?;
            println!(
                "PASS delayed WSS consumer: Scrollback checked={}, copied rows={}, accessible rows={}, graphics supported={}, graphics bytes={}",
                response["reading_checked"]
                    .as_bool()
                    .ok_or("reading capability report missing")?,
                response["copied_rows"]
                    .as_u64()
                    .ok_or("copy report missing")?,
                response["accessible_rows"]
                    .as_u64()
                    .ok_or("accessibility report missing")?,
                response["graphics_supported"]
                    .as_bool()
                    .ok_or("graphics negotiation report missing")?,
                response["graphics_bytes"]
                    .as_u64()
                    .ok_or("graphics capability report missing")?
            );
            let remote: Lease = api(serde_json::from_value(response["lease"].clone()))?;
            if remote.session != first.session {
                eprintln!(
                    "WSS fixture failure: stage=takeover-session, class=session-changed, elapsed_ms={}, heartbeats={heartbeats}",
                    started.elapsed().as_millis(),
                );
                return Err("WSS takeover changed the Session");
            }
            let stale = connection.exchange(&Request::Poll { lease: first });
            if !matches!(stale, Err(ServerError::StaleControl)) {
                eprintln!(
                    "WSS fixture failure: stage=takeover-old-control, class={}, elapsed_ms={}, heartbeats={heartbeats}",
                    reply_classification(&stale),
                    started.elapsed().as_millis(),
                );
                return Err("original owner connection did not refuse stale control");
            }
            println!(
                "PASS original owner connection refused stale control: elapsed_ms={}, heartbeats={heartbeats}",
                started.elapsed().as_millis(),
            );
            let returned = take(&mut connection, pane, DEVICE_NAME)?;
            require(
                returned.session == first.session,
                "local Take Over replaced the Session",
            )?;
            let response = peer.message(json!({"owner": name}))?;
            let remote: Lease = api(serde_json::from_value(response["lease"].clone()))?;
            require(
                remote.session == first.session,
                "second WSS Take Over replaced the Session",
            )?;
            api(scope.owner.revoke_device(random()?, paired.pairing))?;
            let response = peer.message(json!({"revoked": true}))?;
            require(
                response["revoked"] == true && response["self_confirmed"] == true,
                "revoked device did not reject existing connections and fresh authentication",
            )?;
            let returned = reclaim_after_revocation(&mut connection, pane)?;
            require(
                returned.session == first.session,
                "revocation killed or replaced the Terminal Session",
            )?;
            println!(
                "PASS named control, bulk Scrollback/available graphics, reconnect and exact revocation"
            );
            let last = scope.enroll(&mut peer)?;
            require(
                api(scope.owner.paired_devices())?
                    .iter()
                    .map(|device| device.pairing)
                    .collect::<Vec<_>>()
                    == vec![last.pairing],
                "last-device scope changed",
            )?;
            require(
                peer.message(json!({"pane": pane, "owner": name, "session": first.session}))?["ready"]
                    == true,
                "last device did not acquire the same Session",
            )?;
            require(
                peer.message(json!({"self_revoke": true}))?["self_retry"] == true,
                "last-device exact self-revocation retry failed",
            )?;
            require(
                api(scope.owner.paired_devices())?.is_empty(),
                "last-device revocation was not retained",
            )?;
            let returned = reclaim_after_revocation(&mut connection, pane)?;
            require(
                returned.session == first.session,
                "self-revocation killed or replaced the Terminal Session",
            )?;
            command(
                &mut connection,
                returned,
                "printf 'DIRECT-SURVIVED-UNPAIR\\n'",
            )?;
            output(&mut connection, returned, "DIRECT-SURVIVED-UNPAIR")?;
            peer.finish()?;
            println!(
                "PASS last-device self-unpair, discarded-receipt exact retry and preserved Terminal Session"
            );
            Ok(())
        })();
        let cleanup = scope.cleanup();
        if cleanup.is_ok() {
            println!(
                "PASS scoped cleanup: baseline Workspaces retained, test pairings revoked, exposure off"
            );
        } else {
            eprintln!(
                "direct_devices: scoped cleanup incomplete; inspect the bounded test scope before continuing"
            );
        }
        result.and(cleanup)
    }

    struct MemoryConnector {
        endpoint: String,
        paired: PairedDevice,
        credential: DeviceCredential,
    }
    impl DirectConnector for MemoryConnector {
        fn connect(&self) -> std::result::Result<DirectConnection, DirectError> {
            DirectConnection::connect(&self.endpoint, &self.paired, &self.credential)
        }
    }

    fn enroll_device<R: Read + AsRawFd, W: Write + AsRawFd>(
        input: &mut R,
        output: &mut W,
    ) -> Result<(ServerConnection, Arc<MemoryConnector>)> {
        let invitation = receive_invitation(input)?;
        let credential = api(DeviceCredential::generate())?;
        let mut enrollment = api(Enrollment::claim(
            &invitation,
            DEVICE_NAME.into(),
            &credential,
        ))?;
        require(
            api(enrollment.finish())?.is_none(),
            "enrollment completed without exact owner approval",
        )?;
        let code = enrollment
            .comparison_code()
            .ok_or("authenticated comparison missing")?;
        let retry_code = api(enrollment.submit())?;
        require(
            retry_code == code && api(enrollment.finish())?.is_none(),
            "exact enrollment retry changed the comparison or bypassed approval",
        )?;
        require(
            matches!(
                Enrollment::claim(
                    &invitation,
                    format!("{DEVICE_NAME}-changed").into(),
                    &credential
                ),
                Err(DirectError::Substituted)
            ),
            "claim retry accepted a substituted device name",
        )?;
        let wrong_key = api(DeviceCredential::generate())?;
        require(
            matches!(
                Enrollment::claim(&invitation, DEVICE_NAME.into(), &wrong_key),
                Err(DirectError::AlreadyDecided)
            ),
            "single-use invitation accepted another device key",
        )?;
        send_json(
            output,
            json!({"claim": enrollment.claim_id(), "fingerprint": credential.fingerprint(), "comparison_code": code.to_string()}),
        )?;
        require(
            receive_json(input)?["approved"] == true,
            "owner approval phase refused",
        )?;
        let paired = api(enrollment.finish())?.ok_or("approved claim remained pending")?;
        let endpoint = invitation.endpoint().to_owned();
        require(
            matches!(
                DirectConnection::connect(&endpoint, &paired, &wrong_key),
                Err(DirectError::AuthenticationRefused)
            ),
            "invalid device proof was accepted",
        )?;
        let mut wrong = paired.clone();
        wrong.pin.certificate[0] ^= 1;
        require(
            matches!(
                DirectConnection::connect(&endpoint, &wrong, &credential),
                Err(DirectError::PinMismatch)
            ),
            "substituted TLS certificate pin was not refused",
        )?;
        let mut wrong = paired.clone();
        wrong.pin.server[0] ^= 1;
        require(
            matches!(
                DirectConnection::connect(&endpoint, &wrong, &credential),
                Err(DirectError::WrongServer)
            ),
            "wrong server identity was not refused",
        )?;
        let connector = Arc::new(MemoryConnector {
            endpoint,
            paired: paired.clone(),
            credential,
        });
        let server = api(ServerConnection::direct(
            IDENTITY,
            paired.client,
            paired.clone(),
            connector.clone(),
        ))?;
        require(
            api(server.refresh())?.protocol_major == 1,
            "WSS did not negotiate initial major 1",
        )?;
        send_json(output, json!({"paired": paired}))?;
        Ok((server, connector))
    }

    fn directory_read(
        connection: &mut PaneConnection,
        operation: directories::Operation,
    ) -> Result<directories::Value> {
        let id = u64::from_be_bytes(
            random()?[..8]
                .try_into()
                .map_err(|_| "directory request identity unavailable")?,
        );
        let mut request = directories::Request::Begin { id, operation };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match api(connection.exchange(&Request::DirectoryRead(request)))? {
                Reply::DirectoryRead(Ok(directories::Progress::Ready(value))) => return Ok(value),
                Reply::DirectoryRead(Ok(directories::Progress::Pending))
                    if Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(25));
                    request = directories::Request::Poll { id };
                }
                _ => return Err("WSS directory read did not complete"),
            }
        }
    }

    fn create_remote_workspace(
        server: &ServerConnection,
        name: String,
        handoff: impl FnOnce(&FixtureWorkspace) -> Result<()>,
    ) -> Result<Value> {
        let mut connection = api(server.connect_pane())?;
        require(
            connection.supports_directory_matching(),
            "WSS directory matching is unavailable",
        )?;
        let directories::Value::Directory(home) =
            directory_read(&mut connection, directories::Operation::Home)?
        else {
            return Err("WSS home directory missing");
        };
        require(home.is_absolute(), "WSS home directory was not absolute")?;
        require(
            matches!(
                directory_read(
                    &mut connection,
                    directories::Operation::List { path: home.clone() }
                )?,
                directories::Value::Listing { .. }
            ),
            "WSS directory browsing did not return a listing",
        )?;
        let alias = home.join(".");
        require(
            matches!(
                directory_read(
                    &mut connection,
                    directories::Operation::Validate {
                        path: alias.clone()
                    }
                )?,
                directories::Value::Directory(_)
            ),
            "WSS directory validation failed",
        )?;
        require(
            matches!(
                directory_read(
                    &mut connection,
                    directories::Operation::Match {
                        path: alias,
                        candidates: vec![home.clone()]
                    }
                )?,
                directories::Value::Match { index: Some(0) }
            ),
            "WSS physical directory identity did not match",
        )?;
        let boot = inventory(&mut connection)?
            .0
            .close
            .ok_or("fixture boot missing")?
            .boot;
        let workspace = mutate(
            server,
            &mut connection,
            Mutation::CreateWorkspace {
                name: name.clone(),
                directory: Some(home.clone()),
            },
        )?;
        let mut cleanup = WorkspaceGuard {
            server,
            owned: Some(FixtureWorkspace {
                workspace,
                boot,
                name,
                directory: home.clone(),
                pane: None,
            }),
        };
        handoff(
            cleanup
                .owned
                .as_ref()
                .ok_or("created Workspace ownership missing")?,
        )?;
        let current = inventory(&mut connection)?.0;
        let work = current
            .workspaces
            .iter()
            .find(|work| work.id == workspace)
            .ok_or("WSS-created Workspace missing")?;
        require(
            work.pinned_directory.as_ref() == Some(&home),
            "WSS Workspace pin did not retain its owning directory",
        )?;
        let pane = work.tabs.first().ok_or("WSS root Tab missing")?.root_pane;
        let attachments = api(server.attachments())?;
        api(attachments.initialize(&current, Some(workspace)))?;
        require(
            attachments.is_attached(workspace),
            "WSS-created Workspace did not attach",
        )?;
        drop(attachments);
        require(
            api(server.attachments())?.is_attached(workspace),
            "WSS attachment did not survive reopening",
        )?;
        cleanup.owned = None;
        Ok(json!({"workspace": workspace, "pane": pane, "directory": home}))
    }

    fn device() -> Result<()> {
        require(
            std::env::consts::OS == "linux",
            "device role requires the native Penguin host",
        )?;
        pipe_only(libc::STDIN_FILENO)?;
        pipe_only(libc::STDOUT_FILENO)?;
        // SAFETY: this runs at single-threaded entry, before any WSS/native worker starts.
        unsafe {
            std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", DEVICE_NAME);
        }
        // read_exact polls the descriptor before each read. A buffered StdinLock can consume
        // the next frame bytes before poll sees them, so use an owned unbuffered descriptor.
        // SAFETY: standard input is live throughout this process; cloning owns its duplicate.
        let input = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
        let mut input = File::from(api(input.try_clone_to_owned())?);
        let mut output_pipe = std::io::stdout().lock();
        send_json(&mut output_pipe, json!({"os": std::env::consts::OS}))?;
        let (server, connector) = enroll_device(&mut input, &mut output_pipe)?;
        loop {
            let request = receive_json(&mut input)?;
            let name = request["create_workspace"]
                .as_str()
                .ok_or("WSS Workspace creation phase missing")?;
            if request["fail_before_handoff"] == true {
                let mut owned = None;
                let result = create_remote_workspace(&server, name.to_owned(), |workspace| {
                    owned = Some(workspace.clone());
                    Err("injected failure before Workspace identity handoff")
                });
                require(
                    matches!(
                        result,
                        Err("injected failure before Workspace identity handoff")
                    ),
                    "creation failure did not reach its post-creation boundary",
                )?;
                send_json(
                    &mut output_pipe,
                    json!({"injected_failure": true, "workspace_owner": owned.ok_or("failed creation ownership missing")?}),
                )?;
                continue;
            }
            let created = create_remote_workspace(&server, name.to_owned(), |owned| {
                send_json(&mut output_pipe, json!({"workspace_owner": owned}))?;
                require(
                    receive_json(&mut input)?["workspace_owned"] == json!(owned.workspace),
                    "coordinator did not acknowledge exact Workspace ownership",
                )
            })?;
            send_json(&mut output_pipe, created)?;
            break;
        }
        let scope = receive_json(&mut input)?;
        let pane: Id = api(serde_json::from_value(scope["pane"].clone()))?;
        let session: Id = api(serde_json::from_value(scope["session"].clone()))?;
        let owner = scope["owner"].as_str().ok_or("owner identity missing")?;
        let mut connection = api(server.connect_pane())?;
        let lease = take(&mut connection, pane, owner)?;
        require(
            lease.session == session,
            "WSS Take Over changed the Session",
        )?;
        let report =
            super::terminal_semantics::exercise(&mut connection, lease, super::support::command)
                .map_err(|failure| {
                    eprintln!("WSS fixture exercise failed: {failure}");
                    "WSS semantic protocol exercise failed"
                })?;
        let witness = super::presentation::begin(&mut connection, lease, super::support::command)
            .map_err(|failure| {
            eprintln!("WSS presentation begin: {failure}");
            "WSS presentation exercise failed"
        })?;
        send_json(&mut output_pipe, json!({"presentation_hidden": true}))?;
        require(
            receive_json(&mut input)?["presentation_observed"] == true,
            "owner did not observe hidden metadata",
        )?;
        let previous = lease;
        drop(connection);
        let mut connection = api(server.connect_pane())?;
        let lease = attached(select(&mut connection, pane, false)?)?;
        require(
            lease.session == session,
            "WSS reconnect lost the running Session",
        )?;
        super::presentation::finish(&mut connection, lease, previous, witness).map_err(
            |failure| {
                eprintln!("WSS presentation finish: {failure}");
                "WSS presentation reconnect or clear exercise failed"
            },
        )?;
        command(&mut connection, lease, "printf 'DIRECT-RECONNECTED\\n'")?;
        output(&mut connection, lease, "DIRECT-RECONNECTED")?;
        send_json(
            &mut output_pipe,
            json!({"lease": lease, "reading_checked": true,
            "copied_rows": report.copied_rows, "accessible_rows": report.accessible_rows,
            "graphics_supported": report.graphics_supported, "graphics_bytes": report.graphics_bytes}),
        )?;
        require(
            receive_json(&mut input)?["owner"].as_str() == Some(owner),
            "owner continuation changed",
        )?;
        let lease = take(&mut connection, pane, owner)?;
        let mut second = api(server.connect_pane())?;
        send_json(&mut output_pipe, json!({"lease": lease}))?;
        require(
            receive_json(&mut input)?["revoked"] == true,
            "revocation phase missing",
        )?;
        require(
            connection.exchange(&Request::Inventory).is_err()
                && second.exchange(&Request::Inventory).is_err(),
            "revocation retained an existing WSS connection",
        )?;
        require(
            matches!(connector.connect(), Err(DirectError::Revoked)),
            "revoked direct reconnect was accepted",
        )?;
        let other_pairing = connector.paired.pairing;
        let confirmation_operation = random()?;
        let confirmation = api(direct::revoke(
            &connector.endpoint,
            &connector.paired,
            &connector.credential,
            confirmation_operation,
            other_pairing,
        ))?;
        require(
            confirmation.server == connector.paired.pin.server
                && confirmation.operation == confirmation_operation
                && confirmation.target == other_pairing,
            "owner-revokes-first self-confirmation changed its binding",
        )?;
        drop(connection);
        drop(second);
        drop(server);
        drop(connector);
        send_json(
            &mut output_pipe,
            json!({"revoked": true, "self_confirmed": true}),
        )?;
        let (server, connector) = enroll_device(&mut input, &mut output_pipe)?;
        let scope = receive_json(&mut input)?;
        require(
            api(serde_json::from_value::<Id>(scope["pane"].clone()))? == pane
                && api(serde_json::from_value::<Id>(scope["session"].clone()))? == session,
            "last-device fixture scope changed",
        )?;
        let mut connection = api(server.connect_pane())?;
        require(
            take(&mut connection, pane, owner)?.session == session,
            "last device replaced the Session",
        )?;
        send_json(&mut output_pipe, json!({"ready": true}))?;
        require(
            receive_json(&mut input)?["self_revoke"] == true,
            "self-revocation phase missing",
        )?;
        let operation = random()?;
        let _ = api(direct::revoke(
            &connector.endpoint,
            &connector.paired,
            &connector.credential,
            operation,
            connector.paired.pairing,
        ))?;
        // Discard the first result as a client with a lost acknowledgement would, then resolve
        // only that exact original using its retired device authority.
        let retry = api(direct::revoke(
            &connector.endpoint,
            &connector.paired,
            &connector.credential,
            operation,
            connector.paired.pairing,
        ))?;
        require(
            retry.server == connector.paired.pin.server
                && retry.operation == operation
                && retry.target == connector.paired.pairing,
            "self-revocation original receipt changed",
        )?;
        // An owner may have revoked this device before its self-unpair reached the server.
        // Retired authority can confirm that same self pairing without submitting new effects.
        let confirmation_operation = random()?;
        let confirmation = api(direct::revoke(
            &connector.endpoint,
            &connector.paired,
            &connector.credential,
            confirmation_operation,
            connector.paired.pairing,
        ))?;
        require(
            confirmation.server == connector.paired.pin.server
                && confirmation.operation == confirmation_operation
                && confirmation.target == connector.paired.pairing,
            "retired self-confirmation changed its server, operation or target",
        )?;
        require(
            other_pairing != connector.paired.pairing,
            "two enrollment identities collided",
        )?;
        require(
            matches!(
                direct::revoke(
                    &connector.endpoint,
                    &connector.paired,
                    &connector.credential,
                    random()?,
                    other_pairing
                ),
                Err(DirectError::Revoked)
            ) && matches!(connector.connect(), Err(DirectError::Revoked)),
            "retired authority accepted another target or ordinary reconnect",
        )?;
        send_json(&mut output_pipe, json!({"self_retry": true}))?;
        require(
            receive_json(&mut input)?["done"] == true,
            "owner completion missing",
        )
    }

    pub(super) fn run() -> Result<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.as_slice() == ["device-pipe"] {
            return device();
        }
        let mut bind = None;
        let mut expected = None;
        let mut confirmed = false;
        let mut restart = false;
        let mut help = false;
        let mut args = args.iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--bind" => {
                    bind = Some(api(args
                        .next()
                        .ok_or("missing bind address")?
                        .parse::<SocketAddr>())?)
                }
                "--expect-server" => {
                    expected = Some(id(args.next().ok_or("missing server identity")?)?)
                }
                "--confirm-idle-development" => confirmed = true,
                "--restart-idle-service" => restart = true,
                "--help" => help = true,
                _ => return Err("unknown argument; use --help"),
            }
        }
        if !confirmed || help {
            println!("DRY RUN: no storage, service, desktop, SSH or WSS access performed.");
            println!(
                "Requires a parent-prepared foreground Development server and signed native examples on macOS and Penguin."
            );
            println!("Use --bind IP:PORT --expect-server SERVER_HEX --confirm-idle-development.");
            println!(
                "Confirms no user Terminal Sessions, paired devices, pending claims or outstanding invitations."
            );
            println!(
                "Touches one test-created Workspace, two temporary pairings and its own exposure policy; restores exposure off."
            );
            println!(
                "Optional --restart-idle-service authorizes restart before any fixture Terminal Session exists."
            );
            println!(
                "Invitations cross anonymous stdin/stdout pipes only; native device credentials remain in memory."
            );
            return Ok(());
        }
        require(
            std::env::consts::OS == "macos",
            "owner role requires the native macOS host",
        )?;
        owner(
            bind.ok_or("--bind is required")?,
            expected.ok_or("--expect-server is required")?,
            restart,
        )
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::net::UnixStream;

        use super::*;

        #[test]
        fn frame_receive_pumps_progress_through_idle_header_and_fragmented_body() {
            let (mut reader, mut writer) = UnixStream::pair().unwrap();
            let mut calls = 0;
            let received = receive_while(&mut reader, &mut || {
                calls += 1;
                match calls {
                    1 => {} // No bytes yet: progress must resume after the bounded poll wait.
                    2 => writer.write_all(&[0, 0]).unwrap(),
                    3 => writer.write_all(&[0, 4]).unwrap(),
                    4 => writer.write_all(b"ab").unwrap(),
                    5 => writer.write_all(b"cd").unwrap(),
                    _ => panic!("frame should already be complete"),
                }
                Ok(())
            })
            .unwrap();
            assert_eq!((received.as_slice(), calls), (b"abcd".as_slice(), 5));
        }

        #[test]
        fn frame_receive_propagates_body_progress_failure() {
            let (mut reader, mut writer) = UnixStream::pair().unwrap();
            writer.write_all(&4_u32.to_be_bytes()).unwrap();
            let mut calls = 0;
            let received = receive_while(&mut reader, &mut || {
                calls += 1;
                if calls == 2 {
                    Err("fixture progress refused")
                } else {
                    Ok(())
                }
            });
            assert_eq!(received, Err("fixture progress refused"));
        }

        #[test]
        fn expired_frame_deadline_does_not_run_progress() {
            let (mut reader, _writer) = UnixStream::pair().unwrap();
            let mut byte = [0];
            assert_eq!(
                read_exact(&mut reader, &mut byte, Instant::now(), &mut || {
                    panic!("expired frame must not run progress")
                }),
                Err("pipe phase deadline elapsed"),
            );
        }
    }
}

#[cfg(feature = "client")]
fn main() -> std::process::ExitCode {
    match exercise::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(classification) => {
            eprintln!("direct_devices: {classification}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "client"))]
fn main() {
    eprintln!("direct_devices requires the client feature");
    std::process::exit(2);
}
