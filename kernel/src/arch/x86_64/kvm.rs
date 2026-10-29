//! KVM's paravirtual end of interrupt.
//!
//! Under KVM without hardware APIC virtualization -- which an x2APIC guest on
//! this host's processors does not get -- an `EOI` write to the local APIC is
//! a model-specific register access the hypervisor intercepts: every
//! acknowledgement is a trip to the host and back. KVM offers a word of guest
//! memory instead, registered per processor through `MSR_KVM_PV_EOI_EN`. When
//! it injects an interrupt whose acknowledgement it can take on its own -- an
//! edge-triggered vector, the only one in service, nothing else pending -- it
//! sets bit 0 of that word. The handler clears the bit and does not write the
//! register; the host retires the vector the next time it looks, on an exit
//! it was taking anyway. With the bit clear the handler writes `EOI` exactly
//! as before, so a vector the host could not defer is acknowledged the
//! ordinary way.
//!
//! A Linux guest under KVM does this (`kvm_guest_apic_eoi_write`), and K6
//! compares this kernel with one: each timer tick and each inter-processor
//! interrupt here cost one exit more than there. With a tick every 500 us on
//! every processor that is two thousand exits a second per processor, which
//! is the order of the gap `scale.compute` measures.
//!
//! Enabled only when `CPUID` says the hypervisor is KVM and offers the
//! feature. Under TCG, and on hardware, the leaves are absent, no word is
//! registered, and every acknowledgement is the register write it always was.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering, compiler_fence};

use super::cpu;
use crate::limits::MAX_CPUS;

/// `MSR_KVM_PV_EOI_EN`: the guest-physical address of the word, bit 0 set to
/// enable.
const MSR_KVM_PV_EOI_EN: u32 = 0x4b56_4d04;
/// Bit of `CPUID 0x4000_0001 EAX` that offers it: `KVM_FEATURE_PV_EOI`.
const FEATURE_PV_EOI: u32 = 1 << 6;
/// The hypervisor leaves.
const LEAF_SIGNATURE: u32 = 0x4000_0000;
const LEAF_FEATURES: u32 = 0x4000_0001;
/// `"KVMKVMKVM\0\0\0"` in `EBX`, `ECX`, `EDX`.
const SIGNATURE: [u32; 3] = [0x4b4d_564b, 0x564b_4d56, 0x0000_004d];

/// One word per processor, four-byte aligned as the host requires. They share
/// a line: the host writes a processor's word only when it injects an
/// interrupt into that processor, and the processor clears it once per
/// interrupt, which is too seldom for the sharing to cost anything a line
/// each would save.
static WORDS: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];
/// Processors that registered their word.
static ENABLED: AtomicU64 = AtomicU64::new(0);
/// Physical minus virtual address of the kernel image, which holds `WORDS`.
/// Zero until recorded: the image is linked in the top half and loaded in
/// the bottom one, so the difference is never zero once known.
static IMAGE_DELTA: AtomicU64 = AtomicU64::new(0);

/// Records where the kernel image is in physical memory, so a processor can
/// name its word to the host.
pub fn note_image_delta(delta: u64) {
    IMAGE_DELTA.store(delta, Ordering::Release);
}

/// Whether the hypervisor is KVM and offers the paravirtual end of interrupt.
///
/// The signature alone decides. On hardware the hypervisor leaves either read
/// as zero or repeat the highest basic leaf, and neither spells `KVMKVMKVM`.
fn offered() -> bool {
    let signature = cpu::cpuid(LEAF_SIGNATURE, 0);
    if [signature.ebx, signature.ecx, signature.edx] != SIGNATURE {
        return false;
    }
    cpu::cpuid(LEAF_FEATURES, 0).eax & FEATURE_PV_EOI != 0
}

/// Registers processor `cpu`'s word with the host, if the host offers it.
/// Answers whether it did.
///
/// Called on the processor itself, after its per-processor block is
/// installed and with interrupts masked: the host attaches the word to the
/// processor that writes the register.
#[inline(never)]
pub fn enable_pv_eoi(cpu: usize) -> bool {
    let delta = IMAGE_DELTA.load(Ordering::Acquire);
    let Some(word) = WORDS.get(cpu) else {
        return false;
    };
    if delta == 0 || !offered() {
        return false;
    }
    word.store(0, Ordering::Relaxed);
    let physical = (core::ptr::from_ref(word) as u64).wrapping_add(delta);
    // The word is clear before the host may write it.
    compiler_fence(Ordering::SeqCst);
    // SAFETY: the register exists because the host offered the feature, and
    // the value names a four-byte-aligned word of the kernel image, which
    // stays mapped and owned by this kernel for the whole boot.
    unsafe { cpu::wrmsr(MSR_KVM_PV_EOI_EN, physical | 1) };
    ENABLED.fetch_or(1 << cpu, Ordering::Release);
    true
}

/// Takes the host's offer to acknowledge the interrupt being handled, if it
/// made one: clears the pending bit and answers whether it was set. When it
/// answers `false` the caller writes `EOI` itself.
#[inline]
pub fn take_deferred_eoi() -> bool {
    let enabled = ENABLED.load(Ordering::Relaxed);
    if enabled == 0 {
        return false;
    }
    let cpu = crate::percpu::index();
    if cpu >= MAX_CPUS || enabled & (1 << cpu) == 0 {
        return false;
    }
    let word = WORDS[cpu].as_ptr();
    let taken: u8;
    // SAFETY: `word` is this processor's own word. The host touches it only
    // while this processor is not running, so one read-modify-write
    // instruction is atomic against it without a lock prefix -- the same
    // `btr` Linux uses for the same word.
    unsafe {
        core::arch::asm!(
            "btr dword ptr [{word}], 0",
            "setc {taken}",
            word = in(reg) word,
            taken = out(reg_byte) taken,
            options(nostack),
        );
    }
    taken != 0
}
