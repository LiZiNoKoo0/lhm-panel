//! The link contract: how a scene's bytes and a dispatcher's lines move.
//!
//! What this does NOT do: no sockets, no UART, no threads, no retries by
//! itself. It defines the shape every transport must have, plus the reconnect
//! ladder, so that swapping UART for TCP does not change a single caller.

/// Why a transport gave up. Not every layer cares, but they must all be able to
/// say "this failed" without panicking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkError {
    /// Peer is gone / socket closed / cable pulled.
    Closed,
    /// Nothing arrived for a while, or a write timed out.
    Timeout,
    /// Hardware said no (driver returned an error).
    Io,
}

/// Backoff ladder shared by every transport and by the PC side (PROTOCOL §1).
/// Index is the number of consecutive failures; the last entry is the cap.
pub const BACKOFF_MS: [u32; 5] = [1_000, 2_000, 4_000, 8_000, 10_000];

/// Longest single diagnostics line, terminator included.
///
/// The line formatters (`diag.rs`) and the queue that carries their lines
/// (`log.rs`) must agree on this number, so it lives here where both can see
/// it. It has to be wide enough for the widest honest message -- a driver error
/// debug-printed in full -- and narrow enough that `Line::len` stays a `u8`.
pub const LINE_MAX: usize = 192;

pub fn backoff_ms(consecutive_failures: u32) -> u32 {
    let idx = consecutive_failures.saturating_sub(1) as usize;
    BACKOFF_MS[if idx < BACKOFF_MS.len() { idx } else { BACKOFF_MS.len() - 1 }]
}

/// How long a link may be silent before the panel says `LINK LOST` (SPEC L-1,
/// the same 15 s `term_tk.py` uses).
pub const SILENCE_MS: u32 = 15_000;

/// Splits a byte stream into `\n`-terminated lines, one byte at a time.
///
/// This is the one piece every transport must share. UART and TCP differ in
/// where bytes come from and nothing else: an over-long line is dropped whole
/// (SPEC 3.2 / E1), `\r` is ignored, a blank line is skipped rather than handed
/// on as an empty message. Two hand-written copies of this drifted apart once
/// already (BUGLOG #7: the over-long path ate the `\n` and glued the next log
/// line onto it), which is why it lives here and not in each transport.
///
/// Feed bytes one at a time from a non-blocking read. `push` returns the length
/// of a line that just completed, whose bytes are then valid via `line`.
pub struct LineBuf<const N: usize> {
    buf: [u8; N],
    fill: usize,
    /// True while discarding the remainder of an over-long line.
    skipping: bool,
    overlong: u32,
}

impl<const N: usize> LineBuf<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            fill: 0,
            skipping: false,
            overlong: 0,
        }
    }

    /// One byte in; `Some(len)` when that byte completed a usable line.
    pub fn push(&mut self, byte: u8) -> Option<usize> {
        match byte {
            b'\n' => {
                let len = self.fill;
                let dropped = self.skipping;
                self.fill = 0;
                self.skipping = false;
                if dropped || len == 0 {
                    None
                } else {
                    Some(len)
                }
            }
            b'\r' => None,
            _ => {
                if self.skipping {
                    return None;
                }
                if self.fill < N {
                    self.buf[self.fill] = byte;
                    self.fill += 1;
                } else {
                    // Drop the whole line, not half of it: a truncated JSON
                    // line that still parses is worse than no line at all.
                    self.overlong = self.overlong.wrapping_add(1);
                    self.fill = 0;
                    self.skipping = true;
                }
                None
            }
        }
    }

    /// The bytes of the line whose length `push` just returned.
    pub fn line(&self, len: usize) -> &[u8] {
        &self.buf[..len]
    }

    /// Lines dropped for being too long (E1). Never silently zero: a wire that
    /// keeps sending oversized lines must be visible in a `stat` line.
    pub fn overlong(&self) -> u32 {
        self.overlong
    }
}

impl<const N: usize> Default for LineBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Silence detector. Only differences are computed, never comparisons of
/// absolute values: `now_ms` is a wrapping `u32` and wraps every ~49.7 days
/// (SPEC 3.4).
pub struct Watchdog {
    limit_ms: u32,
    last_ms: u32,
}

impl Watchdog {
    pub const fn new(limit_ms: u32) -> Self {
        Self {
            limit_ms,
            last_ms: 0,
        }
    }

    /// Start the clock without counting time already spent booting.
    pub fn start(&mut self, now_ms: u32) {
        self.last_ms = now_ms;
    }

    pub fn feed(&mut self, now_ms: u32) {
        self.last_ms = now_ms;
    }

    pub fn silent_ms(&self, now_ms: u32) -> u32 {
        now_ms.wrapping_sub(self.last_ms)
    }

