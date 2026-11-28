//! Incremental Pi-compatible SSE parsing with typed payload bounds.
//!
//! The sole private HTTP worker treats the typed bound error as terminal without
//! impersonating the stock worker's private marker. Metadata callbacks, EOF dispatch,
//! lossy UTF-8 decoding, and decoded `data:` accounting follow the frozen Pi parser.
mod utf8;

use futures::{Stream, StreamExt, TryStreamExt, stream::BoxStream};
use utf8::LossyUtf8;

use super::PiHttpClientError;

pub(super) fn bounded(
    response: reqwest::Response,
    max: usize,
) -> BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>> {
    bounded_chunks(
        response.bytes_stream().map_err(PiHttpClientError::from),
        max,
    )
}

fn bounded_chunks<S, B>(
    source: S,
    max: usize,
) -> BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>
where
    S: Stream<Item = Result<B, PiHttpClientError>> + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    futures::stream::unfold(
        (Some(source.boxed()), Parser::new(max)),
        |(mut source, mut parser)| async move {
            loop {
                match parser.next_item() {
                    Ok(Some(event)) => return Some((Ok(event), (source, parser))),
                    Ok(None) if parser.finished() => return None,
                    Ok(None) => {}
                    Err(error) => {
                        // Drop the response and unread suffix before exposing the bound error.
                        drop(source.take());
                        parser.fail();
                        return Some((
                            Err(sse_stream::Error::Body(Box::new(error))),
                            (None, parser),
                        ));
                    }
                }

                let mut active = source.take()?;
                match active.next().await {
                    Some(Ok(chunk)) => {
                        source = Some(active);
                        parser.push(chunk.as_ref(), false);
                    }
                    Some(Err(error)) => {
                        return Some((
                            Err(sse_stream::Error::Body(Box::new(error))),
                            (None, parser),
                        ));
                    }
                    None => {
                        parser.push(&[], true);
                        drop(active);
                    }
                }
            }
        },
    )
    .boxed()
}

/// Incremental Pi-compatible SSE parsing. Metadata changes are emitted as
/// metadata-only items when their line ends so reconnect state updates even if
/// the server keeps the event open. Only data-bearing items represent events.
struct Parser {
    max: usize,
    decoder: LossyUtf8,
    text: String,
    offset: usize,
    pending_line: String,
    pending_line_bytes: usize,
    skip_leading_lf: bool,
    at_start: bool,
    eof: bool,
    eof_line_processed: bool,
    eof_dispatched: bool,
    event_name: Option<String>,
    event_id: Option<String>,
    data_lines: Vec<String>,
    data_bytes: usize,
}

impl Parser {
    fn new(max: usize) -> Self {
        Self {
            max,
            decoder: LossyUtf8::new(),
            text: String::new(),
            offset: 0,
            pending_line: String::new(),
            pending_line_bytes: 0,
            skip_leading_lf: false,
            at_start: true,
            eof: false,
            eof_line_processed: false,
            eof_dispatched: false,
            event_name: None,
            event_id: None,
            data_lines: Vec::new(),
            data_bytes: 0,
        }
    }

    fn push(&mut self, bytes: &[u8], eof: bool) {
        let decoded = self.decoder.decode(bytes, eof);
        let mut text = std::str::from_utf8(&decoded).expect("lossy decoder always returns UTF-8");
        if self.at_start && !text.is_empty() {
            self.at_start = false;
            text = text.strip_prefix('\u{feff}').unwrap_or(text);
        }
        if self.skip_leading_lf && !text.is_empty() {
            self.skip_leading_lf = false;
            text = text.strip_prefix('\n').unwrap_or(text);
        }
        if self.offset == self.text.len() {
            self.text.clear();
            self.offset = 0;
        }
        self.text.push_str(text);
        self.eof = eof;
    }

    fn finished(&self) -> bool {
        self.eof && self.eof_dispatched
    }

    fn fail(&mut self) {
        self.text.clear();
        self.offset = 0;
        self.pending_line.clear();
        self.data_lines.clear();
        self.eof = true;
        self.eof_line_processed = true;
        self.eof_dispatched = true;
    }

