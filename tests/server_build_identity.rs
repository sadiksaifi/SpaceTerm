//! Exercises the server build script without linking any desktop or native startup code.
#[allow(
    dead_code,
    reason = "this suite exercises only the server identity derivation"
)]
#[path = "../build/identity.rs"]
mod identity;

#[test]
fn server_bundle_names_do_not_claim_desktop_activation() {
    for build in [
        identity::ServerBuild::Release,
        identity::ServerBuild::Preflight,
        identity::ServerBuild::Development,
    ] {
        assert!(build.bundle_name().ends_with("Server"));
        assert!(build.identifier().ends_with(".server"));
        assert_ne!(build.identifier(), build.launcher_identifier());
        assert_ne!(build.identifier(), build.manager_identifier());
    }
}
