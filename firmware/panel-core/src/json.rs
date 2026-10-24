//! A JSON reader sized for the thin protocol: no alloc, no recursion, no tree.
//!
//! What this does NOT do: it is not a JSON implementation for the general case.
//! It has no floats-to-digits, no unicode escapes, no number limits beyond what
//! the callers ask for. What it *does* is let a caller walk a message the way the
//! message is shaped -- "give me the fields of this object", "give me the items
//! of this array" -- and convert each value at the point of use, so nothing has
//! to be stored twice.
//!
//! Why not a tree: this chip has 96 KB of heap and the stack is the scarcest
//! resource there is (BACKLOG B-1). A `Value` enum with borrowed slices would
//! need an arena to hold child nodes; a cursor needs 16 bytes and no arena.
//! The cost is that a caller must re-scan when it wants a field twice, which for
//! messages under 1 KB is free.
//!
//! Shape of the API:
//!
//! ```text
//! let mut fields = json::Fields::of(body)?;      // body = the object's inside
//! let kind = fields.get(b"type").and_then(json::as_str)?;
//! for node in json::Array::of(fields.get(b"nodes")?)? { ... }
//! ```
//!
//! Strings are returned as `&str` only when they are valid UTF-8 and contain no
//! escape the reader would have to rewrite; `as_str` reports `None` otherwise so
//! a caller can treat "I cannot represent this" as a parse failure instead of
//! drawing mojibake.

/// One `key: value` pair of an object, with the value still in its raw bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field<'a> {
    /// The key, unquoted.
    pub key: &'a str,
    /// The value, exactly as it appeared (whitespace around it trimmed).
    pub raw: &'a [u8],
}

/// What went wrong. Coarse on purpose: the board turns these into one diagnostic
/// line and a dropped message, and a finer taxonomy would only be read by me.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Ran out of bytes in the middle of a value.
    Truncated,
    /// A character that cannot appear where it did.
    Syntax,
    /// The container is not the type the caller asked for.
    WrongType,
}

/// Cursor over the bytes of one JSON value.
#[derive(Clone, Copy)]
pub struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// Position just past the end of the cursor's input (tests use it to prove
    /// nothing was left over).
    pub fn at(&self) -> usize {
        self.at
    }

    pub fn skip_ws(&mut self) {
        while let Some(b) = self.bytes.get(self.at) {
            match b {
                b' ' | b'\t' | b'\r' | b'\n' => self.at += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> Result<(), Error> {
        self.skip_ws();
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else if self.peek().is_none() {
            Err(Error::Truncated)
        } else {
            Err(Error::Syntax)
        }
    }

    /// The raw bytes of the next value, advancing past it.
    ///
    /// Nested containers are returned whole (their own bytes), which is what
    /// lets `Array::of` and `Fields::of` take a value slice and re-read it.
    pub fn value_raw(&mut self) -> Result<&'a [u8], Error> {
        self.skip_ws();
        let start = self.at;
        let first = self.peek().ok_or(Error::Truncated)?;
        match first {
            b'{' | b'[' => {
                self.skip_container()?;
                Ok(&self.bytes[start..self.at])
            }
            b'"' => {
                self.skip_string()?;
                Ok(&self.bytes[start..self.at])
            }
            _ => {
                while let Some(b) = self.peek() {
                    if b == b',' || b == b'}' || b == b']' || b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                        break;
                    }
                    self.at += 1;
                }
                if self.at == start {
                    return Err(Error::Syntax);
                }
                Ok(&self.bytes[start..self.at])
            }
        }
    }

    fn skip_string(&mut self) -> Result<(), Error> {
        self.eat(b'"')?;
        while let Some(b) = self.peek() {
            self.at += 1;
            match b {
                b'"' => return Ok(()),
                b'\\' => {
                    // Skip the escaped byte whatever it is: the reader never
                    // interprets escapes, `as_str` refuses strings that have one.
                    if self.peek().is_none() {
                        return Err(Error::Truncated);
                    }
                    self.at += 1;
                }
                _ => {}
            }
        }
        Err(Error::Truncated)
    }

    fn skip_container(&mut self) -> Result<(), Error> {
        let mut depth = 0usize;
        loop {
            let Some(b) = self.peek() else {
                return Err(Error::Truncated);
            };
            self.at += 1;
            match b {
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                b'"' => {
                    self.at -= 1;
                    self.skip_string()?;
                }
                _ => {}
            }
        }
    }
}

