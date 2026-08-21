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
