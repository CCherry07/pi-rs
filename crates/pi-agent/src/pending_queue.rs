use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};

use pi_core::Message;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueueMode {
    All,
    #[default]
    OneAtATime,
}

#[derive(Debug, Default)]
pub struct PendingMessageQueue {
    mode: RwLock<QueueMode>,
    messages: Mutex<VecDeque<Message>>,
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
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(message);
    }

    pub fn drain(&self) -> Vec<Message> {
        let mut messages = self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.mode() {
            QueueMode::All => messages.drain(..).collect(),
            QueueMode::OneAtATime => messages.pop_front().into_iter().collect(),
        }
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
}
