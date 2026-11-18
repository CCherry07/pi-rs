#![forbid(unsafe_code)]

//! Vendor-neutral HTTP transport and Server-Sent Events primitives.

mod constrained_sampling;
mod credentials;
mod headers;
mod sse;
mod strict_tool_limits;
mod transport;

pub use constrained_sampling::{
    GrammarInputBuffer, GrammarSampling, grammar_tool_input_properties, resolve_grammar_sampling,
    resolve_json_schema_sampling, resolve_json_schema_sampling_with,
};
pub use credentials::{CredentialKind, CredentialProvider, CredentialSource, ProviderCredential};
pub use headers::{insert_header, remove_header};
pub use sse::{SseDecoder, SseEvent};
pub use transport::{
    HttpBodyStream, HttpResponse, HttpTransport, REQUEST_TIMEOUT_ENV, ReqwestTransport,
    ReqwestTransportConfig, TransportError, collect_body_limited, post_json_with_provider_hooks,
    read_error_response,
};

pub use strict_tool_limits::{
    clear_strict_tool_limits, disable_preferred_strict_tools, has_exceeded_strict_tool_limits,
    is_strict_tool_limit_error, mark_strict_tool_limits_exceeded, prefers_json_schema_sampling,
    with_provider_diagnostic,
};
