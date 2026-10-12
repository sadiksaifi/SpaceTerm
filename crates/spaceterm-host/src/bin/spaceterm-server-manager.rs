//! Fixed manager entry point. No native-authority composition was supplied.
//! Valid requests remain fail-closed, including under sudo or pkexec.

use std::ffi::OsStr;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).take(5).collect();
    let valid = match arguments.as_slice() {
        [mode] => mode == OsStr::new("--gui-demand") || mode == OsStr::new("--request-stdin"),
        [operation, transport] => {
            [
                "install",
                "stage",
                "activate",
                "start",
                "stop",
                "restart",
                "uninstall",
                "repair",
                "collect",
                "remote-access",
                "owner-enrollment",
            ]
            .iter()
            .any(|name| operation == OsStr::new(name))
                && transport == OsStr::new("--request-stdin")
        }
        _ => false,
    };
    if valid {
        println!("classification=authorization-unavailable");
        ExitCode::from(18)
    } else {
        println!("classification=invalid-record");
        ExitCode::from(9)
    }
}
