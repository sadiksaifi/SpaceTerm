//! Evidence-only shell entry and a single existing synthetic target attempt.

#[path = "../../../../tests/server_native/macos/evidence_client/client.rs"]
mod client;

fn main() -> std::process::ExitCode {
    client::entry()
}
