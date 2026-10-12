use std::sync::Mutex;

// Both fixture modules mutate temporary ancestors observed without name filtering.
// Hold this shared lock from before fixture construction until after cleanup, not
// just around protected-directory operations.
pub(crate) static FIXTURE_NAMESPACE_LOCK: Mutex<()> = Mutex::new(());
