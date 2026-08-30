use std::fmt;
use std::sync::Arc;

use crate::AssistantMessage;

/// Stable identity for one assistant response while it is streaming.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AssistantStreamId(Arc<str>);

impl AssistantStreamId {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AssistantStreamId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AssistantStreamId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for AssistantStreamId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Read-only view of a live assistant stream.
///
/// Implementations own incremental assembly. Calling [`Self::snapshot`]
/// materializes the complete cumulative message and is intentionally the only
/// operation whose cost grows with the response size.
#[doc(hidden)]
pub trait AssistantStreamView: Send + Sync {
    fn snapshot(&self) -> Option<AssistantMessage>;
}

/// Cloneable handle to the one live accumulator for an assistant response.
///
/// Cloning the handle and forwarding stream updates are O(1). Consumers that
/// only need deltas should not call [`Self::snapshot`].
#[derive(Clone)]
pub struct AssistantStream {
    id: AssistantStreamId,
    view: Arc<dyn AssistantStreamView>,
}

impl AssistantStream {
    #[doc(hidden)]
    pub fn new(id: AssistantStreamId, view: Arc<dyn AssistantStreamView>) -> Self {
        Self { id, view }
    }

    pub fn id(&self) -> &AssistantStreamId {
        &self.id
    }

    pub fn snapshot(&self) -> Option<AssistantMessage> {
        self.view.snapshot()
    }
}

impl fmt::Debug for AssistantStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssistantStream")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl PartialEq for AssistantStream {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for AssistantStream {}
