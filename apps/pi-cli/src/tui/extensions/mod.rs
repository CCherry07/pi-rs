//! Interactive plugin presentation as one deep module.
//!
//! `bridge` admits generation-scoped documents, `surface` owns semantic state and rendering,
//! `host` executes physical effects, and `startup` keeps the same frontend alive before a session
//! has finished activating. Callers should use this facade instead of depending on those pieces.

mod bridge;
mod graphics;
pub(super) mod host;
mod startup;
mod surface;
mod transcript;

pub(crate) use bridge::{PluginUiService, SurfaceReceiver};
pub(crate) use startup::during_startup;
pub(super) use surface::{Effect, ExtensionSurface};
pub(in crate::tui) use transcript::snapshot as transcript_snapshot;
