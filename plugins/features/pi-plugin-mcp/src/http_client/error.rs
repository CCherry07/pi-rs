//! Private HTTP evidence. Credentials and server-controlled text are never diagnostic sources.
use std::fmt;

use http::StatusCode;

use super::RequestOrigin;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NetworkErrorFlags {
    pub(crate) connect: bool,
    pub(crate) timeout: bool,
    pub(crate) request: bool,
    pub(crate) body: bool,
}

#[derive(Clone)]
pub(crate) struct AuthRejection {
    status: StatusCode,
    challenge: Option<String>,
    insufficient_scope: bool,
    requires_sign_in: bool,
    provider_attached: bool,
}

impl AuthRejection {
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    pub(crate) fn challenge(&self) -> Option<&str> {
        self.challenge.as_deref()
    }

    pub(crate) fn is_insufficient_scope(&self) -> bool {
        self.insufficient_scope
    }

    pub(crate) fn requires_sign_in(&self) -> bool {
        self.requires_sign_in
    }

    /// The actual request wrapper, not token presence or configuration, supplies this fact.
    pub(crate) fn has_auth_provider(&self) -> bool {
        self.provider_attached
    }
}

impl fmt::Debug for AuthRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthRejection")
            .field("status", &self.status.as_u16())
            .field("insufficient_scope", &self.insufficient_scope)
            .field("requires_sign_in", &self.requires_sign_in)
            .field("provider_attached", &self.provider_attached)
            .finish_non_exhaustive()
    }
}

pub(crate) struct PiHttpClientError {
    kind: FailureKind,
}

enum FailureKind {
    Request(reqwest::Error),
    Http { status: StatusCode, origin: RequestOrigin },
    Auth(AuthRejection),
    MissingReply,
    Protocol(&'static str),
    BodyLimit { kind: &'static str, limit: usize },
}

impl PiHttpClientError {
    pub(crate) fn request(error: reqwest::Error) -> Self {
        Self { kind: FailureKind::Request(error) }
    }

    pub(crate) fn status(&self) -> Option<StatusCode> {
        match &self.kind {
            FailureKind::Request(error) => error.status(),
            FailureKind::Http { status, .. } => Some(*status),
            FailureKind::Auth(rejection) => Some(rejection.status()),
            FailureKind::MissingReply | FailureKind::Protocol(_) | FailureKind::BodyLimit { .. } => None,
        }
    }

    pub(crate) fn network_flags(&self) -> Option<super::NetworkErrorFlags> {
        let FailureKind::Request(error) = &self.kind else {
            return None;
        };
        Some(NetworkErrorFlags {
            connect: error.is_connect(),
            timeout: error.is_timeout(),
            request: error.is_request(),
            body: error.is_body(),
        })
    }

    /// Evidence for the separately bounded setup fallback, never a general retry signal.
    pub(crate) fn discover_rejection(&self) -> Option<StatusCode> {
        match self.kind {
            FailureKind::Http { status, origin: RequestOrigin::DiscoverUnattached }
                if matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND) => Some(status),
            _ => None,
        }
    }

    pub(crate) fn auth_rejection(&self) -> Option<&super::AuthRejection> {
        match &self.kind {
            FailureKind::Auth(rejection) => Some(rejection),
            _ => None,
        }
    }

    pub(crate) fn with_auth_provider(mut self) -> Self {
        if let FailureKind::Auth(rejection) = &mut self.kind {
            rejection.provider_attached = true;
        }
        self
    }

    /// Only the OAuth coordinator calls this after observing its own rejected request.
    pub(crate) fn require_sign_in(mut self) -> Self {
        if let FailureKind::Auth(rejection) = &mut self.kind {
            rejection.requires_sign_in = true;
        }
        self
    }

    pub(super) fn response(status: StatusCode, origin: RequestOrigin) -> Self {
        Self { kind: FailureKind::Http { status, origin } }
    }

    pub(super) fn auth(status: StatusCode, challenge: Option<String>, insufficient_scope: bool) -> Self {
        Self {
            kind: FailureKind::Auth(AuthRejection {
                status,
                challenge,
                insufficient_scope,
                requires_sign_in: false,
                provider_attached: false,
            }),
        }
    }

    pub(super) fn missing_reply() -> Self {
        Self { kind: FailureKind::MissingReply }
    }

    #[cfg(test)]
    pub(super) fn is_missing_reply(&self) -> bool {
        matches!(self.kind, FailureKind::MissingReply)
    }

    pub(super) fn protocol(message: &'static str) -> Self {
        Self { kind: FailureKind::Protocol(message) }
    }

    pub(super) fn limit(kind: &'static str, limit: usize) -> Self {
        Self { kind: FailureKind::BodyLimit { kind, limit } }
    }
}

impl From<reqwest::Error> for PiHttpClientError {
    fn from(error: reqwest::Error) -> Self {
        Self::request(error)
    }
}

impl fmt::Display for PiHttpClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            FailureKind::Request(_) => f.write_str("MCP HTTP request failed"),
            FailureKind::Http { status, .. } => write!(f, "MCP HTTP response status {}", status.as_u16()),
            FailureKind::Auth(rejection) => write!(f, "MCP authentication rejected (HTTP {})", rejection.status.as_u16()),
            FailureKind::MissingReply => f.write_str("MCP request accepted without a response"),
            FailureKind::Protocol(message) => f.write_str(message),
            FailureKind::BodyLimit { kind, limit } => write!(f, "MCP {kind} exceeded {limit} bytes"),
        }
    }
}

impl fmt::Debug for PiHttpClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for PiHttpClientError {}
