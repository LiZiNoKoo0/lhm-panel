//! Diagnostics that go up the wire: boot banner and the per-frame CRC line.
//!
//! What this does NOT do: no formatting of user data, no buffering, no retries.
//! Every line here is written once and forgotten -- if the link is down, the
//! information is lost on purpose, because a monitor that blocks on logging is
//! worse than one that is quiet.

use panel_core::link::{Transport, LINE_MAX};

/// Greeting before the panel is touched. Byte-identical to the pre-refactor
/// firmware so existing logs stay comparable.
pub fn banner(t: &mut dyn Transport) {
    let _ = t.write_all(b"\n=== st7735 160x80 live text / 8x16 dot matrix ===\n");
}

/// `stack: <n> bytes for the main task\n`
///
/// Printed on every boot because of B-1 (BACKLOG): on this chip `.data`, `.bss`
/// and the stack share one DRAM range, so **every static byte is a stack byte**.
/// A queue that grew by 12 KB moved the stack bottom up by 12 KB, the stack fell
/// to 11 KB, and WiFi association overflowed it; the panic that produced pointed
/// at a ROM address and named nothing in our code. One number per boot makes the
/// next occurrence a subtraction instead of an investigation.
pub fn stack_total(t: &mut dyn Transport, bytes: usize) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"stack: ");
    at += put_dec(&mut line[at..], bytes as u32);
    at += put(&mut line[at..], b" bytes for the main task\n");
    let _ = t.write_all(&line[..at]);
}

/// What the panel was actually initialised with. Values are passed in, never
/// hardcoded here: if board.rs changes, this line changes with it.
pub fn init_done(
    t: &mut dyn Transport,
    madctl: u8,
    off_x: u16,
    off_y: u16,
    invert: bool,
    spi_mhz: u32,
) {
    let mut line = [0u8; 96];
    let mut at = 0;
    at += put(&mut line[at..], b"init done: MADCTL=0x");
    at += put_hex_u8(&mut line[at..], madctl);
    at += put(&mut line[at..], b" OFF=(");
    at += put_dec(&mut line[at..], off_x as u32);
    at += put(&mut line[at..], b",");
    at += put_dec(&mut line[at..], off_y as u32);
    at += put(&mut line[at..], b") INVERT=");
    at += put(&mut line[at..], if invert { b"true" } else { b"false" });
    at += put(&mut line[at..], b" SPI=");
    at += put_dec(&mut line[at..], spi_mhz);
    at += put(&mut line[at..], b"MHz\n");
    let _ = t.write_all(&line[..at]);
}

/// `heap: 65536 free=64512  clock: 0ms\n`
///
/// One line, two claims, both falsifiable: `free` is measured while the probe
/// allocation is still held (so it only drops if the allocator really worked),
/// and `clock` is the millisecond counter read straight after boot.
pub fn heap_clock(t: &mut dyn Transport, heap_bytes: usize, heap_free: usize, clock_ms: u32) {
    let mut line = [0u8; 64];
    let mut at = 0;
    at += put(&mut line[at..], b"heap: ");
    at += put_dec(&mut line[at..], heap_bytes as u32);
    at += put(&mut line[at..], b" free=");
    at += put_dec(&mut line[at..], heap_free as u32);
    at += put(&mut line[at..], b"  clock: ");
    at += put_dec(&mut line[at..], clock_ms);
    at += put(&mut line[at..], b"ms\n");
    let _ = t.write_all(&line[..at]);
}

/// Only ever emitted when the heap self-check failed. Never part of a healthy
/// boot, so it cannot be mistaken for the normal line above.
pub fn heap_unverified(t: &mut dyn Transport) {
    let _ = t.write_all(b"heap: probe readback mismatch\n");
}

// -- wifi / store state lines (T3) --------------------------------------
//
// These are events, not a banner: each one is written once, when it happens,
// in the order it happens. Every line is pinned by a test, because these are
// the only window into a board that has no screen yet.

