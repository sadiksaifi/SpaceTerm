//! The Data Generation of every data root, in the one form release tooling reads. Each root's
//! const stays on one line, which `rustfmt::skip` keeps:
//! `pub const <ROOT>: Generations = Generations { oldest: N, current: N };`.
//!
//! `current` names the format this build writes. `oldest` names the oldest generation its steps
//! still carry forward; retiring steps raises it. Neither ever decreases between releases.

/// The generations one data root accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Generations {
    pub oldest: u32,
    pub current: u32,
}

/// SpaceTerm Core's data root.
#[rustfmt::skip]
pub const CORE_DATA: Generations = Generations { oldest: 1, current: 1 };
/// The CLI's state root, `clients/cli` under SpaceTerm Core's data root.
#[rustfmt::skip]
pub const CLI_STATE: Generations = Generations { oldest: 1, current: 1 };
/// The desktop's configuration root, which holds the Settings Document.
#[rustfmt::skip]
pub const DESKTOP_CONFIG: Generations = Generations { oldest: 1, current: 1 };
/// The desktop's state root: Saved Connections, Client View State, Workspace order, presentation
/// context and update history.
#[rustfmt::skip]
pub const DESKTOP_STATE: Generations = Generations { oldest: 1, current: 1 };
