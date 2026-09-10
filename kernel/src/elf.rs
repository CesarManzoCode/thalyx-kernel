//! Bounded ELF64 reader for the initial boot package.
//!
//! This is **not** a general program loader. The architecture puts that in the
//! supervisor; K1 has no supervisor yet, so the kernel reads the initial
//! package itself and the reader is deliberately as small as that job allows:
//! statically linked `ET_EXEC` only, no dynamic section, no relocations, no
//! interpreter, no section headers, a hard bound on segment count, and every
//! offset checked against the image length with non-overflowing arithmetic.
//!
//! Anything it cannot fully validate it refuses. A module that is refused never
//! becomes a domain, which is the same outcome as a module that does not exist.

use thalyx_boot_protocol::{PAGE_SIZE, USER_MAX_ADDR, USER_MIN_ADDR};

/// Largest number of loadable segments accepted from one module.
pub const MAX_SEGMENTS: usize = 8;

/// Largest image accepted from the boot package.
pub const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

/// Largest total memory footprint accepted for one module's segments.
///
/// Raised from 256 in K5. A native C program that carries a real language
/// runtime does not fit in a megabyte, and the ceiling that actually binds an
/// image is the memory object it arrives in: `MAX_MEMORY_PAGES_PER_OBJECT` is
/// an interface limit and caps the file at two megabytes, while this number is
/// an implementation capacity and caps the mapped footprint, `.bss` included.
pub const MAX_IMAGE_PAGES: u64 = 768;

const EI_NIDENT: usize = 16;
const HEADER_LEN: usize = 64;
const PROGRAM_HEADER_LEN: usize = 56;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Why a module was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reject {
    /// Image shorter than an ELF header.
    TooShort,
    /// Image larger than the accepted bound.
    TooLarge,
    /// Not an ELF64 little-endian version 1 image.
    NotElf64,
    /// Not a statically linked executable for x86_64.
    WrongObjectType,
    /// Program header table missing, malformed or too large.
    BadProgramHeaders,
    /// A segment's file range lies outside the image.
    SegmentOutsideImage,
    /// A segment is not page aligned.
    SegmentUnaligned,
    /// `p_filesz` exceeds `p_memsz`.
    SegmentFileLargerThanMemory,
    /// A segment lies outside the address range a user domain may map.
    SegmentOutsideUserRange,
    /// A segment is writable and executable.
    SegmentWriteAndExecute,
    /// A segment is writable or executable without being readable.
    SegmentWriteOrExecuteWithoutRead,
    /// Two segments cover the same page.
    SegmentOverlap,
    /// No loadable segment, or more than [`MAX_SEGMENTS`].
    SegmentCount,
    /// Total footprint exceeds [`MAX_IMAGE_PAGES`].
    ImageTooManyPages,
    /// The entry point is not inside an executable segment.
    EntryNotExecutable,
}

impl Reject {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Reject::TooShort => "too_short",
            Reject::TooLarge => "too_large",
            Reject::NotElf64 => "not_elf64",
            Reject::WrongObjectType => "wrong_object_type",
            Reject::BadProgramHeaders => "bad_program_headers",
            Reject::SegmentOutsideImage => "segment_outside_image",
            Reject::SegmentUnaligned => "segment_unaligned",
            Reject::SegmentFileLargerThanMemory => "segment_filesz_gt_memsz",
            Reject::SegmentOutsideUserRange => "segment_outside_user_range",
            Reject::SegmentWriteAndExecute => "segment_write_and_execute",
            Reject::SegmentWriteOrExecuteWithoutRead => "segment_write_or_execute_without_read",
            Reject::SegmentOverlap => "segment_overlap",
            Reject::SegmentCount => "segment_count",
            Reject::ImageTooManyPages => "image_too_many_pages",
            Reject::EntryNotExecutable => "entry_not_executable",
        }
    }
}

/// A validated loadable segment.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Page-aligned virtual base.
    pub vaddr: u64,
    /// Byte offset of the segment's contents in the image.
    pub file_offset: usize,
    /// Bytes present in the image.
    pub file_len: usize,
    /// Bytes the segment occupies in memory; the excess is zero filled.
    pub mem_len: u64,
    /// Readable.
    pub read: bool,
    /// Writable.
    pub write: bool,
    /// Executable.
    pub execute: bool,
}

impl Segment {
    /// Pages the segment occupies.
    #[must_use]
    pub const fn pages(&self) -> u64 {
        self.mem_len.div_ceil(PAGE_SIZE)
    }
}

/// A validated image.
#[derive(Clone, Copy, Debug)]
pub struct Image {
    /// Entry point virtual address.
    pub entry: u64,
    /// Validated loadable segments.
    pub segments: [Segment; MAX_SEGMENTS],
    /// Number of valid entries in `segments`.
    pub segment_count: usize,
    /// Total pages the segments occupy.
    pub pages: u64,
}

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

