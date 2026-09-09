//! Endpoints, messages and admitted work.
//!
//! Three facts are kept apart here because the IPC contract insists they are
//! different: a message was **admitted**, its capabilities were **installed**,
//! and its result was **delivered**. A caller that stops waiting learns nothing
//! about the first two, and a failed delivery undoes neither.
//!
//! Admission reserves everything at once — a queue cell, the queue bytes, an
//! invocation record, the receiver's capability slots and a control receipt —
//! or it reserves nothing. That joint reservation is what INV-09 asks for: a
//! receiver never observes a subset of a message's capabilities.
//!
//! K2 resolves the open question the K2 dossier records about when a moved
//! capability is consumed. The capabilities are installed in the receiver's
//! table **at admission**, which is the only order in which "moving invalidates
//! the source only once the whole set is installed in the receiver" can be
//! true. A delivery that is later withdrawn — because the sender's authority
//! was fenced, or the endpoint closed — removes those entries again, so an
//! unread delivery has a defined life rather than an undefined one.

use thalyx_abi::generated::{cancel_state, effect_state, invocation_state};
use thalyx_abi::limit::MAX_INLINE_PAYLOAD;

use crate::limits::MAX_MESSAGES;
use crate::obj::{GrantId, NO_GRANT, ScopeId};

/// A message index that names no message.
pub const NO_MESSAGE: u16 = u16::MAX;

/// A bounded queue with one owner, an epoch and explicit facets.
pub struct Endpoint {
    /// Whether the slot is in use.
    pub used: bool,
    /// Whether the endpoint still admits messages.
    pub open: bool,
    /// Generation of this table slot.
    pub generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Session epoch. Restarting a service produces a new endpoint and a new
    /// epoch; facets are unique within one epoch and never resurrect.
    pub epoch: u64,
    /// Scope charged for the queue.
    pub owner_scope: ScopeId,
    /// Domain that holds the receive capability. An endpoint has one
    /// destination; several workers of that domain may serve it.
    pub receiver_domain: u16,
    /// Generation of that domain when it became the receiver.
    pub receiver_generation: u32,
    /// Messages the queue may hold, reserved cells included.
    pub capacity: u32,
    /// Cells reserved for traffic that must never be refused, namely the fault
    /// reports of domains that named this endpoint as their supervisor channel.
    pub reserved_cells: u32,
    /// Reserved cells currently occupied.
    pub reserved_used: u32,
    /// Messages queued.
    pub queued: u32,
    /// Head of the admission-ordered queue.
    pub head: u16,
    /// Tail of the admission-ordered queue.
    pub tail: u16,
    /// Facets minted from this endpoint.
    pub facets: u32,
    /// Next facet the binder will hand out.
    pub next_facet: u64,
    /// Messages admitted since creation.
    pub admitted: u64,
    /// Messages delivered since creation.
    pub delivered: u64,
    /// Diagnostic label.
    pub label: [u8; 16],
    /// Capability entries naming this endpoint.
    pub refs: u32,
}

impl Endpoint {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            open: false,
            generation: 0,
            id: 0,
            epoch: 0,
            owner_scope: 0,
            receiver_domain: u16::MAX,
            receiver_generation: 0,
            capacity: 0,
            reserved_cells: 0,
            reserved_used: 0,
            queued: 0,
            head: NO_MESSAGE,
            tail: NO_MESSAGE,
            facets: 0,
            next_facet: 1,
            admitted: 0,
            delivered: 0,
            label: [0; 16],
            refs: 0,
        }
    }

    /// Label as text, for diagnostic records.
    #[must_use]
    pub fn label_str(&self) -> &str {
        let len = self.label.iter().position(|byte| *byte == 0).unwrap_or(16);
        core::str::from_utf8(&self.label[..len]).unwrap_or("?")
    }

    /// Cells ordinary traffic may occupy.
    #[must_use]
    pub const fn ordinary_capacity(&self) -> u32 {
        self.capacity.saturating_sub(self.reserved_cells)
    }

    /// Whether ordinary traffic can still be admitted.
    #[must_use]
    pub const fn has_ordinary_room(&self) -> bool {
        self.queued.saturating_sub(self.reserved_used) < self.ordinary_capacity()
    }
}

