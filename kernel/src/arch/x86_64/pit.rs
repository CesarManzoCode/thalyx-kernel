//! Intel 8254 programmable interval timer, used only as a frequency reference.
//!
//! The PIT is not a source of interrupts here: its channel 0 output drives IRQ0,
//! which stays masked at the legacy interrupt controller. It is read by latching
//! the counter, so the measurement needs nothing but ports 0x40 and 0x43 and
//! does not depend on the speaker gate at port 0x61 being implemented.

use super::cpu::{inb, outb};

/// Input frequency of the 8254 in hertz.
pub const FREQUENCY: u64 = 1_193_182;

const CHANNEL0_DATA: u16 = 0x40;
const COMMAND: u16 = 0x43;

/// Starts channel 0 counting down from 0xFFFF in mode 0.
///
/// # Safety
///
/// The caller must own the PIT and must have masked IRQ0 at the interrupt
/// controller, because channel 0's output is wired to it.
pub unsafe fn start_reference() {
    // SAFETY: the caller guarantees ownership of the device.
    unsafe {
        // Channel 0, access low then high byte, mode 0, binary counting.
        outb(COMMAND, 0x30);
        outb(CHANNEL0_DATA, 0xFF);
        outb(CHANNEL0_DATA, 0xFF);
    }
}

/// Reads the current channel 0 counter by latching it.
///
/// # Safety
///
/// [`start_reference`] must have run and nothing else may be reprogramming the
/// channel.
pub unsafe fn read_counter() -> u16 {
    // SAFETY: the caller guarantees the channel is ours and running.
    unsafe {
        outb(COMMAND, 0x00);
        let low = inb(CHANNEL0_DATA);
        let high = inb(CHANNEL0_DATA);
        (u16::from(high) << 8) | u16::from(low)
    }
}

/// Ticks elapsed since [`start_reference`], valid until the counter wraps past
/// 0xFFFF, which takes about 55 ms.
///
/// # Safety
///
/// See [`read_counter`].
pub unsafe fn elapsed_ticks() -> u16 {
    // SAFETY: the caller guarantees the channel is ours and running.
    0xFFFFu16.wrapping_sub(unsafe { read_counter() })
}
