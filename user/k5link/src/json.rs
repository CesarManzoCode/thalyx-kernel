//! Just enough JSON to read Thalyx's managed protocol and write its replies.
//!
//! The protocol is `thalyx_platform::managed::protocol`: flat objects with
//! string, integer and string-array fields, one nested object (`request`:
//! principal and sequence), and object bytes hex-encoded in strings. This
//! reads exactly that shape from a byte slice without allocating, and writes
//! replies into a caller's buffer. It is not a general JSON library and does
//! not pretend to be one: anything outside that shape is refused, and a
//! refusal is a reply the consumer reads as `unintelligible`.

/// A value found at a key: where its text begins and ends in the input.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum Value<'a> {
    Str(&'a [u8]),
    Number(u64),
    Bool(bool),
    Null,
    Array(&'a [u8]),
    Object(&'a [u8]),
}

fn skip_ws(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\n' | b'\r' | b'\t') {
        at += 1;
    }
    at
}

/// The end of the string whose opening quote is at `at`, or nothing.
fn string_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    let mut index = at + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// The end of the value beginning at `at`.
fn value_end(bytes: &[u8], at: usize) -> Option<usize> {
    match *bytes.get(at)? {
        b'"' => string_end(bytes, at),
        b'{' | b'[' => {
            let mut depth = 0i32;
            let mut index = at;
            while index < bytes.len() {
                match bytes[index] {
                    b'"' => index = string_end(bytes, index)?,
                    b'{' | b'[' => {
                        depth += 1;
                        index += 1;
                    }
                    b'}' | b']' => {
                        depth -= 1;
                        index += 1;
                        if depth == 0 {
                            return Some(index);
                        }
                    }
                    _ => index += 1,
                }
            }
            None
        }
        _ => {
            let mut index = at;
            while index < bytes.len() && !matches!(bytes[index], b',' | b'}' | b']') {
                index += 1;
            }
            Some(index)
        }
    }
}

fn classify(raw: &[u8]) -> Option<Value<'_>> {
    let raw = &raw[skip_ws(raw, 0)..];
    let mut end = raw.len();
    while end > 0 && matches!(raw[end - 1], b' ' | b'\n' | b'\r' | b'\t') {
        end -= 1;
    }
    let raw = &raw[..end];
    match raw.first()? {
        b'"' => Some(Value::Str(&raw[1..raw.len() - 1])),
        b'{' => Some(Value::Object(raw)),
        b'[' => Some(Value::Array(raw)),
        b't' if raw == b"true" => Some(Value::Bool(true)),
        b'f' if raw == b"false" => Some(Value::Bool(false)),
        b'n' if raw == b"null" => Some(Value::Null),
        b'0'..=b'9' => {
            let mut number = 0u64;
            for byte in raw {
                if !byte.is_ascii_digit() {
                    return None;
                }
                number = number
                    .checked_mul(10)?
                    .checked_add(u64::from(byte - b'0'))?;
            }
            Some(Value::Number(number))
        }
        _ => None,
    }
}

/// The value at `key` in the object `bytes`, when the object has it.
pub fn field<'a>(bytes: &'a [u8], key: &str) -> Option<Value<'a>> {
    let mut at = skip_ws(bytes, 0);
    if bytes.get(at) != Some(&b'{') {
        return None;
    }
    at += 1;
    loop {
        at = skip_ws(bytes, at);
        match bytes.get(at)? {
            b'}' => return None,
            b',' => {
                at += 1;
                continue;
            }
            b'"' => {}
            _ => return None,
        }
        let key_end = string_end(bytes, at)?;
        let name = &bytes[at + 1..key_end - 1];
        at = skip_ws(bytes, key_end);
        if bytes.get(at) != Some(&b':') {
            return None;
        }
        at = skip_ws(bytes, at + 1);
        let end = value_end(bytes, at)?;
        if name == key.as_bytes() {
            return classify(&bytes[at..end]);
        }
        at = end;
    }
}

/// The string at `key`, raw (escapes still in place).
pub fn string<'a>(bytes: &'a [u8], key: &str) -> Option<&'a [u8]> {
    match field(bytes, key)? {
        Value::Str(raw) => Some(raw),
        _ => None,
    }
}