/// `WIFI sta ssid=<ssid>\n`
pub fn wifi_sta(t: &mut dyn Transport, ssid: &str) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line[..], b"WIFI sta ssid=");
    at += put_ssid(&mut line[at..], ssid);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `WIFI up ip=<a.b.c.d> rssi=<n>\n` -- rssi is signed, -30 is a good signal.
pub fn wifi_up(t: &mut dyn Transport, ip: [u8; 4], rssi: i8) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"WIFI up ip=");
    at += put_ipv4(&mut line[at..], ip);
    at += put(&mut line[at..], b" rssi=");
    at += put_dec_signed(&mut line[at..], rssi as i32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `STORE ok creds=yes|no [slot=A|B seq=<n>]` -- the flash was read, which
/// copy answered, and whether it held something to join with.
///
/// The slot and seq are the whole point of T4 being visible: without them, "the
/// save worked" and "the save wrote to the other sector and the board still
/// booted the old one" look identical on the wire.
pub fn store_ok(t: &mut dyn Transport, has_creds: bool, copy: Option<(u8, u32)>) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"STORE ok creds=");
    let yes_no: &[u8] = if has_creds { b"yes" } else { b"no" };
    at += put(&mut line[at..], yes_no);
    if let Some((slot, seq)) = copy {
        at += put(&mut line[at..], b" slot=");
        at += put(&mut line[at..], &[slot]);
        at += put(&mut line[at..], b" seq=");
        at += put_dec(&mut line[at..], seq);
    }
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `STORE migrate ok\n` -- the pre-T4 record was rewritten into the framed
/// layout. Printed because a flash write nobody asked for must not be silent.
pub fn store_migrated(t: &mut dyn Transport) {
    let _ = t.write_all(b"STORE migrate ok\n");
}

/// `WIFI init\n` -- printed *before* the radio is touched.
///
/// One bit, and it is the bit that was missing: a boot that ends at `READY`
/// says nothing about where it stopped. With this line, silence after `READY`
/// means the flash store blocked, and silence after `WIFI init` means
/// `esp_radio::wifi::new` did -- two very different bugs, and on the bench the
/// wire could not tell them apart.
pub fn wifi_init(t: &mut dyn Transport) {
    let _ = t.write_all(b"WIFI init\n");
}

