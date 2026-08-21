use std::borrow::Cow;

use crate::TransportError;

const DEFAULT_MAX_SSE_BUFFER_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_SSE_EVENT_DATA_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Cow<'static, str>,
    pub id: Option<String>,
    pub retry: Option<u64>,
    pub data: String,
}

impl Default for SseEvent {
    fn default() -> Self {
        Self {
            event: Cow::Borrowed("message"),
            id: None,
            retry: None,
            data: String::new(),
        }
    }
}

#[derive(Debug)]
pub struct SseDecoder {
    text: String,
    utf8_tail: Vec<u8>,
    current: SseEvent,
    has_data: bool,
    bom_checked: bool,
    max_buffer_bytes: usize,
    max_event_data_bytes: usize,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self {
            text: String::new(),
            utf8_tail: Vec::new(),
            current: SseEvent::default(),
            has_data: false,
            bom_checked: false,
            max_buffer_bytes: DEFAULT_MAX_SSE_BUFFER_BYTES,
            max_event_data_bytes: DEFAULT_MAX_SSE_EVENT_DATA_BYTES,
        }
    }
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limits(max_buffer_bytes: usize, max_event_data_bytes: usize) -> Self {
        Self {
            max_buffer_bytes,
            max_event_data_bytes,
            ..Self::default()
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, TransportError> {
        self.utf8_tail.extend_from_slice(bytes);
        let valid_len = match std::str::from_utf8(&self.utf8_tail) {
            Ok(_) => self.utf8_tail.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => {
                return Err(TransportError::InvalidSse(
                    "stream contains invalid UTF-8".to_string(),
                ));
            }
        };
        if valid_len > 0 {
            let valid = std::str::from_utf8(&self.utf8_tail[..valid_len])
                .map_err(|error| TransportError::InvalidSse(error.to_string()))?;
            self.text.push_str(valid);
            self.utf8_tail.drain(..valid_len);
        }
        if self.text.len() > self.max_buffer_bytes {
            return Err(TransportError::InvalidSse(format!(
                "buffer exceeds the {}-byte limit",
                self.max_buffer_bytes
            )));
        }
        self.process_lines(false)
    }

    pub fn finish(&mut self) -> Result<Option<SseEvent>, TransportError> {
        if !self.utf8_tail.is_empty() {
            return Err(TransportError::InvalidSse(
                "stream ended with an incomplete UTF-8 sequence".to_string(),
            ));
        }
        let mut events = self.process_lines(true)?;
        if events.len() > 1 {
            return Err(TransportError::InvalidSse(
                "decoder finish produced multiple pending events".to_string(),
            ));
        }
        Ok(events.pop())
    }

    fn process_lines(&mut self, flush: bool) -> Result<Vec<SseEvent>, TransportError> {
        let mut events = Vec::new();
        while let Some((end, consumed)) = next_line(&self.text, flush) {
            let mut line = self.text[..end].to_string();
            self.text.drain(..consumed);
            if !self.bom_checked {
                self.bom_checked = true;
                if let Some(stripped) = line.strip_prefix('\u{feff}') {
                    line = stripped.to_string();
                }
            }
            if line.is_empty() {
                if self.has_data {
                    events.push(self.take_event());
                }
            } else {
                self.process_line(&line)?;
            }
        }
        if flush && !self.text.is_empty() {
            let line = std::mem::take(&mut self.text);
            self.process_line(line.trim_end_matches('\r'))?;
        }
        if flush && self.has_data {
            events.push(self.take_event());
        }
        Ok(events)
    }

    fn process_line(&mut self, line: &str) -> Result<(), TransportError> {
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "event" => self.current.event = intern_event(value),
            "id" if !value.contains('\0') => self.current.id = Some(value.to_string()),
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                self.current.retry = value.parse().ok();
            }
            "data" => {
                let added = value.len() + usize::from(self.has_data);
                if self.current.data.len().saturating_add(added) > self.max_event_data_bytes {
                    return Err(TransportError::InvalidSse(format!(
                        "event data exceeds the {}-byte limit",
                        self.max_event_data_bytes
                    )));
                }
                if self.has_data {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn take_event(&mut self) -> SseEvent {
        let id = self.current.id.clone();
        let retry = self.current.retry;
        let event = std::mem::take(&mut self.current);
        self.current.id = id;
        self.current.retry = retry;
        self.has_data = false;
        event
    }
}

fn next_line(input: &str, flush: bool) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\n' => return Some((index, index + 1)),
            b'\r' if index + 1 < bytes.len() => {
                return Some((index, index + 1 + usize::from(bytes[index + 1] == b'\n')));
            }
            b'\r' if flush => return Some((index, index + 1)),
            b'\r' => return None,
            _ => {}
        }
    }
    None
}

fn intern_event(value: &str) -> Cow<'static, str> {
    match value {
        "" | "message" => Cow::Borrowed("message"),
        "error" => Cow::Borrowed("error"),
        "ping" => Cow::Borrowed("ping"),
        "message_start" => Cow::Borrowed("message_start"),
        "message_stop" => Cow::Borrowed("message_stop"),
        "message_delta" => Cow::Borrowed("message_delta"),
        "content_block_start" => Cow::Borrowed("content_block_start"),
        "content_block_delta" => Cow::Borrowed("content_block_delta"),
        "content_block_stop" => Cow::Borrowed("content_block_stop"),
        value => Cow::Owned(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_fragmented_multiline_sse() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"event: message\r").unwrap().is_empty());
        assert!(decoder.push(b"\ndata: one\r\n").unwrap().is_empty());
        let events = decoder.push(b"data: two\r\nid: 7\r\n\r\n").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "message");
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].data, "one\ntwo");
    }

    #[test]
    fn preserves_utf8_split_across_chunks() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"data: \xe2").unwrap().is_empty());
        let events = decoder.push(b"\x98\x83\n\n").unwrap();
        assert_eq!(events[0].data, "☃");
    }

    #[test]
    fn carries_id_and_retry_forward() {
        let mut decoder = SseDecoder::new();
        let events = decoder
            .push(b"id: 9\nretry: 1000\ndata: a\n\ndata: b\n\n")
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].id.as_deref(), Some("9"));
        assert_eq!(events[1].retry, Some(1000));
    }

    #[test]
    fn rejects_event_over_limit() {
        let mut decoder = SseDecoder::with_limits(100, 3);
        let error = decoder.push(b"data: four\n\n").unwrap_err();
        assert!(matches!(error, TransportError::InvalidSse(_)));
    }
}