/// A capability staged for delivery, already installed in the receiver.
#[derive(Clone, Copy, Debug)]
pub struct DeliveredCap {
    /// Slot the entry was installed in.
    pub slot: u16,
    /// Handle the receiver will be told.
    pub handle: u64,
    /// Grant the entry refers to, so a withdrawal can drop its reference.
    pub grant: GrantId,
}

impl DeliveredCap {
    const fn empty() -> Self {
        Self {
            slot: u16::MAX,
            handle: 0,
            grant: NO_GRANT,
        }
    }
}

/// One admitted message.
#[derive(Clone, Copy)]
pub struct Message {
    /// Whether the slot is in use.
    pub used: bool,
    /// Next message in the endpoint's admission order.
    pub next: u16,
    /// Endpoint the message was admitted to.
    pub endpoint: u16,
    /// Generation of that endpoint at admission.
    pub endpoint_generation: u32,
    /// Invocation the message belongs to.
    pub invocation: u16,
    /// Whether the message occupies a reserved cell.
    pub reserved_cell: bool,
    /// Message kind: an ordinary request or a fault report.
    pub kind: u32,
    /// Payload bytes, copied once at admission.
    pub payload: [u8; MAX_INLINE_PAYLOAD as usize],
    /// Significant payload bytes.
    pub payload_len: u32,
    /// Capabilities already installed in the receiver.
    pub caps: [DeliveredCap; thalyx_abi::MAX_MESSAGE_CAPS],
    /// Number of installed capabilities.
    pub cap_count: u32,
    /// Queue bytes charged for this message.
    pub charged_bytes: u64,
}

impl Message {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            next: NO_MESSAGE,
            endpoint: 0,
            endpoint_generation: 0,
            invocation: 0,
            reserved_cell: false,
            kind: 0,
            payload: [0; MAX_INLINE_PAYLOAD as usize],
            payload_len: 0,
            caps: [DeliveredCap::empty(); thalyx_abi::MAX_MESSAGE_CAPS],
            cap_count: 0,
            charged_bytes: 0,
        }
    }
}

const _: () = assert!(MAX_MESSAGES < NO_MESSAGE as usize);

/// Lifecycle of admitted work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Free table slot.
    Empty,
    /// Admitted and queued; no receiver holds it yet.
    Admitted,
    /// A receiver holds it as an obligation.
    Delivered,
    /// Discharged: replied, resolved or withdrawn.
    Resolved,
}

impl State {
    /// Interface value reported in an invocation descriptor.
    #[must_use]
    pub const fn abi(self) -> u32 {
        match self {
            State::Empty => 0,
            State::Admitted => invocation_state::ADMITTED,
            State::Delivered => invocation_state::DELIVERED,
            State::Resolved => invocation_state::RESOLVED,
        }
    }

    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            State::Empty => "empty",
            State::Admitted => "admitted",
            State::Delivered => "delivered",
            State::Resolved => "resolved",
        }
    }
}

/// Whether an effect has been admitted against the original lineage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// No effect was ever requested.
    None,
    /// One effect was admitted and is not yet resolved.
    Admitted,
    /// An effect was requested after the barrier and refused.
    Refused,
    /// The admitted effect was resolved.
    Resolved,
}

impl Effect {
    /// Interface value reported in an invocation descriptor.
    #[must_use]
    pub const fn abi(self) -> u32 {
        match self {
            Effect::None => effect_state::NONE,
            Effect::Admitted => effect_state::ADMITTED,
            Effect::Refused => effect_state::REFUSED,
            Effect::Resolved => effect_state::RESOLVED,
        }
    }

    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Effect::None => "none",
            Effect::Admitted => "admitted",
            Effect::Refused => "refused",
            Effect::Resolved => "resolved",
        }
    }
}

/// Whether the authority that admitted the work is still live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cancel {
    /// The origin is still open.
    Live,
    /// The origin's scope or grant was fenced after admission.
    OriginFenced,
    /// The originating domain is gone.
    OriginDead,
}

