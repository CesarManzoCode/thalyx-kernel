//! Thalyx's managed protocol, carried out on the K4 state service.
//!
//! `thalyx_platform::managed::protocol` is the whole conversation between the
//! real Thalyx's managed client and its store: objects put and fetched by
//! content, a seed, private work forked from a generation, a candidate
//! published by compare-and-swap on that generation with its receipt, work
//! abandoned, evidence kept and found, sequences per principal and the result
//! of a request whose reply was lost. On Linux the far side is
//! `thalyx-managed`'s store over a loopback. Here it is this module, and every
//! sentence of it is a call on the K4 service, which is the only thing on this
//! machine that writes the medium.
//!
//! ## How Thalyx's model lands on K4's
//!
//! K4 publishes a *root*: a manifest over a tree of at most twelve named
//! bindings, whose closure is what a publication makes durable. Thalyx's model
//! is a *line*: a sequence of generations, each a content identity of a tree
//! (`c1-…`), plus objects nobody has published yet and evidence that survives
//! abandonment. So one K4 root carries the whole state of every line this
//! machine serves:
//!
//! ```text
//! root ── O   every Thalyx object this store holds, as a 12-ary tree of
//!         │   K4 objects (an object larger than K4's ceiling is a K4 tree
//!         │   of chunks); this is what keeps them all durable
//!         ├── X   the index: Thalyx digest and kind → K4 digest and shape
//!         └── L<view>   one per line: H its generations, E its evidence by
//!                       transaction, R the receipt of its last decision
//! ```
//!
//! A Thalyx publication is one K4 publication that moves a line's `H`; keeping
//! evidence is another that moves its `E`. Thalyx's generation of a line is
//! the length of its `H`, so two lines and the evidence between them do not
//! move each other's generations. A Thalyx compare-and-swap is decided here
//! against the line's generation and carried out as K4's compare-and-swap on
//! the root's, which is the one that is durable: two publications cannot
//! interleave because this domain serves one request at a time and K4 admits
//! one prepare at a time, and a cut between the prepare and the commit is
//! resolved by K4's recovery exactly as K4's matrix shows.
//!
//! ## What the kernel enforces
//!
//! A Thalyx work -- one transaction -- is a kernel scope the supervisor
//! creates when the work is forked. The grant this module publishes through
//! for that work is derived from the principal's facet under that scope's
//! life, so once the scope is fenced the kernel refuses the publication with
//! `SCOPE_CLOSED` whatever this program does, and `admit` reports the same
//! lineage the kernel would refuse. Abandoning is not gated, on purpose: a
//! closed work keeps nothing, and putting it back is how it keeps nothing.

use thalyx_abi::{cap_lineage, right};
use thalyx_user_k4fmt::generated::{object_type, store_status};
use thalyx_user_k4fmt::{self as k4, Binding, Manifest, Pod, Policy, Validation};
use thalyx_user_k5pkg::link::note;
use thalyx_user_rt::k2;

use crate::json::{self, Strings, Value, Writer};
use crate::store::{Answer, Store};
use crate::works::Works;

/// The format this store speaks; a client that speaks another stops.
pub const FORMAT: &[u8] = b"thalyx-managed-local-v1";
/// The receipt object bytes Thalyx puts and names.
const KIND_BYTES: u8 = 0;
const KIND_TREE: u8 = 1;
const KIND_RECEIPT: u8 = 2;
const KIND_EVIDENCE: u8 = 3;

const KIND_WORDS: [&[u8]; 4] = [b"bytes", b"tree", b"receipt", b"evidence"];

/// Objects the index names. Bounded by K4's own index, which every one of
/// these occupies at least one entry of.
pub const MAX_INDEX: usize = 320;
/// Lines this machine serves at once.
pub const MAX_LINES: usize = 6;
/// Generations a line remembers.
pub const MAX_GENERATIONS: usize = 48;
/// Evidence records a line keeps.
pub const MAX_EVIDENCE: usize = 48;
/// Results answered from memory: what became of a request identity.
pub const MAX_RESULTS: usize = 64;
/// Principals a line distinguishes: one per port, plus a spare zero slot.
/// These are Thalyx's principals, enforced here and by the work scopes; the
/// K4 store sees only this link, its one writer.
pub const MAX_PRINCIPALS: usize = 8;
/// Bytes of a transaction name this store keeps.
pub const NAME_MAX: usize = 64;
/// K4's object ceiling.
const CHUNK: usize = k4::geometry::OBJECT_MAX_BYTES as usize;
/// Bindings a K4 tree holds.
const FANOUT: usize = k4::geometry::MAX_TREE_ENTRIES as usize;
/// The largest object this store carries, chunked.
pub const OBJECT_MAX: usize = 192 * 1024;

#[derive(Clone, Copy)]
pub struct Entry {
    pub thalyx: [u8; 32],
    pub kind: u8,
    pub live: bool,
    /// Written to the medium by a K4 publication.
    pub durable: bool,
    /// `BYTES` for a whole object, `TREE` for a tree of chunks.
    pub shape: u32,
    pub len: u64,
    pub k4: [u8; 32],
}

impl Entry {
    const EMPTY: Entry = Entry {
        thalyx: [0; 32],
        kind: 0,
        live: false,
        durable: false,
        shape: 0,
        len: 0,
        k4: [0; 32],
    };
}

/// One transaction's open work.
#[derive(Clone, Copy)]
pub struct Work {
    pub open: bool,
    pub principal: u32,
    pub name: [u8; NAME_MAX],
    pub name_len: usize,
    pub base_generation: u64,
    /// The supervisor's number for the work's scope.
    pub scope_work: u64,
    /// The scope handle, for `life_scope`.
    pub scope: u64,
    /// The grant publications go through: the facet under the scope's life.
    pub publish_grant: u64,
    pub opened_ns: u64,
}

impl Work {
    const EMPTY: Work = Work {
        open: false,
        principal: 0,
        name: [0; NAME_MAX],
        name_len: 0,
        base_generation: 0,
        scope_work: 0,
        scope: 0,
        publish_grant: 0,
        opened_ns: 0,
    };

    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }
}

#[derive(Clone, Copy)]
pub struct Evidence {
    pub name: [u8; NAME_MAX],
    pub name_len: usize,
    pub digest: [u8; 32],
}

/// One line: a view's store, as Thalyx's client sees it.
#[derive(Clone, Copy)]
pub struct Line {
    pub used: bool,
    pub id: [u8; 12],
    pub generation: u64,
    pub roots: [[u8; 32]; MAX_GENERATIONS],
    pub evidence: [Evidence; MAX_EVIDENCE],
    pub evidence_count: usize,
    /// The receipt of the last decision, as its Thalyx digest.
    pub receipt: [u8; 32],
    pub works: [Work; MAX_PRINCIPALS],
    pub next_sequence: [u64; MAX_PRINCIPALS],
}

