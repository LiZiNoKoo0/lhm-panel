//! The daemon link: one TCP socket owned by one task, and a facade that lets
//! the render task see lines without ever awaiting.
//!
//! What this does NOT do: it does not understand `ui`/`data` (T6). It moves
//! bytes, asks `panel_core::session` what they mean, and sleeps for as long as
//! that session says -- the rules themselves live there, where they are host
//! tests instead of bench experiments. Where the daemon *is* comes from UDP
//! discovery (T5.5), asked only when the address is unknown or has just failed
//! twice, so the common case costs nothing.
//!
//! Why the socket lives here and not in the render task: `poll_line` is called
//! from the render loop every 5 ms, and awaiting anything there starves the
//! single-threaded executor -- the failure that cost this project a whole night
//! (BACKLOG #1, `Uart::read` blocking). So the flow is:
//!
//!     pump task  --recv-->  INBOUND  --try_receive-->  TcpLink (render task)
//!     pump task  <--send--  OUTBOUND <--try_send-----  TcpLink (render task)
//!
//! Neither channel call on the render side can block; a full queue drops a line
//! instead, which is the right trade for a display whose data arrives again in
//! a second anyway (PROTOCOL §16: keep the newest value, drop the middle).

use alloc::boxed::Box;

use embassy_net::tcp::{ConnectError, TcpSocket};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer, WithTimeout};
use heapless::Vec as HeaplessVec;

use core::sync::atomic::{AtomicBool, Ordering};
use panel_core::boot::{self, Step};
use panel_core::discover;
use panel_core::link::{LineBuf, LinkError, Transport};
use panel_core::auth;
use panel_core::session::{self, Session};
use panel_core::wire::{self, LINE_CAP};

use crate::board;
use crate::bootstate;
use crate::diag;
use crate::log::ChanLog;

/// Lines queued in each direction.
///
/// **This number is not free.** 2026-10-22, B-1: a queue of 8 costs 8,384 bytes
/// of `.bss` per direction, and on this chip the main stack is whatever DRAM is
/// left after `.bss` -- so a deep queue is paid for in stack. The pump build
/// brought both directions live (+11,952 bytes of statics) and the stack fell
/// from 22.9 KB to 11.2 KB, which is less than one WiFi association needs; the
/// board then reset in a loop with a stack-guard panic that pointed nowhere
/// near this file.
///
/// 3 is enough: the render task drains 6 lines per 5 ms tick, the daemon sends
/// at 1 Hz, and a full queue drops the *newest* line, which is exactly the
/// PROTOCOL §16 trade (keep the display alive, lose a value that arrives again
/// in a second). Deeper would only hide a stall -- now with a stack bill.
const QUEUE: usize = 3;

/// Longest line that can cross the channel: one protocol line plus slack.
type Line = HeaplessVec<u8, LINE_CAP>;

static INBOUND: Channel<CriticalSectionRawMutex, Line, QUEUE> = Channel::new();
static OUTBOUND: Channel<CriticalSectionRawMutex, Line, QUEUE> = Channel::new();

/// Mirrors the socket state for `is_up`. The render task cannot see the socket,
/// so this is the only way `Transport::is_up` can tell the truth.
static UP: AtomicBool = AtomicBool::new(false);

/// Socket buffers. 1 KB each: a protocol line is <= 1040 bytes and the pump
/// reads before it writes, so neither direction needs to hold a backlog.
const SOCK_BUF: usize = 1024;

/// How long one `read` may wait before the pump re-checks the outbound queue.
/// Deliberately short: outbound lines are typed by a human or produced at 1 Hz,
/// so 20 ms of added latency is invisible, while a long wait would make the
/// link feel dead the moment anything is sent.
const READ_SLICE: Duration = Duration::from_millis(20);

/// Association/connection budget for one attempt. Five seconds is short enough
/// that an unreachable daemon does not double the ladder's first rung, and long
/// enough for a sleeping router to wake up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether this build identifies itself to the peer.
///
/// The `bench-echo` build talks to a server that knows nothing about JSON and
/// echoes bytes back, so it must **not** send `hello` (the echo would come back
/// as a `hello` and the bench would prove the wrong thing). A `const` rather
/// than `#[cfg]` because cfg-gating the call made rustc call `send_hello` dead
/// code in that build, and "zero warnings" is a rule here.
const SEND_HELLO: bool = !cfg!(feature = "bench-echo");

/// The render task's view of the daemon link.
///
/// Cheap to construct and free of state: it is two channel handles and one
/// atomic. That is what makes "the board works with no daemon at all" free --
/// an idle `TcpLink` is a struct that reports `is_up() == false`.
pub struct TcpLink;