/// `WIFI fail reason=<Name>\n` -- the name is the `WifiError` variant, not a number.///
/// The terminator is reserved *before* the reason is copied. `{:?}` on a driver
/// error can be longer than the line (`DisconnectedStationInfo { ssid: ... }`),
/// and a reason that eats the `\n` glues the next line onto this one: the bench
/// board printed exactly that, turning one truncation into a corrupt record.
/// So the tail of an over-long reason is what gets dropped, never the newline.
pub fn wifi_fail(t: &mut dyn Transport, reason: &str) {
    let mut line = [0u8; LINE_MAX];
    let mut at = put(&mut line, b"WIFI fail reason=");
    let room = line.len() - 1 - at;
    at += put_str(&mut line[at..at + room], reason);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `WIFI ap ssid=<ssid> ip=<a.b.c.d>\n`
pub fn wifi_ap(t: &mut dyn Transport, ssid: &str, ip: [u8; 4]) {
    let mut line = [0u8; 96];
    let mut at = put(&mut line, b"WIFI ap ssid=");
    at += put_ssid(&mut line[at..], ssid);
    at += put(&mut line[at..], b" ip=");
    at += put_ipv4(&mut line[at..], ip);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `DHCP up port=67\n`
///
/// The AP line on its own is not evidence that a client can get an address:
/// the beacon is the radio's, the lease is this firmware's, and only one of
/// those runs our code. This is the line that says the other half is alive.
pub fn dhcp_up(t: &mut dyn Transport) {
    let _ = t.write_all(b"DHCP up port=67\n");
}

/// `SCAN n=<count>\n` -- how many networks the config page could list.
///
/// Zero is a real answer and a worrying one: it is what you see when the radio
/// refuses to scan while the hotspot is up, and it used to be indistinguishable
/// from "your neighbour's SSID has a quote in it".
pub fn scan_n(t: &mut dyn Transport, count: usize) {
    let mut line = [0u8; 32];
    let mut at = put(&mut line[..], b"SCAN n=");
    at += put_dec(&mut line[at..], count as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `WIFI save ok ssid=<ssid>\n` -- written and read back byte-identical.
pub fn wifi_save_ok(t: &mut dyn Transport, ssid: &str) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"WIFI save ok ssid=");
    at += put_ssid(&mut line[at..], ssid);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `WIFI save bad reason=<Name>\n` -- nothing was kept, the board did not reset.
pub fn wifi_save_bad(t: &mut dyn Transport, reason: &str) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"WIFI save bad reason=");
    at += put_str(&mut line[at..], reason);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `STORE layout bad off=<n> len=<n>\n` -- the nvs partition is not where
/// board.rs says it is, so nothing was written. Better no creds than wrong ones.
pub fn store_layout_bad(t: &mut dyn Transport, off: u32, len: u32) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"STORE layout bad off=");
    at += put_dec(&mut line[at..], off);
    at += put(&mut line[at..], b" len=");
    at += put_dec(&mut line[at..], len);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

pub fn ready(t: &mut dyn Transport) {
    let _ = t.write_all(b"READY\n");
}

/// `FRAME 160x80 crc=0x........ bytes=25600\n`
///
/// This is the whole验收 mechanism of P1: the number is the CRC of the exact
/// bytes that were pushed, so the PC can prove the glass matches its preview
/// without a camera.
pub fn frame_line(t: &mut dyn Transport, w: usize, h: usize, crc: u32, bytes: usize) {
    let mut line = [0u8; 64];
    let mut at = 0;
    at += put(&mut line[at..], b"FRAME ");
    at += put_dec(&mut line[at..], w as u32);
    at += put(&mut line[at..], b"x");
    at += put_dec(&mut line[at..], h as u32);
    at += put(&mut line[at..], b" crc=0x");
    at += put_hex_u32(&mut line[at..], crc);
    at += put(&mut line[at..], b" bytes=");
    at += put_dec(&mut line[at..], bytes as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

// -- daemon link state lines (T5.2) -------------------------------------
//
// The TCP link has the same problem the WiFi state machine had: when it does
// not work, the screen says nothing and the wire says nothing, so there is
// nothing to bisect. Each of these lines marks one step that either happened
// or did not.

/// `BOOT <label> <status>\n` -- one line per self-test row as it settles.
///
/// Same reason the badge has a line: the boot card is painted by this board
/// alone, so without this the only evidence that a stage finished is somebody
/// looking at the glass. The words match `panel_core::boot::Step::text()`.
pub fn boot_step(t: &mut dyn Transport, label: &str, status: &str) {
    let mut line = [0u8; 48];
    let mut at = put(&mut line, b"BOOT ");
    let room = line.len() - 3 - at;
    at += put_str(&mut line[at..at + room], label);
    let room = line.len() - 3 - at;
    at += put(&mut line[at..at + room], b" ");
    let room = line.len() - 1 - at;
    at += put_str(&mut line[at..at + room], status);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `DISC tx\n` -- one UDP broadcast of `{"t":"discover"}` went out.
///
/// Printed per datagram, not per round: `DISC tx` twice before any `here` is
/// the honest picture of a lost packet, and a single `DISC tx` followed by
/// nothing says the radio never got a chance to send.
pub fn disc_tx(t: &mut dyn Transport) {
    let _ = t.write_all(b"DISC tx\n");
}

/// `DISC here ip=<a.b.c.d> port=<n>\n` -- a usable answer arrived.
pub fn disc_here(t: &mut dyn Transport, ip: [u8; 4], port: u16) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"DISC here ip=");
    at += put_ipv4(&mut line[at..], ip);
    at += put(&mut line[at..], b" port=");
    at += put_dec(&mut line[at..], port as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `DISC none\n` -- a full round of broadcasts produced no usable answer
/// (nothing arrived, or everything that arrived was rejected: a multicast or
/// loopback `ip`, a zero port).
pub fn disc_none(t: &mut dyn Transport) {
    let _ = t.write_all(b"DISC none\n");
}

/// `LINK tcp peer=<a.b.c.d>:<port>\n` -- about to attempt a connection.
pub fn link_tcp(t: &mut dyn Transport, peer: [u8; 4], port: u16) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"LINK tcp peer=");
    at += put_ipv4(&mut line[at..], peer);
    at += put(&mut line[at..], b":");
    at += put_dec(&mut line[at..], port as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK up\n` -- the socket is connected. Says nothing about the protocol:
/// a connection to a random port also reaches this line (that is the T5.2
/// point -- prove bytes before proving messages).
pub fn link_up(t: &mut dyn Transport) {
    let _ = t.write_all(b"LINK up\n");
}

/// `LINK fail reason=<Name>\n` -- same rule as `wifi_fail`: a name, never a
/// self-invented number that will drift from the library's next version.
pub fn link_fail(t: &mut dyn Transport, reason: &str) {
    let mut line = [0u8; LINE_MAX];
    let mut at = put(&mut line, b"LINK fail reason=");
    let room = line.len() - 1 - at;
    at += put_str(&mut line[at..at + room], reason);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK lost idle=<ms>\n` -- the socket is gone. `idle` is how long the wire
/// had been silent, which is what separates "the peer closed" (idle=0) from
/// "the peer stopped answering" (idle=15000+).
pub fn link_lost(t: &mut dyn Transport, idle_ms: u32) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"LINK lost idle=");
    at += put_dec(&mut line[at..], idle_ms);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK rx n=<len> text=<printable head>\n`
///
/// T5.2's whole acceptance is "a line I sent came back": length alone would be
/// satisfied by an echo server that answers with the wrong bytes. Non-printable
/// bytes become `.` so this stays ASCII and therefore pinnable by a test.
pub fn link_rx(t: &mut dyn Transport, bytes: &[u8]) {
    const SHOWN: usize = 40;
    let mut line = [0u8; 96];
    let mut at = put(&mut line, b"LINK rx n=");
    at += put_dec(&mut line[at..], bytes.len() as u32);
    at += put(&mut line[at..], b" text=");
    let shown = if bytes.len() < SHOWN { bytes.len() } else { SHOWN };
    for &byte in &bytes[..shown] {
        let ch = if (0x20..0x7f).contains(&byte) { byte } else { b'.' };
        at += put(&mut line[at..], &[ch]);
    }
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK tx n=<len>\n` -- a line was handed to the socket. Separate from `up`
/// because a connection that accepts bytes and silently drops them looks
/// exactly like a healthy one until the peer is checked.
pub fn link_tx(t: &mut dyn Transport, bytes: &[u8]) {
    let mut line = [0u8; 48];
    let mut at = put(&mut line, b"LINK tx n=");
    at += put_dec(&mut line[at..], bytes.len() as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

// -- daemon protocol lines (T5.3) ---------------------------------------
//
// `LINK up` says a socket exists; these say what the daemon made of us. They
// are the only way to tell "the daemon refused our token" from "the daemon
// accepted us and then had nothing to say" -- both are `LINK up` followed by
// silence on the wire.

/// `LINK hello tx n=<len>\n` -- `hello` went out on a *fresh* connection.
///
/// Separated from `LINK tx` because SPEC L-3 says hello is re-sent on every
/// reconnect: this line is what makes "did it really?" checkable in a log with
/// five reconnects in it.
pub fn link_hello_tx(t: &mut dyn Transport, bytes: usize) {
    let mut line = [0u8; 48];
    let mut at = put(&mut line, b"LINK hello tx n=");
    at += put_dec(&mut line[at..], bytes as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK catalog n=<metrics> ver=<n>\n` -- the daemon answered hello with a
/// catalog. This is the line that means "the protocol handshake completed";
/// `n` is counted from the line itself (`wire::count_ids`), not from anything
/// the daemon claimed.
pub fn link_catalog(t: &mut dyn Transport, metrics: usize, ver: u32) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"LINK catalog n=");
    at += put_dec(&mut line[at..], metrics as u32);
    at += put(&mut line[at..], b" ver=");
    at += put_dec(&mut line[at..], ver);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK deny reason=<reason>\n` -- the daemon refused the hello and will hang
/// up. The reason is the daemon's own words (`bad token`, `proto mismatch`),
/// never a code of our invention: the fix differs per reason, and a number
/// would only be re-decoded wrong.
pub fn link_deny(t: &mut dyn Transport, reason: &[u8]) {
    let mut line = [0u8; LINE_MAX];
    let mut at = put(&mut line, b"LINK deny reason=");
    let room = line.len() - 1 - at;
    at += put(&mut line[at..at + room], reason);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK ping\n` -- a `ping` arrived and a `pong` was queued behind it.
///
/// The one message this board answers, and therefore the cheapest proof that
/// the protocol path is bidirectional rather than "daemon talks, board listens".
pub fn link_ping(t: &mut dyn Transport) {
    let _ = t.write_all(b"LINK ping\n");
}

/// `LINK unknown kind=<name> n=<count>\n` -- a message kind nobody here knows
/// (W7). Printed for the first few per connection only: an unknown 1 Hz stream
/// must not own the serial line, but the first occurrence must not be silent.
pub fn link_unknown(t: &mut dyn Transport, kind: &[u8], seen: u32) {
    const SHOWN: usize = 24;
    let mut line = [0u8; 80];
    let mut at = put(&mut line, b"LINK unknown kind=");
    let shown = if kind.len() < SHOWN { kind.len() } else { SHOWN };
    for &byte in &kind[..shown] {
        let ch = if (0x20..0x7f).contains(&byte) { byte } else { b'.' };
        at += put(&mut line[at..], &[ch]);
    }
    at += put(&mut line[at..], b" n=");
    at += put_dec(&mut line[at..], seen);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK silent ms=<n>\n` -- SPEC L-1: nothing arrived for 15 s while the
/// socket still looked open, so the socket is dropped and hello will be re-sent
/// on the next one. This is *not* `LINK lost`: there the peer hung up, here it
/// said nothing, which is what a slept WiFi router or a hung daemon looks like.
pub fn link_silent(t: &mut dyn Transport, ms: u32) {
    let mut line = [0u8; 48];
    let mut at = put(&mut line, b"LINK silent ms=");
    at += put_dec(&mut line[at..], ms);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `LINK backoff ms=<n> tries=<n>\n` -- SPEC L-2: the delay before the next
/// attempt, and how many consecutive failures bought it. Printed before the
/// sleep, so a board that goes off the air still leaves the ladder in the log.
pub fn link_backoff(t: &mut dyn Transport, ms: u32, tries: u32) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"LINK backoff ms=");
    at += put_dec(&mut line[at..], ms);
    at += put(&mut line[at..], b" tries=");
    at += put_dec(&mut line[at..], tries);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `BADGE on|off\n` -- the `LINK LOST` overlay appeared or was removed.
///
/// The badge is the only pixels this board adds on its own initiative, and its
/// dangerous failure is not "never drawn" but "never removed": a stuck badge
/// tells the user the values are stale while they are not. Without this line
/// that state is only visible by looking at the glass, which cannot be done from
/// a script (2026-10-23, T5.4).
pub fn badge(t: &mut dyn Transport, on: bool) {
    let _ = t.write_all(if on { b"BADGE on\n" } else { b"BADGE off\n" });
}

/// `PAGE ui scene=<n> ver=<v> nodes=<n>\n` -- a layout was accepted and is on
/// the glass from this frame on.
///
/// The board has to say this: `ui` arrives on a socket the PC opened, and a
/// daemon that thinks it sent a page while the panel still shows the self-test
/// card is the exact failure a screenshot cannot distinguish from "wrong page".
pub fn page_ui(t: &mut dyn Transport, scene: u8, ver: u32, nodes: usize) {
    let mut line = [0u8; 64];
    let mut at = put(&mut line, b"PAGE ui scene=");
    at += put_dec(&mut line[at..], scene as u32);
    at += put(&mut line[at..], b" ver=");
    at += put_dec(&mut line[at..], ver);
    at += put(&mut line[at..], b" nodes=");
    at += put_dec(&mut line[at..], nodes as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `PAGE data n=<keys>\n` -- how many readings this round moved.
///
/// Only the count, not the values: they are already on the glass, and printing
/// them would double the UART traffic at 1 Hz for no extra proof.
pub fn page_data(t: &mut dyn Transport, keys: usize) {
    let mut line = [0u8; 32];
    let mut at = put(&mut line, b"PAGE data n=");
    at += put_dec(&mut line[at..], keys as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `PAGE restored scene=<n> ver=<v> nodes=<n>\n` -- a page came back from flash
/// at boot, before any daemon existed.
///
/// Said out loud because "the panel drew something" is otherwise
/// indistinguishable from "the daemon was quick": the whole point of T6c is that
/// the picture is there *without* a PC, and only a line that names the source
/// can prove which happened.
pub fn page_restored(t: &mut dyn Transport, scene: u8, ver: u32, nodes: usize) {
    let mut line = [0u8; 56];
    let mut at = put(&mut line, b"PAGE restored scene=");
    at += put_dec(&mut line[at..], scene as u32);
    at += put(&mut line[at..], b" ver=");
    at += put_dec(&mut line[at..], ver);
    at += put(&mut line[at..], b" nodes=");
    at += put_dec(&mut line[at..], nodes as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `PAGE saved ver=<v>\n` -- the page is on flash and will come back next boot.
pub fn page_saved(t: &mut dyn Transport, ver: u32) {
    let mut line = [0u8; 32];
    let mut at = put(&mut line, b"PAGE saved ver=");
    at += put_dec(&mut line[at..], ver);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `PAGE save-failed ver=<v>\n` -- the page is on the glass but not on flash.
///
/// A separate word from `saved`, because the failure mode is silent otherwise:
/// the user sees the new page, reboots, and gets the old one with no clue why.
pub fn page_save_failed(t: &mut dyn Transport, ver: u32) {
    let mut line = [0u8; 40];
    let mut at = put(&mut line, b"PAGE save-failed ver=");
    at += put_dec(&mut line[at..], ver);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `PAGE reject <reason>\n` -- a `ui`/`data` line was refused; the previous
/// page stays. A refused page that repainted would look like a layout bug, so
/// the board says out loud that it declined instead.
pub fn page_reject(t: &mut dyn Transport, reason: &str) {
    let mut line = [0u8; 48];
    let mut at = put(&mut line, b"PAGE reject ");
    let room = line.len() - 1 - at;
    at += put_str(&mut line[at..at + room], reason);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

/// `COLOURS left=0xF800 mid=0x07E0 right=0x001F madctl=0x60 inv=<0|1>\n`
///
/// Bench-only (`bench-colours` feature). The three bars are the only way to tell
/// RGB from BGR on a panel whose white is white in both orders: 0xF800 is red
/// *if* the glass is RGB. This line says what was written; the glass says what
/// was understood, and the difference between the two is the bug.
// Only the `bench-colours` build calls this, but it is compiled unconditionally
// on purpose: panel-core's `diag_format` test `include!`s this file, and a
// feature name panel-core does not know would trip `unexpected_cfgs` there.
#[allow(dead_code)]
pub fn colours(t: &mut dyn Transport, madctl: u8, invert: bool) {
    let mut line = [0u8; 96];
    let mut at = put(&mut line, b"COLOURS left=0xF800 mid=0x07E0 right=0x001F madctl=0x");
    at += put_hex_u8(&mut line[at..], madctl);
    at += put(&mut line[at..], if invert { b" inv=1\n" } else { b" inv=0\n" });
    let _ = t.write_all(&line[..at]);
}

/// `TOKEN ok len=<n>` / `TOKEN empty\n` -- whether this board has an auth token.
///
/// Length only, never the value: it is a secret, and the point of the line is to
/// answer "will a strict daemon accept me?" from a log, not to print the key.
/// An empty token means every strict daemon answers `bad token` (T5.6).
// Only the firmware calls this, but it is compiled unconditionally on purpose:
// panel-core's `diag_format` test `include!`s this file, and gating it by a
// feature panel-core does not know would trip `unexpected_cfgs` there.
#[allow(dead_code)]
pub fn token_state(t: &mut dyn Transport, has_token: bool, len: usize) {
    let mut line = [0u8; 48];
    let mut at = if has_token {
        put(&mut line, b"TOKEN ok len=")
    } else {
        put(&mut line, b"TOKEN empty")
    };
    if has_token {
        at += put_dec(&mut line[at..], len as u32);
    }
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

// -- tiny formatting helpers (no alloc, no fmt machinery) ---------------

fn put(dst: &mut [u8], src: &[u8]) -> usize {
    let n = if src.len() < dst.len() { src.len() } else { dst.len() };
    dst[..n].copy_from_slice(&src[..n]);
    n
}

/// Truncating copy of a `&str`. An over-long SSID is cut, never panicked on:
/// a diagnostics line must not be able to kill the boot.
fn put_str(dst: &mut [u8], src: &str) -> usize {
    put(dst, src.as_bytes())
}

/// Same, but with the protocol's 32-byte SSID limit. Cutting here rather than
/// at the line buffer keeps the output stable no matter how long the input is.
fn put_ssid(dst: &mut [u8], src: &str) -> usize {
    let bytes = src.as_bytes();
    let n = if bytes.len() < 32 { bytes.len() } else { 32 };
    put(dst, &bytes[..n])
}

fn put_ipv4(dst: &mut [u8], ip: [u8; 4]) -> usize {
    let mut at = 0;
    for (i, octet) in ip.iter().enumerate() {
        if i > 0 {
            at += put(&mut dst[at..], b".");
        }
        at += put_dec(&mut dst[at..], *octet as u32);
    }
    at
}

/// Two's complement handled explicitly: rssi arrives negative and `put_dec`
/// only knows unsigned.
fn put_dec_signed(dst: &mut [u8], value: i32) -> usize {
    if value < 0 {
        let mut at = put(dst, b"-");
        // i32::MIN would overflow on negation; rssi never comes near it, but
        // saturating keeps this total instead of panicking in release.
        at += put_dec(&mut dst[at..], value.unsigned_abs());
        at
    } else {
        put_dec(dst, value as u32)
    }
}

fn put_dec(dst: &mut [u8], mut value: u32) -> usize {
    let mut tmp = [0u8; 10];
    let mut at = tmp.len();
    if value == 0 {
        at -= 1;
        tmp[at] = b'0';
    }
    while value > 0 {
        at -= 1;
        tmp[at] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    put(dst, &tmp[at..])
}

fn put_hex_u32(dst: &mut [u8], value: u32) -> usize {
    let mut at = 0;
    for shift in [28u32, 24, 20, 16, 12, 8, 4, 0] {
        let nibble = ((value >> shift) & 0xF) as u8;
        at += put(
            &mut dst[at..],
            &[if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 }],
        );
    }
    at
}

fn put_hex_u8(dst: &mut [u8], value: u8) -> usize {
    let mut at = 0;
    for shift in [4u32, 0] {
        let nibble = ((value >> shift) & 0xF) as u8;
        at += put(
            &mut dst[at..],
            &[if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 }],
        );
    }
    at
}
