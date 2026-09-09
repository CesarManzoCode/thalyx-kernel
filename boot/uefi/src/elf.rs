//! Kernel image validation.
//!
//! The loader accepts one shape of image and refuses everything else: a
//! statically linked `ET_EXEC` ELF64 for x86_64, with page-aligned `PT_LOAD`
//! segments that all lie in the upper canonical half at the address the
//! hand-off contract fixes. There is no relocation processing and no dynamic
//! linking, so an image that would need either is refused rather than loaded
//! incorrectly.

use thalyx_boot_protocol::KERNEL_IMAGE_BASE;

/// Largest kernel image the loader will place.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest number of loadable segments.
pub const MAX_SEGMENTS: usize = 8;

const HEADER_LEN: usize = 64;
const PROGRAM_HEADER_LEN: usize = 56;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Why an image was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reject {
    /// Shorter than an ELF header.
    TooShort,
    /// Larger than the accepted bound.
    TooLarge,
    /// Not an ELF64 little-endian version 1 image.
    NotElf64,
    /// Not a statically linked executable for x86_64.
    WrongObjectType,
    /// Program header table missing or malformed.
    BadProgramHeaders,
    /// A segment's file range lies outside the image.
    SegmentOutsideImage,
    /// A segment is not page aligned.
    SegmentUnaligned,
    /// `p_filesz` exceeds `p_memsz`.
    SegmentFileLargerThanMemory,
    /// A segment lies outside the kernel's virtual range.
    SegmentOutsideKernelRange,
    /// A segment is writable and executable.
    SegmentWriteAndExecute,
    /// Two segments cover the same page.
    SegmentOverlap,
    /// No loadable segment, or more than [`MAX_SEGMENTS`].
    SegmentCount,
    /// The image does not start at the address the hand-off contract fixes.
    WrongImageBase,
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
            Reject::SegmentOutsideKernelRange => "segment_outside_kernel_range",
            Reject::SegmentWriteAndExecute => "segment_write_and_execute",
            Reject::SegmentOverlap => "segment_overlap",
            Reject::SegmentCount => "segment_count",
            Reject::WrongImageBase => "wrong_image_base",
            Reject::EntryNotExecutable => "entry_not_executable",
        }
    }
}

/// A validated loadable segment.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Page-aligned virtual base.
    pub vaddr: u64,
    /// Byte offset in the image.
    pub file_offset: usize,
    /// Bytes present in the image.
    pub file_len: usize,
    /// Bytes occupied in memory.
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
        self.mem_len.div_ceil(4096)
    }
}

/// A validated kernel image.
#[derive(Clone, Copy, Debug)]
pub struct Image {
    /// Entry point virtual address.
    pub entry: u64,
    /// Lowest page-aligned virtual address of any segment.
    pub base: u64,
    /// One past the highest page-aligned virtual address of any segment.
    pub end: u64,
    /// Validated segments.
    pub segments: [Segment; MAX_SEGMENTS],
    /// Number of valid entries in `segments`.
    pub segment_count: usize,
}

impl Image {
    /// Pages the whole image occupies once placed contiguously.
    #[must_use]
    pub const fn pages(&self) -> u64 {
        (self.end - self.base) / 4096
    }
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

/// Validates a kernel image.
pub fn parse(image: &[u8]) -> Result<Image, Reject> {
    if image.len() as u64 > MAX_IMAGE_BYTES {
        return Err(Reject::TooLarge);
    }
    if image.len() < HEADER_LEN {
        return Err(Reject::TooShort);
    }
    if image[0..4] != [0x7F, b'E', b'L', b'F'] || image[4] != 2 || image[5] != 1 || image[6] != 1 {
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
    let table_len = (phnum as u64) * (PROGRAM_HEADER_LEN as u64);
    let table_end = phoff
        .checked_add(table_len)
        .ok_or(Reject::BadProgramHeaders)?;
    if table_end > image.len() as u64 {
        return Err(Reject::BadProgramHeaders);
    }

    let empty = Segment {
        vaddr: 0,
        file_offset: 0,
        file_len: 0,
        mem_len: 0,
        read: false,
        write: false,
        execute: false,
    };
    let mut segments = [empty; MAX_SEGMENTS];
    let mut count = 0usize;
    let mut base = u64::MAX;
    let mut end = 0u64;

    for index in 0..phnum as usize {
        let offset = phoff as usize + index * PROGRAM_HEADER_LEN;
        if read_u32(image, offset).ok_or(Reject::BadProgramHeaders)? != PT_LOAD {
            continue;
        }
        if count == MAX_SEGMENTS {
            return Err(Reject::SegmentCount);
        }
        let p_flags = read_u32(image, offset + 4).ok_or(Reject::BadProgramHeaders)?;
        let p_offset = read_u64(image, offset + 8).ok_or(Reject::BadProgramHeaders)?;
        let p_vaddr = read_u64(image, offset + 16).ok_or(Reject::BadProgramHeaders)?;
        let p_filesz = read_u64(image, offset + 32).ok_or(Reject::BadProgramHeaders)?;
        let p_memsz = read_u64(image, offset + 40).ok_or(Reject::BadProgramHeaders)?;

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
        if p_vaddr % 4096 != 0 {
            return Err(Reject::SegmentUnaligned);
        }
        let mem_end = p_vaddr
            .checked_add(p_memsz)
            .ok_or(Reject::SegmentOutsideKernelRange)?;
        if p_vaddr < KERNEL_IMAGE_BASE || mem_end < p_vaddr {
            return Err(Reject::SegmentOutsideKernelRange);
        }

        let write = p_flags & PF_W != 0;
        let execute = p_flags & PF_X != 0;
        if write && execute {
            return Err(Reject::SegmentWriteAndExecute);
        }

        let segment = Segment {
            vaddr: p_vaddr,
            file_offset: p_offset as usize,
            file_len: p_filesz as usize,
            mem_len: p_memsz,
            read: p_flags & PF_R != 0,
            write,
            execute,
        };
        let start = segment.vaddr;
        let stop = segment.vaddr + segment.pages() * 4096;
        for existing in &segments[..count] {
            let other_start = existing.vaddr;
            let other_stop = existing.vaddr + existing.pages() * 4096;
            if start < other_stop && other_start < stop {
                return Err(Reject::SegmentOverlap);
            }
        }
        base = base.min(start);
        end = end.max(stop);
        segments[count] = segment;
        count += 1;
    }

    if count == 0 {
        return Err(Reject::SegmentCount);
    }
    if base != KERNEL_IMAGE_BASE {
        return Err(Reject::WrongImageBase);
    }
    let executable = segments[..count]
        .iter()
        .any(|s| s.execute && entry >= s.vaddr && entry < s.vaddr + s.mem_len);
    if !executable {
        return Err(Reject::EntryNotExecutable);
    }

    Ok(Image {
        entry,
        base,
        end,
        segments,
        segment_count: count,
    })
}
