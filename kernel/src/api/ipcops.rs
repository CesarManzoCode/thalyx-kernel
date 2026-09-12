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
    message_kind, outcome as outcome_value, receipt_kind, right, status,
};

use crate::api::{BODY, Ctx, begin_response, receipt, reserve_receipt, resolve};
use crate::ipc::{Cancel, DeliveredCap, Effect, Endpoint, Invocation, Message, NO_MESSAGE, State};
use crate::obj::{NO_GRANT, ObjKind, ObjRef, ScopeId};
use crate::sched::WakeHint;
use crate::scope::{self, Resource};
use crate::state::{FaultRecord, MACHINE, Machine, Wait};
use crate::thread;
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
        .endpoints
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
    let generation = machine.endpoints[index].generation.saturating_add(1);
    machine.endpoints[index] = Endpoint {
        used: true,
        open: true,
        generation,
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
        refs: 0,
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
        machine.endpoints[index].label_str(),
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
    let parent = machine.grants[ctx.cap.grant as usize];
    let deadline = if request.deadline_ns == 0 {
        parent.deadline_ns
    } else {
        if parent.deadline_ns != 0 && request.deadline_ns > parent.deadline_ns {
            return Err(status::INVALID_ARGUMENT);
        }
        request.deadline_ns
    };
    let facet = machine.endpoints[index].next_facet;
    if facet == u64::MAX {
        return Err(status::LIMIT_EXHAUSTED);
    }
    machine.endpoints[index].next_facet += 1;
    machine.endpoints[index].facets += 1;

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
    trace!(
        "ipc.facet_bound",
        "endpoint={} epoch={} facet={facet} grant={} rights=0x{:x} domain={}",
        machine.endpoints[index].id,
        machine.endpoints[index].epoch,
        machine.grants[grant as usize].id,
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
    machine: &mut Machine,
    ctx: &Ctx,
    request: &SendRequest,
    kind: u32,
    waiter: Option<usize>,
) -> Result<Admitted, i64> {
    let endpoint = ctx.cap.object.index as usize;
    if !machine.endpoints[endpoint].open {
        return Err(status::PEER_DEAD);
    }
    let receiver = machine.endpoints[endpoint].receiver_domain;
    if receiver == u16::MAX {
        return Err(status::PEER_DEAD);
    }
    let receiver = receiver as usize;
    if machine.domains[receiver].generation != machine.endpoints[endpoint].receiver_generation
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
    if ordinary && !machine.endpoints[endpoint].has_ordinary_room() {
        return Err(status::QUEUE_FULL);
    }
    if !ordinary
        && machine.endpoints[endpoint].reserved_used >= machine.endpoints[endpoint].reserved_cells
    {
        return Err(status::QUEUE_FULL);
    }
    if machine.domains[receiver].caps.free_slots() < count {
        let free = machine.domains[receiver].caps.free_slots() as u64;
        return Err(exhausted("receiver_cap_slots", count as u64, free));
    }

    let message_index = match machine.messages.iter().position(|message| !message.used) {
        Some(index) => index,
        None => {
            let used = machine
                .messages
                .iter()
                .filter(|message| message.used)
                .count() as u64;
            return Err(exhausted("messages", used, machine.messages.len() as u64));
        }
    };
    let invocation_index = match machine
        .invocations
        .iter()
        .position(|invocation| invocation.state == State::Empty)
    {
        Some(index) => index,
        None => {
            let used = machine
                .invocations
                .iter()
                .filter(|invocation| invocation.state != State::Empty)
                .count() as u64;
            return Err(exhausted(
                "invocations",
                used,
                machine.invocations.len() as u64,
            ));
        }
    };

    let origin_scope = thread::get(ctx.thread).effective_scope();
    let charged = if ordinary {
        let bytes = MESSAGE_OVERHEAD_BYTES + u64::from(request.payload_len);
        if !scope::reserve(origin_scope, Resource::QueueBytes, bytes) {
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
        if charged != 0 {
            scope::release(origin_scope, Resource::QueueBytes, charged);
        }
        let (used, capacity) = match machine.system_log {
            Some(index) => {
                let log = &machine.logs[index as usize];
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
    let generation = machine.invocations[invocation_index]
        .generation
        .saturating_add(1);
    let parent_id = thread::get(ctx.thread)
        .bound()
        .map_or(0, |(index, _)| machine.invocations[index as usize].id);

    machine.invocations[invocation_index] = Invocation {
        state: State::Admitted,
        generation,
        id,
        endpoint: endpoint as u16,
        endpoint_generation: machine.endpoints[endpoint].generation,
        epoch: machine.endpoints[endpoint].epoch,
        origin_domain: ctx.domain as u16,
        origin_domain_generation: machine.domains[ctx.domain].generation,
        origin_domain_id: machine.domains[ctx.domain].id,
        origin_scope,
        origin_scope_id: scope::table()[origin_scope as usize].id(),
        waiter,
        parent_id,
        grant: ctx.cap.grant,
        facet: ctx.cap.facet,
        cancel: Cancel::Live,
        effect: Effect::None,
        closure_reserved_ns: 0,
        closure_service: 0,
        admitted_ns: ctx.now,
        message: message_index as u16,
        receiver_domain: receiver as u16,
        charged_bytes: charged,
        refs: 0,
        replied: false,
        outcome: 0,
        reply_payload: [0; 256],
        reply_len: 0,
        reply_caps: [0; 4],
        reply_cap_count: 0,
        reply_result: 0,
    };
    machine.grants[ctx.cap.grant as usize].refs += 1;

    let mut message = Message::empty();
    message.used = true;
    message.next = NO_MESSAGE;
    message.endpoint = endpoint as u16;
    message.endpoint_generation = machine.endpoints[endpoint].generation;
    message.invocation = invocation_index as u16;
    message.reserved_cell = !ordinary;
    message.kind = kind;
    message.payload_len = request.payload_len;
    message.payload[..request.payload_len as usize]
        .copy_from_slice(&request.payload[..request.payload_len as usize]);
    message.caps = installed;
    message.cap_count = request.cap_count;
    message.charged_bytes = charged;
    machine.messages[message_index] = message;

    let tail = machine.endpoints[endpoint].tail;
    if tail == NO_MESSAGE {
        machine.endpoints[endpoint].head = message_index as u16;
    } else {
        machine.messages[tail as usize].next = message_index as u16;
    }
    machine.endpoints[endpoint].tail = message_index as u16;
    machine.endpoints[endpoint].queued += 1;
    if !ordinary {
        machine.endpoints[endpoint].reserved_used += 1;
    }
    machine.endpoints[endpoint].admitted += 1;
    scope::table()[origin_scope as usize]
        .invocations_pending
        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    trace!(
        "ipc.admitted",
        "invocation={id} endpoint={} epoch={} facet={} grant={} origin_domain={} \
         origin_scope={} parent={parent_id} payload_len={} caps={} queued={} charged_bytes={charged} \
         kind={kind}",
        machine.endpoints[endpoint].id,
        machine.endpoints[endpoint].epoch,
        ctx.cap.facet,
        machine.grants[ctx.cap.grant as usize].id,
        machine.domains[ctx.domain].id,
        scope::table()[origin_scope as usize].id(),
        request.payload_len,
        request.cap_count,
        machine.endpoints[endpoint].queued
    );
    receipt(
        machine,
        receipt_kind::ADMIT,
        ctx.domain,
        origin_scope,
        machine.endpoints[endpoint].id,
        machine.grants[ctx.cap.grant as usize].id,
        parent_id,
        status::OK,
        id,
        ctx.cap.facet,
        true,
    );

    // A caller that will block for the reply leaves its processor free, and
    // that processor picks the receiver up on its way to idle; kicking another
    // one would turn a round trip on one processor into two interrupts across
    // two. A sender that keeps running has no free processor to offer.
    wake_receiver(machine, endpoint, waiter.is_none());
    Ok(Admitted {
        invocation: invocation_index as u16,
        generation,
        id,
    })
}

fn wake_receiver(machine: &mut Machine, endpoint: usize, kick: bool) {
    let generation = machine.endpoints[endpoint].generation;
    // A caller that will block for the reply hands its processor to the
    // receiver; a sender that keeps running sends it to an idle one.
    let hint = if kick { WakeHint::Any } else { WakeHint::Sync };
    for (index, _) in thread::iter() {
        if thread::wake_if(
            index,
            |record| record.wait == Wait::Receive(endpoint as u16, generation),
            status::OK,
            0,
            hint,
        ) {
            return;
        }
    }
}

fn wake_waiter(machine: &mut Machine, invocation: usize, code: i64, hint: WakeHint) {
    let Some(waiter) = machine.invocations[invocation].waiter else {
        return;
    };
    let generation = machine.invocations[invocation].generation;
    thread::wake_if(
        waiter,
        |record| record.wait == Wait::Reply(invocation as u16, generation),
        code,
        0,
        hint,
    );
}

/// Admits a message without waiting for a reply.
pub fn send(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
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
        let mut machine = MACHINE.lock();
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
            &mut machine,
            &full,
            &request,
            message_kind::REQUEST,
            Some(ctx.thread),
        )?;
        thread::prepare_wait(
            ctx.thread,
            Wait::Reply(admitted.invocation, admitted.generation),
            ctx.deadline,
        );
        (admitted.invocation, admitted.generation)
    };

    crate::sched::block_current();

    let (woken, _) = thread::take_wake_status(ctx.thread);
    let mut machine = MACHINE.lock();
    let index = invocation as usize;
    if machine.invocations[index].generation != generation {
        return Err(status::PEER_DEAD);
    }
    let invocation = machine.invocations[index];
    if invocation.replied {
        let mut result = CallResult {
            payload_len: invocation.reply_len,
            cap_count: invocation.reply_cap_count,
            caps: invocation.reply_caps,
            result: invocation.reply_result,
            invocation_id: invocation.id,
            payload: invocation.reply_payload,
        };
        result.payload_len = invocation.reply_len;
        begin_response(staging, ctx.operation);
        staging.write(BODY, result);
        release_invocation(&mut machine, index);
        return Ok(invocation.id);
    }
    if invocation.state == State::Resolved {
        let code = match invocation.outcome {
            outcome_value::COMMITTED => status::OK,
            outcome_value::UNKNOWN => status::PENDING,
            _ => status::CANCELLED,
        };
        release_invocation(&mut machine, index);
        if code == status::OK {
            begin_response(staging, ctx.operation);
            return Ok(invocation.id);
        }
        return Err(code);
    }
    // The wait ended without a result. The invocation stays admitted and its
    // identifier stays recoverable: a timeout is not a proof of absence.
    machine.invocations[index].waiter = None;
    Err(if woken == status::OK {
        status::TIMED_OUT
    } else {
        woken
    })
}

/// Takes the oldest admitted message with its authenticated header.
pub fn receive(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    loop {
        {
            let mut machine = MACHINE.lock();
            let cap = resolve(
                &machine,
                ctx.domain,
                ctx.handle,
                spec.object_type,
                spec.rights,
                crate::api::now_ns(),
            )?;
            let endpoint = cap.object.index as usize;

            // An endpoint has exactly one receiver. `install_cap` names it when
            // a supervisor hands the receive side to a domain it is building;
            // this is the same claim made by a domain acting for itself, which
            // is the only way a supervisor can receive on an endpoint it
            // created. It is a claim, not a transfer: an endpoint another live
            // domain already receives on is refused rather than stolen.
            let current = machine.endpoints[endpoint].receiver_domain;
            if current == u16::MAX {
                machine.endpoints[endpoint].receiver_domain = ctx.domain as u16;
                machine.endpoints[endpoint].receiver_generation =
                    machine.domains[ctx.domain].generation;
                trace!(
                    "ipc.receiver_claimed",
                    "endpoint={} domain={} name={} route=self_claim",
                    machine.endpoints[endpoint].id,
                    machine.domains[ctx.domain].id,
                    machine.domains[ctx.domain].name_str()
                );
            } else if current as usize != ctx.domain {
                return Err(status::STATE_CONFLICT);
            }

            let head = machine.endpoints[endpoint].head;
            if head != NO_MESSAGE {
                return deliver(&mut machine, ctx, endpoint, head as usize, staging);
            }
            if ctx.flags & thalyx_abi::generated::flag::NONBLOCKING != 0 {
                return Err(status::WOULD_BLOCK);
            }
            thread::prepare_wait(
                ctx.thread,
                Wait::Receive(endpoint as u16, machine.endpoints[endpoint].generation),
                ctx.deadline,
            );
        }
        crate::sched::block_current();
        let (woken, _) = thread::take_wake_status(ctx.thread);
        if woken != status::OK {
            return Err(woken);
        }
    }
}

fn deliver(
    machine: &mut Machine,
    ctx: &Ctx,
    endpoint: usize,
    message_index: usize,
    staging: &mut Staging,
) -> Result<u64, i64> {
    let message = machine.messages[message_index];
    let invocation_index = message.invocation as usize;

    // Admission recorded which endpoint the work arrived on. Delivering it from
    // a different one would mean the queue and the invocation had drifted
    // apart, and the header the receiver is about to be handed -- which it is
    // entitled to treat as the kernel's statement -- would be wrong about where
    // the work came from.
    if machine.invocations[invocation_index].endpoint as usize != endpoint
        || machine.invocations[invocation_index].endpoint_generation
            != machine.endpoints[endpoint].generation
    {
        return Err(status::STATE_CONFLICT);
    }

    // The receiver needs a handle on the obligation before the message leaves
    // the queue: if the table is full the message stays where it was, which is
    // what "the message is kept if the delivery cannot be prepared" means.
    let object = ObjRef::new(
        ObjKind::Invocation,
        message.invocation,
        machine.invocations[invocation_index].generation,
    );
    let sponsor = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        sponsor,
        NO_GRANT,
        object,
        TICKET_RIGHTS,
        0,
        None,
        machine.invocations[invocation_index].facet,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let Some(ticket) = crate::api::cap_install(machine, ctx.domain, object, grant, None) else {
        crate::api::collect_grant(machine, grant);
        return Err(status::LIMIT_EXHAUSTED);
    };

    // Dequeue only now that the delivery is certain.
    machine.endpoints[endpoint].head = message.next;
    if machine.endpoints[endpoint].head == NO_MESSAGE {
        machine.endpoints[endpoint].tail = NO_MESSAGE;
    }
    machine.endpoints[endpoint].queued -= 1;
    if message.reserved_cell {
        machine.endpoints[endpoint].reserved_used =
            machine.endpoints[endpoint].reserved_used.saturating_sub(1);
    }
    machine.endpoints[endpoint].delivered += 1;
    machine.messages[message_index] = Message::empty();

    let invocation = machine.invocations[invocation_index];
    machine.invocations[invocation_index].state = State::Delivered;
    machine.invocations[invocation_index].message = NO_MESSAGE;
    machine.invocations[invocation_index].receiver_domain = ctx.domain as u16;

    let mut result = ReceiveResult {
        header: MessageHeader {
            epoch: invocation.epoch,
            invocation_id: invocation.id,
            sender_domain_id: invocation.origin_domain_id,
            sender_scope_id: invocation.origin_scope_id,
            parent_invocation_id: invocation.parent_id,
            facet: invocation.facet,
            grant_id: machine.grants[invocation.grant as usize].id,
            sent_ns: invocation.admitted_ns,
            rights_transferred: 0,
            cancel_state: invocation.cancel.abi(),
            kind: message.kind,
            payload_len: message.payload_len,
        },
        cap_count: message.cap_count,
        reserved0: 0,
        caps: [0; 4],
        payload: message.payload,
    };
    for index in 0..message.cap_count as usize {
        result.caps[index] = message.caps[index].handle;
        result.header.rights_transferred |=
            machine.grants[message.caps[index].grant as usize].rights;
    }
    begin_response(staging, ctx.operation);
    staging.write(BODY, result);

    trace!(
        "ipc.delivered",
        "invocation={} endpoint={} receiver_domain={} facet={} caps={} payload_len={} \
         cancel={} ticket=0x{ticket:x}",
        invocation.id,
        machine.endpoints[endpoint].id,
        machine.domains[ctx.domain].id,
        invocation.facet,
        message.cap_count,
        message.payload_len,
        invocation.cancel.name()
    );
    Ok(ticket)
}

/// Releases everything an invocation held once it is discharged.
fn release_invocation(machine: &mut Machine, index: usize) {
    let invocation = machine.invocations[index];
    if invocation.state == State::Empty {
        return;
    }
    if invocation.charged_bytes != 0 {
        scope::release(
            invocation.origin_scope,
            Resource::QueueBytes,
            invocation.charged_bytes,
        );
        machine.invocations[index].charged_bytes = 0;
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
        machine.invocations[index].closure_reserved_ns = 0;
        machine.invocations[index].effect = Effect::Resolved;
    }
    scope::note_progress(invocation.origin_scope, crate::api::now_ns());
    if invocation.grant != NO_GRANT {
        machine.grants[invocation.grant as usize].refs = machine.grants[invocation.grant as usize]
            .refs
            .saturating_sub(1);
        let grant = invocation.grant;
        machine.invocations[index].grant = NO_GRANT;
        crate::api::collect_grant(machine, grant);
    }
    machine.invocations[index].state = State::Resolved;
    crate::api::collect_invocation(machine, index);
}

/// Consumes the one-shot reply of an invocation.
pub fn reply(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: ReplyRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    let invocation = machine.invocations[index];
    if invocation.state == State::Resolved || invocation.replied {
        return Err(status::ALREADY_RESOLVED);
    }
    if invocation.waiter.is_none() {
        return Err(status::STATE_CONFLICT);
    }
    if u64::from(request.payload_len) > thalyx_abi::limit::MAX_INLINE_PAYLOAD
        || u64::from(request.cap_count) > thalyx_abi::limit::MAX_CAPS_PER_MESSAGE
    {
        return Err(status::INVALID_ARGUMENT);
    }

    let caller = invocation.origin_domain as usize;
    if machine.domains[caller].generation != invocation.origin_domain_generation {
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

    machine.invocations[index].reply_payload = request.payload;
    machine.invocations[index].reply_len = request.payload_len;
    machine.invocations[index].reply_caps = installed;
    machine.invocations[index].reply_cap_count = request.cap_count;
    machine.invocations[index].reply_result = request.result;
    machine.invocations[index].replied = true;
    machine.invocations[index].outcome = outcome_value::COMMITTED;

    trace!(
        "ipc.replied",
        "invocation={} responder_domain={} payload_len={} caps={} result=0x{:x}",
        invocation.id,
        machine.domains[ctx.domain].id,
        request.payload_len,
        request.cap_count,
        request.result
    );
    wake_waiter(machine, index, status::OK, WakeHint::Sync);
    Ok(invocation.id)
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
    let invocation = machine.invocations[index];
    if invocation.state == State::Resolved {
        return Err(status::ALREADY_RESOLVED);
    }
    if invocation.effect != Effect::None {
        return Err(status::ALREADY_RESOLVED);
    }

    let lineage = crate::api::lineage_status(machine, invocation.grant, ctx.now);
    let scope_open = scope::is_open(invocation.origin_scope);
    if lineage != status::OK || !scope_open {
        machine.invocations[index].effect = Effect::Refused;
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
            machine.grants[invocation.grant as usize].id
        );
        receipt(
            machine,
            receipt_kind::EFFECT,
            ctx.domain,
            invocation.origin_scope,
            invocation.id,
            machine.grants[invocation.grant as usize].id,
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

    node.closure_reserved_ns
        .fetch_add(reserve, core::sync::atomic::Ordering::Relaxed);
    machine.invocations[index].effect = Effect::Admitted;
    machine.invocations[index].closure_service = service;
    machine.invocations[index].closure_reserved_ns = reserve;
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
        machine.grants[invocation.grant as usize].id,
        request.effect_kind
    );
    receipt(
        machine,
        receipt_kind::EFFECT,
        ctx.domain,
        invocation.origin_scope,
        invocation.id,
        machine.grants[invocation.grant as usize].id,
        invocation.parent_id,
        status::OK,
        u64::from(request.effect_kind),
        reserve,
        false,
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
    let invocation = machine.invocations[index];
    if invocation.state == State::Resolved {
        return Err(status::ALREADY_RESOLVED);
    }
    machine.invocations[index].outcome = request.outcome;
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
    let invocation = machine.invocations[index];
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
            machine.grants[invocation.grant as usize].id
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
    let endpoint = &machine.endpoints[index];
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
    let invocation = machine.invocations[index];
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
    machine.invocations[index].refs += 1;
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
    if machine.invocations[index as usize].generation == generation {
        machine.invocations[index as usize].refs =
            machine.invocations[index as usize].refs.saturating_sub(1);
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
        if !machine.endpoints[endpoint].used {
            continue;
        }
        let mut previous = NO_MESSAGE;
        let mut current = machine.endpoints[endpoint].head;
        while current != NO_MESSAGE {
            let message = machine.messages[current as usize];
            let invocation = message.invocation as usize;
            let origin = machine.invocations[invocation].origin_scope;
            let next = message.next;
            if machine.invocations[invocation].state == State::Admitted
                && scope::is_within(root, origin)
            {
                if previous == NO_MESSAGE {
                    machine.endpoints[endpoint].head = next;
                } else {
                    machine.messages[previous as usize].next = next;
                }
                if machine.endpoints[endpoint].tail == current {
                    machine.endpoints[endpoint].tail = previous;
                }
                machine.endpoints[endpoint].queued =
                    machine.endpoints[endpoint].queued.saturating_sub(1);
                if message.reserved_cell {
                    machine.endpoints[endpoint].reserved_used =
                        machine.endpoints[endpoint].reserved_used.saturating_sub(1);
                }
                for entry in message.caps.iter().take(message.cap_count as usize) {
                    let receiver = machine.endpoints[endpoint].receiver_domain as usize;
                    crate::api::cap_release_slot(machine, receiver, entry.slot as usize);
                }
                machine.messages[current as usize] = Message::empty();
                machine.invocations[invocation].cancel = Cancel::OriginFenced;
                machine.invocations[invocation].outcome = outcome_value::ABORTED;
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
        let invocation = machine.invocations[index];
        if invocation.state == State::Empty || invocation.state == State::Resolved {
            continue;
        }
        if scope::is_within(root, invocation.origin_scope) {
            machine.invocations[index].cancel = Cancel::OriginFenced;
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
        if !machine.endpoints[endpoint].used {
            continue;
        }
        let mut previous = NO_MESSAGE;
        let mut current = machine.endpoints[endpoint].head;
        while current != NO_MESSAGE {
            let message = machine.messages[current as usize];
            let invocation = message.invocation as usize;
            let next = message.next;
            if machine.invocations[invocation].origin_domain as usize == domain
                && machine.invocations[invocation].origin_domain_generation == generation
            {
                if previous == NO_MESSAGE {
                    machine.endpoints[endpoint].head = next;
                } else {
                    machine.messages[previous as usize].next = next;
                }
                if machine.endpoints[endpoint].tail == current {
                    machine.endpoints[endpoint].tail = previous;
                }
                machine.endpoints[endpoint].queued =
                    machine.endpoints[endpoint].queued.saturating_sub(1);
                if message.reserved_cell {
                    machine.endpoints[endpoint].reserved_used =
                        machine.endpoints[endpoint].reserved_used.saturating_sub(1);
                }
                for entry in message.caps.iter().take(message.cap_count as usize) {
                    let receiver = machine.endpoints[endpoint].receiver_domain as usize;
                    crate::api::cap_release_slot(machine, receiver, entry.slot as usize);
                }
                machine.messages[current as usize] = Message::empty();
                machine.invocations[invocation].cancel = Cancel::OriginDead;
                machine.invocations[invocation].outcome = outcome_value::ABORTED;
                release_invocation(machine, invocation);
            } else {
                previous = current;
            }
            current = next;
        }
    }

    for index in 0..machine.invocations.len() {
        let invocation = machine.invocations[index];
        if invocation.state == State::Empty || invocation.state == State::Resolved {
            continue;
        }
        if invocation.origin_domain as usize == domain
            && invocation.origin_domain_generation == generation
        {
            machine.invocations[index].cancel = Cancel::OriginDead;
            machine.invocations[index].waiter = None;
        }
        // A receiver that died owes an answer it can no longer give. The result
        // is unknown, not aborted: the kernel never invents a rollback.
        if invocation.state == State::Delivered && invocation.receiver_domain as usize == domain {
            machine.invocations[index].outcome = outcome_value::UNKNOWN;
            wake_waiter(machine, index, status::PEER_DEAD, WakeHint::Any);
            release_invocation(machine, index);
        }
    }

    // An endpoint whose receiver is gone stops admitting. Clients detect the
    // dead peer and reconnect through new capabilities; nothing resurrects.
    for endpoint in 0..machine.endpoints.len() {
        if machine.endpoints[endpoint].used
            && machine.endpoints[endpoint].receiver_domain as usize == domain
            && machine.endpoints[endpoint].receiver_generation == generation
        {
            machine.endpoints[endpoint].open = false;
        }
    }

    if let Some(object) = machine.domains[domain].fault_endpoint.take() {
        let index = object.index as usize;
        if machine.endpoints[index].used && machine.endpoints[index].generation == object.generation
        {
            machine.endpoints[index].reserved_cells =
                machine.endpoints[index].reserved_cells.saturating_sub(1);
        }
        let grant = machine.domains[domain].fault_grant;
        if grant != NO_GRANT {
            machine.grants[grant as usize].refs =
                machine.grants[grant as usize].refs.saturating_sub(1);
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
                machine.endpoints[object.index as usize].id,
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
