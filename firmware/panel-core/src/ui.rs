//! The `ui` message, as a thing this panel can draw: a fixed table of nodes.
//!
//! What this does NOT do: no `path` (T7), no `bar`/`dot` sugar, no `bind_w`, no
//! focus table yet (T6d), no scenes beyond the one that arrived. Those are
//! separate cards on purpose -- each one has a way to be wrong that the others
//! do not, and a parser that grew all of them at once would be untestable.
//!
//! Why a fixed table: the panel is 160x80 and holds at most 20x5 text cells, so
//! a page with 32 nodes is already generous. A fixed array in the layout means
//! the whole `ui` parse is allocation-free, which matters because B-1 showed
//! what a big static costs (stack) and what a big alloc costs (fragmentation).
//!
//! The shape parsed is PROTOCOL section 7:
//!
//! ```json
//! {"t":"ui","scene":0,"ver":7,"bg":"#000000","nodes":[
//!   {"type":"text","x":8,"y":6,"text":"CPU","color":"#888888"},
//!   {"type":"num","x":8,"y":22,"bind":"cpu.total","dec":0,"unit":"%"},
//!   {"type":"rect","x":8,"y":54,"w":144,"h":6,"fill":"#44DD88"}
//! ]}
//! ```
//!
//! Colours are `"#RRGGBB"` on the wire and RGB565 in the frame; the conversion
//! lives here so the parser and the renderer cannot disagree about rounding.

use crate::canvas::Canvas;
use crate::json;

/// Nodes a page may carry (PROTOCOL 7: `max_nodes` in `hello` is 32).
pub const MAX_NODES: usize = 32;
/// Longest static text in cells: the panel is 20 cells wide.
pub const MAX_TEXT: usize = 20;
/// Longest metric id we key values by (`cpu.total`, `gpu.temp`, ...).
pub const MAX_ID: usize = 16;
/// Longest unit string (`%`, `C`, `MB/s`).
pub const MAX_UNIT: usize = 4;

/// A metric id, stored inline so a node never points into the receive buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Id {
    bytes: [u8; MAX_ID],
    len: u8,
}

impl Id {
    pub fn new(text: &str) -> Option<Self> {
        let b = text.as_bytes();
        if b.len() > MAX_ID {
            return None;
        }
        let mut id = Self { bytes: [0; MAX_ID], len: b.len() as u8 };
        id.bytes[..b.len()].copy_from_slice(b);
        Some(id)
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl core::fmt::Debug for Id {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a node draws. Only the three primitives this panel can honour today.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Rect,
    Text,
    Num,
}

/// One node, in drawing order.
#[derive(Clone, Copy)]
pub struct Node {
    pub kind: Kind,
    pub x: i16,
    pub y: i16,
    /// `rect` only: size. Text and numbers are sized by the font.
    pub w: u16,
    pub h: u16,
    /// Fill for `rect`, ink for `text`/`num`.
    pub colour: u16,
    /// `rect` outline, if the node had `color` as well as `fill`.
    pub border: Option<u16>,
    /// Static text (`text`).
    pub text: [u8; MAX_TEXT],
    pub text_len: u8,
    /// Metric binding (`num`; NUL for the other kinds).
    pub bind: Id,
    /// Decimal places (`num`), 0..=3.
    pub dec: u8,
    pub unit: [u8; MAX_UNIT],
    pub unit_len: u8,
}

impl Node {
    const EMPTY: Node = Node {
        kind: Kind::Text,
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        colour: 0xFFFF,
        border: None,
        text: [0; MAX_TEXT],
        text_len: 0,
        bind: Id { bytes: [0; MAX_ID], len: 0 },
        dec: 0,
        unit: [0; MAX_UNIT],
        unit_len: 0,
    };

    fn text_str(&self) -> &str {
        core::str::from_utf8(&self.text[..self.text_len as usize]).unwrap_or("")
    }

    fn unit_str(&self) -> &str {
        core::str::from_utf8(&self.unit[..self.unit_len as usize]).unwrap_or("")
    }
}

/// Why a `ui` line was refused. Each variant becomes one diagnostic word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Not a JSON object, or not `"t":"ui"`.
    NotUi,
    /// The `nodes` array is missing or not an array.
    NoNodes,
    /// More than [`MAX_NODES`] nodes: the page cannot be honoured, and drawing
    /// half of it would look like a bug in the data rather than in the layout.
    TooManyNodes,
    /// A node with a type this firmware does not implement (`path`, `line`,
    /// `bar`, a typo).
    Unsupported,
    /// A required field missing or of the wrong JSON type.
    BadField,
    /// `"#RRGGBB"` that is not six hex digits.
    BadColour,
    /// Static text longer than the panel is wide, or an id longer than MAX_ID.
    TooLong,
}

impl Error {
    /// The short ASCII word this error prints as (`PAGE reject <word>`).
    ///
    /// Words instead of numbers on purpose: a diagnostic that says `reject 3`
    /// needs a table to read, and this line is what a log reader gets.
    pub fn word(self) -> &'static str {
        match self {
            Error::NotUi => "not-ui",
            Error::NoNodes => "no-nodes",
            Error::TooManyNodes => "too-many-nodes",
            Error::Unsupported => "unsupported",
            Error::BadField => "bad-field",
            Error::BadColour => "bad-colour",
            Error::TooLong => "too-long",
        }
    }
}

