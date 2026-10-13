//! Headless SpaceTerm Server composition.
//!
//! Owns authenticated transports, runtime worker composition, publication,
//! recovery, management CLI, and orderly shutdown. Tokio drives transports and
//! composition; terminal workers remain OS threads. Account host adapters supply
//! private file-backed server keys, retained storage and direct process ownership.

#![forbid(unsafe_code)]

pub mod account_startup;
pub mod deployment;
pub mod distribution_metadata;
pub use account_startup::{StartupFailure, production_startup};

pub mod api;

pub mod failure;

pub mod service;

pub mod listeners;

pub mod connection;

pub mod protocol_dispatch;

pub mod control;

pub mod startup_intents;

pub mod runtime_registry;

pub mod recovery;

pub mod shutdown;

pub mod publication;

pub mod egress;

pub mod extensions;

pub mod pairing;

pub mod git;

pub mod adapters;

pub mod contracts;