    fn next_item(&mut self) -> Result<Option<sse_stream::Sse>, PiHttpClientError> {
        loop {
            let remaining = &self.text[self.offset..];
            if let Some(relative) = remaining
                .as_bytes()
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
            {
                let end = self.offset + relative;
                self.pending_line.push_str(&self.text[self.offset..end]);
                let terminator = self.text.as_bytes()[end];
                let next = end + 1;
                if terminator == b'\r' {
                    if self.text.as_bytes().get(next) == Some(&b'\n') {
                        self.offset = next + 1;
                    } else {
                        self.offset = next;
                        self.skip_leading_lf = next == self.text.len();
                    }
                } else {
                    self.offset = next;
                }
                self.pending_line_bytes = 0;
                let line = std::mem::take(&mut self.pending_line);
                if let Some(event) = self.process_line(line)? {
                    return Ok(Some(event));
                }
                continue;
            }

            if !remaining.is_empty() {
                self.pending_line.push_str(remaining);
                self.pending_line_bytes = self.pending_line_bytes.saturating_add(remaining.len());
                self.offset = self.text.len();
                if self.pending_line_bytes > self.max {
                    return Err(PiHttpClientError::limit("SSE event", self.max));
                }
            }
            if !self.eof {
                return Ok(None);
            }
            if !self.eof_line_processed {
                self.eof_line_processed = true;
                if !self.pending_line.is_empty() {
                    self.pending_line_bytes = 0;
                    let line = std::mem::take(&mut self.pending_line);
                    if let Some(event) = self.process_line(line)? {
                        return Ok(Some(event));
                    }
                }
            }
            if !self.eof_dispatched {
                self.eof_dispatched = true;
                if let Some(event) = self.dispatch() {
                    return Ok(Some(event));
                }
            }
            return Ok(None);
        }
    }

