//! Devices as objects: authorised registers, one interrupt, DMA grants and a
//! session.
//!
//! A driver in this system is a domain, not part of the kernel, and what it
//! receives is deliberately narrower than "the device". It gets the register
//! windows the kernel validated, mapped uncacheable at addresses the kernel
//! chose; it gets an interrupt delivered as a signal, with the table entry that
//! produces it written only by the kernel; and it gets grants naming exactly
//! the pages the device may reach. It does not get configuration space, it does
//! not get bus mastering, and it does not get the table that decides where an
//! interrupt is written.
//!
//! That split is the whole point. A driver with unrestricted configuration
//! space could move its own base address registers, and a driver that can
//! program an MSI-X entry chooses the address and payload of a memory write the
//! machine performs on its behalf. Both are authority over the machine rather
//! than over a queue, and neither is what a capability over a device is
//! supposed to convey.
//!
//! Two things this revision does **not** claim. The address a device is given
//! is a physical address: no remapping unit is programmed, so a grant is a
//! record of intent and an accounting entry, not a boundary the hardware
//! enforces. And the session — the number every operation names and a reset
//! advances — bounds what *software* will accept after a reset; whether the
//! hardware has finished every transaction it had already issued is a separate
//! question, answered here only by the transport's own protocol.
//!
//! Source: virtio 1.2 §§2.4, 4.1 and 5.2 for the transport and the reset
//! criterion; VT-d 5.20 chapters 3–6 for the conditions a strong profile would
//! have to satisfy, which is why it is refused rather than approximated.

use thalyx_abi::generated::{device_state, dma_profile};
use thalyx_boot_protocol::PAGE_SIZE;

use crate::event;
use crate::limits::{MAX_DEVICE_REGIONS, MAX_DMA_GRANTS, MAX_IRQ_BINDINGS};
use crate::obj::ScopeId;
use crate::pci::{self, Msix};
use crate::state::MACHINE;

/// Device mappings the kernel tracks at once.
pub const MAX_DEVICE_MAPS: usize = 12;

/// Virtio device status register bits used here.
mod status {
    /// Every bit clear: the device has been reset.
    pub const RESET: u8 = 0;
}

/// Offsets inside the virtio common configuration structure.
mod common {
    /// Selects which 32 feature bits `DEVICE_FEATURE` reports.
    pub const DEVICE_FEATURE_SELECT: u64 = 0x00;
    /// The selected 32 feature bits.
    pub const DEVICE_FEATURE: u64 = 0x04;
    /// Device status.
    pub const DEVICE_STATUS: u64 = 0x14;
}

/// `VIRTIO_F_ACCESS_PLATFORM`, feature bit 33.
const FEATURE_ACCESS_PLATFORM: u32 = 1 << 1;
/// `VIRTIO_F_VERSION_1`, feature bit 32.
const FEATURE_VERSION_1: u32 = 1 << 0;

/// Lifecycle of an assigned device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Free table slot.
    Empty,
    /// Assigned, quiescent, ready to be driven.
    Ready,
    /// Bus mastering enabled: the device may be issuing transactions.
    Running,
    /// A stop was attempted and did not complete.
    Stopping,
}

impl State {
    /// Interface value.
    #[must_use]
    pub const fn abi(self) -> u32 {
        match self {
            State::Empty => 0,
            State::Ready => device_state::READY,
            State::Running => device_state::RUNNING,
            State::Stopping => device_state::STOPPING,
        }
    }

    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            State::Empty => "empty",
            State::Ready => "ready",
            State::Running => "running",
            State::Stopping => "stopping",
        }
    }
}

/// One register window a driver may be given.
#[derive(Clone, Copy, Debug, Default)]
pub struct Region {
    /// Virtio structure kind.
    pub kind: u32,
    /// Base address register it lives in.
    pub bar: u8,
    /// Page-aligned physical base.
    pub phys: u64,
    /// Byte offset inside that register's window, kept for the record.
    pub offset: u64,
    /// Page-aligned length.
    pub length: u64,
    /// Notify offset multiplier, meaningful for the notify structure only.
    pub notify_off_multiplier: u32,
    /// Mappings of this region currently installed.
    pub maps: u32,
}

