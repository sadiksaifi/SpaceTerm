/// How the connection to the SpaceTerm Server stands, as a Workspace that server owns presents it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerConnectionPhase {
    Connected,
    Reconnecting,
    Disconnected,
    Failed,
}
