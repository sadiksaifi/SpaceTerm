# Derive one application identity from the build

Release, Preflight, and Development builds have distinct identities so they can run together without sharing Settings or privacy grants. Development is the default because a mistaken build must not write release state.

The command-line launcher uses the build identity: `spaceterm`, `spaceterm-preflight` or `spaceterm-development`. It selects that identity's headless executable and account state under [ADR 0054](0054-use-one-headless-executable.md). The desktop binary is `spaceterm-desktop`; the macOS bundle executable remains `SpaceTerm`.
