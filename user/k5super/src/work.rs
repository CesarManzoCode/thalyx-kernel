//! Building a work domain.
//!
//! One piece of Thalyx work is one domain in one scope with one principal. That
//! is the whole of the adaptation `vault/integration/thalyx.md` asks for in the
//! row about attempts: a private workspace, a work scope, and a frozen
//! candidate, with abandoning costing nothing that was published.
//!
//! What a work is given here is what its role needs and nothing else. It never
//! holds a device capability, so it cannot reach the medium except through the
//! service; it never holds authority to build a domain, so running a tool is a
//! call to a launcher and not something it can do quietly; and its identity is
//! the facet the supervisor bound, so nothing in a request it sends can claim
//! to be another principal.

use thalyx_abi::right;
use thalyx_user_k5pkg::proto::{WorkConfig, work_addr, work_slot};
use thalyx_user_rt::k2::{self, name16};

use crate::services::config_object;

/// What one work domain is built from.
pub struct WorkParts<'a> {
    /// Diagnostic name.
    pub name: &'a str,
    /// The work image.
    pub image: u64,
    /// The scope it is charged to.
    pub scope: u64,
    /// The facet of the state service that is this work's principal.
    pub store_facet: u64,
    /// The signal it raises when it has finished.
    pub done: u64,
    /// The control log, when the role may append.
    pub log: u64,
    /// The endpoint faults are reported on.
    pub supervision: u64,
    /// The launcher facet, or zero when the role does not validate.
    pub launcher_facet: u64,
    /// The engine facet, or zero when the role does not infer.
    pub engine_facet: u64,
    /// The endpoint the language runtime calls it on, or zero.
    pub host_endpoint: u64,
    /// The region shared with the language runtime, or zero.
    pub channel: u64,
    /// The signal the supervisor raises to ask it to stop.
    pub cancel: u64,
}

/// The work's domain and the staging buffer it lends.
pub struct BuiltWork {
    /// The domain capability.
    pub domain: u64,
    /// The staging buffer, still named by the supervisor so it can be reclaimed.
    pub stage: u64,
}

/// Builds one work domain and activates it.
pub fn build(parts: &WorkParts<'_>, config: WorkConfig) -> Option<BuiltWork> {
    let domain = k2::scope_create_domain(parts.scope, parts.image, name16(parts.name)).ok()?;
    let page = config_object(parts.scope, "workcfg", &config)?;
    k2::domain_map(domain, page, work_addr::CONFIG, 0, 1, right::MEMORY_READ).ok()?;
    let _ = k2::cap_close(page);

    let stage = k2::scope_create_memory(
        parts.scope,
        work_addr::STAGE_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("stage"),
    )
    .ok()?;
    k2::domain_map(
        domain,
        stage,
        work_addr::STAGE,
        0,
        work_addr::STAGE_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .ok()?;

    k2::domain_install_cap(
        domain,
        parts.store_facet,
        work_slot::STORE,
        right::INSPECT | right::ENDPOINT_CALL,
        0,
    )
    .ok()?;
    // `DERIVE` and `TRANSFER`, because lending this buffer to the service is a
    // derivation and a transfer. What the service then receives is narrower
    // still: one call's worth of one direction.
    k2::domain_install_cap(
        domain,
        stage,
        work_slot::STAGE,
        right::INSPECT | right::DERIVE | right::TRANSFER | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        parts.done,
        work_slot::DONE,
        right::INSPECT | right::SIGNAL_RAISE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        parts.cancel,
        work_slot::CANCEL,
        right::INSPECT | right::SIGNAL_WAIT,
        0,
    )
    .ok()?;
    // Its own scope: to read what it has been charged, and to make the object a
    // candidate is sealed into. `SCOPE_CREATE` is a real authority and it is
    // given on purpose -- assembling a candidate is the work's job -- but it is
    // bounded by this scope's own page ceiling, so a work that tried to make
    // room for itself by making objects would be refused by the kernel rather
    // than by a rule in a program.
    k2::domain_install_cap(
        domain,
        parts.scope,
        work_slot::SELF_SCOPE,
        right::INSPECT | right::SCOPE_CREATE,
        0,
    )
    .ok()?;
    if parts.log != 0 {
        k2::domain_install_cap(
            domain,
            parts.log,
            work_slot::LOG,
            right::INSPECT | right::LOG_APPEND,
            0,
        )
        .ok()?;
    }
    if parts.launcher_facet != 0 {
        k2::domain_install_cap(
            domain,
            parts.launcher_facet,
            work_slot::LAUNCH,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
        )
        .ok()?;
    }
    if parts.engine_facet != 0 {
        k2::domain_install_cap(
            domain,
            parts.engine_facet,
            work_slot::ENGINE,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
        )
        .ok()?;
    }
    if parts.host_endpoint != 0 {
        k2::domain_install_cap(
            domain,
            parts.host_endpoint,
            work_slot::HOST,
            right::INSPECT | right::ENDPOINT_RECEIVE,
            0,
        )
        .ok()?;
    }
    if parts.channel != 0 {
        k2::domain_map(
            domain,
            parts.channel,
            work_addr::CHANNEL,
            0,
            work_addr::CHANNEL_PAGES as u32,
            right::MEMORY_READ | right::MEMORY_WRITE,
        )
        .ok()?;
        k2::domain_install_cap(
            domain,
            parts.channel,
            work_slot::CHANNEL,
            right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
            0,
        )
        .ok()?;
    }

    k2::domain_set_fault_channel(domain, parts.supervision).ok()?;
    k2::domain_activate(domain).ok()?;
    Some(BuiltWork { domain, stage })
}
