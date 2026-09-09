//! K1 user domain `worker`.
//!
//! Runs a long integer workload with exactly one kernel entry per iteration and
//! never yields. It exists to show two things at once: that a domain keeps
//! making progress while sibling domains fault and are destroyed, and that the
//! timer takes the CPU away from a domain that never asks it to.
//!
//! The boot package instantiates this image twice. Two surviving domains mean
//! preemption is still observable after every faulting domain is gone, which a
//! single survivor could not show: with nothing to switch to, the scheduler has
//! no reason to preempt.

#![no_std]
#![no_main]

use thalyx_user_rt as rt;

/// Base of this program's FP pattern; the domain identifier is mixed in at run
/// time so two domains from the same image differ.
const FP_BASE: u64 = 0xA5A5_5A5A_0F0F_1001;
/// Progress notes emitted before the domain exits voluntarily.
const NOTES: u64 = 96;
/// Iterations of loop-carried integer work between two consecutive notes.
/// Sized so at least one 1 ms quantum expires inside one interval under QEMU
/// TCG, with exactly one kernel entry per interval.
const BURN: u64 = 200_000;

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
    rt::self_check(rt::check::WORK_OBSERVED, acc != 0);
    rt::exit(0)
}

rt::entry!(run);
