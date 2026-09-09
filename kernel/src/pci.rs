//! PCI configuration space, through the window the firmware described.
//!
//! This is not a bus driver. It reads what ACPI said is there, walks bus zero,
//! finds the one device this revision knows how to assign, validates every
//! structure that device advertises against the window it claims to live in,
//! and stops. There is no rebalancer, no resource allocator and no hotplug: the
//! firmware's assignment is kept, and a function whose assignment does not
//! validate is refused rather than repaired.
//!
//! Three things here are deliberate rather than incidental:
//!
//! * the address of a function is computed from the base the firmware gave for
//!   **bus zero** of its segment plus that function's own bus number. Nothing
//!   subtracts a starting bus from the base;
//! * a base address register is sized with memory decoding turned off and its
//!   original value restored afterwards, which is the controlled sequence the
//!   specification requires. Probing a device that something is driving is not
//!   the same operation;
//! * bus mastering is left **off**. It is authority over the machine's memory,
//!   and it is granted separately, by a capability, not by enumeration.
//!
//! Source: PCI configuration mechanism as described by the ACPI MCFG table and
//! the PCI Firmware Specification; virtio 1.2 §4.1 for the capability layout.

use thalyx_boot_protocol::PAGE_SIZE;

use crate::event;

/// Offset of the capability list pointer in a type 0 header.
const CAP_POINTER: u32 = 0x34;
/// Command register.
const COMMAND: u32 = 0x04;
/// Status register.
const STATUS: u32 = 0x06;
/// First base address register.
const BAR0: u32 = 0x10;

/// `COMMAND.Memory Space Enable`.
const COMMAND_MEMORY: u16 = 1 << 1;
/// `COMMAND.Bus Master Enable`: the device may issue transactions of its own.
pub const COMMAND_BUS_MASTER: u16 = 1 << 2;
/// `STATUS.Capabilities List`.
const STATUS_CAP_LIST: u16 = 1 << 4;

/// Vendor-specific capability, which is how virtio describes itself.
const CAP_VENDOR: u8 = 0x09;
/// MSI-X capability.
const CAP_MSIX: u8 = 0x11;

/// Steps a capability walk may take before it is treated as a cycle.
const CAP_WALK_LIMIT: usize = 48;

/// Base address registers a type 0 header has.
pub const BAR_COUNT: usize = 6;

/// The mapped configuration window.
#[derive(Clone, Copy)]
pub struct Ecam {
    base: u64,
    bus_start: u8,
    bus_end: u8,
}

/// A base address register as the firmware left it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bar {
    /// Physical base, or zero when the register is unused.
    pub base: u64,
    /// Length in bytes, measured with decoding disabled.
    pub length: u64,
    /// Whether the register addresses memory rather than I/O ports.
    pub memory: bool,
    /// Whether the register is the low half of a 64-bit pair.
    pub wide: bool,
}

/// One virtio configuration structure the device advertises.
#[derive(Clone, Copy, Debug, Default)]
pub struct VirtioCap {
    /// Structure kind, as virtio numbers them.
    pub cfg_type: u8,
    /// Base address register it lives in.
    pub bar: u8,
    /// Byte offset inside that register's window.
    pub offset: u64,
    /// Byte length.
    pub length: u64,
    /// Notify offset multiplier; meaningful for the notify structure only.
    pub notify_off_multiplier: u32,
}

/// The MSI-X capability of a function.
#[derive(Clone, Copy, Debug, Default)]
pub struct Msix {
    /// Offset of the capability in configuration space.
    pub offset: u32,
    /// Interrupts the device supports.
    pub vectors: u16,
    /// Base address register holding the table.
    pub table_bar: u8,
    /// Byte offset of the table in that register's window.
    pub table_offset: u64,
    /// Base address register holding the pending-bit array.
    pub pba_bar: u8,
    /// Byte offset of that array.
    pub pba_offset: u64,
}

impl Msix {
    /// Bytes the table occupies.
    #[must_use]
    pub const fn table_len(&self) -> u64 {
        self.vectors as u64 * 16
    }
}

