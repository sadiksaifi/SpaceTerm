//! Protocol latency probe for `mise run bench:latency:protocol`.
//!
//! latency_probe measure IDENTITY local [--samples N] [--requests N]
//! latency_probe measure IDENTITY paired [--owner-command COMMAND] [--expose ADDRESS] [...]
//! latency_probe owner IDENTITY
//!
//! `measure` creates a probe Workspace whose Pane runs a raw `cat`, times request round trips
//! and key-to-echo, closes the Workspace and prints one JSON result per line. A paired run
//! enrolls a fresh pairing through an owner on the server's machine and revokes it afterwards.
//! Without `--owner-command` the owner is this process, so the server must be on this machine;
//! `--expose` enables its network exposure for the run. `owner` serves the owner side as JSON
//! lines on standard input and output. Protocol calls live in `protocol.rs`.
mod protocol;

use protocol::{Claim, EchoPane, Failure, Owner, Route, fail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Failure>;

struct Options {
    identity: String,
    route: String,
    owner_command: Option<String>,
    expose: Option<std::net::SocketAddr>,
    samples: usize,
    requests: usize,
}

fn options(arguments: &[String]) -> Result<Options> {
    let [identity, route, flags @ ..] = arguments else {
        return fail("usage: measure IDENTITY local|paired [options]");
    };
    let mut options = Options {
        identity: identity.clone(),
        route: route.clone(),
        owner_command: None,
        expose: None,
        samples: 200,
        requests: 50,
    };
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let Some(value) = flags.next() else {
            return fail("an option is missing its value");
        };
        match flag.as_str() {
            "--owner-command" => options.owner_command = Some(value.clone()),
            "--expose" => match value.parse() {
                Ok(address) => options.expose = Some(address),
                Err(_) => return fail("--expose needs an IP address and port"),
            },
            "--samples" => match value.parse() {
                Ok(count) if count > 0 => options.samples = count,
                _ => return fail("--samples needs a positive count"),
            },
            "--requests" => match value.parse() {
                Ok(count) if count > 0 => options.requests = count,
                _ => return fail("--requests needs a positive count"),
            },
            _ => return fail("unknown option"),
        }
    }
    if options.route != "local" && options.route != "paired" {
        return fail("the route must be local or paired");
    }
    if options.expose.is_some() && options.owner_command.is_some() {
        return fail("--expose needs the owner on this machine");
    }
    Ok(options)
}

fn emit(measurement: &str, route: &str, samples: &[f64]) {
    let line = json!({
        "measurement": measurement,
        "app": route,
        "unit": "ms",
        "samples": samples,
    });
    println!("{line}");
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// A small xorshift generator for human-like gaps between keys.
struct Gaps(u64);
impl Gaps {
    fn next(&mut self) -> Duration {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let unit = (self.0 >> 11) as f64 / (1u64 << 53) as f64;
        Duration::from_millis(80 + (unit * 80.0) as u64)
    }
}

fn sample(pane: &mut EchoPane, options: &Options, route: &str) -> Result<()> {
    let mut round_trips = Vec::with_capacity(options.requests);
    for _ in 0..options.requests {
        let start = Instant::now();
        pane.round_trip()?;
        round_trips.push(ms(start.elapsed()));
        std::thread::sleep(Duration::from_millis(20));
    }
    emit("request round trip", route, &round_trips);

    let mut gaps = Gaps(0x9e37_79b9_7f4a_7c15 ^ u64::from(std::process::id()));
    let mut echoes = Vec::with_capacity(options.samples);
    pane.rewind()?;
    for index in 0..options.samples {
        if index > 0 && index % 40 == 0 {
            pane.rewind()?;
        }
        let character = char::from(b'a' + (index % 26) as u8);
        let start = Instant::now();
        pane.send_key(character)?;
        pane.await_echo(start + Duration::from_secs(5))?;
        echoes.push(ms(start.elapsed()));
        std::thread::sleep(gaps.next());
    }
    emit("key to echo", route, &echoes);
    Ok(())
}

/// The owner of the server under test, in this process or behind an owner command.
enum OwnerLink {
    Here(Owner),
    Command {
        child: Child,
        input: Option<ChildStdin>,
        output: BufReader<ChildStdout>,
    },
}

impl OwnerLink {
    fn open(options: &Options) -> Result<Self> {
        let Some(command) = &options.owner_command else {
            return Owner::open(&options.identity).map(Self::Here);
        };
        let Ok(mut child) = Command::new("/bin/sh")
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        else {
            return fail("the owner command did not start");
        };
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            return fail("owner pipes");
        };
        Ok(Self::Command {
            child,
            input: Some(input),
            output: BufReader::new(output),
        })
    }

    fn ask(&mut self, request: Value) -> Result<Value> {
        let Self::Command { input, output, .. } = self else {
            return fail("owner pipe");
        };
        let Some(input) = input else {
            return fail("owner pipe");
        };
        let mut line = String::new();
        if writeln!(input, "{request}").is_err()
            || input.flush().is_err()
            || output.read_line(&mut line).is_err()
        {
            return fail("the owner command stopped");
        }
        let Ok(reply) = serde_json::from_str::<Value>(&line) else {
            return fail("the owner command replied with something other than JSON");
        };
        match reply["error"].as_str() {
            Some(_) => fail("the owner refused a request"),
            None => Ok(reply),
        }
    }

    fn invite(&mut self) -> Result<String> {
        if let Self::Here(owner) = self {
            return owner.invite();
        }
        let reply = self.ask(json!({ "invite": true }))?;
        reply["invitation"]
            .as_str()
            .map(str::to_owned)
            .map_or_else(|| fail("owner invitation"), Ok)
    }

    fn approve(&mut self, device: &str) -> Result<bool> {
        if let Self::Here(owner) = self {
            return owner.approve(device);
        }
        let reply = self.ask(json!({ "approve": device }))?;
        Ok(reply["approved"] == true)
    }

    fn revoke(&mut self, device: &str) -> Result<usize> {
        if let Self::Here(owner) = self {
            return owner.revoke(device);
        }
        let reply = self.ask(json!({ "revoke": device }))?;
        reply["revoked"]
            .as_u64()
            .map(|count| count as usize)
            .map_or_else(|| fail("owner revocation"), Ok)
    }
}