/// Validates a user module image.
pub fn parse_user_image(image: &[u8]) -> Result<Image, Reject> {
    if image.len() as u64 > MAX_IMAGE_BYTES {
        return Err(Reject::TooLarge);
    }
    if image.len() < HEADER_LEN {
        return Err(Reject::TooShort);
    }

    let ident = &image[..EI_NIDENT];
    if ident[0..4] != [0x7F, b'E', b'L', b'F'] || ident[4] != 2 || ident[5] != 1 || ident[6] != 1 {
        return Err(Reject::NotElf64);
    }
    if read_u16(image, 16).ok_or(Reject::TooShort)? != ET_EXEC
        || read_u16(image, 18).ok_or(Reject::TooShort)? != EM_X86_64
    {
        return Err(Reject::WrongObjectType);
    }

    let entry = read_u64(image, 24).ok_or(Reject::TooShort)?;
    let phoff = read_u64(image, 32).ok_or(Reject::TooShort)?;
    let phentsize = read_u16(image, 54).ok_or(Reject::TooShort)?;
    let phnum = read_u16(image, 56).ok_or(Reject::TooShort)?;

    if phentsize as usize != PROGRAM_HEADER_LEN || phnum == 0 || phnum as usize > 64 {
        return Err(Reject::BadProgramHeaders);
    }
    let table_len = (phnum as u64)
        .checked_mul(PROGRAM_HEADER_LEN as u64)
        .ok_or(Reject::BadProgramHeaders)?;
    let table_end = phoff
        .checked_add(table_len)
        .ok_or(Reject::BadProgramHeaders)?;
    if table_end > image.len() as u64 {
        return Err(Reject::BadProgramHeaders);
    }

    let mut segments = [Segment {
        vaddr: 0,
        file_offset: 0,
        file_len: 0,
        mem_len: 0,
        read: false,
        write: false,
        execute: false,
    }; MAX_SEGMENTS];
    let mut count = 0usize;
    let mut pages = 0u64;

    for index in 0..phnum as usize {
        let base = phoff as usize + index * PROGRAM_HEADER_LEN;
        let p_type = read_u32(image, base).ok_or(Reject::BadProgramHeaders)?;
        if p_type != PT_LOAD {
            continue;
        }
        if count == MAX_SEGMENTS {
            return Err(Reject::SegmentCount);
        }
        let p_flags = read_u32(image, base + 4).ok_or(Reject::BadProgramHeaders)?;
        let p_offset = read_u64(image, base + 8).ok_or(Reject::BadProgramHeaders)?;
        let p_vaddr = read_u64(image, base + 16).ok_or(Reject::BadProgramHeaders)?;
        let p_filesz = read_u64(image, base + 32).ok_or(Reject::BadProgramHeaders)?;
        let p_memsz = read_u64(image, base + 40).ok_or(Reject::BadProgramHeaders)?;

        if p_memsz == 0 {
            continue;
        }
        if p_filesz > p_memsz {
            return Err(Reject::SegmentFileLargerThanMemory);
        }
        let file_end = p_offset
            .checked_add(p_filesz)
            .ok_or(Reject::SegmentOutsideImage)?;
        if file_end > image.len() as u64 {
            return Err(Reject::SegmentOutsideImage);
        }
        if p_vaddr % PAGE_SIZE != 0 {
            return Err(Reject::SegmentUnaligned);
        }
        let mem_end = p_vaddr
            .checked_add(p_memsz)
            .ok_or(Reject::SegmentOutsideUserRange)?;
        if p_vaddr < USER_MIN_ADDR || mem_end > USER_MAX_ADDR {
            return Err(Reject::SegmentOutsideUserRange);
        }

        let read = p_flags & PF_R != 0;
        let write = p_flags & PF_W != 0;
        let execute = p_flags & PF_X != 0;
        if write && execute {
            return Err(Reject::SegmentWriteAndExecute);
        }
        if !read && (write || execute) {
            return Err(Reject::SegmentWriteOrExecuteWithoutRead);
        }
        if !read {
            return Err(Reject::SegmentWriteOrExecuteWithoutRead);
        }

        let segment = Segment {
            vaddr: p_vaddr,
            file_offset: p_offset as usize,
            file_len: p_filesz as usize,
            mem_len: p_memsz,
            read,
            write,
            execute,
        };

        let start = segment.vaddr;
        let end = segment.vaddr + segment.pages() * PAGE_SIZE;
        for existing in &segments[..count] {
            let other_start = existing.vaddr;
            let other_end = existing.vaddr + existing.pages() * PAGE_SIZE;
            if start < other_end && other_start < end {
                return Err(Reject::SegmentOverlap);
            }
        }

        pages = pages
            .checked_add(segment.pages())
            .ok_or(Reject::ImageTooManyPages)?;
        if pages > MAX_IMAGE_PAGES {
            return Err(Reject::ImageTooManyPages);
        }
        segments[count] = segment;
        count += 1;
    }

    if count == 0 {
        return Err(Reject::SegmentCount);
    }

    let entry_ok = segments[..count]
        .iter()
        .any(|s| s.execute && entry >= s.vaddr && entry < s.vaddr + s.mem_len);
    if !entry_ok {
        return Err(Reject::EntryNotExecutable);
    }

    Ok(Image {
        entry,
        segments,
        segment_count: count,
        pages,
    })
}
