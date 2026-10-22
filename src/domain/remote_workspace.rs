/// How the connection to the SpaceTerm Server on another computer stands, as a Workspace that
/// server owns presents it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteConnectionPhase {
    Connected,
    Reconnecting,
    Disconnected,
    Failed,
}
