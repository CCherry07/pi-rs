mod fastembed;
pub(crate) mod initialization;

pub use fastembed::{
    FastEmbedInstallReceipt, FastEmbedModelError, FastEmbedModelState, FastEmbedModelStatus,
    FastEmbedModelStore,
};

use async_trait::async_trait;

/// Immutable identity of one embedding space.
///
/// Changing any field requires rebuilding the derived vector index. The
/// descriptor deliberately stays outside [`crate::MemoryRecord`]: vectors and
/// model identity are provider data, not canonical session data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingDescriptor {
    pub model: String,
    pub revision: String,
    pub dimensions: usize,
}

impl EmbeddingDescriptor {
    pub fn validate(&self) -> Result<(), EmbeddingError> {
        if self.model.trim().is_empty() {
            return Err(EmbeddingError::InvalidDescriptor(
                "model must not be empty".to_string(),
            ));
        }
        if self.revision.trim().is_empty() {
            return Err(EmbeddingError::InvalidDescriptor(
                "revision must not be empty".to_string(),
            ));
        }
        if self.dimensions == 0 {
            return Err(EmbeddingError::InvalidDescriptor(
                "dimensions must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingPurpose {
    Query,
    Document,
}

/// Interface at the SQLite Adapter's internal embedding seam.
///
/// Implementations own model-specific query/document prefixes and batching.
/// Callers receive one vector for every input, in the same order.
#[async_trait]
pub trait MemoryEmbedder: Send + Sync {
    fn descriptor(&self) -> &EmbeddingDescriptor;

    async fn embed(
        &self,
        purpose: EmbeddingPurpose,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmbeddingError {
    #[error("invalid embedding descriptor: {0}")]
    InvalidDescriptor(String),
    #[error("embedding provider failed: {0}")]
    Provider(String),
    #[error("embedding provider returned invalid output: {0}")]
    InvalidOutput(String),
}

pub(crate) fn validate_embeddings(
    descriptor: &EmbeddingDescriptor,
    expected_count: usize,
    embeddings: Vec<Vec<f32>>,
) -> Result<Vec<Vec<u8>>, EmbeddingError> {
    descriptor.validate()?;
    if embeddings.len() != expected_count {
        return Err(EmbeddingError::InvalidOutput(format!(
            "expected {expected_count} vectors, received {}",
            embeddings.len()
        )));
    }
    embeddings
        .into_iter()
        .enumerate()
        .map(|(index, embedding)| {
            if embedding.len() != descriptor.dimensions {
                return Err(EmbeddingError::InvalidOutput(format!(
                    "vector {index} has {} dimensions; expected {}",
                    embedding.len(),
                    descriptor.dimensions
                )));
            }
            if embedding.iter().any(|value| !value.is_finite()) {
                return Err(EmbeddingError::InvalidOutput(format!(
                    "vector {index} contains a non-finite value"
                )));
            }
            Ok(embedding
                .iter()
                .flat_map(|value| value.to_ne_bytes())
                .collect())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> EmbeddingDescriptor {
        EmbeddingDescriptor {
            model: "test".to_string(),
            revision: "v1".to_string(),
            dimensions: 2,
        }
    }

    #[test]
    fn validates_count_dimensions_and_finite_values() {
        assert!(validate_embeddings(&descriptor(), 1, vec![vec![1.0, 0.0]]).is_ok());
        assert!(validate_embeddings(&descriptor(), 2, vec![vec![1.0, 0.0]]).is_err());
        assert!(validate_embeddings(&descriptor(), 1, vec![vec![1.0]]).is_err());
        assert!(validate_embeddings(&descriptor(), 1, vec![vec![f32::NAN, 0.0]]).is_err());
    }
}
