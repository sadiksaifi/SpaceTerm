//! Creates this account's SpaceTerm Core state for one identity, as `setup` does before it
//! installs the service, and does nothing else. The compatibility pairs run it before they start
//! this commit's Core in the foreground. Run only against disposable account state.
use spaceterm_core::MachineConnection;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let (Some(identity), None) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: account_setup <identity>");
        return ExitCode::from(2);
    };
    match MachineConnection::for_identity(&identity).and_then(|core| core.setup()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
