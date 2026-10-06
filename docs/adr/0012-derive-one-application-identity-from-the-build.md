# Derive one application identity from the build

Release, Preflight, and Development builds have distinct identities so they can run together
without sharing Settings or privacy grants. Development is the default because a mistaken build
must not write release state. Its microphone capability is absent because repeated ad hoc signing
would invalidate the grant across builds.
