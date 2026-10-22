//! Run only through server:compatibility:run:linux's disposable namespace.
#[path = "support/terminal_semantics.rs"]
#[expect(dead_code, reason = "shared bounded fixture diagnostics")]
mod diagnostics;
#[path = "support/lifecycle.rs"]
#[expect(dead_code, reason = "shared geometry and connect helpers")]
mod lifecycle;
#[path = "support/mod.rs"]
mod support;

use spaceterm_server::{LocalServer, ServerError, pane::*};
use std::{
    error::Error,
    path::Path,
    process::{Child, Command, Stdio},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Owned(Child);
impl Owned {
    fn start(binary: &Path) -> Result<Self> {
        Ok(Self(
            Command::new(binary)
                .args(["serve", "spaceterm-development"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ))
    }
    fn stop(&mut self) -> Result<()> {
        self.0.kill()?;
        self.0.wait()?;
        Ok(())
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn main() -> std::result::Result<(), diagnostics::Failure> {
    diagnostics::stage("older-server-process", run())
}

fn run() -> Result<()> {
    // The wrapper hides every host home and socket, including /tmp and /run. An environment
    // override alone cannot isolate account state because production resolves getpwuid_r.
    if std::fs::read_to_string("/tmp/compat/disposable")? != "spaceterm-compatibility-v1\n"
        || std::env::var("HOME").as_deref() != Ok("/home/spaceterm-compat")
    {
        return Err("use mise run server:compatibility:run:linux".into());
    }
    let binary = Path::new("/tmp/compat/installation/bin/spaceterm-server");
    let build = Command::new(binary).arg("--build-info").output()?;
    if !build.status.success() {
        return Err("server build information unavailable".into());
    }
    let build: serde_json::Value = serde_json::from_slice(&build.stdout)?;
    let expected = std::env::var("SPACETERM_COMPATIBILITY_COMMIT")?;
    if build["commit"] != expected
        || build["identity"] != "spaceterm-development"
        || build["protocol_major"] != 1
    {
        return Err("unexpected server identity or protocol".into());
    }
    println!("server_commit={expected}; identity=spaceterm-development; protocol_major=1");
    let client = LocalServer::for_development_client("compatibility", 1)?;
    client.setup()?;
    let mut host = Owned::start(binary)?;
    let mut connection = lifecycle::connect(&client)?;
    let original = support::attach(&mut connection, true)?;
    support::command(
        &mut connection,
        original,
        "stty -echo; printf '\\033[2J\\033[HBEFORE-RESTART\\n'",
    )?;
    support::await_output(&mut connection, original, "BEFORE-RESTART")?;
    // Exercise the new inventory decoder against actual old-server bytes, including omitted
    // optional metadata. Do not construct a current-version Inventory in this fixture.
    if !matches!(
        connection.exchange(&Request::Inventory)?,
        Reply::Inventory { .. }
    ) {
        return Err("inventory reply missing".into());
    }
    host.stop()?;
    drop(connection);
    let mut host = Owned::start(binary)?;
    let mut connection = lifecycle::connect(&client)?;
    let restored = connection.exchange(&Request::Select {
        create: false,
        size: lifecycle::size(),
        resume: Some(original.into()),
        style: None,
    })?;
    let Reply::Restored {
        server,
        workspace,
        tab,
        pane,
        ..
    } = restored
    else {
        return Err("restored Pane missing".into());
    };
    for _ in 0..3 {
        if !matches!(connection.exchange(&Request::SelectPane { pane, explicit: false, size: lifecycle::size(), style: None })?, Reply::Restored { workspace: w, tab: t, pane: p, .. } if (w,t,p)==(workspace,tab,pane))
        {
            return Err("passive reconnect started a shell".into());
        }
    }
    let Reply::Attached { lease } = connection.exchange(&Request::StartRestored {
        server,
        pane,
        size: lifecycle::size(),
        style: Some(PresentationStyle {
            light: false,
            foreground: [220; 3],
            background: [20; 3],
            palette: [[0; 3]; 16],
            cursor: [220; 3],
            cursor_text: None,
            bold_as_bright: false,
        }),
    })?
    else {
        return Err("restored shell failed to start".into());
    };
    if lease.session == original.session || lease.server == original.server {
        return Err("old Terminal Session survived restart".into());
    }
    support::command(
        &mut connection,
        lease,
        "stty -echo; printf '\\033[2J\\033[HAFTER-RESTART\\n'",
    )?;
    support::await_output(&mut connection, lease, "AFTER-RESTART")?;
    if connection
        .exchange(&Request::Poll { lease: original })
        .err()
        != Some(ServerError::SessionUnavailable)
    {
        return Err("old lease accepted".into());
    }
    drop(connection);
    println!("PASS: real older process restored startup and PTY input/output");
    // The old process stays alive for admission and capability cases. Relays change only
    // bounded protocol fields; they never replace the actual server's runtime or storage.
    relay::exercise(lease.into())?;
    host.stop()?;
    println!(
        "PASS: older process/newer client: inventory, dormant reconnect, explicit restored startup, PTY input/output, and stale lease refusal"
    );
    Ok(())
}

mod relay {
    use super::*;
    use spaceterm_server::account_bridge::{
        AccountBridge, AccountBridgeIdentity, AccountConnector,
    };
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[derive(Clone, Copy)]
    enum Mode {
        Optional,
        Required,
        Major,
    }
    struct Connector {
        expected: AccountBridgeIdentity,
        mode: Mode,
        operations: Arc<AtomicUsize>,
    }
    fn read(stream: &mut UnixStream) -> std::io::Result<serde_json::Value> {
        let mut header = [0; 4];
        stream.read_exact(&mut header)?;
        let size = u32::from_be_bytes(header) as usize;
        if size > MAX_TERMINAL_FRAME {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut bytes = vec![0; size];
        stream.read_exact(&mut bytes)?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn write(stream: &mut UnixStream, value: &serde_json::Value) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
        stream.write_all(&bytes)
    }
    impl AccountConnector for Connector {
        fn connect(&self) -> std::result::Result<AccountBridge, ServerError> {
            let (client, mut proxy) = UnixStream::pair().map_err(|_| ServerError::Protocol)?;
            let expected = self.expected.clone();
            let mode = self.mode;
            let operations = self.operations.clone();
            let worker = std::thread::spawn(move || -> std::io::Result<()> {
                proxy.set_read_timeout(Some(Duration::from_secs(12)))?;
                let socket = format!("/tmp/spaceterm-development-server-{}/s", expected.account);
                let mut server = UnixStream::connect(socket)?;
                server.set_read_timeout(Some(Duration::from_secs(12)))?;
                write(&mut proxy, &serde_json::to_value(expected)?)?;
                let hello = read(&mut proxy)?;
                write(&mut server, &hello)?;
                let mut offer = read(&mut server)?;
                if matches!(mode, Mode::Major) {
                    offer["selected"] = 0.into();
                }
                write(&mut proxy, &offer)?;
                while let Ok(request) = read(&mut proxy) {
                    if request != "Capabilities" {
                        operations.fetch_add(1, Ordering::Relaxed);
                    }
                    write(&mut server, &request)?;
                    let mut reply = read(&mut server)?;
                    if request == "Capabilities" {
                        if matches!(mode, Mode::Required) {
                            reply["Capabilities"]["workspace_policy"] = false.into();
                        }
                        if matches!(mode, Mode::Optional) {
                            for name in [
                                "key_input_batches",
                                "presentation_changes",
                                "graphics",
                                "accessibility",
                            ] {
                                reply["Capabilities"]
                                    .as_object_mut()
                                    .ok_or(std::io::ErrorKind::InvalidData)?
                                    .remove(name);
                            }
                        }
                    }
                    write(&mut proxy, &reply)?;
                }
                Ok(())
            });
            let close = client.try_clone().map_err(|_| ServerError::Protocol)?;
            AccountBridge::from_streams(
                Box::new(client.try_clone().map_err(|_| ServerError::Protocol)?),
                Box::new(client),
                Box::new(move || {
                    let _ = close.shutdown(std::net::Shutdown::Both);
                    let _ = worker.join();
                }),
                &self.expected,
            )
        }
    }
    pub fn exercise(target: PaneTarget) -> Result<()> {
        let identity: serde_json::Value = serde_json::from_slice(&std::fs::read(
            "/home/spaceterm-compat/.local/share/spaceterm-development/server/identity.json",
        )?)?;
        let expected = AccountBridgeIdentity {
            // SAFETY: geteuid has no preconditions.
            account: unsafe { libc::geteuid() },
            build: "spaceterm-development".into(),
            server: serde_json::from_value(identity["server"].clone())?,
        };
        let mut preserved: Option<(PaneConnection, Lease)> = None;
        for (index, mode) in [Mode::Optional, Mode::Required, Mode::Major]
            .into_iter()
            .enumerate()
        {
            let operations = Arc::new(AtomicUsize::new(0));
            let remote = LocalServer::remote(
                "spaceterm-development",
                [index as u8 + 1; 32],
                expected.clone(),
                Arc::new(Connector {
                    expected: expected.clone(),
                    mode,
                    operations: operations.clone(),
                }),
            )?;
            match mode {
                Mode::Required | Mode::Major => {
                    let error = if matches!(mode, Mode::Required) {
                        ServerError::MissingRequiredCapability
                    } else {
                        ServerError::IncompatibleProtocol
                    };
                    if remote.connect_pane().err() != Some(error)
                        || operations.load(Ordering::Relaxed) != 0
                    {
                        return Err("admission did not refuse before dependent operations".into());
                    }
                    let (connection, lease) = preserved.as_mut().ok_or("live shell missing")?;
                    let (command, marker) = if matches!(mode, Mode::Required) {
                        (
                            "printf '\\033[2J\\033[HREQUIRED-REFUSAL-PRESERVED\\n'",
                            "REQUIRED-REFUSAL-PRESERVED",
                        )
                    } else {
                        (
                            "printf '\\033[2J\\033[HMAJOR-REFUSAL-PRESERVED\\n'",
                            "MAJOR-REFUSAL-PRESERVED",
                        )
                    };
                    support::command(connection, *lease, command)?;
                    support::await_output(connection, *lease, marker)?;
                }
                Mode::Optional => {
                    let mut connection = remote.connect_pane()?;
                    if connection.supports_presentation_changes()
                        || connection.supports_graphics()
                        || connection.supports_accessibility()
                    {
                        return Err("absent optional capability enabled".into());
                    }
                    let Reply::Attached { lease } = connection.exchange(&Request::TakeOver {
                        target,
                        size: lifecycle::size(),
                        style: None,
                    })?
                    else {
                        return Err("optional-capability controller missing".into());
                    };
                    support::command(
                        &mut connection,
                        lease,
                        "stty -echo; printf '\\033[2J\\033[HOPTIONAL-FALLBACK\\n'",
                    )?;
                    support::await_output(&mut connection, lease, "OPTIONAL-FALLBACK")?;
                    preserved = Some((connection, lease));
                }
            }
        }
        println!(
            "PASS: real-process relay: optional capability fallback; required capability and major refusal before operations"
        );
        Ok(())
    }
}