/// One function found on the bus.
#[derive(Clone, Copy, Debug, Default)]
pub struct Function {
    /// Segment, bus, device and function packed as the interface reports them.
    pub bdf: u32,
    /// Vendor identifier.
    pub vendor: u16,
    /// Device identifier.
    pub device: u16,
    /// Class, subclass and programming interface.
    pub class: u32,
    /// Base address registers.
    pub bars: [Bar; BAR_COUNT],
    /// Virtio structures, indexed by `cfg_type`.
    pub virtio: [Option<VirtioCap>; 6],
    /// MSI-X capability, if it has one.
    pub msix: Option<Msix>,
    /// Capabilities walked before the list ended or was cut off.
    pub capabilities: usize,
}

impl Function {
    /// Bus number.
    #[must_use]
    pub const fn bus(&self) -> u8 {
        ((self.bdf >> 8) & 0xFF) as u8
    }
    /// Device number.
    #[must_use]
    pub const fn slot(&self) -> u8 {
        ((self.bdf >> 3) & 0x1F) as u8
    }
    /// Function number.
    #[must_use]
    pub const fn function(&self) -> u8 {
        (self.bdf & 0x7) as u8
    }
}

/// Packs a bus/device/function triple the way the interface reports it.
#[must_use]
pub const fn pack_bdf(segment: u16, bus: u8, device: u8, function: u8) -> u32 {
    ((segment as u32) << 16) | ((bus as u32) << 8) | ((device as u32) << 3) | (function as u32)
}

impl Ecam {
    /// Binds to a configuration window already mapped at `base`.
    ///
    /// # Safety
    ///
    /// `base` must be the virtual address the firmware's window for bus
    /// `bus_start` of this segment is mapped at, uncacheable and writable, and
    /// the mapping must cover every bus in `[bus_start, bus_end]`.
    pub const unsafe fn new(base: u64, bus_start: u8, bus_end: u8) -> Self {
        Self {
            base,
            bus_start,
            bus_end,
        }
    }

    fn address(&self, bus: u8, device: u8, function: u8, offset: u32) -> Option<u64> {
        if bus < self.bus_start || bus > self.bus_end || device >= 32 || function >= 8 {
            return None;
        }
        if offset >= 4096 {
            return None;
        }
        // The window this kernel maps starts at `bus_start`; the base the
        // firmware named belongs to bus zero, and the mapping was placed so the
        // two agree. The bus offset is therefore relative to the first mapped
        // bus, not absolute.
        let bus_offset = u64::from(bus - self.bus_start) << 20;
        Some(
            self.base
                + bus_offset
                + (u64::from(device) << 15)
                + (u64::from(function) << 12)
                + u64::from(offset),
        )
    }

    fn read32(&self, bus: u8, device: u8, function: u8, offset: u32) -> u32 {
        let Some(address) = self.address(bus, device, function, offset & !3) else {
            return u32::MAX;
        };
        // SAFETY: the address is inside the mapped, uncacheable configuration
        // window, and a naturally aligned 32-bit read of configuration space
        // has no side effects.
        unsafe { core::ptr::read_volatile(address as *const u32) }
    }

    fn write32(&self, bus: u8, device: u8, function: u8, offset: u32, value: u32) {
        let Some(address) = self.address(bus, device, function, offset & !3) else {
            return;
        };
        // SAFETY: as `read32`. The caller owns the function being written.
        unsafe { core::ptr::write_volatile(address as *mut u32, value) }
    }

    fn read16(&self, bus: u8, device: u8, function: u8, offset: u32) -> u16 {
        let word = self.read32(bus, device, function, offset);
        ((word >> ((offset & 2) * 8)) & 0xFFFF) as u16
    }

    fn read8(&self, bus: u8, device: u8, function: u8, offset: u32) -> u8 {
        let word = self.read32(bus, device, function, offset);
        ((word >> ((offset & 3) * 8)) & 0xFF) as u8
    }