impl TcpLink {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for TcpLink {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for TcpLink {
    fn poll_line(&mut self, out: &mut [u8]) -> Result<Option<usize>, LinkError> {
        match INBOUND.try_receive() {
            Ok(line) => {
                let n = if line.len() < out.len() { line.len() } else { out.len() };
                out[..n].copy_from_slice(&line[..n]);
                Ok(Some(n))
            }
            Err(_) => Ok(None),
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        if !self.is_up() {
            // Same contract as a closed socket: the caller learns the link is
            // gone instead of filling a queue nobody will ever drain.
            return Err(LinkError::Closed);
        }
        let mut line = Line::new();
        line.extend_from_slice(bytes).map_err(|_| LinkError::Io)?;
        OUTBOUND.try_send(line).map_err(|_| LinkError::Timeout)
    }

    fn is_up(&self) -> bool {
        UP.load(Ordering::Relaxed)
    }
}

/// Connect, say `hello`, serve until the wire dies, back off, repeat -- forever.
///
/// One attempt of TCP connect + session, repeated forever.
///
/// `initial` is `None` in a shipped build: the address is then whatever UDP
/// discovery answers (`board::FALLBACK_PEER` if nothing answers at all, which
/// keeps a desk without a daemon responder working). The `bench-echo` build
/// passes a fixed bench address and skips discovery entirely.
#[embassy_executor::task]
pub async fn pump(stack: Stack<'static>, initial: Option<([u8; 4], u16)>, token: &'static str) -> ! {
    // Allocated once and reborrowed per attempt: `Box::leak` inside the loop
    // would lose 2 KB of a 96 KB heap on every reconnect.
    let rx_buf: &'static mut [u8] = Box::leak(Box::new([0u8; SOCK_BUF]));
    let tx_buf: &'static mut [u8] = Box::leak(Box::new([0u8; SOCK_BUF]));

    // One session for the lifetime of the task: it carries the failure count,
    // the watchdog and the per-kind counters across reconnects.
    let mut session = Session::new();
    let mut peer = initial;

    loop {
        // T5.5. Asked only when it can help: no address at all, or two
        // consecutive failures, which is what a moved DHCP lease looks like.
        // The failure count is `session`'s, so discovery and the backoff ladder
        // cannot disagree about what "failed" means -- and neither resets the
        // other, which keeps L-5 (only a catalog clears the ladder) intact.
        if board::DISCOVERY && discover::should_discover(peer.is_some(), session.failures()) {
            match discover_peer(stack).await {
                Some(here) => {
                    diag::disc_here(&mut ChanLog::new(), here.ip, here.port);
                    peer = Some(here.endpoint());
                }
                None => {
                    diag::disc_none(&mut ChanLog::new());
                    // Nothing answered. Keep whatever we had; on a cold start
                    // fall back to the desk address so the board still links
                    // on a network without a responder instead of staying dark.
                    if peer.is_none() {
                        peer = Some(board::FALLBACK_PEER);
                    }
                }
            }
        }

        // A shipped build always ends up with a peer (`FALLBACK_PEER` above);
        // the `bench-echo` build with discovery off may not have one at all.
        let (ip, port) = match peer {
            Some(endpoint) => endpoint,
            None => {
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };
        let remote = IpEndpoint::new(
            IpAddress::Ipv4(Ipv4Address::new(ip[0], ip[1], ip[2], ip[3])),
            port,
        );

        // The self-test row stays `...` while attempts are being made, so the
        // card reads as "working on it" instead of frozen.
        bootstate::announce(boot::LINK, Step::Busy);

        diag::link_tcp(&mut ChanLog::new(), ip, port);

        let mut socket = TcpSocket::new(stack, &mut *rx_buf, &mut *tx_buf);
        let mut fail_reason: Option<&'static str> = None;
        // Set once the connection has ended, so the ladder is advanced exactly
        // once per attempt (an argument for `Session::closed`).
        let mut ended: Option<End> = None;

        match socket.connect(remote).with_timeout(CONNECT_TIMEOUT).await {
            Ok(Ok(())) => {
                UP.store(true, Ordering::Relaxed);
                diag::link_up(&mut ChanLog::new());

                // Bench hook (Cargo.toml): one line as if the render task had
                // queued it, so the byte path alone is proven. The echo server
                // answers with the same bytes, which is exactly why this build
                // must not also send `hello`.
                #[cfg(feature = "bench-echo")]
                {
                    let mut line = Line::new();
                    let _ = line.extend_from_slice(b"PING\n");
                    let _ = OUTBOUND.try_send(line);
                }

                // SPEC L-3, sent before the first read: a daemon that speaks
                // first would otherwise get a terminal that never identifies
                // itself, and the silence would look like a dead wire.
                let action = session.connected(now_ms());
                let hello_ok = if SEND_HELLO && action == session::Action::SendHello {
                    send_hello(&mut socket).await
                } else {
                    true
                };

                if hello_ok {
                    ended = Some(serve(&mut socket, &mut session, token).await);
                } else {
                    fail_reason = Some("WriteFailed");
                    ended = Some(End::PeerClosed);
                }
                UP.store(false, Ordering::Relaxed);
            }
            Ok(Err(err)) => fail_reason = Some(connect_reason(err)),
            Err(_) => fail_reason = Some("Timeout"),
        }

        if let Some(reason) = fail_reason {
            diag::link_fail(&mut ChanLog::new(), reason);
        }

        // L-1 needs the silence in milliseconds for its own line, and it is
        // only right while the session is still up -- read it before closing.
        let silent_ms = session.silent_ms(now_ms());

        UP.store(false, Ordering::Relaxed);
        socket.abort();

        let wait_ms = match ended {
            Some(End::Silent) => {
                diag::link_silent(&mut ChanLog::new(), silent_ms);
                diag::link_lost(&mut ChanLog::new(), silent_ms);
                session.closed()
            }
            // The deny was already printed, with the daemon's own reason; it
            // hangs up itself, so there is nothing to add. Still a failure: the
            // ladder must climb, or a wrong token means one connect per second.
            Some(End::Denied) => session.closed(),
            Some(End::PeerClosed) => {
                diag::link_lost(&mut ChanLog::new(), silent_ms);
                session.closed()
            }
            // The connect itself never happened: same ladder, different line.
            None => session.connect_failed(),
        };
        diag::link_backoff(&mut ChanLog::new(), wait_ms, session.failures());
        Timer::after(Duration::from_millis(wait_ms as u64)).await;
    }
}

/// Ask the network where the daemon is: broadcast `discover`, listen for `here`.
///
/// One round only: three datagrams back to back, then 800 ms of listening. A
/// round costs under a second so a failed discovery delays a connect by less
/// than the ladder's first rung; the caller decides whether to ask at all.
///
/// The socket is created and dropped per round rather than kept alive. That
/// costs one stack slot for a moment -- and a long-lived socket would hold one
/// of the six `StackResources` sockets forever, which is the resource B-1
/// already made scarce.
async fn discover_peer(stack: Stack<'static>) -> Option<discover::Here> {
    // One packet in flight in each direction is the whole workload; the reply
    // is at most REPLY_CAP bytes and the request is 16.
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; discover::REPLY_CAP];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 32];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);

