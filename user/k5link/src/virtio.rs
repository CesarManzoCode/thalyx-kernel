//! The virtio-console transport, multiport, driven from user space.
//!
//! K3's block driver brought the modern virtio transport up from a user
//! domain; this is the same transport over a console function: a byte stream
//! per port between this machine and whatever holds the other end of the
//! socket QEMU exposes. The rings and the buffers are memory objects the
//! supervisor granted to the device; the register windows are what the kernel
//! mapped; the interrupt is a signal the kernel raises. Nothing here reaches
//! the device by any other road.
//!
//! virtio 1.2 §5.3. With `VIRTIO_CONSOLE_F_MULTIPORT` the queues are: 0 and 1
//! for port zero, 2 and 3 for control, then `2n+2` and `2n+3` for port `n`.
//! Port zero is reserved for a console by the specification and is not used.

use core::sync::atomic::{Ordering, fence};

use thalyx_user_k5pkg::link::{DATA_PAGES, MAX_PORTS, RX_BUFFERS, TX_PAGES, link_addr, note};
use thalyx_user_rt::k2::{self, report};

// Offsets inside the virtio common configuration structure, virtio 1.2 §4.1.4.3.
const DEVICE_FEATURE_SELECT: u64 = 0x00;
const DEVICE_FEATURE: u64 = 0x04;
const DRIVER_FEATURE_SELECT: u64 = 0x08;
const DRIVER_FEATURE: u64 = 0x0C;
const MSIX_CONFIG: u64 = 0x10;
const NUM_QUEUES: u64 = 0x12;
const DEVICE_STATUS: u64 = 0x14;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_MSIX_VECTOR: u64 = 0x1A;
const QUEUE_ENABLE: u64 = 0x1C;
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

/// Feature bits, low word.
const FEATURE_MULTIPORT: u32 = 1 << 1;
/// Feature bits, high word.
const FEATURE_VERSION_1: u32 = 1 << 0;

/// Offset of `max_nr_ports` in the device configuration, §5.3.4.
const CONFIG_MAX_PORTS: u64 = 4;

const DESC_WRITE: u16 = 2;

/// Entries in every queue this driver publishes.
const QUEUE_DEPTH: u16 = 8;
/// Bytes one queue's rings occupy in the ring object.
const QUEUE_STRIDE: u64 = 0x300;
const DESC_OFFSET: u64 = 0x000;
const AVAIL_OFFSET: u64 = 0x100;
const USED_OFFSET: u64 = 0x200;
/// Queues the ring object has room for: control plus `MAX_PORTS` ports.
const MAX_QUEUES: usize = 2 * (MAX_PORTS as usize) + 4;
const _: () = assert!(MAX_QUEUES as u64 * QUEUE_STRIDE <= 3 * 4096);

/// The fourth ring page: control-queue buffers.
const CONTROL_BUFFERS: u64 = 3 * 4096;
const CONTROL_SLOT: u64 = 64;
const CONTROL_RX_SLOTS: u16 = 8;
const CONTROL_TX_AT: u64 = CONTROL_BUFFERS + 0x800;

/// Control events, §5.3.6.2.
pub const DEVICE_READY: u16 = 0;
pub const DEVICE_ADD: u16 = 1;
pub const DEVICE_REMOVE: u16 = 2;
pub const PORT_READY: u16 = 3;
pub const CONSOLE_PORT: u16 = 4;
pub const RESIZE: u16 = 5;
pub const PORT_OPEN: u16 = 6;
pub const PORT_NAME: u16 = 7;

const PAGE: u64 = 4096;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UsedElem {
    id: u32,
    len: u32,
}

/// # Safety
///
/// `address` must be inside a window the kernel mapped for this domain, and
/// the access width one the device defines at that offset.
unsafe fn mmio_read<T: Copy>(address: u64) -> T {
    // SAFETY: the caller guarantees the address and the width.
    unsafe { core::ptr::read_volatile(address as *const T) }
}

