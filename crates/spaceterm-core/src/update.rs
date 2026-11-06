//! SpaceTerm Core updates: the facts a Core reports about them, the release feed it reads, the
//! keys that sign every release, and the sources new versions come from.
pub mod check;
pub mod facts;
pub mod feed;
pub mod fetch;
pub mod ordering;
pub mod proxy;
pub mod resource_manifest;
pub mod source;
pub mod sources;
pub mod system;
pub mod trust;
