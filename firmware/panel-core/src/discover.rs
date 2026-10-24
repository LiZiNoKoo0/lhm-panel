//! UDP discovery: the `discover` / `here` pair from PROTOCOL §1.
//!
//! What this does NOT do: no socket, no broadcast, no timing. It produces the
//! exact bytes to send, understands the exact bytes that come back, and answers
//! one policy question -- "should we ask the network again before this attempt?"
//! The BSP owns the socket and the clock.
//!
//! Why the board needs this at all: `board::DAEMON_PEER` used to be a
//! compile-time address, so a DHCP lease change broke the link until someone
//! rebuilt the firmware. PROTOCOL §1 fixes the answer: the terminal broadcasts,
//! the daemon says where it is, and the address becomes data instead of a
//! constant.

use crate::wire;

/// The request, byte for byte (PROTOCOL §1: `{"t":"discover"}`).
pub const DISCOVER: &[u8] = b"{\"t\":\"discover\"}";

/// Most meters in a reply: `{"t":"here","ip":"255.255.255.255","port":65535}`.
pub const REPLY_CAP: usize = 96;

/// How many broadcast attempts before the caller gives up for this round.
///
/// Three, because a single UDP datagram is allowed to be lost and the radio may
/// still be waking up from the association; the caller sends them back to back
/// and listens once, which keeps the worst case short (a failed discovery must
/// not delay a connect by more than a second).
pub const TRIES: u32 = 3;

/// How long the caller listens after the last broadcast, in milliseconds.
pub const REPLY_MS: u32 = 800;

/// Where the daemon says it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Here {
    pub ip: [u8; 4],
    pub port: u16,
}

impl Here {
    pub fn endpoint(&self) -> ([u8; 4], u16) {
        (self.ip, self.port)
    }
}

/// `{"t":"here","ip":"192.168.1.100","port":7802}` -> `Here`.
///
/// Rejects everything that would make a later connect pointless or dangerous:
/// a different `t`, a malformed or non-unicast address (0.0.0.0, 255.255.255.255,
/// multicast), and port 0. Any of those, if accepted, turns into a connect that
/// hangs for the full timeout and an error line that points at the wrong thing.
pub fn parse_here(line: &[u8]) -> Option<Here> {
    if wire::message_kind(line)? != b"here" {
        return None;
    }
    let ip = parse_ipv4(wire::str_field(line, b"ip")?)?;
    let port = wire::uint_field(line, b"port")?;
    if port == 0 || port > u16::MAX as u32 {
        return None;
    }
    if !is_unicast(ip) {
        return None;
    }
    Some(Here {
        ip,
        port: port as u16,
    })
}

/// `192.168.1.100` -> `[192, 168, 1, 100]`. Exactly four parts, 0..=255, no
/// signs, no spaces, nothing after the last digit.
pub fn parse_ipv4(s: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = 0usize;
    let mut value: u32 = 0;
    let mut digits = 0usize;
    for (i, &b) in s.iter().enumerate() {
        match b {
            b'0'..=b'9' => {
                digits += 1;
                if digits > 3 {
                    return None;
                }
                value = value * 10 + (b - b'0') as u32;
                if value > 255 {
                    return None;
                }
            }
            b'.' => {
                if digits == 0 || parts == 3 {
                    return None;
                }
                out[parts] = value as u8;
                parts += 1;
                value = 0;
                digits = 0;
            }
            _ => return None,
        }
        if i + 1 == s.len() && digits == 0 {
            return None;
        }
    }
    if parts != 3 || digits == 0 {
        return None;
    }
    out[3] = value as u8;
    Some(out)
}

/// An address a TCP connect can actually reach.
fn is_unicast(ip: [u8; 4]) -> bool {
    if ip == [0, 0, 0, 0] || ip == [255, 255, 255, 255] {
        return false;
    }
    // 224.0.0.0/4 is multicast, 240.0.0.0/4 is reserved.
    if ip[0] & 0xf0 >= 0xe0 {
        return false;
    }
    // 127.0.0.0/8 is loopback: the daemon answering from inside its own PC
    // would hand us an address the board can never reach.
    if ip[0] == 127 {
        return false;
    }
    true
}

