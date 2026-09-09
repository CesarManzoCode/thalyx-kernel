//! Boot package validation.
//!
//! The package is a flat directory of named byte ranges. Every field is read as
//! a little-endian integer from a bounds-checked offset and every range is
//! checked against the file length with non-overflowing arithmetic, because the
//! package is the first untrusted input the system sees and the loader has no
//! allocator of its own to fall back on if it gets this wrong.

use thalyx_boot_protocol::{MAX_MODULES, module_kind, package};

/// Why a package was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reject {
    /// Shorter than a package header.
    TooShort,
    /// Magic value does not match.
    BadMagic,
    /// Major version not recognised.
    IncompatibleVersion,
    /// Header or entry size does not match this version.
    BadLayout,
    /// `total_len` disagrees with the file length.
    LengthMismatch,
    /// More entries than the hand-off can carry.
    TooManyEntries,
    /// The directory does not fit inside the file.
    DirectoryOutsideFile,
    /// An entry's payload does not fit inside the file.
    EntryOutsideFile,
    /// An entry's payload overlaps the directory or another entry.
    EntryOverlap,
    /// An entry is empty.
    EmptyEntry,
    /// An entry's name is not NUL-terminated printable ASCII.
    BadName,
    /// An entry declares a module kind this loader does not carry.
    UnknownKind,
}

impl Reject {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Reject::TooShort => "too_short",
            Reject::BadMagic => "bad_magic",
            Reject::IncompatibleVersion => "incompatible_version",
            Reject::BadLayout => "bad_layout",
            Reject::LengthMismatch => "length_mismatch",
            Reject::TooManyEntries => "too_many_entries",
            Reject::DirectoryOutsideFile => "directory_outside_file",
            Reject::EntryOutsideFile => "entry_outside_file",
            Reject::EntryOverlap => "entry_overlap",
            Reject::EmptyEntry => "empty_entry",
            Reject::BadName => "bad_name",
            Reject::UnknownKind => "unknown_kind",
        }
    }
}

/// A validated directory entry.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// NUL-padded name.
    pub name: [u8; 32],
    /// Payload offset from the start of the package.
    pub offset: usize,
    /// Payload length.
    pub length: usize,
    /// Module kind.
    pub kind: u32,
    /// Module flags.
    pub flags: u32,
}

/// A validated package directory.
#[derive(Clone, Copy, Debug)]
pub struct Directory {
    /// Validated entries.
    pub entries: [Entry; MAX_MODULES],
    /// Number of valid entries.
    pub count: usize,
}

const EMPTY: Entry = Entry {
    name: [0; 32],
    offset: 0,
    length: 0,
    kind: 0,
    flags: 0,
};

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

/// Validates a package file.
pub fn parse(bytes: &[u8]) -> Result<Directory, Reject> {
    let header_len = core::mem::size_of::<package::Header>();
    let entry_len = core::mem::size_of::<package::Entry>();
    if bytes.len() < header_len {
        return Err(Reject::TooShort);
    }
    if read_u64(bytes, 0).ok_or(Reject::TooShort)? != package::MAGIC {
        return Err(Reject::BadMagic);
    }
    if read_u16(bytes, 8).ok_or(Reject::TooShort)? != package::VERSION_MAJOR {
        return Err(Reject::IncompatibleVersion);
    }
    if read_u32(bytes, 12).ok_or(Reject::TooShort)? as usize != header_len
        || read_u32(bytes, 20).ok_or(Reject::TooShort)? as usize != entry_len
    {
        return Err(Reject::BadLayout);
    }
    if read_u64(bytes, 24).ok_or(Reject::TooShort)? != bytes.len() as u64 {
        return Err(Reject::LengthMismatch);
    }
    let count = read_u32(bytes, 16).ok_or(Reject::TooShort)? as usize;
    if count > MAX_MODULES {
        return Err(Reject::TooManyEntries);
    }
    let directory_end = header_len
        .checked_add(
            count
                .checked_mul(entry_len)
                .ok_or(Reject::DirectoryOutsideFile)?,
        )
        .ok_or(Reject::DirectoryOutsideFile)?;
    if directory_end > bytes.len() {
        return Err(Reject::DirectoryOutsideFile);
    }

    let mut entries = [EMPTY; MAX_MODULES];
    for index in 0..count {
        let base = header_len + index * entry_len;
        let mut name = [0u8; 32];
        name.copy_from_slice(&bytes[base..base + 32]);
        let terminator = name
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Reject::BadName)?;
        if terminator == 0
            || name[..terminator]
                .iter()
                .any(|byte| !byte.is_ascii_graphic())
            || name[terminator..].iter().any(|byte| *byte != 0)
        {
            return Err(Reject::BadName);
        }

        let offset = read_u64(bytes, base + 32).ok_or(Reject::DirectoryOutsideFile)?;
        let length = read_u64(bytes, base + 40).ok_or(Reject::DirectoryOutsideFile)?;
        let kind = read_u32(bytes, base + 48).ok_or(Reject::DirectoryOutsideFile)?;
        let flags = read_u32(bytes, base + 52).ok_or(Reject::DirectoryOutsideFile)?;

        if length == 0 {
            return Err(Reject::EmptyEntry);
        }
        let end = offset.checked_add(length).ok_or(Reject::EntryOutsideFile)?;
        if end > bytes.len() as u64 || offset < directory_end as u64 {
            return Err(Reject::EntryOutsideFile);
        }
        if kind != module_kind::USER_ELF && kind != module_kind::SUPERVISOR {
            return Err(Reject::UnknownKind);
        }
        for existing in &entries[..index] {
            let other_start = existing.offset as u64;
            let other_end = other_start + existing.length as u64;
            if offset < other_end && other_start < end {
                return Err(Reject::EntryOverlap);
            }
        }

        entries[index] = Entry {
            name,
            offset: offset as usize,
            length: length as usize,
            kind,
            flags,
        };
    }

    Ok(Directory { entries, count })
}
