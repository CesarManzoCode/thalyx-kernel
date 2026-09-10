//! Building a native domain: the whole of `ProgramLaunch` on this system.
//!
//! A native C program cannot create its own threads and does not choose what
//! it is. Both facts come from the kernel rather than from a convention: a
//! running domain is refused `DOMAIN_ADD_THREAD`, and what a domain holds is
//! installed while it is being built. So a launcher does all of it, in an order
//! where nothing partial is ever activated:
//!
//! read the image, make a scope for it, build the domain, map the stack the
//! image's `_start` will move onto, write the boot record and map it read-only,
//! install the capabilities the role gets and no others, place a stack for each
//! built thread, add those threads at the trampoline the image published, bind
//! a fault channel, activate.
//!
//! Every step reports which one it was if it refuses, because a launcher that
//! answers "it did not work" is not evidence of anything.

use thalyx_abi::{right, status};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k5pkg::native::{self, Config, addr};
use thalyx_user_k5pkg::plan;
use thalyx_user_rt::k2::{self, name16};

/// What a role needs, decided by the supervisor and not by the program.
pub struct Recipe<'a> {
    /// Diagnostic name of the domain.
    pub name: &'a str,
    /// The sealed image object.
    pub image: u64,
    /// The scope the domain and everything it makes is charged to.
    pub scope: u64,
    /// Pages of main stack, mapped downwards from [`addr::STACK_TOP`].
    pub stack_pages: u64,
    /// Threads besides the first one.
    pub threads: u32,
    /// The boot record.
    pub config: Config,
    /// Endpoint the domain's faults are reported on.
    pub fault_channel: u64,
}

/// A capability to install, and what it is narrowed to.
pub struct Install {
    /// Slot in the domain's table.
    pub slot: u32,
    /// The supervisor's handle.
    pub handle: u64,
    /// Rights the copy carries. Never more than the supervisor's own.
    pub rights: u32,
}

/// The domain, and the two signal handles the supervisor keeps.
pub struct Built {
    /// The domain capability.
    pub domain: u64,
    /// Signal the runtime's workers wait on.
    pub work_signal: u64,
    /// Signal the runtime's workers raise when finished.
    pub done_signal: u64,
}

fn read_image_at(image: u64, offset: u64, into: &mut [u8]) -> bool {
    let mut at = 0usize;
    while at < into.len() {
        let take = core::cmp::min(into.len() - at, 256);
        let mut chunk = [0u8; 256];
        if k2::memory_read(image, offset + at as u64, &mut chunk[..take]).is_err() {
            return false;
        }
        into[at..at + take].copy_from_slice(&chunk[..take]);
        at += take;
    }
    true
}

/// Reads what the image publishes about itself.
pub fn inspect(image: u64) -> Option<plan::Image> {
    let mut head = [0u8; 256];
    if !read_image_at(image, 0, &mut head) {
        return None;
    }
    plan::inspect(&head, |offset, into| read_image_at(image, offset, into)).ok()
}

fn config_object(scope: u64, label: &str, config: &Config) -> Option<u64> {
    let handle = k2::scope_create_memory(
        scope,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16(label),
    )
    .ok()?;
    k2::memory_write(handle, 0, config.as_bytes()).ok()?;
    Some(handle)
}

