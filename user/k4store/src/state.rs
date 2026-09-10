//! The durable store: what is on the medium, and how it gets there.
//!
//! Three rules shape everything below.
//!
//! **Nothing is believed because an earlier run wrote it.** Every record is
//! verified against its own digest, every object's content digest is recomputed
//! from the bytes rather than read out of a header, and a chain that does not
//! link is the end of the log even when what follows it decodes.
//!
//! **A valid prefix is not the same as a truncated tail.** The superblock
//! records a lower bound on what was durable when it was written. A prefix
//! shorter than that bound is an integrity failure and the service refuses to
//! serve; it is not licence to skip forward to a later record that happens to
//! verify.
//!
//! **A prepare that is durable may not be forgotten.** Recovery resolves every
//! prepare it finds unresolved, durably, before it admits another publication.
//! A commit whose objects are not all present is not adopted and its prepare is
//! aborted with the reason that says why.

use thalyx_user_k4fmt as k4;
use thalyx_user_k4fmt::Pod;
use thalyx_user_k4fmt::pkg::note;
use thalyx_user_k4fmt::{
    AbortRecord, Binding, CheckpointRecord, CommitRecord, Manifest, ObjectRecordHeader,
    OutboxRecord, Policy, PrepareRecord, PrincipalEntry, RecordFrame, ResultEntry, RetainedEntry,
    StoreSuperblock, Validation, abort_reason, fault_mode, fault_point, geometry, magic,
    object_type, outbox_status, record_kind, result_outcome, store_status,
};
use thalyx_user_rt::k2;

use crate::disk::{Disk, slot_bytes};

/// Objects the in-memory index can name at once.
pub const MAX_OBJECTS: usize = 40;
/// Objects that may be staged but not yet durable.
pub const MAX_STAGED: usize = 12;
/// Largest object this service accepts.
pub const OBJECT_MAX: usize = geometry::OBJECT_MAX_BYTES as usize;
/// Principals the service keeps durable state for.
pub const MAX_PRINCIPALS: usize = geometry::MAX_PRINCIPALS as usize;
/// Recent results the service keeps.
pub const MAX_RESULTS: usize = geometry::MAX_RESULTS as usize;
/// Roots held against reuse.
pub const MAX_RETAINED: usize = geometry::MAX_RETAINED as usize;
/// Bindings a workspace may hold.
pub const MAX_BINDINGS: usize = geometry::MAX_TREE_ENTRIES as usize;

/// Buffer slot the service reads into.
const READ_SLOT: u32 = 0;
/// Buffer slot the service frames records in.
const WRITE_SLOT: u32 = 1;

/// Staged object content. In `.bss`, because a service with no allocator still
/// has to hold what a client handed it until the publication that uses it.
static mut STAGED: [[u8; OBJECT_MAX]; MAX_STAGED] = [[0u8; OBJECT_MAX]; MAX_STAGED];
/// One record payload under construction.
static mut PAYLOAD: [u8; OBJECT_MAX + 256] = [0u8; OBJECT_MAX + 256];
/// One record payload being replayed.
static mut REPLAYED: [u8; OBJECT_MAX + 256] = [0u8; OBJECT_MAX + 256];
/// One object's content, read back from the medium.
static mut CONTENT: [u8; OBJECT_MAX] = [0u8; OBJECT_MAX];

// These four buffers are in `.bss` rather than on the stack because a user
// domain's stack is eight pages and three kilobytes of it per nested call is
// how a service faults instead of answering. Each accessor below hands out one
// `&'static mut`; this domain is single-threaded and no two of them are alive
// over the same region at once, which is what makes that sound.

/// The staged content of slot `index`.
fn staged(index: usize) -> &'static mut [u8; OBJECT_MAX] {
    // SAFETY: this domain is single-threaded and every caller drops the borrow
    // before the next one takes it; the index is bounded by the caller.
    unsafe { &mut (*(&raw mut STAGED))[index] }
}

/// The record payload under construction.
fn payload() -> &'static mut [u8; OBJECT_MAX + 256] {
    // SAFETY: as `staged`.
    unsafe { &mut *(&raw mut PAYLOAD) }
}

/// The record payload being replayed.
fn replayed() -> &'static mut [u8; OBJECT_MAX + 256] {
    // SAFETY: as `staged`.
    unsafe { &mut *(&raw mut REPLAYED) }
}

/// The object content buffer.
fn content() -> &'static mut [u8; OBJECT_MAX] {
    // SAFETY: as `staged`.
    unsafe { &mut *(&raw mut CONTENT) }
}

/// One object the service knows about.
#[derive(Clone, Copy)]
pub struct ObjectEntry {
    pub digest: [u8; 32],
    pub object_type: u32,
    pub length: u32,
    /// Absolute device block of the record that holds it, when it is durable.
    pub block: u64,
    pub durable: bool,
    /// Staging slot, or `u32::MAX`.
    pub stage: u32,
    pub live: bool,
}

impl Default for ObjectEntry {
    fn default() -> Self {
        Self {
            digest: [0u8; 32],
            object_type: 0,
            length: 0,
            block: 0,
            durable: false,
            stage: u32::MAX,
            live: false,
        }
    }
}

/// A publication that was prepared and has not been resolved.
#[derive(Clone, Copy, Default)]
pub struct Pending {
    pub live: bool,
    pub principal: u64,
    pub request_sequence: u64,
    pub prepare_sequence: u64,
    pub prepare_digest: [u8; 32],
    pub request_digest: [u8; 32],
    pub candidate_root: [u8; 32],
    pub reason: u32,
}

/// What the harness told this run to do, and what it has done.
#[derive(Clone, Copy, Default)]
pub struct Harness {
    pub present: bool,
    pub point: u32,
    pub mode: u32,
    pub arg: u64,
    pub leg: u64,
    pub scenario: u64,
    pub seed: u64,
    pub stop_at_next_flush: u64,
    /// The mode has been applied and must not be applied twice.
    pub applied: bool,
    /// The next flush ends the run instead of flushing.
    pub armed: bool,
}

/// What the service decided to do at a fault point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Carry on.
    None,
    /// Apply this mode to the write about to be issued.
    Write(u32),
    /// End the run: the machine stops here.
    Stop,
    /// Stop this service domain; the machine keeps running.
    Kill,
    /// Complete durably, then end the run before answering.
    LoseResponse,
}

/// The whole service state.
pub struct Store {
    pub disk: Disk,
    pub harness: Harness,
    /// A fault that asked for the run to end, or for this domain to be
    /// replaced. The engine cannot do either: only the loop that holds the
    /// signal can, so it is recorded here and read there. Without this a
    /// `STOP` inside a publication is indistinguishable from a medium that
    /// refused, and the run would carry on writing after the point it was
    /// supposed to be cut at.
    pub demanded: Option<Fault>,

    pub uuid: [u8; 16],
    pub store_epoch: u64,
    pub superblock_generation: u64,
    /// Which of the two superblock blocks holds the newest one.
    pub superblock_slot: u64,
    pub arena: u64,
    pub arena_start: u64,
    pub arena_blocks: u64,
    pub next_block: u64,
    pub next_sequence: u64,
    pub prev_digest: [u8; 32],
    pub checkpoint_sequence: u64,
    pub durable_through: u64,
    pub compaction_generation: u64,