    // Port 0: let the stack pick an ephemeral source port. Binding the daemon's
    // own port number would make the reply arrive on a socket the daemon owns.
    if socket.bind(0).is_err() {
        return None;
    }

    // 255.255.255.255, not the subnet broadcast: the board must not assume it
    // knows the prefix, and the limited broadcast is what PROTOCOL §1 names.
    let dest = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), board::DAEMON_PORT);
    for _ in 0..discover::TRIES {
        if socket.send_to(discover::DISCOVER, dest).await.is_ok() {
            diag::disc_tx(&mut ChanLog::new());
        }
        // A gap between datagrams so the radio's TX queue drains; three copies
        // sent in one poll would go out together or not at all.
        Timer::after(Duration::from_millis(20)).await;
    }

    // A deadline, not a per-call timeout: a stray datagram that is not a usable
    // `here` must not restart the clock, or the round could last forever.
    let deadline = Instant::now() + Duration::from_millis(discover::REPLY_MS as u64);
    let found = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.as_ticks() <= 0 {
            break None;
        }
        // Anything that is not a usable `here` is skipped rather than ending the
        // round: a stray datagram must not cost the whole discovery.
        match socket
            .recv_from_with(|data, _meta| discover::parse_here(data))
            .with_timeout(left)
            .await
        {
            Ok(Some(here)) => break Some(here),
            Ok(None) => continue,
            Err(_) => break None,
        }
    };

    socket.close();
    found
}

/// The board's millisecond counter, as the session wants it (`u32`, wraps).
fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

