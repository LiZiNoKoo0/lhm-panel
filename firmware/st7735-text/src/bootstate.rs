//! Where the boot self-test state lives while four different things fill it in.
//!
//! The card is painted by the render task, but the facts it shows are produced
//! by `main` (panel, flash), `net` (WiFi) and `net_link` (daemon). One atomic
//! byte per row is the smallest thing that lets all three publish without
//! sharing a lock -- and `AtomicU8` cannot be observed half-written, which a
//! `Mutex<[Step; N]>` held across an `.await` in the radio task could.
//!
//! What this does NOT do: it never draws. Painting belongs to the render task.
//! It does print, because a row settling is a wire event too -- the same rule
//! the `LINK LOST` badge follows: anything the board paints about itself must
//! also be something a script can hear.

use core::sync::atomic::{AtomicU8, Ordering};

use panel_core::boot::{Step, LABELS, STEPS};

use crate::diag;
use crate::log::ChanLog;

/// Zero means `Step::Pending`, which is exactly the state of a fresh boot --
/// so a zeroed static is already the right initial value.
static ROWS: [AtomicU8; STEPS] = [const { AtomicU8::new(0) }; STEPS];

/// Record one row's outcome, and say so once.
///
/// The swap makes the announcement edge-triggered: `net_link` calls this on
/// every connect attempt, and without the comparison the log would gain a line
/// per retry and the card would repaint for a state that did not change.
pub fn announce(index: usize, step: Step) {
    if index >= STEPS {
        return;
    }
    if ROWS[index].swap(step.code(), Ordering::Relaxed) != step.code() {
        let word = step.text();
        let status = core::str::from_utf8(word).unwrap_or("?");
        diag::boot_step(&mut ChanLog::new(), LABELS[index], status);
    }
}

/// Read all four rows. Cheap enough for the render loop to call every tick.
pub fn snapshot() -> [Step; STEPS] {
    let mut out = panel_core::boot::START;
    for (row, cell) in out.iter_mut().zip(ROWS.iter()) {
        *row = Step::from_code(cell.load(Ordering::Relaxed));
    }
    out
}
