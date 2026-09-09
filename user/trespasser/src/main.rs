//! K1 user domain `trespasser`.
//!
//! Makes ordinary progress, then reads an address that belongs to the kernel's
//! half of its own address space. The page is present in this domain's page
//! tables — the kernel half is shared by every address space — and is mapped
//! without the user bit, so the fault is a privilege violation rather than a
//! missing translation, which is the case that actually tests the boundary.

#![no_std]
#![no_main]

use thalyx_user_rt as rt;

/// Base of this program's FP pattern; the domain identifier is mixed in at run
/// time so two domains from the same image differ.
const FP_BASE: u64 = 0x1234_ABCD_7777_2002;
/// Progress notes emitted before the deliberate illegal access.
const NOTES: u64 = 6;
/// Iterations of loop-carried integer work between two consecutive notes.
/// Sized so at least one 1 ms quantum expires inside one interval under QEMU
/// TCG, with exactly one kernel entry per interval.
const BURN: u64 = 200_000;

/// Probe identifier carried in the intent note.
const PROBE_KERNEL_READ: u64 = 1;
/// Base of the kernel image mapping: present, and supervisor-only.
const TARGET: u64 = 0xFFFF_FFFF_8000_0000;

fn run() -> ! {
    let pattern = rt::establish(FP_BASE);
    let mut acc = pattern;
    let mut i = 0u64;
    while i < NOTES {
        acc = rt::burn(BURN, acc);
        // Second value of a progress note: 1 when this domain's FP and SSE
        // state was still intact at the end of the interval.
        rt::progress(i, u64::from(rt::fp::verify(pattern)));
        i += 1;
    }
    rt::probe_intent(PROBE_KERNEL_READ, TARGET);
    // SAFETY obligation intentionally unmet: this load is the experiment. The
    // domain has no authority over kernel memory, so the CPU must fault here
    // and the kernel must classify and contain it.
    let observed = unsafe { rt::probe::read_u64(TARGET) };

    // Only reachable if the boundary failed. Reporting it is the negative
    // control: the run is a failure exactly when this note appears.
    rt::self_check(rt::check::UNREACHABLE, false);
    rt::exit(observed)
}

rt::entry!(run);
