//! IPC: admission, delivery, reply, effect admission and resolution.
//!
//! Admission is one indivisible step. Everything the message will need — a
//! queue cell, the queue bytes charged to the sender's scope, an invocation
//! record, capability slots in the receiver and a control receipt — is reserved
//! before anything becomes visible, and a failure at any point leaves the
//! sender exactly as it was. A receiver never sees three of four capabilities.
//!
//! Capabilities are installed in the receiver **at admission**, which is what
//! makes "moving a capability invalidates the source only once the whole set is
//! installed in the receiver" true rather than aspirational. An unread delivery
//! therefore has a defined life: if the message is withdrawn, the entries are
//! withdrawn with it.
//!
//! The header the receiver reads is built here, not copied from the payload.
//! The sending domain, its scope, the grant that was validated, the facet and
//! the causal parent are the kernel's statements. A payload may carry any
//! identity it likes; it cannot become one.

use thalyx_abi::generated::{
    CallResult, EffectRequest, EndpointCreateRequest, EndpointInfo, FaultReport, InvocationInfo,
    MessageHeader, OpSpec, ReceiveResult, ReplyRequest, ResolveRequest, SendRequest, cap_op,
    message_kind, object_type, op, outcome as outcome_value, receipt_kind, right, status,
};

use crate::api::{BODY, Ctx, begin_response, receipt, reserve_receipt, resolve};
use crate::ipc::{Cancel, DeliveredCap, Effect, Endpoint, Invocation, Message, NO_MESSAGE, State};
use crate::obj::{NO_GRANT, ObjKind, ObjRef, ScopeId};
use crate::sched::WakeHint;
use crate::scope::{self, Resource};
use crate::state::{FaultRecord, MACHINE, Machine, Wait};
use crate::thread::{self, Handoff};
use crate::ucopy::Staging;
use crate::{event, trace};

/// Bytes charged per message on top of its payload: the record the kernel keeps
/// for it. Charging only the payload would let a flood of empty messages cost a
/// sender nothing while costing the kernel a table.
const MESSAGE_OVERHEAD_BYTES: u64 = 64;

/// Refuses an admission and says which of its five reservations ran out.
///
/// Admission reserves a queue cell, capability slots, a message record, an
/// invocation record, the sender's queue bytes and a receipt cell, and every
/// one of them answers `LIMIT_EXHAUSTED`. A caller that gets that answer knows
/// only that something bounded was full, and the five have nothing in common:
/// one is the receiver's fault, one the sender's, and three are the machine's.
/// Naming the table and what it held is what turns the refusal into evidence.
fn exhausted(resource: &str, used: u64, capacity: u64) -> i64 {
    trace!(
        "k2.admission_exhausted",
        "resource={resource} used={used} capacity={capacity}"
    );
    status::LIMIT_EXHAUSTED
}

/// Rights an invocation capability carries when a receiver takes the message.
const TICKET_RIGHTS: u32 = right::INSPECT
    | right::TRANSFER
    | right::DESTROY
    | right::INVOCATION_REPLY
    | right::INVOCATION_EFFECT
    | right::INVOCATION_RESOLVE
    | right::INVOCATION_BIND;

