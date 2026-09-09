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
    LogAckRequest, LogAppendRequest, LogInfo, LogReadResult, ReceiptRecord, receipt_kind, status,
};

use crate::api::{BODY, Ctx, begin_response};
use crate::event;
use crate::state::Machine;
use crate::ucopy::Staging;

/// Reads a bounded batch of receipts and the loss count.
pub fn read(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let mut records = [ReceiptRecord::zeroed(); 4];
    let count = machine.logs[index].read(&mut records);
    let result = LogReadResult {
        count: count as u32,
        lost: machine.logs[index].lost,
        next_sequence: machine.logs[index].next_sequence,
        records,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, result);
    Ok(count as u64)
}

/// Appends a service note whose origin the kernel stamps.
pub fn append(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: LogAppendRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let scope = machine.threads[ctx.thread].effective_scope;
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
        machine.threads[ctx.thread]
            .bound_invocation
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
