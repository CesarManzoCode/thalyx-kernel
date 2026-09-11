//! The entries of the plan this supervisor runs itself, and the audit.
//!
//! The client runs every entry it holds the authority for. These are the ones
//! it does not: a scope with a budget of its own, a domain built only to be
//! stopped, the engine's own load, a work closed while the engine computes for
//! it, and the auditor's view of what draining the control log costs. Each is
//! run when the client hands it over, in the plan's order, and reports its
//! samples between the client's BEGIN and END for that entry.

use core::cell::UnsafeCell;

use thalyx_abi::{cap_op, receipt_kind, right, status};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k5pkg::proto::{EngineReply, EngineRequest, engine_op, engine_status};
use thalyx_user_rt::k2::{self, name16};

use crate::generated::{self, Handoff, bench};
use crate::{Run, bench_config, cycles, launch, limits, note};

/// Samples one entry may take here.
pub const SAMPLES: usize = 4096;

struct Buffer(UnsafeCell<[u32; SAMPLES]>);

// SAFETY: this supervisor has one thread, and each buffer is reached only
// through the accessor below, whose borrows never overlap.
unsafe impl Sync for Buffer {}

static SCRATCH: Buffer = Buffer(UnsafeCell::new([0; SAMPLES]));
static DRAIN: Buffer = Buffer(UnsafeCell::new([0; SAMPLES]));

