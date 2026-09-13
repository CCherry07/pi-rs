#![forbid(unsafe_code)]

//! Shared, policy-free helpers used across Pi product modules.
//!
//! Utilities belong here only when several callers need the same mechanics.
//! Product validation, discovery, persistence, and error policy stay with the
//! module that owns the concept.

#[cfg(feature = "frontmatter")]
pub mod frontmatter;
pub mod path;
pub mod text;
pub mod time;