impl Line {
    const EMPTY: Line = Line {
        used: false,
        id: [0; 12],
        generation: 0,
        roots: [[0; 32]; MAX_GENERATIONS],
        evidence: [Evidence {
            name: [0; NAME_MAX],
            name_len: 0,
            digest: [0; 32],
        }; MAX_EVIDENCE],
        evidence_count: 0,
        receipt: [0; 32],
        works: [Work::EMPTY; MAX_PRINCIPALS],
        next_sequence: [1; MAX_PRINCIPALS],
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Committed { generation: u64, root: [u8; 32] },
    Forked { generation: u64 },
    Abandoned,
    Stale { expected: u64, current: u64 },
    Aborted,
}

#[derive(Clone, Copy)]
pub struct Result {
    pub used: bool,
    pub line: usize,
    pub principal: u32,
    pub sequence: u64,
    pub outcome: Outcome,
}

/// The K4 side of every line: the root's published generation, the policy
/// and validation objects every K4 publication names, and the sequences.
pub struct Backing {
    pub k4_generation: u64,
    pub policy: [u8; 32],
    pub validation: [u8; 32],
    /// The one writer's next K4 sequence. The link is the medium's single
    /// writer, so the K4 store keeps one principal for it.
    pub next_k4_sequence: u64,
    pub publications: u64,
    pub compactions: u64,
}

/// Buffers too large for a stack: decoded objects and encoded blobs.
struct Buffers {
    /// A decoded object: what a `put` carries, what a `get` reads back, and
    /// the blobs a root encodes.
    object: [u8; OBJECT_MAX],
    /// One K4 tree, encoded or decoded.
    chunk: [u8; CHUNK],
}

static mut BUFFERS: Buffers = Buffers {
    object: [0; OBJECT_MAX],
    chunk: [0; CHUNK],
};

#[allow(clippy::deref_addrof)]
fn buffers() -> &'static mut Buffers {
    // SAFETY: this domain is single-threaded and every caller drops the borrow
    // before the next one is taken.
    unsafe { &mut *(&raw mut BUFFERS) }
}

pub struct Managed {
    pub index: [Entry; MAX_INDEX],
    pub lines: [Line; MAX_LINES],
    pub results: [Result; MAX_RESULTS],
    pub results_next: usize,
    pub backing: Backing,
    pub seed: u64,
    /// The one facet of the K4 state service the link writes the medium
    /// through. Every root commit is this writer's; the Thalyx principals are
    /// enforced above K4, by this store and by the work scopes.
    pub k4_facet: u64,
    pub stage_cap: u64,
    pub store_calls: u64,
}

fn thalyx_digest(kind: u8, bytes: &[u8]) -> [u8; 32] {
    let mut hasher = k4::Sha256::new();
    hasher.update(b"thalyx-object-v1\0");
    hasher.update(KIND_WORDS[kind as usize]);
    hasher.update(b"\0");
    hasher.update(bytes);
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.finish()
}

fn kind_of(word: &[u8]) -> Option<u8> {
    KIND_WORDS
        .iter()
        .position(|known| *known == word)
        .map(|kind| kind as u8)
}

/// A `c1-<hex>` identity's digest.
fn digest_of_id(id: &[u8]) -> Option<[u8; 32]> {
    let rest = id.strip_prefix(b"c1-")?;
    json::digest_of(rest)
}

fn name_of(binding: &Binding) -> &[u8] {
    &binding.name[..binding.name_len]
}

impl Managed {
    pub const fn new(seed: u64, k4_facet: u64, stage_cap: u64) -> Self {
        Managed {
            index: [Entry::EMPTY; MAX_INDEX],
            lines: [Line::EMPTY; MAX_LINES],
            results: [Result {
                used: false,
                line: 0,
                principal: 0,
                sequence: 0,
                outcome: Outcome::Aborted,
            }; MAX_RESULTS],
            results_next: 0,
            backing: Backing {
                k4_generation: 0,
                policy: [0; 32],
                validation: [0; 32],
                next_k4_sequence: 1,
                publications: 0,
                compactions: 0,
            },
            seed,
            k4_facet,
            stage_cap,
            store_calls: 0,
        }
    }

    fn store(&self, _principal: u32) -> Store {
        Store::new(self.k4_facet, self.stage_cap)
    }

    fn account(&mut self, store: &Store) {
        self.store_calls += store.calls;
    }

    // -- the index -----------------------------------------------------------

    fn find(&self, thalyx: &[u8; 32]) -> Option<usize> {
        self.index
            .iter()
            .position(|entry| entry.live && entry.thalyx == *thalyx)
    }

    fn insert(&mut self, entry: Entry) -> bool {
        let Some(slot) = self.index.iter().position(|entry| !entry.live) else {
            return false;
        };
        self.index[slot] = entry;
        true
    }

    // -- blobs: Thalyx objects as K4 objects -----------------------------------
    /// Reads a K4 object of either shape back into `out`.
    fn load_blob(
        &mut self,
        principal: u32,
        k4: &[u8; 32],
        shape: u32,
        len: u64,
        out: &mut [u8],
    ) -> Option<usize> {
        let mut store = self.store(principal);
        let result = load_blob_with(&mut store, k4, shape, len, out, 0);
        self.account(&store);
        result
    }

    // -- the K4 root ---------------------------------------------------------

    /// Publishes the current state of every line as a new K4 root, through
    /// `grant` (a work's grant, or the writer's own facet). Answers K4's
    /// status word on refusal; `u32::MAX` means the kernel refused the grant,
    /// which for a work's grant is a fenced work.
    fn commit_root(&mut self, grant: u64) -> core::result::Result<(), u32> {
        let mut store = Store::new(grant, self.stage_cap);
        let outcome = self.commit_root_with(&mut store);
        self.account(&store);
        outcome
    }