/// A whole page: background plus the nodes in drawing order.
#[derive(Clone)]
pub struct Layout {
    pub scene: u8,
    pub ver: u32,
    pub bg: u16,
    nodes: [Node; MAX_NODES],
    len: u8,
}

impl Layout {
    pub fn nodes(&self) -> &[Node] {
        &self.nodes[..self.len as usize]
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Parse one `ui` line (no trailing newline needed; it is ignored).
    ///
    /// Rejects the whole message rather than dropping bad nodes: a page that is
    /// missing one node looks like a rendering bug, and the daemon will resend
    /// `ui` anyway (PROTOCOL 16).
    pub fn parse(line: &[u8]) -> Result<Self, Error> {
        let top = json::Fields::of(trim_line(line)).map_err(|_| Error::NotUi)?;
        match top.get(b"t").and_then(json::as_str) {
            Some("ui") => {}
            _ => return Err(Error::NotUi),
        }

        let scene = top
            .get(b"scene")
            .and_then(json::as_i64)
            .unwrap_or(0)
            .clamp(0, 255) as u8;
        let ver = top
            .get(b"ver")
            .and_then(json::as_i64)
            .unwrap_or(0)
            .clamp(0, u32::MAX as i64) as u32;
        let bg = match top.get(b"bg") {
            Some(raw) => colour(raw)?,
            None => 0x0000,
        };

        let nodes_raw = top.get(b"nodes").ok_or(Error::NoNodes)?;
        let array = json::Array::of(nodes_raw).map_err(|_| Error::NoNodes)?;

        let mut layout = Layout {
            scene,
            ver,
            bg,
            nodes: [Node::EMPTY; MAX_NODES],
            len: 0,
        };
        for item in array {
            if layout.len as usize == MAX_NODES {
                return Err(Error::TooManyNodes);
            }
            let node = parse_node(item)?;
            layout.nodes[layout.len as usize] = node;
            layout.len += 1;
        }
        Ok(layout)
    }

    /// Draw the page onto a canvas.
    ///
    /// Fills the background first: a page that only clears the rectangle it
    /// draws over would leave the previous page's pixels in the gaps, which is
    /// exactly the kind of ghosting that looks like a broken panel.
    pub fn render(&self, canvas: &mut Canvas<'_>, values: &Values) {
        canvas.clear(self.bg);
        let (cw, ch) = (canvas.width(), canvas.height());
        for node in self.nodes() {
            match node.kind {
                Kind::Rect => {
                    if let Some(border) = node.border {
                        fill(canvas, cw, ch, node.x, node.y, node.w, node.h, border);
                        // Inset by one pixel so the fill does not eat the outline
                        // when w/h are small.
                        if node.w > 2 && node.h > 2 {
                            fill(canvas, cw, ch, node.x + 1, node.y + 1, node.w - 2, node.h - 2, node.colour);
                        }
                    } else {
                        fill(canvas, cw, ch, node.x, node.y, node.w, node.h, node.colour);
                    }
                }
                // Text is placed by its top-left cell; a cell that starts off the
                // panel is dropped rather than half-drawn, because half a glyph
                // reads as a corruption. Partial cells are a T6 open point.
                Kind::Text => {
                    if node.x >= 0 && node.y >= 0 {
                        canvas.text(node.x as usize, node.y as usize, node.text_str().as_bytes(), node.colour)
                    }
                }
                Kind::Num => {
                    if node.x >= 0 && node.y >= 0 {
                        let mut buf = [0u8; 24];
                        let len = format_number(values.get(&node.bind), node.dec, node.unit_str(), &mut buf);
                        canvas.text(node.x as usize, node.y as usize, &buf[..len], node.colour)
                    }
                }
            }
        }
    }
}

/// Fill a node rectangle, clipped to the canvas.
///
/// `Canvas::fill_rect` speaks `usize` and treats off-canvas as a bug; a page may
/// legally place a rect partly outside (PROTOCOL 7 only says "absolute
/// coordinates"), so the clipping happens here, once, for both the fill and the
/// outline.
fn fill(canvas: &mut Canvas<'_>, cw: usize, ch: usize, x: i16, y: i16, w: u16, h: u16, colour: u16) {
    let x0 = (x as i32).max(0);
    let y0 = (y as i32).max(0);
    let x1 = (x as i32 + w as i32).min(cw as i32);
    let y1 = (y as i32 + h as i32).min(ch as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    canvas.fill_rect(x0 as usize, y0 as usize, (x1 - x0) as usize, (y1 - y0) as usize, colour);
}

fn trim_line(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &line[..end]
}

fn parse_node(raw: &[u8]) -> Result<Node, Error> {
    let f = json::Fields::of(raw).map_err(|_| Error::BadField)?;
    let kind = match f.get(b"type").and_then(json::as_str) {
        Some("rect") => Kind::Rect,
        Some("text") => Kind::Text,
        Some("num") => Kind::Num,
        // `path`, `line`, `bar`, `dot`, or a typo: the page is refused whole,
        // because a silently missing element is worse than a refused page --
        // the daemon resends `ui` on the next handshake.
        _ => return Err(Error::Unsupported),
    };

    let mut node = Node { kind, ..Node::EMPTY };
    node.x = int(f.get(b"x"))?;
    node.y = int(f.get(b"y"))?;

    match kind {
        Kind::Rect => {
            node.w = uint(f.get(b"w"))?;
            node.h = uint(f.get(b"h"))?;
            // `fill` is the inside, `color` the outline; either alone is legal.
            node.colour = match f.get(b"fill") {
                Some(raw) => colour(raw)?,
                None => match f.get(b"color") {
                    Some(raw) => colour(raw)?,
                    None => return Err(Error::BadField),
                },
            };
            node.border = match (f.get(b"fill").is_some(), f.get(b"color")) {
                (true, Some(raw)) => Some(colour(raw)?),
                _ => None,
            };
        }
        Kind::Text => {
            node.colour = match f.get(b"color") {
                Some(raw) => colour(raw)?,
                None => 0xFFFF,
            };
            let text = f.get(b"text").and_then(json::as_str).ok_or(Error::BadField)?;
            let bytes = text.as_bytes();
            if bytes.len() > MAX_TEXT {
                return Err(Error::TooLong);
            }
            node.text[..bytes.len()].copy_from_slice(bytes);
            node.text_len = bytes.len() as u8;
        }
        Kind::Num => {
            node.colour = match f.get(b"color") {
                Some(raw) => colour(raw)?,
                None => 0xFFFF,
            };
            let bind = f.get(b"bind").and_then(json::as_str).ok_or(Error::BadField)?;
            node.bind = Id::new(bind).ok_or(Error::TooLong)?;
            node.dec = f
                .get(b"dec")
                .and_then(json::as_i64)
                .unwrap_or(0)
                .clamp(0, 3) as u8;
            if let Some(unit) = f.get(b"unit").and_then(json::as_str) {
                let bytes = unit.as_bytes();
                if bytes.len() > MAX_UNIT {
                    return Err(Error::TooLong);
                }
                node.unit[..bytes.len()].copy_from_slice(bytes);
                node.unit_len = bytes.len() as u8;
            }
        }
    }
    Ok(node)
}

fn int(raw: Option<&[u8]>) -> Result<i16, Error> {
    let missing = raw.ok_or(Error::BadField)?;
    let v = json::as_f64(missing).ok_or(Error::BadField)?;
    // Out-of-panel coordinates are legal (clipping handles them); only a value
    // that cannot fit an i16 is a broken message.
    if !(-32768.0..=32767.0).contains(&v) {
        return Err(Error::BadField);
    }
    Ok(v as i16)
}

fn uint(raw: Option<&[u8]>) -> Result<u16, Error> {
    let missing = raw.ok_or(Error::BadField)?;
    let v = json::as_f64(missing).ok_or(Error::BadField)?;
    if !(0.0..=65535.0).contains(&v) {
        return Err(Error::BadField);
    }
    Ok(v as u16)
}

/// `"#RRGGBB"` -> RGB565, 5 bits red, 6 green, 5 blue.
///
/// Both ends must use this function: the parser stores 565, and the PC preview
/// computes the same value from the same hex string, so a page can be compared
/// CRC for CRC.
pub fn colour(raw: &[u8]) -> Result<u16, Error> {
    let text = json::as_str(raw).ok_or(Error::BadColour)?;
    let bytes = text.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return Err(Error::BadColour);
    }
    let mut rgb = [0u8; 3];
    for (i, chunk) in bytes[1..].chunks(2).enumerate() {
        rgb[i] = (hex(chunk[0])? << 4) | hex(chunk[1])?;
    }
    Ok(to_rgb565(rgb[0], rgb[1], rgb[2]))
}

fn hex(byte: u8) -> Result<u8, Error> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(Error::BadColour),
    }
}