/// Send `hello` + `\n` and say so. `false` means the socket died mid-write.
///
/// v0.6: one static string. The old build wrote head + token + tail in three
/// runs to keep a 1 KB buffer off the stack; with no key in `hello` there is
/// nothing to interpolate, so a single `write_out` of a `const` is both
/// shorter and cheaper. The key stays in flash and only ever appears as a mac.
async fn send_hello(socket: &mut TcpSocket<'_>) -> bool {
    let bytes = wire::HELLO.as_bytes();
    if !write_out(socket, bytes).await || !write_out(socket, b"\n").await {
        return false;
    }
    if socket.flush().await.is_err() {
        return false;
    }
    diag::link_hello_tx(&mut ChanLog::new(), bytes.len() + 1);
    true
}

/// Writes everything or reports the socket gone. Partial writes are retried
/// here rather than by the caller (`Transport::write_all`'s contract).
async fn write_out(socket: &mut TcpSocket<'_>, bytes: &[u8]) -> bool {
    let mut sent = 0usize;
    while sent < bytes.len() {
        match socket.write(&bytes[sent..]).await {
            Ok(0) => return false,
            Ok(n) => sent += n,
            Err(_) => return false,
        }
    }
    true
}

/// Why a session ended. Each variant maps to a different line, because "the
/// daemon hung up" and "the daemon went quiet" call for different digging.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    /// Read 0 / write error: the peer closed or the socket broke.
    PeerClosed,
    /// SPEC L-1: 15 s without a byte while the socket still looked open.
    Silent,
    /// The daemon said `deny` and is hanging up.
    Denied,
}

/// Move bytes until the connection dies, asking the session what each line was.
async fn serve(socket: &mut TcpSocket<'_>, session: &mut Session, key: &str) -> End {
    let mut lines: LineBuf<LINE_CAP> = LineBuf::new();
    let mut buf = [0u8; 256];
    // How many unknown kinds were printed *on this connection*. The session
    // keeps the honest total in `counters.other`; this only decides when to
    // stop printing, so an unknown 1 Hz stream cannot own the serial line.
    let mut unknown_logged: u32 = 0;

    loop {
        // Outbound first: a line produced by the render task should leave in
        // this pass, not after the next read slice.
        while let Ok(line) = OUTBOUND.try_receive() {
            if !write_out(socket, &line).await {
                return End::PeerClosed;
            }
            if socket.flush().await.is_err() {
                return End::PeerClosed;
            }
            diag::link_tx(&mut ChanLog::new(), &line);
        }

        match socket.read(&mut buf).with_timeout(READ_SLICE).await {
            Ok(Ok(0)) => return End::PeerClosed, // remote closed its half
            Ok(Ok(n)) => {
                for &byte in &buf[..n] {
                    if let Some(len) = lines.push(byte) {
                        let line = lines.line(len);
                        // Ask the session what this line is *once*, then act on
                        // the answer. Two calls would count the line twice.
                        let what = session.line(line, now_ms());
                        match handle_line(line, what, session, &mut unknown_logged) {
                            Handled::Ok => {}
                            Handled::Denied => return End::Denied,
                            Handled::Answer => {
                                if !answer_challenge(socket, key, line).await {
                                    return End::PeerClosed;
                                }
                            }
                        }
                    }
                }
            }
            // Read timeout: not an error, just an empty slice of time. The
            // socket keeps its own state because we never set a socket-level
            // timeout -- cancelling this future cannot reset anything.
            Ok(Err(_)) => return End::PeerClosed,
            Err(_) => {
                if session.is_silent_dead(now_ms()) {
                    return End::Silent;
                }
            }
        }
    }
}

enum Handled {
    Ok,
    Denied,
    /// The line was a `challenge`: the caller must send `auth`.
    Answer,
}

