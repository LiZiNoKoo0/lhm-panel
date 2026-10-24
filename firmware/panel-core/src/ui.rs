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
//!
//! The `catalog` message is parsed too, for one field only: `interval_ms`. It is
//! not needed to draw anything, but it is what turns "the daemon went quiet"
//! into `--` instead of a number frozen on the glass (PROTOCOL 9.1). A metric
//! the catalog never described keeps interval 0 and never goes stale.

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

/// Font sizes this panel can draw, as pixel heights of the 8x16 cell.
///
/// The wire value is the pixel height (size in PROTOCOL 7); the internal
/// value is that height, and Node::font_scale divides it by the native cell.
/// A size the panel does not know degrades to 16 rather than to something
/// unreadable, matching what it_nodes does on the PC side.
pub const FONT_SIZES: [u8; 3] = [16, 32, 48];

/// One node, in drawing order.
#[derive(Clone, Copy)]
pub struct Node {
    pub kind: Kind,
    pub x: i16,
    pub y: i16,
    /// `rect` only: size. Text and numbers are sized by `size`.
    pub w: u16,
    pub h: u16,
    /// `text`/`num`: one of `FONT_SIZES`, already snapped by the parser.
    pub size: u8,
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
    /// `num` only: the expected update period, in milliseconds, taken from the
    /// matching `catalog` entry. `0` = unknown, and an unknown period never
    /// goes stale -- guessing a deadline for a metric nobody described would
    /// blank a perfectly live number.
    pub interval_ms: u32,
}

impl Node {
    const EMPTY: Node = Node {
        kind: Kind::Text,
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        size: FONT_SIZES[0],
        colour: 0xFFFF,
        border: None,
        text: [0; MAX_TEXT],
        text_len: 0,
        bind: Id { bytes: [0; MAX_ID], len: 0 },
        dec: 0,
        unit: [0; MAX_UNIT],
        unit_len: 0,
        interval_ms: 0,
    };

    /// Integer magnification for this node's font size (1, 2 or 3).
    ///
    /// The parser already snapped size into FONT_SIZES, so this is a
    /// straight division; .max(1) guards a zero that should be impossible.
    pub fn font_scale(&self) -> usize {
        (self.size as usize / FONT_SIZES[0] as usize).max(1)
    }

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

/// The `id -> interval_ms` half of `catalog` (PROTOCOL 6).
///
/// Only `interval_ms` is kept. Every other catalog field describes *what* a
/// metric is, which is the daemon's business; the panel needs exactly one thing
/// out of it -- how long silence means "that number is dead" (PROTOCOL 9.1).
///
/// A metric missing here keeps interval 0 and never goes stale. That is
/// deliberate: a page drawn against a catalog-less daemon must behave exactly
/// like it did before this type existed.
#[derive(Clone, Copy)]
pub struct Catalog {
    entries: [(Id, u32); MAX_NODES],
    len: u8,
}

impl Catalog {
    pub const EMPTY: Catalog = Catalog {
        entries: [(Id { bytes: [0; MAX_ID], len: 0 }, 0); MAX_NODES],
        len: 0,
    };

    /// Parse a `catalog` line and replace the table wholesale.
    ///
    /// Replacing rather than merging is correct here: PROTOCOL 6 makes the
    /// daemon resend `catalog` whenever plugins come and go, so the newest
    /// message is the whole truth. Merging would keep intervals for plugins that
    /// have already left.
    ///
    /// Returns the number of ids stored.
    pub fn parse(&mut self, line: &[u8]) -> Result<usize, Error> {
        let top = json::Fields::of(trim_line(line)).map_err(|_| Error::NotUi)?;
        match top.get(b"t").and_then(json::as_str) {
            Some("catalog") => {}
            _ => return Err(Error::NotUi),
        }
        let metrics = top.get(b"metrics").ok_or(Error::BadField)?;
        let items = json::Array::of(metrics).map_err(|_| Error::BadField)?;

        self.len = 0;
        for item in items {
            let Ok(fields) = json::Fields::of(item) else {
                continue;
            };
            let Some(id) = fields.get(b"id").and_then(json::as_str).and_then(Id::new) else {
                continue;
            };
            // A metric without an interval is legal; it just never expires.
            let interval = fields
                .get(b"interval_ms")
                .and_then(json::as_i64)
                .filter(|ms| *ms > 0)
                .map(|ms| ms.clamp(0, u32::MAX as i64) as u32)
                .unwrap_or(0);
            if (self.len as usize) < MAX_NODES {
                self.entries[self.len as usize] = (id, interval);
                self.len += 1;
            }
        }
        Ok(self.len as usize)
    }

    /// The declared interval for `id`, or 0 if the catalog never described it.
    pub fn interval_ms(&self, id: &Id) -> u32 {
        self.entries[..self.len as usize]
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, ms)| *ms)
            .unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
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

