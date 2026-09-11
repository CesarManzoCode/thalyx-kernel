//! Reading an image before launching it.
//!
//! `ProgramLaunch` in `vault/integration/thalyx.md` is a platform interface,
//! and this is the half of it that has to exist before a domain does: a
//! launcher holding a memory object has to find out where the image will be
//! when it is mapped, so that it can map a stack, place thread stacks and add
//! threads that start somewhere real.
//!
//! The kernel validates the image again when it builds the domain. This is not
//! that check and does not stand in for it; it only reads what it needs, and
//! refuses anything it does not recognise rather than assuming a layout.

use crate::native::{IMAGE_MAGIC, ImageHeader};
use thalyx_user_k4fmt::Pod;

/// Why an image could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// Not an ELF64 little-endian executable for this machine.
    NotOurElf,
    /// The program header table is outside what was read, or absurd.
    Truncated,
    /// No loadable segment covers the address the record should be at.
    NoRecord,
    /// The record is there and does not carry [`IMAGE_MAGIC`].
    NotNative,
}

const EI_NIDENT: usize = 16;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;
const PT_LOAD: u32 = 1;

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// One loadable segment, as the launcher needs it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Segment {
    /// Virtual address the segment is mapped at.
    pub vaddr: u64,
    /// Offset of its bytes in the file.
    pub offset: u64,
    /// Bytes present in the file.
    pub file_len: u64,
    /// Bytes it occupies once mapped.
    pub mem_len: u64,
}

/// What a launcher learned about an image.
#[derive(Clone, Copy)]
pub struct Image {
    /// The ELF entry point.
    pub entry: u64,
    /// The record the image published about itself.
    pub header: ImageHeader,
}

/// Largest number of program headers this reader will walk.
pub const MAX_SEGMENTS: usize = 8;

/// Reads the ELF header and program headers out of `head`, then the image
/// record out of whichever segment covers it.
///
/// `head` must be the first bytes of the image; `read_at` is how the caller
/// gets any other part of it, since a launcher usually holds the image as a
/// memory object and reads it in bounded pieces.
pub fn inspect(
    head: &[u8],
    mut read_at: impl FnMut(u64, &mut [u8]) -> bool,
) -> Result<Image, ImageError> {
    if head.len() < 64 || &head[..4] != b"\x7fELF" || head[4] != 2 || head[5] != 1 {
        return Err(ImageError::NotOurElf);
    }
    if u16_at(head, EI_NIDENT).ok_or(ImageError::Truncated)? != ET_EXEC
        || u16_at(head, EI_NIDENT + 2).ok_or(ImageError::Truncated)? != EM_X86_64
    {
        return Err(ImageError::NotOurElf);
    }
    let entry = u64_at(head, 24).ok_or(ImageError::Truncated)?;
    let phoff = u64_at(head, 32).ok_or(ImageError::Truncated)?;
    let phentsize = u16_at(head, 54).ok_or(ImageError::Truncated)? as usize;
    let phnum = u16_at(head, 56).ok_or(ImageError::Truncated)? as usize;
    if phentsize != 56 || phnum == 0 || phnum > MAX_SEGMENTS {
        return Err(ImageError::Truncated);
    }

    let mut table = [0u8; MAX_SEGMENTS * 56];
    let wanted = &mut table[..phnum * 56];
    if !read_at(phoff, wanted) {
        return Err(ImageError::Truncated);
    }

    // The record sits at the start of the first loadable segment, which is
    // where the linker script keeps it. Finding it by address rather than by
    // file offset is what keeps the layout out of this reader's assumptions.
    let mut record_offset = None;
    let mut lowest = u64::MAX;
    for index in 0..phnum {
        let base = index * 56;
        if u32_at(wanted, base).ok_or(ImageError::Truncated)? != PT_LOAD {
            continue;
        }
        let offset = u64_at(wanted, base + 8).ok_or(ImageError::Truncated)?;
        let vaddr = u64_at(wanted, base + 16).ok_or(ImageError::Truncated)?;
        let file_len = u64_at(wanted, base + 32).ok_or(ImageError::Truncated)?;
        if vaddr < lowest && file_len >= size_of::<ImageHeader>() as u64 {
            lowest = vaddr;
            record_offset = Some(offset);
        }
    }
    let record_offset = record_offset.ok_or(ImageError::NoRecord)?;

    let mut record = [0u8; size_of::<ImageHeader>()];
    if !read_at(record_offset, &mut record) {
        return Err(ImageError::NoRecord);
    }
    let header = ImageHeader::read_from(&record, 0).ok_or(ImageError::NoRecord)?;
    if header.magic != IMAGE_MAGIC {
        return Err(ImageError::NotNative);
    }
    if header.start != entry {
        return Err(ImageError::NotNative);
    }
    Ok(Image { entry, header })
}
