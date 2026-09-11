//! K3 user domain `k3super`: the supervisor that builds the multiprocessor run.
//!
//! It receives the same shape of authority the K2 supervisor did — its own
//! domain, its own scope, the control log, the sealed module images — plus, on
//! a machine that has one, a capability over a device. Everything else in the
//! run it makes through the interface, and it makes it so that the races K3 is
//! about actually happen rather than being described.
//!
//! What it arranges, and why each one needs more than one processor to mean
//! anything:
//!
//! * **A budget two processors could spend twice.** Several spinners share one
//!   scope whose window budget is smaller than what they could consume between
//!   them. On one processor the arithmetic bounds them; on four, only a
//!   reservation taken before dispatch does.
//! * **An address space live on two processors while a mapping is taken away.**
//!   The probing domain has two threads. The supervisor withdraws the mapping
//!   from a third processor, and the withdrawal does not answer until every
//!   processor has acknowledged. The faults that follow are the observable
//!   half.
//! * **A seal published while a writer is running.** The writing domain stores
//!   into the object in a loop. Sealing has to withdraw its mapping, confirm the
//!   withdrawal everywhere, and only then promise the bytes are immutable — and
//!   the supervisor reads them twice, with time in between, to check the
//!   promise held.
//! * **A barrier raised while calls are being admitted.** Two callers keep
//!   calling; the supervisor services some of them and then fences their scope
//!   while they are still calling from other processors. Every call is admitted
//!   before or refused after; the run records which, and the ordering of the
//!   records is what makes the claim checkable.
//! * **A device stopped underneath its driver.** The driver finishes its work
//!   and says so. The supervisor, which holds the control right the driver does
//!   not, resets the device; everything the driver still remembers about the old
//!   session stops being accepted.
//!
//! The supervisor keeps the recovery authority throughout: bus mastering and
//! reset are rights it never hands to the driver, which is what makes "the
//! driver cannot re-arm a device that is being stopped" a property of the
//! capability rather than of the driver's good behaviour.

#![no_std]
#![no_main]

use thalyx_abi::generated::{ScopeLimits, dma_profile, memory_state, right, scope_state, status};
use thalyx_abi::{boot_handle, boot_slot};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, name16, report};
use thalyx_user_rt::k3::{self, WorkerConfig, bit, driver_slot, role, worker_slot};

/// Base of this program's FP pattern.
const FP_BASE: u64 = 0x1357_9BDF_2468_5001;
/// Module images the package may carry.
const MAX_IMAGES: u32 = 4;
/// Workers this run builds.
const WORKERS: usize = 6;
/// Intervals a spinning worker performs.
const SPIN_ROUNDS: u64 = 48;
/// Integer work in one interval.
const SPIN_BURN: u64 = 40_000;
/// Calls a calling worker attempts.
const CALL_ROUNDS: u64 = 96;
/// Calls the supervisor answers before it raises the barrier.
const SERVICED_BEFORE_FENCE: u64 = 12;
/// Windows the supervisor watches the shared budget over.
const BUDGET_WINDOWS: u64 = 12;

/// The most windows the supervisor waits for the writer to have run on every
/// processor and the probe to have read its page.
const SPREAD_WINDOWS: u64 = 200;

/// The longest the supervisor waits for the callers' first call.
const FIRST_CALL_NS: u64 = 20_000_000_000;
/// How long a fenced scope is watched before its retirement is attempted
/// regardless. Generous: the watch ends at quiescence.
const DRAIN_DEADLINE_NS: u64 = 30_000_000_000;

/// Where the supervisor keeps its own view of the shared page.
const SUPER_SHARED_VADDR: u64 = 0x1000_0000;
/// Where the supervisor keeps the object it is going to seal.
const SUPER_SEAL_VADDR: u64 = 0x1010_0000;

fn fail(step: u64, code: i64) -> ! {
    k2::note(
        report::BUILD_FAILED,
        (step << 32) | (code as u64 & 0xFFFF_FFFF),
    );
    rt::exit(step)
}

/// Finds the boot slot whose sealed image carries `label`.
fn image_named(label: &str) -> Option<u64> {
    let wanted = name16(label);
    for offset in 0..MAX_IMAGES {
        let handle = boot_handle(boot_slot::FIRST_MODULE + offset);
        let Ok(info) = k2::memory_query(handle) else {
            continue;
        };
        if info.state == memory_state::SEALED && info.label == wanted {
            return Some(handle);
        }
    }
    None
}

