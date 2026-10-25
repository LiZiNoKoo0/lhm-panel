//! Four breadboard keys, read as a focus control (T6d).
//!
//! What this does NOT do: it does not know what a key *means*. It reports "up was
//! pressed"; `main` decides that means "move the focus one step". That split is
//! PROTOCOL 11.5 and it is the reason the same firmware can drive a page whose
//! third button means "next page" today and "shut down" tomorrow.
//!
//! Wiring, from `board.rs`: each key sits between its GPIO and GND, with the
//! internal pull-up enabled here. Released reads high, pressed reads low. Never
//! wire a key to 3V3 -- pressing it would short the supply to ground.
//!
//! Why debounce at all: a mechanical contact bounces for 5-20 ms, so one press
//! arrives as a burst of edges. Without a filter, one press walks the focus three
//! or four items and looks like a double-press bug in the daemon.
//!
//! The filter is deliberately the simplest thing that works: a key only counts
//! when its level has been *unchanged* for DEBOUNCE_MS, and only on the
//! high-to-low edge. No timers, no interrupts, no queue -- the render loop
//! already ticks every 5 ms, which is four times faster than the debounce window
//! needs.

use esp_hal::gpio::{Input, InputConfig, Pull};

/// `{"t":"event","k":"key","id":"focus","v":N}\n` into `out`, returning the length.
///
/// Hand-rolled because the firmware has no allocator on this path and the
/// alternative -- a `json` writer for one fixed message -- would be more code
/// than the message. `position` is 1-based and at most `MAX_NODES`, so it can
/// only be one or two digits.
pub fn focus_event(out: &mut [u8], position: u8) -> Option<usize> {
    const HEAD: &[u8] = br#"{"t":"event","k":"key","id":"focus","v":"#;
    if out.len() < HEAD.len() + 8 {
        return None;
    }
    out[..HEAD.len()].copy_from_slice(HEAD);
    let mut at = HEAD.len();
    if position >= 10 {
        out[at] = b'0' + position / 10;
        at += 1;
    }
    out[at] = b'0' + position % 10;
    at += 1;
    out[at] = b'}';
    at += 1;
    out[at] = b'\n';
    at += 1;
    Some(at)
}
// Pin numbers live in `board.rs` (PIN_KEY1..PIN_KEY4) beside every other
// wire on this board: they are a wiring fact, not a property of the key
// reader. Duplicating them here produced four dead constants and four
// warnings; this module needs the peripherals `main` hands over, not the
// integers.

/// How long a level must hold before it is believed. 20 ms is above the bounce
/// time of the switches on the bench and far below a human's shortest tap.
const DEBOUNCE_MS: u32 = 20;

/// One key press, already debounced. The names describe the *direction* the
/// operator means, not the electrical event: a key going low is "up" only
/// because this firmware says so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Press {
    Up,
    Down,
    Confirm,
    Back,
}

struct Key<'d> {
    pin: Input<'d>,
    /// Level we last accepted, to detect edges rather than levels.
    held_low: bool,
    /// When the current level first appeared.
    since: u32,
    /// A press is reported at most once until the key is released.
    spent: bool,
}

impl<'d> Key<'d> {
    fn new(pin: Input<'d>, now_ms: u32) -> Self {
        let low = pin.is_low();
        Self {
            pin,
            held_low: low,
            since: now_ms,
            spent: low,
        }
    }

    /// `Some(())` on a debounced high-to-low edge, at most once per press.
    fn pressed(&mut self, now_ms: u32) -> bool {
        let low = self.pin.is_low();
        if low != self.held_low {
            // The level moved: restart the clock and believe nothing yet.
            self.held_low = low;
            self.since = now_ms;
            return false;
        }
        if now_ms.wrapping_sub(self.since) < DEBOUNCE_MS {
            return false;
        }
        if low && !self.spent {
            self.spent = true;
            return true;
        }
        if !low {
            // Released: arm the next press.
            self.spent = false;
        }
        false
    }
}

/// The four keys, polled from the render loop.
pub struct Keys<'d> {
    keys: [Key<'d>; 4],
}

impl<'d> Keys<'d> {
    pub fn new(
        up: Input<'d>,
        down: Input<'d>,
        confirm: Input<'d>,
        back: Input<'d>,
        now_ms: u32,
    ) -> Self {
        Self {
            keys: [
                Key::new(up, now_ms),
                Key::new(down, now_ms),
                Key::new(confirm, now_ms),
                Key::new(back, now_ms),
            ],
        }
    }

