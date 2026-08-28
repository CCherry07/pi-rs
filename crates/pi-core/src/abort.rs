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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abort_is_idempotent_and_wakes_parent_and_child_waiters() {
        let (handle, signal) = AbortHandle::new();
        let child = signal.child();
        let parent_wait = tokio::spawn({
            let signal = signal.clone();
            async move { signal.wait().await }
        });
        let child_wait = tokio::spawn({
            let child = child.clone();
            async move { child.wait().await }
        });

        assert!(signal.check().is_ok());
        assert!(!handle.is_aborted());
        handle.abort();
        handle.abort();
        parent_wait.await.unwrap();
        child_wait.await.unwrap();

        assert!(handle.is_aborted());
        assert!(signal.is_aborted());
        assert!(child.is_aborted());
        assert_eq!(signal.check(), Err(AbortError));
    }

    #[tokio::test]
    async fn cancelling_a_child_does_not_cancel_its_parent() {
        let (_parent_handle, parent) = AbortHandle::new();
        let child = parent.child();
        let child_token = child.token.clone();
        child_token.cancel();

        child.wait().await;
        assert!(child.is_aborted());
        assert!(!parent.is_aborted());
        assert!(parent.check().is_ok());
    }
}