    pub published_generation: u64,
    pub published_root: [u8; 32],
    pub published_policy: [u8; 32],
    pub published_receipt: [u8; 32],

    pub objects: [ObjectEntry; MAX_OBJECTS],
    pub stage_used: [bool; MAX_STAGED],
    pub stage_len: [u32; MAX_STAGED],
    pub principals: [PrincipalEntry; MAX_PRINCIPALS],
    pub results: [ResultEntry; MAX_RESULTS],
    pub result_count: usize,
    pub result_next: usize,
    pub results_evicted: u64,
    pub retained: [RetainedEntry; MAX_RETAINED],
    pub retained_count: usize,
    pub pending: Pending,

    pub ready: bool,
    pub integrity_failed: bool,
    pub scanned: u64,
    pub recovery_aborts: u64,
    pub dependency_missing: u64,
    pub compactions: u64,
    pub publications: u64,
    pub refusals: u64,
}

/// Absolute device block of a store block index.
fn absolute(index: u64) -> u64 {
    geometry::STORE_BASE_BLOCK + index
}

impl Store {
    pub fn new(disk: Disk) -> Self {
        Self {
            disk,
            harness: Harness::default(),
            demanded: None,
            uuid: [0u8; 16],
            store_epoch: 0,
            superblock_generation: 0,
            superblock_slot: geometry::SUPERBLOCK_A_BLOCK,
            arena: 0,
            arena_start: absolute(geometry::ARENA0_START_BLOCK),
            arena_blocks: geometry::ARENA_BLOCK_COUNT,
            next_block: absolute(geometry::ARENA0_START_BLOCK),
            next_sequence: 1,
            prev_digest: [0u8; 32],
            checkpoint_sequence: 0,
            durable_through: 0,
            compaction_generation: 0,
            published_generation: 0,
            published_root: [0u8; 32],
            published_policy: [0u8; 32],
            published_receipt: [0u8; 32],
            objects: [ObjectEntry::default(); MAX_OBJECTS],
            stage_used: [false; MAX_STAGED],
            stage_len: [0u32; MAX_STAGED],
            principals: [PrincipalEntry::zeroed(); MAX_PRINCIPALS],
            results: [ResultEntry::zeroed(); MAX_RESULTS],
            result_count: 0,
            result_next: 0,
            results_evicted: 0,
            retained: [RetainedEntry::zeroed(); MAX_RETAINED],
            retained_count: 0,
            pending: Pending::default(),
            ready: false,
            integrity_failed: false,
            scanned: 0,
            recovery_aborts: 0,
            dependency_missing: 0,
            compactions: 0,
            publications: 0,
            refusals: 0,
        }
    }

    // -- the harness ---------------------------------------------------------

    /// Reads the fault directive out of a block that is not part of the store.
    ///
    /// Scaffolding, and the only place it is read. Recovery never consults it
    /// and no format rule depends on it: a run started with no directive is the
    /// same run with nothing done to it.
    pub fn read_directive(&mut self) {
        if !self.disk.read(geometry::HARNESS_DIRECTIVE_BLOCK, READ_SLOT) {
            return;
        }
        let Some(directive) = k4::parse_harness(slot_bytes(READ_SLOT)) else {
            return;
        };
        self.harness = Harness {
            present: true,
            point: directive.fault_point,
            mode: directive.fault_mode,
            arg: directive.fault_arg,
            leg: u64::from(directive.leg),
            scenario: directive.scenario,
            seed: directive.seed,
            stop_at_next_flush: directive.stop_at_next_flush,
            applied: false,
            armed: false,
        };
        k2::note(
            note::FAULT_DIRECTIVE,
            u64::from(directive.fault_point)
                | (u64::from(directive.fault_mode) << 8)
                | (u64::from(directive.leg) << 16)
                | (directive.scenario << 24),
        );
    }

    /// Decides what the harness wants done at `point`.
    pub fn fault_at(&mut self, point: u32, ordinal: u64) -> Fault {
        if !self.harness.present || self.harness.applied || self.harness.point != point {
            return Fault::None;
        }
        if point == fault_point::BEFORE_OBJECT && self.harness.arg != ordinal {
            return Fault::None;
        }
        self.harness.applied = true;
        if self.harness.stop_at_next_flush != 0 {
            self.harness.armed = true;
        }
        k2::note(
            note::FAULT_APPLIED,
            u64::from(point) | (u64::from(self.harness.mode) << 8),
        );
        let decided = match self.harness.mode {
            fault_mode::STOP => Fault::Stop,
            fault_mode::KILL_SERVICE => Fault::Kill,
            fault_mode::LOSE_RESPONSE => Fault::LoseResponse,
            fault_mode::DROP_WRITE
            | fault_mode::TEAR_WRITE
            | fault_mode::IO_ERROR
            | fault_mode::REORDER => Fault::Write(self.harness.mode),
            _ => Fault::None,
        };
        if let Fault::Stop | Fault::Kill | Fault::LoseResponse = decided {
            self.demanded = Some(decided);
        }
        decided
    }

    // -- the medium ----------------------------------------------------------

    /// Flushes, unless the harness armed the run to end where a flush would be.
    ///
    /// Returns false when the run is over, which every caller treats as "stop
    /// doing things to the medium" rather than as a failure of the medium.
    pub fn flush(&mut self) -> bool {
        if self.harness.armed {
            self.harness.armed = false;
            self.disk.latch();
            return false;
        }
        self.disk.flush()
    }

    /// Frames a record, writes it and advances the log. Returns its digest.
    fn append(&mut self, kind: u32, body: &[u8], mode: u32) -> Option<[u8; 32]> {
        if self.next_block >= self.arena_start + self.arena_blocks {
            return None;
        }
        let frame = RecordFrame {
            kind,
            sequence: self.next_sequence,
            store_epoch: self.store_epoch,
            arena: self.arena as u32,
            prev_digest: self.prev_digest,
        };
        let (blocks, digest) = k4::frame_record(slot_bytes(WRITE_SLOT), &frame, body).ok()?;
        if blocks != 1 {
            return None;
        }
        let block = self.next_block;
        if !self.disk.write(block, WRITE_SLOT, mode) {
            return None;
        }
        k2::note(
            note::RECORD_WRITTEN,
            self.next_sequence | (u64::from(kind) << 32),
        );
        self.next_block += 1;
        self.next_sequence += 1;
        self.prev_digest = digest;
        Some(digest)
    }

    /// Blocks left in the active arena.
    pub fn free_blocks(&self) -> u64 {
        (self.arena_start + self.arena_blocks).saturating_sub(self.next_block)
    }

    // -- the object index ----------------------------------------------------

    pub fn find(&self, digest: &[u8; 32]) -> Option<usize> {
        self.objects
            .iter()
            .position(|entry| entry.live && entry.digest == *digest)
    }

    fn free_object_slot(&self) -> Option<usize> {
        self.objects.iter().position(|entry| !entry.live)
    }

    fn free_stage_slot(&self) -> Option<usize> {
        self.stage_used.iter().position(|used| !*used)
    }