    /// Build the four pins with the internal pull-up the breadboard keys need.
    ///
    /// Kept here rather than in `main` so the pull-up cannot be forgotten at the
    /// call site: a key wired to GND without a pull-up floats when released and
    /// reads as random presses.
    pub fn config() -> InputConfig {
        InputConfig::default().with_pull(Pull::Up)
    }

    /// The first key that completed a debounced press, if any.
    ///
    /// At most one per tick, in a fixed priority order. Two keys pressed within
    /// the same 5 ms tick is a chording attempt, and this control has no chords;
    /// picking one deterministically beats a random winner.
    pub fn poll(&mut self, now_ms: u32) -> Option<Press> {
        const ORDER: [Press; 4] = [Press::Up, Press::Down, Press::Confirm, Press::Back];
        for (index, press) in ORDER.iter().enumerate() {
            if self.keys[index].pressed(now_ms) {
                return Some(*press);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    // The debounce is the only part with logic worth testing, and it is also the
    // only part that can be tested without hardware: the level comes from a pin,
    // so the test drives a fake.

    /// What `Key::pressed` needs from a pin: "is it low right now".
    struct FakePin {
        low: bool,
    }

    /// A copy of the state machine, with the pin replaced by a settable flag.
    ///
    /// Duplicated on purpose: the real `Key` owns an `Input<'d>` which cannot be
    /// constructed off-target. Keeping the rule in one place matters more than
    /// keeping the *code* in one place, so this mirrors the three fields and the
    /// decision exactly, and the cost of them drifting is paid here by a reviewer
    /// comparing twenty lines.
    struct Machine {
        held_low: bool,
        since: u32,
        spent: bool,
    }

    impl Machine {
        fn new(low: bool, now: u32) -> Self {
            Self { held_low: low, since: now, spent: low }
        }

        fn pressed(&mut self, low: bool, now: u32) -> bool {
            if low != self.held_low {
                self.held_low = low;
                self.since = now;
                return false;
            }
            if now.wrapping_sub(self.since) < super::DEBOUNCE_MS {
                return false;
            }
            if low && !self.spent {
                self.spent = true;
                return true;
            }
            if !low {
                self.spent = false;
            }
            false
        }
    }

    #[test]
    fn a_clean_press_reports_once() {
        let mut key = Machine::new(false, 0);
        assert!(!key.pressed(false, 100), "still released");

        // The press arrives at t=200; the level is only believed after 20 ms.
        assert!(!key.pressed(true, 200), "edge: clock restarts, nothing yet");
        assert!(!key.pressed(true, 215), "15 ms: still bouncing");
        assert!(key.pressed(true, 220), "20 ms held: this is a press");
        assert!(!key.pressed(true, 400), "held down: no second press");
    }

    #[test]
    fn a_bouncy_press_reports_once() {
        // A real switch: down, up, down, up within a few ms, then held.
        let mut key = Machine::new(false, 0);
        let mut reported = 0;
        for (low, t) in [
            (true, 100), (false, 102), (true, 105), (false, 107), (true, 110),
            (true, 125), (true, 131), (true, 200),
        ] {
            if key.pressed(low, t) {
                reported += 1;
            }
        }
        assert_eq!(reported, 1, "bounce must not walk the focus several steps");
    }

    #[test]
    fn a_second_press_needs_a_release_first() {
        let mut key = Machine::new(false, 0);
        assert!(!key.pressed(true, 100));
        assert!(key.pressed(true, 130), "first press");
        assert!(!key.pressed(true, 200), "still held");

        // Release, then press again.
        assert!(!key.pressed(false, 300), "release is not a press");
        assert!(!key.pressed(true, 320), "edge of the second press");
        assert!(key.pressed(true, 345), "second press reported");
    }

    #[test]
    fn a_tap_shorter_than_the_debounce_window_is_ignored() {
        // 10 ms of contact is below the filter: either a bounce or a glitch.
        let mut key = Machine::new(false, 0);
        assert!(!key.pressed(true, 100));
        assert!(!key.pressed(true, 110), "10 ms: too short");
        assert!(!key.pressed(false, 115), "released before it counted");
        assert!(!key.pressed(true, 200));
        assert!(key.pressed(true, 230), "the next real press still works");
    }

    #[test]
    fn a_key_held_at_boot_does_not_report_until_released() {
        // Power-on with a key stuck down (or a mis-wire to GND) must not fire a
        // press that nobody made.
        let mut key = Machine::new(true, 0);
        assert!(!key.pressed(true, 100), "held since boot");
        assert!(!key.pressed(true, 500), "still held");
        assert!(!key.pressed(false, 600), "release");
        assert!(!key.pressed(true, 620), "edge");
        assert!(key.pressed(true, 650), "now it counts");
    }
}
