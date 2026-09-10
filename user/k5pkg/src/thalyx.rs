//! The Thalyx surface, ported.
//!
//! `vault/integration/thalyx.md` says what has to survive the port and what
//! does not. What survives is the *meaning*: a version is identified, work
//! happens privately against a named version, a change is real, a validation is
//! a limited claim about named inputs, publication is conditioned, abandoning
//! costs nothing that was published, and evidence outlives the run. What does
//! not survive is the mechanism: there is no filesystem here, no SQLite, no
//! Btrfs subvolume and no cgroup. A workspace is a fork of a version in the
//! managed-state service; a boundary is a scope; a confined tool is a domain.
//!
//! The names are Thalyx's, because a port that renamed everything would be a
//! different system with a familiar shape.
//!
//! What crosses the seam between the two languages is not here: `finish`,
//! `verdict` and the run's metrics are generated from
//! `abi/schema/k5-proto-v1.json` into [`crate::generated`], because the C side
//! encodes them too.

pub use crate::generated::{finish, verdict};

/// Whether a finish means the transaction should treat the program as having
/// done what it was asked.
///
/// `NEEDS_MODEL` is deliberately not one: the program stopped short of what it
/// was asked to do, and a commit there would keep half a change and report it as
/// finished work. It is not a failure either, and the difference is visible in
/// the word rather than in the outcome.
#[must_use]
pub const fn went_through(value: u32) -> bool {
    value == finish::RETURNED
}

/// The verbs this port answers. Thalyx has more; these are the ones the
/// vertical needs, and the list is one place a person can read.
pub mod verb {
    /// What a name is: kind, where it is, how many uses -- small enough to read.
    pub const CONTEXT: u32 = 1;
    /// The bytes of one file of the workspace.
    pub const READ: u32 = 2;
    /// The names in the workspace.
    pub const LIST: u32 = 3;
    /// Replace one run of bytes in one file with another.
    pub const SUBSTITUTE: u32 = 4;
    /// Put a file into the workspace, or replace it whole.
    pub const WRITE: u32 = 5;
    /// What the workspace really shows changed since the boundary opened.
    pub const CHANGED: u32 = 6;
    /// Search the workspace for a run of bytes.
    pub const GREP: u32 = 7;
    /// The published version this work is against.
    pub const STATE: u32 = 8;

    /// Names, in the order of the numbers above.
    pub const NAMES: [&str; 8] = [
        "contexto",
        "leer",
        "listar",
        "sustituir",
        "escribir",
        "cambios",
        "buscar",
        "estado",
    ];

    /// The number for a name, or `None`.
    #[must_use]
    pub fn of(name: &str) -> Option<u32> {
        let mut index = 0;
        while index < NAMES.len() {
            if NAMES[index].as_bytes() == name.as_bytes() {
                return Some(index as u32 + 1);
            }
            index += 1;
        }
        None
    }
}

/// What a validation demands and how it finds out. Thalyx's `Check`, minus the
/// kinds whose tools this system does not carry.
pub mod check {
    /// Every file the change reaches still parses, by the runtime's own parser.
    pub const PARSES: u32 = 1;
    /// A confined program is run over the candidate and must exit zero.
    pub const PROGRAM: u32 = 2;
    /// A run of bytes must be gone from the workspace, or still be there.
    pub const TEXT: u32 = 3;
}

/// The names a workspace binds. Fixed, because the vertical is about one
/// module and a general filesystem is explicitly out of scope for K5.
pub mod name {
    /// The module under work.
    pub const MODULE: &[u8] = b"module.js";
    /// Its assertions, which the tool runs.
    pub const TESTS: &[u8] = b"module.test.js";
    /// The program the work executes in the language runtime.
    pub const PROGRAM: &[u8] = b"program.js";
    /// Prose that travels with the version.
    pub const NOTES: &[u8] = b"notes.md";
    /// What one run produced and did not send back.
    pub const EVIDENCE: &[u8] = b"evidence.json";
}

/// A bounded writer of JSON.
///
/// The language runtime parses what this writes with its own `JSON.parse`, so
/// the two do not have to agree about a format either of them invented. It
/// refuses rather than truncates: an answer cut in half is an answer a program
/// will act on believing it is whole.
pub struct Json<'a> {
    out: &'a mut [u8],
    at: usize,
    overflowed: bool,
    needs_comma: bool,
}