    /// Stages an object's canonical bytes and returns its content digest.
    pub fn stage(&mut self, object_type: u32, bytes: &[u8]) -> Result<[u8; 32], i64> {
        if bytes.len() > OBJECT_MAX {
            return Err(store_status::TOO_LARGE as i64);
        }
        let digest = k4::object_digest(object_type, bytes);
        if self.find(&digest).is_some() {
            return Ok(digest);
        }
        let slot = self
            .free_stage_slot()
            .ok_or(store_status::EXHAUSTED as i64)?;
        let index = self
            .free_object_slot()
            .ok_or(store_status::EXHAUSTED as i64)?;
        staged(slot)[..bytes.len()].copy_from_slice(bytes);
        self.stage_used[slot] = true;
        self.stage_len[slot] = bytes.len() as u32;
        self.objects[index] = ObjectEntry {
            digest,
            object_type,
            length: bytes.len() as u32,
            block: 0,
            durable: false,
            stage: slot as u32,
            live: true,
        };
        Ok(digest)
    }

    /// Records an object that is already on the medium.
    fn adopt(&mut self, digest: [u8; 32], object_type: u32, length: u32, block: u64) {
        if let Some(index) = self.find(&digest) {
            self.objects[index].durable = true;
            self.objects[index].block = block;
            return;
        }
        let Some(index) = self.free_object_slot() else {
            return;
        };
        self.objects[index] = ObjectEntry {
            digest,
            object_type,
            length,
            block,
            durable: true,
            stage: u32::MAX,
            live: true,
        };
    }

    /// Copies an object's canonical bytes into `content()`, returning its length.
    ///
    /// A durable object is read back from the medium and its content digest is
    /// recomputed: an index entry is a memory of what was written, and this is
    /// the service checking that memory against the bytes.
    pub fn load(&mut self, index: usize) -> Option<usize> {
        let entry = self.objects[index];
        if !entry.live {
            return None;
        }
        let length = entry.length as usize;
        if entry.stage != u32::MAX {
            let bytes = staged(entry.stage as usize);
            content()[..length].copy_from_slice(&bytes[..length]);
            return Some(length);
        }
        if !entry.durable || !self.disk.read(entry.block, READ_SLOT) {
            return None;
        }
        let span = slot_bytes(READ_SLOT);
        let record = k4::parse_record(span)?;
        if record.header.kind != record_kind::OBJECT {
            return None;
        }
        let body = record.payload(span);
        let head = ObjectRecordHeader::read_from(body, 0)?;
        let start = ObjectRecordHeader::SIZE;
        if head.length as usize != length || body.len() < start + length {
            return None;
        }
        let bytes = &body[start..start + length];
        if k4::object_digest(head.object_type, bytes) != entry.digest {
            return None;
        }
        content()[..length].copy_from_slice(bytes);
        Some(length)
    }

    /// Copies one object's canonical bytes out, verified against its digest.
    ///
    /// `load` fills a buffer private to this module; a caller outside it needs
    /// the bytes themselves, and getting them this way means it never sees an
    /// object whose content did not hash to the identity it asked for.
    pub fn read_object(&mut self, digest: &[u8; 32], out: &mut [u8]) -> Option<(u32, usize)> {
        let index = self.find(digest)?;
        let length = self.load(index)?;
        if out.len() < length {
            return None;
        }
        out[..length].copy_from_slice(&content()[..length]);
        Some((self.objects[index].object_type, length))
    }

    /// Writes one object as a record. Returns false when the medium refused.
    fn write_object(&mut self, index: usize, mode: u32) -> bool {
        let entry = self.objects[index];
        let length = entry.length as usize;
        let Some(read) = self.load(index) else {
            return false;
        };
        if read != length {
            return false;
        }
        let head = ObjectRecordHeader {
            object_type: entry.object_type,
            reserved0: 0,
            length: entry.length as u64,
            content_digest: entry.digest,
        };
        let body = payload();
        if head.write_to(body, 0).is_err() {
            return false;
        }
        body[ObjectRecordHeader::SIZE..ObjectRecordHeader::SIZE + length]
            .copy_from_slice(&content()[..length]);
        let block = self.next_block;
        let total = ObjectRecordHeader::SIZE + length;
        if self
            .append(record_kind::OBJECT, &body[..total], mode)
            .is_none()
        {
            return false;
        }
        self.objects[index].durable = true;
        self.objects[index].block = block;
        if entry.stage != u32::MAX {
            self.stage_used[entry.stage as usize] = false;
            self.objects[index].stage = u32::MAX;
        }
        true
    }

    /// Collects every object reachable from `root`, appending indices to `out`.
    ///
    /// Bounded by the size of `out` and by an explicit depth, because a tree
    /// read back from a medium is untrusted input and a cycle in it would
    /// otherwise be an unbounded walk inside the service.
    pub fn closure(&mut self, root: &[u8; 32], out: &mut [usize], count: &mut usize) -> bool {
        *count = 0;
        let mut queue = [0usize; MAX_OBJECTS];
        let mut head = 0usize;
        let mut tail = 0usize;
        let Some(index) = self.find(root) else {
            return false;
        };
        queue[tail] = index;
        tail += 1;
        let mut depth_guard = 0u32;
        while head < tail {
            depth_guard += 1;
            if depth_guard > MAX_OBJECTS as u32 * 2 {
                return false;
            }
            let index = queue[head];
            head += 1;
            if out[..*count].contains(&index) {
                continue;
            }
            if *count == out.len() {
                return false;
            }
            out[*count] = index;
            *count += 1;

            let entry = self.objects[index];
            let mut children = [[0u8; 32]; MAX_BINDINGS + 3];
            let mut child_count = 0usize;
            match entry.object_type {
                object_type::MANIFEST => {
                    let Some(length) = self.load(index) else {
                        return false;
                    };
                    let Some(manifest) = Manifest::read_from(&content()[..length], 0) else {
                        return false;
                    };
                    children[0] = manifest.tree_digest;
                    children[1] = manifest.policy_digest;
                    children[2] = manifest.validation_digest;
                    child_count = 3;
                }
                object_type::TREE => {
                    let Some(length) = self.load(index) else {
                        return false;
                    };
                    let mut bindings = [Binding::default(); MAX_BINDINGS];
                    let Ok(bound) = k4::decode_tree(&content()[..length], &mut bindings) else {
                        return false;
                    };
                    for binding in bindings.iter().take(bound) {
                        children[child_count] = binding.digest;
                        child_count += 1;
                    }
                }
                _ => {}
            }
            for digest in children.iter().take(child_count) {
                if *digest == [0u8; 32] {
                    continue;
                }
                let Some(child) = self.find(digest) else {
                    return false;
                };
                if tail == queue.len() {
                    return false;
                }
                queue[tail] = child;
                tail += 1;
            }
        }
        true
    }

    // -- principals and results ---------------------------------------------

    pub fn principal_slot(&mut self, principal: u64) -> Option<usize> {
        if let Some(index) = self
            .principals
            .iter()
            .position(|entry| entry.principal == principal)
        {
            return Some(index);
        }
        let index = self
            .principals
            .iter()
            .position(|entry| entry.principal == 0)?;
        self.principals[index].principal = principal;
        Some(index)
    }

    pub fn result_for(&self, principal: u64, sequence: u64) -> Option<ResultEntry> {
        self.results
            .iter()
            .take(self.result_count)
            .find(|entry| entry.principal == principal && entry.request_sequence == sequence)
            .copied()
    }