    fn process_line(&mut self, line: String) -> Result<Option<sse_stream::Sse>, PiHttpClientError> {
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, mut value) = line
            .split_once(':')
            .map_or((line.as_str(), ""), |(field, value)| (field, value));
        if let Some(stripped) = value.strip_prefix(' ') {
            value = stripped;
        }
        match field {
            "data" => {
                self.data_bytes = self
                    .data_bytes
                    .saturating_add(value.len())
                    .saturating_add(usize::from(!self.data_lines.is_empty()));
                if self.data_bytes > self.max {
                    return Err(PiHttpClientError::limit("SSE event", self.max));
                }
                self.data_lines.push(value.to_owned());
                Ok(None)
            }
            "event" => {
                self.event_name = Some(value.to_owned());
                Ok(None)
            }
            "id" if !value.contains('\0') => {
                self.event_id = Some(value.to_owned());
                Ok(Some(sse_stream::Sse {
                    id: Some(value.to_owned()),
                    ..Default::default()
                }))
            }
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                let Ok(retry) = value.parse::<u64>() else {
                    return Ok(None);
                };
                Ok(Some(sse_stream::Sse {
                    retry: Some(retry),
                    ..Default::default()
                }))
            }
            _ => Ok(None),
        }
    }

    fn dispatch(&mut self) -> Option<sse_stream::Sse> {
        if self.data_lines.is_empty() {
            self.event_name = None;
            self.event_id = None;
            self.data_bytes = 0;
            return None;
        }
        let data = std::mem::take(&mut self.data_lines).join("\n");
        self.data_bytes = 0;
        Some(sse_stream::Sse {
            event: self.event_name.take().filter(|value| !value.is_empty()),
            data: Some(data),
            id: self.event_id.take().filter(|value| !value.is_empty()),
            retry: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn complete_events_precede_overflow_independent_of_chunk_boundaries() {
        let body = b"data: a\n\ndata: b\n\ndata: 0123456789abcdefghijklmnopqrstuvwxyz\n\n";
        for chunk_size in [1, 2, 7, body.len()] {
            let source = futures::stream::iter(
                body.chunks(chunk_size)
                    .map(|chunk| Ok(chunk.to_vec()))
                    .collect::<Vec<_>>(),
            );
            let mut stream = bounded_chunks(source, 16);
            for expected in ["a", "b"] {
                let event = stream
                    .next()
                    .await
                    .expect("complete prefix event")
                    .unwrap_or_else(|error| panic!("chunk size {chunk_size}: {error}"));
                assert_eq!(event.data.as_deref(), Some(expected));
            }
            let Some(Err(sse_stream::Error::Body(error))) = stream.next().await else {
                panic!("chunk size {chunk_size}: expected the oversized suffix error");
            };
            assert!(error.downcast_ref::<PiHttpClientError>().is_some());
            assert!(stream.next().await.is_none());
        }
    }

    #[tokio::test]
    async fn split_crlf_and_empty_chunks_preserve_complete_events() {
        let chunks: &[&[u8]] = &[
            b"data: a\r",
            b"",
            b"\n\r",
            b"",
            b"\ndata: b\r\n\r",
            b"",
            b"\n",
        ];
        let source = futures::stream::iter(
            chunks
                .iter()
                .map(|chunk| Ok(chunk.to_vec()))
                .collect::<Vec<_>>(),
        );
        let events = bounded_chunks(source, 16).collect::<Vec<_>>().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap().data.as_deref(), Some("a"));
        assert_eq!(events[1].as_ref().unwrap().data.as_deref(), Some("b"));
    }

    #[tokio::test]
    async fn overflow_drops_source_before_delivering_error() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let source = futures::stream::iter([Ok(b"data: a\n\ndata: oversized-pending-line")])
            .chain(futures::stream::pending())
            .inspect(move |_| {
                let _ = &guard;
            });
        let mut stream = bounded_chunks(source, 16);
        assert_eq!(
            stream.next().await.unwrap().unwrap().data.as_deref(),
            Some("a")
        );
        assert!(!dropped.load(Ordering::SeqCst));
        assert!(stream.next().await.unwrap().is_err());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn malformed_utf8_is_replaced_without_losing_following_events() {
        // Frozen consumeSseStream uses nonfatal streaming TextDecoder, including metadata.
        let body = [
            b"\xef\xbb\xbfdata: prefix\n\nevent: message\nid: cursor-\xff\ndata: bad:\xff|\xc3(|\xe2\x82|good:".as_slice(),
            "é日本🦀".as_bytes(),
            b"\n\ndata: suffix\n\n",
        ].concat();
        for chunk_size in [1, 2, 3, 7, 31, body.len()] {
            let source = futures::stream::iter(
                body.chunks(chunk_size)
                    .map(|chunk| Ok(chunk.to_vec()))
                    .collect::<Vec<_>>(),
            );
            let events = bounded_chunks(source, 4096)
                .try_collect::<Vec<_>>()
                .await
                .unwrap_or_else(|error| panic!("chunk size {chunk_size}: {error}"));
            assert!(
                events
                    .iter()
                    .any(|event| event.id.as_deref() == Some("cursor-�"))
            );
            let data = events
                .iter()
                .filter_map(|event| event.data.as_deref())
                .collect::<Vec<_>>();
            assert_eq!(data, ["prefix", "bad:�|�(|�|good:é日本🦀", "suffix"]);
            let event = events
                .iter()
                .find(|event| event.data.as_deref() == Some("bad:�|�(|�|good:é日本🦀"))
                .expect("decoded data event");
            assert_eq!(event.id.as_deref(), Some("cursor-�"));
            assert_eq!(event.event.as_deref(), Some("message"));
        }
    }

    #[tokio::test]
    async fn malformed_utf8_expansion_keeps_the_configured_bound_and_drops_source() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        // Raw bytes fit, but four U+FFFD replacements must not bypass the configured cap.
        let source = futures::stream::iter([Ok(
            b"data: a\n\ndata: \xff\xff\xff\xff\n\ndata: forbidden\n\n",
        )])
        .chain(futures::stream::pending())
        .inspect(move |_| {
            let _ = &guard;
        });
        let mut stream = bounded_chunks(source, 11);
        assert_eq!(
            stream.next().await.unwrap().unwrap().data.as_deref(),
            Some("a")
        );
        assert!(!dropped.load(Ordering::SeqCst));
        let Some(Err(sse_stream::Error::Body(error))) = stream.next().await else {
            panic!("replacement expansion must retain the typed bound error");
        };
        assert!(error.downcast_ref::<PiHttpClientError>().is_some());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn lenient_fields_and_duplicate_metadata_match_frozen_parser() {
        let body = b"event: first\nevent: second\nid: one\nid: two\nretry: nope\nretry: 12\nretry: 7\nunknown: ignored\nbare\ndata\ndata: value\n\n";
        let source = futures::stream::iter([Ok(body.as_slice())]);
        let events = bounded_chunks(source, 64)
            .try_collect::<Vec<_>>()
            .await
            .expect("legacy accepts duplicate, unknown, and bare fields");

        let ids = events
            .iter()
            .filter_map(|event| event.id.as_deref())
            .collect::<Vec<_>>();
        let retries = events
            .iter()
            .filter_map(|event| event.retry)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["one", "two", "two"]);
        assert_eq!(retries, [12, 7]);
        let event = events.last().expect("data event");
        assert_eq!(event.event.as_deref(), Some("second"));
        assert_eq!(event.id.as_deref(), Some("two"));
        assert_eq!(event.data.as_deref(), Some("\nvalue"));
    }

    #[tokio::test]
    async fn eof_dispatches_an_unterminated_final_event() {
        let source =
            futures::stream::iter([Ok(b"event: final\nid: cursor\ndata: tail".as_slice())]);
        let events = bounded_chunks(source, 16)
            .try_collect::<Vec<_>>()
            .await
            .expect("unterminated EOF event");
        let event = events.last().expect("EOF dispatch");
        assert_eq!(event.event.as_deref(), Some("final"));
        assert_eq!(event.id.as_deref(), Some("cursor"));
        assert_eq!(event.data.as_deref(), Some("tail"));
    }

    #[tokio::test]
    async fn limit_counts_decoded_data_and_joining_newlines_not_field_syntax() {
        let body = b"id: metadata-may-be-longer-than-the-payload-limit\ndata: ab\ndata: cd\n\n";
        let source = futures::stream::iter([Ok(body.as_slice())]);
        let events = bounded_chunks(source, 5)
            .try_collect::<Vec<_>>()
            .await
            .expect("two data lines occupy exactly five decoded bytes");
        assert_eq!(events.last().unwrap().data.as_deref(), Some("ab\ncd"));

        let source = futures::stream::iter([Ok(b"data: ab\ndata: cde\n\n".as_slice())]);
        assert!(
            bounded_chunks(source, 5)
                .collect::<Vec<_>>()
                .await
                .last()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn id_and_retry_updates_are_visible_when_the_stream_stays_open() {
        let source = futures::stream::iter([Ok(b"id: cursor\nretry: 7\n".as_slice())])
            .chain(futures::stream::pending());
        let mut stream = bounded_chunks(source, 64);
        let id = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next())
            .await
            .expect("id is applied when its line ends")
            .expect("id update")
            .expect("valid id");
        assert_eq!(id.id.as_deref(), Some("cursor"));
        let retry = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next())
            .await
            .expect("retry is applied when its line ends")
            .expect("retry update")
            .expect("valid retry");
        assert_eq!(retry.retry, Some(7));
    }

    #[tokio::test]
    async fn bounds_an_unterminated_line_across_input_chunks() {
        let source = futures::stream::iter([Ok(b"data: 123".as_slice()), Ok(b"4567".as_slice())]);
        let Some(Err(sse_stream::Error::Body(error))) = bounded_chunks(source, 12).next().await
        else {
            panic!("unterminated line must be bounded before EOF");
        };
        assert!(error.downcast_ref::<PiHttpClientError>().is_some());
    }
}
