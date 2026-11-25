//! Optional shared-process Host control client, separate from `host::HostSession`
//! (which owns native RPC child processes). This module never spawns rpi, owns
//! an Agent, reads a transcript, or silently falls back to native session RPC.
//! Callers supply an explicit endpoint, host identity and private credential.
//! Wire declarations are not a claim that a running host implements them.

pub mod client;
pub mod protocol;

pub use client::{ClientError, ConnectionConfig, Failure, HostClient};
