//! K1 user domain `wxprobe`.
//!
//! Makes ordinary progress, then writes to its own executable text. The page is
//! mapped, present and owned by this domain; the only thing denying the store is
//! the write bit the kernel refused to set on an executable mapping. It is the
//! W^X half of the boundary, which a probe into kernel memory does not cover.

#![no_std]
#![no_main]

use thalyx_user_rt as rt;

/// Base of this program's FP pattern; the domain identifier is mixed in at run
/// time so two domains from the same image differ.
const FP_BASE: u64 = 0xDEAD_0F0F_5151_3003;
/// Progress notes emitted before the deliberate illegal access.
const NOTES: u64 = 6;
/// Iterations of loop-carried integer work between two consecutive notes.
/// Sized so at least one 1 ms quantum expires inside one interval under QEMU
/// TCG, with exactly one kernel entry per interval.
const BURN: u64 = 200_000;

/// Probe identifier carried in the intent note.
const PROBE_TEXT_WRITE: u64 = 2;

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
    let target = run as *const () as u64;
    rt::probe_intent(PROBE_TEXT_WRITE, target);
    // SAFETY obligation intentionally unmet, as in `trespasser`: this store is
    // the experiment.
    unsafe { rt::probe::write_u8(target, 0xCC) };

    rt::self_check(rt::check::UNREACHABLE, false);
    rt::exit(acc)
}

rt::entry!(run);
