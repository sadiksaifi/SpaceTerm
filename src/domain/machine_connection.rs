/// How the connection to the SpaceTerm Server stands, as a Workspace that server owns presents it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MachineConnectionPhase {
    Connected,
    Reconnecting,
    Disconnected,
    Failed,
}
