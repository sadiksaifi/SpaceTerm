//! The protocol major. It lives alone and uses no other item, so the CLI's build script can
//! include this file and stamp the same major into server build documents.

/// The work protocol major, which build documents carry. It always equals
/// `version::WORK.major`.
pub const MAJOR: u16 = 1;
