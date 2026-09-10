//! Local APIC: the interrupt controller, the preemption timer and the way one
//! processor reaches another.
//!
//! There are two register interfaces and they are not two spellings of one
//! thing. In xAPIC mode the registers are a page of uncacheable memory; in
//! x2APIC mode they are model-specific registers, the interrupt command
//! register is a single 64-bit write with a 32-bit destination, and it has no
//! delivery-status bit to poll. The difference matters twice: a request
//! published in ordinary memory is ordered before an uncacheable store by the
//! processor's own rules, but `WRMSR` to an x2APIC register is not serialising,
//! so the x2APIC path fences explicitly before announcing anything. Sending an
//! interrupt is not evidence that the receiver acted on it either way; that is
//! what the acknowledgements in `crate::tlb` are for.
//!
//! The timer frequency is *measured* rather than assumed, because the local
//! APIC timer runs off a bus frequency neither the architecture nor the
//! emulator guarantees. The bootstrap processor measures against the PIT, which
//! also yields the time-stamp counter frequency; every other processor measures
//! its own timer against that counter, so no processor inherits a number it did
//! not observe.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use super::cpu;
use super::pit;

const REG_ID: usize = 0x020;
const REG_VERSION: usize = 0x030;
const REG_EOI: usize = 0x0B0;
const REG_SPURIOUS: usize = 0x0F0;
const REG_ICR_LOW: usize = 0x300;
const REG_ICR_HIGH: usize = 0x310;
const REG_LVT_TIMER: usize = 0x320;
const REG_LVT_LINT0: usize = 0x350;
const REG_LVT_LINT1: usize = 0x360;
const REG_LVT_ERROR: usize = 0x370;
const REG_TIMER_INITIAL: usize = 0x380;
const REG_TIMER_CURRENT: usize = 0x390;
const REG_TIMER_DIVIDE: usize = 0x3E0;

const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const SPURIOUS_ENABLE: u32 = 1 << 8;
/// Divide configuration value selecting divide-by-16.
const DIVIDE_BY_16: u32 = 0b0011;

const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Delivery mode: fixed vector.
const ICR_FIXED: u32 = 0b000 << 8;
/// Delivery mode: INIT.
const ICR_INIT: u32 = 0b101 << 8;
/// Delivery mode: start-up.
const ICR_STARTUP: u32 = 0b110 << 8;
/// Level bit, set for everything except an INIT de-assert.
const ICR_ASSERT: u32 = 1 << 14;
/// Trigger mode: level.
const ICR_LEVEL: u32 = 1 << 15;
/// Delivery status, xAPIC only: set while the interrupt is still being sent.
const ICR_DELIVERY_PENDING: u32 = 1 << 12;

/// Which register interface this machine uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    /// Memory-mapped registers at a fixed physical page.
    XApic,
    /// Model-specific registers with a 32-bit destination field.
    X2Apic,
}

impl Backend {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Backend::XApic => "xapic",
            Backend::X2Apic => "x2apic",
        }
    }
}

/// Result of the frequency measurement.
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    /// Local APIC timer ticks per second after the divider.
    pub lapic_hz: u64,
    /// Time-stamp counter ticks per second.
    pub tsc_hz: u64,
    /// Nanoseconds the measurement window actually covered.
    pub window_ns: u64,
}

/// A local APIC, reachable through whichever interface this machine uses.
#[derive(Clone, Copy)]
pub struct Lapic {
    backend: Backend,
    base: u64,
}

/// Chooses the register interface and puts the local APIC of the calling
/// processor into it.
///
/// Firmware may have left the processor in x2APIC mode, where the memory window
/// is gone. Going from x2APIC straight to xAPIC raises a general-protection
/// fault, so a transition in that direction goes through the disabled state.
/// Reaching x2APIC from xAPIC needs no such detour.
///
/// # Safety
///
/// Must run at CPL 0 with interrupts masked and with no interrupt source of
/// this processor enabled: disabling the controller discards pending state.
pub unsafe fn enable_local(want_x2apic: bool) -> u64 {
    // SAFETY: `IA32_APIC_BASE` exists because the feature gate confirmed a
    // local APIC, and the caller guarantees the machine state.
    unsafe {
        let mut value = cpu::rdmsr(cpu::MSR_APIC_BASE);
        if value & APIC_BASE_X2APIC != 0 && !want_x2apic {
            cpu::wrmsr(
                cpu::MSR_APIC_BASE,
                value & !(APIC_BASE_X2APIC | APIC_BASE_ENABLE),
            );
            value &= !APIC_BASE_X2APIC;
        }
        let mut wanted = value | APIC_BASE_ENABLE;
        if want_x2apic {
            wanted |= APIC_BASE_X2APIC;
        }
        cpu::wrmsr(cpu::MSR_APIC_BASE, wanted);
        cpu::rdmsr(cpu::MSR_APIC_BASE) & APIC_BASE_ADDRESS_MASK
    }
}

