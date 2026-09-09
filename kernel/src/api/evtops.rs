//! Signals and timers.
//!
//! A signal is a coalescing bit set, not a queue. Waking a consumer means "go
//! and look", and the sequence number is what lets it tell a repeat from a
//! stale read. The race this has to survive is the classic one — look, find
//! nothing, producer raises, sleep forever — and it survives it because
//! registering the wait and testing the bits happen under the same lock a raise
//! takes.
//!
//! A timer raises a signal. It records the generation of that signal when it is
//! bound, so an expiry that arrives after the signal was destroyed and its slot
//! reused raises nothing rather than raising the wrong thing.

use thalyx_abi::generated::{
    OpSpec, SignalBits, SignalInfo, TimerArmRequest, TimerCreateRequest, TimerInfo, object_type,
    right, status,
};

use crate::api::{BODY, Ctx, begin_response, resolve};
use crate::event;
use crate::events::{Signal, Timer};
use crate::obj::{NO_GRANT, ObjKind, ObjRef};
use crate::scope::{self, Resource};
use crate::state::{MACHINE, Machine, ThreadState, Wait};
use crate::ucopy::Staging;

/// Creates a coalescing signal charged to the addressed scope.
pub fn create_signal(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let sponsor = ctx.cap.object.index;
    if machine.scopes[sponsor as usize].state != scope::State::Open {
        return Err(status::SCOPE_CLOSED);
    }
    let index = machine
        .signals
        .iter()
        .position(|signal| !signal.used)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    if !scope::reserve(&mut machine.scopes, sponsor, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    let Some(id) = machine.next_id() else {
        scope::release(&mut machine.scopes, sponsor, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    };
    let generation = machine.signals[index].generation.saturating_add(1);
    machine.signals[index] = Signal {
        used: true,
        generation,
        id,
        owner_scope: sponsor,
        bits: 0,
        sequence: 0,
        waiters: 0,
        refs: 0,
    };
    let object = ObjRef::new(ObjKind::Signal, index as u16, generation);
    let owner = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        owner,
        NO_GRANT,
        object,
        ObjKind::Signal.rights_mask(),
        0,
        None,
        0,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    event!(
        "event.signal_created",
        "signal={id} scope={}",
        machine.scopes[sponsor as usize].id
    );
    Ok(handle)
}

/// Creates a timer bound to a signal and the bits it raises.
pub fn create_timer(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: TimerCreateRequest = staging.read(BODY);
    let signal = resolve(
        machine,
        ctx.domain,
        request.signal_handle,
        object_type::SIGNAL,
        right::SIGNAL_RAISE,
        ctx.now,
    )?;
    if request.bits == 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let sponsor = ctx.cap.object.index;
    if machine.scopes[sponsor as usize].state != scope::State::Open {
        return Err(status::SCOPE_CLOSED);
    }
    let index = machine
        .timers
        .iter()
        .position(|timer| !timer.used)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    if !scope::reserve(&mut machine.scopes, sponsor, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    let Some(id) = machine.next_id() else {
        scope::release(&mut machine.scopes, sponsor, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    };
    let generation = machine.timers[index].generation.saturating_add(1);
    machine.timers[index] = Timer {
        used: true,
        generation,
        id,
        owner_scope: sponsor,
        signal: signal.object.index,
        signal_generation: signal.object.generation,
        bits: request.bits,
        deadline_ns: 0,
        armed: false,
        fired: 0,
        refs: 0,
    };
    let object = ObjRef::new(ObjKind::Timer, index as u16, generation);
    let owner = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        owner,
        NO_GRANT,
        object,
        ObjKind::Timer.rights_mask(),
        0,
        None,
        0,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    event!(
        "event.timer_created",
        "timer={id} signal={} bits=0x{:x} scope={}",
        machine.signals[signal.object.index as usize].id,
        request.bits,
        machine.scopes[sponsor as usize].id
    );
    Ok(handle)
}

/// Raises bits and wakes whoever is waiting for any of them.
pub fn raise(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: SignalBits = staging.read(BODY);
    if request.bits == 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let index = ctx.cap.object.index as usize;
    raise_bits(
        machine,
        index,
        machine.signals[index].generation,
        request.bits,
    );
    Ok(machine.signals[index].sequence)
}

/// Raises bits on a signal, checking the generation first.
pub fn raise_bits(machine: &mut Machine, index: usize, generation: u32, bits: u64) {
    if !machine.signals[index].used || machine.signals[index].generation != generation {
        return;
    }
    machine.signals[index].bits |= bits;
    machine.signals[index].sequence = machine.signals[index].sequence.wrapping_add(1);
    for thread in 0..machine.threads.len() {
        if machine.threads[thread].state != ThreadState::Blocked {
            continue;
        }
        if let Wait::Signal(signal, signal_generation, mask) = machine.threads[thread].wait
            && signal as usize == index
            && signal_generation == generation
            && mask & bits != 0
        {
            machine.threads[thread].wait = Wait::None;
            machine.threads[thread].wait_deadline_ns = 0;
            machine.threads[thread].wake_status = status::OK;
            machine.threads[thread].state = ThreadState::Ready;
        }
    }
}

/// Waits for any of a mask of bits, consuming what it observed.
pub fn wait(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    let request: SignalBits = staging.read(BODY);
    if request.bits == 0 {
        return Err(status::INVALID_ARGUMENT);
    }
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
            let index = cap.object.index as usize;
            let observed = machine.signals[index].bits & request.bits;
            if observed != 0 {
                machine.signals[index].bits &= !observed;
                let info = SignalInfo {
                    bits: observed,
                    sequence: machine.signals[index].sequence,
                    object_id: machine.signals[index].id,
                    waiters: machine.signals[index].waiters,
                    reserved0: 0,
                };
                drop(machine);
                begin_response(staging, ctx.operation);
                staging.write(BODY, info);
                return Ok(observed);
            }
            if ctx.flags & thalyx_abi::generated::flag::NONBLOCKING != 0 {
                return Err(status::WOULD_BLOCK);
            }
            let thread = ctx.thread;
            machine.threads[thread].wait =
                Wait::Signal(index as u16, cap.object.generation, request.bits);
            machine.threads[thread].wait_deadline_ns = ctx.deadline;
            machine.threads[thread].wake_status = status::OK;
            machine.threads[thread].state = ThreadState::Blocked;
            machine.signals[index].waiters += 1;
        }
        crate::sched::block_current();
        let mut machine = MACHINE.lock();
        let woken = machine.threads[ctx.thread].wake_status;
        machine.threads[ctx.thread].wake_status = status::OK;
        drop(machine);
        if woken != status::OK {
            return Err(woken);
        }
    }
}

/// Reads bits and sequence without waiting.
pub fn query_signal(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let signal = &machine.signals[index];
    let info = SignalInfo {
        bits: signal.bits,
        sequence: signal.sequence,
        object_id: signal.id,
        waiters: signal.waiters,
        reserved0: 0,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(info.bits)
}

/// Arms a monotonic expiry.
pub fn arm(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: TimerArmRequest = staging.read(BODY);
    if request.deadline_ns == 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let index = ctx.cap.object.index as usize;
    machine.timers[index].deadline_ns = request.deadline_ns;
    machine.timers[index].armed = true;
    Ok(request.deadline_ns)
}

/// Cancels an armed expiry.
pub fn cancel(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    machine.timers[index].armed = false;
    machine.timers[index].deadline_ns = 0;
    Ok(0)
}

/// Reports the armed deadline and fire count.
pub fn query_timer(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let timer = &machine.timers[index];
    let info = TimerInfo {
        armed: u32::from(timer.armed),
        reserved0: 0,
        deadline_ns: timer.deadline_ns,
        fired: timer.fired,
        object_id: timer.id,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(info.fired)
}

/// Fires every armed timer whose deadline has passed.
///
/// Called from the tick with the machine lock held by the caller.
pub fn expire(machine: &mut Machine, now: u64) -> u32 {
    let mut fired = 0;
    for index in 0..machine.timers.len() {
        if !machine.timers[index].used
            || !machine.timers[index].armed
            || machine.timers[index].deadline_ns > now
        {
            continue;
        }
        machine.timers[index].armed = false;
        machine.timers[index].fired += 1;
        let signal = machine.timers[index].signal as usize;
        let generation = machine.timers[index].signal_generation;
        let bits = machine.timers[index].bits;
        raise_bits(machine, signal, generation, bits);
        fired += 1;
    }
    fired
}

/// True when a timer is armed, so the idle path knows something can still wake.
#[must_use]
pub fn any_armed(machine: &Machine) -> bool {
    machine.timers.iter().any(|timer| timer.used && timer.armed)
}