    fn record_result(&mut self, entry: ResultEntry) {
        if let Some(index) = self
            .results
            .iter()
            .take(self.result_count)
            .position(|held| {
                held.principal == entry.principal && held.request_sequence == entry.request_sequence
            })
        {
            self.results[index] = entry;
            return;
        }
        if self.result_count < MAX_RESULTS {
            self.results[self.result_count] = entry;
            self.result_count += 1;
            return;
        }
        // Evicting a result is not forgetting the request: the high-water mark
        // still refuses it as new, and a later query gets RESULT_EXPIRED, which
        // asserts neither commit nor abort.
        self.results[self.result_next] = entry;
        self.result_next = (self.result_next + 1) % MAX_RESULTS;
        self.results_evicted += 1;
    }

    // -- formatting and recovery --------------------------------------------

    /// Writes a store where there is none.
    pub fn format(&mut self, seed: u64) -> bool {
        self.store_epoch = 1;
        self.uuid = k4::store_uuid(seed, self.store_epoch);
        self.arena = 0;
        self.arena_start = absolute(geometry::ARENA0_START_BLOCK);
        self.arena_blocks = geometry::ARENA_BLOCK_COUNT;
        self.next_block = self.arena_start;
        self.next_sequence = 1;
        self.prev_digest = [0u8; 32];
        self.published_generation = 0;
        self.published_root = [0u8; 32];
        if !self.write_checkpoint(0) {
            return false;
        }
        if !self.flush() {
            return false;
        }
        self.superblock_generation = 1;
        self.superblock_slot = geometry::SUPERBLOCK_A_BLOCK;
        if !self.write_superblock(self.superblock_slot, self.superblock_generation) {
            return false;
        }
        if !self.flush() {
            return false;
        }
        k2::note(note::STORE_FORMATTED, self.store_epoch);
        true
    }

    /// Writes the checkpoint that opens the active arena.
    fn write_checkpoint(&mut self, source_arena: u64) -> bool {
        let principal_count = self
            .principals
            .iter()
            .filter(|entry| entry.principal != 0)
            .count() as u64;
        let head = CheckpointRecord {
            published_generation: self.published_generation,
            published_root: self.published_root,
            published_policy: self.published_policy,
            published_receipt: self.published_receipt,
            principal_count,
            result_count: self.result_count as u64,
            retained_count: self.retained_count as u64,
            object_count: 0,
            created_ns: k2::now_ns(),
            source_arena,
            compaction_generation: self.compaction_generation,
        };
        let mut body = [0u8; 1024];
        let mut at = 0usize;
        if head.write_to(&mut body, at).is_err() {
            return false;
        }
        at += CheckpointRecord::SIZE;
        for entry in self.principals.iter().filter(|entry| entry.principal != 0) {
            if entry.write_to(&mut body, at).is_err() {
                return false;
            }
            at += PrincipalEntry::SIZE;
        }
        for index in 0..self.result_count {
            if self.results[index].write_to(&mut body, at).is_err() {
                return false;
            }
            at += ResultEntry::SIZE;
        }
        for index in 0..self.retained_count {
            if self.retained[index].write_to(&mut body, at).is_err() {
                return false;
            }
            at += RetainedEntry::SIZE;
        }
        let sequence = self.next_sequence;
        // A checkpoint opens an arena, so it links to nothing.
        self.prev_digest = [0u8; 32];
        if self
            .append(record_kind::CHECKPOINT, &body[..at], fault_mode::NONE)
            .is_none()
        {
            return false;
        }
        self.checkpoint_sequence = sequence;
        k2::note(note::CHECKPOINT_WRITTEN, self.arena);
        true
    }

    /// Writes one of the two superblocks.
    fn write_superblock(&mut self, slot: u64, generation: u64) -> bool {
        let mut block = StoreSuperblock::zeroed();
        block.magic = magic::SUPERBLOCK;
        block.format_major = k4::FORMAT_MAJOR;
        block.format_minor = k4::FORMAT_MINOR;
        block.store_uuid = self.uuid;
        block.store_epoch = self.store_epoch;
        block.superblock_generation = generation;
        block.active_arena = self.arena;
        block.arena0_start_block = geometry::ARENA0_START_BLOCK;
        block.arena0_block_count = geometry::ARENA_BLOCK_COUNT;
        block.arena1_start_block = geometry::ARENA1_START_BLOCK;
        block.arena1_block_count = geometry::ARENA_BLOCK_COUNT;
        block.block_size = geometry::BLOCK_SIZE;
        block.checkpoint_sequence = self.checkpoint_sequence;
        block.durable_through_sequence = self.next_sequence - 1;
        block.published_generation = self.published_generation;
        block.published_root = self.published_root;
        block.digest = k4::superblock_digest(&block);

        let span = slot_bytes(WRITE_SLOT);
        span.fill(0);
        if block.write_to(span, 0).is_err() {
            return false;
        }
        if !self
            .disk
            .write(absolute(slot), WRITE_SLOT, fault_mode::NONE)
        {
            return false;
        }
        self.durable_through = block.durable_through_sequence;
        k2::note(note::SUPERBLOCK_SWITCHED, generation);
        true
    }

    /// Reads a superblock, or `None` when that block holds none.
    fn read_superblock(&mut self, slot: u64) -> Option<StoreSuperblock> {
        if !self.disk.read(absolute(slot), READ_SLOT) {
            return None;
        }
        k4::parse_superblock(slot_bytes(READ_SLOT))
    }

    /// Brings the service up from whatever is on the medium.
    pub fn recover(&mut self, seed: u64) -> bool {
        let a = self.read_superblock(geometry::SUPERBLOCK_A_BLOCK);
        let b = self.read_superblock(geometry::SUPERBLOCK_B_BLOCK);
        let chosen = match (a, b) {
            (None, None) => {
                if !self.format(seed) {
                    return false;
                }
                self.ready = true;
                k2::note(note::STORE_RECOVERED, self.published_generation);
                return true;
            }
            (Some(one), None) => (one, geometry::SUPERBLOCK_A_BLOCK),
            (None, Some(two)) => (two, geometry::SUPERBLOCK_B_BLOCK),
            (Some(one), Some(two)) => {
                if one.superblock_generation >= two.superblock_generation {
                    (one, geometry::SUPERBLOCK_A_BLOCK)
                } else {
                    (two, geometry::SUPERBLOCK_B_BLOCK)
                }
            }
        };
        let (superblock, slot) = chosen;
        self.uuid = superblock.store_uuid;
        self.store_epoch = superblock.store_epoch;
        self.superblock_generation = superblock.superblock_generation;
        self.superblock_slot = slot;
        self.arena = superblock.active_arena;
        self.arena_start = absolute(if self.arena == 0 {
            superblock.arena0_start_block
        } else {
            superblock.arena1_start_block
        });
        self.arena_blocks = if self.arena == 0 {
            superblock.arena0_block_count
        } else {
            superblock.arena1_block_count
        };
        self.checkpoint_sequence = superblock.checkpoint_sequence;
        self.durable_through = superblock.durable_through_sequence;

        if !self.replay(&superblock) {
            self.integrity_failed = true;
            k2::note(
                note::INTEGRITY_REFUSED,
                store_status::INTEGRITY_FAILED as u64,
            );
            return false;
        }
        if !self.resolve_pending() {
            return false;
        }
        self.ready = true;
        k2::note(note::STORE_RECOVERED, self.published_generation);
        k2::note(note::ROOT_HEAD, root_head(&self.published_root));
        for entry in self.principals.iter().filter(|entry| entry.principal != 0) {
            k2::note(note::HIGH_WATER, entry.principal | (entry.high_water << 8));
        }
        true
    }

