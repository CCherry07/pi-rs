//! Incremental Markdown parsing, mending, highlighting, and Ratatui rendering.
//!
//! The parser and lexer are copied from the GPUI `pi-agent-md` crate. This
//! crate replaces the GPUI paint adapter with a Ratatui adapter while keeping
//! the document model independent of either UI toolkit.

pub mod highlight;
pub mod mend;
pub mod parser;
pub mod render;

pub use render::{Appearance, MarkdownTheme, RenderedMarkdown, render};
