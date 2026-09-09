//! ACPI tables: what the firmware says the machine has.
//!
//! K3 needs three answers from the firmware and no more: which processors
//! exist and by what identity, where the PCI configuration space is mapped,
//! and whether a DMA remapping unit is described at all. Everything else in
//! ACPI — AML, device routing, power states — is out of the profile and is not
//! parsed, because parsing it would mean interpreting a bytecode the kernel has
//! no way to validate.
//!
//! Every table is validated before it is believed: signature, declared length
//! against the mapped window, and the byte checksum the specification defines.
//! A table that fails any of those is discarded and named, never partially
//! used. Identifiers are read as the firmware wrote them: an APIC ID is not an
//! index, the boot processor is not assumed to be zero, and a duplicate entry
//! is rejected rather than collapsed.
//!
//! Source: ACPI 6.6, §5.2.5 (RSDP), §5.2.7 (RSDT/XSDT), §5.2.12 (MADT).

use thalyx_boot_protocol::HHDM_BASE;

use crate::event;
use crate::limits::MAX_CPUS;

/// A processor the firmware reports.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuEntry {
    /// Local APIC identifier. Not an index and not dense.
    pub apic_id: u32,
    /// Firmware's own processor identifier, kept for diagnostics only.
    pub acpi_uid: u32,
    /// Whether the entry is enabled, which is the only state this profile
    /// accepts. An online-capable but disabled processor would need hotplug.
    pub enabled: bool,
    /// Whether the entry came from an x2APIC structure rather than a local
    /// APIC structure.
    pub x2apic: bool,
}

/// What the firmware described.
#[derive(Clone, Copy)]
pub struct Platform {
    /// Processors, in firmware order.
    pub cpus: [CpuEntry; MAX_CPUS],
    /// Processors recorded above.
    pub cpu_count: usize,
    /// Processors the firmware listed that did not fit the table.
    pub cpus_dropped: usize,
    /// Enabled entries the firmware listed twice under one identifier.
    pub duplicates: usize,
    /// Physical base of the local APIC register page.
    pub lapic_phys: u64,
    /// Base of the PCI configuration window for bus zero of segment zero, or
    /// zero when no MCFG was found.
    pub ecam_base: u64,
    /// First bus the window covers.
    pub ecam_bus_start: u8,
    /// Last bus the window covers.
    pub ecam_bus_end: u8,
    /// Whether a DMA remapping description exists.
    pub dmar_present: bool,
    /// Remapping hardware units the DMAR describes.
    pub dmar_units: usize,
    /// Whether the DMAR claims every PCI segment is covered by one unit.
    pub dmar_include_all: bool,
    /// Tables the walk rejected.
    pub rejected: usize,
}

impl Platform {
    const fn new() -> Self {
        Self {
            cpus: [CpuEntry {
                apic_id: 0,
                acpi_uid: 0,
                enabled: false,
                x2apic: false,
            }; MAX_CPUS],
            cpu_count: 0,
            cpus_dropped: 0,
            duplicates: 0,
            lapic_phys: 0,
            ecam_base: 0,
            ecam_bus_start: 0,
            ecam_bus_end: 0,
            dmar_present: false,
            dmar_units: 0,
            dmar_include_all: false,
            rejected: 0,
        }
    }

    /// Enabled processors the firmware described.
    #[must_use]
    pub fn enabled(&self) -> usize {
        self.cpus[..self.cpu_count]
            .iter()
            .filter(|cpu| cpu.enabled)
            .count()
    }
}

/// Reads `count` bytes at physical `phys` through the direct map.
///
/// # Safety
///
/// `phys` must lie inside the direct map the loader established, which every
/// firmware table below the map limit does. The read is of plain bytes with no
/// invalid patterns, so the validation that follows decides whether the content
/// can be used.
unsafe fn bytes(phys: u64, count: usize) -> &'static [u8] {
    // SAFETY: the caller guarantees the range is inside the direct map.
    unsafe { core::slice::from_raw_parts((HHDM_BASE + phys) as *const u8, count) }
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(value)
}

fn checksum(data: &[u8]) -> u8 {
    let mut sum = 0u8;
    for byte in data {
        sum = sum.wrapping_add(*byte);
    }
    sum
}

/// Largest table this kernel will read. A declared length beyond it is refused
/// rather than trusted: the direct map covers the memory, but a length that
/// large means the header is wrong.
const MAX_TABLE_LEN: usize = 256 * 1024;

/// Header every system description table starts with.
struct Sdt {
    signature: [u8; 4],
    body: &'static [u8],
}