pub fn to_rgb565(r: u8, g: u8, b: u8) -> u16 {
    (((r as u16) & 0xF8) << 8) | (((g as u16) & 0xFC) << 3) | ((b as u16) >> 3)
}

/// Format one reading for a `num` node: `--` when there is no value, otherwise
/// `dec` decimals plus the unit, e.g. `23.4%` or `61C`.
///
/// Deliberately integer arithmetic: `f64` formatting would pull in `core::fmt`'s
/// float machinery, which is several KB of code for three decimal places.
pub fn format_number(value: Option<f64>, dec: u8, unit: &str, out: &mut [u8]) -> usize {
    let Some(value) = value else {
        return put(out, b"--");
    };
    let dec = dec.min(3) as usize;
    let scale = [1i64, 10, 100, 1000][dec];
    let rounded = round_to_i64(value * scale as f64);
    let negative = rounded < 0;
    let magnitude = rounded.unsigned_abs();

    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut rest = magnitude;
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    let count = digits.len() - at;

    let mut n = 0usize;
    if negative {
        n += put(&mut out[n..], b"-");
    }
    if dec == 0 {
        n += put(&mut out[n..], &digits[at..]);
    } else {
        // Pad with leading zeros so `0.05` prints as `0.05`, not `.5`.
        if count <= dec {
            n += put(&mut out[n..], b"0.");
            for _ in 0..(dec - count) {
                n += put(&mut out[n..], b"0");
            }
            n += put(&mut out[n..], &digits[at..]);
        } else {
            let split = count - dec;
            n += put(&mut out[n..], &digits[at..at + split]);
            n += put(&mut out[n..], b".");
            n += put(&mut out[n..], &digits[at + split..]);
        }
    }
    n += put(&mut out[n..], unit.as_bytes());
    n
}