/// # Safety
///
/// As [`mmio_read`].
unsafe fn mmio_write<T: Copy>(address: u64, value: T) {
    // SAFETY: the caller guarantees the address and the width.
    unsafe { core::ptr::write_volatile(address as *mut T, value) }
}

/// # Safety
///
/// `address` must be inside an object the supervisor mapped for this domain.
unsafe fn ring_read<T: Copy>(address: u64) -> T {
    // SAFETY: the caller guarantees the address.
    unsafe { core::ptr::read_volatile(address as *const T) }
}

/// # Safety
///
/// As [`ring_read`].
unsafe fn ring_write<T: Copy>(address: u64, value: T) {
    // SAFETY: the caller guarantees the address.
    unsafe { core::ptr::write_volatile(address as *mut T, value) }
}

/// One virtqueue: where its rings are, and what has been published and seen.
#[derive(Clone, Copy)]
struct Queue {
    /// Ring base in this domain.
    base: u64,
    size: u16,
    avail_index: u16,
    used_index: u16,
    /// Descriptors in flight, by index; a free descriptor has `len == 0`.
    in_flight: [Desc; QUEUE_DEPTH as usize],
    notify: u64,
    enabled: bool,
}

impl Queue {
    const EMPTY: Queue = Queue {
        base: 0,
        size: 0,
        avail_index: 0,
        used_index: 0,
        in_flight: [Desc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }; QUEUE_DEPTH as usize],
        notify: 0,
        enabled: false,
    };

    fn desc_at(&self, index: u16) -> u64 {
        self.base + DESC_OFFSET + u64::from(index) * 16
    }

    fn free_slot(&self) -> Option<u16> {
        self.in_flight
            .iter()
            .position(|desc| desc.len == 0)
            .map(|index| index as u16)
    }

    /// Publishes one single-descriptor buffer and notifies the device.
    fn publish(&mut self, addr: u64, len: u32, device_writes: bool) -> Option<u16> {
        let head = self.free_slot()?;
        let desc = Desc {
            addr,
            len,
            flags: if device_writes { DESC_WRITE } else { 0 },
            next: 0,
        };
        // SAFETY: every address is inside the ring object the supervisor mapped
        // writable for this domain, and each write is naturally aligned.
        unsafe {
            ring_write(self.desc_at(head), desc);
            let slot = self.avail_index % self.size;
            ring_write::<u16>(self.base + AVAIL_OFFSET + 4 + u64::from(slot) * 2, head);
        }
        self.in_flight[head as usize] = desc;
        // Descriptors and the ring entry before the index; the index before
        // the notification.
        fence(Ordering::Release);
        self.avail_index = self.avail_index.wrapping_add(1);
        // SAFETY: inside the mapped object.
        unsafe { ring_write::<u16>(self.base + AVAIL_OFFSET + 2, self.avail_index) };
        fence(Ordering::SeqCst);
        // SAFETY: the notify window the kernel mapped, at this queue's offset.
        unsafe { mmio_write::<u16>(self.notify, 0) };
        Some(head)
    }

    /// Consumes one completion: which descriptor, and how many bytes the
    /// device wrote (or read, for a transmit).
    fn consume(&mut self) -> Option<(u16, u32)> {
        // SAFETY: inside the mapped object.
        let device_index: u16 = unsafe { ring_read(self.base + USED_OFFSET + 2) };
        fence(Ordering::Acquire);
        let distance = device_index.wrapping_sub(self.used_index);
        if distance == 0 {
            return None;
        }
        if distance > self.size {
            k2::note(report::RING_REJECTED, u64::from(distance));
            return None;
        }
        let slot = self.used_index % self.size;
        // SAFETY: inside the mapped object.
        let entry: UsedElem =
            unsafe { ring_read(self.base + USED_OFFSET + 4 + u64::from(slot) * 8) };
        self.used_index = self.used_index.wrapping_add(1);
        let id = entry.id as usize;
        if id >= self.in_flight.len() || self.in_flight[id].len == 0 {
            k2::note(report::RING_REJECTED, u64::from(entry.id));
            return None;
        }
        let posted = self.in_flight[id].len;
        self.in_flight[id].len = 0;
        // A device may not write more than it was given.
        Some((entry.id as u16, entry.len.min(posted)))
    }
}