impl<'a> Json<'a> {
    /// A writer over `out`.
    pub fn new(out: &'a mut [u8]) -> Self {
        Self {
            out,
            at: 0,
            overflowed: false,
            needs_comma: false,
        }
    }

    /// Bytes written, or `None` if it did not fit.
    #[must_use]
    pub fn finish(self) -> Option<usize> {
        if self.overflowed { None } else { Some(self.at) }
    }

    fn raw(&mut self, bytes: &[u8]) {
        if self.overflowed || self.at + bytes.len() > self.out.len() {
            self.overflowed = true;
            return;
        }
        self.out[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
    }

    fn comma(&mut self) {
        if self.needs_comma {
            self.raw(b",");
        }
        self.needs_comma = true;
    }

    /// Opens an object.
    pub fn open(&mut self) {
        self.comma();
        self.raw(b"{");
        self.needs_comma = false;
    }

    /// Closes an object.
    pub fn close(&mut self) {
        self.raw(b"}");
        self.needs_comma = true;
    }

    /// Opens an array.
    pub fn open_array(&mut self) {
        self.comma();
        self.raw(b"[");
        self.needs_comma = false;
    }

    /// Closes an array.
    pub fn close_array(&mut self) {
        self.raw(b"]");
        self.needs_comma = true;
    }

    /// Writes a key, after which exactly one value must follow.
    pub fn key(&mut self, key: &str) {
        self.comma();
        self.string_raw(key.as_bytes());
        self.raw(b":");
        self.needs_comma = false;
    }

    fn string_raw(&mut self, bytes: &[u8]) {
        self.raw(b"\"");
        for byte in bytes {
            match byte {
                b'"' => self.raw(b"\\\""),
                b'\\' => self.raw(b"\\\\"),
                b'\n' => self.raw(b"\\n"),
                b'\r' => self.raw(b"\\r"),
                b'\t' => self.raw(b"\\t"),
                0x00..=0x1F => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    self.raw(b"\\u00");
                    self.raw(&[HEX[(*byte >> 4) as usize], HEX[(*byte & 0xF) as usize]]);
                }
                // Bytes above 0x7F are passed through: the content this port
                // carries is UTF-8 and a re-encoder here would be a second
                // opinion about it.
                _ => self.raw(&[*byte]),
            }
        }
        self.raw(b"\"");
    }

    /// Writes a string value.
    pub fn string(&mut self, value: &[u8]) {
        self.comma();
        self.string_raw(value);
        self.needs_comma = true;
    }

    /// Writes an unsigned number.
    pub fn number(&mut self, value: u64) {
        self.comma();
        let mut scratch = [0u8; 20];
        let mut len = 0;
        let mut left = value;
        loop {
            scratch[len] = b'0' + (left % 10) as u8;
            len += 1;
            left /= 10;
            if left == 0 {
                break;
            }
        }
        let mut reversed = [0u8; 20];
        for index in 0..len {
            reversed[index] = scratch[len - 1 - index];
        }
        self.raw(&reversed[..len]);
        self.needs_comma = true;
    }

    /// Writes a signed number.
    pub fn signed(&mut self, value: i64) {
        if value < 0 {
            self.comma();
            self.raw(b"-");
            self.needs_comma = false;
            self.number(value.unsigned_abs());
        } else {
            self.number(value as u64);
        }
    }

    /// Writes a boolean.
    pub fn boolean(&mut self, value: bool) {
        self.comma();
        self.raw(if value { b"true" } else { b"false" });
        self.needs_comma = true;
    }

    /// Writes a hexadecimal digest as a string.
    pub fn digest(&mut self, value: &[u8; 32]) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = [0u8; 64];
        for (index, byte) in value.iter().enumerate() {
            text[index * 2] = HEX[(*byte >> 4) as usize];
            text[index * 2 + 1] = HEX[(*byte & 0xF) as usize];
        }
        self.string(&text);
    }

    /// A key and a string in one call, which is most of what a caller writes.
    pub fn field_string(&mut self, key: &str, value: &[u8]) {
        self.key(key);
        self.string(value);
    }

    /// A key and a number.
    pub fn field_number(&mut self, key: &str, value: u64) {
        self.key(key);
        self.number(value);
    }

    /// A key and a boolean.
    pub fn field_bool(&mut self, key: &str, value: bool) {
        self.key(key);
        self.boolean(value);
    }
}