fn round_to_i64(v: f64) -> i64 {
    // `f64::round` is in core on this toolchain, but NaN/inf are not inputs the
    // protocol can produce; clamping keeps a corrupt value from becoming UB.
    if !v.is_finite() {
        return 0;
    }
    let r = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    (r as i64).clamp(-999_999_999, 999_999_999)
}

fn put(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n
}

/// The latest readings, keyed by metric id (PROTOCOL 9).
///
/// A missing key means "no update this round" and the old value stays; an
/// explicit `null` clears it so the node prints `--`. Both cases are why the
/// value is `Option<f64>` and not a bare number.
#[derive(Clone, Copy)]
pub struct Values {
    entries: [(Id, Option<f64>); MAX_NODES],
    len: u8,
}

impl Values {
    pub const EMPTY: Values = Values { entries: [(Id { bytes: [0; MAX_ID], len: 0 }, None); MAX_NODES], len: 0 };

    pub fn get(&self, id: &Id) -> Option<f64> {
        self.entries[..self.len as usize]
            .iter()
            .find(|(k, _)| k == id)
            .and_then(|(_, v)| *v)
    }

    /// A `data` message: `{"t":"data","v":{"cpu.total":23.4,"gpu.temp":null}}`.
    ///
    /// Keys not mentioned are left alone (PROTOCOL 9), which is what keeps a
    /// number from blinking to `--` because a slower plugin missed a round.
    pub fn apply_data(&mut self, line: &[u8]) -> Result<usize, Error> {
        let top = json::Fields::of(trim_line(line)).map_err(|_| Error::NotUi)?;
        match top.get(b"t").and_then(json::as_str) {
            Some("data") => {}
            _ => return Err(Error::NotUi),
        }
        let values_raw = top.get(b"v").ok_or(Error::BadField)?;
        let fields = json::Fields::of(values_raw).map_err(|_| Error::BadField)?;

        let mut updated = 0usize;
        for field in fields.iter() {
            let Some(id) = Id::new(field.key) else {
                // An over-long id cannot be in this page (MAX_ID is larger than
                // any metric name the daemon publishes); skipping it keeps the
                // rest of the message useful.
                continue;
            };
            let value = if json::is_null(field.raw) {
                None
            } else {
                match json::as_f64(field.raw) {
                    Some(v) => Some(v),
                    None => continue,
                }
            };
            self.set(id, value);
            updated += 1;
        }
        Ok(updated)
    }

