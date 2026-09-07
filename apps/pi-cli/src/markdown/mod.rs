//! TUI-owned incremental Markdown parsing, mending, highlighting, and Ratatui rendering.
//!
//! The parser and lexer are copied from the GPUI `pi-agent-md` crate. This
//! module replaces the GPUI paint adapter with a Ratatui adapter while keeping
//! the document model independent of either UI toolkit.
//!
//! The imported parser's complete internal API stays intact even though the TUI currently uses only
//! the one-shot render entry point. Its tests exercise the incremental seams directly.

#![allow(dead_code, unreachable_pub)]

pub mod highlight;
pub mod mend;
pub mod parser;
pub mod render;

pub use render::{Appearance, MarkdownTheme, render};