pub fn number(bytes: &[u8], key: &str) -> Option<u64> {
    match field(bytes, key)? {
        Value::Number(value) => Some(value),
        _ => None,
    }
}

pub fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodes hex into `out`; answers the length.
pub fn hex_decode(raw: &[u8], out: &mut [u8]) -> Option<usize> {
    if raw.len() % 2 != 0 || raw.len() / 2 > out.len() {
        return None;
    }
    for (index, pair) in raw.chunks(2).enumerate() {
        out[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(raw.len() / 2)
}

/// Iterates the string elements of an array value.
pub struct Strings<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Strings<'a> {
    pub fn of(array: &'a [u8]) -> Self {
        Strings {
            bytes: array,
            at: 1,
        }
    }
}

impl<'a> Iterator for Strings<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        loop {
            self.at = skip_ws(self.bytes, self.at);
            match self.bytes.get(self.at)? {
                b']' => return None,
                b',' => {
                    self.at += 1;
                    continue;
                }
                b'"' => {
                    let end = string_end(self.bytes, self.at)?;
                    let raw = &self.bytes[self.at + 1..end - 1];
                    self.at = end;
                    return Some(raw);
                }
                _ => return None,
            }
        }
    }
}

/// A reply under construction.
pub struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
    overflow: bool,
}

impl<'a> Writer<'a> {
    pub fn new(out: &'a mut [u8]) -> Self {
        Writer {
            out,
            len: 0,
            overflow: false,
        }
    }

    pub fn raw(&mut self, text: &[u8]) -> &mut Self {
        if self.len + text.len() > self.out.len() {
            self.overflow = true;
            return self;
        }
        self.out[self.len..self.len + text.len()].copy_from_slice(text);
        self.len += text.len();
        self
    }

    /// A string value, escaped.
    pub fn str(&mut self, text: &[u8]) -> &mut Self {
        self.raw(b"\"");
        for byte in text {
            match byte {
                b'"' => {
                    self.raw(b"\\\"");
                }
                b'\\' => {
                    self.raw(b"\\\\");
                }
                b'\n' => {
                    self.raw(b"\\n");
                }
                b'\r' => {
                    self.raw(b"\\r");
                }
                b'\t' => {
                    self.raw(b"\\t");
                }
                0..=0x1F => {
                    let mut escaped = *b"\\u0000";
                    escaped[4] = b"0123456789abcdef"[(byte >> 4) as usize];
                    escaped[5] = b"0123456789abcdef"[(byte & 15) as usize];
                    self.raw(&escaped);
                }
                _ => {
                    self.raw(core::slice::from_ref(byte));
                }
            }
        }
        self.raw(b"\"")
    }

    pub fn number(&mut self, value: u64) -> &mut Self {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        let mut rest = value;
        loop {
            at -= 1;
            digits[at] = b'0' + (rest % 10) as u8;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        self.raw(&digits[at..])
    }

    /// Bytes as lowercase hex, inside quotes.
    pub fn hex(&mut self, bytes: &[u8]) -> &mut Self {
        self.raw(b"\"");
        for byte in bytes {
            self.raw(&[
                b"0123456789abcdef"[(byte >> 4) as usize],
                b"0123456789abcdef"[(byte & 15) as usize],
            ]);
        }
        self.raw(b"\"")
    }

    /// Thirty-two bytes as sixty-four hex characters, inside quotes.
    pub fn digest(&mut self, digest: &[u8; 32]) -> &mut Self {
        self.hex(digest)
    }

    pub fn finish(self) -> Option<usize> {
        self.done()
    }

    /// The length written so far, unless it overflowed.
    pub fn done(&self) -> Option<usize> {
        if self.overflow { None } else { Some(self.len) }
    }

    /// Starts over: what was written is discarded.
    pub fn reset(&mut self) {
        self.len = 0;
        self.overflow = false;
    }
}

/// Sixty-four hex characters into thirty-two bytes.
pub fn digest_of(raw: &[u8]) -> Option<[u8; 32]> {
    if raw.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    hex_decode(raw, &mut out)?;
    Some(out)
}
