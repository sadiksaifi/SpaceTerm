//! The `spaceterm` command line: its command tree, output contract, targeting, terminal I/O and
//! state. The executable in `main.rs` runs it.
pub mod args;
pub mod contract;
pub mod io;
pub mod journal;
pub mod list;
pub mod scope;
pub mod server;
pub mod state;
pub mod work;

pub mod connection;
