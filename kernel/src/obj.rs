//! Object references, capability tables and the grant tree.
//!
//! Three things with three different lifetimes meet here, and the architecture
//! is explicit that they must not be collapsed into one:
//!
//! * a **handle** is a 64-bit integer local to one domain, made of a table slot
//!   and a generation. It is not a pointer, it carries no authority between
//!   domains, and a stale one names a slot that has moved on;
//! * a **grant** is the node that holds the rights, the deadline, the parent
//!   and the bounding scope. Copies share it; derivations add a child that can
//!   only remove rights or bring the deadline forward;
//! * an **object** is what the grant authorises, named by a kind, a table index
//!   and that entry's own generation, so a recycled object slot is refused
//!   rather than reinterpreted.
//!
//! Generations solve the naming half of reuse and nothing else. A live internal
//! reference to a retired object is a different problem, and is handled by
//! holding the machine lock for the whole of an operation rather than by the
//! counter.

use thalyx_abi::{generated, right};

use crate::limits::{MAX_CAPS, MAX_GRANTS};

/// Index of a scope in the scope table.
pub type ScopeId = u16;
/// Index of a grant in the grant table.
pub type GrantId = u16;

/// A grant index that names no grant.
pub const NO_GRANT: GrantId = u16::MAX;

/// Kinds of kernel object a capability can name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjKind {
    /// Resource principal.
    Scope,
    /// Protection boundary.
    Domain,
    /// Sequence of physical pages.
    Memory,
    /// Bounded message queue.
    Endpoint,
    /// Admitted work and its obligation.
    Invocation,
    /// Coalescing bits.
    Signal,
    /// Monotonic expiry.
    Timer,
    /// Control-receipt ring.
    ControlLog,
    /// An assigned device function.
    Device,
}

impl ObjKind {
    /// Interface type code from the ABI schema.
    #[must_use]
    pub const fn abi_type(self) -> u32 {
        match self {
            ObjKind::Scope => generated::object_type::SCOPE,
            ObjKind::Domain => generated::object_type::DOMAIN,
            ObjKind::Memory => generated::object_type::MEMORY,
            ObjKind::Endpoint => generated::object_type::ENDPOINT,
            ObjKind::Invocation => generated::object_type::INVOCATION,
            ObjKind::Signal => generated::object_type::SIGNAL,
            ObjKind::Timer => generated::object_type::TIMER,
            ObjKind::ControlLog => generated::object_type::CONTROL_LOG,
            ObjKind::Device => generated::object_type::DEVICE,
        }
    }

    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ObjKind::Scope => "scope",
            ObjKind::Domain => "domain",
            ObjKind::Memory => "memory",
            ObjKind::Endpoint => "endpoint",
            ObjKind::Invocation => "invocation",
            ObjKind::Signal => "signal",
            ObjKind::Timer => "timer",
            ObjKind::ControlLog => "control_log",
            ObjKind::Device => "device",
        }
    }

    /// Rights every object type defines, whatever it is.
    ///
    /// These say what may be done with the *capability* -- read it, narrow it,
    /// hand it on, withdraw it -- as opposed to what may be done through it to
    /// the object. A creator holds all of them over what it just created; a
    /// ceiling on an object's type-specific rights does not bound them.
    pub const COMMON_RIGHTS: u32 =
        right::INSPECT | right::DERIVE | right::TRANSFER | right::DESTROY | right::ADMIN;

    /// Every right this type defines, common bits included. Rights outside it
    /// are refused rather than stored, so one bit never means two things.
    #[must_use]
    pub const fn rights_mask(self) -> u32 {
        let common = Self::COMMON_RIGHTS;
        let specific = match self {
            ObjKind::Scope => {
                right::SCOPE_CREATE | right::SCOPE_LIMIT | right::SCOPE_FENCE | right::SCOPE_EXEC
            }
            ObjKind::Domain => {
                right::DOMAIN_BUILD
                    | right::DOMAIN_ACTIVATE
                    | right::DOMAIN_STOP
                    | right::DOMAIN_WAIT
            }
            ObjKind::Memory => {
                right::MEMORY_READ
                    | right::MEMORY_WRITE
                    | right::MEMORY_EXECUTE
                    | right::MEMORY_MAP
                    | right::MEMORY_SEAL
            }
            ObjKind::Endpoint => {
                right::ENDPOINT_SEND
                    | right::ENDPOINT_CALL
                    | right::ENDPOINT_RECEIVE
                    | right::ENDPOINT_BIND
            }
            ObjKind::Invocation => {
                right::INVOCATION_REPLY
                    | right::INVOCATION_EFFECT
                    | right::INVOCATION_RESOLVE
                    | right::INVOCATION_BIND
            }
            ObjKind::Signal => right::SIGNAL_RAISE | right::SIGNAL_WAIT,
            ObjKind::Timer => right::TIMER_ARM,
            ObjKind::ControlLog => right::LOG_READ | right::LOG_APPEND | right::LOG_ACK,
            ObjKind::Device => {
                right::DEVICE_MAP | right::DEVICE_IRQ | right::DEVICE_DMA | right::DEVICE_CONTROL
            }
        };
        common | specific
    }
}

