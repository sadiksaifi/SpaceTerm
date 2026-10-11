# Support current and previous major protocol versions

Each SpaceTerm Client and SpaceTerm Server release supports the current and previous major versions of SpaceTerm Protocol, so devices can update separately while compatibility support remains bounded. Connections negotiate the newest supported version they share; app release numbers alone do not determine whether a connection works.

Breaking protocol changes introduce a new major protocol version. Optional additions use negotiated capabilities, and incompatibility outside the supported versions follows [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md).