    pub fn is_lost(&self, now_ms: u32) -> bool {
        self.silent_ms(now_ms) >= self.limit_ms
    }
}

/// Edge, not level: the caller logs a line per transition, not per poll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transition {
    Lost,
    Recovered,
}

/// Wraps a `Watchdog` with the state it is guarding, so "went silent" and
/// "came back" are each reported exactly once.
pub struct Silence {
    watchdog: Watchdog,
    down: bool,
}

impl Silence {
    pub const fn new(limit_ms: u32) -> Self {
        Self {
            watchdog: Watchdog::new(limit_ms),
            down: false,
        }
    }

    /// Bytes arrived: the link is up again, if it ever went down.
    pub fn feed(&mut self, now_ms: u32) -> Option<Transition> {
        self.watchdog.feed(now_ms);
        if self.down {
            self.down = false;
            return Some(Transition::Recovered);
        }
        None
    }

    pub fn poll(&mut self, now_ms: u32) -> Option<Transition> {
        if !self.down && self.watchdog.is_lost(now_ms) {
            self.down = true;
            return Some(Transition::Lost);
        }
        None
    }

    pub fn is_down(&self) -> bool {
        self.down
    }

    pub fn silent_ms(&self, now_ms: u32) -> u32 {
        self.watchdog.silent_ms(now_ms)
    }
}

/// One line at a time. `poll_line` must never block forever: returning
/// `Ok(None)` means "come back later", which is what keeps the render loop
/// predictable (ARCHITECTURE §4).
pub trait Transport {
    /// Copies one `\n`-terminated line (without the newline) into `out`.
    /// Returns the length, or `None` if no complete line is available yet.
    fn poll_line(&mut self, out: &mut [u8]) -> Result<Option<usize>, LinkError>;

    /// Writes everything or reports an error. Partial writes must be retried
    /// inside the implementation, not by the caller.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError>;

    fn is_up(&self) -> bool;
}

// -- one inbox, several wires ------------------------------------------------

/// Which layer a line belongs to. Decided by the first non-space byte, not by
/// which transport delivered it: the UART carries bench lines today and may
/// carry a protocol line tomorrow, and a decision that depends on the cable
/// would make one wire behave differently from another for no reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// JSON on the wire: the thin protocol (`hello`, `catalog`, `ui`, `data`).
    Proto,
    /// The bench format `<top>|<bottom>` that parity checks are built on.
    Bench,
}

/// Most lines taken from one transport in a single tick. A daemon that starts
/// streaming `data` must not starve the frame: the render loop has 5 ms between
/// ticks and it has to spend most of them pushing pixels.
pub const PER_SOURCE_PER_TICK: usize = 6;

/// Hard cap across all transports per tick, so two noisy wires cannot add up
/// to a stall that neither would cause alone.
pub const PER_TICK_TOTAL: usize = 8;

/// `None` for a line with nothing routable in it (empty or whitespace only).
pub fn route(line: &[u8]) -> Option<Route> {
    for &byte in line {
        match byte {
            b' ' | b'\t' => continue,
            b'{' => return Some(Route::Proto),
            _ => return Some(Route::Bench),
        }
    }
    None
}

/// Merges every transport into one ordered stream of classified lines.
///
/// What it does NOT do: it does not decide who owns the screen, it does not
/// parse anything, and it holds no queue -- a line is handed to the callback
/// and forgotten, so a slow consumer cannot grow a backlog. Screen ownership
/// (remote layout vs. a local page vs. the fallback page) is a separate state
/// machine, because "which line arrived" and "whose picture is on the glass"
/// are different questions that change on different events.
pub struct Inbox {
    proto: u32,
    bench: u32,
    unroutable: u32,
    errors: u32,
}

impl Inbox {
    pub const fn new() -> Self {
        Self {
            proto: 0,
            bench: 0,
            unroutable: 0,
            errors: 0,
        }
    }

    /// Pulls at most `PER_SOURCE_PER_TICK` lines per transport and at most
    /// `PER_TICK_TOTAL` overall, never blocking. `buf` must be at least as long
    /// as the widest line any source may send; a shorter one silently truncates
    /// for the sources that honour it, so pass `wire::LINE_CAP` for TCP.
    pub fn poll<F>(&mut self, sources: &mut [&mut dyn Transport], buf: &mut [u8], mut on_line: F)
    where
        F: FnMut(Route, &[u8]),
    {
        let mut total = 0usize;
        for source in sources.iter_mut() {
            let mut taken = 0usize;
            while taken < PER_SOURCE_PER_TICK && total < PER_TICK_TOTAL {
                match source.poll_line(buf) {
                    Ok(Some(len)) => {
                        taken += 1;
                        total += 1;
                        let line = &buf[..len];
                        match route(line) {
                            Some(Route::Proto) => {
                                self.proto = self.proto.wrapping_add(1);
                                on_line(Route::Proto, line);
                            }
                            Some(Route::Bench) => {
                                self.bench = self.bench.wrapping_add(1);
                                on_line(Route::Bench, line);
                            }
                            None => self.unroutable = self.unroutable.wrapping_add(1),
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        self.errors = self.errors.wrapping_add(1);
                        break;
                    }
                }
            }
        }
    }

    pub fn proto_lines(&self) -> u32 {
        self.proto
    }

    pub fn bench_lines(&self) -> u32 {
        self.bench
    }

    pub fn unroutable(&self) -> u32 {
        self.unroutable
    }

    pub fn errors(&self) -> u32 {
        self.errors
    }
}

impl Default for Inbox {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::backoff_ms;
    use super::{
        route, Inbox, LineBuf, LinkError, Route, Silence, Transport, Transition, Watchdog,
        PER_SOURCE_PER_TICK, PER_TICK_TOTAL, SILENCE_MS,
    };

