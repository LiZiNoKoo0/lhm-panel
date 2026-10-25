//! The wire text must not move during refactors either.
//!
//! `diag.rs` lives in the BSP crate (it writes to a real transport), but its
//! content is pure formatting. Including it here lets a plain `cargo test` on
//! the PC pin down the exact bytes -- otherwise "the banner still looks fine"
//! is the best we can do, and that is exactly the kind of claim this project
//! refuses to make about pixels or about logs.

use panel_core::link::{LinkError, Transport};

#[path = "../../st7735-text/src/diag.rs"]
mod diag;

struct Sink(Vec<u8>);

impl Transport for Sink {
    fn poll_line(&mut self, _out: &mut [u8]) -> Result<Option<usize>, LinkError> {
        Ok(None)
    }
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn is_up(&self) -> bool {
        true
    }
}

fn text_of(f: impl FnOnce(&mut dyn Transport)) -> String {
    let mut sink = Sink(Vec::new());
    f(&mut sink);
    String::from_utf8(sink.0).expect("diagnostics must stay ASCII")
}

#[test]
fn banner_is_unchanged() {
    assert_eq!(
        text_of(|t| diag::banner(t)),
        "\n=== st7735 160x80 live text / 8x16 dot matrix ===\n"
    );
}

#[test]
fn init_line_is_unchanged() {
    assert_eq!(
        text_of(|t| diag::init_done(t, 0x60, 0, 24, false, 20)),
        "init done: MADCTL=0x60 OFF=(0,24) INVERT=false SPI=20MHz\n"
    );
}

#[test]
fn ready_line_is_unchanged() {
    assert_eq!(text_of(|t| diag::ready(t)), "READY\n");
}

#[test]
fn heap_clock_line_reports_both_capabilities() {
    // Pasted from the acceptance run on the bench board (T2): the free figure is
    // 64 KB minus the 1 KB probe while it is still held.
    assert_eq!(
        text_of(|t| diag::heap_clock(t, 65536, 64512, 0)),
        "heap: 65536 free=64512  clock: 0ms\n"
    );
    // two spaces before `clock:` on purpose: the columns line up in a log
    assert!(!text_of(|t| diag::heap_clock(t, 65536, 64512, 0)).contains("   clock"));
    // and the wrap case still has to fit the 64 byte line buffer
    assert_eq!(
        text_of(|t| diag::heap_clock(t, 65536, 1, u32::MAX)),
        "heap: 65536 free=1  clock: 4294967295ms\n"
    );
}

#[test]
fn heap_failure_line_is_distinguishable() {
    // A healthy boot never prints this, so its shape only has to be impossible
    // to confuse with the line above.
    assert_eq!(
        text_of(|t| diag::heap_unverified(t)),
        "heap: probe readback mismatch\n"
    );
}

#[test]
fn frame_line_is_unchanged_and_lowercase() {
    assert_eq!(
        text_of(|t| diag::frame_line(t, 160, 80, 0x0046_644B, 25600)),
        "FRAME 160x80 crc=0x0046644b bytes=25600\n"
    );
    // all-nibbles case, to catch a missing leading zero or an upper case hex
    assert_eq!(
        text_of(|t| diag::frame_line(t, 160, 80, 0xFFFF_FFFF, 25600)),
        "FRAME 160x80 crc=0xffffffff bytes=25600\n"
    );
    assert_eq!(
        text_of(|t| diag::frame_line(t, 160, 80, 0x0000_0000, 25600)),
        "FRAME 160x80 crc=0x00000000 bytes=25600\n"
    );
}

// -- T3: wifi / store state lines ---------------------------------------
//
// These seven lines are the only window into a board mid-provisioning: no
// screen, no debugger. A field name that drifts or an `ssid=` that loses its
// space is invisible on the bench and painful in a log, so each one is pinned
// as exact text, not as "contains".

#[test]
fn wifi_sta_line_is_pinned() {
    assert_eq!(
        text_of(|t| diag::wifi_sta(t, "mywifi")),
        "WIFI sta ssid=mywifi\n"
    );
}

#[test]
fn wifi_up_line_pins_ip_and_signed_rssi() {
    assert_eq!(
        text_of(|t| diag::wifi_up(t, [192, 168, 4, 1], -55)),
        "WIFI up ip=192.168.4.1 rssi=-55\n"
    );
}

