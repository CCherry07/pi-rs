use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};

use pi_core::Message;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueueMode {
    All,
    #[default]
    OneAtATime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueDestination {
    Steering,
    FollowUp,
}

#[derive(Debug, Default)]
pub struct PendingMessageQueue {
    mode: RwLock<QueueMode>,
    messages: Mutex<VecDeque<(Option<String>, Message)>>,
}

impl PendingMessageQueue {
    pub fn new(mode: QueueMode) -> Self {
        Self {
            mode: RwLock::new(mode),
            messages: Mutex::new(VecDeque::new()),
        }
    }

    pub fn set_mode(&self, mode: QueueMode) {
        *self
            .mode
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
    }

    pub fn mode(&self) -> QueueMode {
        *self
            .mode
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn enqueue(&self, message: Message) {
        self.enqueue_with_id(None, message);
    }

    pub(crate) fn enqueue_with_id(&self, id: Option<String>, message: Message) {
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back((id, message));
    }

    /// Exclude draining while the owner commits and replaces a known queue.
    /// A stale identity list never invokes the commit callback.
    pub(crate) fn replace_identified<E>(
        &self,
        expected: &[String],
        replacement: Vec<(String, Message)>,
        commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut messages = self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if messages.len() != expected.len()
            || !messages
                .iter()
                .zip(expected)
                .all(|((id, _), expected)| id.as_ref() == Some(expected))
        {
            return Ok(false);
        }
        commit()?;
        *messages = replacement
            .into_iter()
            .map(|(id, message)| (Some(id), message))
            .collect();
        Ok(true)
    }

    pub fn drain(&self) -> Vec<Message> {
        let mut messages = self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.mode() {
            QueueMode::All => messages.drain(..).map(|(_, message)| message).collect(),
            QueueMode::OneAtATime => messages
                .pop_front()
                .map(|(_, message)| message)
                .into_iter()
                .collect(),
        }
    }

    pub fn peek(&self) -> Vec<Message> {
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, message)| message.clone())
            .collect()
    }

    pub fn clear(&self) {
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub fn has_items(&self) -> bool {
        !self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::UserMessage;

    fn user(text: &str, timestamp_ms: i64) -> Message {
        Message::User(UserMessage::text(text, timestamp_ms))
    }

    #[test]
    fn one_at_a_time_is_fifo_and_reports_pending_state() {
        let queue = PendingMessageQueue::new(QueueMode::OneAtATime);
        queue.enqueue(user("one", 1));
        queue.enqueue(user("two", 2));

        assert!(queue.has_items());
        assert_eq!(queue.drain(), vec![user("one", 1)]);
        assert!(queue.has_items());
        assert_eq!(queue.drain(), vec![user("two", 2)]);
        assert!(!queue.has_items());
        assert!(queue.drain().is_empty());
    }

    #[test]
    fn all_mode_drains_the_complete_queue_in_source_order() {
        let queue = PendingMessageQueue::new(QueueMode::All);
        queue.enqueue(user("one", 1));
        queue.enqueue(user("two", 2));

        assert_eq!(queue.drain(), vec![user("one", 1), user("two", 2)]);
        assert!(!queue.has_items());
    }

    #[test]
    fn mode_changes_preserve_items_and_clear_discards_them() {
        let queue = PendingMessageQueue::new(QueueMode::OneAtATime);
        queue.enqueue(user("one", 1));
        queue.enqueue(user("two", 2));
        queue.set_mode(QueueMode::All);

        assert_eq!(queue.mode(), QueueMode::All);
        assert_eq!(queue.drain(), vec![user("one", 1), user("two", 2)]);
        queue.enqueue(user("three", 3));
        queue.clear();
        assert!(!queue.has_items());
    }

    #[test]
    fn failed_or_stale_owner_transactions_never_replace_live_input() {
        let queue = PendingMessageQueue::default();
        queue.enqueue_with_id(Some("first".into()), user("identical", 1));
        queue.enqueue_with_id(Some("second".into()), user("identical", 1));
        let ids = vec!["first".into(), "second".into()];
        assert_eq!(
            queue.replace_identified(&ids, Vec::new(), || Err("persistence failed")),
            Err("persistence failed")
        );
        assert_eq!(queue.peek().len(), 2);
        queue.drain();
        let mut committed = false;
        let result = queue.replace_identified(&ids, Vec::new(), || {
            committed = true;
            Ok::<_, ()>(())
        });
        assert_eq!(result, Ok(false));
        assert!(!committed);
        assert_eq!(queue.peek(), vec![user("identical", 1)]);
    }
}