impl Drop for OwnerLink {
    fn drop(&mut self) {
        if let Self::Command { child, input, .. } = self {
            // Closing standard input ends the owner loop and its command.
            drop(input.take());
            let _ = child.wait();
        }
    }
}

fn measure_route(options: &Options, route: &Route, label: &str) -> Result<()> {
    let started = Instant::now();
    let mut pane = EchoPane::open(&options.identity, route)?;
    eprintln!(
        "latency_probe: probe Pane ready after {:.0} ms",
        ms(started.elapsed())
    );
    let result = sample(&mut pane, options, label);
    let closed = pane.close();
    result.and(closed)
}

fn measure_paired(options: &Options) -> Result<()> {
    let mut owner = OwnerLink::open(options)?;
    let exposure = match (&owner, options.expose) {
        (OwnerLink::Here(here), Some(bind)) => Some(here.expose(bind)?),
        _ => None,
    };
    let device = format!("latency-probe-{}", std::process::id());
    let result = (|| {
        let invitation = owner.invite()?;
        let claim = Claim::submit(&invitation, &device)?;
        if !owner.approve(&device)? {
            return fail("the owner found no pairing claim");
        }
        let route = claim.finish()?;
        measure_route(options, &route, "paired")
    })();
    // Revoke even after a failure, since approval may have succeeded.
    let revoked = owner.revoke(&device);
    if let Ok(count) = revoked {
        eprintln!("latency_probe: revoked {count} probe pairing");
    }
    let restored = match (&owner, exposure) {
        (OwnerLink::Here(here), Some(exposure)) => here.restore(exposure),
        _ => Ok(()),
    };
    result.and(revoked.map(drop)).and(restored)
}

fn serve_owner(identity: &str) -> Result<()> {
    let owner = Owner::open(identity)?;
    let mut output = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            return fail("owner input");
        };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            return fail("owner input is not JSON");
        };
        let reply = if request["invite"] == true {
            owner.invite().map(|text| json!({ "invitation": text }))
        } else if let Some(device) = request["approve"].as_str() {
            owner
                .approve(device)
                .map(|approved| json!({ "approved": approved }))
        } else if let Some(device) = request["revoke"].as_str() {
            owner
                .revoke(device)
                .map(|count| json!({ "revoked": count }))
        } else {
            break;
        };
        let reply = reply.unwrap_or_else(|failure| json!({ "error": failure.stage }));
        if writeln!(output, "{reply}").is_err() || output.flush().is_err() {
            return fail("owner output");
        }
    }
    Ok(())
}

fn main() {
    protocol::name_client("latency-probe");
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.split_first() {
        Some((command, rest)) if command == "measure" => options(rest).and_then(|options| {
            if options.route == "local" {
                measure_route(&options, &Route::Local, "local")
            } else {
                measure_paired(&options)
            }
        }),
        Some((command, [identity])) if command == "owner" => serve_owner(identity),
        _ => fail("usage: measure IDENTITY local|paired [options] | owner IDENTITY"),
    };
    if let Err(failure) = result {
        eprintln!("latency_probe: {failure}");
        std::process::exit(1);
    }
}