impl Cancel {
    /// Interface value reported in a message header.
    #[must_use]
    pub const fn abi(self) -> u32 {
        match self {
            Cancel::Live => cancel_state::LIVE,
            Cancel::OriginFenced => cancel_state::ORIGIN_FENCED,
            Cancel::OriginDead => cancel_state::ORIGIN_DEAD,
        }
    }

    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Cancel::Live => "live",
            Cancel::OriginFenced => "origin_fenced",
            Cancel::OriginDead => "origin_dead",
        }
    }
}

/// Admitted work: who it came from, under what authority, and what it still
/// owes.
///
/// The origin fields are stamped by the kernel at admission and are not
/// writable from any payload. A server that trusts the header is trusting the
/// kernel; a server that trusts an identity inside the bytes is trusting the
/// client, which is the confused-deputy mistake the authority contract names.
#[derive(Clone, Copy, Debug)]
pub struct Invocation {
    /// Lifecycle state.
    pub state: State,
    /// Generation of this table slot.
    pub generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Endpoint the work was admitted to.
    pub endpoint: u16,
    /// Generation of that endpoint at admission.
    pub endpoint_generation: u32,
    /// Endpoint epoch at admission.
    pub epoch: u64,
    /// Domain that sent it.
    pub origin_domain: u16,
    /// Generation of that domain at admission.
    pub origin_domain_generation: u32,
    /// Diagnostic identity of that domain.
    pub origin_domain_id: u64,
    /// Scope the work is charged to.
    pub origin_scope: ScopeId,
    /// Diagnostic identity of that scope.
    pub origin_scope_id: u64,
    /// Thread blocked on the reply, if the call was synchronous.
    pub waiter: Option<usize>,
    /// Invocation the sender was serving when it sent this, or zero.
    pub parent_id: u64,
    /// Grant admission validated. Effect admission revalidates this same node,
    /// so presenting an equivalent capability later cannot revive the request.
    pub grant: GrantId,
    /// Facet the send capability carried.
    pub facet: u64,
    /// Whether the authority behind the work is still live.
    pub cancel: Cancel,
    /// Effect admission state.
    pub effect: Effect,
    /// Recovery time reserved when the effect was admitted.
    pub closure_reserved_ns: u64,
    /// Monotonic time of admission.
    pub admitted_ns: u64,
    /// Message holding the payload while it is queued.
    pub message: u16,
    /// Domain that received it, once one has.
    pub receiver_domain: u16,
    /// Queue bytes charged to the origin scope.
    pub charged_bytes: u64,
    /// Capability entries naming this invocation.
    pub refs: u32,
    /// Whether the reply has already been consumed.
    pub replied: bool,
    /// Result the server reported when resolving.
    pub outcome: u32,
    /// Reply payload, held until the waiter reads it.
    pub reply_payload: [u8; MAX_INLINE_PAYLOAD as usize],
    /// Significant reply bytes.
    pub reply_len: u32,
    /// Handles the reply installed in the caller.
    pub reply_caps: [u64; thalyx_abi::MAX_MESSAGE_CAPS],
    /// Number of those handles.
    pub reply_cap_count: u32,
    /// Value the server returned alongside the reply.
    pub reply_result: u64,
}

impl Invocation {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            state: State::Empty,
            generation: 0,
            id: 0,
            endpoint: 0,
            endpoint_generation: 0,
            epoch: 0,
            origin_domain: 0,
            origin_domain_generation: 0,
            origin_domain_id: 0,
            origin_scope: 0,
            origin_scope_id: 0,
            waiter: None,
            parent_id: 0,
            grant: NO_GRANT,
            facet: 0,
            cancel: Cancel::Live,
            effect: Effect::None,
            closure_reserved_ns: 0,
            admitted_ns: 0,
            message: NO_MESSAGE,
            receiver_domain: u16::MAX,
            charged_bytes: 0,
            refs: 0,
            replied: false,
            outcome: 0,
            reply_payload: [0; MAX_INLINE_PAYLOAD as usize],
            reply_len: 0,
            reply_caps: [0; thalyx_abi::MAX_MESSAGE_CAPS],
            reply_cap_count: 0,
            reply_result: 0,
        }
    }
}
