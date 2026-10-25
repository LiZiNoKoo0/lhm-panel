//! The daemon session: what the wire *means*, and in what order it must happen.
//!
//! `link.rs` owns bytes, `wire.rs` owns the shape of a message, this module owns
//! the sequence `connect -> hello -> catalog -> (data...) -> silence -> backoff`
//! (PROTOCOL 搂5/搂16, SPEC 搂5.4). Nothing here holds a socket or a clock, so
//! every rule below is a host test rather than a bench experiment.
//!
//! What this does NOT do: it does not parse JSON. It asks `wire` "what kind of
//! message is this", and that question is answered by looking at one byte.
//! The full parser arrives with T6, when the device has to *understand* `ui`.

use crate::link::{backoff_ms, Watchdog, SILENCE_MS};
use crate::wire;

/// Where the session is. Three states, not five: anything richer would be
/// invented rather than needed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// No socket. `Greeting` and `Ready` both fall back here on any failure.
    Down,
    /// Socket is up, `hello` is out, waiting for the daemon's first word.
    Greeting,
    /// The daemon sent `catalog`: it knows us and will send `ui`/`data`.
    /// The daemon sent challenge: our hello was understood and it wants
    /// proof that we hold the key (PROTOCOL s.2.1). Not Ready yet -- nothing
    /// but the answer may be expected until it sends catalog.
    Challenged,
    Ready,
}

/// What the caller must put on the wire. Returned instead of written, because
/// the byte-moving half lives in the firmware and cannot be tested here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    /// First thing after connect, and the only mandatory message from us.
    SendHello,
    /// PROTOCOL 搂7.1: `ping` is answered, always.
    SendPong,
}

/// What one inbound line turned out to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Line {
    /// Counted and dropped. Includes malformed input: a garbage line is not a
    /// reason to reset a working link (SPEC E3 keeps it as a counter).
    Ignore,
    /// Answer with `pong`.
    Pong,
    /// The metric list, with how many metrics it declared. The session is
    /// usable from here on.
    Catalog(usize),
    /// Token rejected (PROTOCOL 搂5). The caller closes the socket; the reason
    /// text is fetched by the caller with `wire::str_field`.
    /// The daemon's nonce (PROTOCOL s.2.1). The caller answers it with
    /// uth::mac_into; this module holds no key and does no cryptography.
    Challenge,
    Denied,
    /// `cmd` including `reset`. T6 acts on it; counted here so it is never
    /// silently folded into "other" while nobody is looking.
    Command(u32),
}

/// Per-kind counters, the replacement for T5.2's "log every line".
///
/// T5.2 printed each received line to prove bytes moved; a daemon sending `data`
/// at 1 Hz would then own the UART. From T5.3 on, the wire says one line per
/// event and these numbers say how much traffic happened.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Counters {
    pub rx: u32,
    pub catalog: u32,
    pub ui: u32,
    pub data: u32,
    pub ping: u32,
    pub command: u32,
    pub challenge: u32,
    pub denied: u32,
    /// Lines that were not any known kind, including non-JSON noise.
    pub other: u32,
}

/// The session state machine. Cheap to construct (no allocation, ~20 bytes) and
/// free of I/O.
pub struct Session {
    state: State,
    watchdog: Watchdog,
    /// Consecutive failures, 1-based: 1 means "the first attempt just failed",
    /// which is what `backoff_ms` expects.
    failures: u32,
    pub counters: Counters,
}