/// A device function the kernel has validated and assigned.
pub struct Device {
    /// Whether the slot is in use.
    pub used: bool,
    /// Generation of this table slot.
    pub generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Lifecycle.
    pub state: State,
    /// Advances on every reset. An operation naming an older session is
    /// refused, so a driver that was stopped cannot act on what it remembers.
    pub session: u32,
    /// Scope charged for the object.
    pub sponsor: ScopeId,
    /// Diagnostic label.
    pub label: [u8; 16],
    /// Segment, bus, device and function.
    pub bdf: u32,
    /// Vendor identifier.
    pub vendor: u16,
    /// Device identifier.
    pub device_id: u16,
    /// Class, subclass and programming interface.
    pub class: u32,
    /// Authorised register windows.
    pub regions: [Region; MAX_DEVICE_REGIONS],
    /// Windows recorded.
    pub region_count: usize,
    /// MSI-X capability of the function.
    pub msix: Option<Msix>,
    /// Kernel-only mapping of the MSI-X table.
    pub msix_table: u64,
    /// Kernel-only mapping of the common configuration structure.
    pub common_config: u64,
    /// Whether bus mastering is enabled in configuration space.
    pub bus_master: bool,
    /// Whether the device offers platform-mediated access for its own DMA.
    pub access_platform: bool,
    /// Whether the device offers the modern transport at all.
    pub version_1: bool,
    /// Address width the kernel will hand the device.
    pub dma_address_bits: u32,
    /// Isolation group. Without bridge and access-control inspection, the
    /// boundary this revision can justify is the device number.
    pub isolation_group: u32,
    /// Functions in that group.
    pub group_members: u32,
    /// Capability entries naming this device.
    pub refs: u32,
}

impl Device {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            generation: 0,
            id: 0,
            state: State::Empty,
            session: 0,
            sponsor: 0,
            label: [0; 16],
            bdf: 0,
            vendor: 0,
            device_id: 0,
            class: 0,
            regions: [Region {
                kind: 0,
                bar: 0,
                phys: 0,
                offset: 0,
                length: 0,
                notify_off_multiplier: 0,
                maps: 0,
            }; MAX_DEVICE_REGIONS],
            region_count: 0,
            msix: None,
            msix_table: 0,
            common_config: 0,
            bus_master: false,
            access_platform: false,
            version_1: false,
            dma_address_bits: 64,
            isolation_group: 0,
            group_members: 1,
            refs: 0,
        }
    }

    /// Label as text, for diagnostic records.
    #[must_use]
    pub fn label_str(&self) -> &str {
        let len = self.label.iter().position(|byte| *byte == 0).unwrap_or(16);
        core::str::from_utf8(&self.label[..len]).unwrap_or("?")
    }

    /// Which isolation profile this device can actually sustain, and why.
    ///
    /// The conditions are the ones the isolation groundwork lists that this
    /// kernel is in a position to evaluate. Every one of them has to hold for
    /// the strong profile; the first that does not is what the record names.
    /// Nothing here downgrades silently: a caller that required the strong
    /// profile is refused, and a caller that did not is told which one it got.
    #[must_use]
    pub fn profile(&self, iommu_described: bool, iommu_translating: bool) -> (u32, &'static str) {
        if !iommu_described {
            return (
                dma_profile::WEAK_TRUSTED_DRIVER,
                "no_remapping_unit_described",
            );
        }
        if !iommu_translating {
            return (
                dma_profile::WEAK_TRUSTED_DRIVER,
                "remapping_described_but_not_programmed",
            );
        }
        if self.group_members > 1 {
            return (
                dma_profile::WEAK_TRUSTED_DRIVER,
                "group_has_several_functions",
            );
        }
        if !self.access_platform {
            return (
                dma_profile::WEAK_TRUSTED_DRIVER,
                "device_does_not_offer_access_platform",
            );
        }
        (dma_profile::STRONG_IOMMU, "conditions_met")
    }
}