#[test]
fn store_ok_line_says_which_copy_answered() {
    // Slot and seq are what makes T4 visible on the wire: without them a save
    // that landed in the other sector and a save that never happened print the
    // same thing.
    assert_eq!(
        text_of(|t| diag::store_ok(t, true, Some((b'A', 1)))),
        "STORE ok creds=yes slot=A seq=1\n"
    );
    assert_eq!(
        text_of(|t| diag::store_ok(t, true, Some((b'B', 42)))),
        "STORE ok creds=yes slot=B seq=42\n"
    );
    // Nothing usable in either sector: no slot to name.
    assert_eq!(
        text_of(|t| diag::store_ok(t, false, None)),
        "STORE ok creds=no\n"
    );
    // A frame that parses but whose payload will not decode still names the copy:
    // that is a corrupt record, not a blank chip, and the difference matters.
    assert_eq!(
        text_of(|t| diag::store_ok(t, false, Some((b'A', 7)))),
        "STORE ok creds=no slot=A seq=7\n"
    );
}

#[test]
fn store_migrate_line_is_not_silent() {
    assert_eq!(text_of(|t| diag::store_migrated(t)), "STORE migrate ok\n");
}

#[test]
fn wifi_init_line_marks_the_radio_handover() {
    // The one line that separates "the flash store blocked" from
    // "esp_radio::wifi::new blocked" on the wire.
    assert_eq!(text_of(|t| diag::wifi_init(t)), "WIFI init\n");
}

#[test]
fn wifi_fail_line_carries_the_variant_name_not_a_number() {
    // `reason=` takes the `WifiError` Debug name; a numeric code would not
    // survive a driver upgrade, a name does.
    assert_eq!(
        text_of(|t| diag::wifi_fail(t, "ConnectionFailed")),
        "WIFI fail reason=ConnectionFailed\n"
    );

    // A driver error debug-printed in full is far longer than one line. The
    // bench board once printed
    //
    //     WIFI fail reason=Disconnected(DisconnectedStationInfo { ssid: "L
    //     WIFI ap ssid=st7735-c758 ip=192.168.4.1
    //
    // -- the reason ate the terminator and the next line was glued on, so the
    // log had no `WIFI fail` record at all, just a corrupt one. Whatever else
    // is lost, the shape has to survive.
    let long = format!(
        "Disconnected(DisconnectedStationInfo {{ ssid: \"{}\", bssid: [1, 2, 3, 4, 5, 6] }})",
        "LiZiNoKoo".repeat(20)
    );
    let cut = text_of(|t| diag::wifi_fail(t, &long));
    assert!(cut.starts_with("WIFI fail reason=Disconnected("));
    assert!(cut.ends_with('\n'), "the terminator must never be dropped");
    assert_eq!(cut.matches('\n').count(), 1, "exactly one line, no gluing");
    assert_eq!(cut.len(), panel_core::link::LINE_MAX, "one full slot, terminator included");
    assert!(!cut.contains("bssid"), "an over-long reason loses its tail");}

#[test]
fn wifi_ap_line_is_pinned() {
    // The bench board's real AP name, MAC suffix and all.
    assert_eq!(
        text_of(|t| diag::wifi_ap(t, "st7735-c758", [192, 168, 4, 1])),
        "WIFI ap ssid=st7735-c758 ip=192.168.4.1\n"
    );
}

#[test]
fn wifi_save_lines_separate_ok_from_bad() {
    assert_eq!(
        text_of(|t| diag::wifi_save_ok(t, "mywifi")),
        "WIFI save ok ssid=mywifi\n"
    );
    assert_eq!(
        text_of(|t| diag::wifi_save_bad(t, "TooLong")),
        "WIFI save bad reason=TooLong\n"
    );
    // `ok` and `bad` differ by two characters: a typo here would silently turn
    // a failed save into a success in the log.
    let ok = text_of(|t| diag::wifi_save_ok(t, "mywifi"));
    let bad = text_of(|t| diag::wifi_save_bad(t, "mywifi"));
    assert_ne!(ok, bad);
    assert!(ok.starts_with("WIFI save ok "));
    assert!(bad.starts_with("WIFI save bad reason="));
}

