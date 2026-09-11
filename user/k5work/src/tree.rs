//! The private workspace: names and bytes, held here and published there.
//!
//! Thalyx's workspace is a Btrfs subvolume and its attempt is a snapshot of
//! one. Neither exists here and neither is emulated. What the port keeps is
//! what the contract asks it to keep: work happens against a named version, in
//! a place nothing published can see, and abandoning it leaves the published
//! version exactly where it was.
//!
//! So a workspace is a fork in the managed-state service plus this: a bounded
//! set of names and bytes the work edits in its own memory and binds into that
//! fork. There is no allocator in a user domain of this kernel, so the bound is
//! a fixed array and reaching it is a refusal with a name.

use thalyx_user_k4fmt::generated::object_type;
use thalyx_user_k4fmt::{self as k4, Binding};

/// Names one workspace may hold. The store's tree takes twelve.
pub const MAX_ENTRIES: usize = 8;
/// Bytes one entry may hold. The store's object ceiling is 3072.
pub const MAX_BYTES: usize = 3072;

/// One name and its content.
#[derive(Clone, Copy)]
pub struct Entry {
    name: [u8; 32],
    name_len: usize,
    bytes: [u8; MAX_BYTES],
    len: usize,
    /// Whether this entry differs from the version the workspace was forked
    /// from. Observed by comparing content, not remembered from a claim.
    pub changed: bool,
}

impl Entry {
    const fn zeroed() -> Self {
        Self {
            name: [0; 32],
            name_len: 0,
            bytes: [0; MAX_BYTES],
            len: 0,
            changed: false,
        }
    }

    /// The name, without its padding.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }

    /// The content.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Why an edit did not happen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditError {
    /// No entry of that name.
    NoSuchName,
    /// The workspace is full.
    Full,
    /// The result would not fit an object.
    TooLarge,
    /// The text to replace is not there.
    NotFound,
    /// The name is not one this workspace admits.
    BadName,
}

impl EditError {
    /// The word this refusal is reported as.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            EditError::NoSuchName => "no_such_name",
            EditError::Full => "workspace_full",
            EditError::TooLarge => "too_large",
            EditError::NotFound => "not_found",
            EditError::BadName => "bad_name",
        }
    }
}

