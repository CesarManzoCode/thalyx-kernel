//! Polled 16550-compatible UART, the only output device K1 uses.
//!
//! Output is polled and synchronous on purpose: the diagnostic plane has to
//! survive a fault handler and a panic, where an interrupt-driven driver with a
//! queue would be the thing least likely to still work.

use super::cpu::{inb, outb};

/// I/O port of the first PC-compatible serial port.
pub const COM1: u16 = 0x3F8;

const REG_DATA: u16 = 0;
const REG_INTERRUPT_ENABLE: u16 = 1;
const REG_DIVISOR_LOW: u16 = 0;
const REG_DIVISOR_HIGH: u16 = 1;
const REG_FIFO_CONTROL: u16 = 2;
const REG_LINE_CONTROL: u16 = 3;
const REG_MODEM_CONTROL: u16 = 4;
const REG_LINE_STATUS: u16 = 5;

const LINE_STATUS_THR_EMPTY: u8 = 1 << 5;
const LINE_CONTROL_DLAB: u8 = 1 << 7;
const LINE_CONTROL_8N1: u8 = 0b11;

/// Bound on the poll loop, so a port that never reports an empty transmit
/// holding register drops the byte instead of wedging the machine.
const POLL_LIMIT: u32 = 100_000;

/// A polled UART at a fixed I/O port.
#[derive(Clone, Copy)]
pub struct Uart {
    base: u16,
}

impl Uart {
    /// Binds to the UART at `base`. Nothing is programmed until [`Uart::init`].
    #[must_use]
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    /// Programs 115200 baud, 8N1, FIFOs enabled, interrupts disabled.
    ///
    /// # Safety
    ///
    /// `base` must be a 16550-compatible UART and no other agent may be driving
    /// it concurrently. K1 owns the port from the loader's first byte onward.
    pub unsafe fn init(&self) {
        // SAFETY: the caller guarantees exclusive ownership of the port block.
        unsafe {
            outb(self.base + REG_INTERRUPT_ENABLE, 0x00);
            outb(self.base + REG_LINE_CONTROL, LINE_CONTROL_DLAB);
            outb(self.base + REG_DIVISOR_LOW, 0x01);
            outb(self.base + REG_DIVISOR_HIGH, 0x00);
            outb(self.base + REG_LINE_CONTROL, LINE_CONTROL_8N1);
            outb(self.base + REG_FIFO_CONTROL, 0xC7);
            outb(self.base + REG_MODEM_CONTROL, 0x03);
        }
    }

    /// Writes one byte, spinning until the transmitter is ready or the poll
    /// bound expires.
    pub fn write_byte(&self, byte: u8) {
        let mut spins = 0u32;
        // SAFETY: `init` established this port; reading LSR and writing the
        // data register are the documented transmit sequence.
        unsafe {
            while inb(self.base + REG_LINE_STATUS) & LINE_STATUS_THR_EMPTY == 0 {
                spins += 1;
                if spins >= POLL_LIMIT {
                    return;
                }
                core::hint::spin_loop();
            }
            outb(self.base + REG_DATA, byte);
        }
    }

    /// Writes a string, expanding newlines to CRLF for terminal capture.
    pub fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
    }
}

impl core::fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        Uart::write_str(self, s);
        Ok(())
    }
}