    /// Store one reading, appending an entry the first time an id shows up.
    ///
    /// When the table is full the oldest entry is reused: a page cannot bind
    /// more than MAX_NODES metrics, so a message that overflows it is either a
    /// different page's data or a daemon bug, and dropping the newest value
    /// would be the worse of the two failures.
    pub fn set(&mut self, id: Id, value: Option<f64>) {
        for entry in self.entries[..self.len as usize].iter_mut() {
            if entry.0 == id {
                entry.1 = value;
                return;
            }
        }
        if (self.len as usize) < MAX_NODES {
            self.entries[self.len as usize] = (id, value);
            self.len += 1;
        } else {
            self.entries[0] = (id, value);
        }
    }

    /// How many ids are being tracked (tests and the diagnostics line).
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// What one protocol line did to the page.
///
/// `Ignored` is the common case and not an error: `catalog`, `ping`, `pong`
/// and `cmd` all reach the same dispatcher, and none of them say anything about
/// the picture. `Refused` is the interesting one -- the page on the glass stays
/// as it was, because half a page is worse than a stale one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feed {
    /// A new layout was accepted; the screen must be repainted.
    Layout,
    /// `data` moved `n` values (0 = nothing tracked changed).
    Data(usize),
    /// A `ui`/`data` line that could not be honoured.
    Refused(Error),
    /// Not a `ui` or `data` line at all.
    Ignored,
}

/// The page on the glass: the last accepted layout plus the newest readings.
///
/// This is the whole of T6's state. It exists as one type so the render task
/// owns *one* thing instead of juggling a layout and a value table separately,
/// and so the rules below can be tested on a PC instead of discovered on the
/// glass.
pub struct Page {
    layout: Option<Layout>,
    values: Values,
    dirty: bool,
}

impl Page {
    pub const fn new() -> Self {
        Self { layout: None, values: Values::EMPTY, dirty: false }
    }

    /// Take one protocol line. This is the only way the page changes.
    pub fn feed(&mut self, line: &[u8]) -> Feed {
        match Layout::parse(line) {
            Ok(layout) => {
                // Values deliberately survive a layout change: they are the same
                // readings, only the boxes move. Clearing them would blank every
                // number on a scene switch until the next `data`, which on a 1 Hz
                // stream is a visible flicker for no reason.
                self.layout = Some(layout);
                self.dirty = true;
                Feed::Layout
            }
            // `t` is neither `ui`...
            Err(Error::NotUi) => match self.values.apply_data(line) {
                Ok(n) => {
                    // Only a page that exists can be moved by new readings. A
                    // daemon whose `ui` was refused keeps streaming `data`, and
                    // repainting the self-test card once a second for numbers
                    // nothing binds is 25 KB of SPI per second for no pixels.
                    if n > 0 && self.layout.is_some() {
                        self.dirty = true;
                    }
                    Feed::Data(n)
                }
                // ...nor `data`: a `catalog`, a `ping`, a `cmd`.
                Err(Error::NotUi) => Feed::Ignored,
                Err(err) => Feed::Refused(err),
            },
            Err(err) => Feed::Refused(err),
        }
    }

    /// True once a page has been accepted: the self-test card must give way.
    pub fn has_layout(&self) -> bool {
        self.layout.is_some()
    }

    pub fn layout(&self) -> Option<&Layout> {
        self.layout.as_ref()
    }

    pub fn values(&self) -> &Values {
        &self.values
    }

    /// Draw the page. A no-op before the first layout, so the caller does not
    /// have to guard it.
    pub fn render(&self, canvas: &mut Canvas<'_>) {
        if let Some(layout) = &self.layout {
            layout.render(canvas, &self.values);
        }
    }