fn buffer(which: &'static Buffer) -> &'static mut [u32; SAMPLES] {
    // SAFETY: see `Buffer`. The one borrow a caller holds ends before the next
    // accessor call on the same buffer.
    unsafe { &mut *which.0.get() }
}

/// Batches of receipts the auditor read, each timed.
pub struct DrainSamples {
    count: usize,
}

impl DrainSamples {
    pub const fn new() -> Self {
        Self { count: 0 }
    }
}

fn clamp32(value: u64) -> u32 {
    if value >= generated::LOST_SAMPLE {
        (generated::LOST_SAMPLE - 1) as u32
    } else {
        value as u32
    }
}

fn emit(values: &[u32]) {
    let mut index = 0;
    while index < values.len() {
        let low = u64::from(values[index]);
        let high = if index + 1 < values.len() {
            u64::from(values[index + 1])
        } else {
            generated::LOST_SAMPLE
        };
        k2::note(generated::note::SAMPLE, low | (high << 32));
        index += 2;
    }
}

fn spin_cycles(count: u64) {
    let start = cycles();
    while cycles() - start < count {
        core::hint::spin_loop();
    }
}

/// Reads and acknowledges everything the log holds, timing each batch.
///
/// The same reading as K5's auditor, with a clock around it: what one batch of
/// receipts costs the domain that has to drain it is a cost of the audited
/// profile, and `audit.drain` reports it. With `wait_until`, the first read
/// waits that long for a receipt when the log is empty, so an auditor with
/// nothing to do sleeps in the kernel and is woken by the first admission
/// rather than polling; that waited batch is not timed, the ones after it
/// are. Answers whether anything was there.
pub fn drain_timed(run: &mut Run, wait_until: u64) -> bool {
    let mut drained = false;
    let mut waiting = wait_until != 0;
    loop {
        let started = cycles();
        let read = if waiting {
            k2::log_read_until(run.log, wait_until)
        } else {
            k2::log_read(run.log)
        };
        let Ok(batch) = read else {
            return drained;
        };
        run.polls += 1;
        if batch.lost > run.auditor.lost {
            run.auditor.lost = batch.lost;
        }
        if batch.count == 0 {
            run.empty_polls += 1;
            return drained;
        }
        drained = true;
        let mut through = 0u64;
        for record in batch.records.iter().take(batch.count as usize) {
            if run.auditor.expected != 0 && record.sequence != run.auditor.expected {
                run.auditor.gaps += 1;
            }
            run.auditor.expected = record.sequence + 1;
            run.auditor.drained += 1;
            if record.kind == receipt_kind::EFFECT {
                run.auditor.effects += 1;
            }
            through = record.sequence;
        }
        if k2::log_acknowledge(run.log, through).is_err() {
            return drained;
        }
        let spent = cycles() - started;
        if !waiting && run.drain.count < SAMPLES {
            buffer(&DRAIN)[run.drain.count] = clamp32(spent);
            run.drain.count += 1;
        }
        waiting = false;
    }
}

/// Runs one handed-over entry. Zero, or the negated status it stopped with.
pub fn run(run: &mut Run, request: &Handoff) -> u32 {
    let samples = (request.samples as usize).min(SAMPLES);
    let result = match request.bench {
        bench::QUOTA_SHARE => quota(run, request.param),
        bench::CLOSURE_UNIT => closure(run, samples),
        bench::ENGINE_LOAD => engine_load(run),
        bench::ENGINE_CANCEL => engine_cancel(run, samples),
        bench::AUDIT_DRAIN => audit_drain(run, samples),
        _ => Err(status::NOT_SUPPORTED),
    };
    match result {
        Ok(()) => 0,
        Err(code) => (-code) as u32,
    }
}

fn report_step(base: u64) -> impl FnMut(u64, i64) {
    move |which, code| {
        k2::note(note::BUILD_STEP_FAILED, base + which);
        k2::note(thalyx_user_rt::k2::report::UNEXPECTED, code as u64);
    }
}

/// Fences a scope and retires it once nothing it sponsors is outstanding,
/// auditing while it waits.
fn retire(run: &mut Run, scope: u64) -> Result<(), i64> {
    let _ = k2::scope_fence(scope);
    let deadline = k2::now_ns() + 30_000_000_000;
    loop {
        let (outcome, _) = k2::scope_retire(scope);
        if outcome.is_ok() {
            let _ = k2::cap_close(scope);
            return Ok(());
        }
        if k2::now_ns() > deadline {
            return Err(status::DRAIN_INCOMPLETE);
        }
        drain_timed(run, 0);
    }
}

/// The spinning domain of `quota.share`, in a scope whose budget is the
/// entry's share of each window. It reports its own samples.
fn quota(run: &mut Run, percent: u32) -> Result<(), i64> {
    let budget = run.window_ns * u64::from(percent) / 100;
    let scope = k2::scope_create_child(run.system, limits(budget, 512, 1), name16("k6quota"))?;
    let recipe = launch::Recipe {
        name: "k6spin",
        image: run.bench_image,
        scope,
        stack_pages: 32,
        threads: 0,
        config: bench_config(generated::ROLE_SPIN, 0, 1_000_000_000, false, run.seed),
        fault_channel: run.supervision,
    };
    let built = launch::build(&recipe, &[], report_step(200)).ok_or(status::LIMIT_EXHAUSTED)?;
    let _ = k2::cap_close(built.work_signal);
    let deadline = k2::now_ns() + 60_000_000_000;
    let mut done = false;
    while k2::now_ns() < deadline {
        if let Ok(seen) = k2::signal_wait(
            built.done_signal,
            thalyx_user_k5pkg::native::DONE_BIT,
            k2::now_ns() + 20_000_000,
        ) && seen.bits & thalyx_user_k5pkg::native::DONE_BIT != 0
        {
            done = true;
            break;
        }
        drain_timed(run, 0);
    }
    let _ = k2::cap_close(built.done_signal);
    let _ = k2::domain_terminate(built.domain);
    let _ = k2::cap_close(built.domain);
    retire(run, scope)?;
    if done { Ok(()) } else { Err(status::TIMED_OUT) }
}

/// Stopping a unit of work and getting its resources back: a scope whose one
/// domain is blocked in the kernel is fenced, its domain terminated and the
/// scope retired, and the sample is the time from the fence to the retirement
/// the kernel accepted.
fn closure(run: &mut Run, samples: usize) -> Result<(), i64> {
    let values = buffer(&SCRATCH);
    for index in 0..samples {
        let scope = k2::scope_create_child(
            run.system,
            limits(run.window_ns / 10, 256, 1),
            name16("k6idle"),
        )?;
        let recipe = launch::Recipe {
            name: "k6idle",
            image: run.bench_image,
            scope,
            stack_pages: 16,
            threads: 0,
            config: bench_config(generated::ROLE_IDLE, 0, 0, false, run.seed),
            fault_channel: run.supervision,
        };
        let built = launch::build(&recipe, &[], report_step(300)).ok_or(status::LIMIT_EXHAUSTED)?;
        let _ = k2::cap_close(built.work_signal);
        // The domain says when it is about to block; then a moment for the two
        // entries between saying so and blocking. The Linux side gives its
        // child two milliseconds for the same purpose.
        let ready = k2::signal_wait(
            built.done_signal,
            generated::IDLE_READY_BIT,
            k2::now_ns() + 5_000_000_000,
        );
        let _ = k2::cap_close(built.done_signal);
        if !ready.is_ok_and(|seen| seen.bits & generated::IDLE_READY_BIT != 0) {
            let _ = k2::domain_terminate(built.domain);
            let _ = k2::cap_close(built.domain);
            return Err(status::TIMED_OUT);
        }
        spin_cycles(run.tsc_hz / 10_000);
        let started = cycles();
        k2::scope_fence(scope)?;
        let fenced = cycles();
        let terminated = k2::domain_terminate(built.domain);
        let term = cycles();
        let deadline = k2::now_ns() + 30_000_000_000;
        let mut retired = false;
        let mut attempts = 0u64;
        while k2::now_ns() < deadline {
            let (outcome, _) = k2::scope_retire(scope);
            attempts += 1;
            if outcome.is_ok() {
                retired = true;
                break;
            }
        }
        let finished = cycles();
        let _ = k2::cap_close(built.domain);
        let _ = k2::cap_close(scope);
        terminated?;
        if !retired {
            return Err(status::DRAIN_INCOMPLETE);
        }
        if index == 0 {
            // Where the first sample's time went, so a change in the total
            // can be placed.
            for (quantity, value) in [
                (1u64, (fenced - started) * 1_000_000 / run.tsc_hz),
                (2, (term - fenced) * 1_000_000 / run.tsc_hz),
                (3, (finished - term) * 1_000_000 / run.tsc_hz),
                (4, attempts),
            ] {
                k2::note(
                    generated::note::AUX,
                    u64::from(bench::CLOSURE_UNIT) | (quantity << 16) | (value << 32),
                );
            }
        }
        values[index] = clamp32(finished - started);
        drain_timed(run, 0);
    }
    emit(&values[..samples]);
    Ok(())
}

/// The engine's load: from starting to build its domain to its ready signal,
/// measured once, when the run built it.
fn engine_load(run: &mut Run) -> Result<(), i64> {
    if run.engine.is_none() {
        return Err(status::INVALID_HANDLE);
    }
    emit(&[clamp32(run.engine_load_us)]);
    Ok(())
}

/// One question from this supervisor to the engine, the shortest in the
/// fixture, answered or not.
fn ask_engine(run: &mut Run, index: usize) -> Result<(), i64> {
    let prompt = generated::ENGINE_PROMPT[index].as_bytes();
    let buffer = k2::scope_create_memory(
        run.system,
        2,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("k6prompt"),
    )?;
    let outcome = (|| {
        k2::memory_write(buffer, 0, prompt)?;
        let request = EngineRequest {
            op: engine_op::INFER,
            predict: generated::ENGINE_PREDICT[index],
            prompt_len: prompt.len() as u32,
            grammar_len: 0,
            seed: run.seed,
        };
        let lent = k2::derive(
            buffer,
            right::INSPECT | right::TRANSFER | right::MEMORY_READ | right::MEMORY_WRITE,
            0,
            0,
        )?;
        let answer = k2::endpoint_call(
            run.engine_facet,
            6,
            request.as_bytes(),
            &[(lent, cap_op::MOVE)],
            k2::now_ns() + 120_000_000_000,
            false,
        )?;
        let reply = EngineReply::read_from(&answer.payload, 0).ok_or(status::STATE_CONFLICT)?;
        if reply.status != engine_status::OK {
            return Err(-(100 + i64::from(reply.status)));
        }
        Ok(())
    })();
    let _ = k2::cap_close(buffer);
    outcome
}

/// The cancellation this backend's profile declares: the asking work's scope
/// is fenced once the engine is bound to its request and has computed ten
/// milliseconds for it -- the trigger the Linux side uses too -- and the sample
/// is the time from the fence to the next answered request from another
/// caller. The engine stays resident, so the next answer needs no load.
fn engine_cancel(run: &mut Run, samples: usize) -> Result<(), i64> {
    let Some(endpoint) = run.engine.as_ref().map(|engine| engine.endpoint) else {
        return Err(status::INVALID_HANDLE);
    };
    let values = buffer(&SCRATCH);
    for index in 0..samples {
        let scope = k2::scope_create_child(
            run.system,
            limits(run.window_ns / 2, 512, 2),
            name16("k6ask"),
        )?;
        let asking = k2::scope_create_signal(run.system)?;
        let facet = crate::bind_facet(endpoint, right::INSPECT | right::ENDPOINT_CALL)
            .ok_or(status::LIMIT_EXHAUSTED)?;
        let installs = [
            launch::Install {
                slot: generated::SLOT_ENGINE as u32,
                handle: facet,
                rights: right::INSPECT | right::TRANSFER | right::ENDPOINT_CALL,
            },
            launch::Install {
                slot: generated::SLOT_HANDOFF as u32,
                handle: asking,
                rights: right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
            },
        ];
        let recipe = launch::Recipe {
            name: "k6asker",
            image: run.bench_image,
            scope,
            stack_pages: 32,
            threads: 0,
            config: bench_config(generated::ROLE_ASKER, 0, 0, false, run.seed),
            fault_channel: run.supervision,
        };
        let built =
            launch::build(&recipe, &installs, report_step(400)).ok_or(status::LIMIT_EXHAUSTED)?;
        let _ = k2::cap_close(facet);
        let _ = k2::cap_close(built.work_signal);
        k2::signal_wait(
            asking,
            generated::ASKER_ASKING_BIT,
            k2::now_ns() + 60_000_000_000,
        )?;
        let watch = k2::now_ns() + 60_000_000_000;
        let mut bound: Option<u64> = None;
        loop {
            if let Ok(info) = k2::scope_query(scope) {
                if bound.is_none() && info.parallelism_used >= 2 {
                    bound = Some(info.cpu_total_ns);
                }
                if let Some(start) = bound
                    && info.cpu_total_ns >= start + 10_000_000
                {
                    break;
                }
            }
            if k2::now_ns() > watch {
                return Err(status::TIMED_OUT);
            }
            spin_cycles(run.tsc_hz / 10_000);
        }
        let started = cycles();
        k2::scope_fence(scope)?;
        let asked = ask_engine(run, 0);
        let finished = cycles();
        asked?;
        values[index] = clamp32((finished - started) * 1_000_000 / run.tsc_hz);
        retire(run, scope)?;
        let _ = k2::cap_close(built.domain);
        let _ = k2::cap_close(built.done_signal);
        let _ = k2::cap_close(asking);
    }
    emit(&values[..samples]);
    Ok(())
}

/// What draining the log cost the auditor, one sample per batch read so far.
fn audit_drain(run: &mut Run, samples: usize) -> Result<(), i64> {
    let count = run.drain.count.min(samples);
    let values = buffer(&DRAIN);
    emit(&values[..count]);
    k2::note(
        generated::note::AUX,
        u64::from(bench::AUDIT_DRAIN) | (1 << 16) | (run.auditor.drained << 32),
    );
    Ok(())
}