/// Ask the network again before the next connect attempt?
///
/// * No address at all (first boot, or the cache was empty) -> always ask.
/// * Two consecutive failures -> ask again: this is the DHCP-lease-changed case
///   the card exists for. One failure is usually a still-waking radio and does
///   not earn another 0.8 s of listening.
///
/// The failure count comes straight from `Session`, so discovery and the
/// backoff ladder agree on what "failed" means -- and neither one resets the
/// other, which keeps L-5 (only a catalog clears the ladder) intact.
pub fn should_discover(have_peer: bool, failures: u32) -> bool {
    !have_peer || failures >= REDISCOVER_AFTER
}

/// Consecutive failures that mean "the address we have is probably stale".
pub const REDISCOVER_AFTER: u32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    fn here(line: &str) -> Option<Here> {
        parse_here(line.as_bytes())
    }

    #[test]
    fn a_normal_reply_is_understood() {
        let h = here(r#"{"t":"here","ip":"192.168.1.100","port":7802}"#).unwrap();
        assert_eq!(h.ip, [192, 168, 1, 100]);
        assert_eq!(h.port, 7802);
    }

    #[test]
    fn field_order_and_whitespace_do_not_matter() {
        let h = here("{\"port\": 7802 , \"ip\" : \"10.0.0.7\" , \"t\" : \"here\"}").unwrap();
        assert_eq!(h.ip, [10, 0, 0, 7]);
        assert_eq!(h.port, 7802);
    }

    #[test]
    fn other_message_kinds_are_ignored() {
        assert_eq!(here(r#"{"t":"catalog","ip":"192.168.1.100","port":7802}"#), None);
        assert_eq!(here(r#"{"t":"pong"}"#), None);
        assert_eq!(here("CPU 33.5%"), None);
        assert_eq!(here(""), None);
    }

    #[test]
    fn unreachable_addresses_are_rejected_with_a_reason() {
        for bad in [
            "0.0.0.0",
            "255.255.255.255",
            "239.1.2.3",
            "224.0.0.1",
            "127.0.0.1",
            "240.0.0.1",
        ] {
            let line = alloc_here(bad, 7802);
            assert_eq!(here(&line), None, "{} should not be accepted", bad);
        }
    }

    #[test]
    fn a_malformed_address_is_rejected() {
        for bad in ["", "192.168.1", "192.168.1.1.1", "192..1.1", "a.b.c.d",
                    "192.168.1.256", "192.168.1.-1", "192.168.1.1 ", ".192.168.1"] {
            let line = alloc_here(bad, 7802);
            assert_eq!(here(&line), None, "{:?} should not be accepted", bad);
        }
    }

    #[test]
    fn a_zero_or_oversized_port_is_rejected() {
        assert_eq!(here(r#"{"t":"here","ip":"192.168.1.2","port":0}"#), None);
        assert_eq!(here(r#"{"t":"here","ip":"192.168.1.2","port":70000}"#), None);
        assert_eq!(here(r#"{"t":"here","ip":"192.168.1.2"}"#), None);
        assert_eq!(here(r#"{"t":"here","ip":"192.168.1.2","port":"7802"}"#), None);
    }

    #[test]
    fn asking_again_tracks_the_failure_count() {
        assert!(should_discover(false, 0), "no address at all means ask");
        assert!(!should_discover(true, 0));
        assert!(!should_discover(true, 1), "one failure is a waking radio");
        assert!(should_discover(true, 2), "two failures means the lease moved");
        assert!(should_discover(true, 9));
    }

    #[test]
    fn the_request_is_the_bytes_the_protocol_documents() {
        assert_eq!(DISCOVER, br#"{"t":"discover"}"#);
    }

    /// `no_std` has no `format!`, and the tests above want one-string replies.
    fn alloc_here(ip: &str, port: u16) -> String {
        let mut s = String::from(r#"{"t":"here","ip":""#);
        s.push_str(ip);
        s.push_str(r#"","port":"#);
        s.push_str(&port.to_string());
        s.push('}');
        s
    }
}