    /// Edge-trigger for the render loop: a repaint costs 25 KB of SPI, so the
    /// task must know whether anything actually changed.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `r##` because the page itself contains `"#`, which would end an `r#`
    // string at the first colour literal.
    const PAGE: &str = r##"{"t":"ui","s":2,"scene":0,"ver":7,"bg":"#000000","nodes":[
        {"type":"text","x":0,"y":0,"text":"CPU","color":"#888888"},
        {"type":"num","x":0,"y":16,"bind":"cpu.total","dec":0,"unit":"%","color":"#FFFFFF"},
        {"type":"rect","x":0,"y":32,"w":160,"h":6,"fill":"#222222"},
        {"type":"rect","x":0,"y":32,"w":80,"h":6,"fill":"#44DD88"}
    ]}"##;

    #[test]
    fn parses_the_protocol_example() {
        let layout = Layout::parse(PAGE.as_bytes()).expect("valid page");
        assert_eq!(layout.scene, 0);
        assert_eq!(layout.ver, 7);
        assert_eq!(layout.bg, 0x0000);
        assert_eq!(layout.len(), 4);
        let nodes = layout.nodes();
        assert_eq!(nodes[0].kind, Kind::Text);
        assert_eq!(nodes[0].text_str(), "CPU");
        assert_eq!(nodes[1].kind, Kind::Num);
        assert_eq!(nodes[1].bind.as_str(), "cpu.total");
        assert_eq!(nodes[1].dec, 0);
        assert_eq!(nodes[1].unit_str(), "%");
        assert_eq!(nodes[2].kind, Kind::Rect);
        assert_eq!((nodes[2].w, nodes[2].h), (160, 6));
    }

    #[test]
    fn colours_round_to_rgb565_the_way_the_preview_does() {
        assert_eq!(colour(b"\"#FFFFFF\""), Ok(0xFFFF));
        assert_eq!(colour(b"\"#000000\""), Ok(0x0000));
        assert_eq!(colour(b"\"#F80000\""), Ok(0xF800));
        assert_eq!(colour(b"\"#00FF00\""), Ok(0x07E0));
        assert_eq!(colour(b"\"#0000F8\""), Ok(0x001F));
        // Lowercase is legal, and the low bits round like the PC side.
        assert_eq!(colour(b"\"#ffb7c5\""), Ok(to_rgb565(0xFF, 0xB7, 0xC5)));
        // Anything else is a broken message, not a default colour.
        for bad in [&b"\"#FFF\""[..], b"\"FFF\"" , b"\"#GGGGGG\"", b"42"] {
            assert_eq!(colour(bad), Err(Error::BadColour), "{:?}", core::str::from_utf8(bad));
        }
    }

    #[test]
    fn a_page_with_an_unsupported_primitive_is_refused_whole() {
        // PROTOCOL 7's own `path` example: refused until T7 implements it, so
        // the daemon resends rather than the panel drawing a partial page.
        let line = br#"{"t":"ui","ver":1,"nodes":[{"type":"path","x":8,"y":66,"pts":[0,0,10,10]}]}"#;
        assert_eq!(Layout::parse(line).err(), Some(Error::Unsupported));
    }

    #[test]
    fn too_many_nodes_is_an_error_not_a_truncation() {
        let mut line = String::from(r#"{"t":"ui","ver":1,"nodes":["#);
        for i in 0..=MAX_NODES {
            if i > 0 {
                line.push(',');
            }
            line.push_str(r#"{"type":"text","x":0,"y":0,"text":"x"}"#);
        }
        line.push_str("]}");
        assert_eq!(Layout::parse(line.as_bytes()).err(), Some(Error::TooManyNodes));
    }

    #[test]
    fn text_longer_than_the_panel_is_refused() {
        let wide = "x".repeat(MAX_TEXT + 1);
        let line = std::format!(
            r#"{{"t":"ui","ver":1,"nodes":[{{"type":"text","x":0,"y":0,"text":"{wide}"}}]}}"#
        );
        assert_eq!(Layout::parse(line.as_bytes()).err(), Some(Error::TooLong));
    }

    #[test]
    fn a_missing_coordinate_is_a_bad_field() {
        let line = br#"{"t":"ui","ver":1,"nodes":[{"type":"text","y":0,"text":"x"}]}"#;
        assert_eq!(Layout::parse(line).err(), Some(Error::BadField));
    }

    #[test]
    fn not_a_ui_message_is_reported_as_such() {
        assert_eq!(Layout::parse(br#"{"t":"catalog","ver":1}"#).err(), Some(Error::NotUi));
        assert_eq!(Layout::parse(b"garbage").err(), Some(Error::NotUi));
        assert_eq!(Layout::parse(br#"{"t":"ui"}"#).err(), Some(Error::NoNodes));
    }

    #[test]
    fn rendering_clears_the_background_first() {
        let layout = Layout::parse(PAGE.as_bytes()).unwrap();
        let mut values = Values::EMPTY;
        values.set(Id::new("cpu.total").unwrap(), Some(23.4));
        let canvas = render(&layout, &values);
        // `#888888` text at y=0..16: some ink somewhere inside the first cell
        // (the glyph table has blank rows at the top, so a fixed row would be a
        // test of the font, not of the renderer).
        let inked = (0..8).any(|x| (0..16).any(|y| canvas.get(x, y) != Some(0x0000)));
        assert!(inked, "the CPU label should have drawn something");
        // The two rects at y=32: the right half is the darker one.
        assert_eq!(canvas.get(10, 34), Some(colour(b"\"#44DD88\"").unwrap()));
        assert_eq!(canvas.get(150, 34), Some(colour(b"\"#222222\"").unwrap()));
        // Everything below the page stays background.
        assert_eq!(canvas.get(159, 79), Some(0x0000));
    }

    fn render(layout: &Layout, values: &Values) -> Canvas<'static> {
        let buf: &'static mut [u8] = Box::leak(vec![0u8; 160 * 80 * 2].into_boxed_slice());
        let mut canvas = Canvas::new(buf, 160, 80);
        layout.render(&mut canvas, values);
        canvas
    }

    #[test]
    fn numbers_format_with_decimals_and_units() {
        let mut out = [0u8; 24];
        let n = format_number(Some(23.4), 0, "%", &mut out);
        assert_eq!(&out[..n], b"23%");
        let n = format_number(Some(23.45), 1, "%", &mut out);
        assert_eq!(&out[..n], b"23.5%");
        let n = format_number(Some(61.0), 0, "C", &mut out);
        assert_eq!(&out[..n], b"61C");
        let n = format_number(Some(0.05), 2, "", &mut out);
        assert_eq!(&out[..n], b"0.05");
        let n = format_number(Some(-3.5), 1, "V", &mut out);
        assert_eq!(&out[..n], b"-3.5V");
        let n = format_number(None, 0, "%", &mut out);
        assert_eq!(&out[..n], b"--", "a null reading is two dashes, not zero");
    }

    #[test]
    fn data_updates_only_the_keys_it_mentions() {
        let mut values = Values::EMPTY;
        let n = values
            .apply_data(br#"{"t":"data","v":{"cpu.total":23.4,"cpu.temp":61.0}}"#)
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(values.get(&Id::new("cpu.total").unwrap()), Some(23.4));

        // A later message that omits cpu.temp must leave it alone (PROTOCOL 9).
        values
            .apply_data(br#"{"t":"data","v":{"cpu.total":30.0}}"#)
            .unwrap();
        assert_eq!(values.get(&Id::new("cpu.total").unwrap()), Some(30.0));
        assert_eq!(values.get(&Id::new("cpu.temp").unwrap()), Some(61.0));

        // An explicit null clears it, so the node prints `--`.
        values
            .apply_data(br#"{"t":"data","v":{"cpu.temp":null}}"#)
            .unwrap();
        assert_eq!(values.get(&Id::new("cpu.temp").unwrap()), None);
    }

    #[test]
    fn data_ignores_unknown_shapes_without_panicking() {
        let mut values = Values::EMPTY;
        assert_eq!(
            values.apply_data(br#"{"t":"catalog","ver":1}"#).err(),
            Some(Error::NotUi)
        );
        assert_eq!(values.apply_data(b"{\"t\":\"data\"}").err(), Some(Error::BadField));
        // A value that is neither a number nor null is skipped, not fatal.
        values
            .apply_data(br#"{"t":"data","v":{"a":"nope","cpu.total":1}}"#)
            .unwrap();
        assert_eq!(values.get(&Id::new("cpu.total").unwrap()), Some(1.0));
    }

    #[test]
    fn the_value_table_does_not_grow_past_the_node_limit() {
        let mut values = Values::EMPTY;
        for i in 0..(MAX_NODES + 5) {
            let id = Id::new(&std::format!("m{i}")).unwrap();
            values.set(id, Some(i as f64));
        }
        assert_eq!(values.len(), MAX_NODES);
    }

    // -- Page: what the render task actually owns -------------------------

    const A_PAGE: &str = r##"{"t":"ui","scene":1,"ver":3,"bg":"#000000","nodes":[
        {"type":"num","x":0,"y":0,"bind":"cpu.total","dec":1,"unit":"%"}
    ]}"##;

    #[test]
    fn a_page_starts_empty_and_refuses_to_paint_before_a_layout() {
        let page = Page::new();
        assert!(!page.has_layout());
        assert!(!page.is_dirty());
        // Rendering before the first `ui` must not clear the canvas: the boot
        // card is what is on the glass, and a stray clear would black it out.
        let mut canvas = render(&Layout::parse(A_PAGE.as_bytes()).unwrap(), &Values::EMPTY);
        let before = canvas.digest();
        page.render(&mut canvas);
        assert_eq!(canvas.digest(), before);
    }

    #[test]
    fn ui_replaces_the_page_and_data_moves_values() {
        let mut page = Page::new();
        assert_eq!(page.feed(A_PAGE.as_bytes()), Feed::Layout);
        assert!(page.has_layout());
        assert!(page.is_dirty());
        page.mark_clean();

        assert_eq!(page.feed(br#"{"t":"data","v":{"cpu.total":42.5}}"#), Feed::Data(1));
        assert!(page.is_dirty());
        let id = Id::new("cpu.total").unwrap();
        assert_eq!(page.values().get(&id), Some(42.5));

        // A `data` round that mentions a key no node binds still counts and
        // still repaints: `Values` keeps every id the daemon sends (a page may
        // bind it later without a new `ui`), and one extra 25 KB blit per data
        // round is ~1% of SPI time at the protocol's 1 Hz. Being exact here
        // would cost a per-node scan on every message to save that 1%.
        page.mark_clean();
        assert_eq!(page.feed(br#"{"t":"data","v":{"other":1}}"#), Feed::Data(1));
        assert!(page.is_dirty());
    }

    #[test]
    fn values_survive_a_layout_change() {
        // A scene switch moves the boxes; the readings are the same readings.
        // Clearing them would blank every number until the next `data`.
        let mut page = Page::new();
        page.feed(A_PAGE.as_bytes());
        page.feed(br#"{"t":"data","v":{"cpu.total":42.5}}"#);
        page.feed(br##"{"t":"ui","scene":2,"ver":4,"bg":"#000000","nodes":[
            {"type":"num","x":0,"y":16,"bind":"cpu.total","dec":1,"unit":"%"}
        ]}"##);
        assert_eq!(page.values().get(&Id::new("cpu.total").unwrap()), Some(42.5));
        assert_eq!(page.layout().unwrap().scene, 2);
    }

    #[test]
    fn a_refused_layout_leaves_the_old_page_on_the_glass() {
        let mut page = Page::new();
        page.feed(A_PAGE.as_bytes());
        page.mark_clean();
        let bad = br##"{"t":"ui","scene":0,"ver":9,"bg":"#000000","nodes":[
            {"type":"path","x":0,"y":0,"pts":[[0,0],[10,10]]}
        ]}"##;
        assert_eq!(page.feed(bad), Feed::Refused(Error::Unsupported));
        assert!(
            !page.is_dirty(),
            "a refused page must not repaint: half a page looks like a render bug"
        );
        assert_eq!(page.layout().unwrap().ver, 3, "the old page is still the page");
    }

    #[test]
    fn data_without_a_page_does_not_repaint() {
        // The real daemon's `ui` is refused (T6 draws `rect`/`text`/`num` only, and
        // its page uses `path`), yet it keeps streaming `data`. Repainting the
        // self-test card once a second with numbers nothing binds is 25 KB of SPI
        // per second for an identical picture.
        let mut page = Page::new();
        assert_eq!(page.feed(br#"{"t":"data","v":{"cpu.total":42.5}}"#), Feed::Data(1));
        assert!(!page.is_dirty(), "no page on the glass means nothing to repaint");
        assert_eq!(
            page.values().get(&Id::new("cpu.total").unwrap()),
            Some(42.5),
            "the reading is still kept: a page that arrives later starts with it"
        );
    }

#[test]
fn catalog_and_ping_are_neither_layouts_nor_failures() {
    let mut page = Page::new();
        page.feed(A_PAGE.as_bytes());
        page.mark_clean();
        assert_eq!(page.feed(br#"{"t":"catalog","ver":2,"metrics":[]}"#), Feed::Ignored);
        assert_eq!(page.feed(br#"{"t":"ping"}"#), Feed::Ignored);
        assert_eq!(page.feed(br#"{"t":"cmd","c":"clear"}"#), Feed::Ignored);
        assert!(!page.is_dirty());
    }

    #[test]
    fn a_data_line_with_a_broken_number_is_refused_not_guessed() {
        let mut page = Page::new();
        page.feed(A_PAGE.as_bytes());
        page.mark_clean();
        assert_eq!(
            page.feed(br#"{"t":"data","v":"not an object"}"#),
            Feed::Refused(Error::BadField)
        );
        assert!(!page.is_dirty());
    }
}
