//! Provider-neutral multimodal input processing.
//!
//! Callers own file/clipboard I/O and scheduling. This crate owns media
//! identification, conversion and inline limits; `pi-core` owns message content.
//! Images are the first supported modality.
#![forbid(unsafe_code)]

pub mod image;

/// Media failures retain their meaning independently of tools or frontends.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("[Image omitted: could not be converted to a supported inline image format.]")]
    InvalidImage,
    #[error("[Image omitted: could not be resized below the inline image size limit.]")]
    ImageSizeLimit,
    #[error(
        "image limits must have positive dimensions and an encoded byte limit greater than four"
    )]
    InvalidImagePolicy,
}