/// Validates one table at `phys` and returns it, or `None`.
fn table(phys: u64) -> Option<Sdt> {
    if phys == 0 {
        return None;
    }
    // SAFETY: firmware tables live below the direct-map limit the loader
    // established and validated.
    let header = unsafe { bytes(phys, 36) };
    let length = read_u32(header, 4) as usize;
    if length < 36 || length > MAX_TABLE_LEN {
        return None;
    }
    // SAFETY: as above, now for the declared length.
    let body = unsafe { bytes(phys, length) };
    if checksum(body) != 0 {
        return None;
    }
    Some(Sdt {
        signature: [body[0], body[1], body[2], body[3]],
        body,
    })
}

/// Parses the firmware description reachable from `rsdp_phys`.
///
/// Returns `None` when there is no usable root pointer, which is a refusal to
/// guess rather than a reason to probe memory for a signature.
#[must_use]
pub fn parse(rsdp_phys: u64) -> Option<Platform> {
    let mut platform = Platform::new();
    if rsdp_phys == 0 {
        event!("acpi.absent", "reason=no_rsdp_from_loader");
        return None;
    }
    // SAFETY: the loader took this address from the UEFI configuration table
    // and the direct map covers firmware memory.
    let rsdp = unsafe { bytes(rsdp_phys, 36) };
    if &rsdp[..8] != b"RSD PTR " {
        event!("acpi.rejected", "table=rsdp reason=signature");
        return None;
    }
    if checksum(&rsdp[..20]) != 0 {
        event!("acpi.rejected", "table=rsdp reason=checksum");
        return None;
    }
    let revision = rsdp[15];
    let mut root = 0u64;
    let mut wide = false;
    if revision >= 2 {
        let length = read_u32(rsdp, 20) as usize;
        if length >= 36 && length <= 4096 {
            // SAFETY: as above, for the declared extended length.
            let extended = unsafe { bytes(rsdp_phys, length) };
            if checksum(extended) == 0 {
                root = read_u64(extended, 24);
                wide = true;
            }
        }
    }
    if root == 0 {
        root = u64::from(read_u32(rsdp, 16));
        wide = false;
    }

    let Some(sdt) = table(root) else {
        event!(
            "acpi.rejected",
            "table={} reason=header_or_checksum addr=0x{root:x}",
            if wide { "xsdt" } else { "rsdt" }
        );
        return None;
    };
    let expected: &[u8; 4] = if wide { b"XSDT" } else { b"RSDT" };
    if &sdt.signature != expected {
        event!("acpi.rejected", "table=root reason=signature");
        return None;
    }

    let stride = if wide { 8 } else { 4 };
    let entries = (sdt.body.len() - 36) / stride;
    event!(
        "acpi.root",
        "kind={} revision={revision} addr=0x{root:x} entries={entries}",
        if wide { "xsdt" } else { "rsdt" }
    );

    for index in 0..entries {
        let offset = 36 + index * stride;
        let child = if wide {
            read_u64(sdt.body, offset)
        } else {
            u64::from(read_u32(sdt.body, offset))
        };
        let Some(child) = table(child) else {
            platform.rejected += 1;
            event!(
                "acpi.rejected",
                "table=child index={index} reason=validation"
            );
            continue;
        };
        match &child.signature {
            b"APIC" => madt(&mut platform, child.body),
            b"MCFG" => mcfg(&mut platform, child.body),
            b"DMAR" => dmar(&mut platform, child.body),
            _ => {}
        }
    }

    Some(platform)
}

/// Multiple APIC description table: processors and the register base.
fn madt(platform: &mut Platform, body: &'static [u8]) {
    if body.len() < 44 {
        platform.rejected += 1;
        return;
    }
    platform.lapic_phys = u64::from(read_u32(body, 36));
    let flags = read_u32(body, 40);

    let mut offset = 44usize;
    let mut structures = 0usize;
    while offset + 2 <= body.len() {
        let kind = body[offset];
        let length = body[offset + 1] as usize;
        // A zero or truncated length would make the walk loop or read past the
        // table; both end the walk instead.
        if length < 2 || offset + length > body.len() {
            platform.rejected += 1;
            event!(
                "acpi.rejected",
                "table=madt reason=entry_length offset={offset} len={length}"
            );
            break;
        }
        structures += 1;
        match kind {
            0 if length >= 8 => {
                let uid = u32::from(body[offset + 2]);
                let apic_id = u32::from(body[offset + 3]);
                let entry_flags = read_u32(body, offset + 4);
                add_cpu(platform, apic_id, uid, entry_flags & 1 != 0, false);
            }
            9 if length >= 16 => {
                let apic_id = read_u32(body, offset + 4);
                let entry_flags = read_u32(body, offset + 8);
                let uid = read_u32(body, offset + 12);
                add_cpu(platform, apic_id, uid, entry_flags & 1 != 0, true);
            }
            5 if length >= 12 => {
                platform.lapic_phys = read_u64(body, offset + 4);
            }
            _ => {}
        }
        offset += length;
    }

    event!(
        "acpi.madt",
        "structures={structures} processors={} enabled={} dropped={} duplicates={} \
         lapic_phys=0x{:x} pcat_compat={}",
        platform.cpu_count,
        platform.enabled(),
        platform.cpus_dropped,
        platform.duplicates,
        platform.lapic_phys,
        flags & 1
    );
    for index in 0..platform.cpu_count {
        let cpu = platform.cpus[index];
        event!(
            "acpi.processor",
            "entry={index} apic_id={} acpi_uid={} enabled={} structure={}",
            cpu.apic_id,
            cpu.acpi_uid,
            u8::from(cpu.enabled),
            if cpu.x2apic { "x2apic" } else { "lapic" }
        );
    }
}

