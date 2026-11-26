//! Raw pre-parser bounds for the public SSE parser.
//!
//! The stock worker recognizes its own private overflow error as fatal. Our error
//! cannot impersonate that type: exact fatal-overflow/reconnect behavior remains OPEN.
use futures::{StreamExt, stream::BoxStream};

use super::PiHttpClientError;

pub(super) fn bounded(
    response: reqwest::Response,
    max: usize,
) -> BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>> {
    let bytes = futures::stream::unfold(
        (Some(response), EventLimit::new(max)),
        |(response, mut limit)| async move {
            let mut response = response?;
            match response.chunk().await {
                Ok(Some(chunk)) => match limit.observe(&chunk) {
                    Ok(()) => Some((Ok(chunk), (Some(response), limit))),
                    // Drop the response now, not only when the consumer polls again.
                    Err(error) => Some((Err(error), (None, limit))),
                },
                Ok(None) => None,
                Err(error) => Some((Err(PiHttpClientError::from(error)), (None, limit))),
            }
        },
    );
    sse_stream::SseStream::from_bytes_stream(bytes).boxed()
}

struct EventLimit {
    max: usize,
    event_bytes: usize,
    line_bytes: usize,
    comment: bool,
    after_cr: bool,
}

impl EventLimit {
    fn new(max: usize) -> Self {
        Self { max, event_bytes: 0, line_bytes: 0, comment: false, after_cr: false }
    }

    fn observe(&mut self, bytes: &[u8]) -> Result<(), PiHttpClientError> {
        for &byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                if self.line_bytes == 0 {
                    self.event_bytes = 0;
                } else if !self.comment {
                    self.event_bytes = self.event_bytes.saturating_add(self.line_bytes).saturating_add(1);
                }
                self.line_bytes = 0;
                self.comment = false;
                self.after_cr = byte == b'\r';
            } else {
                if self.line_bytes == 0 {
                    self.comment = byte == b':';
                }
                self.line_bytes = self.line_bytes.saturating_add(1);
            }
            if self.event_bytes.saturating_add(self.line_bytes) > self.max {
                return Err(PiHttpClientError::limit("SSE event", self.max));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_unterminated_line_and_accumulated_event_before_parsing() {
        let mut line = EventLimit::new(12);
        line.observe(b"data: 123").unwrap();
        assert!(line.observe(b"4567").is_err());
        let mut event = EventLimit::new(20);
        event.observe(b"data: a\ndata: b\n").unwrap();
        assert!(event.observe(b"data: c\n").is_err());
    }

    #[test]
    fn split_crlf_and_blank_lines_reset_each_event_not_each_chunk() {
        let mut limit = EventLimit::new(20);
        for chunk in [b"data: a\r".as_slice(), b"\n\r", b"\ndata: b\n\n", b": comment\n", b"data: c\n\n"] {
            limit.observe(chunk).unwrap();
        }
    }
}