fn limits_for(cpu_budget_ns: u64, memory_pages: u64, parallelism: u32) -> ScopeLimits {
    ScopeLimits {
        memory_pages,
        metadata_objects: 40,
        cpu_budget_ns,
        queue_bytes: 8192,
        closure_reserve_ns: 500_000,
        parallelism,
        reserved0: 0,
    }
}

/// Releases a handle the supervisor will not name again.
///
/// A handle is charged metadata, and this domain's table is small on purpose.
/// Closing what it has finished with is how a supervisor that builds many
/// things stays inside a table it was given rather than one it grew.
fn drop_handle(handle: u64) {
    let _ = k2::cap_close(handle);
}

/// Writes a worker's configuration into a fresh one-page object.
fn config_object(scope: u64, config: WorkerConfig) -> Option<u64> {
    let object = k2::scope_create_memory(
        scope,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("config"),
    )
    .ok()?;
    // SAFETY: the structure is plain integers with no padding of its own, and
    // the slice borrows it for the duration of the call.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            core::ptr::from_ref(&config).cast::<u8>(),
            core::mem::size_of::<WorkerConfig>(),
        )
    };
    k2::memory_write(object, 0, bytes).ok()?;
    Some(object)
}

/// Builds one worker domain and activates it.
#[allow(clippy::too_many_arguments)]
fn build_worker(
    scope: u64,
    image: u64,
    name: &str,
    config: WorkerConfig,
    shared: u64,
    page: Option<(u64, u32)>,
    endpoint: Option<u64>,
    stop: u64,
    supervision: u64,
    second_thread: bool,
    stack: Option<u64>,
) -> Option<u64> {
    let domain = k2::scope_create_domain(scope, image, name16(name)).ok()?;
    let config_object = config_object(scope, config)?;

    // Its own configuration, read-only: a worker may read what it was built to
    // be and may not change it.
    k2::domain_map(
        domain,
        config_object,
        k3::CONFIG_VADDR,
        0,
        1,
        right::MEMORY_READ,
    )
    .ok()?;
    // The supervisor has no further use for the configuration it just wrote,
    // and a handle it keeps is metadata it is charged for. The mapping holds
    // the object; the handle is what is redundant.
    let _ = k2::cap_close(config_object);
    k2::domain_map(
        domain,
        shared,
        k3::SHARED_VADDR,
        0,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .ok()?;
    if let Some((object, rights)) = page {
        k2::domain_map(domain, object, k3::PROBE_VADDR, 0, 1, rights).ok()?;
    }
    if let Some(facet) = endpoint {
        k2::domain_install_cap(
            domain,
            facet,
            worker_slot::ENDPOINT,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
        )
        .ok()?;
    }
    k2::domain_install_cap(
        domain,
        stop,
        worker_slot::STOP,
        right::INSPECT | right::SIGNAL_WAIT,
        0,
    )
    .ok()?;

    if second_thread {
        let stack = stack?;
        k2::domain_map(
            domain,
            stack,
            k3::SECOND_STACK_VADDR,
            0,
            k3::SECOND_STACK_PAGES as u32,
            right::MEMORY_READ | right::MEMORY_WRITE,
        )
        .ok()?;
        let info = k2::domain_query(domain).ok()?;
        // A second thread of the same image, so the address space can be live
        // on two processors at once. It is the same entry point: what this
        // domain does is read a page, and two readers are the point.
        k2::domain_add_thread(
            domain,
            info.entry_point,
            k3::SECOND_STACK_VADDR + k3::SECOND_STACK_PAGES * 4096 - 16,
            1,
        )
        .ok()?;
        k2::note(report::THREAD_ADDED, 1);
    }

    k2::domain_set_fault_channel(domain, supervision).ok()?;
    k2::domain_activate(domain).ok()?;
    Some(domain)
}

/// Reads the shared page the workers write, through the supervisor's own
/// mapping of it.
fn shared_total() -> u64 {
    let mut total = 0u64;
    for slot in 0..WORKERS as u64 {
        // SAFETY: the supervisor mapped the shared object writable in its own
        // domain at this address, and the offset is inside the page.
        total += unsafe { core::ptr::read_volatile((SUPER_SHARED_VADDR + slot * 8) as *const u64) };
    }
    total
}

/// Sleeps until `deadline` using a timer, so the supervisor stops competing for
/// the processors the run is about.
fn sleep_until(timer: u64, signal: u64, deadline: u64) {
    if k2::timer_arm(timer, deadline).is_err() {
        return;
    }
    let _ = k2::signal_wait(signal, bit::STOP, deadline + 20_000_000);
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let own_scope = boot_handle(boot_slot::SELF_SCOPE);
    let own_domain = boot_handle(boot_slot::SELF_DOMAIN);
    let log = boot_handle(boot_slot::CONTROL_LOG);

    let limits = match k2::limits() {
        Ok(limits) => limits,
        Err(code) => fail(1, code),
    };
    k2::note(report::LIMITS, limits.page_size);
    k2::note(report::CPUS_ONLINE, u64::from(limits.cpus_online));

    let worker_image = match image_named("k3worker") {
        Some(handle) => handle,
        None => fail(2, status::INVALID_HANDLE),
    };

    // --- scopes ------------------------------------------------------------
    // The burning scope's budget is deliberately smaller than what its workers
    // could consume between them. Its parallelism ceiling is above one, so what
    // bounds the total is the budget rather than the thread count.
    let burn_scope = match k2::scope_create_child(
        own_scope,
        limits_for(limits.cpu_window_ns / 8, 96, 4),
        name16("burn"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(3, code),
    };
    let call_scope = match k2::scope_create_child(
        own_scope,
        limits_for(limits.cpu_window_ns / 4, 96, 4),
        name16("call"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(4, code),
    };
    // Half the window rather than a quarter: what this scope's domains are for
    // is to be *running* when a mapping is taken away from underneath them, and
    // a scope starved of budget is asleep at the moment that matters.
    let mem_scope = match k2::scope_create_child(
        own_scope,
        limits_for(limits.cpu_window_ns / 2, 128, 4),
        name16("mem"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(5, code),
    };
    let dev_scope = match k2::scope_create_child(
        own_scope,
        limits_for(limits.cpu_window_ns / 4, 128, 2),
        name16("dev"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(6, code),
    };
    k2::note(report::BUILT, 1);

    // --- objects -----------------------------------------------------------
    let supervision = match k2::scope_create_endpoint(own_scope, 8, name16("faults")) {
        Ok(handle) => handle,
        Err(code) => fail(7, code),
    };
    let work_endpoint = match k2::scope_create_endpoint(own_scope, 8, name16("work")) {
        Ok(handle) => handle,
        Err(code) => fail(8, code),
    };
    let stop = match k2::scope_create_signal(own_scope) {
        Ok(handle) => handle,
        Err(code) => fail(9, code),
    };
    let timer = match k2::scope_create_timer(own_scope, stop, bit::STOP) {
        Ok(handle) => handle,
        Err(code) => fail(10, code),
    };
    let shared = match k2::scope_create_memory(
        own_scope,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("shared"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(11, code),
    };
    let probe_page = match k2::scope_create_memory(
        mem_scope,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("probe"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(12, code),
    };
    let seal_page = match k2::scope_create_memory(
        mem_scope,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP | right::MEMORY_SEAL,
        name16("sealme"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(13, code),
    };
    let stack = match k2::scope_create_memory(
        mem_scope,
        k3::SECOND_STACK_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("stack2"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(14, code),
    };
    if let Err(code) = k2::domain_map(
        own_domain,
        shared,
        SUPER_SHARED_VADDR,
        0,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE,
    ) {
        fail(15, code);
    }
    // The facet number is the kernel's to choose: a caller that could name one
    // could name somebody else's.
    let facet = match k2::endpoint_bind_facet(
        work_endpoint,
        0,
        right::INSPECT | right::DERIVE | right::TRANSFER | right::ADMIN | right::ENDPOINT_CALL,
        0,
    ) {
        Ok(handle) => handle,
        Err(code) => fail(16, code),
    };
    // An endpoint has one receiver, and it is claimed rather than assumed. This
    // supervisor does not service the queue until much later; without claiming
    // now, every call made in between would be refused for want of a peer
    // instead of waiting for one, and the barrier this run raises later would
    // have nothing admitted to race against.
    match k2::endpoint_receive(work_endpoint, 0, true) {
        Ok(_) => k2::note(report::UNEXPECTED, 0),
        Err(code) if code == status::WOULD_BLOCK => k2::note(report::BOUND, 1),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
    k2::note(report::BUILT, 2);

    // --- workers -----------------------------------------------------------
    let mut built = 0u64;
    for instance in 0..2u64 {
        let config = WorkerConfig {
            role: role::SPIN,
            instance,
            rounds: SPIN_ROUNDS,
            burn: SPIN_BURN,
            slot: instance,
            reserved0: 0,
        };
        let name = if instance == 0 { "spin0" } else { "spin1" };
        match build_worker(
            burn_scope,
            worker_image,
            name,
            config,
            shared,
            None,
            None,
            stop,
            supervision,
            false,
            None,
        ) {
            // Nothing later addresses this worker: the scope it lives in is
            // what stops it, so the handle is dropped rather than hoarded.
            Some(domain) => drop_handle(domain),
            None => fail(17, status::STATE_CONFLICT),
        }
        built += 1;
    }

    let probe = match build_worker(
        mem_scope,
        worker_image,
        "probe",
        WorkerConfig {
            role: role::PROBE,
            instance: 0,
            rounds: 0,
            burn: 0,
            slot: 2,
            reserved0: 0,
        },
        shared,
        Some((probe_page, right::MEMORY_READ)),
        None,
        stop,
        supervision,
        true,
        Some(stack),
    ) {
        Some(handle) => handle,
        None => fail(18, status::STATE_CONFLICT),
    };
    built += 1;

    let writer = match build_worker(
        mem_scope,
        worker_image,
        "writer",
        WorkerConfig {
            role: role::WRITER,
            instance: 0,
            rounds: 0,
            burn: 0,
            slot: 3,
            reserved0: 0,
        },
        shared,
        Some((seal_page, right::MEMORY_READ | right::MEMORY_WRITE)),
        None,
        stop,
        supervision,
        false,
        None,
    ) {
        Some(domain) => domain,
        None => fail(19, status::STATE_CONFLICT),
    };
    built += 1;

    for instance in 0..2u64 {
        let name = if instance == 0 { "call0" } else { "call1" };
        match build_worker(
            call_scope,
            worker_image,
            name,
            WorkerConfig {
                role: role::CALLER,
                instance,
                rounds: CALL_ROUNDS,
                burn: 0,
                slot: 4 + instance,
                reserved0: 0,
            },
            shared,
            None,
            Some(facet),
            stop,
            supervision,
            false,
            None,
        ) {
            Some(domain) => drop_handle(domain),
            None => fail(20, status::STATE_CONFLICT),
        }
        built += 1;
    }
    k2::note(report::BUILT, built);

    // --- device ------------------------------------------------------------
    let device_present = build_device(dev_scope, supervision, stop, timer, log);

    // --- shared budget -----------------------------------------------------
    // Watched over several windows rather than once: a single reading could
    // catch a window that happened to be quiet.
    let mut worst = 0u64;
    for window in 0..BUDGET_WINDOWS {
        sleep_until(
            timer,
            stop,
            k2::now_ns() + limits.cpu_window_ns.saturating_mul(2),
        );
        if let Ok(info) = k2::scope_query(burn_scope) {
            if info.cpu_window_used_ns > worst {
                worst = info.cpu_window_used_ns;
            }
            k2::note(report::WINDOW_USED, info.cpu_window_used_ns);
        }
        let _ = window;
    }
    k2::note(report::SHARED_COUNTER, shared_total());

    // --- the conditions the next two steps are judged against --------------
    // The withdrawal is judged against a probe that had read the page, and the
    // seal against a writer whose address space had run on every processor,
    // so the invalidation has every processor to reach. Both used to be
    // assumed from the windows waited above; K6's scheduler, which keeps a
    // woken thread on the processor that woke it when that processor is
    // about to yield, spread the writer less, and the assumption failed on
    // KVM. Waited for as conditions now, bounded, and reported either way.
    let everyone = if limits.cpus_online >= 32 {
        u32::MAX
    } else {
        (1u32 << limits.cpus_online) - 1
    };
    let mut waited = 0u64;
    let mut mask = 0u32;
    while waited < SPREAD_WINDOWS {
        mask = k2::domain_query(writer).map_or(0, |info| info.space_cpu_mask);
        // SAFETY: the supervisor mapped the shared object writable in its own
        // domain at this address; slot 2 is the probe's word.
        let probe_rounds =
            unsafe { core::ptr::read_volatile((SUPER_SHARED_VADDR + 2 * 8) as *const u64) };
        if mask & everyone == everyone && probe_rounds > 0 {
            break;
        }
        sleep_until(timer, stop, k2::now_ns() + limits.cpu_window_ns);
        waited += 1;
    }
    k2::note(report::SPREAD_OBSERVED, u64::from(mask) | (waited << 32));
    drop_handle(writer);

    // --- a mapping taken away while the space is live ----------------------
    // The domain has two threads, so this is a withdrawal from an address space
    // another processor may be executing in right now. It does not answer until
    // every processor has acknowledged.
    match k2::domain_unmap(probe, k3::PROBE_VADDR, 1) {
        Ok(pages) => k2::note(report::MAPPED, pages),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }

    // --- a seal published while a writer is running ------------------------
    match k2::memory_seal(seal_page) {
        Ok(info) if info.state == memory_state::SEALED && info.writable_maps == 0 => {
            k2::note(report::SEALED, info.pages);
        }
        Ok(info) => k2::note(report::UNEXPECTED, u64::from(info.writable_maps)),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
    // The promise, checked rather than assumed: the same bytes twice, with a
    // window of real time in between during which the writer was still alive
    // and still trying.
    let first = read_word(seal_page);
    sleep_until(timer, stop, k2::now_ns() + limits.cpu_window_ns * 2);
    let second = read_word(seal_page);
    k2::note(report::SEAL_HELD, u64::from(first == second));
    if first != second {
        k2::note(report::UNEXPECTED, first ^ second);
    }
    // A writable mapping of it is refused from here on, whoever asks.
    k2::expect_refusal(
        k2::domain_map(
            own_domain,
            seal_page,
            SUPER_SEAL_VADDR,
            0,
            1,
            right::MEMORY_READ | right::MEMORY_WRITE,
        ),
        status::STATE_CONFLICT,
    );

    // --- a barrier raised while calls are being admitted -------------------
    // The first call is waited for as a condition, bounded: the callers share
    // this supervisor's half-window of execution with the burners and the
    // writer, and under KVM, where every diagnostic record a burner writes
    // overruns its dispatch by milliseconds and leaves its scope in debt,
    // their first turn can come seconds after they were built. A barrier
    // raised before any call was admitted would have nothing to be measured
    // against, and the gate says so.
    let mut serviced = 0u64;
    while serviced < SERVICED_BEFORE_FENCE {
        let deadline = k2::now_ns()
            + if serviced == 0 {
                FIRST_CALL_NS
            } else {
                200_000_000
            };
        match k2::endpoint_receive(work_endpoint, deadline, false) {
            Ok((_, invocation)) => {
                let _ = k2::invocation_reply(invocation, serviced, b"ok");
                serviced += 1;
            }
            Err(_) => break,
        }
    }
    k2::note(report::REPLIED, serviced);
    if let Err(code) = k2::scope_fence(call_scope) {
        k2::note(report::UNEXPECTED, code as u64);
    }
    // Everything already queued is answered or cancelled; nothing new is
    // admitted. Draining is reported rather than assumed.
    drain_and_retire(call_scope, timer, stop);

    // --- the device is stopped underneath its driver -----------------------
    if device_present {
        stop_device(timer, stop);
    }

    drain_and_retire(burn_scope, timer, stop);
    drain_and_retire(mem_scope, timer, stop);
    if device_present {
        drain_and_retire(dev_scope, timer, stop);
    }

    k2::note(report::DONE, built);
    rt::exit(0)
}

/// Reads the first word of an object through a mediated read.
fn read_word(memory: u64) -> u64 {
    let mut bytes = [0u8; 8];
    match k2::memory_read(memory, 0, &mut bytes) {
        Ok(_) => u64::from_le_bytes(bytes),
        Err(_) => u64::MAX,
    }
}

/// Fences, watches and retires a scope, reporting what it was still holding.
///
/// Watched until quiescent or until a deadline, not for a count of polls. The
/// count was sixty-four twenty-millisecond waits, which held under TCG; under
/// KVM the spinning workers of the budget scope pay for their own diagnostic
/// notes out of a small budget, still had rounds to run when the sixty-fourth
/// poll came, and finished three milliseconds after the retirement was refused.
fn drain_and_retire(scope: u64, timer: u64, signal: u64) {
    let _ = k2::scope_fence(scope);
    let give_up = k2::now_ns() + DRAIN_DEADLINE_NS;
    while k2::now_ns() < give_up {
        let Ok(report_) = k2::scope_drain_status(scope) else {
            return;
        };
        k2::note(
            report::DRAIN,
            u64::from(report_.state) << 56
                | u64::from(report_.threads_running) << 40
                | u64::from(report_.invocations_pending) << 24
                | u64::from(report_.maps_pending) << 8,
        );
        if report_.state == scope_state::QUIESCENT {
            break;
        }
        let deadline = k2::now_ns() + 20_000_000;
        let _ = k2::timer_arm(timer, deadline);
        let _ = k2::signal_wait(signal, bit::STOP, deadline + 20_000_000);
    }
    let (outcome, report_) = k2::scope_retire(scope);
    match outcome {
        Ok(_) => k2::note(report::DRAIN, u64::from(report_.state) << 56),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
}

/// Builds the driver domain, if this machine has a device to give it.
fn build_device(scope: u64, supervision: u64, stop: u64, timer: u64, log: u64) -> bool {
    let _ = (stop, timer);
    let device = boot_handle(boot_slot::FIRST_DEVICE);
    let info = match k2::device_query(device) {
        Ok(info) => info,
        Err(_) => {
            // No device on this machine. Said once, plainly: the rest of the
            // run is the same run with one fewer thing demonstrated.
            k2::note(report::DEVICE_OBSERVED, 0);
            return false;
        }
    };
    k2::note(report::DEVICE_OBSERVED, u64::from(info.session));
    if info.dma_profile != dma_profile::WEAK_TRUSTED_DRIVER {
        k2::note(report::UNEXPECTED, u64::from(info.dma_profile));
    }

    let image = match image_named("k3driver") {
        Some(handle) => handle,
        None => {
            k2::note(report::BUILD_FAILED, 30);
            return false;
        }
    };
    let domain = match k2::scope_create_domain(scope, image, name16("k3driver")) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::BUILD_FAILED, code as u64);
            return false;
        }
    };
    let irq = match k2::scope_create_signal(scope) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::BUILD_FAILED, code as u64);
            return false;
        }
    };
    let done = match k2::scope_create_signal(scope) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::BUILD_FAILED, code as u64);
            return false;
        }
    };
    let ring = match k2::scope_create_memory(
        scope,
        k3::RING_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("ring"),
    ) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::BUILD_FAILED, code as u64);
            return false;
        }
    };

    // The register windows, mapped by the authority that holds the device, at
    // addresses the driver is told rather than chooses.
    for index in 0..(info.region_count as usize).min(k3::REGION_VADDR.len()) {
        match k2::device_map_region(
            device,
            domain,
            index as u32,
            k3::REGION_VADDR[index],
            info.session,
        ) {
            Ok(_) => k2::note(report::REGION_MAPPED, k3::REGION_VADDR[index]),
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                return false;
            }
        }
    }
    // A window this device does not have, and a session it is not in.
    k2::expect_refusal(
        k2::device_map_region(device, domain, 9, k3::REGION_VADDR[0], info.session),
        status::INVALID_ARGUMENT,
    );
    k2::expect_refusal(
        k2::device_map_region(device, domain, 0, k3::REGION_VADDR[0], info.session + 5),
        status::STATE_CONFLICT,
    );

    match k2::device_bind_irq(device, irq, bit::DEVICE, 0, info.session) {
        Ok(vector) => k2::note(report::IRQ_BOUND, vector),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return false;
        }
    }
    // One interrupt, one binding. Binding the same entry twice would leave two
    // owners for one vector.
    k2::expect_refusal(
        k2::device_bind_irq(device, irq, bit::DEVICE, 0, info.session),
        status::STATE_CONFLICT,
    );

    if k2::domain_map(
        domain,
        ring,
        k3::RING_VADDR,
        0,
        k3::RING_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .is_err()
    {
        return false;
    }

    // What the driver is given, and what it is not. Mapping, interrupts and DMA
    // are its business; bus mastering and reset are not, and the narrowing here
    // is the only thing that makes that true.
    let driver_rights = right::INSPECT | right::DEVICE_MAP | right::DEVICE_IRQ | right::DEVICE_DMA;
    if k2::domain_install_cap(domain, device, driver_slot::DEVICE, driver_rights, 0).is_err() {
        return false;
    }
    if k2::domain_install_cap(
        domain,
        irq,
        driver_slot::IRQ,
        right::INSPECT | right::SIGNAL_WAIT,
        0,
    )
    .is_err()
    {
        return false;
    }
    if k2::domain_install_cap(
        domain,
        ring,
        driver_slot::RING,
        right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    )
    .is_err()
    {
        return false;
    }
    if k2::domain_install_cap(
        domain,
        done,
        driver_slot::DONE,
        right::INSPECT | right::SIGNAL_RAISE,
        0,
    )
    .is_err()
    {
        return false;
    }
    if k2::domain_install_cap(
        domain,
        log,
        driver_slot::LOG,
        right::INSPECT | right::LOG_APPEND,
        0,
    )
    .is_err()
    {
        return false;
    }
    if k2::domain_set_fault_channel(domain, supervision).is_err() {
        return false;
    }
    if k2::domain_activate(domain).is_err() {
        return false;
    }
    // What the driver needed installed is installed. The supervisor keeps the
    // device, its interrupt and its completion signal, because it stops the
    // device later; the domain and the ring it no longer names.
    drop_handle(ring);

    // Bus mastering last, and by the authority that keeps it. Until this, the
    // device cannot issue a transaction whatever the driver writes.
    match k2::device_set_master(device, true, info.session) {
        Ok(_) => k2::note(report::BUILT, 100),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return false;
        }
    }
    // Remembered for the stop, which happens later and from here.
    // SAFETY: written once, before the driver can raise its signal, and read
    // only by this single-threaded supervisor afterwards.
    unsafe {
        DEVICE_HANDLE = device;
        DEVICE_IRQ = irq;
        DEVICE_DONE = done;
        DEVICE_SESSION = info.session;
        DEVICE_DOMAIN = domain;
    }
    true
}

