//! Process-wide allocation counts for separate diagnostic passes, not timing.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static REALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static REQUESTED_BYTES: AtomicU64 = AtomicU64::new(0);

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
type InnerAllocator = mimalloc::MiMalloc;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
type InnerAllocator = std::alloc::System;

struct CountingAllocator(InnerAllocator);

// SAFETY: every operation forwards the caller's pointer/layout unchanged to the
// same allocator. Counter updates neither allocate nor call user code.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            REQUESTED_BYTES.fetch_add(layout.size() as u64, Relaxed);
        }
        // SAFETY: the caller's allocation contract is forwarded unchanged.
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENABLED.load(Relaxed) {
            FREES.fetch_add(1, Relaxed);
        }
        // SAFETY: the pointer and layout go to their original allocator.
        unsafe { self.0.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Relaxed) {
            REALLOCATIONS.fetch_add(1, Relaxed);
            REQUESTED_BYTES.fetch_add(size as u64, Relaxed);
        }
        // SAFETY: the caller's reallocation contract is forwarded unchanged.
        unsafe { self.0.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
static GLOBAL: CountingAllocator = CountingAllocator(mimalloc::MiMalloc);
#[global_allocator]
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
static GLOBAL: CountingAllocator = CountingAllocator(std::alloc::System);

#[derive(serde::Serialize)]
pub struct Counts {
    pub allocations: u64,
    pub reallocations: u64,
    pub frees: u64,
    pub requested_bytes: u64,
}

/// Call only between completed passes, while decoder workers are idle.
pub fn start() {
    ALLOCATIONS.store(0, Relaxed);
    REALLOCATIONS.store(0, Relaxed);
    FREES.store(0, Relaxed);
    REQUESTED_BYTES.store(0, Relaxed);
    ENABLED.store(true, Relaxed);
}

/// Call only after all measured decoding and result destruction have completed.
pub fn finish() -> Counts {
    ENABLED.store(false, Relaxed);
    Counts {
        allocations: ALLOCATIONS.load(Relaxed),
        reallocations: REALLOCATIONS.load(Relaxed),
        frees: FREES.load(Relaxed),
        requested_bytes: REQUESTED_BYTES.load(Relaxed),
    }
}