#[test]
fn store_layout_line_is_pinned_in_decimal() {
    // Real numbers from the T3 partition table: nvs at 0x9000, 24 KB long.
    // Hex here would be a second unit the reader has to convert in their head.
    assert_eq!(
        text_of(|t| diag::store_layout_bad(t, 36864, 24576)),
        "STORE layout bad off=36864 len=24576\n"
    );
}

#[test]
fn dhcp_line_says_the_lease_side_is_up() {
    // The AP line is emitted by the radio; this is the only line that proves the
    // DHCP server -- our code, on our executor -- is actually running. It is the
    // line that was missing for a whole evening, so it stays byte-stable.
    assert_eq!(text_of(|t| diag::dhcp_up(t)), "DHCP up port=67\n");
}

#[test]
fn scan_line_reports_the_count_including_zero() {
    // Zero is the interesting value: it is what a refused scan looks like on the
    // phone (an empty list), so it must print as a number and not as nothing.
    assert_eq!(text_of(|t| diag::scan_n(t, 0)), "SCAN n=0\n");
    assert_eq!(text_of(|t| diag::scan_n(t, 16)), "SCAN n=16\n");
}

#[test]
fn over_long_ssid_is_cut_to_the_protocol_limit() {
    // 40 bytes in, 32 bytes out: the SSID limit is 32 and a status line must
    // never overflow its buffer or print a half-cut field differently.
    let long = "x".repeat(40);
    let expected = std::format!("WIFI sta ssid={}\n", "x".repeat(32));
    assert_eq!(text_of(|t| diag::wifi_sta(t, &long)), expected);
    assert_eq!(text_of(|t| diag::wifi_sta(t, &long)).len(), 14 + 32 + 1);

    // Same cut on the AP and save lines, which share the helper.
    assert_eq!(
        text_of(|t| diag::wifi_ap(t, &long, [192, 168, 4, 1])),
        std::format!("WIFI ap ssid={} ip=192.168.4.1\n", "x".repeat(32))
    );
    assert_eq!(
        text_of(|t| diag::wifi_save_ok(t, &long)),
        std::format!("WIFI save ok ssid={}\n", "x".repeat(32))
    );
}

#[test]
fn rssi_extremes_print_without_a_leading_plus_or_a_wrap() {
    // i8 spans -128..=127 and the formatter works on the two's complement
    // directly, so the two ends are where a sign bug would show.
    assert_eq!(
        text_of(|t| diag::wifi_up(t, [10, 0, 0, 7], -128)),
        "WIFI up ip=10.0.0.7 rssi=-128\n"
    );
    assert_eq!(
        text_of(|t| diag::wifi_up(t, [10, 0, 0, 7], 0)),
        "WIFI up ip=10.0.0.7 rssi=0\n"
    );
    // A good signal still keeps its sign.
    assert_eq!(
        text_of(|t| diag::wifi_up(t, [10, 0, 0, 7], -30)),
        "WIFI up ip=10.0.0.7 rssi=-30\n"
    );
}

#[test]
fn stack_line_reports_the_number_that_nearly_killed_the_boot() {
    // B-1: on this chip every static byte is a stack byte, so one number per
    // boot turns the next occurrence into a subtraction. Pinned here because
    // the line is only ever produced by the firmware -- without this test the
    // formatter can rot silently until the day it is needed.
    assert_eq!(
        text_of(|t| diag::stack_total(t, 21_772)),
        "stack: 21772 bytes for the main task\n"
    );
}

// -- T5.2: daemon link lines -------------------------------------------

#[test]
fn link_lines_name_their_step() {
    // Each of these marks one step that either happened or did not, which is
    // the only way to bisect a link that fails without a screen to look at.
    assert_eq!(
        text_of(|t| diag::link_tcp(t, [192, 168, 1, 100], 7802)),
        "LINK tcp peer=192.168.1.100:7802\n"
    );
    assert_eq!(text_of(|t| diag::link_up(t)), "LINK up\n");
    assert_eq!(
        text_of(|t| diag::link_fail(t, "TimedOut")),
        "LINK fail reason=TimedOut\n"
    );
    assert_eq!(
        text_of(|t| diag::link_lost(t, 15_000)),
        "LINK lost idle=15000\n"
    );
    assert_eq!(text_of(|t| diag::link_tx(t, b"PING\n")), "LINK tx n=5\n");
}