/// One port's buffers.
#[derive(Clone, Copy)]
struct Port {
    /// The device reported it.
    added: bool,
    /// The host side is connected, as the device last said.
    host_open: bool,
    /// Receive pages: descriptor index in flight per buffer, or `u16::MAX`.
    rx_posted: [u16; RX_BUFFERS as usize],
}

impl Port {
    const EMPTY: Port = Port {
        added: false,
        host_open: false,
        rx_posted: [u16::MAX; RX_BUFFERS as usize],
    };
}

/// A control message, §5.3.6.2.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Control {
    pub id: u32,
    pub event: u16,
    pub value: u16,
}

/// What arrived on a port: the device filled one receive page.
pub struct Received {
    pub port: u32,
    pub buffer: usize,
    pub len: usize,
}

/// The console transport.
pub struct Console {
    queues: [Queue; MAX_QUEUES],
    ports: [Port; MAX_PORTS as usize + 1],
    /// Ports the device reports, bounded by `MAX_PORTS`.
    pub max_ports: u32,
    irq: u64,
    data_base: u64,
    data_iova: u64,
    ring_base: u64,
    ring_iova: u64,
    control_tx_next: u16,
    pub rx_frames: u64,
    pub tx_frames: u64,
}

fn rx_queue(port: u32) -> usize {
    2 * port as usize + 2
}

fn tx_queue(port: u32) -> usize {
    2 * port as usize + 3
}

const CONTROL_RX: usize = 2;
const CONTROL_TX: usize = 3;

impl Console {
    /// Where port `port`'s receive buffer `index` begins, in this domain and
    /// as the device sees it.
    fn rx_buffer(&self, port: u32, index: usize) -> (u64, u64) {
        let page = u64::from(port - 1) * (RX_BUFFERS + TX_PAGES) + index as u64;
        (self.data_base + page * PAGE, self.data_iova + page * PAGE)
    }

    /// Where port `port`'s transmit buffer begins.
    fn tx_buffer(&self, port: u32) -> (u64, u64) {
        let page = u64::from(port - 1) * (RX_BUFFERS + TX_PAGES) + RX_BUFFERS;
        (self.data_base + page * PAGE, self.data_iova + page * PAGE)
    }

    /// The bytes of port `port`'s receive buffer `index`.
    pub fn rx_bytes(&self, port: u32, index: usize, len: usize) -> &[u8] {
        let (vaddr, _) = self.rx_buffer(port, index);
        // SAFETY: the supervisor mapped the data object at `link_addr::DATA`
        // for `DATA_PAGES` pages, and `port`, `index` and `len` are bounded by
        // the layout; the device wrote these bytes and has completed them.
        unsafe { core::slice::from_raw_parts(vaddr as *const u8, len.min(PAGE as usize)) }
    }

