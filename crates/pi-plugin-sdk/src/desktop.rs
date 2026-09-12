//! Author-facing transport for durable desktop presentation state.
//!
//! The host owns the entry envelope, limits, and duplicate suppression. Plugins
//! continue to own each value's schema, migration, and business interpretation.

use pi_core::{PluginContextError, SessionContext};
use serde::Serialize;
use serde_json::{Value, json};

pub const WIDGET_ENTRY_TYPE: &str = "pi.ui.widget";
pub const MAX_WIDGET_VALUE_BYTES: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WidgetPublishError {
    #[error("desktop widget keys must be namespaced ASCII identifiers up to 128 bytes")]
    InvalidKey,
    #[error("desktop widget value could not be serialized: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("desktop widget value exceeds 256 KiB ({bytes} bytes)")]
    TooLarge { bytes: usize },
    #[error(transparent)]
    Context(#[from] PluginContextError),
}

/// Publishes one plugin-owned value while hiding the durable entry protocol.
///
/// A publisher belongs to one widget key in one session. It updates its
/// duplicate cache only after the session append succeeds, so a retired
/// context cannot suppress a retry through a replacement context.
#[derive(Debug)]
pub struct WidgetPublisher {
    key: String,
    published: Option<Value>,
}

impl WidgetPublisher {
    pub fn new(key: impl Into<String>) -> Result<Self, WidgetPublishError> {
        let key = key.into();
        if key.is_empty()
            || key.len() > 128
            || !key.contains('.')
            || key.split('.').any(str::is_empty)
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(WidgetPublishError::InvalidKey);
        }
        Ok(Self {
            key,
            published: None,
        })
    }

    /// Returns `true` only when a new durable entry was appended.
    pub fn publish<T: Serialize>(
        &mut self,
        session: &SessionContext,
        value: &T,
    ) -> Result<bool, WidgetPublishError> {
        let value = serde_json::to_value(value)?;
        self.publish_value(session, value)
    }

    /// Removes the key from the materialized widget map.
    pub fn remove(&mut self, session: &SessionContext) -> Result<bool, WidgetPublishError> {
        self.publish_value(session, Value::Null)
    }

    fn publish_value(
        &mut self,
        session: &SessionContext,
        value: Value,
    ) -> Result<bool, WidgetPublishError> {
        if self.published.as_ref() == Some(&value) {
            return Ok(false);
        }
        let bytes = serde_json::to_vec(&value)?.len();
        if bytes > MAX_WIDGET_VALUE_BYTES {
            return Err(WidgetPublishError::TooLarge { bytes });
        }
        session.append_entry(
            WIDGET_ENTRY_TYPE,
            Some(json!({"key":self.key,"value":value})),
        )?;
        self.published = Some(value);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pi_core::{
        ModelsContextAccess, PluginContextEpoch, PluginContextResult, SessionContextAccess,
        UiContextAccess,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Journal(Mutex<Vec<Value>>);

    impl ModelsContextAccess for Journal {}
    #[async_trait]
    impl UiContextAccess for Journal {}
    #[async_trait]
    impl SessionContextAccess for Journal {
        fn append_entry(
            &self,
            custom_type: String,
            data: Option<Value>,
        ) -> PluginContextResult<()> {
            self.0
                .lock()
                .unwrap()
                .push(json!({"customType":custom_type,"data":data}));
            Ok(())
        }
    }

    #[test]
    fn validates_namespaced_keys() {
        assert!(WidgetPublisher::new("checks.progress").is_ok());
        for key in ["checks", "checks:progress", ".", ""] {
            assert!(matches!(
                WidgetPublisher::new(key),
                Err(WidgetPublishError::InvalidKey)
            ));
        }
    }

    #[test]
    fn appends_only_changed_values_and_uses_a_tombstone_for_removal() {
        let journal = Arc::new(Journal::default());
        let epoch = PluginContextEpoch::new(journal.clone());
        let session = epoch.context().session;
        let mut publisher = WidgetPublisher::new("checks.progress").unwrap();

        assert!(publisher.publish(&session, &json!({"done":1})).unwrap());
        assert!(!publisher.publish(&session, &json!({"done":1})).unwrap());
        assert!(publisher.remove(&session).unwrap());
        assert!(!publisher.remove(&session).unwrap());

        let entries = journal.0.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["data"]["key"], "checks.progress");
        assert_eq!(entries[1]["data"]["value"], Value::Null);
    }

    #[test]
    fn failed_writes_and_oversized_values_do_not_poison_duplicate_state() {
        let journal = Arc::new(Journal::default());
        let retired = PluginContextEpoch::new(journal.clone());
        let retired_session = retired.context().session;
        retired.retire();
        let mut publisher = WidgetPublisher::new("checks.progress").unwrap();
        let value = json!({"done":2});
        assert!(matches!(
            publisher.publish(&retired_session, &value),
            Err(WidgetPublishError::Context(PluginContextError::Retired))
        ));

        let current = PluginContextEpoch::new(journal.clone()).context().session;
        assert!(publisher.publish(&current, &value).unwrap());
        assert!(matches!(
            publisher.publish(&current, &"x".repeat(MAX_WIDGET_VALUE_BYTES + 1)),
            Err(WidgetPublishError::TooLarge { .. })
        ));
        assert_eq!(journal.0.lock().unwrap().len(), 1);
    }
}