/// Builds and activates one native domain.
///
/// `report` is called with the step number and the kernel's status whenever a
/// step refuses, and the function then answers `None` without activating
/// anything.
pub fn build(
    recipe: &Recipe<'_>,
    installs: &[Install],
    mut report: impl FnMut(u64, i64),
) -> Option<Built> {
    let mut step;
    let check =
        |outcome: Result<u64, i64>, which: u64, report: &mut dyn FnMut(u64, i64)| match outcome {
            Ok(value) => Some(value),
            Err(code) => {
                report(which, code);
                None
            }
        };

    step = 1;
    let image = match inspect(recipe.image) {
        Some(image) => image,
        None => {
            report(step, status::INVALID_ARGUMENT);
            return None;
        }
    };

    step = 2;
    let domain = check(
        k2::scope_create_domain(recipe.scope, recipe.image, name16(recipe.name)),
        step,
        &mut report,
    )?;

    // The stack first: `_start` moves the stack pointer onto it before it calls
    // anything, so a domain activated without it would fault on its first push
    // and the fault would name an address nobody could explain.
    step = 3;
    let stack = check(
        k2::scope_create_memory(
            recipe.scope,
            recipe.stack_pages,
            right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
            name16("stack"),
        ),
        step,
        &mut report,
    )?;
    step = 4;
    check(
        k2::domain_map(
            domain,
            stack,
            addr::STACK_TOP - recipe.stack_pages * 4096,
            0,
            recipe.stack_pages as u32,
            right::MEMORY_READ | right::MEMORY_WRITE,
        ),
        step,
        &mut report,
    )?;
    let _ = k2::cap_close(stack);

    step = 5;
    let config = match config_object(recipe.scope, "bootcfg", &recipe.config) {
        Some(handle) => handle,
        None => {
            report(step, status::LIMIT_EXHAUSTED);
            return None;
        }
    };
    step = 6;
    check(
        k2::domain_map(domain, config, addr::CONFIG, 0, 1, right::MEMORY_READ),
        step,
        &mut report,
    )?;
    let _ = k2::cap_close(config);

    step = 7;
    let self_scope = check(
        k2::domain_install_cap(
            domain,
            recipe.scope,
            native::slot::SELF_SCOPE,
            right::INSPECT | right::SCOPE_CREATE,
            0,
        ),
        step,
        &mut report,
    )?;
    let _ = self_scope;

    // Authority over itself, and only the part of it a running domain can use:
    // mapping what it created. `DOMAIN_ACTIVATE` and `DOMAIN_TERMINATE` stay
    // with the launcher, so a program cannot restart or stop itself out from
    // under the supervisor that is accounting for it.
    step = 8;
    check(
        k2::domain_install_cap(
            domain,
            domain,
            native::slot::SELF_DOMAIN,
            right::INSPECT | right::DOMAIN_BUILD,
            0,
        ),
        step,
        &mut report,
    )?;

    step = 9;
    let work_signal = check(k2::scope_create_signal(recipe.scope), step, &mut report)?;
    step = 10;
    let done_signal = check(k2::scope_create_signal(recipe.scope), step, &mut report)?;
    step = 11;
    check(
        k2::domain_install_cap(
            domain,
            work_signal,
            native::slot::SIGNAL_WORK,
            right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
            0,
        ),
        step,
        &mut report,
    )?;
    step = 12;
    check(
        k2::domain_install_cap(
            domain,
            done_signal,
            native::slot::SIGNAL_DONE,
            right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
            0,
        ),
        step,
        &mut report,
    )?;

    let mut which = 20u64;
    for install in installs {
        check(
            k2::domain_install_cap(domain, install.handle, install.slot, install.rights, 0),
            which,
            &mut report,
        )?;
        which += 1;
    }

    // One stack per built thread, each in its own slot so a thread can find its
    // index by masking its stack pointer. The runtime has no thread-local
    // segment: `FS` needs an instruction this kernel does not enable.
    for index in 1..=recipe.threads {
        let pages = 16u64;
        let base = addr::THREAD_STACKS + u64::from(index) * addr::THREAD_SLOT;
        let top = base + addr::THREAD_SLOT;
        which += 1;
        let stack = check(
            k2::scope_create_memory(
                recipe.scope,
                pages,
                right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
                name16("tstack"),
            ),
            which,
            &mut report,
        )?;
        which += 1;
        check(
            k2::domain_map(
                domain,
                stack,
                top - pages * 4096,
                0,
                pages as u32,
                right::MEMORY_READ | right::MEMORY_WRITE,
            ),
            which,
            &mut report,
        )?;
        let _ = k2::cap_close(stack);
        which += 1;
        check(
            k2::domain_add_thread(
                domain,
                image.header.thread_trampoline,
                top,
                u64::from(index),
            ),
            which,
            &mut report,
        )?;
    }

    which += 1;
    check(
        k2::domain_set_fault_channel(domain, recipe.fault_channel),
        which,
        &mut report,
    )?;
    which += 1;
    check(k2::domain_activate(domain), which, &mut report)?;
    let _ = step;

    Some(Built {
        domain,
        work_signal,
        done_signal,
    })
}
