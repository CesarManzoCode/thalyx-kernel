//! The control log seen through a capability.
//!
//! Reading receipts requires a right; appending one requires a different right.
//! That separation is the observability contract's: a domain that may record
//! what it did is not thereby allowed to read what everyone else did.
//!
//! An appended receipt carries the caller's own two values and the kernel's
//! statement of who appended it. The request structure has a field for a
//! claimed origin precisely so that a forged one can be observed being ignored:
//! the kernel stamps the real domain and scope over it, every time.

use thalyx_abi::generated::{
    LogAckRequest, LogAppendRequest, LogInfo, LogReadResult, OpSpec, ReceiptRecord, receipt_kind,
    status,
};

use crate::api::{BODY, Ctx, begin_response, resolve};
use crate::event;
use crate::sched::WakeHint;
use crate::state::{MACHINE, Machine, Wait};
use crate::thread;
use crate::ucopy::Staging;

/// Receipts one read carries: the interface's batch.
pub const BATCH: usize = thalyx_abi::generated::limit::RECEIPT_BATCH as usize;

/// Reads a bounded batch of receipts and the loss count.
///
/// With a deadline, the read waits until the log holds a full batch or the
/// deadline passes, and answers with what there is then -- possibly nothing.
/// Without one it answers at once, as it always did. The wait is what lets an
/// auditor keep up with the log without polling it and without being woken
/// for every receipt: K6 found that an auditor that slept a tick between looks
/// let a sixty-four-cell log fill in a fraction of a millisecond of a client's
/// calls, so the admissions it was meant to cover were refused for want of a
/// cell; and that one woken per receipt cost as much execution as the client
/// it audited. The audited profile's promise is that a covered operation is
/// refused rather than unrecorded; this is what keeps the refusal from being
/// the ordinary case.
pub fn read(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    let mut final_pass = false;
    loop {
        {
            let machine = MACHINE.lock();
            let now = crate::api::now_ns();
            let cap = resolve(
                &machine,
                ctx.domain,
                ctx.handle,
                spec.object_type,
                spec.rights,
                now,
            )?;
            let index = cap.object.index as usize;
            let mut records = [ReceiptRecord::zeroed(); BATCH];
            let count = machine.logs[index].read(&mut records);
            if count >= BATCH || ctx.deadline == 0 || final_pass || now >= ctx.deadline {
                let result = LogReadResult {
                    count: count as u32,
                    lost: machine.logs[index].lost,
                    next_sequence: machine.logs[index].next_sequence,
                    records,
                };
                drop(machine);
                begin_response(staging, ctx.operation);
                staging.write(BODY, result);
                return Ok(count as u64);
            }
            if ctx.flags & thalyx_abi::generated::flag::NONBLOCKING != 0 {
                return Err(status::WOULD_BLOCK);
            }
            thread::prepare_wait(
                ctx.thread,
                Wait::Log(index as u16, cap.object.generation),
                ctx.deadline,
            );
        }
        crate::sched::block_current();
        let (woken, _) = thread::take_wake_status(ctx.thread);
        match woken {
            status::OK => {}
            // The deadline is an answer, not a failure: what the log holds at
            // that moment is what the reader asked for.
            status::TIMED_OUT => final_pass = true,
            other => return Err(other),
        }
    }
}

/// Wakes a thread waiting on `index`, once the log holds a full batch for it.
pub fn wake_reader(machine: &mut Machine, index: usize, generation: u32) {
    if machine.logs[index].count < BATCH {
        return;
    }
    for (thread, _) in thread::iter() {
        if thread::wake_if(
            thread,
            |record| record.wait == Wait::Log(index as u16, generation),
            status::OK,
            0,
            WakeHint::Any,
        ) {
            return;
        }
    }
}

/// Appends a service note whose origin the kernel stamps.
pub fn append(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: LogAppendRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let scope = thread::get(ctx.thread).effective_scope();
    let claimed = request.claimed_origin_domain_id;
    let real = machine.domains[ctx.domain].id;
    if claimed != 0 && claimed != real {
        // Not an error: the field is untrusted data. Recording that it differed
        // is what makes "the payload cannot choose an origin" observable rather
        // than asserted.
        event!(
            "ctrl.origin_overridden",
            "domain={real} claimed={claimed} kind={} note=payload_cannot_set_origin",
            request.kind
        );
    }
    let sequence = crate::api::receipt(
        machine,
        receipt_kind::SERVICE_NOTE,
        ctx.domain,
        scope,
        u64::from(request.kind),
        0,
        thread::get(ctx.thread)
            .bound()
            .map_or(0, |(index, _)| machine.invocations[index as usize].id),
        status::OK,
        request.a,
        request.b,
        false,
    );
    if sequence == 0 {
        return Err(status::LIMIT_EXHAUSTED);
    }
    Ok(sequence)
}

/// Acknowledges consumption up to a sequence.
pub fn acknowledge(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: LogAckRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    let dropped = machine.logs[index].acknowledge(request.through_sequence);
    Ok(dropped as u64)
}

/// Reports capacity, reservation and loss.
pub fn query(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let log = &machine.logs[index];
    let info = LogInfo {
        capacity: crate::limits::CONTROL_LOG_CAPACITY as u32,
        reserved_cells: crate::limits::CONTROL_LOG_RESERVED as u32,
        used: log.count as u32,
        lost: log.lost,
        next_sequence: log.next_sequence,
        oldest_sequence: log.oldest_sequence,
        object_id: log.id,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(u64::from(info.used))
}
