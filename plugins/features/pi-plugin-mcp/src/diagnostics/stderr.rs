//! A draining, bounded, in-memory stderr tail. rmcp retains child process ownership.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinHandle;

use super::{Redactor, clean_text};

const TAIL_UTF16: usize = 2000;
const TAIL_BYTES: usize = TAIL_UTF16 * 4;
const DRAIN_GRACE: Duration = Duration::from_millis(100);

struct Tail {
    redactor: Redactor,
    pending: Vec<u8>,
    bytes: VecDeque<u8>,
}

impl Tail {
    fn new(redactor: Redactor) -> Self {
        Self {
            redactor,
            pending: Vec::new(),
            bytes: VecDeque::new(),
        }
    }

    fn push(&mut self, bytes: &[u8], eof: bool) {
        self.pending.extend_from_slice(bytes);
        let used = self.redactor.consume(&self.pending, eof, |output| {
            for byte in output {
                if self.bytes.len() == TAIL_BYTES {
                    self.bytes.pop_front();
                }
                self.bytes.push_back(*byte);
            }
        });
        self.pending.drain(..used);
    }

    fn text(&self) -> String {
        let bytes = self.bytes.iter().copied().collect::<Vec<_>>();
        let text = clean_text(&String::from_utf8_lossy(&bytes));
        let text = text.trim();
        // Pi counts UTF-16 units. Never produce an isolated surrogate at the boundary.
        let mut units = 0;
        let start = text
            .char_indices()
            .rev()
            .find_map(|(offset, c)| {
                units += c.len_utf16();
                (units > TAIL_UTF16).then_some(offset + c.len_utf8())
            })
            .unwrap_or(0);
        text[start..].to_string()
    }
}

struct DrainTask(JoinHandle<()>);

impl Drop for DrainTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Capture {
    tail: Arc<Mutex<Tail>>,
    task: Mutex<Option<DrainTask>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Ok(task) = self.task.get_mut()
            && let Some(task) = task.take()
        {
            drop(task);
        }
    }
}

#[derive(Clone)]
pub(crate) struct StderrCapture(Arc<Capture>);

impl Default for StderrCapture {
    fn default() -> Self {
        Self::new(Redactor::default())
    }
}

impl StderrCapture {
    pub(crate) fn new(redactor: Redactor) -> Self {
        Self(Arc::new(Capture {
            tail: Arc::new(Mutex::new(Tail::new(redactor))),
            task: Mutex::new(None),
        }))
    }

    pub(crate) fn start(&self, mut reader: impl AsyncRead + Send + Unpin + 'static) {
        let tail = self.0.tail.clone();
        if let Ok(mut state) = tail.lock() {
            state.pending.clear();
            state.bytes.clear();
        }
        let task = tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) | Err(_) => {
                        if let Ok(mut tail) = tail.lock() {
                            tail.push(&[], true);
                        }
                        break;
                    }
                    Ok(count) => {
                        if let Ok(mut tail) = tail.lock() {
                            tail.push(&buffer[..count], false);
                        }
                    }
                }
            }
        });
        if let Ok(mut slot) = self.0.task.lock() {
            *slot = Some(DrainTask(task));
        } else {
            task.abort();
        }
    }

    /// Collect bytes already in the pipe after close, but don't wait for inherited pipes forever.
    pub(crate) async fn finish(&self) {
        let task = self.0.task.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut task) = task
            && tokio::time::timeout(DRAIN_GRACE, &mut task.0)
                .await
                .is_err()
        {
            task.0.abort();
            let _ = (&mut task.0).await;
        }
    }

    pub(crate) fn message(&self, summary: &str) -> String {
        let tail = self
            .0
            .tail
            .lock()
            .map(|tail| tail.text())
            .unwrap_or_default();
        if tail.is_empty() {
            summary.to_owned()
        } else {
            format!("{summary}\n{tail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_split_across_reads_and_tail_cut_never_leaks() {
        let secret = "sensitive-value";
        for split in 0..=secret.len() {
            let mut tail = Tail::new(Redactor::from_values(vec![secret.into()]));
            tail.push(&vec![b'x'; TAIL_BYTES + 123], false);
            tail.push(&secret.as_bytes()[..split], false);
            tail.push(&secret.as_bytes()[split..], true);
            let text = tail.text();
            assert!(text.ends_with("[REDACTED]"), "split {split}: {text}");
            assert_eq!(text.encode_utf16().count(), TAIL_UTF16);
            assert!(tail.bytes.len() <= TAIL_BYTES);
        }
    }

    #[test]
    fn unicode_invalid_utf8_controls_and_trim_are_safe() {
        let mut tail = Tail::new(Redactor::default());
        let input = format!("{}\r\n reason\u{1b}\0\n", "🦀".repeat(4000));
        for bytes in input.as_bytes().chunks(3) {
            tail.push(bytes, false);
        }
        tail.push(&[], true);
        let text = tail.text();
        assert!(text.ends_with("\r\n reason"));
        assert!(!text.contains('\u{fffd}'));
        assert!(text.encode_utf16().count() <= TAIL_UTF16);
        tail.push(b"\xff", true);
        assert!(tail.text().ends_with('\u{fffd}'));
    }

    #[tokio::test]
    async fn drains_beyond_pipe_capacity_and_finishes_when_writer_stays_open() {
        use tokio::io::AsyncWriteExt;
        let capture = StderrCapture::default();
        let (mut writer, reader) = tokio::io::duplex(256);
        capture.start(reader);
        tokio::time::timeout(
            Duration::from_secs(2),
            writer.write_all(&vec![b'x'; 100_000]),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), capture.finish())
            .await
            .unwrap();
        assert!(capture.0.task.lock().unwrap().is_none());
        assert!(capture.0.tail.lock().unwrap().bytes.len() <= TAIL_BYTES);
    }

    #[tokio::test]
    async fn dropping_last_capture_closes_the_reader() {
        use tokio::io::AsyncWriteExt;
        let capture = StderrCapture::default();
        let (mut writer, reader) = tokio::io::duplex(256);
        capture.start(reader);
        drop(capture);
        let result =
            tokio::time::timeout(Duration::from_secs(1), writer.write_all(&vec![b'x'; 4096]))
                .await
                .unwrap();
        assert!(result.is_err());
    }
}