    fn commit_root_with(&mut self, store: &mut Store) -> core::result::Result<(), u32> {
        // O: every object, as a 12-ary tree. Leaves first, then the levels
        // above, each level a tree of the trees below it.
        let mut level: [([u8; 32], u32, u64); MAX_INDEX] = [([0; 32], 0, 0); MAX_INDEX];
        let mut count = 0usize;
        let mut order: [usize; MAX_INDEX] = [0; MAX_INDEX];
        for (index, entry) in self.index.iter().enumerate() {
            if entry.live {
                order[count] = index;
                count += 1;
            }
        }
        // Sorted by identity, so the same set of objects makes the same tree.
        order[..count].sort_unstable_by(|a, b| self.index[*a].thalyx.cmp(&self.index[*b].thalyx));
        for (slot, index) in order[..count].iter().enumerate() {
            let entry = self.index[*index];
            level[slot] = (entry.k4, entry.shape, entry.len);
        }
        let objects = tree_of_level(store, &mut level, count)?;

        // X: the index itself, so the mapping survives this run.
        let encoded = {
            let object = &mut buffers().object;
            let mut at = 0usize;
            for entry in self.index.iter().filter(|entry| entry.live) {
                if at + 80 > object.len() {
                    return Err(store_status::TOO_LARGE);
                }
                object[at] = entry.kind;
                object[at + 1] = entry.shape as u8;
                object[at + 2..at + 8].fill(0);
                object[at + 8..at + 16].copy_from_slice(&entry.len.to_le_bytes());
                object[at + 16..at + 48].copy_from_slice(&entry.thalyx);
                object[at + 48..at + 80].copy_from_slice(&entry.k4);
                at += 80;
            }
            at
        };
        let index_blob = {
            let object = &buffers().object;
            store_blob_with(store, &object[..encoded]).ok_or(store_status::EXHAUSTED)?
        };

        // One tree per line, then the root.
        let mut root: [Binding; FANOUT] = [Binding::default(); FANOUT];
        let mut root_count = 0usize;
        root[root_count] = Binding::new(b"O", objects.1, 0, objects.2, objects.0)
            .map_err(|_| store_status::INVALID_REQUEST)?;
        root_count += 1;
        root[root_count] = Binding::new(b"X", index_blob.1, 0, encoded as u64, index_blob.0)
            .map_err(|_| store_status::INVALID_REQUEST)?;
        root_count += 1;
        for line_index in 0..MAX_LINES {
            let line = self.lines[line_index];
            if !line.used {
                continue;
            }
            let mut members: [Binding; FANOUT] = [Binding::default(); FANOUT];
            let mut member_count = 0usize;
            // H: generation and root, forty bytes each, oldest first.
            let history_len = {
                let object = &mut buffers().object;
                let mut at = 0usize;
                for generation in 0..(line.generation as usize).min(MAX_GENERATIONS) {
                    object[at..at + 8].copy_from_slice(&((generation as u64) + 1).to_le_bytes());
                    object[at + 8..at + 40].copy_from_slice(&line.roots[generation]);
                    at += 40;
                }
                at
            };
            if history_len != 0 {
                let history = {
                    let object = &buffers().object;
                    store_blob_with(store, &object[..history_len]).ok_or(store_status::EXHAUSTED)?
                };
                members[member_count] =
                    Binding::new(b"H", history.1, 0, history_len as u64, history.0)
                        .map_err(|_| store_status::INVALID_REQUEST)?;
                member_count += 1;
            }
            // E: name length, name, digest: ninety-six bytes each.
            let evidence_len = {
                let object = &mut buffers().object;
                let mut at = 0usize;
                for record in &line.evidence[..line.evidence_count] {
                    object[at] = record.name_len as u8;
                    object[at + 1..at + 1 + NAME_MAX].copy_from_slice(&record.name);
                    object[at + 1 + NAME_MAX..at + 33 + NAME_MAX].copy_from_slice(&record.digest);
                    at += 96;
                }
                at
            };
            if evidence_len != 0 {
                let evidence = {
                    let object = &buffers().object;
                    store_blob_with(store, &object[..evidence_len])
                        .ok_or(store_status::EXHAUSTED)?
                };
                members[member_count] =
                    Binding::new(b"E", evidence.1, 0, evidence_len as u64, evidence.0)
                        .map_err(|_| store_status::INVALID_REQUEST)?;
                member_count += 1;
            }
            if line.receipt != [0u8; 32]
                && let Some(found) = self.find(&line.receipt)
            {
                let entry = self.index[found];
                members[member_count] = Binding::new(b"R", entry.shape, 0, entry.len, entry.k4)
                    .map_err(|_| store_status::INVALID_REQUEST)?;
                member_count += 1;
            }
            let (line_tree, line_len) = stage_tree(store, &members[..member_count])?;
            let mut name = [0u8; 25];
            name[0] = b'L';
            for (index, byte) in line.id.iter().enumerate() {
                name[1 + index * 2] = b"0123456789abcdef"[(byte >> 4) as usize];
                name[2 + index * 2] = b"0123456789abcdef"[(byte & 15) as usize];
            }
            if root_count >= FANOUT {
                return Err(store_status::EXHAUSTED);
            }
            root[root_count] = Binding::new(&name, object_type::TREE, 0, line_len, line_tree)
                .map_err(|_| store_status::INVALID_REQUEST)?;
            root_count += 1;
        }
        root[..root_count].sort_unstable_by(|a, b| name_of(a).cmp(name_of(b)));

        // K4: fork the published root, bind, freeze, publish with CAS on the
        // K4 generation. Everything above was staged and is in the closure.
        let generation = self.backing.k4_generation;
        let workspace = store.fork(generation).ok_or(store.last_status)?;
        for binding in &root[..root_count] {
            if store
                .bind(workspace, name_of(binding), binding.digest)
                .is_none()
            {
                let status = store.last_status;
                store.discard(workspace);
                return Err(status);
            }
        }
        let frozen = store
            .freeze(workspace, self.backing.policy, self.backing.validation)
            .ok_or_else(|| {
                let status = store.last_status;
                store.discard(workspace);
                status
            })?;
        let sequence = self.backing.next_k4_sequence;
        let answer = store.publish(
            sequence,
            generation,
            frozen.digest0,
            self.backing.policy,
            self.backing.validation,
        );
        store.discard(workspace);
        match answer {
            Answer::Reply(reply) if reply.status == store_status::OK => {
                self.backing.next_k4_sequence = sequence + 1;
                self.backing.k4_generation = reply.generation;
                self.backing.publications += 1;
                for entry in &mut self.index {
                    if entry.live {
                        entry.durable = true;
                    }
                }
                // The other arena is where the next root goes once this one
                // is nearly full; asking for it early is what keeps a
                // publication from meeting the reserve.
                if reply.free_blocks < 64 {
                    if store.compact().is_some() {
                        self.backing.compactions += 1;
                    }
                }
                Ok(())
            }
            Answer::Reply(reply) => {
                if reply.status == store_status::CONFLICT
                    || reply.status == store_status::SEQUENCE_GAP
                {
                    // The sequence is what the service knows; ask it again.
                    self.learn_sequences();
                }
                Err(reply.status)
            }
            Answer::Outcome(_) => Err(store_status::UNAVAILABLE),
            Answer::Gone(code) => {
                if code == thalyx_abi::status::SCOPE_CLOSED {
                    Err(u32::MAX)
                } else {
                    Err(store_status::UNAVAILABLE)
                }
            }
        }
    }

    /// Finds the writer's next K4 sequence from what the service holds, after
    /// a reboot: an identity with a durable result is spent.
    pub fn learn_sequences(&mut self) {
        let mut store = Store::new(self.k4_facet, self.stage_cap);
        let mut sequence = 1u64;
        loop {
            let Some(reply) = store.result(sequence) else {
                break;
            };
            if reply.status == store_status::OK && reply.outcome == k4::result_outcome::NEVER_SEEN {
                break;
            }
            sequence += 1;
            if sequence > 4096 {
                break;
            }
        }
        self.backing.next_k4_sequence = sequence.max(1);
        self.account(&store);
    }