    /// Brings the transport up, negotiates multiport, and enables the control
    /// queues and the queues of ports one to `max_ports`.
    #[allow(clippy::too_many_arguments)]
    pub fn initialise(
        common: u64,
        notify_base: u64,
        multiplier: u32,
        device_config: u64,
        irq: u64,
        ring_iova: u64,
        data_iova: u64,
    ) -> Option<Console> {
        let ring_base = link_addr::RING;
        let mut console = Console {
            queues: [Queue::EMPTY; MAX_QUEUES],
            ports: [Port::EMPTY; MAX_PORTS as usize + 1],
            max_ports: 0,
            irq,
            data_base: link_addr::DATA,
            data_iova,
            ring_base,
            ring_iova,
            control_tx_next: 0,
            rx_frames: 0,
            tx_frames: 0,
        };
        let _ = DATA_PAGES;

        // SAFETY: `common` is the common configuration window the kernel mapped
        // for this domain, and every offset below is one the transport defines.
        unsafe {
            mmio_write::<u8>(common + DEVICE_STATUS, 0);
            while mmio_read::<u8>(common + DEVICE_STATUS) != 0 {
                core::hint::spin_loop();
            }
            mmio_write::<u8>(common + DEVICE_STATUS, STATUS_ACKNOWLEDGE);
            mmio_write::<u8>(common + DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

            mmio_write::<u32>(common + DEVICE_FEATURE_SELECT, 0);
            let low = mmio_read::<u32>(common + DEVICE_FEATURE);
            mmio_write::<u32>(common + DEVICE_FEATURE_SELECT, 1);
            let high = mmio_read::<u32>(common + DEVICE_FEATURE);
            if high & FEATURE_VERSION_1 == 0 || low & FEATURE_MULTIPORT == 0 {
                mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
                k2::note(report::UNEXPECTED, u64::from(low) | (u64::from(high) << 32));
                return None;
            }
            // Multiport and the modern transport, and nothing else: no event
            // index, no indirect descriptors, no emergency write.
            mmio_write::<u32>(common + DRIVER_FEATURE_SELECT, 0);
            mmio_write::<u32>(common + DRIVER_FEATURE, FEATURE_MULTIPORT);
            mmio_write::<u32>(common + DRIVER_FEATURE_SELECT, 1);
            mmio_write::<u32>(common + DRIVER_FEATURE, FEATURE_VERSION_1);
            mmio_write::<u8>(
                common + DEVICE_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
            );
            let confirmed = mmio_read::<u8>(common + DEVICE_STATUS);
            if confirmed & STATUS_FEATURES_OK == 0 {
                mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
                k2::note(report::UNEXPECTED, u64::from(confirmed));
                return None;
            }

            let reported = mmio_read::<u32>(device_config + CONFIG_MAX_PORTS);
            console.max_ports = reported.min(MAX_PORTS);
            let queues = mmio_read::<u16>(common + NUM_QUEUES) as usize;
            let needed = tx_queue(console.max_ports) + 1;
            if queues < needed {
                mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
                k2::note(report::UNEXPECTED, queues as u64);
                return None;
            }
            // No configuration-change interrupt: nothing is hot-plugged here.
            mmio_write::<u16>(common + MSIX_CONFIG, 0xFFFF);

            for index in 0..needed {
                // Port zero's queues stay disabled: the specification reserves
                // that port and this package attaches nothing to it.
                if index < CONTROL_RX {
                    continue;
                }
                mmio_write::<u16>(common + QUEUE_SELECT, index as u16);
                let offered = mmio_read::<u16>(common + QUEUE_SIZE);
                if offered == 0 {
                    mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
                    k2::note(report::UNEXPECTED, index as u64);
                    return None;
                }
                let size = offered.min(QUEUE_DEPTH);
                let base = ring_base + index as u64 * QUEUE_STRIDE;
                let iova = ring_iova + index as u64 * QUEUE_STRIDE;
                mmio_write::<u16>(common + QUEUE_SIZE, size);
                mmio_write::<u64>(common + QUEUE_DESC, iova + DESC_OFFSET);
                mmio_write::<u64>(common + QUEUE_DRIVER, iova + AVAIL_OFFSET);
                mmio_write::<u64>(common + QUEUE_DEVICE, iova + USED_OFFSET);
                // Every queue on the one vector the supervisor bound: the
                // driver looks at every used ring when it is woken.
                mmio_write::<u16>(common + QUEUE_MSIX_VECTOR, 0);
                let notify_off = mmio_read::<u16>(common + QUEUE_NOTIFY_OFF);
                mmio_write::<u16>(common + QUEUE_ENABLE, 1);
                console.queues[index] = Queue {
                    base,
                    size,
                    avail_index: 0,
                    used_index: 0,
                    in_flight: [Desc::default(); QUEUE_DEPTH as usize],
                    notify: notify_base + u64::from(notify_off) * u64::from(multiplier),
                    enabled: true,
                };
            }
            mmio_write::<u8>(
                common + DEVICE_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
            );
            let ready = mmio_read::<u8>(common + DEVICE_STATUS);
            k2::note(report::TRANSPORT_READY, u64::from(ready));
            if ready & STATUS_DRIVER_OK == 0 {
                return None;
            }
        }

        // Control receive buffers, then the one message that starts the
        // conversation: the device answers with one `DEVICE_ADD` per port.
        for slot in 0..CONTROL_RX_SLOTS {
            let at = CONTROL_BUFFERS + u64::from(slot) * CONTROL_SLOT;
            console.queues[CONTROL_RX].publish(ring_iova + at, CONTROL_SLOT as u32, true);
        }
        console.send_control(Control {
            id: 0,
            event: DEVICE_READY,
            value: 1,
        });
        k2::note(note::LINK_READY, u64::from(console.max_ports));
        Some(console)
    }

    /// Sends one control message to the device.
    pub fn send_control(&mut self, message: Control) {
        // Reclaim what the device has finished with before taking a slot.
        while self.queues[CONTROL_TX].consume().is_some() {}
        let slot = u64::from(self.control_tx_next % CONTROL_RX_SLOTS);
        self.control_tx_next = self.control_tx_next.wrapping_add(1);
        let at = CONTROL_TX_AT + slot * CONTROL_SLOT;
        // SAFETY: inside the ring object, a slot this driver owns.
        unsafe {
            ring_write::<u32>(self.ring_base + at, message.id);
            ring_write::<u16>(self.ring_base + at + 4, message.event);
            ring_write::<u16>(self.ring_base + at + 6, message.value);
        }
        if self.queues[CONTROL_TX]
            .publish(self.ring_iova + at, 8, false)
            .is_none()
        {
            k2::note(note::LINK_UNEXPECTED, 0x0C01);
        }
    }

    /// Posts every receive page of a port that is not already posted.
    fn post_rx(&mut self, port: u32) {
        for index in 0..RX_BUFFERS as usize {
            if self.ports[port as usize].rx_posted[index] != u16::MAX {
                continue;
            }
            let (_, iova) = self.rx_buffer(port, index);
            match self.queues[rx_queue(port)].publish(iova, PAGE as u32, true) {
                Some(head) => self.ports[port as usize].rx_posted[index] = head,
                None => k2::note(note::LINK_UNEXPECTED, 0x0C02),
            }
        }
    }

    /// Gives a receive page back to the device once its bytes were consumed.
    pub fn repost(&mut self, port: u32, buffer: usize) {
        self.ports[port as usize].rx_posted[buffer] = u16::MAX;
        self.post_rx(port);
    }

    /// Reads and answers control messages the device sent.
    fn serve_control(&mut self) {
        loop {
            let Some((head, len)) = self.queues[CONTROL_RX].consume() else {
                break;
            };
            let slot = u64::from(head) * CONTROL_SLOT;
            let at = self.ring_base + CONTROL_BUFFERS + slot;
            if len >= 8 {
                // SAFETY: inside the ring object, a slot the device just filled.
                let message = unsafe {
                    Control {
                        id: ring_read::<u32>(at),
                        event: ring_read::<u16>(at + 4),
                        value: ring_read::<u16>(at + 6),
                    }
                };
                self.on_control(message);
            }
            // The slot goes back to the device.
            self.queues[CONTROL_RX].publish(
                self.ring_iova + CONTROL_BUFFERS + slot,
                CONTROL_SLOT as u32,
                true,
            );
        }
    }

    fn on_control(&mut self, message: Control) {
        let port = message.id;
        match message.event {
            DEVICE_ADD => {
                if port == 0 || port > self.max_ports {
                    return;
                }
                self.ports[port as usize].added = true;
                self.post_rx(port);
                self.send_control(Control {
                    id: port,
                    event: PORT_READY,
                    value: 1,
                });
                // Opened from this side at once: a port nobody has connected
                // to yet is still a port this machine listens on.
                self.send_control(Control {
                    id: port,
                    event: PORT_OPEN,
                    value: 1,
                });
                k2::note(note::PORT_OPEN, u64::from(port));
            }
            PORT_OPEN => {
                if port != 0 && port <= self.max_ports {
                    self.ports[port as usize].host_open = message.value != 0;
                }
            }
            DEVICE_REMOVE | CONSOLE_PORT | RESIZE | PORT_NAME => {}
            other => k2::note(note::LINK_UNEXPECTED, 0x0C00 | u64::from(other)),
        }
    }

    /// Waits until something completes, then reports one received page if
    /// there is one. Control traffic and transmit completions are handled on
    /// the way. Returns `None` at the deadline.
    pub fn receive(&mut self, deadline_ns: u64) -> Option<Received> {
        loop {
            self.serve_control();
            for port in 1..=self.max_ports {
                let queue = rx_queue(port);
                if !self.queues[queue].enabled {
                    continue;
                }
                if let Some((head, len)) = self.queues[queue].consume() {
                    let Some(buffer) = self.ports[port as usize]
                        .rx_posted
                        .iter()
                        .position(|posted| *posted == head)
                    else {
                        k2::note(note::LINK_UNEXPECTED, 0x0C03);
                        continue;
                    };
                    self.rx_frames += 1;
                    return Some(Received {
                        port,
                        buffer,
                        len: len as usize,
                    });
                }
            }
            if k2::now_ns() >= deadline_ns {
                return None;
            }
            // Level, not edge: a completion that landed between the poll above
            // and this wait leaves the bit raised, and the wait returns at once.
            let _ = k2::signal_wait(
                self.irq,
                thalyx_user_k4fmt::pkg::bit::DEVICE,
                deadline_ns.min(k2::now_ns() + 200_000_000),
            );
        }
    }

    /// Sends bytes on a port, in pieces the transmit buffer holds, waiting for
    /// each piece to be taken before the next is written over it.
    pub fn send(&mut self, port: u32, bytes: &[u8]) -> bool {
        let (vaddr, iova) = self.tx_buffer(port);
        let queue = tx_queue(port);
        let capacity = (TX_PAGES * PAGE) as usize;
        let mut sent = 0usize;
        while sent < bytes.len() {
            let piece = (bytes.len() - sent).min(capacity);
            // SAFETY: the transmit buffer is `TX_PAGES` pages of the data
            // object the supervisor mapped writable here, and nothing is in
            // flight on it: the previous piece completed below.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes[sent..sent + piece].as_ptr(),
                    vaddr as *mut u8,
                    piece,
                );
            }
            fence(Ordering::Release);
            if self.queues[queue]
                .publish(iova, piece as u32, false)
                .is_none()
            {
                return false;
            }
            let deadline = k2::now_ns() + 10_000_000_000;
            loop {
                self.serve_control();
                if self.queues[queue].consume().is_some() {
                    break;
                }
                if k2::now_ns() >= deadline {
                    k2::note(note::LINK_UNEXPECTED, 0x0C04);
                    return false;
                }
                let _ = k2::signal_wait(
                    self.irq,
                    thalyx_user_k4fmt::pkg::bit::DEVICE,
                    k2::now_ns() + 50_000_000,
                );
            }
            sent += piece;
        }
        self.tx_frames += 1;
        true
    }
}