/// The whole of one work's private state.
pub struct Workspace {
    entries: [Entry; MAX_ENTRIES],
    count: usize,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    /// An empty workspace.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [Entry::zeroed(); MAX_ENTRIES],
            count: 0,
        }
    }

    /// How many names it holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// The entries, in the order they were put.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries[..self.count]
    }

    /// The index of a name.
    #[must_use]
    pub fn find(&self, name: &[u8]) -> Option<usize> {
        self.entries[..self.count]
            .iter()
            .position(|entry| entry.name() == name)
    }

    /// The content of a name.
    #[must_use]
    pub fn read(&self, name: &[u8]) -> Option<&[u8]> {
        self.find(name).map(|index| self.entries[index].bytes())
    }

    /// Puts a name, replacing what was there.
    ///
    /// `baseline` says whether this is the state the workspace was forked from,
    /// in which case the entry is not a change. Everything after that is.
    pub fn put(&mut self, name: &[u8], bytes: &[u8], baseline: bool) -> Result<(), EditError> {
        if !k4::legal_name(name) || name.len() > 32 {
            return Err(EditError::BadName);
        }
        if bytes.len() > MAX_BYTES {
            return Err(EditError::TooLarge);
        }
        let index = match self.find(name) {
            Some(index) => index,
            None => {
                if self.count == MAX_ENTRIES {
                    return Err(EditError::Full);
                }
                let index = self.count;
                self.count += 1;
                self.entries[index].name[..name.len()].copy_from_slice(name);
                self.entries[index].name_len = name.len();
                index
            }
        };
        let same = self.entries[index].bytes() == bytes;
        self.entries[index].bytes[..bytes.len()].copy_from_slice(bytes);
        self.entries[index].len = bytes.len();
        if !baseline && !same {
            self.entries[index].changed = true;
        }
        if baseline {
            self.entries[index].changed = false;
        }
        Ok(())
    }

    /// Replaces the first occurrence of `before` with `after` in one entry.
    ///
    /// One occurrence and not all of them: a substitution that quietly changed
    /// six places when the caller meant one is the edit nobody reviews.
    pub fn substitute(
        &mut self,
        name: &[u8],
        before: &[u8],
        after: &[u8],
    ) -> Result<usize, EditError> {
        let index = self.find(name).ok_or(EditError::NoSuchName)?;
        let len = self.entries[index].len;
        let at =
            find_bytes(&self.entries[index].bytes[..len], before).ok_or(EditError::NotFound)?;
        let new_len = len - before.len() + after.len();
        if new_len > MAX_BYTES {
            return Err(EditError::TooLarge);
        }
        let mut rebuilt = [0u8; MAX_BYTES];
        rebuilt[..at].copy_from_slice(&self.entries[index].bytes[..at]);
        rebuilt[at..at + after.len()].copy_from_slice(after);
        let tail = at + before.len();
        rebuilt[at + after.len()..new_len].copy_from_slice(&self.entries[index].bytes[tail..len]);
        self.entries[index].bytes = rebuilt;
        self.entries[index].len = new_len;
        self.entries[index].changed = true;
        Ok(at)
    }

    /// How many entries differ from the version this was forked from.
    #[must_use]
    pub fn changed(&self) -> usize {
        self.entries[..self.count]
            .iter()
            .filter(|entry| entry.changed)
            .count()
    }

    /// Builds the bindings a tree is encoded from, **in canonical order**.
    ///
    /// The order is the format's, not this workspace's: a tree is encoded with
    /// its names strictly ascending, and the service refuses one that is not.
    /// Sorting here rather than insisting the caller inserts in order is the
    /// difference between a rule the format enforces and a rule two programs
    /// have to remember; the first version of this file did the second and the
    /// whole vertical stopped at the first freeze, silently, because a helper
    /// answered `None`.
    ///
    /// The service encodes the same bindings and answers with the digest it
    /// got. The caller compares: two encoders that disagree about the bytes of
    /// a tree would make every digest after this meaningless.
    pub fn bindings(&self, digests: &[[u8; 32]], out: &mut [Binding]) -> Option<usize> {
        if digests.len() < self.count || out.len() < self.count {
            return None;
        }
        let mut order = [0usize; MAX_ENTRIES];
        for (index, slot) in order.iter_mut().enumerate().take(self.count) {
            *slot = index;
        }
        // Insertion sort over four to eight names. Anything cleverer would be
        // more code than the thing it sorts.
        for i in 1..self.count {
            let mut j = i;
            while j > 0 && self.entries[order[j - 1]].name() > self.entries[order[j]].name() {
                order.swap(j - 1, j);
                j -= 1;
            }
        }
        for position in 0..self.count {
            let index = order[position];
            out[position] = Binding::new(
                self.entries[index].name(),
                object_type::BYTES,
                0,
                self.entries[index].len as u64,
                digests[index],
            )
            .ok()?;
        }
        Some(self.count)
    }
}

/// Where `needle` first appears in `haystack`.
#[must_use]
pub fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    for at in 0..=haystack.len() - needle.len() {
        if &haystack[at..at + needle.len()] == needle {
            return Some(at);
        }
    }
    None
}

/// How many times `needle` appears in `haystack`, without overlapping.
#[must_use]
pub fn count_bytes(haystack: &[u8], needle: &[u8]) -> usize {
    let mut at = 0;
    let mut found = 0;
    while at < haystack.len() {
        match find_bytes(&haystack[at..], needle) {
            Some(offset) => {
                found += 1;
                at += offset + needle.len();
            }
            None => break,
        }
    }
    found
}