impl Lapic {
    /// Binds to the register interface `backend`, whose memory window, if it
    /// has one, is mapped at `base`.
    ///
    /// # Safety
    ///
    /// For [`Backend::XApic`], `base` must be the virtual address of the local
    /// APIC register page, mapped uncacheable and writable in the current
    /// address space. For [`Backend::X2Apic`], the processor must already be in
    /// x2APIC mode.
    pub const unsafe fn new(backend: Backend, base: u64) -> Self {
        Self { backend, base }
    }

    /// Which interface this handle uses.
    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    fn read(&self, register: usize) -> u32 {
        match self.backend {
            // SAFETY: `base` is the mapped register page and `register` is one
            // of the constants above, all inside the 4 KiB window.
            Backend::XApic => unsafe { read_volatile((self.base + register as u64) as *const u32) },
            // SAFETY: the processor is in x2APIC mode, where every register
            // above exists as the model-specific register at this index.
            Backend::X2Apic => unsafe {
                cpu::rdmsr(cpu::MSR_X2APIC_BASE + (register as u32 >> 4)) as u32
            },
        }
    }

    fn write(&self, register: usize, value: u32) {
        match self.backend {
            // SAFETY: as `read`. Writes to these registers are the documented
            // way to configure the controller.
            Backend::XApic => unsafe {
                write_volatile((self.base + register as u64) as *mut u32, value);
            },
            // SAFETY: as `read`.
            Backend::X2Apic => unsafe {
                cpu::wrmsr(
                    cpu::MSR_X2APIC_BASE + (register as u32 >> 4),
                    u64::from(value),
                );
            },
        }
    }

    /// Local APIC identifier of the processor executing this call.
    ///
    /// The two interfaces report it differently: xAPIC keeps an eight-bit
    /// identifier in the top byte of a 32-bit register, x2APIC keeps a full
    /// 32-bit identifier. Shifting the second would silently rename every
    /// processor above 255.
    #[must_use]
    pub fn id(&self) -> u32 {
        match self.backend {
            Backend::XApic => self.read(REG_ID) >> 24,
            Backend::X2Apic => self.read(REG_ID),
        }
    }

