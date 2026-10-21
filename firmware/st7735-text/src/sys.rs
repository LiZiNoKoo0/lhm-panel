//! The two capabilities every later card assumes: a heap and a millisecond
//! clock. Nothing here prints -- `main.rs` decides what the banner says, and
//! `diag.rs` decides how it looks.
//!
//! What this does NOT do: allocate anything on its own after the one probe, or
//! keep time across a reset. The clock is read-only and always starts at 0.

use alloc::vec::Vec;

/// Heap budget. 64 KB of internal DRAM, and it is *only* the heap: the 25600
/// byte frame (SPEC D-5) is a `static`, so it never competes with this.
pub const HEAP_BYTES: usize = 64 * 1024;

/// What the self-check allocates and holds while the banner prints `free`.
/// Big enough that the drop in free bytes is unmistakable, small enough that
/// holding it costs nothing.
const PROBE_BYTES: usize = 1024;

/// Bring the global allocator up. Call exactly once, before the first `Vec`.
pub fn init_heap() {
    esp_alloc::heap_allocator!(size: HEAP_BYTES);
}

/// Milliseconds since power-up. 0 at boot, +1 per millisecond.
///
/// On this chip `Instant::now()` reads the 16 MHz TIMG0 counter that esp-hal's
/// `rt` startup already configured (it is the same source embassy-time uses
/// later). It wraps after ~49.7 days, which no monitor session reaches; the
/// wrap is silent, so never compare two readings by `<`.
pub fn now_ms() -> u32 {
    (esp_hal::time::Instant::now().duration_since_epoch().as_micros() / 1_000) as u32
}

/// What the banner needs to know about the heap -- and the one bit that says
/// the allocator actually served memory instead of just reporting numbers.
pub struct HeapProbe {
    /// Deliberately kept alive while `free` is measured: the number then proves
    /// the allocator reserved something. A `free` equal to `size` means the
    /// probe did not happen, not that the heap is healthy.
    _held: Vec<u8>,
    pub size: usize,
    pub free: usize,
    /// All 1024 bytes were written one pattern and read back the same.
    pub verified: bool,
}

/// Allocate, fill, read back, then report the heap while still holding the
/// allocation. Drop the returned value to give the KB back.
pub fn probe() -> HeapProbe {
    let mut held = Vec::with_capacity(PROBE_BYTES);
    for i in 0..PROBE_BYTES {
        held.push((i as u8) ^ 0x5A);
    }
    // Read back through the heap: if the allocator handed us a bad region this
    // is the cheapest place to find out, before three later cards depend on it.
    let verified = (0..PROBE_BYTES).all(|i| held[i] == (i as u8) ^ 0x5A);

    let stats = esp_alloc::HEAP.stats();
    HeapProbe {
        _held: held,
        size: stats.size,
        free: stats.size.saturating_sub(stats.current_usage),
        verified,
    }
}
