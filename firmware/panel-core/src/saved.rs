//! The saved page: a layout as bytes, for flash.
//!
//! What this does NOT do: no flash, no erase, no sector addresses, no `ver`
//! bookkeeping across reboots. It turns a `Layout` into bytes and back, so the
//! "does a page survive a power cut" question is answerable by `cargo test`
//! instead of by pulling the cable.
//!
//! Why binary and not the JSON that arrived (SPEC S-6): booting would then have
//! to run the parser before it could draw anything, and the parser needs the
//! whole message in a buffer. The binary table is roughly a quarter the size and
//! needs no allocation, which matters because this is read before WiFi is up.
//!
//! Layout (little-endian):
//!
//! ```text
//! off  len  what
//!   0    4   magic "PUI1"
//!   4    4   ver, the daemon's `ui` version this page came from
//!   8    1   scene index
//!   9    2   background RGB565
//!  11    1   node count (0..=32)
//!  12    n   nodes, packed by kind
//!  ..    4   crc32 over every preceding byte
//! ```
//!
//! A node is `kind u8 | x i16 | y i16` followed by only the fields its kind
//! owns. Fixed-size records would be simpler, but `rect` needs no bind and `num`
//! needs no text, and 32 nodes of the union does not fit in the 1024-byte sector
//! payload -- the compact form does.
//!
//! `interval_ms` is deliberately **not** saved. It is a property of the daemon's
//! catalog, not of the page, and on boot there is no catalog yet. Leaving it 0
//! means "never stale", and with an empty value table every `num` draws `--`
//! anyway, which is exactly what PROTOCOL 22 says a page looks like with no PC.

use crate::crc32::crc32;
use crate::ui::{Id, Kind, Layout, Node, MAX_ID, MAX_NODES, MAX_TEXT, MAX_UNIT};

pub const MAGIC: [u8; 4] = *b"PUI1";
pub const VER: u8 = 1;

/// Header before the node table.
pub const HEADER_LEN: usize = 12;
/// Trailing checksum.
pub const CRC_LEN: usize = 4;
/// Smallest possible saved page: header, no nodes, checksum.
pub const MIN_LEN: usize = HEADER_LEN + CRC_LEN;
/// The sector payload this must fit into (see `sector::MAX_PAYLOAD`).
pub const MAX_LEN: usize = 1024;

const KIND_RECT: u8 = 0;
const KIND_TEXT: u8 = 1;
const KIND_NUM: u8 = 2;

fn put_u16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(src: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([src[at], src[at + 1]])
}

fn get_u32(src: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([src[at], src[at + 1], src[at + 2], src[at + 3]])
}

/// Worst case bytes one node can occupy: a `num` with a full id and unit.
fn node_bound(node: &Node) -> usize {
    let fixed = 1 + 2 + 2; // kind, x, y
    match node.kind {
        Kind::Rect => fixed + 2 + 2 + 2 + 1 + 2,
        Kind::Text => fixed + 1 + 2 + 1 + MAX_TEXT,
        Kind::Num => fixed + 1 + 2 + 1 + 1 + MAX_ID + 1 + MAX_UNIT,
    }
}

/// Serialise `layout` into `out`. Returns the byte count, or `None` when it will
/// not fit -- refusing is better than writing a page that decodes to half a
/// screen.
pub fn encode(layout: &Layout, out: &mut [u8]) -> Option<usize> {
    let nodes = layout.nodes();
    if nodes.len() > MAX_NODES {
        return None;
    }
    let need: usize = HEADER_LEN + CRC_LEN + nodes.iter().map(node_bound).sum::<usize>();
    if need > out.len() || need > MAX_LEN {
        return None;
    }

    out[0..4].copy_from_slice(&MAGIC);
    put_u32(out, 4, layout.ver);
    out[8] = layout.scene;
    put_u16(out, 9, layout.bg);
    out[11] = nodes.len() as u8;

    let mut at = HEADER_LEN;
    for node in nodes {
        out[at] = match node.kind {
            Kind::Rect => KIND_RECT,
            Kind::Text => KIND_TEXT,
            Kind::Num => KIND_NUM,
        };
        at += 1;
        put_u16(out, at, node.x as u16);
        at += 2;
        put_u16(out, at, node.y as u16);
        at += 2;

        match node.kind {
            Kind::Rect => {
                put_u16(out, at, node.w);
                at += 2;
                put_u16(out, at, node.h);
                at += 2;
                put_u16(out, at, node.colour);
                at += 2;
                match node.border {
                    Some(border) => {
                        out[at] = 1;
                        at += 1;
                        put_u16(out, at, border);
                        at += 2;
                    }
                    None => {
                        out[at] = 0;
                        at += 1;
                    }
                }
            }
            Kind::Text => {
                out[at] = node.size;
                at += 1;
                put_u16(out, at, node.colour);
                at += 2;
                let len = (node.text_len as usize).min(MAX_TEXT);
                out[at] = len as u8;
                at += 1;
                out[at..at + len].copy_from_slice(&node.text[..len]);
                at += len;
            }
            Kind::Num => {
                out[at] = node.size;
                at += 1;
                put_u16(out, at, node.colour);
                at += 2;
                out[at] = node.dec;
                at += 1;
                let id_len = node.bind.len();
                out[at] = id_len as u8;
                at += 1;
                out[at..at + id_len].copy_from_slice(node.bind.raw());
                at += id_len as usize;
                let unit_len = (node.unit_len as usize).min(MAX_UNIT);
                out[at] = unit_len as u8;
                at += 1;
                out[at..at + unit_len].copy_from_slice(&node.unit[..unit_len]);
                at += unit_len;
            }
        }
    }

    let crc = crc32(&out[..at]);
    put_u32(out, at, crc);
    at += CRC_LEN;
    Some(at)
}