/// A mapping of a device register window in some domain.
#[derive(Clone, Copy, Debug)]
pub struct MapRecord {
    /// Whether the record is in use.
    pub used: bool,
    /// Device table index.
    pub device: u16,
    /// Generation the device had when the mapping was installed.
    pub device_generation: u32,
    /// Session the device was in.
    pub session: u32,
    /// Region index.
    pub region: u8,
    /// Domain the mapping lives in.
    pub domain: u16,
    /// Generation that domain had.
    pub domain_generation: u32,
    /// First virtual address.
    pub vaddr: u64,
    /// Pages mapped.
    pub pages: u32,
    /// Scope charged for the mapping's metadata.
    pub scope: ScopeId,
}

impl MapRecord {
    /// A free record.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            device: 0,
            device_generation: 0,
            session: 0,
            region: 0,
            domain: 0,
            domain_generation: 0,
            vaddr: 0,
            pages: 0,
            scope: 0,
        }
    }
}

/// Pages a device is allowed to reach, under one session.
#[derive(Clone, Copy, Debug)]
pub struct DmaGrant {
    /// Whether the record is in use.
    pub used: bool,
    /// Device table index.
    pub device: u16,
    /// Session the grant belongs to. A reset makes it stale by construction.
    pub session: u32,
    /// Memory object index.
    pub memory: u16,
    /// Generation that object had.
    pub memory_generation: u32,
    /// First page of the object.
    pub offset_pages: u32,
    /// Pages granted.
    pub pages: u32,
    /// Rights the device has, from the device's point of view.
    pub rights: u32,
    /// Address the device uses.
    pub iova: u64,
    /// Profile the grant was made under.
    pub profile: u32,
    /// Scope charged for the grant.
    pub scope: ScopeId,
}

impl DmaGrant {
    /// A free record.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            device: 0,
            session: 0,
            memory: 0,
            memory_generation: 0,
            offset_pages: 0,
            pages: 0,
            rights: 0,
            iova: 0,
            profile: 0,
            scope: 0,
        }
    }
}

/// One device interrupt routed to a signal.
#[derive(Clone, Copy, Debug)]
pub struct IrqBinding {
    /// Whether the record is in use.
    pub used: bool,
    /// Device table index.
    pub device: u16,
    /// Generation the device had.
    pub device_generation: u32,
    /// Session the binding belongs to.
    pub session: u32,
    /// MSI-X entry programmed.
    pub entry: u16,
    /// Interrupt vector the entry writes.
    pub vector: u8,
    /// Signal raised.
    pub signal: u16,
    /// Generation that signal had.
    pub signal_generation: u32,
    /// Bits raised.
    pub bits: u64,
    /// Interrupts delivered through this binding.
    pub deliveries: u64,
}

impl IrqBinding {
    /// A free record.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            device: 0,
            device_generation: 0,
            session: 0,
            entry: 0,
            vector: 0,
            signal: 0,
            signal_generation: 0,
            bits: 0,
            deliveries: 0,
        }
    }
}

/// Interrupts that arrived for a vector nothing is bound to.
static mut UNCLAIMED: u64 = 0;
/// Interrupts delivered to a binding since boot.
///
/// Counted here rather than only on the binding, because a reset destroys the
/// binding and a summary that read the binding's counter would report that the
/// interrupts never happened.
static mut DELIVERED: u64 = 0;
/// Per-interrupt records already emitted. Bounded: the evidence a run needs is
/// that interrupts arrive at all and reach the right binding, not a line per
/// completion.
static mut INTERRUPT_RECORDS: u32 = 0;
/// Most per-interrupt records one run emits.
const INTERRUPT_RECORD_LIMIT: u32 = 8;