static mut DEVICE_HANDLE: u64 = 0;
static mut DEVICE_IRQ: u64 = 0;
static mut DEVICE_DONE: u64 = 0;
static mut DEVICE_SESSION: u32 = 0;
static mut DEVICE_DOMAIN: u64 = 0;

/// Waits for the driver to finish, then stops the device underneath it.
fn stop_device(timer: u64, stop: u64) {
    // SAFETY: written once during the build, before the driver existed.
    let (device, irq, done, session, domain) = unsafe {
        (
            DEVICE_HANDLE,
            DEVICE_IRQ,
            DEVICE_DONE,
            DEVICE_SESSION,
            DEVICE_DOMAIN,
        )
    };
    let deadline = k2::now_ns() + 4_000_000_000;
    match k2::signal_wait(done, bit::DRIVER_DONE, deadline) {
        Ok(_) => {}
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // Bus mastering off first: the driver must not be able to issue anything
    // between the decision to stop and the confirmation that it stopped.
    match k2::device_set_master(device, false, session) {
        Ok(_) => {}
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
    // One window taken back by name, from a domain still executing, before the
    // reset takes the rest. A withdrawal of a device register window is a
    // translation withdrawal like any other: it does not answer until every
    // processor has retired it.
    match k2::device_unmap_region(device, domain, k3::REGION_VADDR[3], session) {
        Ok(_) => k2::note(report::REGION_MAPPED, 0),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
    match k2::device_reset(device) {
        Ok(after) => {
            k2::note(report::DEVICE_RESET, u64::from(after.session));
            if after.dma_grants != 0 || after.irq_bound != 0 {
                k2::note(report::UNEXPECTED, u64::from(after.dma_grants));
            }
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
    // The driver is told to try what it remembers. Everything it names belongs
    // to a session that no longer exists.
    let _ = k2::signal_raise(irq, bit::STALE);
    let deadline = k2::now_ns() + 500_000_000;
    let _ = k2::timer_arm(timer, deadline);
    let _ = k2::signal_wait(stop, bit::STOP, deadline + 100_000_000);
}

rt::entry!(run);