impl Session {
    pub const fn new() -> Self {
        Self {
            state: State::Down,
            watchdog: Watchdog::new(SILENCE_MS),
            failures: 0,
            counters: Counters {
                rx: 0,
                catalog: 0,
                ui: 0,
                data: 0,
                ping: 0,
                command: 0,
                challenge: 0,
                denied: 0,
                other: 0,
            },
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Milliseconds the wire has been silent. Only meaningful while connected.
    pub fn silent_ms(&self, now_ms: u32) -> u32 {
        self.watchdog.silent_ms(now_ms)
    }

    /// A socket just connected: greet it.
    ///
    /// The watchdog starts here and not at boot, so a slow connect never looks
    /// like a silent daemon.
    pub fn connected(&mut self, now_ms: u32) -> Action {
        self.state = State::Greeting;
        self.watchdog.start(now_ms);
        Action::SendHello
    }

    /// One inbound line, already split by `LineBuf`.
    pub fn line(&mut self, line: &[u8], now_ms: u32) -> Line {
        // Any byte at all means the peer is alive -- including nonsense. This
        // must happen before the kind is examined, or a stream of unknown
        // messages would still read as "silent".
        self.watchdog.feed(now_ms);
        self.counters.rx = self.counters.rx.saturating_add(1);

        let kind = wire::message_kind(line);
        match kind {
            Some(b"catalog") => {
                self.counters.catalog = self.counters.catalog.saturating_add(1);
                self.state = State::Ready;
                // A daemon that described its metrics answered our hello, so
                // the next disconnect starts the ladder from the bottom again.
                self.failures = 0;
                Line::Catalog(wire::count_ids(line))
            }
            Some(b"ping") => {
                self.counters.ping = self.counters.ping.saturating_add(1);
                Line::Pong
            }
            Some(b"ui") => {
                self.counters.ui = self.counters.ui.saturating_add(1);
                Line::Ignore
            }
            Some(b"data") => {
                self.counters.data = self.counters.data.saturating_add(1);
                Line::Ignore
            }
            Some(b"cmd") => {
                self.counters.command = self.counters.command.saturating_add(1);
                Line::Command(0)
            }
            Some(b"challenge") => {
                self.counters.challenge = self.counters.challenge.saturating_add(1);
                if self.state == State::Greeting {
                    self.state = State::Challenged;
                }
                Line::Challenge
            }
            Some(b"deny") => {
                self.counters.denied = self.counters.denied.saturating_add(1);
                // Not `Ready`, and not `Greeting` either: the daemon will drop
                // us, so the only honest state is the one before any socket.
                self.state = State::Down;
                Line::Denied
            }
            _ => {
                self.counters.other = self.counters.other.saturating_add(1);
                Line::Ignore
            }
        }
    }

    /// Has the wire been silent for `SILENCE_MS` while a socket is up?
    ///
    /// A level, not an edge: the caller is a `select` loop that must act on the
    /// first true, and only closes the socket once, so an edge here would need
    /// its own state for no gain.
    pub fn is_silent_dead(&self, now_ms: u32) -> bool {
        self.state != State::Down && self.watchdog.is_lost(now_ms)
    }

    /// The connection ended: by peer close, error, or our own watchdog.
    /// Returns how long to wait before the next attempt.
    pub fn closed(&mut self) -> u32 {
        self.state = State::Down;
        self.failures = self.failures.saturating_add(1);
        backoff_ms(self.failures)
    }

    /// The connect itself failed. Same ladder as `closed`, different log line:
    /// "the daemon is not answering" and "the daemon hung up" send an operator
    /// to different places.
    pub fn connect_failed(&mut self) -> u32 {
        self.state = State::Down;
        self.failures = self.failures.saturating_add(1);
        backoff_ms(self.failures)
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the message the daemon really sends, so the tests cannot drift
    /// into a shape the daemon never produces.
    fn cat(metrics: usize) -> std::vec::Vec<u8> {
        let mut s = std::string::String::from("{\"t\":\"catalog\",\"s\":1,\"ver\":1,\"metrics\":[");
        for i in 0..metrics {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"id\":\"m");
            s.push_str(&i.to_string());
            s.push_str("\",\"label\":\"x\"}");
        }
        s.push_str("]}");
        s.into_bytes()
    }

    #[test]
    fn hello_is_the_first_thing_after_connect() {
        let mut s = Session::new();
        assert_eq!(s.state(), State::Down);
        assert_eq!(s.connected(1_000), Action::SendHello);
        assert_eq!(s.state(), State::Greeting, "not ready until the daemon speaks");
    }

    #[test]
    fn catalog_makes_the_session_ready_and_reports_the_metric_count() {
        let mut s = Session::new();
        s.connected(0);
        assert_eq!(s.line(&cat(3), 10), Line::Catalog(3));
        assert_eq!(s.state(), State::Ready);
        assert_eq!(s.counters.catalog, 1);
        assert_eq!(s.counters.rx, 1);
    }

    #[test]
    fn deny_goes_back_to_down_and_is_never_ready() {
        let mut s = Session::new();
        s.connected(0);
        let line = br#"{"t":"deny","reason":"bad token"}"#;
        assert_eq!(s.line(line, 5), Line::Denied);
        assert_eq!(s.state(), State::Down);
        assert_eq!(s.counters.denied, 1);
    }

    #[test]
    fn ping_is_always_answered() {
        let mut s = Session::new();
        s.connected(0);
        assert_eq!(s.line(br#"{"t":"ping"}"#, 1), Line::Pong);
        assert_eq!(s.counters.ping, 1);
    }

    #[test]
    fn unknown_and_malformed_lines_feed_the_watchdog_and_are_counted() {
        let mut s = Session::new();
        s.connected(0);
        assert_eq!(s.line(b"not json at all", 9_000), Line::Ignore);
        assert_eq!(s.counters.other, 1);
        // 9 s of silence is forgiven because a byte arrived, garbage or not.
        assert!(!s.is_silent_dead(9_500));
        assert_eq!(s.line(b"{", 9_600), Line::Ignore);
        assert_eq!(s.counters.other, 2);
    }

    #[test]
    fn fifteen_seconds_of_silence_is_the_threshold() {
        let mut s = Session::new();
        s.connected(0);
        assert!(!s.is_silent_dead(SILENCE_MS - 1));
        assert!(s.is_silent_dead(SILENCE_MS), "15 s exactly is already lost");
    }

    #[test]
    fn a_down_session_is_never_silent_dead() {
        // Otherwise a board with no daemon would log `LINK lost` forever while
        // it is merely waiting to reconnect.
        let s = Session::new();
        assert!(!s.is_silent_dead(1_000_000));
    }

    #[test]
    fn the_backoff_ladder_matches_the_pc_ladder_and_resets_after_a_catalog() {
        let mut s = Session::new();
        assert_eq!(s.connect_failed(), 1_000);
        assert_eq!(s.connect_failed(), 2_000);
        assert_eq!(s.connect_failed(), 4_000);
        assert_eq!(s.connect_failed(), 8_000);
        assert_eq!(s.connect_failed(), 10_000);
        assert_eq!(s.connect_failed(), 10_000, "the top step repeats");

        s.connected(0);
        s.line(&cat(1), 10);
        assert_eq!(s.failures(), 0, "a real session resets the ladder");
        assert_eq!(s.closed(), 1_000);
    }

    #[test]
    fn a_denied_hello_still_climbs_the_ladder() {
        // 2026-10-22, found on the board (B-2): the pump used to treat
        // "connected" as "accepted", so a strict daemon's `deny` produced one
        // connect per second forever with `tries=1`. Being refused is a
        // failure, and the ladder is what stops the retry storm.
        let deny = br#"{"t":"deny","reason":"bad token"}"#;
        let mut s = Session::new();
        s.connected(0);
        assert_eq!(s.line(deny, 5), Line::Denied);
        assert_eq!(s.closed(), 1_000);
        s.connected(10);
        assert_eq!(s.line(deny, 15), Line::Denied);
        assert_eq!(s.closed(), 2_000);
    }

    #[test]
    fn a_session_that_dies_silent_also_walks_the_ladder() {
        let mut s = Session::new();
        s.connected(0);
        assert!(s.is_silent_dead(SILENCE_MS));
        assert_eq!(s.closed(), 1_000);
        s.connected(0);
        assert!(s.is_silent_dead(SILENCE_MS));
        assert_eq!(s.closed(), 2_000);
    }

    #[test]
    fn ui_and_data_are_counted_not_logged() {
        let mut s = Session::new();
        s.connected(0);
        s.line(&cat(2), 1);
        s.line(br#"{"t":"ui","ver":7}"#, 2);
        s.line(br#"{"t":"ui","ver":7}"#, 3);
        s.line(br#"{"t":"data","v":{"m0":1}}"#, 4);
        assert_eq!(s.counters.ui, 2);
        assert_eq!(s.counters.data, 1);
        assert_eq!(s.counters.other, 0, "known kinds never land in `other`");
    }

    #[test]
    fn cmd_is_its_own_kind_so_t6_cannot_be_blinded_by_the_counters() {
        let mut s = Session::new();
        s.connected(0);
        assert_eq!(s.line(br#"{"t":"cmd","action":"reset"}"#, 1), Line::Command(0));
        assert_eq!(s.counters.command, 1);
        assert_eq!(s.counters.other, 0);
    }

    #[test]
    fn duplicate_catalog_lines_are_cheap_and_stay_ready() {
        // PROTOCOL 搂16: on `hello` the daemon re-sends everything, so a second
        // catalog is normal after a reconnect, not an anomaly.
        let mut s = Session::new();
        s.connected(0);
        s.line(&cat(2), 1);
        assert_eq!(s.line(&cat(2), 2), Line::Catalog(2));
        assert_eq!(s.state(), State::Ready);
        assert_eq!(s.counters.catalog, 2);
    }
}