/// Delivers one device interrupt.
///
/// Called from the interrupt handler with interrupts masked and the controller
/// already acknowledged. A vector whose binding has gone — because the device
/// was reset, or the signal was destroyed — raises nothing and is counted. That
/// is the whole reason a binding names a session and a generation: an interrupt
/// that arrives late, after the session it belonged to ended, must not be
/// attributed to the session that replaced it.
pub fn on_interrupt(vector: u8) {
    let mut machine = MACHINE.lock();
    let mut delivered = false;
    for index in 0..MAX_IRQ_BINDINGS {
        let binding = machine.irqs[index];
        if !binding.used || binding.vector != vector {
            continue;
        }
        let device = binding.device as usize;
        if !machine.devices[device].used
            || machine.devices[device].generation != binding.device_generation
            || machine.devices[device].session != binding.session
        {
            continue;
        }
        machine.irqs[index].deliveries += 1;
        let deliveries = machine.irqs[index].deliveries;
        let signal = binding.signal as usize;
        let generation = binding.signal_generation;
        let bits = binding.bits;
        crate::api::evtops::raise_bits(&mut machine, signal, generation, bits);
        delivered = true;
        // SAFETY: written only from interrupt handlers, which run with
        // interrupts masked while this lock is held, so the increments are
        // serialised by the lock this function already holds.
        let record = unsafe {
            DELIVERED += 1;
            let emit = INTERRUPT_RECORDS < INTERRUPT_RECORD_LIMIT;
            if emit {
                INTERRUPT_RECORDS += 1;
            }
            emit
        };
        if record {
            let device_id = machine.devices[device].id;
            let signal_id = machine.signals[signal].id;
            event!(
                "device.interrupt",
                "device={device_id} vector=0x{vector:x} entry={} signal={signal_id} \
                 bits=0x{bits:x} session={} deliveries={deliveries} cpu={}",
                binding.entry,
                binding.session,
                crate::percpu::index()
            );
        }
    }
    if !delivered {
        // SAFETY: written only from interrupt handlers, which run with
        // interrupts masked while this lock is held, so the increment is
        // serialised by the lock the caller already holds.
        unsafe { UNCLAIMED += 1 };
    }
}

/// Interrupts delivered to a binding over the whole run.
#[must_use]
pub fn delivered() -> u64 {
    // SAFETY: read while the machine is quiescent, from the summary path.
    unsafe { DELIVERED }
}

/// Interrupts that arrived for no binding.
#[must_use]
pub fn unclaimed() -> u64 {
    // SAFETY: read while the machine is quiescent, from the summary path.
    unsafe { UNCLAIMED }
}

/// Reads the device's own feature bits 32..63 through the kernel's mapping.
///
/// # Safety
///
/// `common` must be the kernel's mapping of the device's common configuration
/// structure, and the device must not be being driven by anyone else.
unsafe fn high_features(common: u64) -> u32 {
    // SAFETY: the caller guarantees the mapping. Selecting a feature word and
    // reading it back is the transport's own protocol and has no effect on a
    // device nobody has acknowledged.
    unsafe {
        core::ptr::write_volatile((common + common::DEVICE_FEATURE_SELECT) as *mut u32, 1);
        core::ptr::read_volatile((common + common::DEVICE_FEATURE) as *const u32)
    }
}

/// Reads the device status register.
///
/// # Safety
///
/// As [`high_features`].
pub unsafe fn read_status(common: u64) -> u8 {
    // SAFETY: the caller guarantees the mapping.
    unsafe { core::ptr::read_volatile((common + common::DEVICE_STATUS) as *const u8) }
}

/// Writes the device status register.
///
/// # Safety
///
/// As [`high_features`].
pub unsafe fn write_status(common: u64, value: u8) {
    // SAFETY: the caller guarantees the mapping.
    unsafe { core::ptr::write_volatile((common + common::DEVICE_STATUS) as *mut u8, value) }
}

