#![forbid(unsafe_code)]

//! Vendor-neutral HTTP transport and Server-Sent Events primitives.

mod headers;
mod sse;
mod transport;

pub use headers::{insert_header, remove_header};
pub use sse::{SseDecoder, SseEvent};
pub use transport::{
    HttpBodyStream, HttpResponse, HttpTransport, REQUEST_TIMEOUT_ENV, ReqwestTransport,
    ReqwestTransportConfig, TransportError, collect_body_limited, post_json_with_provider_hooks,
    read_error_response,
};