    /// Rebuilds the state from the valid prefix of the active arena.
    fn replay(&mut self, superblock: &StoreSuperblock) -> bool {
        if !self.disk.read(self.arena_start, READ_SLOT) {
            return false;
        }
        let span = slot_bytes(READ_SLOT);
        let Some(record) = k4::parse_record(span) else {
            return false;
        };
        if record.header.kind != record_kind::CHECKPOINT
            || record.header.sequence != superblock.checkpoint_sequence
            || record.header.store_epoch != superblock.store_epoch
            || record.header.arena as u64 != self.arena
        {
            return false;
        }
        if !self.load_checkpoint(record.payload(span)) {
            return false;
        }
        self.next_sequence = record.header.sequence + 1;
        self.next_block = self.arena_start + record.header.block_count;
        self.prev_digest = record.header.digest;
        self.scanned = 1;

        // Forward scan. The first block that is not the record this chain says
        // comes next ends the log, whatever it holds.
        let mut last_commit_generation = self.published_generation;
        loop {
            if self.next_block >= self.arena_start + self.arena_blocks {
                break;
            }
            if !self.disk.read(self.next_block, READ_SLOT) {
                break;
            }
            let span = slot_bytes(READ_SLOT);
            let Some(record) = k4::parse_record(span) else {
                break;
            };
            if record.header.sequence != self.next_sequence
                || record.header.store_epoch != self.store_epoch
                || record.header.arena as u64 != self.arena
                || record.header.prev_digest != self.prev_digest
            {
                break;
            }
            let block = self.next_block;
            let kind = record.header.kind;
            let body = replayed();
            let payload_len = record.payload_len;
            body[..payload_len].copy_from_slice(record.payload(span));
            self.next_block += record.header.block_count;
            self.next_sequence += 1;
            self.prev_digest = record.header.digest;
            self.scanned += 1;
            if !self.replay_record(
                kind,
                &body[..payload_len],
                block,
                &mut last_commit_generation,
            ) {
                return false;
            }
        }
        k2::note(note::RECOVERY_SCANNED, self.scanned);

        // A prefix shorter than what the superblock said was durable is an
        // integrity failure. Skipping forward to a record that happens to
        // verify would be inventing a history.
        if self.next_sequence - 1 < superblock.durable_through_sequence {
            k2::note(
                note::RECOVERY_INTEGRITY_FAILED,
                superblock.durable_through_sequence,
            );
            return false;
        }
        true
    }

    fn load_checkpoint(&mut self, body: &[u8]) -> bool {
        let Some(head) = CheckpointRecord::read_from(body, 0) else {
            return false;
        };
        self.published_generation = head.published_generation;
        self.published_root = head.published_root;
        self.published_policy = head.published_policy;
        self.published_receipt = head.published_receipt;
        self.compaction_generation = head.compaction_generation;
        let mut at = CheckpointRecord::SIZE;
        if head.principal_count > MAX_PRINCIPALS as u64
            || head.result_count > MAX_RESULTS as u64
            || head.retained_count > MAX_RETAINED as u64
        {
            return false;
        }
        for index in 0..head.principal_count as usize {
            let Some(entry) = PrincipalEntry::read_from(body, at) else {
                return false;
            };
            self.principals[index] = entry;
            at += PrincipalEntry::SIZE;
        }
        self.result_count = head.result_count as usize;
        for index in 0..self.result_count {
            let Some(entry) = ResultEntry::read_from(body, at) else {
                return false;
            };
            self.results[index] = entry;
            at += ResultEntry::SIZE;
        }
        self.retained_count = head.retained_count as usize;
        for index in 0..self.retained_count {
            let Some(entry) = RetainedEntry::read_from(body, at) else {
                return false;
            };
            self.retained[index] = entry;
            at += RetainedEntry::SIZE;
        }
        true
    }

    fn replay_record(
        &mut self,
        kind: u32,
        body: &[u8],
        block: u64,
        last_generation: &mut u64,
    ) -> bool {
        match kind {
            record_kind::OBJECT => {
                let Some(head) = ObjectRecordHeader::read_from(body, 0) else {
                    return false;
                };
                let start = ObjectRecordHeader::SIZE;
                let length = head.length as usize;
                if length > OBJECT_MAX || body.len() < start + length {
                    return false;
                }
                // Recomputed, not believed: the header says what the digest is
                // supposed to be and the bytes say what it is.
                if k4::object_digest(head.object_type, &body[start..start + length])
                    != head.content_digest
                {
                    return false;
                }
                self.adopt(head.content_digest, head.object_type, length as u32, block);
            }
            record_kind::PREPARE => {
                let Some(record) = PrepareRecord::read_from(body, 0) else {
                    return false;
                };
                self.pending = Pending {
                    live: true,
                    principal: record.principal,
                    request_sequence: record.request_sequence,
                    prepare_sequence: self.next_sequence - 1,
                    prepare_digest: self.prev_digest,
                    request_digest: record.request_digest,
                    candidate_root: record.candidate_root,
                    reason: abort_reason::RECOVERED_UNRESOLVED,
                };
                if let Some(slot) = self.principal_slot(record.principal) {
                    self.principals[slot].pending_sequence = record.request_sequence;
                }
            }
            record_kind::COMMIT => {
                let Some(record) = CommitRecord::read_from(body, 0) else {
                    return false;
                };
                if !self.pending.live
                    || self.pending.prepare_sequence != record.prepare_sequence
                    || self.pending.prepare_digest != record.prepare_digest
                {
                    return false;
                }
                // The commit has to publish what its prepare named. A record
                // that verifies under its own digest and still names a
                // different root is not a torn tail; it is a contradiction,
                // and adopting it would publish a version no admission ever
                // decided on.
                if self.pending.candidate_root != record.root_digest {
                    k2::note(note::RECOVERY_INTEGRITY_FAILED, record.prepare_sequence);
                    return false;
                }
                // Every object the root names has to be here, and verified.
                // A commit whose dependency is missing is not adopted, and the
                // prepare it names is the one recovery aborts.
                let mut reachable = [0usize; MAX_OBJECTS];
                let mut count = 0usize;
                if !self.closure(&record.root_digest, &mut reachable, &mut count) {
                    self.pending.reason = abort_reason::DEPENDENCY_MISSING;
                    self.dependency_missing += 1;
                    k2::note(note::RECOVERY_DEPENDENCY_MISSING, record.prepare_sequence);
                    return true;
                }
                if reachable[..count]
                    .iter()
                    .any(|index| !self.objects[*index].durable)
                {
                    self.pending.reason = abort_reason::DEPENDENCY_MISSING;
                    self.dependency_missing += 1;
                    k2::note(note::RECOVERY_DEPENDENCY_MISSING, record.prepare_sequence);
                    return true;
                }
                self.published_generation = record.new_generation;
                self.published_root = record.root_digest;
                self.published_policy = record.policy_digest;
                self.published_receipt = record.receipt_digest;
                *last_generation = record.new_generation;
                let entry = ResultEntry {
                    principal: record.principal,
                    request_sequence: record.request_sequence,
                    outcome: result_outcome::COMMITTED,
                    outbox_status: outbox_status::NONE,
                    generation: record.new_generation,
                    root_digest: record.root_digest,
                    request_digest: self.pending.request_digest,
                };
                self.record_result(entry);
                if let Some(slot) = self.principal_slot(record.principal) {
                    self.principals[slot].high_water = record.request_sequence;
                    self.principals[slot].pending_sequence = 0;
                }
                self.pending = Pending::default();
            }
            record_kind::ABORT => {
                let Some(record) = AbortRecord::read_from(body, 0) else {
                    return false;
                };
                let entry = ResultEntry {
                    principal: record.principal,
                    request_sequence: record.request_sequence,
                    outcome: result_outcome::ABORTED,
                    outbox_status: outbox_status::NONE,
                    generation: 0,
                    root_digest: [0u8; 32],
                    request_digest: self.pending.request_digest,
                };
                self.record_result(entry);
                if let Some(slot) = self.principal_slot(record.principal) {
                    self.principals[slot].high_water = record.request_sequence;
                    self.principals[slot].pending_sequence = 0;
                }
                self.pending = Pending::default();
            }
            record_kind::OUTBOX => {
                let Some(record) = OutboxRecord::read_from(body, 0) else {
                    return false;
                };
                if let Some(index) = self
                    .results
                    .iter()
                    .take(self.result_count)
                    .position(|held| {
                        held.principal == record.principal
                            && held.request_sequence == record.request_sequence
                    })
                {
                    self.results[index].outbox_status = record.status;
                }
            }
            record_kind::CHECKPOINT => return false,
            _ => return false,
        }
        true
    }