/// One line off the wire: let the session classify it, then say the one line
/// that classification deserves, then pass the line to the render task.
///
/// The split matters: this function decides what to *say* about a message and
/// never what to do with it. T6 owns the doing.
fn handle_line(
    line: &[u8],
    what: session::Line,
    session: &mut Session,
    unknown_logged: &mut u32,
) -> Handled {
    match what {
        session::Line::Challenge => {
            // Answered by the caller, which owns the socket and the key. The
            // session already moved to `Challenged`, so this is only the log.
            diag::link_challenge(&mut ChanLog::new());
            return Handled::Answer;
        }
        session::Line::Denied => {
            // Printed before returning: the daemon closes the socket right after
            // a deny, and the reason is the only thing that says why.
            let reason: &[u8] = wire::str_field(line, b"reason").unwrap_or(b"?");
            diag::link_deny(&mut ChanLog::new(), reason);
            bootstate::announce(boot::LINK, Step::Fail);
            return Handled::Denied;
        }
        session::Line::Pong => {
            // PROTOCOL 5.2: `ping` -> `pong`, queued on the same channel the
            // render task writes to, so bytes leave this firmware one way only.
            diag::link_ping(&mut ChanLog::new());
            forward(wire::PONG.as_bytes());
            forward(b"\n");
        }
        session::Line::Catalog(metrics) => {
            diag::link_catalog(
                &mut ChanLog::new(),
                metrics,
                wire::uint_field(line, b"ver").unwrap_or(0),
            );
            // Handshake done: the last self-test row settles here, not at
            // `LINK up`. A socket that opens and is then refused is not a
            // working link (L-5 says the same thing about the ladder).
            bootstate::announce(boot::LINK, Step::Ok);
        }
        // T6 acts on `cmd`; the session counts it so it cannot be lost in
        // "other" while nobody is looking.
        session::Line::Command(_) => {}
        session::Line::Ignore => match wire::message_kind(line) {
            // Not JSON: the T5.2 echo bench answering with the same bytes.
            None => diag::link_rx(&mut ChanLog::new(), line),
            Some(kind) if !wire::is_known_kind(kind) => {
                *unknown_logged = unknown_logged.saturating_add(1);
                if *unknown_logged <= 3 {
                    diag::link_unknown(&mut ChanLog::new(), kind, session.counters.other);
                }
            }
            // Known but uninteresting right now (`ui`, `data`): T6's business,
            // and one line each would let a 1 Hz stream own the UART.
            Some(_) => {}
        },
    }

    forward(line);
    Handled::Ok
}

/// Answer the daemon's nonce, or drop the link trying (PROTOCOL.md s.2.1).
///
/// Order is deliberate: check the daemon's own proof **first**. If `srv` does
/// not verify, the thing on the other end does not hold the key, so sending a
/// valid mac would be handing an impostor exactly what it came for. Drop the
/// link instead; the backoff ladder will retry.
///
/// `false` means the socket died, not "the daemon lied" -- a liar is handled by
/// closing, which the caller turns into `End::PeerClosed`.
async fn answer_challenge(socket: &mut TcpSocket<'_>, key: &str, line: &[u8]) -> bool {
    let nonce_raw = match wire::str_field(line, b"nonce") {
        Some(bytes) => bytes,
        None => {
            diag::link_srv_bad(&mut ChanLog::new());
            return false;
        }
    };
    let nonce = match core::str::from_utf8(nonce_raw) {
        Ok(text) => text,
        Err(_) => {
            diag::link_srv_bad(&mut ChanLog::new());
            return false;
        }
    };

    // A daemon that omits `srv` cannot be verified. v0.6 daemons always send it;
    // a daemon that does not is either old or lying, and this firmware has no
    // way to tell the difference, so it refuses to authenticate blind.
    let srv_raw = match wire::str_field(line, b"srv") {
        Some(bytes) => bytes,
        None => {
            diag::link_srv_bad(&mut ChanLog::new());
            return false;
        }
    };
    let srv = match core::str::from_utf8(srv_raw) {
        Ok(text) => text,
        Err(_) => {
            diag::link_srv_bad(&mut ChanLog::new());
            return false;
        }
    };
    if !auth::verify_srv(key, nonce, srv) {
        diag::link_srv_bad(&mut ChanLog::new());
        return false;
    }

    let mut mac = [0u8; auth::MAC_HEX];
    if !auth::mac_into(key, nonce, wire::CLIENT_ID, &mut mac) {
        diag::link_srv_bad(&mut ChanLog::new());
        return false;
    }
    let mut out = [0u8; auth::AUTH_LINE_CAP];
    let len = match auth::auth_into(&mac, &mut out) {
        Some(len) => len,
        None => return false,
    };
    if !write_out(socket, &out[..len]).await || !write_out(socket, b"\n").await {
        return false;
    }
    if socket.flush().await.is_err() {
        return false;
    }
    diag::link_auth_tx(&mut ChanLog::new(), len + 1);
    true
}

/// Hand one line to the render task, dropping it if nobody is draining.
fn forward(line: &[u8]) {
    let mut queued = Line::new();
    if queued.extend_from_slice(line).is_ok() {
        let _ = INBOUND.try_send(queued);
    }
}

/// `Debug` of `ConnectError` is already a stable variant name (`TimedOut`,
/// `NoRoute`, ...), which is the same rule `WIFI fail` follows: a name, never a
/// self-invented number.
fn connect_reason(err: ConnectError) -> &'static str {
    match err {
        ConnectError::InvalidState => "InvalidState",
        ConnectError::ConnectionReset => "ConnectionReset",
        ConnectError::TimedOut => "TimedOut",
        ConnectError::NoRoute => "NoRoute",
    }
}
