use std::{
    alloc::{GlobalAlloc, Layout, System},
    process::Command,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use pi_core::{AbortHandle, ContentBlock, ToolCallId};
use pi_plugin::{Tool, ToolContext, ToolUpdateSink};
use pi_plugin_read::ReadTool;
use serde_json::json;

struct CappedAllocator;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static BYTE_LIMIT: AtomicUsize = AtomicUsize::new(usize::MAX);

fn reserve(bytes: usize) -> bool {
    let mut current = LIVE_BYTES.load(Ordering::Relaxed);
    loop {
        if bytes > BYTE_LIMIT.load(Ordering::Relaxed).saturating_sub(current) {
            return false;
        }
        match LIVE_BYTES.compare_exchange_weak(
            current,
            current + bytes,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

fn release(bytes: usize) {
    LIVE_BYTES.fetch_sub(bytes, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CappedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !reserve(layout.size()) {
            return ptr::null_mut();
        }
        // SAFETY: `layout` comes from the allocator contract and is forwarded unchanged.
        let allocation = unsafe { System.alloc(layout) };
        if allocation.is_null() {
            release(layout.size());
        }
        allocation
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !reserve(layout.size()) {
            return ptr::null_mut();
        }
        // SAFETY: `layout` comes from the allocator contract and is forwarded unchanged.
        let allocation = unsafe { System.alloc_zeroed(layout) };
        if allocation.is_null() {
            release(layout.size());
        }
        allocation
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` and `layout` are the pair previously returned by this allocator.
        unsafe { System.dealloc(pointer, layout) };
        release(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        if new_size > old_size && !reserve(new_size - old_size) {
            return ptr::null_mut();
        }
        // SAFETY: the pointer/layout pair and new size satisfy `GlobalAlloc::realloc`'s contract.
        let allocation = unsafe { System.realloc(pointer, layout, new_size) };
        if allocation.is_null() {
            if new_size > old_size {
                release(new_size - old_size);
            }
        } else if old_size > new_size {
            release(old_size - new_size);
        }
        allocation
    }
}

#[global_allocator]
static ALLOCATOR: CappedAllocator = CappedAllocator;

#[test]
fn reads_sparse_200mb_single_line_in_a_memory_capped_child() {
    const CHILD: &str = "PI_READ_200MB_RESOURCE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "reads_sparse_200mb_single_line_in_a_memory_capped_child",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "memory-capped child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("big.log");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(200 * 1024 * 1024)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // The old whole-file implementation requests a 200 MiB Vec and deterministically aborts this
    // child. The streaming implementation needs only its 64 KiB decode chunk plus bounded lines.
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    BYTE_LIMIT.store(baseline + 32 * 1024 * 1024, Ordering::Relaxed);
    let result = runtime.block_on(async {
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        ReadTool
            .execute(
                ToolContext::standalone(directory.path().to_path_buf(), signal),
                ToolCallId::new("resource"),
                json!({"path":"big.log"}),
                updates,
            )
            .await
    });
    BYTE_LIMIT.store(usize::MAX, Ordering::Relaxed);

    let result = result.unwrap();
    assert!(matches!(
        &result.content[0],
        ContentBlock::Text(text)
            if text.text
                == "[Line 1 is 200.0MB, exceeds 50.0KB limit. Use bash: sed -n '1p' big.log | head -c 51200]"
    ));
    let truncation = result.details.unwrap()["truncation"].clone();
    assert_eq!(truncation["totalBytes"], 200 * 1024 * 1024usize);
    assert_eq!(truncation["outputBytes"], 0);
    assert_eq!(truncation["firstLineExceedsLimit"], true);
}
