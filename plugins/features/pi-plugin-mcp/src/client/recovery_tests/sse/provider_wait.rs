//! Real supervised-slot GET authority while a non-OAuth provider callback is suspended.
use super::*;
use crate::{McpClientAuth, McpProviderAuth, McpProviderTokenResolver};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::Semaphore;

struct Provider {
    hold_next: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
    dropped: Semaphore,
    calls: AtomicUsize,
}

struct Waiting<'a>(&'a Provider);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.dropped.add_permits(1);
    }
}

#[async_trait]
impl McpProviderTokenResolver for Provider {
    async fn token(&self, provider: &str) -> Result<Option<String>, String> {
        assert_eq!(provider, "sse-provider");
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.hold_next.swap(false, Ordering::AcqRel) {
            let _waiting = Waiting(self);
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        Ok(Some("initial".into()))
    }
}

async fn prepare_provider(fixture: &OAuthSseFixture) -> (Runtime, Arc<Provider>) {
    let provider = Arc::new(Provider {
        hold_next: AtomicBool::new(false),
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        dropped: Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let mut config = fixture.config();
    config.auth = Some(McpProviderAuth {
        provider: "sse-provider".into(),
    });
    let auth = McpClientAuth::default().with_provider_tokens(provider.clone());
    let owner = McpSupervisor::new();
    let input =
        ConnectionInput::new(config, (&McpServerPolicy::direct()).into(), auth, [0; 32]).unwrap();
    owner
        .reconcile(owner.begin_candidate(), vec![input])
        .unwrap();
    let (source, pool) = publish(fixture, &owner).await;
    observe(fixture, |snapshot| common_started(snapshot, "session-1")).await;
    (
        Runtime {
            owner,
            _source: source,
            pool,
        },
        provider,
    )
}

async fn provider_wait_case(retire_generation: bool) {
    let fixture = OAuthSseFixture::start(SseOptions {
        post_unauthorized_once: false,
        resume_reply: ResumeReply::Success,
        ..Default::default()
    })
    .await;
    let priming = fixture.hold(GatePoint::PrimingEof("provider-wait".into()));
    let (
        Runtime {
            owner,
            _source: source,
            pool,
        },
        provider,
    ) = prepare_provider(&fixture).await;
    let mut original = call(&pool, "echo", "provider-wait");
    original.progress(1).await;
    entered(&priming).await;
    provider.hold_next.store(true, Ordering::Release);
    priming.release();
    tokio::time::timeout(WAIT, provider.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let waiting = fixture.snapshot();
    echo_posts(&waiting, "provider-wait", 1);
    assert_eq!(waiting.executions, ["provider-wait"]);
    assert!(
        gets(&waiting, "provider-wait").is_empty(),
        "provider wait precedes GET wire dispatch"
    );
    assert!(
        waiting.exchanges.is_empty(),
        "provider authentication never enters OAuth"
    );

    // Publish an equivalent view on the SAME connection before retiring just the old
    // generation. Reconnect would mask a missing generation fence by closing transport.
    let (next_source, next_pool) = publish(&fixture, &owner).await;
    assert!(Arc::ptr_eq(
        &pool.inner.servers[0].connection,
        &next_pool.inner.servers[0].connection
    ));
    if retire_generation {
        drop(source);
    } else {
        original.abort.abort();
    }
    let error = original.finish().await.unwrap_err();
    if retire_generation {
        assert!(error.to_string().contains("retired"), "{error}");
    } else {
        assert!(matches!(error, ToolError::Aborted));
    }
    // Positive completion of the suspended provider future, not an absence window.
    tokio::time::timeout(WAIT, provider.dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    provider.release.add_permits(1);
    let snapshot = probe_common(&fixture, &next_pool, "session-1", "after-provider-wait", 0).await;
    assert!(
        !next_source.changed(),
        "cancellation must not poison the shared slot"
    );
    assert_eq!(snapshot.initializations, 1);
    echo_posts(&snapshot, "provider-wait", 1);
    assert!(
        gets(&snapshot, "provider-wait").is_empty(),
        "no late GET after provider resolution"
    );
    assert_eq!(snapshot.executions, ["provider-wait"]);
    assert!(matches!(
        owner.snapshot().servers[fixture.name()],
        ConnectionReadiness::Ready(_)
    ));
    let before_close = provider.calls.load(Ordering::Acquire);
    owner.close().await.unwrap();
    assert_eq!(
        provider.calls.load(Ordering::Acquire),
        before_close,
        "DELETE uses the acquired-token cache, not another provider lookup"
    );
}

#[tokio::test]
async fn resumed_provider_wait_cancellation_drops_the_waiter_without_a_late_get() {
    provider_wait_case(false).await;
}

#[tokio::test]
async fn resumed_provider_wait_generation_retirement_keeps_the_new_view_on_the_same_slot() {
    provider_wait_case(true).await;
}
