//! Legacy 8259 interrupt controllers.
//!
//! K1 routes no interrupt through them; it uses the local APIC timer. They are
//! still reprogrammed rather than ignored, because firmware leaves them mapped
//! over the CPU exception vectors, and a spurious legacy interrupt arriving
//! there would be delivered as a fault with an invented error code.

use super::cpu::outb;

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// Vector base the primary controller is remapped to.
pub const REMAP_BASE_PRIMARY: u8 = 0x20;
/// Vector base the secondary controller is remapped to.
pub const REMAP_BASE_SECONDARY: u8 = 0x28;

/// Remaps both controllers away from the exception vectors and masks every
/// line.
///
/// # Safety
///
/// Must run during bootstrap with interrupts masked, before any interrupt
/// source is enabled.
pub unsafe fn remap_and_mask() {
    // SAFETY: bootstrap path; the ports belong to the kernel.
    unsafe {
        // ICW1: begin initialisation, expect ICW4.
        outb(PIC1_COMMAND, 0x11);
        outb(PIC2_COMMAND, 0x11);
        // ICW2: vector offsets.
        outb(PIC1_DATA, REMAP_BASE_PRIMARY);
        outb(PIC2_DATA, REMAP_BASE_SECONDARY);
        // ICW3: cascade wiring on IRQ2.
        outb(PIC1_DATA, 0x04);
        outb(PIC2_DATA, 0x02);
        // ICW4: 8086 mode.
        outb(PIC1_DATA, 0x01);
        outb(PIC2_DATA, 0x01);
        // Mask every line.
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}
