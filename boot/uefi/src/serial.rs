//! Polled 16550 UART for loader diagnostics.
//!
//! The loader duplicates this rather than sharing it with the kernel. The two
//! are separate binaries for separate targets, and the shared crate between
//! them is the hand-off contract; putting a device driver in that contract
//! would make the loader and the kernel share code that neither's boundary
//! calls for. Thirty lines of duplication is cheaper than that coupling.

use core::arch::asm;

/// I/O port of the first PC-compatible serial port.
pub const COM1: u16 = 0x3F8;

unsafe fn outb(port: u16, value: u8) {
    // SAFETY: the caller owns the port.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags))
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller owns the port.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags))
    }
    value
}

/// Programs the port for 115200 8N1 with interrupts disabled.
///
/// # Safety
///
/// `COM1` must be a 16550-compatible UART. Firmware may also be writing to it;
/// the only consequence is interleaved output, which the record prefix makes
/// unambiguous.
pub unsafe fn init() {
    // SAFETY: see above.
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x01);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x03);
    }
}

/// Writes one byte, giving up after a bounded number of polls.
pub fn write_byte(byte: u8) {
    // SAFETY: `init` established the port.
    unsafe {
        let mut spins = 0u32;
        while inb(COM1 + 5) & (1 << 5) == 0 {
            spins += 1;
            if spins >= 100_000 {
                return;
            }
            core::hint::spin_loop();
        }
        outb(COM1, byte);
    }
}

/// Sink implementing [`core::fmt::Write`] over the port.
pub struct Port;

impl core::fmt::Write for Port {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                write_byte(b'\r');
            }
            write_byte(byte);
        }
        Ok(())
    }
}
