//! Polled 16550-compatible UART, the only output device K1 uses.
//!
//! Output is polled and synchronous on purpose: the diagnostic plane has to
//! survive a fault handler and a panic, where an interrupt-driven driver with a
//! queue would be the thing least likely to still work.
//!
//! Polled does not mean one poll per byte. A 16550A with its FIFO enabled
//! reports an empty transmit holding register only when the whole transmit
//! FIFO is empty, so after one successful poll it accepts a FIFO's worth of
//! bytes without asking again. Under KVM every port access is an exit to the
//! emulator, and a status read per byte was half of them: [`Uart::write_burst`]
//! reads the status once per FIFO's worth. A port whose FIFO is not confirmed
//! at [`Uart::init`] keeps the one-byte depth, which is the old behaviour.

use super::cpu::{inb, outb};

/// I/O port of the first PC-compatible serial port.
pub const COM1: u16 = 0x3F8;

const REG_DATA: u16 = 0;
const REG_INTERRUPT_ENABLE: u16 = 1;
const REG_DIVISOR_LOW: u16 = 0;
const REG_DIVISOR_HIGH: u16 = 1;
const REG_FIFO_CONTROL: u16 = 2;
/// Read side of offset 2: the interrupt identification register.
const REG_INTERRUPT_ID: u16 = 2;
const REG_LINE_CONTROL: u16 = 3;
const REG_MODEM_CONTROL: u16 = 4;
const REG_LINE_STATUS: u16 = 5;

const LINE_STATUS_THR_EMPTY: u8 = 1 << 5;
const LINE_CONTROL_DLAB: u8 = 1 << 7;
const LINE_CONTROL_8N1: u8 = 0b11;
/// Both bits set in IIR: the FIFOs are enabled and usable (16550A and later).
const INTERRUPT_ID_FIFO_ENABLED: u8 = 0b1100_0000;

/// Transmit FIFO depth of a 16550A.
const FIFO_DEPTH_16550A: u8 = 16;

/// Bound on the poll loop, so a port that never reports an empty transmit
/// holding register drops the byte instead of wedging the machine.
const POLL_LIMIT: u32 = 100_000;

/// A polled UART at a fixed I/O port.
#[derive(Clone, Copy)]
pub struct Uart {
    base: u16,
    /// Bytes the transmitter accepts after one empty-register poll: the
    /// FIFO depth when [`Uart::init`] confirmed the FIFO, one otherwise.
    burst: u8,
}

impl Uart {
    /// Binds to the UART at `base`. Nothing is programmed until [`Uart::init`].
    #[must_use]
    pub const fn new(base: u16) -> Self {
        Self { base, burst: 1 }
    }

    /// Bytes written per empty-register poll: 16 with a confirmed FIFO, 1
    /// without. The diagnostic summary states it.
    #[must_use]
    pub const fn burst(&self) -> u8 {
        self.burst
    }

    /// Programs 115200 baud, 8N1, FIFOs enabled, interrupts disabled, and
    /// reads back whether the FIFOs really are enabled.
    ///
    /// # Safety
    ///
    /// `base` must be a 16550-compatible UART and no other agent may be driving
    /// it concurrently. K1 owns the port from the loader's first byte onward.
    pub unsafe fn init(&mut self) {
        // SAFETY: the caller guarantees exclusive ownership of the port block.
        unsafe {
            outb(self.base + REG_INTERRUPT_ENABLE, 0x00);
            outb(self.base + REG_LINE_CONTROL, LINE_CONTROL_DLAB);
            outb(self.base + REG_DIVISOR_LOW, 0x01);
            outb(self.base + REG_DIVISOR_HIGH, 0x00);
            outb(self.base + REG_LINE_CONTROL, LINE_CONTROL_8N1);
            outb(self.base + REG_FIFO_CONTROL, 0xC7);
            outb(self.base + REG_MODEM_CONTROL, 0x03);
            // An 8250 or a 16550 without working FIFOs leaves these bits
            // clear; filling a FIFO it does not have would overwrite bytes.
            let fifo = inb(self.base + REG_INTERRUPT_ID) & INTERRUPT_ID_FIFO_ENABLED
                == INTERRUPT_ID_FIFO_ENABLED;
            self.burst = if fifo { FIFO_DEPTH_16550A } else { 1 };
        }
    }

    /// Spins until the transmit holding register (with the FIFO enabled, the
    /// whole transmit FIFO) is empty. False if the poll bound expired.
    fn wait_transmit_empty(&self) -> bool {
        let mut spins = 0u32;
        // SAFETY: `init` established this port; reading LSR has no side
        // effect on the transmitter.
        while unsafe { inb(self.base + REG_LINE_STATUS) } & LINE_STATUS_THR_EMPTY == 0 {
            spins += 1;
            if spins >= POLL_LIMIT {
                return false;
            }
            core::hint::spin_loop();
        }
        true
    }

    /// Writes bytes exactly as given, with no newline expansion, polling the
    /// line status once per [`Uart::burst`] bytes. A port that never empties
    /// drops the rest of `bytes` instead of wedging the machine.
    pub fn write_burst(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(usize::from(self.burst.max(1))) {
            if !self.wait_transmit_empty() {
                return;
            }
            for &byte in chunk {
                // SAFETY: the transmitter reported room for a whole chunk.
                unsafe { outb(self.base + REG_DATA, byte) };
            }
        }
    }

    /// Writes one byte, spinning until the transmitter is ready or the poll
    /// bound expires. The panic path uses this; it assumes nothing about the
    /// FIFO.
    pub fn write_byte(&self, byte: u8) {
        if self.wait_transmit_empty() {
            // SAFETY: `init` established this port and the transmitter is
            // ready; writing the data register is the transmit sequence.
            unsafe { outb(self.base + REG_DATA, byte) };
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
