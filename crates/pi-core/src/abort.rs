use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct AbortHandle {
    token: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct AbortSignal {
    token: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation aborted")]
pub struct AbortError;

impl AbortHandle {
    pub fn new() -> (Self, AbortSignal) {
        let token = CancellationToken::new();
        (
            Self {
                token: token.clone(),
            },
            AbortSignal { token },
        )
    }

    pub fn abort(&self) {
        self.token.cancel();
    }

    pub fn is_aborted(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl AbortSignal {
    pub fn is_aborted(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn check(&self) -> Result<(), AbortError> {
        if self.is_aborted() {
            Err(AbortError)
        } else {
            Ok(())
        }
    }

    pub async fn wait(&self) {
        self.token.cancelled().await;
    }

    pub fn child(&self) -> Self {
        Self {
            token: self.token.child_token(),
        }
    }
}