    fn write16(&self, bus: u8, device: u8, function: u8, offset: u32, value: u16) {
        let shift = (offset & 2) * 8;
        let word = self.read32(bus, device, function, offset);
        let cleared = word & !(0xFFFFu32 << shift);
        self.write32(
            bus,
            device,
            function,
            offset,
            cleared | (u32::from(value) << shift),
        );
    }

    /// Sets or clears bits of the command register.
    pub fn set_command(&self, bdf: u32, bits: u16, enable: bool) -> u16 {
        let (bus, device, function) = split(bdf);
        let current = self.read16(bus, device, function, COMMAND);
        let wanted = if enable {
            current | bits
        } else {
            current & !bits
        };
        self.write16(bus, device, function, COMMAND, wanted);
        self.read16(bus, device, function, COMMAND)
    }

    /// Writes one entry of a function's MSI-X table.
    ///
    /// The table lives in a base address register, and this is the only code
    /// that touches it. A driver that could reach it could choose the address
    /// and the payload of a memory write the machine performs on its behalf,
    /// which is authority over the interrupt space rather than over its own
    /// device.
    ///
    /// # Safety
    ///
    /// `table` must be the mapped virtual address of the function's MSI-X
    /// table, `entry` must be below the table's own vector count, and `address`
    /// and `data` must be the interrupt this kernel intends to receive.
    pub unsafe fn write_msix_entry(table: u64, entry: u16, address: u64, data: u32, masked: bool) {
        let slot = table + u64::from(entry) * 16;
        // SAFETY: the caller guarantees the table address and the entry bound.
        // The order is the one the specification requires: the entry is masked,
        // then its address and data are written, then it is unmasked, so no
        // interrupt can be delivered against a half-written entry.
        unsafe {
            core::ptr::write_volatile((slot + 12) as *mut u32, 1);
            core::ptr::write_volatile(slot as *mut u32, address as u32);
            core::ptr::write_volatile((slot + 4) as *mut u32, (address >> 32) as u32);
            core::ptr::write_volatile((slot + 8) as *mut u32, data);
            core::ptr::write_volatile((slot + 12) as *mut u32, u32::from(masked));
        }
    }

    /// Enables MSI-X on a function and clears its function-wide mask.
    pub fn enable_msix(&self, bdf: u32, capability: u32, enable: bool) -> u16 {
        let (bus, device, function) = split(bdf);
        let control = self.read16(bus, device, function, capability + 2);
        // Bit 15 enables the capability; bit 14 masks every vector.
        let wanted = if enable {
            (control | (1 << 15)) & !(1 << 14)
        } else {
            control & !(1 << 15)
        };
        self.write16(bus, device, function, capability + 2, wanted);
        self.read16(bus, device, function, capability + 2)
    }

    /// Measures one base address register with decoding disabled.
    ///
    /// The original value is restored before decoding is turned back on, so the
    /// function is left exactly as the firmware assigned it.
    fn size_bar(&self, bus: u8, device: u8, function: u8, index: usize) -> Bar {
        let offset = BAR0 + (index as u32) * 4;
        let original = self.read32(bus, device, function, offset);
        if original == 0 {
            return Bar::default();
        }
        let memory = original & 1 == 0;
        let wide = memory && (original >> 1) & 3 == 2;
        let mask: u32 = if memory { !0xF } else { !0x3 };

        self.write32(bus, device, function, offset, u32::MAX);
        let probed = self.read32(bus, device, function, offset);
        self.write32(bus, device, function, offset, original);

        let mut base = u64::from(original & mask);
        let mut size = u64::from(!(probed & mask)).wrapping_add(1) & 0xFFFF_FFFF;
        if wide {
            let high_offset = offset + 4;
            let high = self.read32(bus, device, function, high_offset);
            self.write32(bus, device, function, high_offset, u32::MAX);
            let high_probed = self.read32(bus, device, function, high_offset);
            self.write32(bus, device, function, high_offset, high);
            base |= u64::from(high) << 32;
            let full = (u64::from(high_probed) << 32) | u64::from(probed & mask);
            size = (!full).wrapping_add(1);
        }
        Bar {
            base,
            length: size,
            memory,
            wide,
        }
    }

