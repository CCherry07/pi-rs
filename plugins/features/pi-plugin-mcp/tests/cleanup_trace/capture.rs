//! Test-local observation only: no global subscriber, filtering or production changes.
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

#[derive(Clone, Debug)]
pub struct Entry {
    pub cleanup: bool,
    pub kind: &'static str,
    metadata: Option<&'static Metadata<'static>>,
    fields: Vec<(String, String)>,
}

impl Entry {
    pub fn is_sdk_worker_exit(&self) -> bool {
        self.kind == "event"
            && self.metadata.is_some_and(|metadata| {
                metadata.target() == "rmcp::transport::worker"
                    && metadata
                        .file()
                        .is_some_and(|file| file.ends_with("transport/worker.rs"))
            })
            && self
                .fields
                .iter()
                .any(|(field, value)| field == "message" && value == "worker quit")
    }

    pub fn render(&self) -> String {
        let origin = self.metadata.map_or_else(
            || "span update (source recorded on span creation)".to_owned(),
            |metadata| {
                format!(
                    "{} {} {} {}:{}",
                    metadata.level(),
                    metadata.target(),
                    metadata.name(),
                    metadata.file().unwrap_or("unknown"),
                    metadata.line().unwrap_or(0)
                )
            },
        );
        format!(
            "cleanup={} {} {origin} {:?}",
            self.cleanup, self.kind, self.fields
        )
    }
}

#[derive(Default)]
struct State {
    cleanup: AtomicBool,
    next_span: AtomicU64,
    entries: Mutex<Vec<Entry>>,
}

#[derive(Clone, Default)]
pub struct Capture(Arc<State>);

impl Capture {
    pub fn begin_cleanup(&self) {
        self.0.cleanup.store(true, Ordering::SeqCst);
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.0.entries.lock().unwrap().clone()
    }

    fn push(
        &self,
        kind: &'static str,
        metadata: Option<&'static Metadata<'static>>,
        fields: Fields,
    ) {
        self.0.entries.lock().unwrap().push(Entry {
            cleanup: self.0.cleanup.load(Ordering::SeqCst),
            kind,
            metadata,
            fields: fields.0,
        });
    }
}

#[derive(Default)]
struct Fields(Vec<(String, String)>);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().to_owned(), value.to_owned()));
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::always()
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        let id = Id::from_u64(self.0.next_span.fetch_add(1, Ordering::SeqCst) + 1);
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        fields
            .0
            .push(("captured_span_id".into(), id.into_u64().to_string()));
        self.push("span", Some(attributes.metadata()), fields);
        id
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        let mut fields = Fields::default();
        values.record(&mut fields);
        fields
            .0
            .push(("captured_span_id".into(), span.into_u64().to_string()));
        self.push("span_update", None, fields);
    }

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.push("event", Some(event.metadata()), fields);
    }

    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}
