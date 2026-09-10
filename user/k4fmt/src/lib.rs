//! The K4 durable store format, as the guest sees it.
//!
//! Three things live here and nothing else does. The generated layout, which
//! comes from `abi/schema/k4-store-v1.json` and is never edited by hand. A
//! SHA-256 written in this repository, so that agreement with the host's
//! digests is agreement between two implementations rather than one library
//! called twice. And the canonical encoders: the code that turns an object, a
//! record or a superblock into the exact bytes that reach the medium.
//!
//! The store service, the block driver, the supervisor and the clients all
//! share this crate, because a format that two programs describe separately is
//! a format they will eventually disagree about while both still compile.
//!
//! Nothing here performs I/O, holds a capability or knows what a block device
//! is. It is a description of bytes.

#![no_std]

pub mod generated;
pub mod pkg;
pub mod sha256;

use core::mem::size_of;

pub use generated::*;
pub use sha256::Sha256;

// Every structure is little-endian by schema, and every encoder below copies a
// `repr(C)` structure's bytes straight out. On a big-endian target that would
// silently write the wrong format, so the target is refused rather than
// mis-served.
const _: () = assert!(cfg!(target_endian = "little"));

/// What went wrong when bytes did not describe what they were supposed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// The destination is not large enough for what was asked.
    TooSmall,
    /// A bound the schema states was exceeded.
    OutOfRange,
    /// A name broke the tree naming rules.
    BadName,
    /// Entries were not in canonical order, or a name repeated.
    NotCanonical,
}

/// Plain data whose `repr(C)` bytes are its encoding.
///
/// # Safety
///
/// Implementers must be `repr(C)`, contain no padding the schema did not
/// declare as an explicit reserved field, and have no invalid bit patterns.
/// Every generated structure satisfies all three, which is why the schema
/// refuses a field that would need implicit padding instead of inserting it.
pub unsafe trait Pod: Copy {
    /// This structure with every byte zero.
    fn zeroed() -> Self {
        // SAFETY: the trait's contract says all-zero is a valid value.
        unsafe { core::mem::zeroed() }
    }

    /// The structure's bytes, in the order they occupy the medium.
    fn as_bytes(&self) -> &[u8] {
        // SAFETY: `Self` is `repr(C)` plain data of `size_of::<Self>()` bytes,
        // and the borrow lives exactly as long as the reference it came from.
        unsafe {
            core::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>())
        }
    }

    /// Reads the structure out of `bytes` at `offset`, or `None` if short.
    fn read_from(bytes: &[u8], offset: usize) -> Option<Self> {
        let end = offset.checked_add(size_of::<Self>())?;
        if end > bytes.len() {
            return None;
        }
        let mut value = Self::zeroed();
        // SAFETY: `value` is `size_of::<Self>()` writable bytes of plain data,
        // the source range was just bounds-checked, and the two do not overlap.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr().add(offset),
                (&raw mut value).cast::<u8>(),
                size_of::<Self>(),
            );
        }
        Some(value)
    }

    /// Writes the structure into `out` at `offset`.
    fn write_to(&self, out: &mut [u8], offset: usize) -> Result<(), FormatError> {
        let end = offset
            .checked_add(size_of::<Self>())
            .ok_or(FormatError::OutOfRange)?;
        if end > out.len() {
            return Err(FormatError::TooSmall);
        }
        out[offset..end].copy_from_slice(self.as_bytes());
        Ok(())
    }
}

macro_rules! pod {
    ($($name:ty),* $(,)?) => {
        $(
            // SAFETY: generated from the schema, which refuses implicit padding
            // and admits only integer and byte-array fields.
            unsafe impl Pod for $name {}
        )*
    };
}

pod!(
    StoreSuperblock,
    RecordHeader,
    ObjectRecordHeader,
    PrepareRecord,
    CommitRecord,
    AbortRecord,
    CheckpointRecord,
    PrincipalEntry,
    ResultEntry,
    RetainedEntry,
    OutboxRecord,
    HarnessDirective,
    TreeHeader,
    TreeEntry,
    Manifest,
    Policy,
    Validation,
    Receipt,
    StoreRequest,
    StoreReply,
);

/// Block size as a `usize`, which is what every buffer index needs.
pub const BLOCK: usize = geometry::BLOCK_SIZE as usize;

