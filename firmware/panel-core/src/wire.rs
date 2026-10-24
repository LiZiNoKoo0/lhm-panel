//! Wire facts for the thin protocol: the hello line itself, and the cheapest
//! possible way to tell one message kind from another.
//!
//! What this does NOT do: no JSON parser, no allocation, no I/O, no idea what
//! to do with a message. It answers a few questions only -- "which kind is this
//! line" (`t` value), "which version does it carry" (`ver`), "how many metrics
//! did the catalog hold" -- which is all T5 needs to prove the link works. The
//! real dispatcher arrives with `ui`/`data` in T6, and it reuses these helpers
//! rather than growing a second, subtly different scanner.
//!
//! Why not a JSON parser yet: on this line the firmware's job is
//! classification, not comprehension. A full parser in `no_std` costs code and
//! adds a new class of failure (nesting, escapes, depth) to answer a question
//! that one byte answers. The moment `ui`/`data` must be *understood*, T6
//! brings a real parser -- and then this file stops being the dispatcher and
//! becomes exactly what its name says: a pre-filter.

/// The `hello` this board sends, as the exact bytes that go on the wire.
///
/// Differences from SPEC 5.1's copy, all deliberate and all recorded there:
///
///   * `proto` instead of `v`. PROTOCOL 5 names the field `proto` and the
///     daemon reads `proto`; SPEC 5.1's `v` was never a field the daemon
///     looked at. SPEC 0 makes PROTOCOL the tie-breaker, so `proto` wins.
///   * `token` added. The daemon refuses a hello with the wrong (or missing)
///     token: `fake_daemon.py::_check_hello` returns "bad token" and closes the
///     connection. An empty token only works against a daemon started with
///     `--lenient` or `--token ""` (T5.6 moves it into flash).
///   * `accept` added. The daemon only sends `ui` and `data` to a terminal
///     that asked for them (`client.accepts("ui")`), so without this the board
///     would receive a catalog and then silence.
///
/// Field order follows SPEC 5.1 so the two can be diffed by eye.
pub const HELLO: &str = concat!(
    "{\"t\":\"hello\",\"proto\":1,\"token\":\"\",",
    "\"client_id\":\"st7735-160x80\",\"role\":\"thin\",",
    "\"accept\":[\"ui\",\"data\"],",
    "\"display\":{\"w\":160,\"h\":80,\"depth\":16},",
    "\"charset\":\"ascii\",",
    // What the firmware really draws: `ui::parse_node` refuses `path`, `line`,
    // `bar` and `dot` whole. Claiming `path` here made the T6 daemon send a
    // page containing one, and the page was rejected -- the handshake was
    // telling the daemon a capability this board does not have.
    "\"primitives\":[\"rect\",\"text\",\"num\"],",
    "\"fonts\":[16,32,48],",
    "\"input\":[],",
    "\"limits\":{\"max_nodes\":32,\"max_json_bytes\":1024,\"max_text_len\":20}}"
);

/// `hello` split around the token value, so T5.6 can substitute the stored
/// token at runtime without `format!` (no alloc on this path) and without a
/// second copy of the field list that could silently drift from `HELLO`.
///
/// `hello_into("")` must equal `HELLO` byte for byte -- a test enforces it, so
/// the two definitions cannot disagree.
pub const HELLO_HEAD: &str = "{\"t\":\"hello\",\"proto\":1,\"token\":\"";
pub const HELLO_TAIL: &str = concat!(
    "\",",
    "\"client_id\":\"st7735-160x80\",\"role\":\"thin\",",
    "\"accept\":[\"ui\",\"data\"],",
    "\"display\":{\"w\":160,\"h\":80,\"depth\":16},",
    "\"charset\":\"ascii\",",
    "\"primitives\":[\"rect\",\"text\",\"num\"],",
    "\"fonts\":[16,32,48],",
    "\"input\":[],",
    "\"limits\":{\"max_nodes\":32,\"max_json_bytes\":1024,\"max_text_len\":20}}"
);

/// Write `hello` carrying `token` into `out`; returns its length.
///
/// `None` only if `out` is too small -- the caller sizes the buffer from
/// `HELLO_HEAD.len() + creds::TOKEN_MAX + HELLO_TAIL.len()`.
pub fn hello_into(token: &str, out: &mut [u8]) -> Option<usize> {
    let t = token.as_bytes();
    let total = HELLO_HEAD.len() + t.len() + HELLO_TAIL.len();
    if out.len() < total {
        return None;
    }
    out[..HELLO_HEAD.len()].copy_from_slice(HELLO_HEAD.as_bytes());
    let mut at = HELLO_HEAD.len();
    out[at..at + t.len()].copy_from_slice(t);
    at += t.len();
    out[at..at + HELLO_TAIL.len()].copy_from_slice(HELLO_TAIL.as_bytes());
    Some(total)
}