/// A reference to one kernel object, checked against that entry's generation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjRef {
    /// What kind of object.
    pub kind: ObjKind,
    /// Index in that kind's table.
    pub index: u16,
    /// Generation the entry had when the reference was made.
    pub generation: u32,
}

impl ObjRef {
    /// A reference to `index` of `kind` at `generation`.
    #[must_use]
    pub const fn new(kind: ObjKind, index: u16, generation: u32) -> Self {
        Self {
            kind,
            index,
            generation,
        }
    }
}

/// One entry of a domain's capability table.
#[derive(Clone, Copy, Debug)]
pub struct CapEntry {
    /// Generation of the slot. Zero means the slot was never occupied; an
    /// occupied slot always has a non-zero generation, which is what makes a
    /// zero handle invalid.
    pub generation: u32,
    /// Whether the slot currently holds a capability.
    pub live: bool,
    /// Whether the slot is permanently withdrawn because its generation space
    /// is exhausted. A withdrawn slot is never reused in this epoch.
    pub retired: bool,
    /// The object the capability names.
    pub object: ObjRef,
    /// The grant that carries its rights and lifetime.
    pub grant: GrantId,
}

impl CapEntry {
    const fn empty() -> Self {
        Self {
            generation: 0,
            live: false,
            retired: false,
            object: ObjRef::new(ObjKind::Scope, 0, 0),
            grant: NO_GRANT,
        }
    }
}

/// A domain's local capability table.
///
/// Two threads of one domain share it: the handle namespace is the domain's,
/// not the thread's, because the domain is the authority boundary.
pub struct CapTable {
    /// Slots, addressed by the low 32 bits of a handle.
    pub slots: [CapEntry; MAX_CAPS],
}

impl CapTable {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [CapEntry::empty(); MAX_CAPS],
        }
    }

    /// Number of live entries.
    #[must_use]
    pub fn live(&self) -> usize {
        self.slots.iter().filter(|slot| slot.live).count()
    }

    /// Number of slots still available for an installation.
    #[must_use]
    pub fn free_slots(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| !slot.live && !slot.retired)
            .count()
    }

    /// Resolves a handle to its entry, checking slot bounds and generation.
    ///
    /// A handle whose generation does not match names an entry that has moved
    /// on; it is refused rather than reinterpreted as the new occupant.
    #[must_use]
    pub fn lookup(&self, handle: u64) -> Option<&CapEntry> {
        if handle == 0 {
            return None;
        }
        let slot = thalyx_abi::handle_slot(handle) as usize;
        let generation = thalyx_abi::handle_generation(handle);
        let entry = self.slots.get(slot)?;
        if !entry.live || entry.generation != generation {
            return None;
        }
        Some(entry)
    }

    /// Installs a capability in a chosen slot, which must be free.
    ///
    /// A slot whose generation would wrap is withdrawn permanently instead of
    /// being handed out again: the alternative is hoping a 32-bit counter never
    /// comes round, which is not a protection mechanism.
    pub fn install_at(&mut self, slot: usize, object: ObjRef, grant: GrantId) -> Option<u64> {
        let entry = self.slots.get_mut(slot)?;
        if entry.live || entry.retired {
            return None;
        }
        if entry.generation == u32::MAX {
            entry.retired = true;
            return None;
        }
        entry.generation += 1;
        entry.live = true;
        entry.object = object;
        entry.grant = grant;
        Some(thalyx_abi::handle(slot as u32, entry.generation))
    }

    /// Installs a capability in the lowest free slot.
    pub fn install(&mut self, object: ObjRef, grant: GrantId) -> Option<(usize, u64)> {
        for slot in 0..MAX_CAPS {
            if self.slots[slot].live || self.slots[slot].retired {
                continue;
            }
            let handle = self.install_at(slot, object, grant)?;
            return Some((slot, handle));
        }
        None
    }

    /// Releases a live handle and returns what it named.
    pub fn release(&mut self, handle: u64) -> Option<CapEntry> {
        let slot = thalyx_abi::handle_slot(handle) as usize;
        let generation = thalyx_abi::handle_generation(handle);
        let entry = self.slots.get_mut(slot)?;
        if !entry.live || entry.generation != generation {
            return None;
        }
        let copy = *entry;
        entry.live = false;
        entry.grant = NO_GRANT;
        Some(copy)
    }

    /// Releases the entry in `slot` whatever its generation, used when a
    /// delivery installed at admission has to be withdrawn again.
    pub fn release_slot(&mut self, slot: usize) -> Option<CapEntry> {
        let entry = self.slots.get_mut(slot)?;
        if !entry.live {
            return None;
        }
        let copy = *entry;
        entry.live = false;
        entry.grant = NO_GRANT;
        Some(copy)
    }
}