/// The fields of one object body (the bytes between `{` and `}`).
///
/// Lookups are **repeatable and order-free**: the type holds the bytes, not a
/// cursor, so `get(b"w")` before `get(b"x")` works and asking twice gives the
/// same answer. The first version of this file kept a cursor and made lookups
/// forward-only; `ui`'s golden-page test caught it immediately, which is why
/// the API looks the way it does.
pub struct Fields<'a> {
    body: &'a [u8],
}

impl<'a> Fields<'a> {
    /// `value` must be a whole object, braces included.
    pub fn of(value: &'a [u8]) -> Result<Self, Error> {
        if value.first() != Some(&b'{') {
            return Err(Error::WrongType);
        }
        Ok(Self { body: value })
    }

    /// Every field, in document order.
    pub fn iter(&self) -> FieldIter<'a> {
        FieldIter { cursor: Cursor::new(self.body), done: false }
    }

    /// The first field named `key`, if any.
    pub fn get(&self, key: &[u8]) -> Option<&'a [u8]> {
        self.find(|k| k == key)
    }

    /// The first field whose key matches `pred`. Useful when the caller wants a
    /// key it cannot name as a byte string (metric ids arrive at runtime).
    pub fn find(&self, pred: impl Fn(&[u8]) -> bool) -> Option<&'a [u8]> {
        self.iter().find(|f| pred(f.key.as_bytes())).map(|f| f.raw)
    }
}

/// One pass over an object's fields.
pub struct FieldIter<'a> {
    cursor: Cursor<'a>,
    done: bool,
}

impl<'a> FieldIter<'a> {
    /// Where the pass stopped, for tests that care whether the object was read
    /// to the end.
    pub fn at(&self) -> usize {
        self.cursor.at()
    }
}

impl<'a> Iterator for FieldIter<'a> {
    type Item = Field<'a>;

    fn next(&mut self) -> Option<Field<'a>> {
        if self.done {
            return None;
        }
        // The first iteration steps over the opening brace; `Fields::of` already
        // proved it is there, and a second `eat` is harmless.
        if self.cursor.at() == 0 {
            if self.cursor.eat(b'{').is_err() {
                self.done = true;
                return None;
            }
        }
        self.cursor.skip_ws();
        match self.cursor.peek() {
            Some(b'}') => {
                self.cursor.at += 1;
                self.done = true;
                return None;
            }
            // An empty body or a trailing comma: either way there is no field.
            None => {
                self.done = true;
                return None;
            }
            _ => {}
        }

        let key_raw = match self.cursor.value_raw() {
            Ok(raw) => raw,
            Err(_) => {
                self.done = true;
                return None;
            }
        };
        let key = as_str(key_raw)?;
        if self.cursor.eat(b':').is_err() {
            self.done = true;
            return None;
        }
        let raw = match self.cursor.value_raw() {
            Ok(raw) => raw,
            Err(_) => {
                self.done = true;
                return None;
            }
        };

        // After a value: a comma continues, anything else ends the object. An
        // error here just stops iteration; the caller already has its values.
        self.cursor.skip_ws();
        match self.cursor.peek() {
            Some(b',') => self.cursor.at += 1,
            _ => self.done = true,
        }
        Some(Field { key, raw })
    }
}

/// The items of one array (the bytes between `[` and `]`).
pub struct Array<'a> {
    cursor: Cursor<'a>,
    done: bool,
}

impl<'a> Array<'a> {
    /// `value` must be a whole array, brackets included.
    pub fn of(value: &'a [u8]) -> Result<Self, Error> {
        if value.first() != Some(&b'[') {
            return Err(Error::WrongType);
        }
        let mut cursor = Cursor::new(value);
        cursor.eat(b'[')?;
        Ok(Self { cursor, done: false })
    }
}

impl<'a> Iterator for Array<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.done {
            return None;
        }
        self.cursor.skip_ws();
        match self.cursor.peek() {
            Some(b']') => {
                self.cursor.at += 1;
                self.done = true;
                return None;
            }
            None => {
                self.done = true;
                return None;
            }
            _ => {}
        }

        let raw = match self.cursor.value_raw() {
            Ok(raw) => raw,
            Err(_) => {
                self.done = true;
                return None;
            }
        };
        self.cursor.skip_ws();
        match self.cursor.peek() {
            Some(b',') => self.cursor.at += 1,
            _ => self.done = true,
        }
        Some(raw)
    }
}

/// A JSON string as `&str`, or `None` if it is escaped or not UTF-8.
///
/// The protocol's strings are ASCII (metric ids, hex colours, short labels), so
/// refusing escapes costs nothing and keeps the drawing path free of a decoder
/// that would need its own output buffer.
pub fn as_str(raw: &[u8]) -> Option<&str> {
    if raw.len() < 2 || raw[0] != b'"' || raw[raw.len() - 1] != b'"' {
        return None;
    }
    let inner = &raw[1..raw.len() - 1];
    if inner.contains(&b'\\') {
        return None;
    }
    core::str::from_utf8(inner).ok()
}