/// A digest domain prefix followed by its terminating zero byte.
fn prefixed(hasher: &mut Sha256, domain: &[u8]) {
    hasher.update(domain);
}

/// The canonical content digest of an object.
///
/// The type and the length are inside the preimage, so an empty byte object
/// and an empty tree do not share a digest, and no length-extension of one
/// object's bytes produces another object's identity.
pub fn object_digest(object_type: u32, content: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    prefixed(&mut hasher, prefix::OBJECT);
    hasher.update(&object_type.to_le_bytes());
    hasher.update(&0u32.to_le_bytes());
    hasher.update(&(content.len() as u64).to_le_bytes());
    hasher.update(content);
    hasher.finish()
}

/// The identity of a request: every input that decides what it means.
///
/// Two requests with the same principal and sequence but different inputs have
/// different digests, and that difference is what `CONFLICT` is decided from.
#[allow(clippy::too_many_arguments)]
pub fn request_digest(
    principal: u64,
    sequence: u64,
    op: u32,
    expected_generation: u64,
    candidate_root: &[u8; 32],
    policy_digest: &[u8; 32],
    validation_digest: &[u8; 32],
    outbox_target: u64,
    outbox_payload_digest: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    prefixed(&mut hasher, prefix::REQUEST);
    hasher.update(&principal.to_le_bytes());
    hasher.update(&sequence.to_le_bytes());
    hasher.update(&op.to_le_bytes());
    hasher.update(&0u32.to_le_bytes());
    hasher.update(&expected_generation.to_le_bytes());
    hasher.update(candidate_root);
    hasher.update(policy_digest);
    hasher.update(validation_digest);
    hasher.update(&outbox_target.to_le_bytes());
    hasher.update(outbox_payload_digest);
    hasher.finish()
}

/// The identity of a store, derived from a seed and the format epoch.
pub fn store_uuid(seed: u64, store_epoch: u64) -> [u8; 16] {
    let mut hasher = Sha256::new();
    prefixed(&mut hasher, prefix::STORE_UUID);
    hasher.update(&seed.to_le_bytes());
    hasher.update(&store_epoch.to_le_bytes());
    let full = hasher.finish();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// A record's digest: its header up to the digest field, then its payload.
pub fn record_digest(header: &RecordHeader, payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(&header.as_bytes()[..RecordHeader::OFFSET_DIGEST]);
    hasher.update(payload);
    hasher.finish()
}

/// A superblock's digest: everything in the block before the digest field.
pub fn superblock_digest(block: &StoreSuperblock) -> [u8; 32] {
    sha256::digest(&block.as_bytes()[..StoreSuperblock::OFFSET_DIGEST])
}

/// A harness directive's digest. Scaffolding, and marked as such everywhere.
pub fn harness_digest(directive: &HarnessDirective) -> [u8; 32] {
    sha256::digest(&directive.as_bytes()[..HarnessDirective::OFFSET_DIGEST])
}

/// Whether a name may appear in a tree.
///
/// Non-empty, at most 32 bytes, no NUL, no separator, and neither of the two
/// components a path resolver would treat specially. Nothing is normalised: a
/// name is bytes, and two different byte strings are two different names.
pub fn legal_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > 32 {
        return false;
    }
    if name == b"." || name == b".." {
        return false;
    }
    !name.iter().any(|byte| *byte == 0 || *byte == b'/')
}

/// One binding, as a caller states it before it becomes canonical bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Binding {
    /// The name, unpadded.
    pub name: [u8; 32],
    /// Bytes of `name` that are the name.
    pub name_len: usize,
    /// What kind of object the name binds.
    pub child_type: u32,
    /// Interpretation bits that are part of the version.
    pub flags: u32,
    /// Canonical length of the bound object.
    pub length: u64,
    /// Canonical digest of the bound object.
    pub digest: [u8; 32],
}

impl Binding {
    /// A binding of `name`, refusing a name the format does not admit.
    pub fn new(
        name: &[u8],
        child_type: u32,
        flags: u32,
        length: u64,
        digest: [u8; 32],
    ) -> Result<Self, FormatError> {
        if !legal_name(name) {
            return Err(FormatError::BadName);
        }
        let mut stored = [0u8; 32];
        stored[..name.len()].copy_from_slice(name);
        Ok(Self {
            name: stored,
            name_len: name.len(),
            child_type,
            flags,
            length,
            digest,
        })
    }