/// Creates an endpoint charged to the addressed scope.
pub fn create_endpoint(
    machine: &mut Machine,
    ctx: &Ctx,
    staging: &mut Staging,
) -> Result<u64, i64> {
    let request: EndpointCreateRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    if request.queue_capacity == 0
        || u64::from(request.queue_capacity) > thalyx_abi::limit::MAX_ENDPOINT_QUEUE
    {
        return Err(status::INVALID_ARGUMENT);
    }
    let sponsor = ctx.cap.object.index;
    if scope::table()[sponsor as usize].state() != scope::State::Open {
        return Err(status::SCOPE_CLOSED);
    }
    let index = machine
        .endpoint_ids
        .iter()
        .position(|endpoint| !endpoint.used)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    if !scope::reserve(sponsor, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    let Some(id) = machine.next_id() else {
        scope::release(sponsor, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    };
    let generation = machine.endpoint_ids[index].generation.saturating_add(1);
    machine.endpoint_ids[index].used = true;
    machine.endpoint_ids[index].generation = generation;
    machine.endpoint_ids[index]
        .refs
        .store(0, core::sync::atomic::Ordering::Relaxed);
    *machine.endpoints[index].get_mut() = Endpoint {
        open: true,
        id,
        epoch: id,
        owner_scope: sponsor,
        receiver_domain: u16::MAX,
        receiver_generation: 0,
        capacity: request.queue_capacity,
        reserved_cells: 0,
        reserved_used: 0,
        queued: 0,
        head: NO_MESSAGE,
        tail: NO_MESSAGE,
        facets: 0,
        next_facet: 1,
        admitted: 0,
        delivered: 0,
        label: request.label,
        receivers: 0,
    };

    let object = ObjRef::new(ObjKind::Endpoint, index as u16, generation);
    let owner = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        owner,
        NO_GRANT,
        object,
        ObjKind::Endpoint.rights_mask(),
        0,
        None,
        0,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    trace!(
        "ipc.endpoint_created",
        "endpoint={id} label={} epoch={id} capacity={} scope={}",
        machine.endpoints[index].get_mut().label_str(),
        request.queue_capacity,
        scope::table()[sponsor as usize].id()
    );
    Ok(handle)
}

/// Mints a send capability bound to one logical object of the service.
///
/// The facet is assigned by the kernel, never by the caller. A binder that
/// could choose the integer could reuse one, and a facet that can be reused
/// inside an epoch is not an identity a server can resolve against.
pub fn bind_facet(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: thalyx_abi::generated::BindFacetRequest = staging.read(BODY);
    if request.reserved0 != 0 || request.facet != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    if request.rights_mask & !ctx.cap.rights != 0 {
        return Err(status::INSUFFICIENT_RIGHTS);
    }
    if request.rights_mask & !ObjKind::Endpoint.rights_mask() != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let index = ctx.cap.object.index as usize;
    let parent = *machine.grants.nodes[ctx.cap.grant as usize].lock();
    let deadline = if request.deadline_ns == 0 {
        parent.deadline_ns
    } else {
        if parent.deadline_ns != 0 && request.deadline_ns > parent.deadline_ns {
            return Err(status::INVALID_ARGUMENT);
        }
        request.deadline_ns
    };
    let facet = machine.endpoints[index].get_mut().next_facet;
    if facet == u64::MAX {
        return Err(status::LIMIT_EXHAUSTED);
    }
    machine.endpoints[index].get_mut().next_facet += 1;
    machine.endpoints[index].get_mut().facets += 1;

    let sponsor = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        sponsor,
        ctx.cap.grant,
        ctx.cap.object,
        request.rights_mask,
        deadline,
        parent.life_scope,
        facet,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, ctx.cap.object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    let (endpoint_id, endpoint_epoch) = {
        let slot = machine.endpoints[index].get_mut();
        (slot.id, slot.epoch)
    };
    trace!(
        "ipc.facet_bound",
        "endpoint={} epoch={} facet={facet} grant={} rights=0x{:x} domain={}",
        endpoint_id,
        endpoint_epoch,
        machine.grants.nodes[grant as usize].lock().id,
        request.rights_mask,
        ctx.domain
    );
    Ok(handle)
}

struct Admitted {
    invocation: u16,
    generation: u32,
    id: u64,
}

/// The joint admission point: validate, reserve everything, then publish.
#[allow(clippy::too_many_arguments)]
fn admit(
    machine: &Machine,
    ctx: &Ctx,
    request: &SendRequest,
    kind: u32,
    waiter: Option<usize>,
) -> Result<Admitted, i64> {
    let endpoint = ctx.cap.object.index as usize;
    // One hold of the channel for the whole admission: the room in the queue,
    // the receiver it is admitted for, the cell it is linked into and the
    // receiver it may be handed straight to are one decision, and a second
    // sender must not find the room this one has taken. Endpoints are locked
    // one at a time, so admissions on different channels do not meet at all --
    // which is what K6's four independent pairs were waiting on.
    let mut channel = machine.endpoints[endpoint].lock();
    if !channel.open {
        return Err(status::PEER_DEAD);
    }
    let receiver = channel.receiver_domain;
    if receiver == u16::MAX {
        return Err(status::PEER_DEAD);
    }
    let receiver = receiver as usize;
    if machine.domains[receiver].generation != channel.receiver_generation
        || !matches!(
            machine.domains[receiver].state,
            crate::state::DomainState::Building | crate::state::DomainState::Runnable
        )
    {
        return Err(status::PEER_DEAD);
    }

    if u64::from(request.payload_len) > thalyx_abi::limit::MAX_INLINE_PAYLOAD {
        return Err(status::INVALID_ARGUMENT);
    }
    if u64::from(request.cap_count) > thalyx_abi::limit::MAX_CAPS_PER_MESSAGE {
        return Err(status::INVALID_ARGUMENT);
    }
    let count = request.cap_count as usize;
    for index in count..thalyx_abi::MAX_MESSAGE_CAPS {
        if request.caps[index] != 0 || request.cap_ops[index] != 0 {
            return Err(status::INVALID_ARGUMENT);
        }
    }

    // Resolve every capability first, refusing an ambiguous batch before
    // anything moves. Two moves of one handle would have to invalidate the same
    // entry twice; the interface refuses it rather than picking a meaning.
    let mut sources = [(0usize, ObjRef::new(ObjKind::Scope, 0, 0), NO_GRANT, false); 4];
    for index in 0..count {
        let operation = request.cap_ops[index];
        if operation != cap_op::COPY && operation != cap_op::MOVE {
            return Err(status::INVALID_ARGUMENT);
        }
        let cap = resolve(
            machine,
            ctx.domain,
            request.caps[index],
            thalyx_abi::generated::object_type::NONE,
            right::TRANSFER,
            ctx.now,
        )?;
        if sources
            .iter()
            .take(index)
            .any(|previous| previous.0 == cap.slot)
        {
            return Err(status::INVALID_ARGUMENT);
        }
        sources[index] = (cap.slot, cap.object, cap.grant, operation == cap_op::MOVE);
    }

    let ordinary = kind != message_kind::FAULT;
    if ordinary && !channel.has_ordinary_room() {
        return Err(status::QUEUE_FULL);
    }
    if !ordinary && channel.reserved_used >= channel.reserved_cells {
        return Err(status::QUEUE_FULL);
    }
    let free_slots = machine.domains[receiver].caps.lock().free_slots();
    if free_slots < count {
        return Err(exhausted(
            "receiver_cap_slots",
            count as u64,
            free_slots as u64,
        ));
    }

    // Claimed now, so the refusal comes before anything is reserved; given
    // back unwritten when the message is handed straight to a waiting
    // receiver.
    let Some(message_index) = machine.claim_message() else {
        return Err(exhausted(
            "messages",
            machine.messages_used(),
            machine.messages.len() as u64,
        ));
    };
    let Some(invocation_index) = machine.claim_invocation() else {
        machine.release_message(message_index);
        return Err(exhausted(
            "invocations",
            machine.invocations_used(),
            machine.invocations.len() as u64,
        ));
    };

    let origin_scope = thread::get(ctx.thread).effective_scope();
    let charged = if ordinary {
        let bytes = MESSAGE_OVERHEAD_BYTES + u64::from(request.payload_len);
        if !scope::reserve(origin_scope, Resource::QueueBytes, bytes) {
            machine.release_invocation(invocation_index);
            machine.release_message(message_index);
            let scope = &scope::table()[origin_scope as usize];
            return Err(exhausted(
                "queue_bytes",
                scope.used(Resource::QueueBytes).saturating_add(bytes),
                scope.limits.load().queue_bytes,
            ));
        }
        bytes
    } else {
        0
    };

    if !reserve_receipt(machine) {
        machine.release_invocation(invocation_index);
        machine.release_message(message_index);
        if charged != 0 {
            scope::release(origin_scope, Resource::QueueBytes, charged);
        }
        let (used, capacity) = match machine.system_log {
            Some(index) => {
                let log = machine.logs[index as usize].lock();
                (
                    (log.count as u64).saturating_add(u64::from(log.pending_reservations)),
                    log.ordinary_capacity() as u64,
                )
            }
            None => (0, 0),
        };
        return Err(exhausted("receipt_cells", used, capacity));
    }

    // Everything is reserved. From here the transfer either completes or is
    // rolled back to exactly this point.
    let mut installed = [DeliveredCap {
        slot: u16::MAX,
        handle: 0,
        grant: NO_GRANT,
    }; 4];
    let mut placed = 0usize;
    let mut failed = false;
    for index in 0..count {
        let (_, object, grant, _) = sources[index];
        match crate::api::cap_install(machine, receiver, object, grant, None) {
            Some(handle) => {
                installed[index] = DeliveredCap {
                    slot: thalyx_abi::handle_slot(handle) as u16,
                    handle,
                    grant,
                };
                placed += 1;
            }
            None => {
                failed = true;
                break;
            }
        }
    }
    if failed {
        for entry in installed.iter().take(placed) {
            crate::api::cap_release_slot(machine, receiver, entry.slot as usize);
        }
        crate::api::release_receipt(machine);
        machine.release_invocation(invocation_index);
        machine.release_message(message_index);
        if charged != 0 {
            scope::release(origin_scope, Resource::QueueBytes, charged);
        }
        return Err(status::LIMIT_EXHAUSTED);
    }

    // The whole set is installed, so consuming the moves is now correct.
    for index in 0..count {
        let (slot, _, _, moved) = sources[index];
        if moved {
            crate::api::cap_release_slot(machine, ctx.domain, slot);
        }
    }

    let Some(id) = machine.next_id() else {
        return Err(status::LIMIT_EXHAUSTED);
    };
    let generation = machine.invocations[invocation_index].lock().generation + 1;
    let parent_id = thread::get(ctx.thread).bound().map_or(0, |(index, _)| {
        machine.invocations[index as usize].lock().id
    });

    // Written into the slot field by field rather than assembled and copied.
    // Building the record on the stack and moving it is six hundred bytes of
    // memory traffic per admission, most of it a reply payload nothing has
    // written yet, and all of it under the control lock.
    let (endpoint_generation, epoch) = (machine.endpoint_ids[endpoint].generation, channel.epoch);
    let (origin_domain_generation, origin_domain_id) = {
        let slot = &machine.domains[ctx.domain];
        (slot.generation, slot.id)
    };
    {
        let slot = &mut *machine.invocations[invocation_index].lock();
        slot.state = State::Admitted;
        slot.generation = generation;
        slot.id = id;
        slot.endpoint = endpoint as u16;
        slot.endpoint_generation = endpoint_generation;
        slot.epoch = epoch;
        slot.origin_domain = ctx.domain as u16;
        slot.origin_domain_generation = origin_domain_generation;
        slot.origin_domain_id = origin_domain_id;
        slot.origin_scope = origin_scope;
        slot.origin_scope_id = scope::table()[origin_scope as usize].id();
        slot.waiter = waiter;
        slot.parent_id = parent_id;
        slot.grant = ctx.cap.grant;
        slot.facet = ctx.cap.facet;
        slot.cancel = Cancel::Live;
        slot.effect = Effect::None;
        slot.closure_reserved_ns = 0;
        slot.closure_service = 0;
        slot.admitted_ns = ctx.now;
        slot.message = message_index as u16;
        slot.receiver_domain = receiver as u16;
        slot.charged_bytes = charged;
        slot.refs = 0;
        slot.replied = false;
        slot.outcome = 0;
        // The reply payload is not cleared: `reply_len` says how much of it is
        // a reply, it is zero here, and the only writer of those bytes is a
        // reply that sets the length with them.
        slot.reply_len = 0;
        slot.reply_caps = [0; 4];
        slot.reply_cap_count = 0;
        slot.reply_result = 0;
    }
    machine.grants.nodes[ctx.cap.grant as usize].lock().refs += 1;
    channel.admitted += 1;
    scope::table()[origin_scope as usize]
        .invocations_pending
        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // A receiver waiting for exactly this message takes it now, without the
    // message ever being queued: the queue cell was found free above, so the
    // refusals are the ones a queued admission would have met, and what is
    // saved is writing three hundred and fifty bytes into the cell and
    // reading them straight back out. A receiver that cannot take it -- its
    // handle no longer resolves, its domain cannot hold the ticket -- is
    // woken empty-handed and the message is queued for it to find.
    //
    // A caller that will block for the reply leaves its processor free, and
    // that processor picks the receiver up on its way to idle; kicking another
    // one would turn a round trip on one processor into two interrupts across
    // two. A sender that keeps running has no free processor to offer.
    let hint = if waiter.is_none() {
        WakeHint::Any
    } else {
        WakeHint::Sync
    };
    let mut wake = None;
    let mut delivered = false;
    if let Some((thread, offer)) = take_receiver(machine, &mut channel, endpoint, ctx.now) {
        wake = Some(thread);
        // SAFETY: the wait was consumed by `take_receiver`, under the control
        // lock, and the wake is deferred to after this hold.
        if channel.head == NO_MESSAGE
            && let Some(staging) = unsafe { offer.staging() }
        {
            let receiver = channel.receiver_domain as usize;
            if let Ok(ticket) = issue_ticket(machine, receiver, invocation_index) {
                let record = {
                    let mut slot = machine.invocations[invocation_index].lock();
                    slot.state = State::Delivered;
                    slot.message = NO_MESSAGE;
                    *slot
                };
                channel.delivered += 1;
                {
                    lay_out_receive(
                        staging,
                        machine,
                        &record,
                        kind,
                        &request.payload[..request.payload_len as usize],
                        &installed[..count],
                    );
                }
                thread::set_wake_aux(thread, ticket);
                trace!(
                    "ipc.delivered",
                    "invocation={id} endpoint={} receiver_domain={} facet={} caps={} \
                     payload_len={} cancel=live ticket=0x{ticket:x} route=direct",
                    channel.id,
                    machine.domains[receiver].id,
                    ctx.cap.facet,
                    request.cap_count,
                    request.payload_len
                );
                delivered = true;
            }
        }
    }

    if delivered {
        machine.release_message(message_index);
    } else {
        let length = request.payload_len as usize;
        let slot = &mut *machine.messages[message_index].lock();
        slot.used = true;
        slot.next = NO_MESSAGE;
        slot.endpoint = endpoint as u16;
        slot.endpoint_generation = endpoint_generation;
        slot.invocation = invocation_index as u16;
        slot.reserved_cell = !ordinary;
        slot.kind = kind;
        slot.payload_len = request.payload_len;
        slot.payload[..length].copy_from_slice(&request.payload[..length]);
        // Everything past the significant bytes is cleared, because the whole
        // array crosses to the receiver and the slot has held another domain's
        // message before.
        slot.payload[length..].fill(0);
        slot.caps = installed;
        slot.cap_count = request.cap_count;
        slot.charged_bytes = charged;

        let tail = channel.tail;
        if tail == NO_MESSAGE {
            channel.head = message_index as u16;
        } else {
            machine.messages[tail as usize].lock().next = message_index as u16;
        }
        channel.tail = message_index as u16;
        channel.queued += 1;
        if !ordinary {
            channel.reserved_used += 1;
        }
    }

    let (endpoint_id, endpoint_epoch, queued) = (channel.id, channel.epoch, channel.queued);
    let grant_id = machine.grants.nodes[ctx.cap.grant as usize].lock().id;
    trace!(
        "ipc.admitted",
        "invocation={id} endpoint={endpoint_id} epoch={endpoint_epoch} facet={} grant={} \
         origin_domain={} origin_scope={} parent={parent_id} payload_len={} caps={} queued={queued} \
         charged_bytes={charged} kind={kind}",
        ctx.cap.facet,
        grant_id,
        machine.domains[ctx.domain].id,
        scope::table()[origin_scope as usize].id(),
        request.payload_len,
        request.cap_count
    );
    // The channel is released before the receipt and the wake: the log is not
    // this channel's and the woken thread has a processor to reach.
    drop(channel);
    receipt(
        machine,
        receipt_kind::ADMIT,
        ctx.domain,
        origin_scope,
        endpoint_id,
        grant_id,
        parent_id,
        status::OK,
        id,
        ctx.cap.facet,
        true,
    );
    if let Some(thread) = wake {
        crate::sched::defer_wake(thread, hint);
    }
    Ok(Admitted {
        invocation: invocation_index as u16,
        generation,
        id,
    })
}

/// Claims a thread waiting to receive on `endpoint`, for the admission to
/// deliver to.
///
/// Returns the thread and its offer, which is empty when the receiver cannot
/// take the message on the sender's behalf: it offered no buffer, or its
/// handle no longer resolves as the receiver itself would resolve it on
/// waking -- the same check, made for it. The caller wakes the thread either
/// way, once its hold of the control lock ends: the delivery is the one the
/// receiver would perform on waking, done in the hold the admission already
/// has, and what the receiver saves is a hold of its own -- one handover
/// fewer of a lock four processors queue for.
fn take_receiver(
    machine: &Machine,
    channel: &mut Endpoint,
    endpoint: usize,
    now: u64,
) -> Option<(usize, Handoff)> {
    let generation = machine.endpoint_ids[endpoint].generation;
    let mut waiting = channel.receivers;
    while waiting != 0 {
        let index = waiting.trailing_zeros() as usize;
        waiting &= waiting - 1;
        // Whether it was waiting here or not, the bit goes: a timeout, a
        // cancellation or a message it already took has moved it on, and a
        // wake moves it on now.
        channel.receivers &= !(1u64 << index);
        let Some(offer) = thread::claim_handoff(
            index,
            |record| record.wait == Wait::Receive(endpoint as u16, generation),
            status::OK,
            0,
        ) else {
            continue;
        };
        let receiver = channel.receiver_domain as usize;
        let resolved = resolve(
            machine,
            receiver,
            offer.handle,
            object_type::ENDPOINT,
            right::ENDPOINT_RECEIVE,
            now,
        );
        if resolved.is_ok_and(|cap| cap.object.index as usize == endpoint) {
            return Some((index, offer));
        }
        return Some((index, Handoff::NONE));
    }
    None
}

/// Issues `receiver` a ticket for the invocation: a grant on it, sponsored
/// by the receiver's scope, installed in the receiver's table. Nothing is
/// changed when either step fails.
fn issue_ticket(machine: &Machine, receiver: usize, invocation: usize) -> Result<u64, i64> {
    let (generation, facet) = {
        let record = machine.invocations[invocation].lock();
        (record.generation, record.facet)
    };
    let object = ObjRef::new(ObjKind::Invocation, invocation as u16, generation);
    let sponsor = machine.domains[receiver].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        sponsor,
        NO_GRANT,
        object,
        TICKET_RIGHTS,
        0,
        None,
        facet,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let Some(ticket) = crate::api::cap_install(machine, receiver, object, grant, None) else {
        crate::api::collect_grant(machine, grant);
        return Err(status::LIMIT_EXHAUSTED);
    };
    machine.invocations[invocation].lock().receiver_domain = receiver as u16;
    Ok(ticket)
}

/// Lays out the response a receiver reads: the header the kernel states
/// about the invocation, the capabilities already installed for it, and the
/// bytes.
///
/// Written field by field into the response, whose body `begin_response`
/// has just zeroed, so the payload's tail is the zeroes already there and
/// only the significant bytes move. Building the record whole -- a zeroed
/// payload of two hundred and fifty-six bytes, then the copy of the record
/// into the buffer -- was six hundred bytes of stores for every delivery,
/// most of them under the control lock.
fn lay_out_receive(
    staging: &mut Staging,
    machine: &Machine,
    record: &Invocation,
    kind: u32,
    payload: &[u8],
    caps: &[DeliveredCap],
) {
    let mut header = MessageHeader {
        epoch: record.epoch,
        invocation_id: record.id,
        sender_domain_id: record.origin_domain_id,
        sender_scope_id: record.origin_scope_id,
        parent_invocation_id: record.parent_id,
        facet: record.facet,
        grant_id: machine.grants.nodes[record.grant as usize].lock().id,
        sent_ns: record.admitted_ns,
        rights_transferred: 0,
        cancel_state: record.cancel.abi(),
        kind,
        payload_len: payload.len() as u32,
    };
    let mut handles = [0u64; 4];
    for (index, cap) in caps.iter().enumerate() {
        handles[index] = cap.handle;
        header.rights_transferred |= machine.grants.nodes[cap.grant as usize].lock().rights;
    }
    begin_response(staging, op::ENDPOINT_RECEIVE);
    staging.write(BODY + core::mem::offset_of!(ReceiveResult, header), header);
    staging.write(
        BODY + core::mem::offset_of!(ReceiveResult, cap_count),
        caps.len() as u32,
    );
    staging.write(BODY + core::mem::offset_of!(ReceiveResult, caps), handles);
    let at = BODY + core::mem::offset_of!(ReceiveResult, payload);
    staging.bytes[at..at + payload.len()].copy_from_slice(payload);
}

fn wake_waiter(machine: &Machine, invocation: usize, code: i64, hint: WakeHint) {
    let (waiter, generation) = {
        let slot = machine.invocations[invocation].lock();
        (slot.waiter, slot.generation)
    };
    let Some(waiter) = waiter else {
        return;
    };
    thread::defer_wake_if(
        waiter,
        |record| record.wait == Wait::Reply(invocation as u16, generation),
        code,
        0,
        hint,
    );
}

/// Admits a message without waiting for a reply.
pub fn send(machine: &Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: SendRequest = staging.read(BODY);
    let admitted = admit(machine, ctx, &request, message_kind::REQUEST, None)?;
    Ok(admitted.id)
}

/// Admits a message and waits for its one-shot reply.
pub fn call(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    if ctx.flags & thalyx_abi::generated::flag::NONBLOCKING != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let request: SendRequest = staging.read(BODY);

    let (invocation, generation) = {
        let machine = MACHINE.read();
        let cap = resolve(
            &machine,
            ctx.domain,
            ctx.handle,
            spec.object_type,
            spec.rights,
            ctx.now,
        )?;
        let full = Ctx { cap, ..*ctx };
        let admitted = admit(
            &machine,
            &full,
            &request,
            message_kind::REQUEST,
            Some(ctx.thread),
        )?;
        // The buffer the reply will be laid out in, offered to the replier
        // so it can lay it out itself.
        thread::prepare_wait_offering(
            ctx.thread,
            Wait::Reply(admitted.invocation, admitted.generation),
            ctx.deadline,
            thread::offer(staging, ctx.handle),
        );
        (admitted.invocation, admitted.generation)
    };

    crate::sched::block_current();

    let (woken, delivered) = thread::take_wake_status(ctx.thread);
    if woken == status::OK && delivered != 0 {
        // The replier laid the reply out here and released the invocation;
        // the value is its identifier. Nothing is left to lock for.
        return Ok(delivered);
    }
    let machine = MACHINE.read();
    let index = invocation as usize;
    // Read as fields rather than as a record, in one hold of the record's own
    // lock: an invocation is six hundred bytes, most of them the reply
    // payload, and copying the whole of it to look at four of them is done
    // with the record held against the replier that may be answering now.
    let (replied, state, outcome, id) = {
        let mut slot = machine.invocations[index].lock();
        if slot.generation != generation {
            return Err(status::PEER_DEAD);
        }
        if slot.replied {
            lay_out_reply(
                staging,
                &slot.reply_payload[..slot.reply_len as usize],
                &slot.reply_caps[..slot.reply_cap_count as usize],
                slot.reply_result,
                slot.id,
            );
            (true, slot.state, slot.outcome, slot.id)
        } else {
            if slot.state != State::Resolved {
                // The wait ended without a result. The invocation stays
                // admitted and its identifier stays recoverable: a timeout is
                // not a proof of absence.
                slot.waiter = None;
                return Err(if woken == status::OK {
                    status::TIMED_OUT
                } else {
                    woken
                });
            }
            (false, slot.state, slot.outcome, slot.id)
        }
    };
    if replied {
        release_invocation(&machine, index);
        return Ok(id);
    }
    debug_assert!(state == State::Resolved);
    let code = match outcome {
        outcome_value::COMMITTED => status::OK,
        outcome_value::UNKNOWN => status::PENDING,
        _ => status::CANCELLED,
    };
    release_invocation(&machine, index);
    if code == status::OK {
        begin_response(staging, ctx.operation);
        return Ok(id);
    }
    Err(code)
}

/// Takes the oldest admitted message with its authenticated header.
pub fn receive(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    loop {
        {
            let machine = MACHINE.read();
            let cap = resolve(
                &machine,
                ctx.domain,
                ctx.handle,
                spec.object_type,
                spec.rights,
                crate::api::now_ns(),
            )?;
            let endpoint = cap.object.index as usize;
            // One hold of the channel: the claim of the receive side, the
            // queue's head and the registration of the wait are one decision
            // against the senders racing it.
            let mut channel = machine.endpoints[endpoint].lock();

            // An endpoint has exactly one receiver. `install_cap` names it when
            // a supervisor hands the receive side to a domain it is building;
            // this is the same claim made by a domain acting for itself, which
            // is the only way a supervisor can receive on an endpoint it
            // created. It is a claim, not a transfer: an endpoint another live
            // domain already receives on is refused rather than stolen.
            let current = channel.receiver_domain;
            if current == u16::MAX {
                channel.receiver_domain = ctx.domain as u16;
                channel.receiver_generation = machine.domains[ctx.domain].generation;
                let endpoint_id = channel.id;
                trace!(
                    "ipc.receiver_claimed",
                    "endpoint={endpoint_id} domain={} name={} route=self_claim",
                    machine.domains[ctx.domain].id,
                    machine.domains[ctx.domain].name_str()
                );
            } else if current as usize != ctx.domain {
                return Err(status::STATE_CONFLICT);
            }

            let head = channel.head;
            if head != NO_MESSAGE {
                let delivered = deliver(
                    &machine,
                    &mut channel,
                    ctx.domain,
                    endpoint,
                    head as usize,
                    staging,
                );
                drop(channel);
                drop(machine);
                crate::sched::flush_wakes();
                return delivered;
            }
            if ctx.flags & thalyx_abi::generated::flag::NONBLOCKING != 0 {
                return Err(status::WOULD_BLOCK);
            }
            // The buffer the message will be laid out in, offered to the
            // sender so it can lay it out itself.
            thread::prepare_wait_offering(
                ctx.thread,
                Wait::Receive(endpoint as u16, machine.endpoint_ids[endpoint].generation),
                ctx.deadline,
                thread::offer(staging, ctx.handle),
            );
            // Registered under the same hold of the channel as the wait, so a
            // sender that takes the channel after this finds the bit.
            channel.receivers |= 1u64 << ctx.thread;
        }
        crate::sched::block_current();
        let (woken, ticket) = thread::take_wake_status(ctx.thread);
        if woken != status::OK {
            return Err(woken);
        }
        if ticket != 0 {
            // The sender delivered on its way: the message is laid out here
            // and the ticket is installed. Nothing is left to lock for.
            return Ok(ticket);
        }
    }
}

/// Takes the message at the head of `endpoint`'s queue for `receiver`,
/// laying it out in `staging` and returning the receiver's ticket.
fn deliver(
    machine: &Machine,
    channel: &mut Endpoint,
    receiver: usize,
    endpoint: usize,
    message_index: usize,
    staging: &mut Staging,
) -> Result<u64, i64> {
    // The header fields, not the record: a message is three hundred and fifty
    // bytes and an invocation six hundred, and the delivery needs a dozen
    // numbers out of them.
    let (invocation_index, message_next, message_reserved, message_kind) = {
        let slot = machine.messages[message_index].lock();
        (
            slot.invocation as usize,
            slot.next,
            slot.reserved_cell,
            slot.kind,
        )
    };

    // Admission recorded which endpoint the work arrived on. Delivering it from
    // a different one would mean the queue and the invocation had drifted
    // apart, and the header the receiver is about to be handed -- which it is
    // entitled to treat as the kernel's statement -- would be wrong about where
    // the work came from.
    {
        let slot = machine.invocations[invocation_index].lock();
        if slot.endpoint as usize != endpoint
            || slot.endpoint_generation != machine.endpoint_ids[endpoint].generation
        {
            return Err(status::STATE_CONFLICT);
        }
    }

    // The receiver needs a handle on the obligation before the message leaves
    // the queue: if the table is full the message stays where it was, which is
    // what "the message is kept if the delivery cannot be prepared" means.
    let ticket = issue_ticket(machine, receiver, invocation_index)?;

    // Dequeue only now that the delivery is certain.
    channel.head = message_next;
    if channel.head == NO_MESSAGE {
        channel.tail = NO_MESSAGE;
    }
    channel.queued -= 1;
    if message_reserved {
        channel.reserved_used = channel.reserved_used.saturating_sub(1);
    }
    channel.delivered += 1;

    let (record, invocation_id, invocation_facet, cancel) = {
        let mut invocation = machine.invocations[invocation_index].lock();
        invocation.state = State::Delivered;
        invocation.message = NO_MESSAGE;
        (
            *invocation,
            invocation.id,
            invocation.facet,
            invocation.cancel,
        )
    };
    let (cap_count, payload_len) = {
        let message = machine.messages[message_index].lock();
        let cap_count = message.cap_count as usize;
        let payload_len = message.payload_len as usize;
        lay_out_receive(
            staging,
            machine,
            &record,
            message_kind,
            &message.payload[..payload_len],
            &message.caps[..cap_count],
        );
        (cap_count, payload_len)
    };
    // The message's slot is free from here: everything it carried is in the
    // response. Freed by its flag and its link, not by rewriting the whole
    // record: an admission writes every field of a slot it takes, the payload
    // included, and the readers of the table skip a slot that is not in use.
    // Rewriting the three hundred and fifty bytes it holds is six lines of
    // stores under the control lock for nothing that is ever read.
    {
        let mut slot = machine.messages[message_index].lock();
        slot.used = false;
        slot.next = NO_MESSAGE;
        slot.cap_count = 0;
        slot.payload_len = 0;
    }
    machine.release_message(message_index);

    trace!(
        "ipc.delivered",
        "invocation={invocation_id} endpoint={} receiver_domain={} facet={invocation_facet} \
         caps={cap_count} payload_len={payload_len} cancel={} ticket=0x{ticket:x} route=queue",
        channel.id,
        machine.domains[receiver].id,
        cancel.name()
    );
    Ok(ticket)
}

/// What releasing an invocation has to know about it.
struct ReleaseFacts {
    state: State,
    charged_bytes: u64,
    origin_scope: ScopeId,
    effect: Effect,
    closure_service: ScopeId,
    closure_reserved_ns: u64,
    grant: crate::obj::GrantId,
}

/// Releases everything an invocation held once it is discharged.
fn release_invocation(machine: &Machine, index: usize) {
    // The seven fields this needs, not the six hundred bytes the record is --
    // and the fields it clears, cleared in the same hold. Reading and writing
    // them one at a time took the record's lock five times for one release,
    // which is five transfers of its line between the caller's processor and
    // the replier's.
    let invocation = {
        let mut slot = machine.invocations[index].lock();
        if slot.state == State::Empty {
            return;
        }
        let facts = ReleaseFacts {
            state: slot.state,
            charged_bytes: slot.charged_bytes,
            origin_scope: slot.origin_scope,
            effect: slot.effect,
            closure_service: slot.closure_service,
            closure_reserved_ns: slot.closure_reserved_ns,
            grant: slot.grant,
        };
        slot.charged_bytes = 0;
        if facts.effect == Effect::Admitted {
            slot.closure_reserved_ns = 0;
            slot.effect = Effect::Resolved;
        }
        slot.grant = NO_GRANT;
        slot.state = State::Resolved;
        facts
    };
    if invocation.charged_bytes != 0 {
        scope::release(
            invocation.origin_scope,
            Resource::QueueBytes,
            invocation.charged_bytes,
        );
    }
    let origin = &scope::table()[invocation.origin_scope as usize];
    scope::decrement(&origin.invocations_pending);
    if invocation.effect == Effect::Admitted {
        scope::decrement(&origin.effects_pending);
        // The closing capacity this effect was holding goes back to the service
        // that reserved it. Holding a reservation past the obligation it was
        // taken for would shrink a service's reserve a little with every
        // request it ever answered.
        let service = &scope::table()[invocation.closure_service as usize];
        scope::subtract(&service.closure_reserved_ns, invocation.closure_reserved_ns);
    }
    scope::note_progress(invocation.origin_scope, crate::api::now_ns());
    if invocation.grant != NO_GRANT {
        {
            let mut node = machine.grants.nodes[invocation.grant as usize].lock();
            node.refs = node.refs.saturating_sub(1);
        }
        crate::api::collect_grant(machine, invocation.grant);
    }
    crate::api::collect_invocation(machine, index);
}

/// Consumes the one-shot reply of an invocation.
pub fn reply(machine: &Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: ReplyRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    if u64::from(request.payload_len) > thalyx_abi::limit::MAX_INLINE_PAYLOAD
        || u64::from(request.cap_count) > thalyx_abi::limit::MAX_CAPS_PER_MESSAGE
    {
        return Err(status::INVALID_ARGUMENT);
    }
    let (origin_domain, origin_generation, invocation_id) = {
        let slot = machine.invocations[index].lock();
        if slot.state == State::Resolved || slot.replied {
            return Err(status::ALREADY_RESOLVED);
        }
        if slot.waiter.is_none() {
            return Err(status::STATE_CONFLICT);
        }
        (slot.origin_domain, slot.origin_domain_generation, slot.id)
    };

    let caller = origin_domain as usize;
    if machine.domains[caller].generation != origin_generation {
        return Err(status::PEER_DEAD);
    }
    let count = request.cap_count as usize;
    let mut installed = [0u64; 4];
    let mut slots = [u16::MAX; 4];
    let mut placed = 0usize;
    for i in 0..count {
        if request.cap_ops[i] != cap_op::COPY && request.cap_ops[i] != cap_op::MOVE {
            return Err(status::INVALID_ARGUMENT);
        }
        let cap = match resolve(
            machine,
            ctx.domain,
            request.caps[i],
            thalyx_abi::generated::object_type::NONE,
            right::TRANSFER,
            ctx.now,
        ) {
            Ok(cap) => cap,
            Err(code) => {
                for entry in slots.iter().take(placed) {
                    crate::api::cap_release_slot(machine, caller, *entry as usize);
                }
                return Err(code);
            }
        };
        match crate::api::cap_install(machine, caller, cap.object, cap.grant, None) {
            Some(handle) => {
                installed[i] = handle;
                slots[i] = thalyx_abi::handle_slot(handle) as u16;
                placed += 1;
            }
            None => {
                for entry in slots.iter().take(placed) {
                    crate::api::cap_release_slot(machine, caller, *entry as usize);
                }
                return Err(status::LIMIT_EXHAUSTED);
            }
        }
        if request.cap_ops[i] == cap_op::MOVE {
            crate::api::cap_release_slot(machine, ctx.domain, cap.slot);
        }
    }

    // The one-shot is consumed and the caller answered in one hold of the
    // record. Either the caller's offer is claimed here -- and the reply laid
    // out in the buffer it left, which is the work the caller's return would
    // otherwise do in a hold of its own -- or the reply is written onto the
    // record before the hold ends. A caller coming back through a timeout
    // takes the same lock, so it finds either no reply at all or the whole of
    // one, and a second replier on the same ticket loses the race here.
    let answered = {
        let mut slot = machine.invocations[index].lock();
        if slot.state == State::Resolved || slot.replied {
            drop(slot);
            for entry in slots.iter().take(placed) {
                crate::api::cap_release_slot(machine, caller, *entry as usize);
            }
            return Err(status::ALREADY_RESOLVED);
        }
        slot.replied = true;
        slot.outcome = outcome_value::COMMITTED;
        let generation = slot.generation;
        let taken = slot.waiter.and_then(|waiter| {
            thread::claim_handoff(
                waiter,
                |record| record.wait == Wait::Reply(index as u16, generation),
                status::OK,
                0,
            )
            .map(|offer| (waiter, offer))
        });
        match taken {
            // SAFETY: the wait was consumed just above, under the record's own
            // lock, and the wake is deferred to after this hold.
            Some((waiter, offer)) => match unsafe { offer.staging() } {
                Some(buffer) => {
                    lay_out_reply(
                        buffer,
                        &request.payload[..request.payload_len as usize],
                        &installed[..count],
                        request.result,
                        invocation_id,
                    );
                    Some((waiter, true))
                }
                None => {
                    store_reply(&mut slot, &request, installed);
                    Some((waiter, false))
                }
            },
            // Not waiting any more -- timed out, or cancelled -- or waiting
            // without an offer. The reply is kept on the invocation, where a
            // return that still finds it, or a query, reads it.
            None => {
                store_reply(&mut slot, &request, installed);
                None
            }
        }
    };

    trace!(
        "ipc.replied",
        "invocation={} responder_domain={} payload_len={} caps={} result=0x{:x}",
        invocation_id,
        machine.domains[ctx.domain].id,
        request.payload_len,
        request.cap_count,
        request.result
    );

    if let Some((waiter, laid_out)) = answered {
        if laid_out {
            release_invocation(machine, index);
            thread::set_wake_aux(waiter, invocation_id);
        }
        crate::sched::defer_wake(waiter, WakeHint::Sync);
    }
    Ok(invocation_id)
}

/// Lays out the reply a caller reads, as [`lay_out_receive`] does a message:
/// the significant bytes over a body already zeroed.
fn lay_out_reply(staging: &mut Staging, payload: &[u8], caps: &[u64], result: u64, id: u64) {
    let mut handles = [0u64; 4];
    handles[..caps.len()].copy_from_slice(caps);
    begin_response(staging, op::ENDPOINT_CALL);
    staging.write(
        BODY + core::mem::offset_of!(CallResult, payload_len),
        payload.len() as u32,
    );
    staging.write(
        BODY + core::mem::offset_of!(CallResult, cap_count),
        caps.len() as u32,
    );
    staging.write(BODY + core::mem::offset_of!(CallResult, caps), handles);
    staging.write(BODY + core::mem::offset_of!(CallResult, result), result);
    staging.write(BODY + core::mem::offset_of!(CallResult, invocation_id), id);
    let at = BODY + core::mem::offset_of!(CallResult, payload);
    staging.bytes[at..at + payload.len()].copy_from_slice(payload);
}

/// Keeps a reply on its invocation for a caller that will read it from there.
fn store_reply(slot: &mut Invocation, request: &ReplyRequest, installed: [u64; 4]) {
    slot.reply_payload = request.payload;
    slot.reply_len = request.payload_len;
    slot.reply_caps = installed;
    slot.reply_cap_count = request.cap_count;
    slot.reply_result = request.result;
}

/// Admits one effect against the lineage that admitted the request.
///
/// This is the second race the architecture separates from the first. Having
/// been admitted is not permission to act: the same grant, the same scopes and
/// the same clock are checked again, under the same lock a barrier takes, and
/// the mark is consumed once. Presenting an equivalent capability afterwards
/// cannot revive a request whose authority was closed.
pub fn begin_effect(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: EffectRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let index = ctx.cap.object.index as usize;
    let invocation = *machine.invocations[index].get_mut();
    if invocation.state == State::Resolved {
        return Err(status::ALREADY_RESOLVED);
    }
    if invocation.effect != Effect::None {
        return Err(status::ALREADY_RESOLVED);
    }

    let lineage = crate::api::lineage_status(machine, invocation.grant, ctx.now);
    let scope_open = scope::is_open(invocation.origin_scope);
    if lineage != status::OK || !scope_open {
        machine.invocations[index].get_mut().effect = Effect::Refused;
        let code = if lineage != status::OK {
            lineage
        } else {
            status::SCOPE_CLOSED
        };
        trace!(
            "effect.refused",
            "invocation={} origin_scope={} grant={} status={code} reason=barrier_or_expiry",
            invocation.id,
            scope::table()[invocation.origin_scope as usize].id(),
            machine.grants.nodes[invocation.grant as usize].lock().id
        );
        let grant_id = machine.grants.nodes[invocation.grant as usize].lock().id;
        receipt(
            machine,
            receipt_kind::EFFECT,
            ctx.domain,
            invocation.origin_scope,
            invocation.id,
            grant_id,
            invocation.parent_id,
            code,
            0,
            0,
            false,
        );
        return Err(code);
    }

    // Closure capacity is reserved before the effect is permitted, in the
    // server's own scope: finishing an obligation must not depend on the budget
    // of the client that is being closed.
    let service = machine.domains[ctx.domain].owner_scope;
    let reserve = request.closure_reserve_ns;
    let node = &scope::table()[service as usize];
    let limits = node.limits.closure_reserve_ns();
    // Spent, plus what other effects are still holding, plus this one. Checking
    // only what has been spent would let every outstanding effect pass the same
    // test and the reserve would be an advance reservation in name only.
    let used = node
        .closure_used_ns
        .load(core::sync::atomic::Ordering::Relaxed);
    let held = node
        .closure_reserved_ns
        .load(core::sync::atomic::Ordering::Relaxed);
    if used + held + reserve > limits {
        return Err(status::LIMIT_EXHAUSTED);
    }

    // An effect admission is a covered event: the audited profile says a
    // receipt exists for every one of them, and that is only true if the cell
    // is bought before the effect is permitted. Without this the receipt was
    // written afterwards and could be lost -- an effect admitted with nobody
    // able to account for it, which is the one thing the profile claims
    // cannot happen. A service told `LIMIT_EXHAUSTED` here has not begun
    // anything, so there is nothing to reconcile.
    if !crate::api::reserve_receipt(machine) {
        let (used, capacity) = match machine.system_log {
            Some(log) => {
                let log = &machine.logs[log as usize].get_mut();
                (
                    (log.count as u64).saturating_add(u64::from(log.pending_reservations)),
                    log.ordinary_capacity() as u64,
                )
            }
            None => (0, 0),
        };
        return Err(exhausted("receipt_cells", used, capacity));
    }

    node.closure_reserved_ns
        .fetch_add(reserve, core::sync::atomic::Ordering::Relaxed);
    machine.invocations[index].get_mut().effect = Effect::Admitted;
    machine.invocations[index].get_mut().closure_service = service;
    machine.invocations[index].get_mut().closure_reserved_ns = reserve;
    scope::table()[invocation.origin_scope as usize]
        .effects_pending
        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    trace!(
        "effect.admitted",
        "invocation={} origin_scope={} service_scope={} grant={} closure_reserve_ns={reserve} \
         kind={}",
        invocation.id,
        scope::table()[invocation.origin_scope as usize].id(),
        scope::table()[service as usize].id(),
        machine.grants.nodes[invocation.grant as usize].lock().id,
        request.effect_kind
    );
    let grant_id = machine.grants.nodes[invocation.grant as usize].lock().id;
    receipt(
        machine,
        receipt_kind::EFFECT,
        ctx.domain,
        invocation.origin_scope,
        invocation.id,
        grant_id,
        invocation.parent_id,
        status::OK,
        u64::from(request.effect_kind),
        reserve,
        true,
    );
    Ok(invocation.id)
}

/// Discharges a retained obligation with an explicit outcome.
pub fn resolve_invocation(
    machine: &mut Machine,
    ctx: &Ctx,
    staging: &mut Staging,
) -> Result<u64, i64> {
    let request: ResolveRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    if !matches!(
        request.outcome,
        outcome_value::COMMITTED | outcome_value::ABORTED | outcome_value::UNKNOWN
    ) {
        return Err(status::INVALID_ARGUMENT);
    }
    let index = ctx.cap.object.index as usize;
    let invocation = *machine.invocations[index].get_mut();
    if invocation.state == State::Resolved {
        return Err(status::ALREADY_RESOLVED);
    }
    machine.invocations[index].get_mut().outcome = request.outcome;
    let code = match request.outcome {
        outcome_value::COMMITTED => status::OK,
        outcome_value::UNKNOWN => status::PENDING,
        _ => status::CANCELLED,
    };
    trace!(
        "ipc.resolved",
        "invocation={} responder_domain={} outcome={} from_state={} effect={} \
         cancel={} detail=0x{:x} origin_scope={}",
        invocation.id,
        machine.domains[ctx.domain].id,
        request.outcome,
        invocation.state.name(),
        invocation.effect.name(),
        invocation.cancel.name(),
        request.detail,
        scope::table()[invocation.origin_scope as usize].id()
    );
    wake_waiter(machine, index, code, WakeHint::Sync);
    release_invocation(machine, index);
    Ok(invocation.id)
}

/// Reports origin, cancellation and effect state.
pub fn query_invocation(
    machine: &mut Machine,
    ctx: &Ctx,
    staging: &mut Staging,
) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let invocation = *machine.invocations[index].get_mut();
    let info = InvocationInfo {
        state: invocation.state.abi(),
        cancel_state: invocation.cancel.abi(),
        effect_state: invocation.effect.abi(),
        reserved0: 0,
        invocation_id: invocation.id,
        origin_domain_id: invocation.origin_domain_id,
        origin_scope_id: invocation.origin_scope_id,
        facet: invocation.facet,
        grant_id: if invocation.grant == NO_GRANT {
            0
        } else {
            machine.grants.nodes[invocation.grant as usize].lock().id
        },
        admitted_ns: invocation.admitted_ns,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(u64::from(info.cancel_state))
}

/// Reports queue occupancy and epoch.
pub fn query_endpoint(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let endpoint = &machine.endpoints[index].get_mut();
    let info = EndpointInfo {
        queue_capacity: endpoint.capacity,
        queued: endpoint.queued,
        reserved_cells: endpoint.reserved_cells,
        facets: endpoint.facets,
        epoch: endpoint.epoch,
        object_id: endpoint.id,
        admitted: endpoint.admitted,
        delivered: endpoint.delivered,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(u64::from(info.queued))
}

/// Charges this worker's execution to the invocation's originating scope.
///
/// The client does not choose who pays: the scope comes from the invocation the
/// kernel stamped, and the worker must hold a capability on it. When that scope
/// is already fenced the binding is still allowed, because the obligation still
/// has to be finished, but the time is charged to the service's closure reserve
/// and appears as recovery spending.
pub fn bind_worker(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let invocation = *machine.invocations[index].get_mut();
    if invocation.state == State::Resolved {
        return Err(status::ALREADY_RESOLVED);
    }
    let thread = ctx.thread;
    let cell = thread::get(thread);
    if cell.bound().is_some() {
        return Err(status::STATE_CONFLICT);
    }
    let recovery = !scope::is_open(invocation.origin_scope);

    // Who pays. Ordinarily the origin does: the client asked for the work, so
    // the worker adopts the client's scope and competes against its budget
    // instead of adding a second one.
    //
    // Recovery is the exception the authority contract grants, and it names the
    // account: the service reserved closing capacity **in its own scope** in
    // advance, and that is where the cost of finishing goes. Charging it to the
    // closed client would be charging a budget nobody is allowed to spend any
    // more -- and, since a closed scope's closure reserve may legitimately be
    // zero, it would leave the obligation unfinishable rather than paid for.
    // The exception does not return authority to the client and does not change
    // whose invocation this is; the origin stays on the record.
    let charged = if recovery {
        machine.domains[ctx.domain].owner_scope
    } else {
        invocation.origin_scope
    };

    // A borrowed worker competes for the paying scope's parallelism slots like
    // any thread that scope owns.
    if !recovery && !scope::has_parallelism(charged) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    if let Some(previous) = cell.take_parallelism_scope() {
        scope::drop_parallelism(previous);
    }
    cell.effective_scope
        .store(charged, core::sync::atomic::Ordering::Relaxed);
    cell.recovery
        .store(recovery, core::sync::atomic::Ordering::Relaxed);
    cell.set_bound(Some((index as u16, invocation.generation)));
    scope::take_parallelism(charged);
    cell.set_parallelism_scope(Some(charged));
    machine.invocations[index].get_mut().refs += 1;
    trace!(
        "sched.bound",
        "thread={thread} domain={} invocation={} effective_scope={} origin_scope={} \
         account={} recovery={}",
        machine.domains[ctx.domain].id,
        invocation.id,
        scope::table()[charged as usize].id(),
        scope::table()[invocation.origin_scope as usize].id(),
        if recovery {
            "closure_reserve"
        } else {
            "origin_budget"
        },
        u8::from(recovery)
    );
    Ok(invocation.id)
}

/// Returns the worker to the scope that owns its domain.
pub fn unbind_worker(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let cell = thread::get(ctx.thread);
    let Some((index, generation)) = cell.bound() else {
        return Err(status::STATE_CONFLICT);
    };
    cell.set_bound(None);
    if machine.invocations[index as usize].get_mut().generation == generation {
        machine.invocations[index as usize].get_mut().refs = machine.invocations[index as usize]
            .get_mut()
            .refs
            .saturating_sub(1);
    }
    if let Some(previous) = cell.take_parallelism_scope() {
        scope::drop_parallelism(previous);
    }
    let owner = cell.control().owner_scope;
    cell.effective_scope
        .store(owner, core::sync::atomic::Ordering::Relaxed);
    cell.recovery
        .store(false, core::sync::atomic::Ordering::Relaxed);
    scope::take_parallelism(owner);
    cell.set_parallelism_scope(Some(owner));
    Ok(0)
}

/// Withdraws every message admitted under a fenced scope that nobody has
/// received.
///
/// A message still in a queue is an obligation of the sender's perimeter; one
/// that a receiver already took is an obligation of the receiver. Only the
/// first can be withdrawn, and the count is reported rather than folded into
/// success.
pub fn withdraw_undelivered(machine: &mut Machine, root: ScopeId, _now: u64) -> u32 {
    let mut withdrawn = 0;
    for endpoint in 0..machine.endpoints.len() {
        if !machine.endpoint_ids[endpoint].used {
            continue;
        }
        let mut previous = NO_MESSAGE;
        let mut current = machine.endpoints[endpoint].get_mut().head;
        while current != NO_MESSAGE {
            let message = *machine.messages[current as usize].get_mut();
            let invocation = message.invocation as usize;
            let origin = machine.invocations[invocation].get_mut().origin_scope;
            let next = message.next;
            if machine.invocations[invocation].get_mut().state == State::Admitted
                && scope::is_within(root, origin)
            {
                if previous == NO_MESSAGE {
                    machine.endpoints[endpoint].get_mut().head = next;
                } else {
                    machine.messages[previous as usize].get_mut().next = next;
                }
                if machine.endpoints[endpoint].get_mut().tail == current {
                    machine.endpoints[endpoint].get_mut().tail = previous;
                }
                machine.endpoints[endpoint].get_mut().queued = machine.endpoints[endpoint]
                    .get_mut()
                    .queued
                    .saturating_sub(1);
                if message.reserved_cell {
                    machine.endpoints[endpoint].get_mut().reserved_used = machine.endpoints
                        [endpoint]
                        .get_mut()
                        .reserved_used
                        .saturating_sub(1);
                }
                for entry in message.caps.iter().take(message.cap_count as usize) {
                    let receiver = machine.endpoints[endpoint].get_mut().receiver_domain as usize;
                    crate::api::cap_release_slot(machine, receiver, entry.slot as usize);
                }
                *machine.messages[current as usize].get_mut() = Message::empty();
                machine.release_message(current as usize);
                machine.invocations[invocation].get_mut().cancel = Cancel::OriginFenced;
                machine.invocations[invocation].get_mut().outcome = outcome_value::ABORTED;
                wake_waiter(machine, invocation, status::CANCELLED, WakeHint::Any);
                release_invocation(machine, invocation);
                scope::table()[origin as usize]
                    .undelivered_cancelled
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                withdrawn += 1;
            } else {
                previous = current;
            }
            current = next;
        }
    }
    withdrawn
}

/// Marks the invocations of a fenced perimeter so a receiver can see it.
pub fn mark_cancelled(machine: &mut Machine, root: ScopeId) {
    for index in 0..machine.invocations.len() {
        let invocation = *machine.invocations[index].get_mut();
        if invocation.state == State::Empty || invocation.state == State::Resolved {
            continue;
        }
        if scope::is_within(root, invocation.origin_scope) {
            machine.invocations[index].get_mut().cancel = Cancel::OriginFenced;
        }
    }
}

/// Cleans up after a domain died.
pub fn on_domain_death(machine: &mut Machine, domain: usize) {
    let generation = machine.domains[domain].generation;

    // Messages the dead domain sent and nobody received are cancelled; the ones
    // a receiver already took stay its obligation, marked so it can see the
    // origin is gone.
    for endpoint in 0..machine.endpoints.len() {
        if !machine.endpoint_ids[endpoint].used {
            continue;
        }
        let mut previous = NO_MESSAGE;
        let mut current = machine.endpoints[endpoint].get_mut().head;
        while current != NO_MESSAGE {
            let message = *machine.messages[current as usize].get_mut();
            let invocation = message.invocation as usize;
            let next = message.next;
            if machine.invocations[invocation].get_mut().origin_domain as usize == domain
                && machine.invocations[invocation]
                    .get_mut()
                    .origin_domain_generation
                    == generation
            {
                if previous == NO_MESSAGE {
                    machine.endpoints[endpoint].get_mut().head = next;
                } else {
                    machine.messages[previous as usize].get_mut().next = next;
                }
                if machine.endpoints[endpoint].get_mut().tail == current {
                    machine.endpoints[endpoint].get_mut().tail = previous;
                }
                machine.endpoints[endpoint].get_mut().queued = machine.endpoints[endpoint]
                    .get_mut()
                    .queued
                    .saturating_sub(1);
                if message.reserved_cell {
                    machine.endpoints[endpoint].get_mut().reserved_used = machine.endpoints
                        [endpoint]
                        .get_mut()
                        .reserved_used
                        .saturating_sub(1);
                }
                for entry in message.caps.iter().take(message.cap_count as usize) {
                    let receiver = machine.endpoints[endpoint].get_mut().receiver_domain as usize;
                    crate::api::cap_release_slot(machine, receiver, entry.slot as usize);
                }
                *machine.messages[current as usize].get_mut() = Message::empty();
                machine.release_message(current as usize);
                machine.invocations[invocation].get_mut().cancel = Cancel::OriginDead;
                machine.invocations[invocation].get_mut().outcome = outcome_value::ABORTED;
                release_invocation(machine, invocation);
            } else {
                previous = current;
            }
            current = next;
        }
    }

    for index in 0..machine.invocations.len() {
        let invocation = *machine.invocations[index].get_mut();
        if invocation.state == State::Empty || invocation.state == State::Resolved {
            continue;
        }
        if invocation.origin_domain as usize == domain
            && invocation.origin_domain_generation == generation
        {
            machine.invocations[index].get_mut().cancel = Cancel::OriginDead;
            machine.invocations[index].get_mut().waiter = None;
        }
        // A receiver that died owes an answer it can no longer give. The result
        // is unknown, not aborted: the kernel never invents a rollback.
        if invocation.state == State::Delivered && invocation.receiver_domain as usize == domain {
            machine.invocations[index].get_mut().outcome = outcome_value::UNKNOWN;
            wake_waiter(machine, index, status::PEER_DEAD, WakeHint::Any);
            release_invocation(machine, index);
        }
    }

    // An endpoint whose receiver is gone stops admitting. Clients detect the
    // dead peer and reconnect through new capabilities; nothing resurrects.
    for endpoint in 0..machine.endpoints.len() {
        if machine.endpoint_ids[endpoint].used
            && machine.endpoints[endpoint].get_mut().receiver_domain as usize == domain
            && machine.endpoints[endpoint].get_mut().receiver_generation == generation
        {
            machine.endpoints[endpoint].get_mut().open = false;
        }
    }

    if let Some(object) = machine.domains[domain].fault_endpoint.take() {
        let index = object.index as usize;
        if machine.endpoint_ids[index].used
            && machine.endpoint_ids[index].generation == object.generation
        {
            machine.endpoints[index].get_mut().reserved_cells = machine.endpoints[index]
                .get_mut()
                .reserved_cells
                .saturating_sub(1);
        }
        let grant = machine.domains[domain].fault_grant;
        if grant != NO_GRANT {
            {
                let mut node = machine.grants.nodes[grant as usize].lock();
                node.refs = node.refs.saturating_sub(1);
            }
        }
        machine.domains[domain].fault_grant = NO_GRANT;
        machine.domains[domain].fault_reserved = false;
    }
}

/// Reports a fault to the supervisor channel installed before activation.
///
/// The report is bounded, carries no kernel address, and uses the cell reserved
/// when the channel was installed. A supervisor whose queue is full of ordinary
/// traffic still hears that one of its domains died.
pub fn deliver_fault(
    machine: &mut Machine,
    domain: usize,
    thread: usize,
    record: &FaultRecord,
) -> bool {
    let Some(object) = machine.domains[domain].fault_endpoint else {
        return false;
    };
    if !crate::api::object_alive(machine, object) {
        return false;
    }
    let grant = machine.domains[domain].fault_grant;
    let facet = machine.domains[domain].fault_facet;
    let report = FaultReport {
        domain_id: machine.domains[domain].id,
        thread_id: thread::get(thread).control().id,
        generation: u64::from(machine.domains[domain].generation),
        vector: record.vector,
        error_code: record.error_code,
        rip: record.rip,
        rsp: record.rsp,
        address: record.cr2,
        class: 1,
        reserved0: 0,
    };
    let mut request = SendRequest {
        payload_len: core::mem::size_of::<FaultReport>() as u32,
        cap_count: 0,
        caps: [0; 4],
        cap_ops: [0; 4],
        payload: [0; 256],
    };
    // SAFETY: the report is a generated integer structure and the destination
    // is a byte array at least as long as it, inside the kernel's own staging
    // copy of the message.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (&raw const report).cast::<u8>(),
            request.payload.as_mut_ptr(),
            core::mem::size_of::<FaultReport>(),
        );
    }
    let ctx = Ctx {
        domain,
        thread,
        handle: 0,
        operation: thalyx_abi::generated::op::ENDPOINT_SEND,
        flags: 0,
        deadline: 0,
        now: crate::api::now_ns(),
        cap: crate::api::Resolved {
            slot: 0,
            object,
            grant,
            rights: right::ENDPOINT_SEND,
            facet,
        },
    };
    match admit(machine, &ctx, &request, message_kind::FAULT, None) {
        Ok(admitted) => {
            event!(
                "fault.reported",
                "domain={} endpoint={} invocation={} vector={} rip=0x{:x} channel=reserved",
                machine.domains[domain].id,
                machine.endpoints[object.index as usize].get_mut().id,
                admitted.id,
                record.vector,
                record.rip
            );
            true
        }
        Err(code) => {
            event!(
                "fault.report_failed",
                "domain={} status={code} reason=channel_unavailable",
                machine.domains[domain].id
            );
            false
        }
    }
}