/// An integer, or `None` for anything else. `1e3` and `1.0` are refused on
/// purpose: every integer field in this protocol is written as an integer.
pub fn as_i64(raw: &[u8]) -> Option<i64> {
    let text = core::str::from_utf8(raw).ok()?;
    text.parse::<i64>().ok()
}

/// A number, integer or decimal. Refuses `null` and everything else.
pub fn as_f64(raw: &[u8]) -> Option<f64> {
    let text = core::str::from_utf8(raw).ok()?;
    text.parse::<f64>().ok()
}

/// `true` when the raw value is exactly `null`.
pub fn is_null(raw: &[u8]) -> bool {
    raw == b"null"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(src: &str) -> Fields<'_> {
        Fields::of(src.as_bytes()).expect("object")
    }

    #[test]
    fn reads_flat_scalars() {
        let f = fields(r#"{"a":1,"b":"two","c":true,"d":null}"#);
        assert_eq!(f.get(b"a"), Some(&b"1"[..]));
        assert_eq!(as_i64(f.get(b"a").unwrap()), Some(1));
        assert_eq!(as_str(f.get(b"b").unwrap()), Some("two"));
        assert!(is_null(f.get(b"d").unwrap()));
        assert_eq!(as_f64(f.get(b"c").unwrap()), None);
    }

    #[test]
    fn whitespace_anywhere_is_fine() {
        let f = fields("{ \"a\" :\n 1 ,\r\n \"b\"\t: \"x\" }");
        assert_eq!(as_i64(f.get(b"a").unwrap()), Some(1));
        assert_eq!(as_str(f.get(b"b").unwrap()), Some("x"));
    }

    #[test]
    fn nested_containers_come_back_whole() {
        let f = fields(r#"{"nodes":[{"x":1},{"x":2}],"n":2}"#);
        let items: Vec<_> = Array::of(f.get(b"nodes").unwrap()).unwrap().collect();
        assert_eq!(items.len(), 2);
        let first = Fields::of(items[0]).unwrap();
        assert_eq!(as_i64(first.get(b"x").unwrap()), Some(1));
        assert_eq!(as_i64(f.get(b"n").unwrap()), Some(2));
    }

    #[test]
    fn a_comma_then_brace_does_not_hang_or_panic() {
        // Iterators must always terminate: a truncated message is the normal
        // case on a lossy link, and a hang there would take the render task
        // down with it.
        for src in [r#"{"a":1,}"#, r#"{"a":1"#, r#"{"#, r#"{"a":"#] {
            let f = Fields::of(src.as_bytes()).expect("opens");
            let _ = f.get(b"a");
            // no assertion: the point is that we get here at all
        }
    }

    #[test]
    fn strings_with_escapes_are_refused_not_mangled() {
        assert_eq!(as_str(br#""plain""#), Some("plain"));
        assert_eq!(as_str(br#""tab\there""#), None);
        assert_eq!(as_str(br#""unterminated"#), None);
        assert_eq!(as_str(br#""#), None);
    }

    #[test]
    fn numbers_are_not_loosened() {
        assert_eq!(as_i64(b"42"), Some(42));
        assert_eq!(as_i64(b"-7"), Some(-7));
        assert_eq!(as_i64(b"4.2"), None);
        assert_eq!(as_f64(b"4.2"), Some(4.2));
        assert_eq!(as_f64(b"null"), None);
        assert_eq!(as_f64(b"\"4.2\""), None);
    }

    #[test]
    fn an_array_of_scalars_walks_cleanly() {
        let items: Vec<_> = Array::of(br#"[1,2,3]"#).unwrap().collect();
        assert_eq!(items, vec![&b"1"[..], &b"2"[..], &b"3"[..]]);
        let empty: Vec<_> = Array::of(br#"[]"#).unwrap().collect();
        assert!(empty.is_empty());
    }

    #[test]
    fn a_truncated_container_reports_rather_than_guesses() {
        let mut cursor = Cursor::new(br#"{"a":[1,2"#);
        assert_eq!(cursor.value_raw(), Err(Error::Truncated));
    }

    #[test]
    fn wrong_type_is_reported() {
        assert_eq!(Fields::of(b"[1]").err(), Some(Error::WrongType));
        assert_eq!(Array::of(b"{\"a\":1}").err(), Some(Error::WrongType));
    }
}
