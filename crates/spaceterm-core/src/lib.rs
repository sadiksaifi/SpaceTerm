//! Portable SpaceTerm domain contracts and policy.
//!
//! Identity, comparison stamps, shared organization, bounded semantic content,
//! durable mutation records, and pairing state live here. Neither an ID nor a
//! serialized fence is proof of authority. Server composition authenticates the
//! principal and checks current state before admitting and applying an effect.
//! This crate has no host, terminal engine, wire, async, or graphical dependency.

#![forbid(unsafe_code)]

pub mod authority;
pub mod content;
pub mod control;
pub mod geometry;
pub mod identity;
pub mod operations;
pub mod organization;
pub mod pairing;
pub mod storage;