    #[test]
    fn ladder_matches_the_pc_side() {
        assert_eq!(backoff_ms(0), 1_000);
        assert_eq!(backoff_ms(1), 1_000);
        assert_eq!(backoff_ms(2), 2_000);
        assert_eq!(backoff_ms(3), 4_000);
        assert_eq!(backoff_ms(4), 8_000);
        assert_eq!(backoff_ms(5), 10_000);
        assert_eq!(backoff_ms(60), 10_000);
    }

    // -- line splitting ----------------------------------------------------

    fn feed<const N: usize>(buf: &mut LineBuf<N>, bytes: &[u8]) -> Option<usize> {
        let mut done = None;
        for &byte in bytes {
            if let Some(len) = buf.push(byte) {
                done = Some(len);
            }
        }
        done
    }

    #[test]
    fn splits_on_newline_only() {
        let mut buf: LineBuf<16> = LineBuf::new();
        assert_eq!(feed(&mut buf, b"abc\ndef\n"), Some(3), "last line wins");
        assert_eq!(buf.line(3), b"def");

        let mut buf: LineBuf<16> = LineBuf::new();
        assert_eq!(feed(&mut buf, b"abc"), None, "no newline, no line");
        assert_eq!(feed(&mut buf, b"d\n"), Some(4));
        assert_eq!(buf.line(4), b"abcd");
    }

    #[test]
    fn carriage_return_is_ignored() {
        let mut buf: LineBuf<16> = LineBuf::new();
        assert_eq!(feed(&mut buf, b"abc\r\n"), Some(3));
        assert_eq!(buf.line(3), b"abc");
    }

    #[test]
    fn blank_lines_are_skipped_not_delivered() {
        let mut buf: LineBuf<16> = LineBuf::new();
        assert_eq!(feed(&mut buf, b"\n\r\n\n"), None);
        assert_eq!(feed(&mut buf, b"x\n"), Some(1));
    }

    #[test]
    fn overlong_line_is_dropped_whole_and_counted() {
        let mut buf: LineBuf<8> = LineBuf::new();
        // Nine bytes then a newline: the ninth overflows, so the whole line
        // goes away -- including the tail that would have fit.
        assert_eq!(feed(&mut buf, b"123456789\n"), None);
        assert_eq!(buf.overlong(), 1);
        // The next line is unaffected, and the newline was consumed by the
        // dropped line rather than gluing onto what follows (BUGLOG #7).
        assert_eq!(feed(&mut buf, b"ok\n"), Some(2));
        assert_eq!(buf.line(2), b"ok");
    }

    #[test]
    fn exactly_full_line_still_fits() {
        let mut buf: LineBuf<4> = LineBuf::new();
        assert_eq!(feed(&mut buf, b"abcd\n"), Some(4));
        assert_eq!(buf.overlong(), 0);
    }

    // -- silence ---------------------------------------------------------

    #[test]
    fn silence_threshold_is_fifteen_seconds() {
        assert_eq!(SILENCE_MS, 15_000);
        let mut watchdog = Watchdog::new(SILENCE_MS);
        watchdog.start(1_000);
        assert!(!watchdog.is_lost(15_999));
        assert!(watchdog.is_lost(16_000));
        watchdog.feed(16_000);
        assert!(!watchdog.is_lost(16_001));
    }

    #[test]
    fn silence_survives_the_millisecond_counter_wrapping() {
        // SPEC 3.4: only differences are meaningful, so a wrap must not look
        // like 49 days of silence.
        let mut watchdog = Watchdog::new(SILENCE_MS);
        watchdog.start(u32::MAX - 100);
        assert!(!watchdog.is_lost(100), "200 ms of real silence");
        assert!(watchdog.is_lost(15_000), "15.1 s across the wrap");
    }