    /// Reads one function, or `None` when nothing answers.
    fn probe(&self, segment: u16, bus: u8, device: u8, function: u8) -> Option<Function> {
        let identity = self.read32(bus, device, function, 0);
        let vendor = (identity & 0xFFFF) as u16;
        // An absent function reads as all ones. Treating that as a device with
        // vendor 0xFFFF is how a bus walk invents hardware.
        if vendor == 0xFFFF || vendor == 0 {
            return None;
        }
        let mut found = Function {
            bdf: pack_bdf(segment, bus, device, function),
            vendor,
            device: ((identity >> 16) & 0xFFFF) as u16,
            class: self.read32(bus, device, function, 0x08) >> 8,
            ..Function::default()
        };

        let header_type = self.read8(bus, device, function, 0x0E) & 0x7F;
        if header_type != 0 {
            // A bridge has a different header and different registers. This
            // revision assigns endpoints only.
            return Some(found);
        }

        // Turn decoding off while the registers are probed, and leave bus
        // mastering off when it goes back on.
        let original = self.read16(bus, device, function, COMMAND);
        self.write16(
            bus,
            device,
            function,
            COMMAND,
            original & !(COMMAND_MEMORY | COMMAND_BUS_MASTER),
        );
        let mut index = 0usize;
        while index < BAR_COUNT {
            let bar = self.size_bar(bus, device, function, index);
            found.bars[index] = bar;
            index += if bar.wide { 2 } else { 1 };
        }
        self.write16(
            bus,
            device,
            function,
            COMMAND,
            (original | COMMAND_MEMORY) & !COMMAND_BUS_MASTER,
        );

        if self.read16(bus, device, function, STATUS) & STATUS_CAP_LIST != 0 {
            self.walk_capabilities(bus, device, function, &mut found);
        }
        Some(found)
    }

    fn walk_capabilities(&self, bus: u8, device: u8, function: u8, found: &mut Function) {
        let mut pointer = u32::from(self.read8(bus, device, function, CAP_POINTER)) & !3;
        let mut seen = [0u32; CAP_WALK_LIMIT];
        let mut steps = 0usize;
        while pointer >= 0x40 && pointer < 4096 && steps < CAP_WALK_LIMIT {
            // A list that points back at something already visited is a cycle,
            // and a walk without this check does not end.
            if seen[..steps].contains(&pointer) {
                event!(
                    "pci.capability_cycle",
                    "bdf=0x{:x} offset=0x{pointer:x} steps={steps}",
                    found.bdf
                );
                break;
            }
            seen[steps] = pointer;
            steps += 1;
            let id = self.read8(bus, device, function, pointer);
            let next = u32::from(self.read8(bus, device, function, pointer + 1)) & !3;
            match id {
                CAP_VENDOR => {
                    let cfg_type = self.read8(bus, device, function, pointer + 3);
                    let length = self.read8(bus, device, function, pointer + 2);
                    if cfg_type >= 1 && (cfg_type as usize) < found.virtio.len() && length >= 16 {
                        let multiplier = if cfg_type == 2 && length >= 20 {
                            self.read32(bus, device, function, pointer + 16)
                        } else {
                            0
                        };
                        found.virtio[cfg_type as usize] = Some(VirtioCap {
                            cfg_type,
                            bar: self.read8(bus, device, function, pointer + 4),
                            offset: u64::from(self.read32(bus, device, function, pointer + 8)),
                            length: u64::from(self.read32(bus, device, function, pointer + 12)),
                            notify_off_multiplier: multiplier,
                        });
                    }
                }
                CAP_MSIX => {
                    let control = self.read16(bus, device, function, pointer + 2);
                    let table = self.read32(bus, device, function, pointer + 4);
                    let pba = self.read32(bus, device, function, pointer + 8);
                    found.msix = Some(Msix {
                        offset: pointer,
                        vectors: (control & 0x7FF) + 1,
                        table_bar: (table & 7) as u8,
                        table_offset: u64::from(table & !7),
                        pba_bar: (pba & 7) as u8,
                        pba_offset: u64::from(pba & !7),
                    });
                }
                _ => {}
            }
            if next == 0 {
                break;
            }
            pointer = next;
        }
        found.capabilities = steps;
    }
}

