//! Local APIC: the interrupt controller and the preemption timer.
//!
//! The timer is programmed in periodic mode and its frequency is *measured*
//! against the PIT rather than assumed, because the local APIC timer runs off a
//! bus frequency that neither the architecture nor the emulator guarantees.
//! The same measurement window yields the TSC frequency, so both clocks come
//! from one reference.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use super::cpu;
use super::pit;

const REG_ID: usize = 0x020;
const REG_VERSION: usize = 0x030;
const REG_EOI: usize = 0x0B0;
const REG_SPURIOUS: usize = 0x0F0;
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

/// A mapped local APIC.
pub struct Lapic {
    base: u64,
}

/// Puts the local APIC into xAPIC mode and reports its physical register base.
///
/// Firmware may have left the CPU in x2APIC mode, where the registers are MSRs
/// and the memory window is gone. Going from x2APIC straight to xAPIC raises
/// #GP, so the transition goes through the disabled state.
///
/// # Safety
///
/// Must run during bootstrap with interrupts masked. Disabling the APIC
/// discards any pending interrupt state, which is only acceptable because no
/// interrupt source has been enabled yet.
pub unsafe fn enable_xapic() -> u64 {
    // SAFETY: bootstrap path; `IA32_APIC_BASE` exists because the feature gate
    // confirmed a local APIC.
    unsafe {
        let mut value = cpu::rdmsr(cpu::MSR_APIC_BASE);
        if value & APIC_BASE_X2APIC != 0 {
            cpu::wrmsr(
                cpu::MSR_APIC_BASE,
                value & !(APIC_BASE_X2APIC | APIC_BASE_ENABLE),
            );
            value &= !APIC_BASE_X2APIC;
        }
        cpu::wrmsr(cpu::MSR_APIC_BASE, value | APIC_BASE_ENABLE);
        cpu::rdmsr(cpu::MSR_APIC_BASE) & APIC_BASE_ADDRESS_MASK
    }
}

impl Lapic {
    /// Binds to the register page already mapped at `base`.
    ///
    /// # Safety
    ///
    /// `base` must be the virtual address of the local APIC register page,
    /// mapped uncacheable and writable in the current address space.
    pub const unsafe fn new(base: u64) -> Self {
        Self { base }
    }

    fn read(&self, register: usize) -> u32 {
        // SAFETY: `base` is the mapped register page and `register` is one of
        // the constants above, all inside the 4 KiB window.
        unsafe { read_volatile((self.base + register as u64) as *const u32) }
    }

    fn write(&self, register: usize, value: u32) {
        // SAFETY: as `read`. Writes to these registers are the documented way
        // to configure the controller.
        unsafe { write_volatile((self.base + register as u64) as *mut u32, value) }
    }

    /// Local APIC identifier.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.read(REG_ID) >> 24
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

    /// Measures the timer and TSC frequencies over a window of
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

/// Records the virtual address of the mapped register page so interrupt
/// handlers can reach the controller without threading it through every call.
pub fn install(base: u64) {
    BASE.store(base, Ordering::Release);
}

/// The installed local APIC, if one has been mapped.
#[must_use]
pub fn current() -> Option<Lapic> {
    let base = BASE.load(Ordering::Acquire);
    if base == 0 {
        None
    } else {
        // SAFETY: `install` is called only with the virtual address of the
        // register page after it has been mapped uncacheable and writable.
        Some(unsafe { Lapic::new(base) })
    }
}
