//! The service's side of the block protocol.
//!
//! Everything durable the service does goes through here, and nothing here
//! decides anything: the caller says which block, which buffer and, when the
//! harness asked for one, which fault to apply to this particular write. That
//! separation is deliberate. The service knows where it is in its own protocol
//! and therefore which write is "the commit"; the driver knows how to make a
//! write not happen. Neither knows both.
//!
//! The shared buffer is a capability the supervisor gave to both domains and to
//! the device. The service writes bytes into it and names an offset; it never
//! sees a device address, and there is no arithmetic here that turns one of its
//! own pointers into something the device reads.

use thalyx_user_k4fmt::pkg::{
    DiskReply, DiskRequest, IOBUF_PAGES, IOBUF_VADDR, disk_op, disk_status, note,
};
use thalyx_user_k4fmt::{BLOCK, Pod, fault_mode};
use thalyx_user_rt::k2;

/// How long the service waits for the driver to answer one request.
const CALL_DEADLINE_NS: u64 = 5_000_000_000;

/// A handle on the block device, and what it has been made to do.
pub struct Disk {
    endpoint: u64,
    /// Set once a request came back as a medium failure. A service that kept
    /// publishing after this would be publishing on a state it only assumes.
    pub failed: bool,
    /// Set once the driver said nothing further reaches the medium.
    pub latched: bool,
    /// Set when the call itself could not be made -- the kernel refused it, or
    /// the driver never answered. That is not a medium failure and must not be
    /// reported as one: nothing is known about the block either way, and the
    /// difference between "the medium said no" and "I could not ask" is the
    /// difference between a store that is damaged and a service that is not
    /// available.
    pub unreachable: bool,
    pub writes_issued: u64,
    pub writes_suppressed: u64,
    pub flushes: u64,
    pub reads: u64,
}

impl Disk {
    pub fn new(endpoint: u64) -> Self {
        Self {
            endpoint,
            failed: false,
            latched: false,
            unreachable: false,
            writes_issued: 0,
            writes_suppressed: 0,
            flushes: 0,
            reads: 0,
        }
    }

    fn call(&mut self, request: &DiskRequest) -> Option<DiskReply> {
        let result = match k2::endpoint_call(
            self.endpoint,
            0,
            request.as_bytes(),
            &[],
            k2::now_ns() + CALL_DEADLINE_NS,
            false,
        ) {
            Ok(result) => result,
            Err(code) => {
                self.unreachable = true;
                k2::note(note::DISK_UNREACHABLE, (-code) as u64);
                return None;
            }
        };
        let Some(reply) = DiskReply::read_from(&result.payload, 0) else {
            self.unreachable = true;
            return None;
        };
        self.writes_issued = reply.writes_issued;
        self.writes_suppressed = reply.writes_suppressed;
        self.flushes = reply.flushes;
        if reply.status == disk_status::LATCHED {
            self.latched = true;
        }
        if reply.status == disk_status::IO_ERROR {
            self.failed = true;
        }
        Some(reply)
    }

    /// Reads one block into buffer slot `slot`.
    pub fn read(&mut self, block: u64, slot: u32) -> bool {
        let request = DiskRequest {
            op: disk_op::READ,
            fault_mode: fault_mode::NONE,
            block,
            count: 1,
            buffer_block: slot,
            fault_arg: 0,
        };
        match self.call(&request) {
            Some(reply) if reply.status == disk_status::OK => {
                self.reads += 1;
                true
            }
            _ => false,
        }
    }

    /// Writes one block out of buffer slot `slot`, applying `mode` to it.
    ///
    /// Returns false only when the medium refused. A write the harness dropped
    /// returns true, because that is the whole point: the service is supposed
    /// to carry on believing it happened.
    pub fn write(&mut self, block: u64, slot: u32, mode: u32) -> bool {
        let request = DiskRequest {
            op: disk_op::WRITE,
            fault_mode: mode,
            block,
            count: 1,
            buffer_block: slot,
            fault_arg: 0,
        };
        match self.call(&request) {
            Some(reply) => reply.status == disk_status::OK,
            None => false,
        }
    }

    /// Asks the device to make everything it has acknowledged durable.
    pub fn flush(&mut self) -> bool {
        let request = DiskRequest {
            op: disk_op::FLUSH,
            ..DiskRequest::default()
        };
        match self.call(&request) {
            Some(reply) => reply.status == disk_status::OK,
            None => false,
        }
    }

    /// Latches the medium: nothing further reaches it, whatever asks.
    ///
    /// Scaffolding. It is issued before the service asks for the run to end, so
    /// that no write can slip through between the decision and the end.
    pub fn latch(&mut self) {
        let request = DiskRequest {
            op: disk_op::LATCH,
            ..DiskRequest::default()
        };
        let _ = self.call(&request);
        self.latched = true;
    }

    /// Reads the driver's counters without doing anything to the medium.
    pub fn stats(&mut self) -> Option<DiskReply> {
        let request = DiskRequest {
            op: disk_op::STATS,
            ..DiskRequest::default()
        };
        self.call(&request)
    }
}

/// The whole shared buffer, as one contiguous slice.
///
/// # Safety
///
/// The supervisor mapped `IOBUF_PAGES` writable pages at `IOBUF_VADDR` before
/// this domain ran. Used only before the store is touched, by the check that
/// rebuilds the golden vectors, which needs more than one block at once.
pub fn iobuf_bytes() -> &'static mut [u8] {
    // SAFETY: as `slot_bytes`, over every page of the same mapping.
    unsafe { core::slice::from_raw_parts_mut(IOBUF_VADDR as *mut u8, IOBUF_PAGES as usize * BLOCK) }
}

/// The bytes of one buffer slot.
///
/// # Safety
///
/// `slot` must be less than [`IOBUF_PAGES`]. The supervisor mapped the whole
/// buffer writable into this domain before it ran.
pub fn slot_bytes(slot: u32) -> &'static mut [u8] {
    assert!((slot as u64) < IOBUF_PAGES);
    // SAFETY: the supervisor mapped `IOBUF_PAGES` writable pages at
    // `IOBUF_VADDR` before this domain was activated, the offset is inside
    // them, and this domain is single-threaded, so no other borrow exists.
    unsafe {
        core::slice::from_raw_parts_mut(
            (IOBUF_VADDR + u64::from(slot) * BLOCK as u64) as *mut u8,
            BLOCK,
        )
    }
}
