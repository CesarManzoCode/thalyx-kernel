//! The link's notes, written once nobody is waiting for the link.
//!
//! A note is a summary record the kernel writes to COM1 synchronously, inside
//! the call that makes it, and the measurement image's trace-off does not
//! withhold it. Under KVM a record costs about 2 ms (ADR-011): one guest exit
//! per byte and one host system call per byte. The link made one note per
//! publication, one per work opened, one per work closed and one per control
//! request, all inside the request a consumer was waiting on. In the K8
//! baseline (`build/k8/check-20261030T113155`) the two in `open` -- the seed's
//! `PUBLISHED` and the fork's `WORK_OPENED` -- are about 4 ms of the 4.4 ms by
//! which K1's `open` trails L1's, and each publication in `settle` carries one
//! more (ADR-028).
//!
//! So a note made while serving is held here, in order, and written when the
//! link ends: after the reply to `shutdown` and before `FRAMES`. Its value is
//! the one it had when it was made; a note that carries a time carries its own.
//! What is lost is only where it falls among the other domains' records, and a
//! guest that dies before it ends loses the held notes. The ones that say
//! something went wrong (`LINK_UNEXPECTED` and the runtime's reports) are
//! still written at once, because those are the ones a dead guest must leave.

use thalyx_user_rt::k2;

/// Notes held at most. One more writes every held note first, then itself,
/// so the order survives and only the time moves back onto the path.
const HELD: usize = 64;

struct Held {
    kinds: [u64; HELD],
    values: [u64; HELD],
    count: usize,
    total: u64,
}

static mut HELD_NOTES: Held = Held {
    kinds: [0; HELD],
    values: [0; HELD],
    count: 0,
    total: 0,
};

#[allow(clippy::deref_addrof)]
fn held() -> &'static mut Held {
    // SAFETY: single-threaded domain; every borrow is dropped before the next.
    unsafe { &mut *(&raw mut HELD_NOTES) }
}

/// Holds a note until [`write_held`].
pub fn defer(kind: u64, value: u64) {
    let notes = held();
    notes.total += 1;
    if notes.count == HELD {
        write_held();
        k2::note(kind, value);
        return;
    }
    notes.kinds[notes.count] = kind;
    notes.values[notes.count] = value;
    notes.count += 1;
}

/// Writes every held note, oldest first.
pub fn write_held() {
    let notes = held();
    for index in 0..notes.count {
        k2::note(notes.kinds[index], notes.values[index]);
    }
    notes.count = 0;
}

/// Notes made through [`defer`] so far, written or not.
pub fn deferred() -> u64 {
    held().total
}