    /// The name's bytes.
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }
}

/// Encodes a tree's canonical bytes into `out`, returning how many it wrote.
///
/// Order and uniqueness are checked rather than imposed: a caller that hands
/// over unordered entries has a bug about what a version is, and silently
/// sorting them would hide it while changing the digest of what it published.
pub fn encode_tree(out: &mut [u8], entries: &[Binding]) -> Result<usize, FormatError> {
    if entries.len() > geometry::MAX_TREE_ENTRIES as usize {
        return Err(FormatError::OutOfRange);
    }
    for window in entries.windows(2) {
        if window[0].name() >= window[1].name() {
            return Err(FormatError::NotCanonical);
        }
    }
    for entry in entries {
        if !legal_name(entry.name()) {
            return Err(FormatError::BadName);
        }
    }
    let header = TreeHeader {
        entry_count: entries.len() as u32,
        reserved0: 0,
    };
    let total = TreeHeader::SIZE + entries.len() * TreeEntry::SIZE;
    if total > out.len() {
        return Err(FormatError::TooSmall);
    }
    header.write_to(out, 0)?;
    for (index, entry) in entries.iter().enumerate() {
        let encoded = TreeEntry {
            child_type: entry.child_type,
            flags: entry.flags,
            length: entry.length,
            digest: entry.digest,
            name_len: entry.name_len as u32,
            reserved0: 0,
            name: entry.name,
        };
        encoded.write_to(out, TreeHeader::SIZE + index * TreeEntry::SIZE)?;
    }
    Ok(total)
}

/// Decodes a tree's entries out of canonical bytes, checking the rules again.
///
/// Recovery reads trees written by an earlier run of this service, and an
/// earlier run is still not a reason to believe bytes. The same rules that
/// refuse to encode a malformed tree refuse to accept one.
pub fn decode_tree(bytes: &[u8], out: &mut [Binding]) -> Result<usize, FormatError> {
    let header = TreeHeader::read_from(bytes, 0).ok_or(FormatError::TooSmall)?;
    let count = header.entry_count as usize;
    if header.reserved0 != 0 || count > geometry::MAX_TREE_ENTRIES as usize || count > out.len() {
        return Err(FormatError::OutOfRange);
    }
    if bytes.len() != TreeHeader::SIZE + count * TreeEntry::SIZE {
        return Err(FormatError::OutOfRange);
    }
    for index in 0..count {
        let raw = TreeEntry::read_from(bytes, TreeHeader::SIZE + index * TreeEntry::SIZE)
            .ok_or(FormatError::TooSmall)?;
        let name_len = raw.name_len as usize;
        if name_len > 32 || raw.reserved0 != 0 {
            return Err(FormatError::OutOfRange);
        }
        if raw.name[name_len..].iter().any(|byte| *byte != 0) {
            return Err(FormatError::BadName);
        }
        if !legal_name(&raw.name[..name_len]) {
            return Err(FormatError::BadName);
        }
        out[index] = Binding {
            name: raw.name,
            name_len,
            child_type: raw.child_type,
            flags: raw.flags,
            length: raw.length,
            digest: raw.digest,
        };
        if index > 0 && out[index - 1].name() >= out[index].name() {
            return Err(FormatError::NotCanonical);
        }
    }
    Ok(count)
}

/// A record ready to be framed: everything but the lengths and the digest.
#[derive(Clone, Copy, Debug)]
pub struct RecordFrame {
    /// `RecordKind`.
    pub kind: u32,
    /// Sequence within the arena; increases by exactly one.
    pub sequence: u64,
    /// The format and administrative recovery epoch.
    pub store_epoch: u64,
    /// Which arena this record belongs to.
    pub arena: u32,
    /// Digest of the preceding record, or zero for the first of an arena.
    pub prev_digest: [u8; 32],
}