const fn split(bdf: u32) -> (u8, u8, u8) {
    (
        ((bdf >> 8) & 0xFF) as u8,
        ((bdf >> 3) & 0x1F) as u8,
        (bdf & 0x7) as u8,
    )
}

/// Functions one walk will report.
pub const MAX_FUNCTIONS: usize = 24;

/// What a walk of bus zero found.
pub struct Inventory {
    /// Functions found, in enumeration order.
    pub functions: [Function; MAX_FUNCTIONS],
    /// Functions recorded.
    pub count: usize,
    /// Functions found beyond the capacity above.
    pub dropped: usize,
}

/// Walks the buses the window covers and reports every function that answers.
///
/// Only the first bus is walked in this revision: the device this kernel
/// assigns lives there on the machine profile it supports, and walking behind
/// bridges would mean assigning devices whose isolation boundary this revision
/// cannot describe.
#[must_use]
pub fn enumerate(ecam: &Ecam, segment: u16) -> Inventory {
    let mut inventory = Inventory {
        functions: [Function::default(); MAX_FUNCTIONS],
        count: 0,
        dropped: 0,
    };
    let bus = ecam.bus_start;
    for device in 0..32u8 {
        let multifunction = ecam.read8(bus, device, 0, 0x0E) & 0x80 != 0;
        let functions = if multifunction { 8 } else { 1 };
        for function in 0..functions {
            let Some(found) = ecam.probe(segment, bus, device, function) else {
                continue;
            };
            if inventory.count >= MAX_FUNCTIONS {
                inventory.dropped += 1;
                continue;
            }
            inventory.functions[inventory.count] = found;
            inventory.count += 1;
        }
    }
    inventory
}

/// Reports one function on the diagnostic plane.
pub fn report(found: &Function) {
    event!(
        "pci.function",
        "bdf=0x{:x} bus={} device={} function={} vendor=0x{:04x} device_id=0x{:04x} \
         class=0x{:06x} capabilities={} msix={} command=0x{:x}",
        found.bdf,
        found.bus(),
        found.slot(),
        found.function(),
        found.vendor,
        found.device,
        found.class,
        found.capabilities,
        found.msix.map_or(0, |msix| msix.vectors),
        0
    );
    for (index, bar) in found.bars.iter().enumerate() {
        if bar.length == 0 {
            continue;
        }
        event!(
            "pci.bar",
            "bdf=0x{:x} bar={index} base=0x{:x} length=0x{:x} kind={} width={} pages={}",
            found.bdf,
            bar.base,
            bar.length,
            if bar.memory { "memory" } else { "io" },
            if bar.wide { 64 } else { 32 },
            bar.length.div_ceil(PAGE_SIZE)
        );
    }
    for cap in found.virtio.iter().flatten() {
        event!(
            "pci.virtio_capability",
            "bdf=0x{:x} cfg_type={} bar={} offset=0x{:x} length=0x{:x} notify_multiplier={}",
            found.bdf,
            cap.cfg_type,
            cap.bar,
            cap.offset,
            cap.length,
            cap.notify_off_multiplier
        );
    }
    if let Some(msix) = found.msix {
        event!(
            "pci.msix",
            "bdf=0x{:x} vectors={} table_bar={} table_offset=0x{:x} pba_bar={} pba_offset=0x{:x}",
            found.bdf,
            msix.vectors,
            msix.table_bar,
            msix.table_offset,
            msix.pba_bar,
            msix.pba_offset
        );
    }
}
