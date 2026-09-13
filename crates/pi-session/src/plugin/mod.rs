//! Session plugin contracts and their generation-local runtime driver.

mod contract;
mod driver;

pub use contract::*;
pub(crate) use driver::SessionPluginDriver;
pub use driver::SessionPlugins;