/// Frames a record into `out`, returning `(blocks_written, record_digest)`.
///
/// A record occupies whole blocks and the trailing bytes of its last block are
/// zeroed, so a torn write damages the record being written and never one that
/// was already durable.
pub fn frame_record(
    out: &mut [u8],
    frame: &RecordFrame,
    payload: &[u8],
) -> Result<(usize, [u8; 32]), FormatError> {
    let total = RecordHeader::SIZE + payload.len();
    let blocks = total.div_ceil(BLOCK);
    if blocks > geometry::RECORD_MAX_BLOCKS as usize {
        return Err(FormatError::OutOfRange);
    }
    if blocks * BLOCK > out.len() {
        return Err(FormatError::TooSmall);
    }
    let mut header = RecordHeader {
        magic: magic::RECORD,
        format_major: FORMAT_MAJOR,
        format_minor: FORMAT_MINOR,
        kind: frame.kind,
        sequence: frame.sequence,
        store_epoch: frame.store_epoch,
        payload_len: payload.len() as u64,
        block_count: blocks as u64,
        arena: frame.arena,
        reserved0: 0,
        reserved1: 0,
        prev_digest: frame.prev_digest,
        digest: [0u8; 32],
    };
    header.digest = record_digest(&header, payload);
    out[..blocks * BLOCK].fill(0);
    header.write_to(out, 0)?;
    out[RecordHeader::SIZE..RecordHeader::SIZE + payload.len()].copy_from_slice(payload);
    Ok((blocks, header.digest))
}

/// A record that verified: its header and the bounds of its payload.
#[derive(Clone, Copy, Debug)]
pub struct ParsedRecord {
    /// The decoded header.
    pub header: RecordHeader,
    /// Where the payload starts inside the span that was parsed.
    pub payload_offset: usize,
    /// How many payload bytes there are.
    pub payload_len: usize,
}

impl ParsedRecord {
    /// The payload bytes inside the span this record was parsed from.
    pub fn payload<'a>(&self, span: &'a [u8]) -> &'a [u8] {
        &span[self.payload_offset..self.payload_offset + self.payload_len]
    }
}

/// Parses one framed record out of `span`, verifying magic, version and digest.
///
/// Returns `None` for anything that is not a valid record, which is what the
/// end of a log looks like and also what a torn tail looks like. Telling those
/// two apart is not this function's job: it is decided against the durability
/// bound the superblock recorded, higher up.
pub fn parse_record(span: &[u8]) -> Option<ParsedRecord> {
    let header = RecordHeader::read_from(span, 0)?;
    if header.magic != magic::RECORD || header.format_major != FORMAT_MAJOR {
        return None;
    }
    let blocks = header.block_count as usize;
    if blocks == 0 || blocks > geometry::RECORD_MAX_BLOCKS as usize {
        return None;
    }
    let extent = blocks.checked_mul(BLOCK)?;
    if extent > span.len() {
        return None;
    }
    let payload_len = header.payload_len as usize;
    if RecordHeader::SIZE.checked_add(payload_len)? > extent {
        return None;
    }
    let payload = &span[RecordHeader::SIZE..RecordHeader::SIZE + payload_len];
    if header.digest != record_digest(&header, payload) {
        return None;
    }
    Some(ParsedRecord {
        header,
        payload_offset: RecordHeader::SIZE,
        payload_len,
    })
}

/// Parses a superblock out of one block, or `None` if the block holds none.
pub fn parse_superblock(block: &[u8]) -> Option<StoreSuperblock> {
    let decoded = StoreSuperblock::read_from(block, 0)?;
    if decoded.magic != magic::SUPERBLOCK || decoded.format_major != FORMAT_MAJOR {
        return None;
    }
    if decoded.digest != superblock_digest(&decoded) {
        return None;
    }
    Some(decoded)
}

/// Parses a harness directive, or `None` if the block holds none. Scaffolding.
pub fn parse_harness(block: &[u8]) -> Option<HarnessDirective> {
    let decoded = HarnessDirective::read_from(block, 0)?;
    if decoded.magic != magic::HARNESS {
        return None;
    }
    if decoded.digest != harness_digest(&decoded) {
        return None;
    }
    Some(decoded)
}

/// What checking the guest's encoders against the golden bytes found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GoldenReport {
    /// How many vectors were rebuilt and compared.
    pub checked: u32,
    /// How many disagreed, in bytes or in digest.
    pub failed: u32,
    /// The ordinal of the first disagreement, or zero.
    pub first_failure: u32,
}