/// Longest line the protocol may send: `max_json_bytes` + slack (SPEC 5.3).
pub const LINE_CAP: usize = 1024 + 16;

/// The answer to a `ping` (SPEC 5.2: `ping` -> `pong`).
///
/// Kept here next to `HELLO` for the same reason: the exact bytes on the wire
/// are a fact of this firmware, and a fact that lives in one place can be
/// pinned by a test.
pub const PONG: &str = "{\"t\":\"pong\"}";

/// Kind names the thin subset knows about, whether it acts on them yet or not.
///
/// The distinction being drawn is *not* "handled" vs "ignored" -- T5.3 acts on
/// three of them only -- but "a name the protocol defines" vs "a name nobody
/// here has ever heard of". The second is W7 (SPEC 5.2: silently ignore and
/// count) and is worth a diagnostics line the first few times it happens,
/// because a daemon that renames a message should not look like a dead link.
pub fn is_known_kind(kind: &[u8]) -> bool {
    const KNOWN: [&[u8]; 13] = [
        b"hello",
        b"catalog",
        b"scenes",
        b"scene",
        b"ui",
        b"data",
        b"cmd",
        b"time",
        b"debug",
        b"ping",
        b"pong",
        b"deny",
        b"event",
    ];
    KNOWN.contains(&kind)
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

/// Offset of the value that follows `"key"` `:`, whitespace tolerated on both
/// sides of the colon. Not escape-aware: keys are ASCII identifiers and values
/// are numbers or short identifiers, so `\"` inside them cannot occur. This is
/// the whole reason it stays small enough to trust.
fn value_start(line: &[u8], key: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + key.len() + 2 <= line.len() {
        if line[i] == b'"' && &line[i + 1..i + 1 + key.len()] == key && line[i + 1 + key.len()] == b'"'
        {
            let j = skip_ws(line, i + 2 + key.len());
            if line.get(j) == Some(&b':') {
                return Some(skip_ws(line, j + 1));
            }
        }
        i += 1;
    }
    None
}

/// The value of `"t"`, e.g. `b"catalog"`. `None` when the line has no such
/// field, or the value is not a string -- both mean "not a message I know",
/// which the caller counts as W7 (SPEC 5.2).
pub fn message_kind(line: &[u8]) -> Option<&[u8]> {
    str_field(line, b"t")
}

/// A string field, e.g. `"reason"` inside a `deny`.
///
/// `None` when the field is absent, is not a string, or contains a `\` escape.
/// Refusing escapes is deliberate: unescaping means allocating, and the only
/// consumer is a diagnostic line (`LINK deny reason=bad token`). Printing the
/// raw bytes of an escaped string would print JSON, not a reason, so a blank is
/// the honest answer.
pub fn str_field<'a>(line: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut i = value_start(line, key)?;
    if line.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    let start = i;
    while i < line.len() {
        match line[i] {
            b'"' => return Some(&line[start..i]),
            b'\\' => return None,
            _ => i += 1,
        }
    }
    None
}

/// A non-negative integer field, saturating at `u32::MAX`. Used for `ver` and
/// `s`; a fractional or quoted value yields `None` rather than a guess.
pub fn uint_field(line: &[u8], key: &[u8]) -> Option<u32> {
    let i = value_start(line, key)?;
    let mut value: u32 = 0;
    let mut digits = 0;
    let mut j = i;
    while j < line.len() && line[j].is_ascii_digit() {
        value = value.saturating_mul(10).saturating_add(u32::from(line[j] - b'0'));
        j += 1;
        digits += 1;
    }
    if digits == 0 {
        None
    } else {
        Some(value)
    }
}