    /// Stages the policy and validation objects every root names, and reads
    /// the published root back into memory, if there is one.
    pub fn recover(&mut self) -> bool {
        let mut store = Store::new(self.k4_facet, self.stage_cap);
        let policy = Policy {
            format: 1,
            requires_validation: 0,
            publish_principals: (1u64 << MAX_PRINCIPALS) - 2,
            validation_tool_id: 0,
            min_coverage_ppm: 0,
            tool_config_digest: [0; 32],
        };
        // The validation a K4 root carries is Thalyx's own receipt, bound as
        // `R` of its line; this object says so, and the policy above does not
        // ask K4 to check what Thalyx already decided.
        let validation = Validation {
            format: 1,
            result: 1,
            input_tree_digest: [0; 32],
            base_generation: 0,
            tool_id: 0,
            tool_config_digest: [0; 32],
            coverage_ppm: 0,
            produced_ns: 0,
        };
        let Some(policy_digest) = store.put_object(object_type::POLICY, policy.as_bytes()) else {
            return false;
        };
        let Some(validation_digest) =
            store.put_object(object_type::VALIDATION, validation.as_bytes())
        else {
            return false;
        };
        self.backing.policy = policy_digest;
        self.backing.validation = validation_digest;
        let Some(query) = store.query() else {
            return false;
        };
        self.backing.k4_generation = query.generation;
        self.account(&store);
        self.learn_sequences();
        if query.generation == 0 {
            return true;
        }
        let mut store = Store::new(self.k4_facet, self.stage_cap);
        let ok = self.load_root_with(&mut store, query.digest0);
        self.account(&store);
        ok
    }

