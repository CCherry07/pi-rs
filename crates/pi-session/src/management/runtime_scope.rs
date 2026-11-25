use std::sync::atomic::{AtomicU64, Ordering};

/// Opaque identity of one live conversation's runtime scope, not its saved journal.
///
/// Reload and automatic generation refresh preserve the scope. Initial creation,
/// new, resume (even of the same journal), and fork allocate a fresh scope; failed
/// replacement does not change the current scope. IDs are unique within this
/// process and are never serialized into session records or resource metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LiveRuntimeScopeId(u64);

impl LiveRuntimeScopeId {
    /// Allocate an independent live runtime scope. Managed sessions allocate this
    /// before preparing their first generation; custom factory callers may do so too.
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .expect("live runtime scope identities exhausted"),
        )
    }
}

impl Default for LiveRuntimeScopeId {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