fn add_cpu(platform: &mut Platform, apic_id: u32, acpi_uid: u32, enabled: bool, x2apic: bool) {
    // An x2APIC structure with an identifier below 255 describes the same
    // processor as its local APIC structure. Firmware is allowed to list both;
    // counting them twice would invent a processor.
    for index in 0..platform.cpu_count {
        if platform.cpus[index].apic_id == apic_id {
            if enabled && platform.cpus[index].enabled {
                platform.duplicates += 1;
            }
            platform.cpus[index].enabled |= enabled;
            platform.cpus[index].x2apic |= x2apic;
            return;
        }
    }
    if platform.cpu_count >= MAX_CPUS {
        platform.cpus_dropped += 1;
        return;
    }
    platform.cpus[platform.cpu_count] = CpuEntry {
        apic_id,
        acpi_uid,
        enabled,
        x2apic,
    };
    platform.cpu_count += 1;
}

/// Memory-mapped configuration space description.
///
/// The base an allocation names belongs to bus zero of its segment even when
/// the allocation starts at a later bus; the address of a function is computed
/// from that base and the function's own bus number, never by subtracting the
/// starting bus from the base.
fn mcfg(platform: &mut Platform, body: &'static [u8]) {
    if body.len() < 44 {
        platform.rejected += 1;
        return;
    }
    let mut offset = 44usize;
    let mut allocations = 0usize;
    while offset + 16 <= body.len() {
        let base = read_u64(body, offset);
        let segment = read_u16(body, offset + 8);
        let start = body[offset + 10];
        let end = body[offset + 11];
        allocations += 1;
        if segment == 0 && platform.ecam_base == 0 && start <= end {
            platform.ecam_base = base;
            platform.ecam_bus_start = start;
            platform.ecam_bus_end = end;
        }
        event!(
            "acpi.mcfg_allocation",
            "segment={segment} base=0x{base:x} bus_start={start} bus_end={end}"
        );
        offset += 16;
    }
    event!(
        "acpi.mcfg",
        "allocations={allocations} selected_base=0x{:x} bus_start={} bus_end={}",
        platform.ecam_base,
        platform.ecam_bus_start,
        platform.ecam_bus_end
    );
}

/// DMA remapping description.
///
/// Only presence and shape are read. A remapping unit that exists is not a
/// remapping unit this kernel programs, and the isolation profile says so
/// separately; recording the table here is what lets that statement be checked
/// against the firmware rather than assumed.
fn dmar(platform: &mut Platform, body: &'static [u8]) {
    if body.len() < 48 {
        platform.rejected += 1;
        return;
    }
    platform.dmar_present = true;
    let host_address_width = body[36] + 1;
    let flags = body[37];
    let mut offset = 48usize;
    while offset + 4 <= body.len() {
        let kind = read_u16(body, offset);
        let length = read_u16(body, offset + 2) as usize;
        if length < 4 || offset + length > body.len() {
            platform.rejected += 1;
            break;
        }
        if kind == 0 && length >= 16 {
            platform.dmar_units += 1;
            let unit_flags = body[offset + 4];
            if unit_flags & 1 != 0 {
                platform.dmar_include_all = true;
            }
            event!(
                "acpi.dmar_unit",
                "unit={} include_all={} segment={} register_base=0x{:x}",
                platform.dmar_units,
                unit_flags & 1,
                read_u16(body, offset + 6),
                read_u64(body, offset + 8)
            );
        }
        offset += length;
    }
    event!(
        "acpi.dmar",
        "units={} include_all={} host_address_width={host_address_width} flags=0x{flags:x}",
        platform.dmar_units,
        u8::from(platform.dmar_include_all)
    );
}