    fn load_root_with(&mut self, store: &mut Store, root: [u8; 32]) -> bool {
        let Some(length) = store.read_object(root, Manifest::SIZE as u64) else {
            return false;
        };
        let Some(manifest) = Manifest::read_from(&crate::store::stage()[..length as usize], 0)
        else {
            return false;
        };
        let mut bindings: [Binding; FANOUT] = [Binding::default(); FANOUT];
        let Some(count) = read_tree(store, &manifest.tree_digest, &mut bindings) else {
            return false;
        };
        let mut objects = 0u64;
        for binding in &bindings[..count] {
            match name_of(binding) {
                b"O" => {}
                b"X" => {
                    let out = &mut buffers().object;
                    let Some(len) = load_blob_with(
                        store,
                        &binding.digest,
                        binding.child_type,
                        binding.length,
                        out,
                        0,
                    ) else {
                        return false;
                    };
                    let mut at = 0usize;
                    while at + 80 <= len {
                        let mut entry = Entry::EMPTY;
                        entry.kind = out[at];
                        entry.shape = u32::from(out[at + 1]);
                        entry.len = u64::from_le_bytes(out[at + 8..at + 16].try_into().unwrap());
                        entry.thalyx.copy_from_slice(&out[at + 16..at + 48]);
                        entry.k4.copy_from_slice(&out[at + 48..at + 80]);
                        entry.live = true;
                        entry.durable = true;
                        if !self.insert(entry) {
                            return false;
                        }
                        objects += 1;
                        at += 80;
                    }
                }
                name if name.len() == 25 && name[0] == b'L' => {
                    let mut id = [0u8; 12];
                    for (index, byte) in id.iter_mut().enumerate() {
                        let Some(high) = json::hex_value(name[1 + index * 2]) else {
                            return false;
                        };
                        let Some(low) = json::hex_value(name[2 + index * 2]) else {
                            return false;
                        };
                        *byte = (high << 4) | low;
                    }
                    let Some(slot) = self.line_slot(&id) else {
                        return false;
                    };
                    let mut members: [Binding; FANOUT] = [Binding::default(); FANOUT];
                    let Some(member_count) = read_tree(store, &binding.digest, &mut members) else {
                        return false;
                    };
                    for member in &members[..member_count] {
                        let out = &mut buffers().object;
                        match name_of(member) {
                            b"H" | b"E" => {
                                let Some(len) = load_blob_with(
                                    store,
                                    &member.digest,
                                    member.child_type,
                                    member.length,
                                    out,
                                    0,
                                ) else {
                                    return false;
                                };
                                if name_of(member) == b"H" {
                                    let mut at = 0usize;
                                    let line = &mut self.lines[slot];
                                    line.generation = 0;
                                    while at + 40 <= len
                                        && (line.generation as usize) < MAX_GENERATIONS
                                    {
                                        let generation = line.generation as usize;
                                        line.roots[generation]
                                            .copy_from_slice(&out[at + 8..at + 40]);
                                        line.generation += 1;
                                        at += 40;
                                    }
                                } else {
                                    let mut at = 0usize;
                                    let line = &mut self.lines[slot];
                                    line.evidence_count = 0;
                                    while at + 96 <= len && line.evidence_count < MAX_EVIDENCE {
                                        let record = &mut line.evidence[line.evidence_count];
                                        record.name_len = (out[at] as usize).min(NAME_MAX);
                                        record
                                            .name
                                            .copy_from_slice(&out[at + 1..at + 1 + NAME_MAX]);
                                        record.digest.copy_from_slice(
                                            &out[at + 1 + NAME_MAX..at + 33 + NAME_MAX],
                                        );
                                        line.evidence_count += 1;
                                        at += 96;
                                    }
                                }
                            }
                            b"R" => {
                                // Named by its Thalyx digest through the index,
                                // once the index is read; the binding itself
                                // is enough to keep it durable.
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        // The receipt of each line is the one its evidence and history name;
        // the index knows every receipt, and the last one bound is `R`.
        k2::note(note::INDEX_REBUILT, objects);
        true
    }

    // -- lines ---------------------------------------------------------------

    /// The line with this identity, made if it is new.
    pub fn line_slot(&mut self, id: &[u8; 12]) -> Option<usize> {
        if let Some(found) = self
            .lines
            .iter()
            .position(|line| line.used && line.id == *id)
        {
            return Some(found);
        }
        let slot = self.lines.iter().position(|line| !line.used)?;
        self.lines[slot] = Line::EMPTY;
        self.lines[slot].used = true;
        self.lines[slot].id = *id;
        Some(slot)
    }

    fn record(&mut self, line: usize, principal: u32, sequence: u64, outcome: Outcome) {
        let at = self.results_next % MAX_RESULTS;
        self.results_next += 1;
        self.results[at] = Result {
            used: true,
            line,
            principal,
            sequence,
            outcome,
        };
    }

    fn result_for(&self, line: usize, principal: u32, sequence: u64) -> Option<Outcome> {
        self.results
            .iter()
            .find(|result| {
                result.used
                    && result.line == line
                    && result.principal == principal
                    && result.sequence == sequence
            })
            .map(|result| result.outcome)
    }

    // -- the protocol --------------------------------------------------------

    /// Serves one managed request from `principal` on `line`, writing the
    /// reply. `works` is the supervisor's work-scope service.
    pub fn serve(
        &mut self,
        line: usize,
        principal: u32,
        request: &[u8],
        works: &mut Works,
        out: &mut [u8],
    ) -> Option<usize> {
        let mut writer = Writer::new(out);
        let Some(op) = json::string(request, "op") else {
            return refuse(
                &mut writer,
                b"unintelligible",
                b"a managed request names no op",
            );
        };
        match op {
            b"hello" => {
                writer
                    .raw(b"{\"reply\":\"hello\",\"format\":")
                    .str(FORMAT)
                    .raw(b",\"store\":\"k4-")
                    .hex(&self.seed.to_be_bytes())
                    .raw(b"\",\"epoch\":1}");
            }
            b"published" => {
                let line = &self.lines[line];
                writer.raw(b"{\"reply\":\"version\",\"generation\":");
                writer.number(line.generation).raw(b",\"root\":");
                if line.generation == 0 {
                    writer.raw(b"null");
                } else {
                    root_id(&mut writer, &line.roots[line.generation as usize - 1]);
                }
                writer.raw(b"}");
            }
            b"history" => {
                let line = &self.lines[line];
                writer.raw(b"{\"reply\":\"history\",\"roots\":[");
                for generation in 0..line.generation as usize {
                    if generation != 0 {
                        writer.raw(b",");
                    }
                    writer.raw(b"[").number(generation as u64 + 1).raw(b",");
                    root_id(&mut writer, &line.roots[generation]);
                    writer.raw(b"]");
                }
                writer.raw(b"]}");
            }
            b"missing" => {
                let Some(Value::Array(array)) = json::field(request, "digests") else {
                    return refuse(&mut writer, b"invalid", b"`missing` names no digests");
                };
                writer.raw(b"{\"reply\":\"missing\",\"digests\":[");
                let mut first = true;
                for raw in Strings::of(array) {
                    let known = json::digest_of(raw)
                        .and_then(|digest| self.find(&digest))
                        .is_some_and(|found| self.index[found].kind == KIND_BYTES);
                    if !known {
                        if !first {
                            writer.raw(b",");
                        }
                        first = false;
                        writer.str(raw);
                    }
                }
                writer.raw(b"]}");
            }
            b"put" => {
                let Some(kind) = json::string(request, "kind").and_then(kind_of) else {
                    return refuse(
                        &mut writer,
                        b"invalid",
                        b"an object has no kind this store holds",
                    );
                };
                let Some(hex) = json::string(request, "hex") else {
                    return refuse(&mut writer, b"invalid", b"an object is not hex");
                };
                let object = &mut buffers().object;
                let Some(len) = json::hex_decode(hex, object) else {
                    return refuse(&mut writer, b"invalid", b"an object is not hex");
                };
                if kind == KIND_TREE && !object[..len].starts_with(b"thalyx-tree-v1\n") {
                    return refuse(
                        &mut writer,
                        b"invalid",
                        b"a tree does not begin with `thalyx-tree-v1`",
                    );
                }
                let digest = thalyx_digest(kind, &object[..len]);
                if self.find(&digest).is_none() {
                    let stored = {
                        let object = &buffers().object;
                        let mut store = self.store(principal);
                        let stored = store_blob_with(&mut store, &object[..len]);
                        self.account(&store);
                        stored
                    };
                    let Some((k4, shape)) = stored else {
                        return refuse(
                            &mut writer,
                            b"unwritable",
                            b"the state service refused to stage the object",
                        );
                    };
                    if !self.insert(Entry {
                        thalyx: digest,
                        kind,
                        live: true,
                        durable: false,
                        shape,
                        len: len as u64,
                        k4,
                    }) {
                        return refuse(&mut writer, b"unwritable", b"this store's index is full");
                    }
                }
                writer
                    .raw(b"{\"reply\":\"stored\",\"digest\":")
                    .digest(&digest)
                    .raw(b"}");
            }
            b"get" => {
                let Some(digest) = json::string(request, "digest").and_then(json::digest_of) else {
                    return refuse(&mut writer, b"absent", b"not a digest this store names");
                };
                let Some(found) = self.find(&digest) else {
                    return refuse(
                        &mut writer,
                        b"absent",
                        b"nothing is stored under that digest",
                    );
                };
                let entry = self.index[found];
                let loaded = {
                    let out = &mut buffers().object;
                    self.load_blob(principal, &entry.k4, entry.shape, entry.len, out)
                };
                let Some(len) = loaded else {
                    return refuse(
                        &mut writer,
                        b"unreadable",
                        b"the state service could not read the object back",
                    );
                };
                writer
                    .raw(b"{\"reply\":\"object\",\"kind\":")
                    .str(KIND_WORDS[entry.kind as usize])
                    .raw(b",\"hex\":")
                    .hex(&buffers().object[..len])
                    .raw(b"}");
            }
            b"sequence" => {
                let sequence = self.lines[line].next_sequence[principal as usize];
                writer
                    .raw(b"{\"reply\":\"next\",\"sequence\":")
                    .number(sequence)
                    .raw(b"}");
            }
            b"result" => {
                let Some(Value::Object(id)) = json::field(request, "request") else {
                    return refuse(&mut writer, b"invalid", b"`result` names no request");
                };
                let sequence = json::number(id, "sequence").unwrap_or(0);
                match self.result_for(line, principal, sequence) {
                    Some(outcome) => outcome_reply(&mut writer, outcome),
                    None => {
                        writer.raw(b"{\"reply\":\"unknown\"}");
                    }
                }
            }
            b"seed" => return self.seed_request(line, principal, request, writer),
            b"fork" => return self.fork_request(line, principal, request, works, writer),
            b"publish" => return self.publish_request(line, principal, request, works, writer),
            b"abandon" => return self.abandon_request(line, principal, request, works, writer),
            b"keep_evidence" => {
                let Some(name) = json::string(request, "transaction") else {
                    return refuse(
                        &mut writer,
                        b"invalid",
                        b"`keep_evidence` names no transaction",
                    );
                };
                let shaped = !name.is_empty()
                    && name.len() <= NAME_MAX
                    && name
                        .iter()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(byte));
                if !shaped {
                    return refuse(&mut writer, b"invalid", b"not the shape of a handle");
                }
                let Some(digest) = json::string(request, "digest").and_then(json::digest_of) else {
                    return refuse(&mut writer, b"invalid", b"the evidence is not a digest");
                };
                if !self
                    .find(&digest)
                    .is_some_and(|found| self.index[found].kind == KIND_EVIDENCE)
                {
                    return refuse(&mut writer, b"invalid", b"the evidence is not in the store");
                }
                {
                    let target = &mut self.lines[line];
                    let slot =
                        match target.evidence[..target.evidence_count]
                            .iter()
                            .position(|record| {
                                record.name_len == name.len() && &record.name[..name.len()] == name
                            }) {
                            Some(found) => found,
                            None if target.evidence_count < MAX_EVIDENCE => {
                                target.evidence_count += 1;
                                target.evidence_count - 1
                            }
                            None => {
                                return refuse(
                                    &mut writer,
                                    b"unwritable",
                                    b"this line keeps no more evidence",
                                );
                            }
                        };
                    let record = &mut target.evidence[slot];
                    record.name = [0; NAME_MAX];
                    record.name[..name.len()].copy_from_slice(name);
                    record.name_len = name.len();
                    record.digest = digest;
                }
                let grant = self.k4_facet;
                let _ = principal;
                if let Err(status) = self.commit_root(grant) {
                    k2::note(note::STORE_REFUSED, u64::from(status) | (0xE << 24));
                    return refuse(
                        &mut writer,
                        b"unwritable",
                        b"the state service refused to keep the evidence",
                    );
                }
                writer.raw(b"{\"reply\":\"recorded\"}");
            }
            b"find_evidence" => {
                let Some(name) = json::string(request, "transaction") else {
                    return refuse(
                        &mut writer,
                        b"invalid",
                        b"`find_evidence` names no transaction",
                    );
                };
                let target = &self.lines[line];
                match target.evidence[..target.evidence_count]
                    .iter()
                    .find(|record| {
                        record.name_len == name.len() && &record.name[..name.len()] == name
                    }) {
                    Some(record) => {
                        writer
                            .raw(b"{\"reply\":\"found\",\"digest\":")
                            .digest(&record.digest)
                            .raw(b"}");
                    }
                    None => {
                        writer.raw(b"{\"reply\":\"absent\"}");
                    }
                }
            }
            _ => {
                return refuse(
                    &mut writer,
                    b"unintelligible",
                    b"not a managed request this store serves",
                );
            }
        }
        writer.finish()
    }

    /// The request identity of a request, and the sequence it consumes.
    fn take_sequence(&mut self, line: usize, principal: u32, request: &[u8]) -> Option<u64> {
        let Some(Value::Object(id)) = json::field(request, "request") else {
            return None;
        };
        let sequence = json::number(id, "sequence")?;
        let next = &mut self.lines[line].next_sequence[principal as usize];
        if sequence >= *next {
            *next = sequence + 1;
        }
        Some(sequence)
    }

    /// Whether every file a tree names is in this store.
    fn whole_tree(
        &mut self,
        principal: u32,
        root: &[u8; 32],
    ) -> core::result::Result<(), &'static [u8]> {
        let Some(found) = self.find(root) else {
            return Err(b"the candidate is not a tree this store holds");
        };
        let entry = self.index[found];
        if entry.kind != KIND_TREE {
            return Err(b"the candidate is not a tree");
        }
        let loaded = {
            let out = &mut buffers().object;
            self.load_blob(principal, &entry.k4, entry.shape, entry.len, out)
        };
        let Some(len) = loaded else {
            return Err(b"the candidate's tree could not be read back");
        };
        let object = &buffers().object[..len];
        for line in object.split(|byte| *byte == b'\n').skip(1) {
            if line.first() != Some(&b'F') {
                continue;
            }
            let Some(digest) = line.get(2..66).and_then(json::digest_of) else {
                return Err(b"a line of the candidate's tree is not a file line");
            };
            if !self
                .find(&digest)
                .is_some_and(|found| self.index[found].kind == KIND_BYTES)
            {
                return Err(b"the candidate names a file this store does not hold");
            }
        }
        Ok(())
    }

    fn seed_request(
        &mut self,
        line: usize,
        principal: u32,
        request: &[u8],
        mut writer: Writer<'_>,
    ) -> Option<usize> {
        let Some(sequence) = self.take_sequence(line, principal, request) else {
            return refuse(&mut writer, b"invalid", b"`seed` names no request");
        };
        if let Some(outcome) = self.result_for(line, principal, sequence) {
            outcome_reply(&mut writer, outcome);
            return writer.finish();
        }
        if self.lines[line].generation != 0 {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(
                &mut writer,
                b"already_seeded",
                b"the store already publishes a generation, so there is nothing to seed",
            );
        }
        let Some(root) = json::string(request, "root").and_then(digest_of_id) else {
            return refuse(
                &mut writer,
                b"invalid",
                b"the root is not a version this store can name",
            );
        };
        if let Err(why) = self.whole_tree(principal, &root) {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(&mut writer, b"invalid", why);
        }
        let Some(receipt) = json::string(request, "receipt").and_then(json::digest_of) else {
            return refuse(&mut writer, b"invalid", b"the receipt is not in the store");
        };
        if !self
            .find(&receipt)
            .is_some_and(|found| self.index[found].kind == KIND_RECEIPT)
        {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(&mut writer, b"invalid", b"the receipt is not in the store");
        }
        self.lines[line].roots[0] = root;
        self.lines[line].generation = 1;
        self.lines[line].receipt = receipt;
        let grant = self.k4_facet;
        if let Err(status) = self.commit_root(grant) {
            self.lines[line].generation = 0;
            k2::note(note::STORE_REFUSED, u64::from(status) | (0x5 << 24));
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(
                &mut writer,
                b"unwritable",
                b"the state service refused the seed",
            );
        }
        k2::note(note::PUBLISHED, 1);
        self.record(
            line,
            principal,
            sequence,
            Outcome::Committed {
                generation: 1,
                root,
            },
        );
        outcome_reply(
            &mut writer,
            Outcome::Committed {
                generation: 1,
                root,
            },
        );
        writer.finish()
    }

    fn fork_request(
        &mut self,
        line: usize,
        principal: u32,
        request: &[u8],
        works: &mut Works,
        mut writer: Writer<'_>,
    ) -> Option<usize> {
        let Some(sequence) = self.take_sequence(line, principal, request) else {
            return refuse(&mut writer, b"invalid", b"`fork` names no request");
        };
        if let Some(outcome) = self.result_for(line, principal, sequence) {
            outcome_reply(&mut writer, outcome);
            return writer.finish();
        }
        let Some(name) = json::string(request, "transaction") else {
            return refuse(&mut writer, b"invalid", b"`fork` names no transaction");
        };
        if name.is_empty() || name.len() > NAME_MAX {
            return refuse(
                &mut writer,
                b"invalid",
                b"the transaction is not the shape of a handle",
            );
        }
        if self.lines[line].works[principal as usize].open {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(
                &mut writer,
                b"already_open",
                b"an attempt is already open for this principal; settle it before starting another",
            );
        }
        let generation = json::number(request, "generation").unwrap_or(0);
        let current = self.lines[line].generation;
        if generation == 0 || generation != current {
            self.record(
                line,
                principal,
                sequence,
                Outcome::Stale {
                    expected: generation,
                    current,
                },
            );
            outcome_reply(
                &mut writer,
                Outcome::Stale {
                    expected: generation,
                    current,
                },
            );
            return writer.finish();
        }
        // The work: a kernel scope, and a grant to publish through whose life
        // that scope bounds.
        let Some((scope_work, scope)) = works.open(line as u32, name) else {
            return refuse(
                &mut writer,
                b"unavailable",
                b"no work scope could be opened for this transaction",
            );
        };
        let Ok(publish_grant) = k2::derive(
            self.k4_facet,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
            scope,
        ) else {
            works.close(scope_work);
            let _ = k2::cap_close(scope);
            return refuse(
                &mut writer,
                b"unavailable",
                b"the work's grant could not be derived",
            );
        };
        let work = &mut self.lines[line].works[principal as usize];
        *work = Work::EMPTY;
        work.open = true;
        work.principal = principal;
        work.name[..name.len()].copy_from_slice(name);
        work.name_len = name.len();
        work.base_generation = generation;
        work.scope_work = scope_work;
        work.scope = scope;
        work.publish_grant = publish_grant;
        work.opened_ns = k2::now_ns();
        k2::note(note::WORK_OPENED, scope_work);
        self.record(line, principal, sequence, Outcome::Forked { generation });
        outcome_reply(&mut writer, Outcome::Forked { generation });
        writer.finish()
    }

    /// Closes a work's grant and scope.
    fn close_work(&mut self, line: usize, principal: u32, works: &mut Works) {
        let work = self.lines[line].works[principal as usize];
        if !work.open {
            return;
        }
        let _ = k2::cap_close(work.publish_grant);
        let _ = k2::cap_close(work.scope);
        works.close(work.scope_work);
        self.lines[line].works[principal as usize].open = false;
    }

    fn publish_request(
        &mut self,
        line: usize,
        principal: u32,
        request: &[u8],
        works: &mut Works,
        mut writer: Writer<'_>,
    ) -> Option<usize> {
        let Some(sequence) = self.take_sequence(line, principal, request) else {
            return refuse(&mut writer, b"invalid", b"`publish` names no request");
        };
        if let Some(outcome) = self.result_for(line, principal, sequence) {
            outcome_reply(&mut writer, outcome);
            return writer.finish();
        }
        let Some(name) = json::string(request, "transaction") else {
            return refuse(&mut writer, b"invalid", b"`publish` names no transaction");
        };
        let work = self.lines[line].works[principal as usize];
        if !work.open || work.name() != name {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(
                &mut writer,
                b"not_open",
                b"that is not the work open for this principal, so it has nothing to publish",
            );
        }
        let expected = json::number(request, "expected_generation").unwrap_or(0);
        let current = self.lines[line].generation;
        if expected != current {
            self.record(
                line,
                principal,
                sequence,
                Outcome::Stale { expected, current },
            );
            outcome_reply(&mut writer, Outcome::Stale { expected, current });
            return writer.finish();
        }
        let Some(candidate) = json::string(request, "candidate").and_then(digest_of_id) else {
            return refuse(
                &mut writer,
                b"invalid",
                b"the candidate is not a version this store can name",
            );
        };
        if let Err(why) = self.whole_tree(principal, &candidate) {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(&mut writer, b"invalid", why);
        }
        let Some(receipt) = json::string(request, "receipt").and_then(json::digest_of) else {
            return refuse(&mut writer, b"invalid", b"the receipt is not in the store");
        };
        if !self
            .find(&receipt)
            .is_some_and(|found| self.index[found].kind == KIND_RECEIPT)
        {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(&mut writer, b"invalid", b"the receipt is not in the store");
        }
        if current as usize >= MAX_GENERATIONS {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(
                &mut writer,
                b"unwritable",
                b"this line remembers no more generations",
            );
        }
        // Through the work's grant: a fenced work is refused here by the
        // kernel, and nothing below runs.
        let generation = current + 1;
        self.lines[line].roots[current as usize] = candidate;
        self.lines[line].generation = generation;
        self.lines[line].receipt = receipt;
        match self.commit_root(work.publish_grant) {
            Ok(()) => {
                k2::note(note::PUBLISHED, generation);
                self.close_work(line, principal, works);
                self.record(
                    line,
                    principal,
                    sequence,
                    Outcome::Committed {
                        generation,
                        root: candidate,
                    },
                );
                outcome_reply(
                    &mut writer,
                    Outcome::Committed {
                        generation,
                        root: candidate,
                    },
                );
            }
            Err(u32::MAX) => {
                self.lines[line].generation = current;
                k2::note(note::WORK_CLOSED, work.scope_work);
                k2::note(note::FENCE_SEEN_NS, k2::now_ns());
                self.record(line, principal, sequence, Outcome::Aborted);
                return refuse(&mut writer, b"work_closed", b"the work was closed before it could publish: the kernel refused the grant it would have published through");
            }
            Err(status) => {
                self.lines[line].generation = current;
                k2::note(note::STORE_REFUSED, u64::from(status) | (0xB << 24));
                self.record(line, principal, sequence, Outcome::Aborted);
                return refuse(
                    &mut writer,
                    b"unwritable",
                    b"the state service refused the publication",
                );
            }
        }
        writer.finish()
    }

    fn abandon_request(
        &mut self,
        line: usize,
        principal: u32,
        request: &[u8],
        works: &mut Works,
        mut writer: Writer<'_>,
    ) -> Option<usize> {
        let Some(sequence) = self.take_sequence(line, principal, request) else {
            return refuse(&mut writer, b"invalid", b"`abandon` names no request");
        };
        if let Some(outcome) = self.result_for(line, principal, sequence) {
            outcome_reply(&mut writer, outcome);
            return writer.finish();
        }
        let Some(name) = json::string(request, "transaction") else {
            return refuse(&mut writer, b"invalid", b"`abandon` names no transaction");
        };
        let work = self.lines[line].works[principal as usize];
        if !work.open || work.name() != name {
            self.record(line, principal, sequence, Outcome::Aborted);
            return refuse(
                &mut writer,
                b"not_open",
                b"that is not the work open for this principal, so there is nothing to abandon",
            );
        }
        if let Some(receipt) = json::string(request, "receipt")
            && !receipt.is_empty()
        {
            let Some(digest) = json::digest_of(receipt) else {
                return refuse(&mut writer, b"invalid", b"the receipt is not in the store");
            };
            if !self
                .find(&digest)
                .is_some_and(|found| self.index[found].kind == KIND_RECEIPT)
            {
                self.record(line, principal, sequence, Outcome::Aborted);
                return refuse(&mut writer, b"invalid", b"the receipt is not in the store");
            }
            self.lines[line].receipt = digest;
        }
        // Not gated by the work's fence, and not a publication: what the work
        // staged stays staged until the next root names it or K4 sweeps it.
        self.close_work(line, principal, works);
        self.record(line, principal, sequence, Outcome::Abandoned);
        writer.raw(b"{\"reply\":\"abandoned\"}");
        writer.finish()
    }

    /// What the kernel says about a work's grant: live, or closed.
    pub fn work_admit(
        &self,
        line: usize,
        principal: u32,
        name: &[u8],
    ) -> core::result::Result<(), &'static [u8]> {
        if line >= MAX_LINES || principal as usize >= MAX_PRINCIPALS {
            return Err(b"no such line or principal");
        }
        let work = self.lines[line].works[principal as usize];
        if !work.open || (!name.is_empty() && work.name() != name) {
            return Err(b"no such work is open");
        }
        match k2::cap_inspect(work.publish_grant) {
            Ok(info) if info.lineage_state == cap_lineage::LIVE => Ok(()),
            Ok(_) => {
                Err(b"the work's scope was fenced: the kernel refuses every grant derived under it")
            }
            Err(_) => Err(b"the work's grant is gone"),
        }
    }
}

fn root_id(writer: &mut Writer<'_>, root: &[u8; 32]) {
    writer.raw(b"\"c1-");
    for byte in root {
        writer.raw(&[
            b"0123456789abcdef"[(byte >> 4) as usize],
            b"0123456789abcdef"[(byte & 15) as usize],
        ]);
    }
    writer.raw(b"\"");
}

fn outcome_reply(writer: &mut Writer<'_>, outcome: Outcome) {
    match outcome {
        Outcome::Committed { generation, root } => {
            writer
                .raw(b"{\"reply\":\"committed\",\"generation\":")
                .number(generation)
                .raw(b",\"root\":");
            root_id(writer, &root);
            writer.raw(b"}");
        }
        Outcome::Forked { generation } => {
            writer
                .raw(b"{\"reply\":\"forked\",\"generation\":")
                .number(generation)
                .raw(b"}");
        }
        Outcome::Abandoned => {
            writer.raw(b"{\"reply\":\"abandoned\"}");
        }
        Outcome::Stale { expected, current } => {
            writer
                .raw(b"{\"reply\":\"stale\",\"expected\":")
                .number(expected)
                .raw(b",\"current\":")
                .number(current)
                .raw(b"}");
        }
        Outcome::Aborted => {
            writer.raw(b"{\"reply\":\"aborted\",\"reason\":\"the request was refused, and the refusal is what was recorded\"}");
        }
    }
}

fn refuse(writer: &mut Writer<'_>, word: &[u8], message: &[u8]) -> Option<usize> {
    k2::note(
        note::REFUSED,
        u64::from(word[0]) | (u64::from(word.get(1).copied().unwrap_or(0)) << 8),
    );
    // The writer may hold a partial reply; start over at the beginning.
    writer.reset();
    writer
        .raw(b"{\"reply\":\"refused\",\"word\":")
        .str(word)
        .raw(b",\"message\":")
        .str(message)
        .raw(b"}");
    writer.done()
}

/// Stages one tree of bindings; answers its K4 digest and encoded length.
fn stage_tree(
    store: &mut Store,
    bindings: &[Binding],
) -> core::result::Result<([u8; 32], u64), u32> {
    let chunk = &mut buffers().chunk;
    let written = k4::encode_tree(chunk, bindings).map_err(|_| store_status::INVALID_REQUEST)?;
    let digest = store
        .put_object(object_type::TREE, &chunk[..written])
        .ok_or(store.last_status.max(1))?;
    Ok((digest, written as u64))
}

/// Builds a 12-ary tree over `count` objects `(digest, type, length)`,
/// answering the root `(digest, type, length)`.
fn tree_of_level(
    store: &mut Store,
    level: &mut [([u8; 32], u32, u64); MAX_INDEX],
    mut count: usize,
) -> core::result::Result<([u8; 32], u32, u64), u32> {
    if count == 0 {
        let (digest, len) = stage_tree(store, &[])?;
        return Ok((digest, object_type::TREE, len));
    }
    while count > 1 || level[0].1 != object_type::TREE {
        let mut next = 0usize;
        let mut at = 0usize;
        while at < count {
            let take = (count - at).min(FANOUT);
            let mut bindings: [Binding; FANOUT] = [Binding::default(); FANOUT];
            for (slot, member) in level[at..at + take].iter().enumerate() {
                let name = [b'0' + (slot / 10) as u8, b'0' + (slot % 10) as u8];
                bindings[slot] = Binding::new(&name, member.1, 0, member.2, member.0)
                    .map_err(|_| store_status::INVALID_REQUEST)?;
            }
            let (digest, len) = stage_tree(store, &bindings[..take])?;
            level[next] = (digest, object_type::TREE, len);
            next += 1;
            at += take;
        }
        count = next;
        if count == 1 {
            break;
        }
    }
    Ok(level[0])
}

/// Stores bytes as one K4 object, or as a tree of chunks.
fn store_blob_with(store: &mut Store, bytes: &[u8]) -> Option<([u8; 32], u32)> {
    if bytes.len() <= CHUNK {
        return store
            .put_object(object_type::BYTES, bytes)
            .map(|digest| (digest, object_type::BYTES));
    }
    let mut level: [([u8; 32], u32, u64); MAX_INDEX] = [([0; 32], 0, 0); MAX_INDEX];
    let mut count = 0usize;
    for piece in bytes.chunks(CHUNK) {
        if count >= MAX_INDEX {
            return None;
        }
        let digest = store.put_object(object_type::BYTES, piece)?;
        level[count] = (digest, object_type::BYTES, piece.len() as u64);
        count += 1;
    }
    tree_of_level(store, &mut level, count)
        .ok()
        .map(|(digest, kind, _)| (digest, kind))
}

/// Reads a K4 tree's bindings.
fn read_tree(store: &mut Store, digest: &[u8; 32], out: &mut [Binding]) -> Option<usize> {
    let length = store.read_object(*digest, CHUNK as u64)?;
    let chunk = &mut buffers().chunk;
    chunk[..length as usize].copy_from_slice(&crate::store::stage()[..length as usize]);
    k4::decode_tree(&chunk[..length as usize], out).ok()
}

/// Reads a blob of either shape into `out` from `at`; answers the end.
fn load_blob_with(
    store: &mut Store,
    k4: &[u8; 32],
    shape: u32,
    len: u64,
    out: &mut [u8],
    at: usize,
) -> Option<usize> {
    if shape == object_type::BYTES {
        let length = store.read_object(*k4, len.min(CHUNK as u64))? as usize;
        if at + length > out.len() {
            return None;
        }
        out[at..at + length].copy_from_slice(&crate::store::stage()[..length]);
        return Some(at + length);
    }
    let mut bindings: [Binding; FANOUT] = [Binding::default(); FANOUT];
    let count = read_tree(store, k4, &mut bindings)?;
    let mut cursor = at;
    for binding in bindings[..count].iter() {
        cursor = load_blob_with(
            store,
            &binding.digest,
            binding.child_type,
            binding.length,
            out,
            cursor,
        )?;
    }
    Some(cursor)
}
