//! The two capabilities every later card assumes: a heap and a millisecond
//! clock. Nothing here prints -- `main.rs` decides what the banner says, and
//! `diag.rs` decides how it looks.
//!
//! What this does NOT do: allocate anything on its own after the one probe, or
//! keep time across a reset. The clock is read-only and always starts at 0.

use alloc::vec::Vec;

/// Heap budget. **Provisional**: esp-radio's own measurements are 47-57 KB for
/// a station and 53-63 KB for an access point, so the 64 KB of T2 could not
/// survive WiFi.
///
/// The ceiling is not RAM, it is the *stack*: on this chip `.data` + `.bss` +
/// stack share one 192 KB DRAM range ending at 0x3FFE_0000, and the linker puts
/// the stack directly above `.bss`. At 128 KB this left 1,716 bytes of stack and
/// `esp_hal::init` died with "stack pointer out of range" before the banner --
/// a heap that is too big looks exactly like a broken chip.
///
/// Was 96 KB until T6. `Page` (a 32-node layout plus the value table) and the
/// widened UART line buffer are task state, so they live in the executor's
/// pool in `.bss` like everything else -- and the stack assert in `init_heap`
/// is where that bill arrives. 84 KB leaves the same ~20 KB of main stack this
/// board had before T6 and still leaves the station ~27 KB of headroom over the
/// 57 KB worst case. When a later card needs more static RAM, this is the
/// number to cut, not the assert.
pub const HEAP_BYTES: usize = 84 * 1024;

/// The floor for the main task's stack, enforced in `init_heap`.
///
/// B-1 (BACKLOG 2026-10-22) is why it is no longer 8 KB: a build with 11.2 KB
/// passed the old check, booted, printed a full banner, and then overflowed the
/// stack inside WiFi association -- reset loop, panic pointing at a ROM address,
/// nothing in our code named. 8 KB was a guess made before WiFi existed. 16 KB is
/// the smallest figure we have measured to survive association plus one deep ROM
/// call. When a later card needs more static RAM, this assert is where the bill
/// arrives: early, and by name, instead of as a boot loop.
const MIN_STACK_BYTES: usize = 16 * 1024;

/// What the self-check allocates and holds while the banner prints `free`.
/// Big enough that the drop in free bytes is unmistakable, small enough that
/// holding it costs nothing.
const PROBE_BYTES: usize = 1024;

/// Bring the global allocator up. Call exactly once, before the first `Vec`.
pub fn init_heap() {
    esp_alloc::heap_allocator!(size: HEAP_BYTES);
    // Checked here, at the first line of `main`, because the failure mode it
    // guards against is silent: too big a heap moves the linker's stack bottom
    // up, and the symptom is a boot loop in `esp_hal::init` that names the chip
    // instead of the heap. One comparison per boot buys a message that does.
    assert!(
        stack_bytes() >= MIN_STACK_BYTES,
        "heap leaves under {} KB of stack: .data, .bss and the stack share one DRAM range",
        MIN_STACK_BYTES / 1024
    );
}

/// Bytes available for the main task's stack, as the linker sees them.
///
/// Read straight from the linker's own symbols rather than recomputed, so this
/// cannot drift from what `esp_hal` asserts against. Anything under
/// `MIN_STACK_BYTES` means the heap (or a large static) has eaten the stack and
/// the next `main` frame will not fit. Printed on every boot by `diag::stack_total`.
pub fn stack_bytes() -> usize {
    unsafe extern "C" {
        static _stack_end_cpu0: u32;
        static _stack_start_cpu0: u32;
    }
    let bottom = (&raw const _stack_end_cpu0) as usize;
    let top = (&raw const _stack_start_cpu0) as usize;
    top.saturating_sub(bottom)
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