#[test]
fn link_rx_shows_the_bytes_not_just_the_length() {
    // Length alone would be satisfied by an echo that answers with the wrong
    // bytes, so the printable head is part of the line.
    assert_eq!(
        text_of(|t| diag::link_rx(t, b"PING")),
        "LINK rx n=4 text=PING\n"
    );
    // Non-printable bytes become '.' so this stays ASCII and therefore pinnable.
    assert_eq!(
        text_of(|t| diag::link_rx(t, b"a\x00b\xffc")),
        "LINK rx n=5 text=a.b.c\n"
    );
    // The head is capped: a 1 KB `ui` must not push the line over LINE_MAX.
    let long = vec![b'x'; 900];
    let text = text_of(|t| diag::link_rx(t, &long));
    assert_eq!(text, format!("LINK rx n=900 text={}\n", "x".repeat(40)));
    assert!(text.len() < panel_core::link::LINE_MAX);
}

#[test]
fn link_fail_keeps_its_newline() {
    // Same rule as `WIFI fail`: the tail of an over-long reason is dropped,
    // never the terminator. A line without `\n` is not a short line, it is a
    // corrupt record followed by a glued-on one.
    let long = "X".repeat(400);
    let text = text_of(|t| diag::link_fail(t, &long));
    assert!(text.ends_with('\n'));
    assert_eq!(text.matches('\n').count(), 1);
    assert!(text.len() <= panel_core::link::LINE_MAX);
}

// -- T5.3: connection state machine lines ------------------------------

#[test]
fn link_handshake_lines_are_pinned() {
    // `hello tx` is not `LINK tx`: SPEC L-3 makes the hello a per-connection
    // event, and a log with five reconnects in it has to show five of these.
    assert_eq!(
        text_of(|t| diag::link_hello_tx(t, 341)),
        "LINK hello tx n=341\n"
    );
    // The catalog line is the "handshake completed" marker, and both numbers
    // come from the message itself: count and version, not a claim.
    assert_eq!(
        text_of(|t| diag::link_catalog(t, 6, 3)),
        "LINK catalog n=6 ver=3\n"
    );
    assert_eq!(text_of(|t| diag::link_ping(t)), "LINK ping\n");
}

#[test]
fn link_deny_carries_the_daemons_own_words() {
    // The daemon's reason, verbatim: the fix for `bad token` and the fix for
    // `proto mismatch` are different, and a code of our own invention would
    // only be decoded wrong later.
    assert_eq!(
        text_of(|t| diag::link_deny(t, b"bad token")),
        "LINK deny reason=bad token\n"
    );
    assert_eq!(
        text_of(|t| diag::link_deny(t, b"?")),
        "LINK deny reason=?\n"
    );
}

#[test]
fn link_unknown_prints_a_bounded_kind_and_keeps_its_newline() {
    assert_eq!(
        text_of(|t| diag::link_unknown(t, b"widget", 1)),
        "LINK unknown kind=widget n=1\n"
    );
    // A kind is attacker-adjacent data from the wire: non-printable becomes
    // '.', and a very long one is cut rather than allowed to push the line
    // over LINE_MAX (which would glue the next line onto it).
    assert_eq!(
        text_of(|t| diag::link_unknown(t, b"a\x00\xff", 2)),
        "LINK unknown kind=a.. n=2\n"
    );
    let text = text_of(|t| diag::link_unknown(t, &[b'x'; 200], 3));
    assert_eq!(text, format!("LINK unknown kind={} n=3\n", "x".repeat(24)));
    assert!(text.len() <= panel_core::link::LINE_MAX);
}

#[test]
fn badge_line_says_which_way_the_overlay_went() {
    // T5.4. Two states, two lines: the failure that matters is a badge that is
    // painted and never removed, and this is the only evidence a script can get.
    assert_eq!(text_of(|t| diag::badge(t, true)), "BADGE on\n");
    assert_eq!(text_of(|t| diag::badge(t, false)), "BADGE off\n");
}

#[test]
fn page_lines_say_what_the_page_did() {
    // T6. The board paints whatever arrives, so the only way to tell "the daemon
    // sent nothing" from "the page was refused" is this pair of lines. `scene`
    // and `ver` are the daemon's own numbers, echoed back unchanged.
    assert_eq!(
        text_of(|t| diag::page_ui(t, 2, 17, 10)),
        "PAGE ui scene=2 ver=17 nodes=10\n"
    );
    assert_eq!(text_of(|t| diag::page_data(t, 4)), "PAGE data n=4\n");
    assert_eq!(text_of(|t| diag::page_data(t, 0)), "PAGE data n=0\n");
}