/// Blocks of scratch [`verify_golden`] needs from its caller.
///
/// It works in a caller's buffer rather than on the stack because a user
/// domain's stack is a few pages and a framed record is a whole block; a
/// verifier that overflowed the stack would fail as a fault rather than as a
/// disagreement about the format.
pub const GOLDEN_SCRATCH_BLOCKS: usize = 4;

struct Golden<'a> {
    framing: &'a mut [u8],
    block: &'a mut [u8],
    report: GoldenReport,
}

impl Golden<'_> {
    fn compare(&mut self, produced: &[u8], expected: &[u8], digest: &[u8; 32], want: &[u8; 32]) {
        self.report.checked += 1;
        if produced != expected || digest != want {
            self.report.failed += 1;
            if self.report.first_failure == 0 {
                self.report.first_failure = self.report.checked;
            }
        }
    }

    fn object(&mut self, object_type: u32, content: &[u8], bytes: &[u8], want: &[u8; 32]) {
        let produced = object_digest(object_type, content);
        self.compare(content, bytes, &produced, want);
    }

    fn fail(&mut self) {
        self.report.checked += 1;
        self.report.failed += 1;
        if self.report.first_failure == 0 {
            self.report.first_failure = self.report.checked;
        }
    }

    fn record(
        &mut self,
        frame: &RecordFrame,
        payload: &[u8],
        bytes: &[u8],
        want: &[u8; 32],
    ) -> [u8; 32] {
        match frame_record(self.framing, frame, payload) {
            Ok((blocks, digest)) => {
                self.report.checked += 1;
                if &self.framing[..blocks * BLOCK] != bytes || digest != *want {
                    self.report.failed += 1;
                    if self.report.first_failure == 0 {
                        self.report.first_failure = self.report.checked;
                    }
                }
                digest
            }
            Err(_) => {
                self.fail();
                [0u8; 32]
            }
        }
    }

    fn superblock(&mut self, mut block: StoreSuperblock, bytes: &[u8], want: &[u8; 32]) {
        block.magic = magic::SUPERBLOCK;
        block.format_major = FORMAT_MAJOR;
        block.format_minor = FORMAT_MINOR;
        block.block_size = geometry::BLOCK_SIZE;
        block.digest = superblock_digest(&block);
        self.block.fill(0);
        let _ = block.write_to(self.block, 0);
        let digest = block.digest;
        self.report.checked += 1;
        if self.block != bytes || digest != *want {
            self.report.failed += 1;
            if self.report.first_failure == 0 {
                self.report.first_failure = self.report.checked;
            }
        }
    }
}

