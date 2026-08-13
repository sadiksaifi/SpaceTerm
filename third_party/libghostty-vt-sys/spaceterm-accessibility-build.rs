// Keep SpaceTerm's vendored build entry point distinct from the upstream crate.
// The accessibility-specific filename also prevents shared target directories
// from reusing build-script artifacts for a different SpaceTerm patch stack.
include!("build.rs");