    /// Local APIC version register.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.read(REG_VERSION)
    }

    /// Enables the controller, masks the local interrupt pins and leaves the
    /// timer stopped.
    pub fn configure(&self, spurious_vector: u8) {
        self.write(REG_SPURIOUS, SPURIOUS_ENABLE | u32::from(spurious_vector));
        self.write(REG_LVT_LINT0, LVT_MASKED);
        self.write(REG_LVT_LINT1, LVT_MASKED);
        self.write(REG_LVT_ERROR, LVT_MASKED);
        self.write(REG_LVT_TIMER, LVT_MASKED);
        self.write(REG_TIMER_INITIAL, 0);
        self.write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
    }

    /// Waits until the previous interrupt command has been accepted.
    ///
    /// Only xAPIC has a delivery-status bit. In x2APIC mode the write itself is
    /// the completion, so there is nothing to poll and nothing to pretend to
    /// poll.
    fn wait_for_delivery(&self) {
        if self.backend != Backend::XApic {
            return;
        }
        for _ in 0..1_000_000u32 {
            if self.read(REG_ICR_LOW) & ICR_DELIVERY_PENDING == 0 {
                return;
            }
            core::hint::spin_loop();
        }
    }

    /// Writes the interrupt command register.
    ///
    /// Everything the caller wants the receiver to see must already be in
    /// memory when this runs. On the x2APIC path that is not automatic: the
    /// fence is what orders the publishing stores before a `WRMSR` that is not
    /// itself serialising.
    fn send_command(&self, destination: u32, low: u32) {
        match self.backend {
            Backend::XApic => {
                self.wait_for_delivery();
                self.write(REG_ICR_HIGH, destination << 24);
                self.write(REG_ICR_LOW, low);
                self.wait_for_delivery();
            }
            Backend::X2Apic => {
                cpu::fence_before_wrmsr();
                let value = (u64::from(destination) << 32) | u64::from(low);
                // SAFETY: the processor is in x2APIC mode, where the interrupt
                // command register is this single 64-bit model-specific
                // register.
                unsafe { cpu::wrmsr(cpu::MSR_X2APIC_BASE + (REG_ICR_LOW as u32 >> 4), value) };
            }
        }
    }

    /// Sends a fixed-vector interrupt to one processor.
    pub fn send_fixed(&self, destination: u32, vector: u8) {
        self.send_command(destination, ICR_FIXED | ICR_ASSERT | u32::from(vector));
    }

    /// Sends the INIT assert/de-assert pair that resets a processor into its
    /// wait-for-start-up state.
    pub fn send_init(&self, destination: u32) {
        self.send_command(destination, ICR_INIT | ICR_ASSERT);
        self.send_command(destination, ICR_INIT | ICR_LEVEL);
    }

    /// Sends a start-up interrupt naming the page the target begins executing.
    pub fn send_startup(&self, destination: u32, page: u8) {
        self.send_command(destination, ICR_STARTUP | ICR_ASSERT | u32::from(page));
    }

    /// Measures the timer and time-stamp counter frequencies over a window of
    /// `window_pit_ticks` PIT ticks.
    ///
    /// # Safety
    ///
    /// The PIT must be owned by the caller and IRQ0 must be masked. Interrupts
    /// must be masked for the whole window, otherwise the measurement includes
    /// time spent elsewhere.
    pub unsafe fn calibrate(&self, window_pit_ticks: u16) -> Calibration {
        self.write(REG_LVT_TIMER, LVT_MASKED);
        self.write(REG_TIMER_DIVIDE, DIVIDE_BY_16);

        // SAFETY: the caller guarantees ownership of the PIT and masked
        // interrupts.
        unsafe {
            pit::start_reference();
            self.write(REG_TIMER_INITIAL, u32::MAX);
            let tsc_start = cpu::rdtsc();

            while pit::elapsed_ticks() < window_pit_ticks {
                core::hint::spin_loop();
            }

            let lapic_remaining = self.read(REG_TIMER_CURRENT);
            let tsc_end = cpu::rdtsc();
            let pit_ticks = u64::from(pit::elapsed_ticks());
            self.write(REG_TIMER_INITIAL, 0);

            let window_ns = pit_ticks * 1_000_000_000 / pit::FREQUENCY;
            let lapic_ticks = u64::from(u32::MAX - lapic_remaining);
            let tsc_ticks = tsc_end.wrapping_sub(tsc_start);

            Calibration {
                lapic_hz: if window_ns == 0 {
                    0
                } else {
                    lapic_ticks * 1_000_000_000 / window_ns
                },
                tsc_hz: if window_ns == 0 {
                    0
                } else {
                    tsc_ticks * 1_000_000_000 / window_ns
                },
                window_ns,
            }
        }
    }

    /// Measures this processor's timer against the already-established
    /// time-stamp counter, over a window of `window_ns` nanoseconds.
    ///
    /// An application processor cannot take the PIT: it is a single device the
    /// bootstrap processor already used, and two processors driving it at once
    /// would measure each other. Measuring against the counter costs nothing
    /// and keeps the number this processor uses one it observed itself.
    pub fn calibrate_against_tsc(&self, tsc_hz: u64, window_ns: u64) -> u64 {
        if tsc_hz == 0 || window_ns == 0 {
            return 0;
        }
        self.write(REG_LVT_TIMER, LVT_MASKED);
        self.write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
        let ticks = (u128::from(tsc_hz) * u128::from(window_ns) / 1_000_000_000u128) as u64;
        self.write(REG_TIMER_INITIAL, u32::MAX);
        let start = cpu::rdtsc();
        while cpu::rdtsc().wrapping_sub(start) < ticks {
            core::hint::spin_loop();
        }
        let remaining = self.read(REG_TIMER_CURRENT);
        let elapsed = cpu::rdtsc().wrapping_sub(start);
        self.write(REG_TIMER_INITIAL, 0);
        if elapsed == 0 {
            return 0;
        }
        let lapic_ticks = u64::from(u32::MAX - remaining);
        ((u128::from(lapic_ticks) * u128::from(tsc_hz)) / u128::from(elapsed)) as u64
    }

    /// Starts the periodic timer.
    pub fn start_periodic(&self, vector: u8, initial_count: u32) {
        self.write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
        self.write(REG_LVT_TIMER, LVT_PERIODIC | u32::from(vector));
        self.write(REG_TIMER_INITIAL, initial_count);
    }

    /// Signals end of interrupt.
    pub fn end_of_interrupt(&self) {
        self.write(REG_EOI, 0);
    }
}

static BASE: AtomicU64 = AtomicU64::new(0);
/// Zero until a backend is installed, then one plus its discriminant, so a
/// single atomic answers both "is there one" and "which".
static BACKEND: AtomicU8 = AtomicU8::new(0);

/// Records the register interface, and for xAPIC the virtual address of the
/// mapped register page, so interrupt handlers on any processor can reach their
/// own controller without threading it through every call.
pub fn install(backend: Backend, base: u64) {
    BASE.store(base, Ordering::Relaxed);
    BACKEND.store(
        match backend {
            Backend::XApic => 1,
            Backend::X2Apic => 2,
        },
        Ordering::Release,
    );
}

/// The calling processor's local APIC, if a backend has been installed.
#[must_use]
pub fn current() -> Option<Lapic> {
    let backend = match BACKEND.load(Ordering::Acquire) {
        1 => Backend::XApic,
        2 => Backend::X2Apic,
        _ => return None,
    };
    let base = BASE.load(Ordering::Relaxed);
    if backend == Backend::XApic && base == 0 {
        return None;
    }
    // SAFETY: `install` is called only with a backend this processor is in and,
    // for xAPIC, the virtual address of the register page after it has been
    // mapped uncacheable and writable in the shared kernel half.
    Some(unsafe { Lapic::new(backend, base) })
}

/// The identifier of the processor executing this call, read from its own
/// controller rather than from any per-processor base register.
#[must_use]
pub fn local_apic_id() -> u32 {
    current().map_or(u32::MAX, |lapic| lapic.id())
}
