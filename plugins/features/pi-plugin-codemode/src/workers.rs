//! Process-wide worker admission. A reservation stays live until the native VM
//! actually exits; cancellation of the async caller cannot free a still-running VM.
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::oneshot;

pub(crate) static WORKERS: LazyLock<Arc<Workers>> =
    LazyLock::new(|| Arc::new(Workers::new(max_workers)));

pub(crate) fn max_workers() -> usize {
    std::env::var("PI_CODEMODE_MAX_WORKERS")
        .ok()
        .and_then(|value| {
            let value = value.trim();
            let number = if let Some(hex) = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
            {
                u64::from_str_radix(hex, 16).ok().map(|v| v as f64)
            } else if let Some(binary) = value
                .strip_prefix("0b")
                .or_else(|| value.strip_prefix("0B"))
            {
                u64::from_str_radix(binary, 2).ok().map(|v| v as f64)
            } else if let Some(octal) = value
                .strip_prefix("0o")
                .or_else(|| value.strip_prefix("0O"))
            {
                u64::from_str_radix(octal, 8).ok().map(|v| v as f64)
            } else {
                value.parse::<f64>().ok()
            }?;
            (number.is_finite() && number > 0.0 && number.fract() == 0.0).then_some(number as usize)
        })
        .unwrap_or(2)
}

#[derive(Default)]
struct State {
    next: u64,
    active: HashSet<u64>,
    waiting: VecDeque<(u64, oneshot::Sender<()>)>,
}

pub(crate) struct Workers {
    state: Mutex<State>,
    limit: Box<dyn Fn() -> usize + Send + Sync>,
}

pub(crate) struct Reservation {
    workers: Arc<Workers>,
    id: u64,
}

impl Workers {
    pub(crate) fn new(limit: impl Fn() -> usize + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new(State::default()),
            limit: Box::new(limit),
        }
    }

    pub async fn reserve(self: &Arc<Self>) -> Reservation {
        let (send, receive) = oneshot::channel();
        let reservation = {
            let mut state = self.state.lock().unwrap();
            state.next += 1;
            let id = state.next;
            let reservation = Reservation {
                workers: self.clone(),
                id,
            };
            state.waiting.push_back((id, send));
            self.admit(&mut state);
            reservation
        };
        // Holding the reservation while waiting also makes queue cancellation
        // safe if it races the grant notification.
        let _ = receive.await;
        reservation
    }

    fn admit(&self, state: &mut State) {
        while state.active.len() < (self.limit)() {
            let Some((id, sender)) = state.waiting.pop_front() else {
                break;
            };
            state.active.insert(id);
            let _ = sender.send(());
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.workers.state.lock().unwrap();
        state.waiting.retain(|(id, _)| *id != self.id);
        state.active.remove(&self.id);
        self.workers.admit(&mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn dynamic_limit_fifo_and_cancelled_waiters_do_not_leak() {
        let limit = Arc::new(AtomicUsize::new(1));
        let read = limit.clone();
        let workers = Arc::new(Workers::new(move || read.load(Ordering::SeqCst)));
        let first = workers.reserve().await;
        let mut second = Box::pin(workers.reserve());
        assert!(second.as_mut().now_or_never().is_none());
        let mut cancelled = Box::pin(workers.reserve());
        assert!(cancelled.as_mut().now_or_never().is_none());
        drop(cancelled);
        limit.store(2, Ordering::SeqCst);
        let mut third = Box::pin(workers.reserve());
        assert!(third.as_mut().now_or_never().is_none());
        let second = second.await;
        drop(first);
        let third = third.await;
        drop((second, third));
        let state = workers.state.lock().unwrap();
        assert!(state.active.is_empty());
        assert!(state.waiting.is_empty());
    }
}