/// Rebuild a layout from bytes. `None` for anything that is not a whole,
/// checksum-clean page -- a half-written save must read back as absent, never as
/// a page missing its bottom row.
pub fn decode(bytes: &[u8]) -> Option<Layout> {
    if bytes.len() < MIN_LEN || bytes.len() > MAX_LEN {
        return None;
    }
    if bytes[0..4] != MAGIC {
        return None;
    }

    let count = bytes[11] as usize;
    if count > MAX_NODES {
        return None;
    }

    // The checksum covers everything up to its own field, so a truncated frame
    // fails here rather than at some field halfway through.
    let body_len = bytes.len() - CRC_LEN;
    let want = get_u32(bytes, body_len);
    let got = crc32(&bytes[..body_len]);
    if want != got {
        return None;
    }

    let mut nodes = [Node::EMPTY; MAX_NODES];
    let mut at = HEADER_LEN;
    for slot in nodes.iter_mut().take(count) {
        if at + 5 > body_len {
            return None;
        }
        let kind = bytes[at];
        at += 1;
        let x = get_u16(bytes, at) as i16;
        at += 2;
        let y = get_u16(bytes, at) as i16;
        at += 2;

        let mut node = Node::EMPTY;
        node.x = x;
        node.y = y;
        match kind {
            KIND_RECT => {
                if at + 7 > body_len {
                    return None;
                }
                node.kind = Kind::Rect;
                node.w = get_u16(bytes, at);
                at += 2;
                node.h = get_u16(bytes, at);
                at += 2;
                node.colour = get_u16(bytes, at);
                at += 2;
                let has_border = bytes[at];
                at += 1;
                if has_border == 1 {
                    if at + 2 > body_len {
                        return None;
                    }
                    node.border = Some(get_u16(bytes, at));
                    at += 2;
                } else {
                    node.border = None;
                }
            }
            KIND_TEXT => {
                if at + 4 > body_len {
                    return None;
                }
                node.kind = Kind::Text;
                node.size = bytes[at];
                at += 1;
                node.colour = get_u16(bytes, at);
                at += 2;
                let len = bytes[at] as usize;
                at += 1;
                if len > MAX_TEXT || at + len > body_len {
                    return None;
                }
                node.text[..len].copy_from_slice(&bytes[at..at + len]);
                node.text_len = len as u8;
                at += len;
            }
            KIND_NUM => {
                if at + 5 > body_len {
                    return None;
                }
                node.kind = Kind::Num;
                node.size = bytes[at];
                at += 1;
                node.colour = get_u16(bytes, at);
                at += 2;
                node.dec = bytes[at].min(3);
                at += 1;
                let id_len = bytes[at] as usize;
                at += 1;
                if id_len > MAX_ID || at + id_len > body_len {
                    return None;
                }
                node.bind = Id::from_raw(&bytes[at..at + id_len])?;
                at += id_len;
                if at + 1 > body_len {
                    return None;
                }
                let unit_len = bytes[at] as usize;
                at += 1;
                if unit_len > MAX_UNIT || at + unit_len > body_len {
                    return None;
                }
                node.unit[..unit_len].copy_from_slice(&bytes[at..at + unit_len]);
                node.unit_len = unit_len as u8;
                at += unit_len;
            }
            _ => return None,
        }
        *slot = node;
    }

    Layout::from_parts(bytes[8], get_u32(bytes, 4), get_u16(bytes, 9), &nodes[..count])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn sample() -> Layout {
        let mut nodes = [Node::EMPTY; 3];

        nodes[0].kind = Kind::Text;
        nodes[0].x = 4;
        nodes[0].y = 4;
        nodes[0].size = 16;
        nodes[0].colour = 0xFFFF;
        nodes[0].text[..3].copy_from_slice(b"CPU");
        nodes[0].text_len = 3;

        nodes[1].kind = Kind::Num;
        nodes[1].x = 4;
        nodes[1].y = 24;
        nodes[1].size = 32;
        nodes[1].colour = 0x07E0;
        nodes[1].dec = 1;
        nodes[1].bind = Id::new("cpu.total").unwrap();
        nodes[1].unit[..1].copy_from_slice(b"%");
        nodes[1].unit_len = 1;

        nodes[2].kind = Kind::Rect;
        nodes[2].x = 4;
        nodes[2].y = 60;
        nodes[2].w = 100;
        nodes[2].h = 6;
        nodes[2].colour = 0x2222;
        nodes[2].border = Some(0xF800);

        Layout::from_parts(2, 7, 0x1010, &nodes).unwrap()
    }

    fn roundtrip(layout: &Layout) -> Layout {
        let mut buf = [0u8; MAX_LEN];
        let len = encode(layout, &mut buf).expect("must encode");
        decode(&buf[..len]).expect("must decode")
    }

    #[test]
    fn a_page_survives_the_round_trip() {
        let original = sample();
        let back = roundtrip(&original);

        assert_eq!(back.scene, original.scene);
        assert_eq!(back.ver, original.ver);
        assert_eq!(back.bg, original.bg);
        assert_eq!(back.len(), original.len());

        for (a, b) in original.nodes().iter().zip(back.nodes()) {
            assert_eq!(a.kind, b.kind);
            assert_eq!((a.x, a.y), (b.x, b.y));
            assert_eq!((a.w, a.h), (b.w, b.h));
            assert_eq!(a.size, b.size);
            assert_eq!(a.colour, b.colour);
            assert_eq!(a.border, b.border);
            assert_eq!(a.text_str(), b.text_str());
            assert_eq!(a.bind, b.bind);
            assert_eq!(a.dec, b.dec);
            assert_eq!(a.unit_str(), b.unit_str());
        }
    }

    #[test]
    fn the_decoded_page_draws_the_same_pixels() {
        // The real contract: a page read back from flash looks like the page that
        // was saved. Comparing fields can pass while a colour or a size is
        // subtly wrong; comparing the frame cannot.
        let original = sample();
        let back = roundtrip(&original);

        let mut one = [0u8; 160 * 80 * 2];
        let mut a = Canvas::new(&mut one, 160, 80);
        original.render(&mut a, &crate::ui::Values::EMPTY, 0);

        let mut two = [0u8; 160 * 80 * 2];
        let mut b = Canvas::new(&mut two, 160, 80);
        back.render(&mut b, &crate::ui::Values::EMPTY, 0);

        assert_eq!(a.digest(), b.digest(), "saved and restored pages must be the same picture");
    }

    #[test]
    fn intervals_are_not_saved_and_come_back_zero() {
        // interval_ms is the daemon's catalog talking, not the page. After a
        // reboot there is no catalog yet, and 0 means "never stale" -- which is
        // right, because every value is absent and draws `--` regardless.
        let original = sample();
        let mut buf = [0u8; MAX_LEN];
        let len = encode(&original, &mut buf).unwrap();
        let back = decode(&buf[..len]).unwrap();
        for node in back.nodes() {
            assert_eq!(node.interval_ms, 0);
        }
    }

    #[test]
    fn a_flipped_byte_is_refused_not_drawn() {
        let original = sample();
        let mut buf = [0u8; MAX_LEN];
        let len = encode(&original, &mut buf).unwrap();
        buf[len / 2] ^= 0xFF;
        assert!(decode(&buf[..len]).is_none(), "a corrupt page must read as absent");
    }

    #[test]
    fn a_truncated_page_is_refused() {
        let original = sample();
        let mut buf = [0u8; MAX_LEN];
        let len = encode(&original, &mut buf).unwrap();
        // A write cut short: every prefix must be rejected, never half-decoded.
        for cut in 0..len {
            assert!(
                decode(&buf[..cut]).is_none(),
                "prefix of {} bytes must be refused",
                cut
            );
        }
    }

    #[test]
    fn an_empty_page_is_legal_and_small() {
        let layout = Layout::from_parts(0, 1, 0x0000, &[]).unwrap();
        let mut buf = [0u8; MAX_LEN];
        let len = encode(&layout, &mut buf).unwrap();
        assert_eq!(len, MIN_LEN);
        let back = decode(&buf[..len]).unwrap();
        assert_eq!(back.len(), 0);
    }

    #[test]
    fn a_full_page_of_worst_case_nodes_still_fits_the_sector() {
        // 32 `num` nodes with the longest id and unit is the worst case the
        // compact encoding has to survive, because the sector payload is 1024 B.
        let mut nodes = [Node::EMPTY; MAX_NODES];
        for node in nodes.iter_mut() {
            node.kind = Kind::Num;
            node.bind = Id::new("abcdefghijklmnop").unwrap();
            node.unit[..4].copy_from_slice(b"KB/s");
            node.unit_len = 4;
        }
        let layout = Layout::from_parts(0, 1, 0, &nodes).unwrap();
        let mut buf = [0u8; MAX_LEN];
        let len = encode(&layout, &mut buf).expect("worst case must fit");
        assert!(len <= MAX_LEN, "encoded {} bytes, sector holds {}", len, MAX_LEN);
        assert!(decode(&buf[..len]).is_some());
    }

    #[test]
    fn a_page_too_big_for_the_buffer_is_refused() {
        let layout = sample();
        let mut tiny = [0u8; 8];
        assert!(encode(&layout, &mut tiny).is_none());
    }
}