impl Default for CapTable {
    fn default() -> Self {
        Self::new()
    }
}

/// A node of the grant tree: what a lineage of capabilities may do and for how
/// long.
///
/// Copies of a capability share one grant. Derivation adds a child whose rights
/// are a subset and whose deadline is not later, so an effective-rights check
/// can read the node directly. Liveness cannot: a fence anywhere above must
/// stop the child, so every admission walks the chain.
#[derive(Clone, Copy, Debug)]
pub struct Grant {
    /// Whether the node is in use, as a live grant or as a tombstone.
    pub used: bool,
    /// Diagnostic identity, unique within the boot epoch.
    pub id: u64,
    /// Parent node, or `NO_GRANT` for a root.
    pub parent: GrantId,
    /// Distance from the root. Bounded by the interface's derivation depth.
    pub depth: u16,
    /// Effective rights: never larger than the parent's.
    pub rights: u32,
    /// Monotonic deadline, or zero for none. Never later than the parent's.
    pub deadline_ns: u64,
    /// Extra scope whose life bounds this lineage, or `None`.
    pub life_scope: Option<ScopeId>,
    /// Immutable endpoint facet, or zero. Copying and deriving preserve it.
    pub facet: u64,
    /// The object this lineage authorises.
    pub object: ObjRef,
    /// Whether a barrier has been placed on this node.
    pub fenced: bool,
    /// Capability entries referring to this node.
    pub refs: u32,
    /// Live children, so a tombstone survives while a descendant needs it.
    pub children: u32,
    /// Scope charged for the node's metadata.
    pub sponsor: ScopeId,
}

impl Grant {
    /// A free node.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            id: 0,
            parent: NO_GRANT,
            depth: 0,
            rights: 0,
            deadline_ns: 0,
            life_scope: None,
            facet: 0,
            object: ObjRef::new(ObjKind::Scope, 0, 0),
            fenced: false,
            refs: 0,
            children: 0,
            sponsor: 0,
        }
    }
}

/// Marks `root` and every descendant fenced, returning how many nodes changed.
///
/// The walk is a repeated sweep rather than recursion: the depth is bounded by
/// the interface's derivation depth, and a sweep needs no stack in a path that
/// may run from an interrupt-masked section.
pub fn fence_lineage(grants: &[crate::sync::SpinLock<Grant>; MAX_GRANTS], root: GrantId) -> u32 {
    let mut changed = 0;
    if let Some(node) = grants.get(root as usize) {
        let mut node = node.lock();
        if node.used && !node.fenced {
            node.fenced = true;
            changed += 1;
        }
    }
    let depth_bound = thalyx_abi::limit::MAX_DERIVE_DEPTH as usize;
    for _ in 0..depth_bound {
        let mut progressed = false;
        for index in 0..MAX_GRANTS {
            // One node at a time, the child released before its parent is
            // read: the sweep converges on the same set either way, and a
            // holder of two nodes at once would have to agree with everything
            // else about which of them comes first.
            let (used, fenced, parent) = {
                let node = grants[index].lock();
                (node.used, node.fenced, node.parent)
            };
            if !used || fenced || parent == NO_GRANT {
                continue;
            }
            if grants[parent as usize].lock().fenced {
                grants[index].lock().fenced = true;
                changed += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    changed
}