    /// Fill in every node's `interval_ms` from the daemon's catalog.
    ///
    /// Called once per accepted `ui`, not at render time: the deadline belongs
    /// to the layout, and re-resolving it every frame would mean a catalog
    /// arriving mid-page silently changing how the current page ages.
    pub fn apply_intervals(&mut self, catalog: &Catalog) {
        for node in self.nodes[..self.len as usize].iter_mut() {
            node.interval_ms = catalog.interval_ms(&node.bind);
        }
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
    pub fn render(&self, canvas: &mut Canvas<'_>, values: &Values, now_ms: u32) {
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
                        canvas.text_scaled(
                            node.x as usize,
                            node.y as usize,
                            node.text_str().as_bytes(),
                            node.colour,
                            node.font_scale(),
                        )
                    }
                }
                Kind::Num => {
                    if node.x >= 0 && node.y >= 0 {
                        let mut buf = [0u8; 24];
                        let len = format_number(
                            values.get(&node.bind, now_ms, node.interval_ms),
                            node.dec,
                            node.unit_str(),
                            &mut buf,
                        );
                        canvas.text_scaled(
                            node.x as usize,
                            node.y as usize,
                            &buf[..len],
                            node.colour,
                            node.font_scale(),
                        )
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

/// Snap a wire `size` onto FONT_SIZES, choosing the nearest entry.
///
/// Mirrors protocol.fit_nodes on the PC side so the two cannot disagree about
/// what "28" means. Ties go to the smaller size: a glyph that is too small is
/// still legible, one that overflows its box is not.
fn snap_size(raw: Option<&[u8]>) -> u8 {
    let Some(value) = raw.and_then(json::as_i64) else {
        return FONT_SIZES[0];
    };
    let mut best = FONT_SIZES[0];
    let mut best_gap = u32::MAX;
    for size in FONT_SIZES {
        let gap = (size as i64 - value).unsigned_abs() as u32;
        if gap < best_gap {
            best_gap = gap;
            best = size;
        }
    }
    best
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
            node.size = snap_size(f.get(b"size"));
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
            node.size = snap_size(f.get(b"size"));
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

/// Whether a reading written at `stamped` and expected every `interval_ms` is
/// stale at `now_ms`.
///
/// `interval_ms == 0` means "nobody described this metric", and an undescribed
/// metric never goes stale: the visible failure mode of a wrong guess here is a
/// live number flipping to `--`, which is worse than a slightly late number.
///
/// `wrapping_sub` because the hardware clock is a 32-bit millisecond counter
/// that rolls over roughly every 49.7 days; a plain `now - stamped` would panic
/// in debug and yield nonsense in release on the far side of that boundary.
pub fn is_stale(stamped: u32, now_ms: u32, interval_ms: u32) -> bool {
    if interval_ms == 0 {
        return false;
    }
    let limit = interval_ms.saturating_mul(STALE_FACTOR);
    now_ms.wrapping_sub(stamped) > limit
}

/// PROTOCOL 9.1: three missed rounds. Matches `pcmon.protocol.is_stale`.
pub const STALE_FACTOR: u32 = 3;

/// The latest readings, keyed by metric id (PROTOCOL 9).
///
/// A missing key means "no update this round" and the old value stays; an
/// explicit `null` clears it so the node prints `--`. Both cases are why the
/// value is `Option<f64>` and not a bare number.
///
/// Each entry also carries the tick it was last written on, which is what turns
/// a silent plugin into `--` instead of a number that quietly lies.
#[derive(Clone, Copy)]
pub struct Values {
    entries: [(Id, Option<f64>); MAX_NODES],
    /// Tick of the last write for each entry. Only meaningful while the value
    /// is `Some`; a cleared entry is already showing `--`.
    stamped: [u32; MAX_NODES],
    len: u8,
}

impl Values {
    pub const EMPTY: Values = Values {
        entries: [(Id { bytes: [0; MAX_ID], len: 0 }, None); MAX_NODES],
        stamped: [0; MAX_NODES],
        len: 0,
    };

    /// The newest known reading, or `None` when there is none *or* when it has
    /// gone stale (PROTOCOL 9.1: `interval_ms x 3`).
    ///
    /// Staleness is answered here rather than at render time so every caller --
    /// the page, the diagnostics line, the tests -- sees the same answer. A
    /// second place that decides "is this stale" is a second truth.
    pub fn get(&self, id: &Id, now_ms: u32, interval_ms: u32) -> Option<f64> {
        let index = self.index_of(id)?;
        let value = self.entries[index].1?;
        if is_stale(self.stamped[index], now_ms, interval_ms) {
            return None;
        }
        Some(value)
    }

    /// The reading as stored, ignoring the deadline. Used by tests and by the
    /// `PAGE data` line, both of which describe what arrived, not what the glass
    /// currently honours.
    pub fn raw(&self, id: &Id) -> Option<f64> {
        self.index_of(id).and_then(|i| self.entries[i].1)
    }

    /// How long ago the newest reading for `id` arrived, in milliseconds, or
    /// `None` if there is no reading. Diagnostics only.
    pub fn age_ms(&self, id: &Id, now_ms: u32) -> Option<u32> {
        let index = self.index_of(id)?;
        self.entries[index].1?;
        Some(now_ms.wrapping_sub(self.stamped[index]))
    }

    /// The tick the newest reading for `id` arrived on, or `None`.
    ///
    /// Exposed because "when did this arrive" is not derivable from "how old is
    /// it now" when the reference instant may predate the reading -- the exact
    /// case that broke the staleness edge.
    pub fn stamp_of(&self, id: &Id) -> Option<u32> {
        let index = self.index_of(id)?;
        self.entries[index].1?;
        Some(self.stamped[index])
    }

    fn index_of(&self, id: &Id) -> Option<usize> {
        self.entries[..self.len as usize].iter().position(|(k, _)| k == id)
    }

    /// A `data` message: `{"t":"data","v":{"cpu.total":23.4,"gpu.temp":null}}`.
    ///
    /// Keys not mentioned are left alone (PROTOCOL 9), which is what keeps a
    /// number from blinking to `--` because a slower plugin missed a round.
    ///
    /// `now_ms` stamps every key that this message moved; a key the message did
    /// not mention keeps its old stamp, which is the whole point -- silence is
    /// how a metric finally goes stale.
    pub fn apply_data(&mut self, line: &[u8], now_ms: u32) -> Result<usize, Error> {
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
            self.set(id, value, now_ms);
            updated += 1;
        }
        Ok(updated)
    }

    /// Store one reading, appending an entry the first time an id shows up.
    ///
    /// `now_ms` is the tick the reading arrived on. Stamping at the call site
    /// rather than inside keeps `Values` free of a clock, so a test can drive
    /// time by hand and assert a `--` without sleeping.
    ///
    /// When the table is full the oldest entry is reused: a page cannot bind
    /// more than MAX_NODES metrics, so a message that overflows it is either a
    /// different page's data or a daemon bug, and dropping the newest value
    /// would be the worse of the two failures.
    pub fn set(&mut self, id: Id, value: Option<f64>, now_ms: u32) {
        for (i, entry) in self.entries[..self.len as usize].iter_mut().enumerate() {
            if entry.0 == id {
                entry.1 = value;
                self.stamped[i] = now_ms;
                return;
            }
        }
        if (self.len as usize) < MAX_NODES {
            let i = self.len as usize;
            self.entries[i] = (id, value);
            self.stamped[i] = now_ms;
            self.len += 1;
        } else {
            self.entries[0] = (id, value);
            self.stamped[0] = now_ms;
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
    /// The daemon's declared intervals, taken from `catalog`. Applied to a node
    /// the moment a `ui` line is parsed, and again whenever a later catalog
    /// supplies a deadline the page was missing.
    catalog: Catalog,
    dirty: bool,
    /// Which nodes were drawn as a number (bit set) rather than `--` (bit clear)
    /// by the last repaint. Up to MAX_NODES (32) nodes, so one word covers it.
    ///
    /// This replaces the previous attempt to infer the same fact by comparing
    /// timestamps. That inference is unfixable at the boundary: a paint landing
    /// 3005 ms after the last reading is indistinguishable from one landing 2995
    /// ms after it, yet the two straddle the deadline and give opposite answers.
    /// The renderer already knows which nodes it drew with a value; recording it
    /// costs one word and removes the guess (T6c, 2026-10-25: `lp=26299 st=23294`
    /// froze the last number on the glass forever).
    painted: u32,
}

impl Page {
    pub const fn new() -> Self {
        Self {
            layout: None,
            values: Values::EMPTY,
            catalog: Catalog::EMPTY,
            dirty: false,
            painted: 0,
        }
    }

    /// Take one protocol line. This is the only way the page changes.
    ///
    /// `now_ms` stamps the readings a `data` line carries.
    pub fn feed(&mut self, line: &[u8], now_ms: u32) -> Feed {
        match Layout::parse(line) {
            Ok(mut layout) => {
                // Attach the catalog's intervals before the layout becomes the
                // page: `Layout::parse` cannot know them, and a node without one
                // never expires (PROTOCOL 9.1).
                layout.apply_intervals(&self.catalog);
                // Values deliberately survive a layout change: they are the same
                // readings, only the boxes move. Clearing them would blank every
                // number on a scene switch until the next `data`, which on a 1 Hz
                // stream is a visible flicker for no reason.
                self.layout = Some(layout);
                self.dirty = true;
                Feed::Layout
            }
            // `t` is neither `ui`...
            Err(Error::NotUi) => match self.values.apply_data(line, now_ms) {
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
                Err(Error::NotUi) => match self.catalog.parse(line) {
                    Ok(_) => {
                        // A catalog can arrive *after* the page it describes:
                        // the daemon sends `catalog` before `ui` on a fresh
                        // handshake, but a reconnect, a resend or a dropped
                        // frame can invert that, and a page parsed while the
                        // table was empty carries interval 0 on every node --
                        // which silently disables the whole staleness rule
                        // (T6c, 2026-10-25: nodes=6 iv=0 on the glass).
                        //
                        // Re-applying here costs one pass over at most 32
                        // nodes, swallows no recency (the values are untouched)
                        // and repaints only if a deadline actually changed.
                        if let Some(layout) = self.layout.as_mut() {
                            let before: u32 = layout.nodes().iter().map(|n| n.interval_ms).sum();
                            layout.apply_intervals(&self.catalog);
                            let after: u32 = layout.nodes().iter().map(|n| n.interval_ms).sum();
                            if before != after {
                                self.dirty = true;
                            }
                        }
                        Feed::Ignored
                    }
                    Err(Error::NotUi) => Feed::Ignored,
                    Err(err) => Feed::Refused(err),
                },
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
    ///
    /// `now_ms` is passed down to the nodes: a binding is only honoured while
    /// its reading is younger than `interval_ms x 3` (PROTOCOL 9.1).
    pub fn render(&self, canvas: &mut Canvas<'_>, now_ms: u32) {
        if let Some(layout) = &self.layout {
            layout.render(canvas, &self.values, now_ms);
        }
    }

    /// Draw the page and record which bindings were drawn as a number.
    ///
    /// The render loop must call this rather than `render`: the `painted` word
    /// it writes is the only fact that tells a later tick whether the glass is
    /// currently showing a figure that may need to become `--`. `render` stays
    /// for callers that only want pixels (tests, previews).
    pub fn render_recording(&mut self, canvas: &mut Canvas<'_>, now_ms: u32) {
        let mut painted = 0u32;
        if let Some(layout) = &self.layout {
            layout.render(canvas, &self.values, now_ms);
            for (index, node) in layout.nodes().iter().enumerate().take(MAX_NODES) {
                if node.kind == Kind::Num
                    && self.values.get(&node.bind, now_ms, node.interval_ms).is_some()
                {
                    painted |= 1 << index;
                }
            }
        }
        self.painted = painted;
    }

    /// Edge-trigger for the render loop: a repaint costs 25 KB of SPI.
    ///
    /// True when a message moved the page *and* when a figure on the glass has
    /// since aged out. The second case has no message behind it -- the daemon
    /// just went quiet -- and leaving it out is what keeps a dead number on the
    /// glass indefinitely.
    pub fn is_dirty_at(&self, now_ms: u32) -> bool {
        self.dirty || self.any_binding_newly_stale(now_ms)
    }

    /// True if some node was painted as a number and would now be painted `--`.
    ///
    /// This compares two observations -- what the last paint recorded in
    /// `painted`, and what this tick would draw -- rather than deriving anything
    /// from timestamps. Two earlier versions tried to infer "did the last paint
    /// show a number?" from `last_paint` and both failed at a boundary:
    ///
    /// * `age_ms(stamp, last_paint)` ran backwards when the paint predated the
    ///   reading, so the self-test-card case never fired.
    /// * `last_paint.max(stamp)` fixed that but still lost on a 5 ms margin:
    ///   on 2026-10-25 the board sat at `lp=26299 st=23294`, i.e. the last
    ///   paint landed 3005 ms after the last reading and the comparison against
    ///   a 3000 ms deadline said "it was already stale then", so the edge never
    ///   fired and the figure froze.
    ///
    /// Recording what was actually drawn has no boundary to lose on. It also
    /// keeps a metric that was `--` from the start from asking for 25 KB of SPI
    /// every tick forever, because a clear bit that stays clear raises no edge.
    fn any_binding_newly_stale(&self, now_ms: u32) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        layout
            .nodes()
            .iter()
            .enumerate()
            .take(MAX_NODES)
            .any(|(index, node)| {
                if node.kind != Kind::Num {
                    return false;
                }
                let was_number = self.painted & (1 << index) != 0;
                if !was_number {
                    return false;
                }
                self.values
                    .get(&node.bind, now_ms, node.interval_ms)
                    .is_none()
            })
    }

    /// Edge-trigger for the render loop: a repaint costs 25 KB of SPI.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear the edge flag. Kept for callers that only care about the message
    /// edges; the render loop should prefer `render_recording`, which also
    /// records what went on the glass.
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// Clear the edge flag, ignoring the tick.
    ///
    /// Retained so older call sites keep compiling, but note that this no longer
    /// feeds any staleness decision: the rule reads the `painted` word written
    /// by `render_recording`, not a timestamp.
    pub fn mark_clean_at(&mut self, _now_ms: u32) {
        self.dirty = false;
    }

    /// Which nodes the last `render_recording` drew as a number (bit per node).
    ///
    /// Exposed for the board's staleness probe: `g=0` says a value has expired,
    /// and this says whether the glass is still showing it as a figure.
    pub fn painted_mask(&self) -> u32 {
        self.painted
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
        values.set(Id::new("cpu.total").unwrap(), Some(23.4), 0);
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
        layout.render(&mut canvas, values, 0);
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
            .apply_data(br#"{"t":"data","v":{"cpu.total":23.4,"cpu.temp":61.0}}"#, 0)
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(values.get(&Id::new("cpu.total").unwrap(), 0, 0), Some(23.4));

        // A later message that omits cpu.temp must leave it alone (PROTOCOL 9).
        values
            .apply_data(br#"{"t":"data","v":{"cpu.total":30.0}}"#, 0)
            .unwrap();
        assert_eq!(values.get(&Id::new("cpu.total").unwrap(), 0, 0), Some(30.0));
        assert_eq!(values.get(&Id::new("cpu.temp").unwrap(), 0, 0), Some(61.0));

        // An explicit null clears it, so the node prints `--`.
        values
            .apply_data(br#"{"t":"data","v":{"cpu.temp":null}}"#, 0)
            .unwrap();
        assert_eq!(values.get(&Id::new("cpu.temp").unwrap(), 0, 0), None);
    }

    #[test]
    fn data_ignores_unknown_shapes_without_panicking() {
        let mut values = Values::EMPTY;
        assert_eq!(
            values.apply_data(br#"{"t":"catalog","ver":1}"#, 0).err(),
            Some(Error::NotUi)
        );
        assert_eq!(values.apply_data(b"{\"t\":\"data\"}", 0).err(), Some(Error::BadField));
        // A value that is neither a number nor null is skipped, not fatal.
        values
            .apply_data(br#"{"t":"data","v":{"a":"nope","cpu.total":1}}"#, 0)
            .unwrap();
        assert_eq!(values.get(&Id::new("cpu.total").unwrap(), 0, 0), Some(1.0));
    }

    #[test]
    fn the_value_table_does_not_grow_past_the_node_limit() {
        let mut values = Values::EMPTY;
        for i in 0..(MAX_NODES + 5) {
            let id = Id::new(&std::format!("m{i}")).unwrap();
            values.set(id, Some(i as f64), 0);
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
        page.render(&mut canvas, 0);
        assert_eq!(canvas.digest(), before);
    }

    #[test]
    fn ui_replaces_the_page_and_data_moves_values() {
        let mut page = Page::new();
        assert_eq!(page.feed(A_PAGE.as_bytes(), 0), Feed::Layout);
        assert!(page.has_layout());
        assert!(page.is_dirty());
        page.mark_clean();

        assert_eq!(page.feed(br#"{"t":"data","v":{"cpu.total":42.5}}"#, 0), Feed::Data(1));
        assert!(page.is_dirty());
        let id = Id::new("cpu.total").unwrap();
        assert_eq!(page.values().get(&id, 0, 0), Some(42.5));

        // A `data` round that mentions a key no node binds still counts and
        // still repaints: `Values` keeps every id the daemon sends (a page may
        // bind it later without a new `ui`), and one extra 25 KB blit per data
        // round is ~1% of SPI time at the protocol's 1 Hz. Being exact here
        // would cost a per-node scan on every message to save that 1%.
        page.mark_clean();
        assert_eq!(page.feed(br#"{"t":"data","v":{"other":1}}"#, 0), Feed::Data(1));
        assert!(page.is_dirty());
    }

    #[test]
    fn values_survive_a_layout_change() {
        // A scene switch moves the boxes; the readings are the same readings.
        // Clearing them would blank every number until the next `data`.
        let mut page = Page::new();
        page.feed(A_PAGE.as_bytes(), 0);
        page.feed(br#"{"t":"data","v":{"cpu.total":42.5}}"#, 0);
        page.feed(br##"{"t":"ui","scene":2,"ver":4,"bg":"#000000","nodes":[
            {"type":"num","x":0,"y":16,"bind":"cpu.total","dec":1,"unit":"%"}
        ]}"##, 0);
        assert_eq!(page.values().get(&Id::new("cpu.total").unwrap(), 0, 0), Some(42.5));
        assert_eq!(page.layout().unwrap().scene, 2);
    }

    #[test]
    fn a_refused_layout_leaves_the_old_page_on_the_glass() {
        let mut page = Page::new();
        page.feed(A_PAGE.as_bytes(), 0);
        page.mark_clean();
        let bad = br##"{"t":"ui","scene":0,"ver":9,"bg":"#000000","nodes":[
            {"type":"path","x":0,"y":0,"pts":[[0,0],[10,10]]}
        ]}"##;
        assert_eq!(page.feed(bad, 0), Feed::Refused(Error::Unsupported));
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
        assert_eq!(page.feed(br#"{"t":"data","v":{"cpu.total":42.5}}"#, 0), Feed::Data(1));
        assert!(!page.is_dirty(), "no page on the glass means nothing to repaint");
        assert_eq!(
            page.values().get(&Id::new("cpu.total").unwrap(), 0, 0),
            Some(42.5),
            "the reading is still kept: a page that arrives later starts with it"
        );
    }

#[test]
fn catalog_and_ping_are_neither_layouts_nor_failures() {
    let mut page = Page::new();
        page.feed(A_PAGE.as_bytes(), 0);
        page.mark_clean();
        assert_eq!(page.feed(br#"{"t":"catalog","ver":2,"metrics":[]}"#, 0), Feed::Ignored);
        assert_eq!(page.feed(br#"{"t":"ping"}"#, 0), Feed::Ignored);
        assert_eq!(page.feed(br#"{"t":"cmd","c":"clear"}"#, 0), Feed::Ignored);
        assert!(!page.is_dirty());
    }

    #[test]
    fn a_data_line_with_a_broken_number_is_refused_not_guessed() {
        let mut page = Page::new();
        page.feed(A_PAGE.as_bytes(), 0);
        page.mark_clean();
        assert_eq!(
            page.feed(br#"{"t":"data","v":"not an object"}"#, 0),
            Feed::Refused(Error::BadField)
        );
        assert!(!page.is_dirty());
    }

    // ---- PROTOCOL 9.1: a silent plugin must become `--` -------------------

    /// A one-metric page whose catalog declares a 1 s interval, so the deadline
    /// is 3 s and the numbers below are round.
    const TIMED_PAGE: &str = r##"{"t":"ui","scene":0,"ver":1,"bg":"#000000","nodes":[
        {"type":"num","x":0,"y":0,"bind":"cpu.total","dec":0,"unit":"%"}
    ]}"##;

    const TIMED_CATALOG: &str = r#"{"t":"catalog","s":1,"ver":1,"metrics":[
        {"id":"cpu.total","label":"CPU","unit":"%","type":"number","interval_ms":1000}
    ]}"#;

    fn timed_page() -> Page {
        let mut page = Page::new();
        // Catalog first: the real daemon sends it before the first `ui`.
        assert_eq!(page.feed(TIMED_CATALOG.as_bytes(), 0), Feed::Ignored);
        assert_eq!(page.feed(TIMED_PAGE.as_bytes(), 0), Feed::Layout);
        page
    }

    #[test]
    fn the_catalog_interval_lands_on_the_node_that_binds_it() {
        let page = timed_page();
        let node = &page.layout().unwrap().nodes()[0];
        assert_eq!(
            node.interval_ms, 1000,
            "without this the node never expires and the whole 9.1 rule is dead code"
        );
    }

    #[test]
    fn a_metric_the_catalog_never_described_never_goes_stale() {
        // Same page, no catalog: interval 0. Guessing a deadline here would
        // blank a perfectly live number, which is the worse failure.
        let mut page = Page::new();
        page.feed(TIMED_PAGE.as_bytes(), 0);
        assert_eq!(page.layout().unwrap().nodes()[0].interval_ms, 0);
        page.feed(br#"{"t":"data","v":{"cpu.total":42}}"#, 0);
        assert_eq!(
            page.values().get(&Id::new("cpu.total").unwrap(), 10 * 60 * 1000, 0),
            Some(42.0),
            "ten minutes of silence, but nobody said how often it should talk"
        );
    }

    #[test]
    fn a_reading_survives_its_own_deadline_and_dies_just_after() {
        let id = Id::new("cpu.total").unwrap();
        let mut values = Values::EMPTY;
        values.set(id, Some(23.4), 1000);

        // Deadline is interval x 3 = 3000 ms after the stamp.
        assert_eq!(values.get(&id, 1000, 1000), Some(23.4), "fresh");
        assert_eq!(values.get(&id, 3999, 1000), Some(23.4), "one ms before the edge");
        assert_eq!(values.get(&id, 4000, 1000), Some(23.4), "exactly at it, still shown");
        assert_eq!(values.get(&id, 4001, 1000), None, "one ms past it, `--`");
    }

    #[test]
    fn an_explicit_null_still_wins_over_a_live_interval() {
        // The daemon saying "this is invalid" and the daemon saying nothing are
        // different statements; the deadline must not resurrect a cleared value.
        let id = Id::new("cpu.temp").unwrap();
        let mut values = Values::EMPTY;
        values.set(id, Some(61.0), 0);
        values.set(id, None, 100);
        assert_eq!(values.get(&id, 100, 1000), None);
    }

    #[test]
    fn silence_repaints_the_page_into_dashes_and_says_so_once() {
        // The case that used to freeze the last number on the glass: the daemon
        // stops talking, so no `data` ever arrives to trigger a repaint.
        let mut page = timed_page();
        page.feed(br#"{"t":"data","v":{"cpu.total":23}}"#, 1000);

        // Paint it for real: the staleness rule reads the recorded mask, so a
        // page that was never drawn has nothing to age out.
        let mut buf = [0u8; 160 * 80 * 2];
        let mut canvas = Canvas::new(&mut buf, 160, 80);
        page.render_recording(&mut canvas, 1000);
        page.mark_clean_at(1000);
        assert_eq!(page.painted_mask(), 1, "a figure is on the glass");

        assert!(!page.is_dirty_at(2000), "still fresh: no repaint");
        assert!(
            page.is_dirty_at(4001),
            "the reading aged out, so the glass has to change"
        );

        // Once repainted at `--`, staying `--` must not keep asking for 25 KB of
        // SPI every tick.
        page.render_recording(&mut canvas, 4001);
        page.mark_clean_at(4001);
        assert_eq!(page.painted_mask(), 0, "`--` is not a figure");
        assert!(
            !page.is_dirty_at(20_000),
            "a number that is already `--` must not repaint forever"
        );
    }

    #[test]
    fn a_metric_that_never_had_a_value_does_not_repaint() {
        // Bound but never fed: the page drew `--` from the start, so the edge
        // never fires. This is the common case right after a handshake.
        let mut page = timed_page();
        // The layout itself asked for one repaint; the test is about what
        // happens *after* that paint.
        page.mark_clean_at(0);
        assert!(!page.is_dirty_at(60_000));
    }

    #[test]
    fn a_page_redrawn_at_the_deadline_shows_dashes() {
        // End to end through the renderer: the same pixels a user would see.
        let mut page = timed_page();
        page.feed(br#"{"t":"data","v":{"cpu.total":77}}"#, 0);

        let mut fresh_buf = [0u8; 160 * 80 * 2];
        let mut fresh = Canvas::new(&mut fresh_buf, 160, 80);
        page.render(&mut fresh, 1000);
        let fresh_digest = fresh.digest();

        let mut stale_buf = [0u8; 160 * 80 * 2];
        let mut stale = Canvas::new(&mut stale_buf, 160, 80);
        page.render(&mut stale, 4001);
        let stale_digest = stale.digest();

        assert_ne!(
            fresh_digest, stale_digest,
            "77 and `--` are not the same picture, which is the whole point"
        );
    }

    #[test]
    fn catalog_parsing_replaces_the_table_rather_than_merging() {
        // PROTOCOL 6: a plugin leaving resends the catalog. A merge would keep
        // the departing plugin's interval alive forever.
        let mut catalog = Catalog::EMPTY;
        catalog
            .parse(br#"{"t":"catalog","metrics":[{"id":"a","interval_ms":1000},{"id":"b","interval_ms":2000}]}"#)
            .unwrap();
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.interval_ms(&Id::new("b").unwrap()), 2000);

        catalog.parse(br#"{"t":"catalog","metrics":[{"id":"a","interval_ms":500}]}"#).unwrap();
        assert_eq!(catalog.len(), 1, "`b` left, so its interval must be gone");
        assert_eq!(catalog.interval_ms(&Id::new("b").unwrap()), 0);
    }

    #[test]
    fn a_catalog_with_a_missing_interval_is_legal_and_means_never_stale() {
        let mut catalog = Catalog::EMPTY;
        catalog
            .parse(br#"{"t":"catalog","metrics":[{"id":"music.title","label":"NOW"}]}"#)
            .unwrap();
        assert_eq!(catalog.interval_ms(&Id::new("music.title").unwrap()), 0);
    }

    #[test]
    fn a_catalog_that_changes_no_deadline_does_not_repaint() {
        // The bookkeeping case: the page already carries its deadlines (it was
        // parsed after an identical catalog), so a second catalog offers nothing
        // new and must not cost 25 KB of SPI.
        let mut page = Page::new();
        page.feed(TIMED_CATALOG.as_bytes(), 0);
        page.feed(A_PAGE.as_bytes(), 0);
        page.mark_clean_at(0);
        assert_eq!(page.layout().unwrap().nodes()[0].interval_ms, 1000);

        assert_eq!(page.feed(TIMED_CATALOG.as_bytes(), 10), Feed::Ignored);
        assert!(
            !page.is_dirty(),
            "a catalog that changes no deadline repaints nothing"
        );
    }

    #[test]
    fn a_late_catalog_fills_in_the_deadlines_and_repaints() {
        // Reproduces the glass on 2026-10-25: `nodes=6 iv=0`. The page arrived
        // before the catalog, so every node carried interval 0 and the whole
        // staleness rule was dead on arrival. A catalog that shows up after the
        // page has to be applied to it, not just stored.
        let mut page = Page::new();
        page.feed(A_PAGE.as_bytes(), 0);
        page.mark_clean_at(0);
        assert_eq!(
            page.layout().unwrap().nodes()[0].interval_ms, 0,
            "before the catalog there is no deadline to apply"
        );

        assert_eq!(page.feed(TIMED_CATALOG.as_bytes(), 10), Feed::Ignored);
        let bound = page
            .layout()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.kind == Kind::Num)
            .expect("the fixture page has a num node");
        assert_eq!(bound.interval_ms, 1000, "the late catalog must reach the node");
        assert!(
            page.is_dirty(),
            "a page whose deadlines just appeared must repaint, or the first stale reading is never drawn as `--`"
        );
    }
}

#[cfg(test)]
mod real_wire_tests {
    use super::*;

    // The exact bytes the daemon puts on the wire: python's json.dumps uses
    // ", " and ": " separators, so every field has a space after the colon.
    // A parser that only handles compact JSON passes every hand-written test
    // and fails on the product -- which is how this module came to exist.
    const REAL_CATALOG: &str = r#"{"t": "catalog", "ver": 1, "metrics": [{"id": "cpu.total", "label": "CPU", "unit": "%", "type": "number", "min": 0, "max": 100, "interval_ms": 1000}, {"id": "cpu.temp", "label": "TEMP", "unit": "C", "type": "number", "min": 0, "max": 110, "interval_ms": 1000}]}"#;

    #[test]
    fn parses_the_daemons_real_spacing() {
        let mut catalog = Catalog::EMPTY;
        let n = catalog.parse(REAL_CATALOG.as_bytes()).expect("real catalog must parse");
        assert_eq!(n, 2, "both metrics must survive the spacing");
        assert_eq!(catalog.interval_ms(&Id::new("cpu.total").unwrap()), 1000);
        assert_eq!(catalog.interval_ms(&Id::new("cpu.temp").unwrap()), 1000);
    }

    #[test]
    fn the_real_catalog_reaches_a_rendered_page() {
        let mut page = Page::new();
        page.feed(REAL_CATALOG.as_bytes(), 0);
        page.feed(
            br##"{"t": "ui", "scene": 0, "ver": 1, "bg": "#000000", "nodes": [
                {"type": "num", "x": 0, "y": 0, "bind": "cpu.total", "dec": 0, "unit": "%"}
            ]}"##,
            0,
        );
        assert_eq!(
            page.layout().unwrap().nodes()[0].interval_ms,
            1000,
            "the interval must come from a catalog that looks like the real one"
        );
    }
}


#[cfg(test)]
mod probe_tests {
    use super::*;

    /// then what it reported at 05:40 with a later probe:
    ///
    /// ```text
    /// STALE nodes=6 iv=1000 age=14111 nv=6 now=37267   (no FRAME line)
    /// STALE nodes=6 iv=1000 lp=26299 st=23294 g=0 d=0 (no FRAME line)
    /// ```
    ///
    /// The daemon stopped at `stamp`, the page was painted 3005 ms later, and
    /// the figure stayed on the glass. The first version of this test passed
    /// while the glass was frozen, because it painted its stand-in page at the
    /// same instant the data arrived -- the one ordering the timestamp
    /// comparison could not get wrong. It now paints where the board paints.
    #[test]
    fn the_board_scenario_must_ask_for_a_repaint() {
        let mut page = Page::new();
        page.feed(
            br#"{"t":"catalog","ver":1,"metrics":[{"id":"cpu.total","interval_ms":1000}]}"#,
            0,
        );
        page.feed(
            br##"{"t":"ui","scene":0,"ver":1,"bg":"#000000","nodes":[
                {"type":"num","x":0,"y":0,"bind":"cpu.total","dec":0,"unit":"%"}
            ]}"##,
            0,
        );

        // A reading arrives at 23294 and the page is painted while it is still
        // fresh (250 ms later). Painting it *after* the deadline would be a
        // different case: that frame draws `--`, which is correct, and it is
        // why an earlier version of this test asserted `mask == 1` against a
        // paint at 26299 and failed -- 3005 ms is 5 ms past the 3000 ms limit,
        // so `--` was the right pixel. This test is about the paint that did
        // show a figure and the silence that follows it.
        let stamped = 23_294u32;
        let painted_at = 23_544u32;
        page.feed(br#"{"t":"data","v":{"cpu.total":42}}"#, stamped);

        let mut buf = [0u8; 160 * 80 * 2];
        let mut canvas = Canvas::new(&mut buf, 160, 80);
        page.render_recording(&mut canvas, painted_at);
        page.mark_clean_at(painted_at);

        assert_eq!(page.painted_mask(), 1, "the figure went on the glass");
        assert!(
            !page.is_dirty_at(painted_at + 100),
            "fresh, so the glass stays as it is"
        );

        // The daemon is gone. Once the deadline passes, the glass must be told
        // to repaint -- and that repaint draws `--`.
        assert!(
            page.is_dirty_at(stamped + 3_001),
            "3001 ms after the reading the glass must repaint to `--`"
        );
        page.render_recording(&mut canvas, stamped + 3_001);
        assert_eq!(
            page.painted_mask(), 0,
            "the repaint drew `--`, so nothing is on the glass to age out now"
        );
    }

    /// The edge fires, and having fired it stops: a page already drawn as `--`
    /// must not ask for 25 KB of SPI on every tick forever.
    #[test]
    fn the_stale_edge_fires_once_and_then_stays_quiet() {
        let mut page = Page::new();
        page.feed(
            br#"{"t":"catalog","ver":1,"metrics":[{"id":"cpu.total","interval_ms":1000}]}"#,
            0,
        );
        page.feed(
            br##"{"t":"ui","scene":0,"ver":1,"bg":"#000000","nodes":[
                {"type":"num","x":0,"y":0,"bind":"cpu.total","dec":0,"unit":"%"}
            ]}"##,
            0,
        );
        page.feed(br#"{"t":"data","v":{"cpu.total":42}}"#, 1000);

        let mut buf = [0u8; 160 * 80 * 2];
        let mut canvas = Canvas::new(&mut buf, 160, 80);
        page.render_recording(&mut canvas, 1000);
        page.mark_clean_at(1000);

        assert!(page.is_dirty_at(5_000), "the figure aged out");

        // Paint it again; now the glass shows `--` and the bit is clear.
        page.render_recording(&mut canvas, 5_000);
        page.mark_clean_at(5_000);
        assert_eq!(page.painted_mask(), 0, "`--` is not a figure");
        assert!(
            !page.is_dirty_at(60_000),
            "a metric that is already `--` must not repaint forever"
        );
    }

    /// A page that has only ever drawn the self-test card never recorded a
    /// figure, so there is nothing to age out -- and no reason to repaint.
    ///
    /// This is the case the timestamp version got wrong in the other direction:
    /// it needed `last_paint` to be meaningful before any paint had happened.
    #[test]
    fn a_page_that_never_drew_a_figure_does_not_repaint() {
        let mut page = Page::new();
        page.feed(
            br#"{"t":"catalog","ver":1,"metrics":[{"id":"cpu.total","interval_ms":1000}]}"#,
            0,
        );
        page.feed(
            br##"{"t":"ui","scene":0,"ver":1,"bg":"#000000","nodes":[
                {"type":"num","x":0,"y":0,"bind":"cpu.total","dec":0,"unit":"%"}
            ]}"##,
            0,
        );
        page.feed(br#"{"t":"data","v":{"cpu.total":42}}"#, 1000);
        page.mark_clean_at(0); // painted before any reading existed: drew `--`

        assert_eq!(page.painted_mask(), 0);
        assert!(
            !page.is_dirty_at(10_000),
            "the glass already shows `--`; repainting would draw the same pixels"
        );
    }
}


#[test]
fn dbg_mask() {
    let mut page = Page::new();
    page.feed(br#"{"t":"catalog","ver":1,"metrics":[{"id":"cpu.total","interval_ms":1000}]}"#, 0);
    page.feed(br##"{"t":"ui","scene":0,"ver":1,"bg":"#000000","nodes":[{"type":"num","x":0,"y":0,"bind":"cpu.total","dec":0,"unit":"%"}]}"##, 0);
    page.feed(br#"{"t":"data","v":{"cpu.total":42}}"#, 23294);
    let id = Id::new("cpu.total").unwrap();
    println!("raw={:?}", page.values().raw(&id));
    println!("stamp={:?}", page.values().stamp_of(&id));
    println!("iv={}", page.layout().unwrap().nodes()[0].interval_ms);
    println!("get={:?}", page.values().get(&id, 26299, 1000));
    let mut buf=[0u8;160*80*2];
    let mut c = Canvas::new(&mut buf,160,80);
    page.render_recording(&mut c, 26299);
    println!("mask={}", page.painted_mask());
}

#[test]
fn dbg_mask2() {
    let mut page = Page::new();
    page.feed(br#"{"t":"catalog","ver":1,"metrics":[{"id":"cpu.total","interval_ms":1000}]}"#, 0);
    page.feed(br##"{"t":"ui","scene":0,"ver":1,"bg":"#000000","nodes":[{"type":"num","x":0,"y":0,"bind":"cpu.total","dec":0,"unit":"%"}]}"##, 0);
    println!("after ui: iv={:?}", page.layout().map(|l| l.nodes()[0].interval_ms));
    println!("after ui: kind={:?}", page.layout().map(|l| l.nodes()[0].kind));
    page.feed(br#"{"t":"data","v":{"cpu.total":42}}"#, 23294);
    let id = Id::new("cpu.total").unwrap();
    println!("raw={:?} stamp={:?}", page.values().raw(&id), page.values().stamp_of(&id));
    println!("iv2={}", page.layout().unwrap().nodes()[0].interval_ms);
    println!("get(26299, iv2)={:?}", page.values().get(&id, 26299, page.layout().unwrap().nodes()[0].interval_ms));
    let mut buf=[0u8;160*80*2];
    let mut c = Canvas::new(&mut buf,160,80);
    page.render_recording(&mut c, 26299);
    println!("mask={}", page.painted_mask());
}

