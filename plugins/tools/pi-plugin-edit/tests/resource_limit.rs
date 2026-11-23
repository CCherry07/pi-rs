use std::{
    alloc::{GlobalAlloc, Layout, System},
    io::{BufWriter, Write as _},
    process::Command,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use pi_core::{AbortHandle, ContentBlock, ToolCallId};
use pi_plugin::{Tool, ToolContext, ToolUpdateSink};
use pi_plugin_edit::EditTool;
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
fn edits_twenty_megabyte_file_under_ninety_six_mib_live_allocation_budget() {
    const CHILD: &str = "PI_EDIT_20MB_RESOURCE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "edits_twenty_megabyte_file_under_ninety_six_mib_live_allocation_budget",
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
    let path = directory.path().join("big.txt");
    let file = std::fs::File::create(&path).unwrap();
    let mut writer = BufWriter::new(file);
    let filler = format!("{}\n", "x".repeat(99));
    for _ in 0..100_000 {
        writer.write_all(filler.as_bytes()).unwrap();
    }
    writer.write_all(b"TARGET = 1\n").unwrap();
    for _ in 0..100_000 {
        writer.write_all(filler.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let input = json!({
        "path":"big.txt",
        "edits":[{"oldText":"TARGET = 1", "newText":"TARGET = 2"}]
    });

    // This is the legacy oracle's 96 MiB heap budget. The child-local allocator makes allocation
    // growth deterministic while leaving production code and the parent test process untouched.
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    BYTE_LIMIT.store(baseline + 96 * 1024 * 1024, Ordering::Relaxed);
    let result = runtime.block_on(async {
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        EditTool
            .execute(
                ToolContext::standalone(directory.path().to_path_buf(), signal),
                ToolCallId::new("resource"),
                input,
                updates,
            )
            .await
    });
    BYTE_LIMIT.store(usize::MAX, Ordering::Relaxed);

    let result = result.unwrap();
    assert!(matches!(
        &result.content[0],
        ContentBlock::Text(text)
            if text.text == "Successfully replaced 1 block(s) in big.txt."
    ));
    let details = result.details.unwrap();
    let changes = details["diff"]
        .as_str()
        .unwrap()
        .lines()
        .filter(|line| line.starts_with(['+', '-']))
        .collect::<Vec<_>>();
    assert_eq!(changes, ["-100001 TARGET = 1", "+100001 TARGET = 2"]);
    assert!(details["patch"].as_str().unwrap().len() < 4 * 1024);

    let bytes = std::fs::read(&path).unwrap();
    let target = b"TARGET = 2\n";
    let half_length = 100_000 * filler.len();
    assert_eq!(bytes.len(), 2 * half_length + target.len());
    assert_eq!(&bytes[half_length..half_length + target.len()], target);
    assert!(
        bytes[..half_length]
            .chunks(filler.len())
            .all(|line| line == filler.as_bytes()),
        "the leading 100,000-line filler half changed"
    );
    assert!(
        bytes[half_length + target.len()..]
            .chunks(filler.len())
            .all(|line| line == filler.as_bytes()),
        "the trailing 100,000-line filler half changed"
    );
}
