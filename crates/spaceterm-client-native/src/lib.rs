//! Headless native client composition, separate from server production dependencies.
pub mod bootstrap;
pub mod cli;
pub mod credentials;
pub mod journal;
pub mod native;
pub mod route;

pub use native::NativeClientProvider;
