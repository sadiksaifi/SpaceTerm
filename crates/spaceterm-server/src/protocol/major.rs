//! The protocol major. It lives alone and uses no other item, so the CLI's build script can
//! include this file and stamp the same major into server build documents.

/// The protocol major of the pushed, multiplexed exchange.
pub const MAJOR: u16 = 1;