/// How many metrics a `catalog` line carries, counted as `"id"` occurrences.
///
/// Deliberately not a parse: the number is a log field (SPEC 7.1 "how many
/// things did the PC say exist"), and an off-by-nothing count of a name that
/// only appears once per metric is enough to tell "catalog arrived and is
/// empty" from "catalog arrived with six metrics". A key that merely contained
/// the letters `id` would have to be quoted exactly like a metric to fool it.
pub fn count_ids(line: &[u8]) -> usize {
    const NEEDLE: &[u8] = b"\"id\"";
    let mut count = 0;
    let mut i = 0;
    while i + NEEDLE.len() <= line.len() {
        if &line[i..i + NEEDLE.len()] == NEEDLE {
            count += 1;
            i += NEEDLE.len();
        } else {
            i += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_is_one_line_of_valid_shape() {
        // One line: the transport splits on '\n' and nothing else.
        assert!(!HELLO.contains('\n'));
        assert!(!HELLO.contains('\r'));
        // ASCII only (SPEC 8.2): the wire is ascii, and so is the source.
        assert!(HELLO.is_ascii());
        // Balanced braces/brackets -- a truncated concat would otherwise look
        // fine to every other test here.
        let mut depth = 0i32;
        for byte in HELLO.bytes() {
            match byte {
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "closing more than opened");
        }
        assert_eq!(depth, 0);
    }

    #[test]
    fn hello_declares_everything_the_daemon_checks() {
        // The three fields SPEC 5.1 was missing; the daemon refuses without
        // them (T5.1 finding, see the const's doc comment).
        assert_eq!(message_kind(HELLO.as_bytes()), Some(&b"hello"[..]));
        assert_eq!(uint_field(HELLO.as_bytes(), b"proto"), Some(1));
        assert!(HELLO.contains("\"token\":"));
        assert!(HELLO.contains("\"accept\":[\"ui\",\"data\"]"));
        // And the two the daemon reads to decide what to send.
        assert!(HELLO.contains("\"role\":\"thin\""));
        assert!(HELLO.contains("\"client_id\":\"st7735-160x80\""));
    }

    #[test]
    fn str_field_reads_a_reason_without_unescaping_it() {
        let deny = br#"{"t":"deny","reason":"bad token"}"#;
        assert_eq!(str_field(deny, b"reason"), Some(&b"bad token"[..]));
        assert_eq!(str_field(deny, b"t"), Some(&b"deny"[..]));
        // Escapes are refused rather than printed raw: the caller wants a
        // reason a human can read, not a fragment of JSON.
        let escaped = br#"{"t":"deny","reason":"a\"b"}"#;
        assert_eq!(str_field(escaped, b"reason"), None);
        // Absent / wrong type / unterminated: None, never a panic.
        assert_eq!(str_field(br#"{"t":"ping"}"#, b"reason"), None);
        assert_eq!(str_field(br#"{"reason":7}"#, b"reason"), None);
        assert_eq!(str_field(br#"{"reason":"x"#, b"reason"), None);
        assert_eq!(str_field(b"", b"reason"), None);
    }

    #[test]
    fn hello_limits_match_the_panel() {
        assert!(HELLO.contains("\"w\":160"));
        assert!(HELLO.contains("\"h\":80"));
        assert!(HELLO.contains("\"max_nodes\":32"));
        assert!(HELLO.contains("\"max_json_bytes\":1024"));
        assert!(HELLO.contains("\"max_text_len\":20"));
        assert!(HELLO.contains("\"fonts\":[16,32,48]"));
        // The panel has one 8x16 glyph table and draws 32/48 by integer
        // magnification. Declaring them here is what lets the editor offer
        // them instead of guessing (T6c follow-up, 2026-10-25).
        // No input devices yet: the daemon must not send focusable nodes.
        assert!(HELLO.contains("\"input\":[]"));
        assert!(!HELLO.contains("\"widgets\""));
    }

    #[test]
    fn pong_is_the_one_message_this_board_invents() {
        assert_eq!(message_kind(PONG.as_bytes()), Some(&b"pong"[..]));
        assert_eq!(PONG, "{\"t\":\"pong\"}");
        assert!(!PONG.contains('\n'));
    }

    #[test]
    fn known_kinds_cover_what_the_daemon_sends_and_nothing_else() {
        for kind in [
            &b"hello"[..],
            b"catalog",
            b"scenes",
            b"ui",
            b"data",
            b"cmd",
            b"time",
            b"debug",
            b"ping",
            b"pong",
            b"deny",
        ] {
            assert!(is_known_kind(kind), "{:?}", core::str::from_utf8(kind));
        }
        // A typo, a renamed message, an empty kind: all W7, never "handled".
        for kind in [&b""[..], b"Catalog", b"cat", b"widget"] {
            assert!(!is_known_kind(kind));
        }
    }

    #[test]
    fn hello_fits_one_protocol_line() {
        assert!(HELLO.len() <= LINE_CAP, "hello is {} bytes", HELLO.len());
    }

    #[test]
    fn hello_into_with_no_token_is_the_pinned_hello() {
        // This is the two-definitions guard: `HELLO` (a literal, for the tests
        // that read it) and `HELLO_HEAD`/`HELLO_TAIL` (what the board actually
        // sends) must produce the same bytes, or one of them has drifted.
        let mut buf = [0u8; 512];
        let n = hello_into("", &mut buf).expect("buffer big enough");
        assert_eq!(&buf[..n], HELLO.as_bytes());
    }

    #[test]
    fn hello_into_carries_the_token_verbatim() {
        let token = "0123456789abcdef";
        let mut buf = [0u8; 512];
        let n = hello_into(token, &mut buf).expect("buffer big enough");
        let line = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(line.contains("\"token\":\"0123456789abcdef\""));
        // Everything except the token must be byte-identical to `HELLO`.
        assert_eq!(line.len(), HELLO.len() + token.len());
        assert!(line.starts_with(HELLO_HEAD));
        assert!(line.ends_with(HELLO_TAIL));
        assert!(n <= LINE_CAP, "hello with token is {} bytes", n);
    }

    #[test]
    fn hello_into_refuses_a_buffer_that_is_too_small() {
        let mut tiny = [0u8; 16];
        assert!(hello_into("tok", &mut tiny).is_none());
    }

    #[test]
    fn kind_is_read_wherever_the_field_sits() {
        assert_eq!(message_kind(br#"{"t":"catalog","ver":3}"#), Some(&b"catalog"[..]));
        assert_eq!(message_kind(br#"{"ver":3,"t":"data"}"#), Some(&b"data"[..]));
        assert_eq!(message_kind(br#"{ "t" : "ui" }"#), Some(&b"ui"[..]));
        // `t` inside a string value is not the field: the scanner only accepts
        // a quoted name followed by a colon.
        assert_eq!(message_kind(br#"{"text":"t: not a field"}"#), None);
    }

    #[test]
    fn kind_gives_up_on_garbage_without_panicking() {
        for line in [
            &b""[..],
            b"{",
            b"{\"t\"",
            b"{\"t\":",
            b"{\"t\":\"unterminated",
            b"{\"t\":12}",
            b"not json at all",
            b"\xff\xfe\x00",
        ] {
            let _ = message_kind(line);
            let _ = uint_field(line, b"ver");
            let _ = count_ids(line);
        }
        assert_eq!(message_kind(b"{\"t\":12}"), None);
        assert_eq!(message_kind(b"{\"t\":\"unterminated"), None);
    }

    #[test]
    fn ver_field_is_read_or_refused() {
        assert_eq!(uint_field(br#"{"t":"catalog","ver":0}"#, b"ver"), Some(0));
        assert_eq!(uint_field(br#"{"t":"catalog","ver":12}"#, b"ver"), Some(12));
        assert_eq!(uint_field(br#"{"t":"ui","ver":7,"s":9}"#, b"ver"), Some(7));
        // Quoted, negative and absent all mean "not a number I can use".
        assert_eq!(uint_field(br#"{"ver":"7"}"#, b"ver"), None);
        assert_eq!(uint_field(br#"{"ver":-7}"#, b"ver"), None);
        assert_eq!(uint_field(br#"{"t":"data"}"#, b"ver"), None);
        // A name that contains the key is not the key.
        assert_eq!(uint_field(br#"{"server":3,"ver":4}"#, b"ver"), Some(4));
    }

    #[test]
    fn catalog_metric_count() {
        let line = br#"{"t":"catalog","s":1,"ver":3,"metrics":[{"id":"cpu.total","unit":"%"},{"id":"cpu.temp"},{"id":"mem.used"}]}"#;
        assert_eq!(count_ids(line), 3);
        assert_eq!(count_ids(br#"{"t":"catalog","metrics":[]}"#), 0);
        // `s` is not `id`; a metric named "idle" still counts once.
        assert_eq!(count_ids(br#"{"id":"cpu.idle"}"#), 1);
        assert_eq!(count_ids(b""), 0);
    }

    #[test]
    fn hello_claims_exactly_what_the_page_parser_draws() {
        // The handshake is a promise: a primitive named here is a primitive the
        // daemon may put in a page. Until T6 `path` was promised and
        // `ui::parse_node` refused it whole, so every real page from the daemon
        // was rejected and the panel kept showing the self-test card. This ties
        // the two lists together, so the promise cannot drift from the renderer
        // again.
        let samples = [
            ("rect", r##"{"t":"ui","ver":1,"nodes":[{"type":"rect","x":0,"y":0,"w":4,"h":4,"fill":"#000000"}]}"##),
            ("text", r##"{"t":"ui","ver":1,"nodes":[{"type":"text","x":0,"y":0,"text":"A"}]}"##),
            ("num", r##"{"t":"ui","ver":1,"nodes":[{"type":"num","x":0,"y":0,"bind":"a"}]}"##),
            ("path", r##"{"t":"ui","ver":1,"nodes":[{"type":"path","x":0,"y":0,"pts":[0,0]}]}"##),
            ("line", r##"{"t":"ui","ver":1,"nodes":[{"type":"line","x":0,"y":0,"pts":[0,0]}]}"##),
            ("bar", r##"{"t":"ui","ver":1,"nodes":[{"type":"bar","x":0,"y":0,"w":4,"h":4,"bind":"a"}]}"##),
        ];
        for (name, page) in samples {
            let promised = HELLO.contains(&format!("\"{}\"", name));
            let drawn = crate::ui::Layout::parse(page.as_bytes()).is_ok();
            assert_eq!(
                promised, drawn,
                "hello promises {} = {}, but the page parser draws it = {}",
                name, promised, drawn
            );
        }
    }
}