#[test]
fn page_reject_names_the_reason_in_words() {
    // Each `ui::Error` must reach the log as a distinct ASCII word: two
    // different refusals printing the same thing would send the next debugging
    // session in the wrong direction (`too-many-nodes` vs `unsupported` is the
    // pair most likely to be confused).
    use panel_core::ui::Error;
    let all = [
        (Error::NotUi, "not-ui"),
        (Error::NoNodes, "no-nodes"),
        (Error::TooManyNodes, "too-many-nodes"),
        (Error::Unsupported, "unsupported"),
        (Error::BadField, "bad-field"),
        (Error::BadColour, "bad-colour"),
        (Error::TooLong, "too-long"),
    ];
    let mut seen: Vec<&'static str> = Vec::new();
    for (err, word) in all {
        let text = text_of(|t| diag::page_reject(t, err.word()));
        assert_eq!(text, format!("PAGE reject {}\n", word));
        assert!(text.len() <= panel_core::link::LINE_MAX, "{} is too long", word);
        assert!(!seen.contains(&word), "{} is printed twice", word);
        seen.push(word);
    }
}

#[test]
fn discovery_lines_name_the_round_and_the_answer() {
    // T5.5. Three outcomes, three lines, and the address is printed the same
    // way `LINK tcp peer=` prints it so the two can be diffed by eye.
    assert_eq!(text_of(|t| diag::disc_tx(t)), "DISC tx\n");
    assert_eq!(
        text_of(|t| diag::disc_here(t, [192, 168, 1, 128], 7802)),
        "DISC here ip=192.168.1.128 port=7802\n"
    );
    assert_eq!(text_of(|t| diag::disc_none(t)), "DISC none\n");
}

#[test]
fn boot_step_lines_carry_the_words_the_card_paints() {
    // The words must match `panel_core::boot::Step::text()`, or the log and the
    // glass would describe different states of the same machine.
    for (step, word) in [
        (panel_core::boot::Step::Pending, "--"),
        (panel_core::boot::Step::Busy, "..."),
        (panel_core::boot::Step::Ok, "ok"),
        (panel_core::boot::Step::Fail, "fail"),
    ] {
        assert_eq!(String::from_utf8_lossy(step.text()), word);
        assert_eq!(
            text_of(|t| diag::boot_step(t, "wifi", word)),
            format!("BOOT wifi {}\n", word)
        );
    }
    // A label longer than the buffer loses characters, never the newline.
    let long = text_of(|t| diag::boot_step(t, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "ok"));
    assert!(long.ends_with("ok\n"), "got {:?}", long);
    assert!(long.len() <= 48);
}

#[test]
fn watchdog_and_backoff_lines_pin_the_numbers() {
    // SPEC L-1 and L-2 made visible: the silence threshold in milliseconds,
    // and the delay actually chosen from the ladder.
    assert_eq!(
        text_of(|t| diag::link_silent(t, 15_000)),
        "LINK silent ms=15000\n"
    );
    assert_eq!(
        text_of(|t| diag::link_backoff(t, 8_000, 4)),
        "LINK backoff ms=8000 tries=4\n"
    );
    // The ladder is the one `link.rs` shares with the PC side; this test exists
    // so the printed value cannot drift away from it.
    for tries in 1..=6u32 {
        let ms = panel_core::link::backoff_ms(tries);
        assert_eq!(
            text_of(|t| diag::link_backoff(t, ms, tries)),
            format!("LINK backoff ms={} tries={}\n", ms, tries)
        );
    }
}

#[test]
fn link_challenge_is_pinned() {
    // One line, no fields: the nonce itself never reaches the log.
    assert_eq!(text_of(|t| diag::link_challenge(t)), "LINK challenge\n");
}

#[test]
fn link_auth_tx_is_pinned() {
    assert_eq!(
        text_of(|t| diag::link_auth_tx(t, 51)),
        "LINK auth tx n=51\n"
    );
}

#[test]
fn link_srv_bad_is_pinned() {
    // The impostor case. If this line ever changes, the runbook for "the board
    // refuses to authenticate" stops matching the logs.
    assert_eq!(text_of(|t| diag::link_srv_bad(t)), "LINK srv bad\n");
}