/// Puts the device back into its reset state and reports whether it agreed.
///
/// Virtio gives a protocol criterion rather than a timeout: after a reset the
/// status register reads zero, and until it does the device has not confirmed
/// that it has stopped interacting with the queues. A read that never reaches
/// zero is a failure to observe quiescence, not a slow success.
///
/// # Safety
///
/// `common` must be the kernel's mapping of the device's common configuration.
pub unsafe fn reset_transport(common: u64) -> bool {
    // SAFETY: the caller guarantees the mapping.
    unsafe {
        write_status(common, status::RESET);
        for _ in 0..1_000_000u32 {
            if read_status(common) == status::RESET {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

/// Whether a candidate function is the device this revision assigns.
#[must_use]
pub fn is_virtio_block(function: &pci::Function) -> bool {
    // Modern virtio only. A legacy identifier is not treated as modern because
    // it happens to work under an emulator.
    function.vendor == 0x1AF4 && function.device == 0x1042
}

/// Result of validating a function's advertised structures.
pub struct Validated {
    /// Windows the driver may be given.
    pub regions: [Region; MAX_DEVICE_REGIONS],
    /// Windows recorded.
    pub count: usize,
    /// Why the function was refused, if it was.
    pub refusal: Option<&'static str>,
}

/// Validates every virtio structure a function advertises against the register
/// it claims to live in, and against the MSI-X structures of the same register.
///
/// Nothing is taken on trust. A structure that runs past the end of its base
/// address register, that is not page-aligned in both offset and length, or
/// that would put the interrupt table inside a page the driver is given, makes
/// the whole function unassignable. Handing out three of four windows and
/// calling the driver's failure its own problem would be worse than refusing.
#[must_use]
pub fn validate(function: &pci::Function) -> Validated {
    let mut out = Validated {
        regions: [Region::default(); MAX_DEVICE_REGIONS],
        count: 0,
        refusal: None,
    };
    let msix = function.msix;
    for cfg_type in 1..=4u8 {
        let Some(cap) = function.virtio[cfg_type as usize] else {
            out.refusal = Some("missing_virtio_structure");
            return out;
        };
        let Some(bar) = function.bars.get(cap.bar as usize) else {
            out.refusal = Some("structure_names_no_such_bar");
            return out;
        };
        if !bar.memory || bar.length == 0 {
            out.refusal = Some("structure_bar_is_not_memory");
            return out;
        }
        let end = match cap.offset.checked_add(cap.length) {
            Some(end) => end,
            None => {
                out.refusal = Some("structure_range_overflows");
                return out;
            }
        };
        if cap.length == 0 || end > bar.length {
            out.refusal = Some("structure_outside_its_bar");
            return out;
        }
        if cap.offset % PAGE_SIZE != 0 || cap.length % PAGE_SIZE != 0 {
            out.refusal = Some("structure_not_page_granular");
            return out;
        }
        if let Some(msix) = msix
            && overlaps_interrupt_table(&msix, cap.bar, cap.offset, cap.length)
        {
            // Page protection cannot separate two structures that share a page,
            // and this one carries the address and payload of an interrupt.
            out.refusal = Some("structure_shares_pages_with_interrupt_table");
            return out;
        }
        if out.count >= MAX_DEVICE_REGIONS {
            out.refusal = Some("too_many_structures");
            return out;
        }
        out.regions[out.count] = Region {
            kind: u32::from(cfg_type),
            bar: cap.bar,
            phys: bar.base + cap.offset,
            offset: cap.offset,
            length: cap.length,
            notify_off_multiplier: cap.notify_off_multiplier,
            maps: 0,
        };
        out.count += 1;
    }
    out
}

/// Whether `[offset, offset+length)` in `bar` touches a page the interrupt
/// table or its pending-bit array occupies.
fn overlaps_interrupt_table(msix: &Msix, bar: u8, offset: u64, length: u64) -> bool {
    let mut hit = false;
    for (table_bar, table_offset, table_len) in [
        (msix.table_bar, msix.table_offset, msix.table_len()),
        (
            msix.pba_bar,
            msix.pba_offset,
            u64::from(msix.vectors).div_ceil(8),
        ),
    ] {
        if table_bar != bar {
            continue;
        }
        let table_start = table_offset & !(PAGE_SIZE - 1);
        let table_end = (table_offset + table_len).div_ceil(PAGE_SIZE) * PAGE_SIZE;
        if offset < table_end && table_start < offset + length {
            hit = true;
        }
    }
    hit
}

/// Reads the two feature bits the assignment depends on.
///
/// # Safety
///
/// `common` must be the kernel's mapping of the device's common configuration.
pub unsafe fn read_transport_features(common: u64) -> (bool, bool) {
    // SAFETY: the caller guarantees the mapping.
    let high = unsafe { high_features(common) };
    (
        high & FEATURE_VERSION_1 != 0,
        high & FEATURE_ACCESS_PLATFORM != 0,
    )
}

/// Reports one assigned device on the diagnostic plane.
pub fn report(index: usize) {
    let machine = MACHINE.lock();
    let device = &machine.devices[index];
    if !device.used {
        return;
    }
    let (profile, reason) = device.profile(machine.iommu_described, machine.iommu_translating);
    event!(
        "device.assigned",
        "device={} label={} bdf=0x{:x} vendor=0x{:04x} device_id=0x{:04x} class=0x{:06x} \
         regions={} session={} state={} bus_master={} version_1={} access_platform={} \
         msix_vectors={} isolation_group={} group_members={} dma_profile={profile} \
         profile_reason={reason} dma_address_bits={}",
        device.id,
        device.label_str(),
        device.bdf,
        device.vendor,
        device.device_id,
        device.class,
        device.region_count,
        device.session,
        device.state.name(),
        u8::from(device.bus_master),
        u8::from(device.version_1),
        u8::from(device.access_platform),
        device.msix.map_or(0, |msix| msix.vectors),
        device.isolation_group,
        device.group_members,
        device.dma_address_bits
    );
    for slot in 0..device.region_count {
        let region = device.regions[slot];
        event!(
            "device.region",
            "device={} region={slot} kind={} bar={} phys=0x{:x} offset=0x{:x} length=0x{:x} \
             pages={} notify_multiplier={} msix_bar={}",
            device.id,
            region.kind,
            region.bar,
            region.phys,
            region.offset,
            region.length,
            region.length / PAGE_SIZE,
            region.notify_off_multiplier,
            device.msix.map_or(255, |msix| msix.table_bar)
        );
    }
    let (grants, maps) = (
        machine
            .dma_grants
            .iter()
            .filter(|grant| grant.used && grant.device as usize == index)
            .count(),
        machine
            .device_maps
            .iter()
            .filter(|record| record.used && record.device as usize == index)
            .count(),
    );
    drop(machine);
    event!(
        "device.state",
        "device={index} grants={grants} maps={maps} unclaimed_interrupts={}",
        unclaimed()
    );
}

/// Next free page of the kernel's own device window.
static mut NEXT_MMIO_PAGE: u64 = 0;

/// Maps `pages` frames of device memory for the kernel's own use.
fn map_kernel_window(phys: u64, pages: u64) -> Option<u64> {
    // SAFETY: bump allocated during bootstrap, before any other context can
    // reach this window.
    let first = unsafe { NEXT_MMIO_PAGE };
    if first + pages > crate::layout::MMIO_PAGES {
        return None;
    }
    // SAFETY: as above.
    unsafe { NEXT_MMIO_PAGE = first + pages };
    let vaddr = crate::layout::MMIO_AREA + first * PAGE_SIZE;
    let mut guard = MACHINE.lock();
    let machine = &mut *guard;
    for page in 0..pages {
        let space = machine.kernel_space.as_mut()?;
        let allocator = machine.memory.as_mut()?;
        space
            .map(
                vaddr + page * PAGE_SIZE,
                crate::mm::Frame::containing(phys + page * PAGE_SIZE),
                crate::mm::Rights::KERNEL_DEVICE,
                allocator,
                crate::mm::Owner::Kernel,
            )
            .ok()?;
    }
    Some(vaddr)
}

/// Maps the firmware's configuration window and walks it.
///
/// Runs on every image, because it is platform bring-up rather than part of a
/// package: a machine with no assignable device produces the same records
/// minus an assignment, which is a smaller claim rather than a different one.
pub fn discover(platform: &crate::acpi::Platform, sponsor: ScopeId) {
    if platform.ecam_base == 0 {
        event!("pci.absent", "reason=no_configuration_window_described");
        return;
    }
    {
        let mut guard = MACHINE.lock();
        let machine = &mut *guard;
        machine.iommu_described = platform.dmar_present;
        machine.iommu_translating = false;
        let Some(space) = machine.kernel_space.as_mut() else {
            return;
        };
        let Some(allocator) = machine.memory.as_mut() else {
            return;
        };
        // One large entry, uncacheable. Configuration space is not RAM and a
        // write-back mapping of it would be a different device.
        if space
            .map_large_range(
                crate::layout::ECAM_VADDR,
                platform.ecam_base,
                1,
                crate::mm::Rights::KERNEL_DEVICE,
                allocator,
            )
            .is_err()
        {
            drop(guard);
            event!("pci.absent", "reason=configuration_window_not_mappable");
            return;
        }
    }

    // SAFETY: the window was just mapped uncacheable and writable at this
    // address, and it covers the buses named below.
    let ecam = unsafe {
        pci::Ecam::new(
            crate::layout::ECAM_VADDR,
            platform.ecam_bus_start,
            platform
                .ecam_bus_start
                .saturating_add(crate::layout::ECAM_BUSES - 1)
                .min(platform.ecam_bus_end),
        )
    };
    MACHINE.lock().ecam = Some(ecam);
    let inventory = pci::enumerate(&ecam, 0);
    event!(
        "pci.enumerated",
        "base=0x{:x} bus_start={} buses={} functions={} dropped={} \
         remapping_unit_described={}",
        platform.ecam_base,
        platform.ecam_bus_start,
        crate::layout::ECAM_BUSES,
        inventory.count,
        inventory.dropped,
        u8::from(platform.dmar_present)
    );
    for index in 0..inventory.count {
        pci::report(&inventory.functions[index]);
    }

    for index in 0..inventory.count {
        let function = inventory.functions[index];
        if !is_virtio_block(&function) {
            continue;
        }
        if assign(&ecam, &inventory, &function, sponsor).is_none() {
            continue;
        }
    }
}

/// Assigns one validated function, or refuses it and says why.
fn assign(
    ecam: &pci::Ecam,
    inventory: &pci::Inventory,
    function: &pci::Function,
    sponsor: ScopeId,
) -> Option<usize> {
    let validated = validate(function);
    if let Some(reason) = validated.refusal {
        event!(
            "device.refused",
            "bdf=0x{:x} vendor=0x{:04x} device_id=0x{:04x} reason={reason}",
            function.bdf,
            function.vendor,
            function.device
        );
        return None;
    }
    let Some(msix) = function.msix else {
        event!(
            "device.refused",
            "bdf=0x{:x} reason=no_msix_capability",
            function.bdf
        );
        return None;
    };

    // The kernel's own windows: the structure that carries the device status,
    // and the interrupt table. Neither is ever mapped into a driver.
    let common = validated.regions[0];
    let common_vaddr = map_kernel_window(common.phys, common.length / PAGE_SIZE)?;
    let table_bar = function.bars.get(msix.table_bar as usize)?;
    let table_phys = (table_bar.base + msix.table_offset) & !(PAGE_SIZE - 1);
    let table_pages = (msix.table_offset % PAGE_SIZE + msix.table_len()).div_ceil(PAGE_SIZE);
    let table_vaddr = map_kernel_window(table_phys, table_pages)? + (msix.table_offset % PAGE_SIZE);

    // SAFETY: the common configuration structure is mapped above, and nothing
    // is driving the device: it has not been acknowledged and bus mastering is
    // off.
    let (version_1, access_platform) = unsafe { read_transport_features(common_vaddr) };
    if !version_1 {
        event!(
            "device.refused",
            "bdf=0x{:x} reason=no_version_1_feature note=modern_transport_required",
            function.bdf
        );
        return None;
    }

    let members = inventory.functions[..inventory.count]
        .iter()
        .filter(|other| other.bus() == function.bus() && other.slot() == function.slot())
        .count() as u32;

    let mut machine = MACHINE.lock();
    let index = machine.devices.iter().position(|device| !device.used)?;
    if !crate::scope::reserve(
        &mut machine.scopes,
        sponsor,
        crate::scope::Resource::Metadata,
        1,
    ) {
        return None;
    }
    let id = machine.next_id()?;
    let generation = machine.devices[index].generation.saturating_add(1);
    let mut device = Device::empty();
    device.used = true;
    device.generation = generation;
    device.id = id;
    device.state = State::Ready;
    device.session = 1;
    device.sponsor = sponsor;
    device.label[..9].copy_from_slice(b"virtioblk");
    device.bdf = function.bdf;
    device.vendor = function.vendor;
    device.device_id = function.device;
    device.class = function.class;
    device.regions = validated.regions;
    device.region_count = validated.count;
    device.msix = Some(msix);
    device.msix_table = table_vaddr;
    device.common_config = common_vaddr;
    device.access_platform = access_platform;
    device.version_1 = version_1;
    device.isolation_group = u32::from(function.slot());
    device.group_members = members;
    machine.devices[index] = device;
    drop(machine);

    // The device is left exactly as it was found: quiescent, decoding memory,
    // not mastering the bus. Enabling MSI-X is the one change, and it is made
    // with every vector masked, so no interrupt can arrive before a binding
    // exists to attribute it to.
    let bdf = function.bdf;
    let control = ecam.enable_msix(bdf, msix.offset, true);
    let command = ecam.set_command(bdf, pci::COMMAND_BUS_MASTER, false);
    event!(
        "device.transport",
        "bdf=0x{bdf:x} msix_control=0x{control:x} command=0x{command:x} bus_master={} \
         common_config=0x{common_vaddr:x} msix_table=0x{table_vaddr:x} \
         table_pages={table_pages}",
        u8::from(command & pci::COMMAND_BUS_MASTER != 0)
    );
    report(index);
    Some(index)
}

/// Everything the run holds about devices, once nothing is running.
pub fn summarize() {
    let machine = MACHINE.lock();
    let assigned = machine.devices.iter().filter(|device| device.used).count();
    let grants = machine.dma_grants.iter().filter(|grant| grant.used).count();
    let maps = machine.device_maps.iter().filter(|map| map.used).count();
    let bound = machine.irqs.iter().filter(|binding| binding.used).count();
    let iommu = machine.iommu_described;
    let translating = machine.iommu_translating;
    // Anything a device could still reach when the run ended is named, not
    // counted: a grant that outlived its session, or a mapping that outlived
    // its domain, is exactly what a summary of totals would hide.
    let mut outstanding = [(0u64, 0u32, 0u32, 0u32, 0u32, 0u64); MAX_DMA_GRANTS];
    let mut count = 0usize;
    for grant in &machine.dma_grants {
        if !grant.used {
            continue;
        }
        outstanding[count] = (
            grant.iova,
            grant.pages,
            grant.rights,
            grant.profile,
            grant.session,
            u64::from(grant.offset_pages),
        );
        count += 1;
    }
    let mut stale_maps = 0usize;
    for record in &machine.device_maps {
        if record.used && record.session != machine.devices[record.device as usize].session {
            stale_maps += 1;
        }
    }
    let mut stale_bindings = 0usize;
    for binding in &machine.irqs {
        if binding.used && binding.session != machine.devices[binding.device as usize].session {
            stale_bindings += 1;
        }
    }
    drop(machine);
    for slot in 0..count {
        let (iova, pages, rights, profile, session, generation) = outstanding[slot];
        event!(
            "device.grant_outstanding",
            "iova=0x{iova:x} pages={pages} rights=0x{rights:x} profile={profile} \
             session={session} object_offset_pages={generation}"
        );
    }
    event!(
        "device.summary",
        "assigned={assigned} grants_outstanding={grants} maps_outstanding={maps} \
         bindings_outstanding={bound} interrupts_delivered={} \
         interrupts_unclaimed={} remapping_unit_described={} remapping_programmed={} \
         stale_maps={stale_maps} stale_bindings={stale_bindings}",
        delivered(),
        unclaimed(),
        u8::from(iommu),
        u8::from(translating)
    );
}
