//! External interoperability adapters for the Pi session runtime.
//!
//! This crate owns Pi's stdin/stdout RPC protocol and its shared JSON event
//! projection. ACP is a sibling adapter in `pi-acp`; both depend on the same
//! protocol-neutral session runtime rather than depending on each other.

#![warn(unreachable_pub)]

pub mod json_wire;
pub mod rpc;