    /// Aborts, durably, every prepare recovery found unresolved.
    fn resolve_pending(&mut self) -> bool {
        if !self.pending.live {
            return true;
        }
        let pending = self.pending;
        let record = AbortRecord {
            prepare_sequence: pending.prepare_sequence,
            prepare_digest: pending.prepare_digest,
            reason: pending.reason,
            reserved0: 0,
            principal: pending.principal,
            request_sequence: pending.request_sequence,
            aborted_ns: k2::now_ns(),
        };
        if self
            .append(record_kind::ABORT, record.as_bytes(), fault_mode::NONE)
            .is_none()
        {
            return false;
        }
        if !self.flush() {
            return false;
        }
        self.recovery_aborts += 1;
        k2::note(note::RECOVERY_ABORTED, pending.prepare_sequence);
        let entry = ResultEntry {
            principal: pending.principal,
            request_sequence: pending.request_sequence,
            outcome: result_outcome::ABORTED,
            outbox_status: outbox_status::NONE,
            generation: 0,
            root_digest: [0u8; 32],
            request_digest: pending.request_digest,
        };
        self.record_result(entry);
        if let Some(slot) = self.principal_slot(pending.principal) {
            self.principals[slot].high_water = pending.request_sequence;
            self.principals[slot].pending_sequence = 0;
        }
        self.pending = Pending::default();
        true
    }

    // -- publication ---------------------------------------------------------

    /// Everything a publication needs to be decided.
    #[allow(clippy::too_many_arguments)]
    pub fn admit(
        &mut self,
        principal: u64,
        sequence: u64,
        expected_generation: u64,
        candidate_root: &[u8; 32],
        policy_digest: &[u8; 32],
        validation_digest: &[u8; 32],
        outbox_target: u64,
        outbox_payload: &[u8; 32],
    ) -> Result<[u8; 32], u32> {
        if !self.ready {
            return Err(store_status::UNAVAILABLE);
        }
        let request = k4::request_digest(
            principal,
            sequence,
            k4::store_op::PUBLISH,
            expected_generation,
            candidate_root,
            policy_digest,
            validation_digest,
            outbox_target,
            outbox_payload,
        );
        let slot = self
            .principal_slot(principal)
            .ok_or(store_status::FORBIDDEN)?;
        let high_water = self.principals[slot].high_water;
        let pending = self.principals[slot].pending_sequence;

        if pending != 0 && pending != sequence {
            return Err(store_status::SEQUENCE_GAP);
        }
        if sequence <= high_water {
            return match self.result_for(principal, sequence) {
                Some(entry) if entry.request_digest == request => Err(store_status::OK),
                Some(_) => Err(store_status::CONFLICT),
                None => Err(store_status::RESULT_EXPIRED),
            };
        }
        if sequence != high_water + 1 {
            return Err(store_status::SEQUENCE_GAP);
        }

        // Structure before authority before effect, in that order, which is the
        // order the interface itself admits operations in.
        let root_index = self.find(candidate_root).ok_or(store_status::NOT_FOUND)?;
        if self.objects[root_index].object_type != object_type::MANIFEST {
            return Err(store_status::INVALID_REQUEST);
        }
        let length = self
            .load(root_index)
            .ok_or(store_status::INTEGRITY_FAILED)?;
        let manifest =
            Manifest::read_from(&content()[..length], 0).ok_or(store_status::INVALID_REQUEST)?;
        if manifest.policy_digest != *policy_digest
            || manifest.validation_digest != *validation_digest
        {
            return Err(store_status::INVALID_REQUEST);
        }
        let mut reachable = [0usize; MAX_OBJECTS];
        let mut count = 0usize;
        if !self.closure(candidate_root, &mut reachable, &mut count) {
            return Err(store_status::NOT_FOUND);
        }

        let policy_index = self.find(policy_digest).ok_or(store_status::NOT_FOUND)?;
        let policy_len = self
            .load(policy_index)
            .ok_or(store_status::INTEGRITY_FAILED)?;
        let policy =
            Policy::read_from(&content()[..policy_len], 0).ok_or(store_status::INVALID_REQUEST)?;
        if principal >= 64 || policy.publish_principals & (1u64 << principal) == 0 {
            return Err(store_status::FORBIDDEN);
        }
        if policy.requires_validation != 0 {
            let index = self
                .find(validation_digest)
                .ok_or(store_status::VALIDATION_MISMATCH)?;
            let length = self.load(index).ok_or(store_status::INTEGRITY_FAILED)?;
            let validation = Validation::read_from(&content()[..length], 0)
                .ok_or(store_status::VALIDATION_MISMATCH)?;
            // Evidence is a claim about named inputs and a named version. It
            // does not become a claim about a different version by being
            // presented alongside one.
            if validation.result != 1
                || validation.input_tree_digest != manifest.tree_digest
                || validation.base_generation != expected_generation
                || validation.tool_id != policy.validation_tool_id
                || validation.tool_config_digest != policy.tool_config_digest
                || validation.coverage_ppm < policy.min_coverage_ppm
            {
                return Err(store_status::VALIDATION_MISMATCH);
            }
        }
        if expected_generation != self.published_generation {
            return Err(store_status::GENERATION_STALE);
        }

        // Space: what this publication needs, plus what closing it and copying
        // the reachable set would need. Admitting into the reserve is how a
        // store ends up unable to recover itself.
        let missing = reachable[..count]
            .iter()
            .filter(|index| !self.objects[**index].durable)
            .count() as u64;
        let needed = missing + 3 + u64::from(outbox_target != 0);
        let reserve = geometry::CLOSURE_RESERVE_BLOCKS + geometry::COMPACTION_RESERVE_BLOCKS;
        if self.free_blocks() < needed + reserve {
            k2::note(note::EXHAUSTED, self.free_blocks());
            return Err(store_status::EXHAUSTED);
        }
        Ok(request)
    }