/// Rebuilds every golden vector with this crate's encoders and SHA-256.
///
/// The host computed the same bytes from the same schema with `hashlib` and a
/// separate encoder. Nothing is shared between the two but the schema, so a
/// disagreement here is a disagreement about the format rather than one
/// mistake made twice. `scratch` needs one block.
pub fn verify_golden(scratch: &mut [u8]) -> GoldenReport {
    if scratch.len() < GOLDEN_SCRATCH_BLOCKS * BLOCK {
        return GoldenReport {
            checked: 0,
            failed: 1,
            first_failure: 1,
        };
    }
    let (framing, rest) = scratch.split_at_mut(BLOCK);
    let (block, rest) = rest.split_at_mut(BLOCK);
    let (payload, wide) = rest.split_at_mut(BLOCK);
    let mut state = Golden {
        framing,
        block,
        report: GoldenReport::default(),
    };

    state.object(
        object_type::BYTES,
        b"",
        &golden::OBJECT_EMPTY,
        &golden::OBJECT_EMPTY_DIGEST,
    );
    let alpha = b"alpha bytes for the k4 golden vectors\n";
    state.object(
        object_type::BYTES,
        alpha,
        &golden::OBJECT_ALPHA,
        &golden::OBJECT_ALPHA_DIGEST,
    );
    state.object(
        object_type::BYTES,
        b"beta",
        &golden::OBJECT_BETA,
        &golden::OBJECT_BETA_DIGEST,
    );

    const WIDE_PATTERN: &[u8] = b"thalyx-k4 durable ";
    const WIDE_LEN: usize = 1024;
    let wide = &mut wide[..WIDE_LEN];
    for (index, byte) in wide.iter_mut().enumerate() {
        *byte = WIDE_PATTERN[index % WIDE_PATTERN.len()];
    }
    state.object(
        object_type::BYTES,
        wide,
        &golden::OBJECT_WIDE,
        &golden::OBJECT_WIDE_DIGEST,
    );
    let wide_digest = object_digest(object_type::BYTES, wide);

    let empty_digest = object_digest(object_type::BYTES, b"");
    let alpha_digest = object_digest(object_type::BYTES, alpha);
    let beta_digest = object_digest(object_type::BYTES, b"beta");

    let mut leaf = [0u8; 512];
    let leaf_entries = [
        Binding::new(
            b"alpha",
            object_type::BYTES,
            0,
            alpha.len() as u64,
            alpha_digest,
        ),
        Binding::new(b"beta", object_type::BYTES, 0, 4, beta_digest),
        Binding::new(b"empty", object_type::BYTES, 0, 0, empty_digest),
    ];
    let leaf_len = match (leaf_entries[0], leaf_entries[1], leaf_entries[2]) {
        (Ok(a), Ok(b), Ok(c)) => encode_tree(&mut leaf, &[a, b, c]).unwrap_or(0),
        _ => 0,
    };
    let leaf_digest = object_digest(object_type::TREE, &leaf[..leaf_len]);
    state.compare(
        &leaf[..leaf_len],
        &golden::TREE_LEAF,
        &leaf_digest,
        &golden::TREE_LEAF_DIGEST,
    );

    let mut root = [0u8; 512];
    let root_entries = [
        Binding::new(b"data", object_type::TREE, 0, leaf_len as u64, leaf_digest),
        Binding::new(
            b"notes",
            object_type::BYTES,
            1,
            alpha.len() as u64,
            alpha_digest,
        ),
    ];
    let root_len = match (root_entries[0], root_entries[1]) {
        (Ok(a), Ok(b)) => encode_tree(&mut root, &[a, b]).unwrap_or(0),
        _ => 0,
    };
    let root_digest = object_digest(object_type::TREE, &root[..root_len]);
    state.compare(
        &root[..root_len],
        &golden::TREE_ROOT,
        &root_digest,
        &golden::TREE_ROOT_DIGEST,
    );

    let policy = Policy {
        format: 1,
        requires_validation: 1,
        publish_principals: 6,
        validation_tool_id: 5426678735330934785,
        min_coverage_ppm: 750000,
        tool_config_digest: beta_digest,
    };
    let policy_digest = object_digest(object_type::POLICY, policy.as_bytes());
    state.object(
        object_type::POLICY,
        policy.as_bytes(),
        &golden::POLICY_SIGNED,
        &golden::POLICY_SIGNED_DIGEST,
    );

    let validation = Validation {
        format: 1,
        result: 1,
        input_tree_digest: root_digest,
        base_generation: 7,
        tool_id: 5426678735330934785,
        tool_config_digest: beta_digest,
        coverage_ppm: 812345,
        produced_ns: 1234567890,
    };
    let validation_digest = object_digest(object_type::VALIDATION, validation.as_bytes());
    state.object(
        object_type::VALIDATION,
        validation.as_bytes(),
        &golden::VALIDATION_PASS,
        &golden::VALIDATION_PASS_DIGEST,
    );

    let manifest = Manifest {
        format: 1,
        reserved0: 0,
        tree_digest: root_digest,
        policy_digest,
        validation_digest,
        entry_count: 2,
        total_bytes: 42,
    };
    let manifest_digest = object_digest(object_type::MANIFEST, manifest.as_bytes());
    state.object(
        object_type::MANIFEST,
        manifest.as_bytes(),
        &golden::MANIFEST_V8,
        &golden::MANIFEST_V8_DIGEST,
    );

    let request = request_digest(
        2,
        5,
        store_op::PUBLISH,
        7,
        &manifest_digest,
        &policy_digest,
        &validation_digest,
        39185,
        &beta_digest,
    );
    state.report.checked += 1;
    if request != golden::REQUEST_V8_DIGEST {
        state.report.failed += 1;
        if state.report.first_failure == 0 {
            state.report.first_failure = state.report.checked;
        }
    }

    let receipt = Receipt {
        format: 1,
        reserved0: 0,
        principal: 2,
        request_sequence: 5,
        request_digest: request,
        expected_generation: 7,
        new_generation: 8,
        root_digest: manifest_digest,
        policy_digest,
        validation_digest,
        committed_ns: 1234599999,
        outbox_target: 39185,
    };
    let receipt_digest = object_digest(object_type::RECEIPT, receipt.as_bytes());
    state.object(
        object_type::RECEIPT,
        receipt.as_bytes(),
        &golden::RECEIPT_V8,
        &golden::RECEIPT_V8_DIGEST,
    );

    let uuid = store_uuid(6072076158938251265, 1);
    state.report.checked += 1;
    if uuid != golden::STORE_UUID_A {
        state.report.failed += 1;
        if state.report.first_failure == 0 {
            state.report.first_failure = state.report.checked;
        }
    }

    let checkpoint = CheckpointRecord {
        published_generation: 7,
        published_root: root_digest,
        published_policy: policy_digest,
        published_receipt: beta_digest,
        principal_count: 2,
        result_count: 1,
        retained_count: 1,
        object_count: 0,
        created_ns: 1234500000,
        source_arena: 1,
        compaction_generation: 2,
    };
    let mut at = 0;
    let _ = checkpoint.write_to(payload, at);
    at += CheckpointRecord::SIZE;
    let _ = PrincipalEntry {
        principal: 1,
        high_water: 3,
        pending_sequence: 0,
    }
    .write_to(payload, at);
    at += PrincipalEntry::SIZE;
    let _ = PrincipalEntry {
        principal: 2,
        high_water: 4,
        pending_sequence: 0,
    }
    .write_to(payload, at);
    at += PrincipalEntry::SIZE;
    let _ = ResultEntry {
        principal: 2,
        request_sequence: 4,
        outcome: result_outcome::COMMITTED,
        outbox_status: outbox_status::DELIVERED,
        generation: 7,
        root_digest,
        request_digest: beta_digest,
    }
    .write_to(payload, at);
    at += ResultEntry::SIZE;
    let _ = RetainedEntry {
        root_digest,
        expiry_ns: 9000000000,
    }
    .write_to(payload, at);
    at += RetainedEntry::SIZE;
    let checkpoint_digest = state.record(
        &RecordFrame {
            kind: record_kind::CHECKPOINT,
            sequence: 1,
            store_epoch: 1,
            arena: 0,
            prev_digest: [0u8; 32],
        },
        &payload[..at],
        &golden::RECORD_CHECKPOINT,
        &golden::RECORD_CHECKPOINT_DIGEST,
    );

    let object_head = ObjectRecordHeader {
        object_type: object_type::MANIFEST,
        reserved0: 0,
        length: Manifest::SIZE as u64,
        content_digest: manifest_digest,
    };
    let _ = object_head.write_to(payload, 0);
    payload[ObjectRecordHeader::SIZE..ObjectRecordHeader::SIZE + Manifest::SIZE]
        .copy_from_slice(manifest.as_bytes());
    let object_record_digest = state.record(
        &RecordFrame {
            kind: record_kind::OBJECT,
            sequence: 2,
            store_epoch: 1,
            arena: 0,
            prev_digest: checkpoint_digest,
        },
        &payload[..ObjectRecordHeader::SIZE + Manifest::SIZE],
        &golden::RECORD_OBJECT,
        &golden::RECORD_OBJECT_DIGEST,
    );

    let prepare = PrepareRecord {
        principal: 2,
        request_sequence: 5,
        request_digest: request,
        expected_generation: 7,
        candidate_root: manifest_digest,
        policy_digest,
        validation_digest,
        flags: 1,
        outbox_target: 39185,
        outbox_payload_digest: beta_digest,
        admitted_ns: 1234550000,
        invocation_id: 77,
    };
    let prepare_digest = state.record(
        &RecordFrame {
            kind: record_kind::PREPARE,
            sequence: 3,
            store_epoch: 1,
            arena: 0,
            prev_digest: object_record_digest,
        },
        prepare.as_bytes(),
        &golden::RECORD_PREPARE,
        &golden::RECORD_PREPARE_DIGEST,
    );

    let commit = CommitRecord {
        prepare_sequence: 3,
        prepare_digest,
        new_generation: 8,
        root_digest: manifest_digest,
        policy_digest,
        receipt_digest,
        principal: 2,
        request_sequence: 5,
        committed_ns: 1234599999,
        object_count: 6,
    };
    let commit_digest = state.record(
        &RecordFrame {
            kind: record_kind::COMMIT,
            sequence: 4,
            store_epoch: 1,
            arena: 0,
            prev_digest: prepare_digest,
        },
        commit.as_bytes(),
        &golden::RECORD_COMMIT,
        &golden::RECORD_COMMIT_DIGEST,
    );

    let abort = AbortRecord {
        prepare_sequence: 3,
        prepare_digest,
        reason: abort_reason::RECOVERED_UNRESOLVED,
        reserved0: 0,
        principal: 1,
        request_sequence: 4,
        aborted_ns: 1234700000,
    };
    let abort_digest = state.record(
        &RecordFrame {
            kind: record_kind::ABORT,
            sequence: 5,
            store_epoch: 1,
            arena: 0,
            prev_digest: commit_digest,
        },
        abort.as_bytes(),
        &golden::RECORD_ABORT,
        &golden::RECORD_ABORT_DIGEST,
    );

    let outbox = OutboxRecord {
        commit_sequence: 4,
        request_digest: request,
        principal: 2,
        request_sequence: 5,
        status: outbox_status::UNKNOWN,
        attempts: 2,
        target: 39185,
        recorded_ns: 1234800000,
    };
    let outbox_digest = state.record(
        &RecordFrame {
            kind: record_kind::OUTBOX,
            sequence: 6,
            store_epoch: 1,
            arena: 0,
            prev_digest: abort_digest,
        },
        outbox.as_bytes(),
        &golden::RECORD_OUTBOX,
        &golden::RECORD_OUTBOX_DIGEST,
    );

    let wide_head = ObjectRecordHeader {
        object_type: object_type::BYTES,
        reserved0: 0,
        length: WIDE_LEN as u64,
        content_digest: wide_digest,
    };
    let _ = wide_head.write_to(payload, 0);
    payload[ObjectRecordHeader::SIZE..ObjectRecordHeader::SIZE + WIDE_LEN].copy_from_slice(wide);
    state.record(
        &RecordFrame {
            kind: record_kind::OBJECT,
            sequence: 7,
            store_epoch: 1,
            arena: 0,
            prev_digest: outbox_digest,
        },
        &payload[..ObjectRecordHeader::SIZE + WIDE_LEN],
        &golden::RECORD_OBJECT_WIDE,
        &golden::RECORD_OBJECT_WIDE_DIGEST,
    );

    let mut live = StoreSuperblock::zeroed();
    live.store_uuid = uuid;
    live.store_epoch = 1;
    live.superblock_generation = 4;
    live.active_arena = 0;
    live.arena0_start_block = geometry::ARENA0_START_BLOCK;
    live.arena0_block_count = geometry::ARENA_BLOCK_COUNT;
    live.arena1_start_block = geometry::ARENA1_START_BLOCK;
    live.arena1_block_count = geometry::ARENA_BLOCK_COUNT;
    live.checkpoint_sequence = 1;
    live.durable_through_sequence = 6;
    live.published_generation = 8;
    live.published_root = manifest_digest;
    state.superblock(
        live,
        &golden::SUPERBLOCK_LIVE,
        &golden::SUPERBLOCK_LIVE_DIGEST,
    );

    let mut stale = live;
    stale.superblock_generation = 3;
    stale.active_arena = 1;
    stale.durable_through_sequence = 4;
    stale.published_generation = 7;
    stale.published_root = root_digest;
    state.superblock(
        stale,
        &golden::SUPERBLOCK_STALE,
        &golden::SUPERBLOCK_STALE_DIGEST,
    );

    let mut directive = HarnessDirective::zeroed();
    directive.magic = magic::HARNESS;
    directive.version = FORMAT_MAJOR as u32;
    directive.fault_point = fault_point::AFTER_COMMIT;
    directive.fault_mode = fault_mode::STOP;
    directive.leg = 1;
    directive.scenario = 8;
    directive.seed = 20260909;
    directive.digest = harness_digest(&directive);
    state.block.fill(0);
    let _ = directive.write_to(state.block, 0);
    state.report.checked += 1;
    if state.block != golden::HARNESS_STOP_AFTER_COMMIT
        || directive.digest != golden::HARNESS_STOP_AFTER_COMMIT_DIGEST
    {
        state.report.failed += 1;
        if state.report.first_failure == 0 {
            state.report.first_failure = state.report.checked;
        }
    }

    state.report
}
