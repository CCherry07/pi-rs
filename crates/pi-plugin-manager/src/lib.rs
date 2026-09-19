//! Host-side plugin loading, installation and authoring.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

pub const HOST_TARGET: &str = env!("PI_PLUGIN_HOST_TARGET");

#[cfg(feature = "authoring")]
pub mod authoring;
#[cfg(feature = "install")]
pub mod install;
#[cfg(feature = "loader")]
#[allow(unsafe_code)]
pub mod loader;