    #[test]
    fn transitions_are_reported_once_each() {
        let mut silence = Silence::new(SILENCE_MS);
        assert_eq!(silence.poll(0), None);
        assert_eq!(silence.poll(15_000), Some(Transition::Lost));
        assert_eq!(silence.poll(20_000), None, "level, not edge");
        assert!(silence.is_down());
        assert_eq!(silence.feed(20_001), Some(Transition::Recovered));
        assert!(!silence.is_down());
        assert_eq!(silence.poll(20_002), None);
    }

    // -- routing -----------------------------------------------------------

    #[test]
    fn routing_is_decided_by_the_first_real_byte() {
        assert_eq!(route(br#"{"t":"catalog"}"#), Some(Route::Proto));
        assert_eq!(route(b"  \t{\"t\":\"data\"}"), Some(Route::Proto));
        assert_eq!(route(b"CPU 33%|TEMP 34C"), Some(Route::Bench));
        assert_eq!(route(b"|"), Some(Route::Bench));
        assert_eq!(route(b"   "), None);
        assert_eq!(route(b""), None);
    }

    // -- inbox -------------------------------------------------------------

    /// Hands out one scripted line per `poll_line` call, then `None`. Errors can
    /// be scripted too, because a transport that keeps failing must not spin.
    struct Script {
        lines: [&'static [u8]; 16],
        at: usize,
        fail_at: Option<usize>,
        polls: usize,
    }

    impl Script {
        fn new(lines: &[&'static [u8]]) -> Self {
            let mut store: [&'static [u8]; 16] = [b""; 16];
            for (slot, line) in store.iter_mut().zip(lines.iter()) {
                *slot = line;
            }
            Self {
                lines: store,
                at: 0,
                fail_at: None,
                polls: 0,
            }
        }
    }

    impl Transport for Script {
        fn poll_line(&mut self, out: &mut [u8]) -> Result<Option<usize>, LinkError> {
            self.polls += 1;
            if self.fail_at == Some(self.at) {
                return Err(LinkError::Io);
            }
            if self.at >= self.lines.len() || self.lines[self.at].is_empty() {
                return Ok(None);
            }
            let line = self.lines[self.at];
            self.at += 1;
            let n = line.len().min(out.len());
            out[..n].copy_from_slice(&line[..n]);
            Ok(Some(n))
        }

        fn write_all(&mut self, _bytes: &[u8]) -> Result<(), LinkError> {
            Ok(())
        }

        fn is_up(&self) -> bool {
            true
        }
    }

    #[test]
    fn inbox_classifies_from_every_source() {
        let mut uart = Script::new(&[b"CPU 33%|TEMP 34C"]);
        let mut tcp = Script::new(&[br#"{"t":"catalog","ver":3}"#]);
        let mut inbox = Inbox::new();
        let mut buf = [0u8; 64];
        let mut seen: [(Route, usize); 4] = [(Route::Bench, 0); 4];
        let mut count = 0;

        let mut sources: [&mut dyn Transport; 2] = [&mut uart, &mut tcp];
        inbox.poll(&mut sources, &mut buf, |route, line| {
            seen[count] = (route, line.len());
            count += 1;
        });

        assert_eq!(count, 2);
        assert_eq!(seen[0], (Route::Bench, 16));
        assert_eq!(seen[1], (Route::Proto, 23));
        assert_eq!(inbox.proto_lines(), 1);
        assert_eq!(inbox.bench_lines(), 1);
    }

    #[test]
    fn inbox_stops_at_its_per_source_quota() {
        let lines: [&'static [u8]; 10] = [
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
            br#"{"t":"data","v":{}}"#,
        ];
        let mut tcp = Script::new(&lines);
        let mut inbox = Inbox::new();
        let mut buf = [0u8; 64];
        let mut count = 0;

        let mut sources: [&mut dyn Transport; 1] = [&mut tcp];
        inbox.poll(&mut sources, &mut buf, |_, _| count += 1);

        assert!(count <= PER_SOURCE_PER_TICK, "took {} lines", count);
        assert!(count <= PER_TICK_TOTAL);
        assert!(tcp.at < lines.len(), "backlog must stay in the transport");
    }

    #[test]
    fn inbox_counts_an_error_and_moves_on() {
        let mut broken = Script::new(&[b"never read"]);
        broken.fail_at = Some(0);
        let mut fine = Script::new(&[b"CPU 1%C"]);
        let mut inbox = Inbox::new();
        let mut buf = [0u8; 32];
        let mut count = 0;

        let mut sources: [&mut dyn Transport; 2] = [&mut broken, &mut fine];
        inbox.poll(&mut sources, &mut buf, |_, _| count += 1);

        assert_eq!(inbox.errors(), 1);
        assert_eq!(count, 1, "the healthy wire still delivers");
        assert!(broken.polls <= 2, "a failing source is not re-polled in a spin");
    }
}