    /// Carries out an admitted publication. Returns the new generation.
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        principal: u64,
        sequence: u64,
        expected_generation: u64,
        candidate_root: [u8; 32],
        policy_digest: [u8; 32],
        validation_digest: [u8; 32],
        request: [u8; 32],
        outbox_target: u64,
        outbox_payload: [u8; 32],
        invocation_id: u64,
    ) -> Result<(u64, Fault), u32> {
        // The receipt is an object like any other, and it is written before the
        // commit that names it.
        let receipt = k4::Receipt {
            format: 1,
            reserved0: 0,
            principal,
            request_sequence: sequence,
            request_digest: request,
            expected_generation,
            new_generation: self.published_generation + 1,
            root_digest: candidate_root,
            policy_digest,
            validation_digest,
            committed_ns: k2::now_ns(),
            outbox_target,
        };
        let receipt_digest = self
            .stage(object_type::RECEIPT, receipt.as_bytes())
            .map_err(|_| store_status::EXHAUSTED)?;

        // Asked once. Asking twice would consume the directive on the first
        // call and leave the second with nothing, so a mode meant for the
        // prepare's own write would never reach it.
        let prepare_mode = match self.fault_at(fault_point::BEFORE_PREPARE, 0) {
            Fault::Write(mode) => mode,
            Fault::Stop | Fault::Kill => return Err(store_status::UNAVAILABLE),
            _ => fault_mode::NONE,
        };

        let prepare = PrepareRecord {
            principal,
            request_sequence: sequence,
            request_digest: request,
            expected_generation,
            candidate_root,
            policy_digest,
            validation_digest,
            flags: u64::from(outbox_target != 0),
            outbox_target,
            outbox_payload_digest: outbox_payload,
            admitted_ns: k2::now_ns(),
            invocation_id,
        };
        let prepare_sequence = self.next_sequence;
        let Some(prepare_digest) =
            self.append(record_kind::PREPARE, prepare.as_bytes(), prepare_mode)
        else {
            return Err(store_status::INTEGRITY_FAILED);
        };
        if !self.flush() {
            return Err(store_status::UNAVAILABLE);
        }
        self.pending = Pending {
            live: true,
            principal,
            request_sequence: sequence,
            prepare_sequence,
            prepare_digest,
            request_digest: request,
            candidate_root,
            reason: abort_reason::RECOVERED_UNRESOLVED,
        };
        if let Some(slot) = self.principal_slot(principal) {
            self.principals[slot].pending_sequence = sequence;
        }
        if let Fault::Stop | Fault::Kill = self.fault_at(fault_point::AFTER_PREPARE, 0) {
            return Err(store_status::UNAVAILABLE);
        }

        // Every object the new root depends on that is not already durable.
        let mut reachable = [0usize; MAX_OBJECTS];
        let mut count = 0usize;
        if !self.closure(&candidate_root, &mut reachable, &mut count) {
            return Err(store_status::NOT_FOUND);
        }
        let mut ordinal = 0u64;
        for index in reachable.iter().copied().take(count) {
            if self.objects[index].durable {
                continue;
            }
            ordinal += 1;
            let mode = match self.fault_at(fault_point::BEFORE_OBJECT, ordinal) {
                Fault::Write(mode) => mode,
                Fault::Stop | Fault::Kill => return Err(store_status::UNAVAILABLE),
                _ => fault_mode::NONE,
            };
            if !self.write_object(index, mode) {
                return Err(store_status::INTEGRITY_FAILED);
            }
        }
        let receipt_index = self.find(&receipt_digest).ok_or(store_status::NOT_FOUND)?;
        if !self.objects[receipt_index].durable {
            ordinal += 1;
            let mode = match self.fault_at(fault_point::BEFORE_OBJECT, ordinal) {
                Fault::Write(mode) => mode,
                Fault::Stop | Fault::Kill => return Err(store_status::UNAVAILABLE),
                _ => fault_mode::NONE,
            };
            if !self.write_object(receipt_index, mode) {
                return Err(store_status::INTEGRITY_FAILED);
            }
        }
        if !self.flush() {
            return Err(store_status::UNAVAILABLE);
        }
        if let Fault::Stop | Fault::Kill = self.fault_at(fault_point::AFTER_OBJECTS, 0) {
            return Err(store_status::UNAVAILABLE);
        }

        let commit_mode = match self.fault_at(fault_point::BEFORE_COMMIT, 0) {
            Fault::Write(mode) => mode,
            Fault::Stop | Fault::Kill => return Err(store_status::UNAVAILABLE),
            _ => fault_mode::NONE,
        };
        let new_generation = self.published_generation + 1;
        let commit = CommitRecord {
            prepare_sequence,
            prepare_digest,
            new_generation,
            root_digest: candidate_root,
            policy_digest,
            receipt_digest,
            principal,
            request_sequence: sequence,
            committed_ns: k2::now_ns(),
            object_count: ordinal,
        };
        if self
            .append(record_kind::COMMIT, commit.as_bytes(), commit_mode)
            .is_none()
        {
            // The medium refused the commit. Nothing is published, and the
            // prepare stays durable and unresolved: what happened to this
            // request is not something this run may assert.
            return Err(store_status::INTEGRITY_FAILED);
        }
        if !self.flush() {
            return Err(store_status::UNAVAILABLE);
        }

        self.published_generation = new_generation;
        self.published_root = candidate_root;
        self.published_policy = policy_digest;
        self.published_receipt = receipt_digest;
        self.publications += 1;
        let entry = ResultEntry {
            principal,
            request_sequence: sequence,
            outcome: result_outcome::COMMITTED,
            outbox_status: outbox_status::NONE,
            generation: new_generation,
            root_digest: candidate_root,
            request_digest: request,
        };
        self.record_result(entry);
        if let Some(slot) = self.principal_slot(principal) {
            self.principals[slot].high_water = sequence;
            self.principals[slot].pending_sequence = 0;
        }
        self.pending = Pending::default();
        k2::note(note::PUBLISHED, new_generation);
        k2::note(note::ROOT_HEAD, root_head(&candidate_root));

        let after = self.fault_at(fault_point::AFTER_COMMIT, 0);
        Ok((new_generation, after))
    }

    /// Records what a broker reported about an intent committed with a publication.
    pub fn record_outbox(
        &mut self,
        principal: u64,
        sequence: u64,
        request: [u8; 32],
        target: u64,
        status: u32,
        attempts: u32,
    ) -> bool {
        let record = OutboxRecord {
            commit_sequence: self.next_sequence - 1,
            request_digest: request,
            principal,
            request_sequence: sequence,
            status,
            attempts,
            target,
            recorded_ns: k2::now_ns(),
        };
        if self
            .append(record_kind::OUTBOX, record.as_bytes(), fault_mode::NONE)
            .is_none()
        {
            return false;
        }
        if !self.flush() {
            return false;
        }
        if let Some(index) = self
            .results
            .iter()
            .take(self.result_count)
            .position(|held| held.principal == principal && held.request_sequence == sequence)
        {
            self.results[index].outbox_status = status;
        }
        k2::note(note::OUTBOX_RECORDED, u64::from(status));
        true
    }

    // -- maintenance ---------------------------------------------------------

    /// Copies the reachable set into the other arena and switches to it.
    pub fn compact(&mut self) -> Result<u64, u32> {
        if !self.ready {
            return Err(store_status::UNAVAILABLE);
        }
        if self.pending.live {
            return Err(store_status::CONFLICT);
        }
        let mut reachable = [0usize; MAX_OBJECTS];
        let mut count = 0usize;
        if self.published_generation != 0
            && !self.closure(&self.published_root.clone(), &mut reachable, &mut count)
        {
            return Err(store_status::INTEGRITY_FAILED);
        }
        // Roots held against reuse are copied too, or the retention would be a
        // promise the store could not keep.
        for index in 0..self.retained_count {
            let root = self.retained[index].root_digest;
            let mut held = [0usize; MAX_OBJECTS];
            let mut held_count = 0usize;
            if self.closure(&root, &mut held, &mut held_count) {
                for index in held.iter().copied().take(held_count) {
                    if !reachable[..count].contains(&index) && count < MAX_OBJECTS {
                        reachable[count] = index;
                        count += 1;
                    }
                }
            }
        }
        let target = 1 - self.arena;
        let target_start = absolute(if target == 0 {
            geometry::ARENA0_START_BLOCK
        } else {
            geometry::ARENA1_START_BLOCK
        });
        if count as u64 + 1 > geometry::ARENA_BLOCK_COUNT {
            return Err(store_status::EXHAUSTED);
        }

        // Read every object out of the old arena before anything is written to
        // the new one: after the switch the old extents may be reused, and a
        // copy that read them afterwards would be reading whatever came next.
        let mut copied = [([0u8; 32], 0u32, 0u32); MAX_OBJECTS];
        for slot in 0..count {
            let index = reachable[slot];
            let entry = self.objects[index];
            copied[slot] = (entry.digest, entry.object_type, entry.length);
        }

        let source = self.arena;
        let old_start = self.arena_start;
        let old_blocks = self.arena_blocks;
        self.compaction_generation += 1;
        self.arena = target;
        self.arena_start = target_start;
        self.arena_blocks = geometry::ARENA_BLOCK_COUNT;
        self.next_block = target_start;
        if !self.write_checkpoint(source) {
            self.arena = source;
            self.arena_start = old_start;
            self.arena_blocks = old_blocks;
            return Err(store_status::INTEGRITY_FAILED);
        }
        for entry in copied.iter().take(count) {
            let digest = entry.0;
            let Some(index) = self.find(&digest) else {
                return Err(store_status::INTEGRITY_FAILED);
            };
            // Load from the old arena, then forget where it was: the entry is
            // about to name a block in the new one.
            let Some(length) = self.load(index) else {
                return Err(store_status::INTEGRITY_FAILED);
            };
            let head = ObjectRecordHeader {
                object_type: entry.1,
                reserved0: 0,
                length: length as u64,
                content_digest: digest,
            };
            let body = payload();
            if head.write_to(body, 0).is_err() {
                return Err(store_status::INTEGRITY_FAILED);
            }
            body[ObjectRecordHeader::SIZE..ObjectRecordHeader::SIZE + length]
                .copy_from_slice(&content()[..length]);
            let block = self.next_block;
            if self
                .append(
                    record_kind::OBJECT,
                    &body[..ObjectRecordHeader::SIZE + length],
                    fault_mode::NONE,
                )
                .is_none()
            {
                return Err(store_status::INTEGRITY_FAILED);
            }
            self.objects[index].block = block;
            self.objects[index].durable = true;
        }
        if !self.flush() {
            return Err(store_status::UNAVAILABLE);
        }
        if let Fault::Stop | Fault::Kill = self.fault_at(fault_point::AFTER_CHECKPOINT, 0) {
            return Err(store_status::UNAVAILABLE);
        }

        // Only now is the switch made, and only after it is durable may the
        // old arena be reused.
        let slot = if self.superblock_slot == geometry::SUPERBLOCK_A_BLOCK {
            geometry::SUPERBLOCK_B_BLOCK
        } else {
            geometry::SUPERBLOCK_A_BLOCK
        };
        let generation = self.superblock_generation + 1;
        if !self.write_superblock(slot, generation) {
            return Err(store_status::INTEGRITY_FAILED);
        }
        if !self.flush() {
            return Err(store_status::UNAVAILABLE);
        }
        self.superblock_slot = slot;
        self.superblock_generation = generation;
        self.compactions += 1;
        // Objects that were only in the old arena and were not copied are gone.
        for index in 0..MAX_OBJECTS {
            if self.objects[index].live
                && self.objects[index].durable
                && self.objects[index].block >= old_start
                && self.objects[index].block < old_start + old_blocks
            {
                self.objects[index] = ObjectEntry::default();
            }
        }
        k2::note(note::COMPACTED, count as u64);
        if let Fault::Stop | Fault::Kill = self.fault_at(fault_point::AFTER_SUPERBLOCK, 0) {
            return Err(store_status::UNAVAILABLE);
        }
        Ok(count as u64)
    }

    /// Writes a checkpoint into the current arena without moving anything.
    ///
    /// Used to make the principals' high-water marks and the recent results
    /// durable in a form recovery reads before the log, which is what keeps a
    /// long-running store from replaying its whole history.
    pub fn retain(&mut self, root: [u8; 32], expiry_ns: u64) -> Result<u64, u32> {
        if self.find(&root).is_none() {
            return Err(store_status::NOT_FOUND);
        }
        if let Some(index) = self
            .retained
            .iter()
            .take(self.retained_count)
            .position(|entry| entry.root_digest == root)
        {
            self.retained[index].expiry_ns = expiry_ns;
            return Ok(self.retained_count as u64);
        }
        if self.retained_count == MAX_RETAINED {
            return Err(store_status::EXHAUSTED);
        }
        self.retained[self.retained_count] = RetainedEntry {
            root_digest: root,
            expiry_ns,
        };
        self.retained_count += 1;
        Ok(self.retained_count as u64)
    }

    /// Drops a retention.
    pub fn release(&mut self, root: [u8; 32]) -> Result<u64, u32> {
        let Some(index) = self
            .retained
            .iter()
            .take(self.retained_count)
            .position(|entry| entry.root_digest == root)
        else {
            return Err(store_status::NOT_FOUND);
        };
        for slot in index..self.retained_count - 1 {
            self.retained[slot] = self.retained[slot + 1];
        }
        self.retained_count -= 1;
        Ok(self.retained_count as u64)
    }
}

/// The first eight bytes of a digest, as one integer.
///
/// A note carries two integers, and a digest does not fit in one. This is
/// enough for a gate to match a log line against a medium it decoded itself,
/// and it is not enough to be mistaken for the digest.
pub fn root_head(digest: &[u8; 32]) -> u64 {
    let mut value = 0u64;
    for byte in digest.iter().take(8).rev() {
        value = (value << 8) | u64::from(*byte);
    }
    value
}
