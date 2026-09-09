//! User-side bindings for the V0 interface.
//!
//! Every call here is the same shape, because the interface is: a descriptor
//! whose header the caller fills in, a handle, an operation code, and a status.
//! The wrappers exist so a program states its intent once instead of restating
//! the register contract at every call site, and so a mistake in that contract
//! is one bug here rather than one bug per program.
//!
//! Nothing in this module grants authority. A wrapper that takes a handle can
//! only ever do what the grant behind that handle already permits; the kernel
//! resolves it in the caller's own table and refuses what the rights do not
//! cover. A program that holds no handle can reach exactly three entries — the
//! version query, the limits query and its own exit — and that is the whole
//! point of building the vertical this way.
//!
//! This is deliberately not a platform library. There is no heap, no I/O and no
//! naming service; a domain receives capabilities or it has none.

use thalyx_abi::generated::{
    BindFacetRequest, CallResult, DeriveRequest, DescriptorHeader, DomainCreateRequest,
    DrainReport, EffectRequest, EndpointCreateRequest, FaultChannelRequest, InstallCapRequest,
    InvocationInfo, Limits, LogAppendRequest, MapRequest, MemoryBytes, MemoryCreateRequest,
    MemoryInfo, ReceiveResult, ReplyRequest, ResolveRequest, ScopeCreateRequest, ScopeInfo,
    ScopeLimits, SendRequest, SignalBits, SignalInfo, ThreadCreateRequest, entry, op, spec, status,
};

/// Offset of a descriptor body: everything before it is the common header.
pub const BODY: usize = DescriptorHeader::SIZE;

/// Largest descriptor any assigned operation uses.
pub const MAX_DESCRIPTOR: usize = 432;

/// A descriptor being built or read.
///
/// One aligned buffer big enough for every assigned operation. Sizing it once
/// rather than per operation keeps a program from having to know which
/// operation has the largest descriptor, and costs a few hundred bytes of
/// stack.
#[repr(C, align(8))]
pub struct Desc {
    /// The bytes.
    pub bytes: [u8; MAX_DESCRIPTOR],
}

impl Default for Desc {
    fn default() -> Self {
        Self::new()
    }
}

impl Desc {
    /// A zeroed descriptor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_DESCRIPTOR],
        }
    }

    /// Clears the buffer and writes the header the operation requires.
    ///
    /// The kernel checks the opcode against the operation in RSI and the
    /// declared length against R10, so a descriptor prepared for one operation
    /// cannot be submitted as another. Filling both from the same table here is
    /// what makes that check a consistency check rather than a trap.
    pub fn prepare(&mut self, operation: u32, cookie: u64) -> u32 {
        let len = spec(operation).map_or(0, |s| s.descriptor_len);
        self.bytes = [0; MAX_DESCRIPTOR];
        self.put(
            0,
            DescriptorHeader {
                major: thalyx_abi::VERSION_MAJOR,
                minor: thalyx_abi::VERSION_MINOR,
                opcode: operation,
                flags: 0,
                total_len: len,
                cookie,
                reserved: 0,
            },
        );
        len
    }

    /// Reads a structure out of the buffer at `offset`.
    #[must_use]
    pub fn get<T: Copy>(&self, offset: usize) -> T {
        debug_assert!(offset + core::mem::size_of::<T>() <= MAX_DESCRIPTOR);
        // SAFETY: every generated structure is made of integers and arrays of
        // integers, so any bit pattern is a valid value; the read is explicitly
        // unaligned and bounded by the assertion above.
        unsafe { core::ptr::read_unaligned(self.bytes.as_ptr().add(offset).cast::<T>()) }
    }

    /// Writes a structure into the buffer at `offset`.
    pub fn put<T: Copy>(&mut self, offset: usize, value: T) {
        debug_assert!(offset + core::mem::size_of::<T>() <= MAX_DESCRIPTOR);
        // SAFETY: as `get`, and the buffer is exclusively borrowed here.
        unsafe {
            core::ptr::write_unaligned(self.bytes.as_mut_ptr().add(offset).cast::<T>(), value);
        }
    }

    /// Reads the body of the descriptor.
    #[must_use]
    pub fn body<T: Copy>(&self) -> T {
        self.get(BODY)
    }

    /// Writes the body of the descriptor.
    pub fn set_body<T: Copy>(&mut self, value: T) {
        self.put(BODY, value);
    }
}

/// Result of an operation: `Ok` carries the auxiliary value, `Err` the status.
pub type Outcome = Result<u64, i64>;

fn finish(st: i64, aux: u64) -> Outcome {
    if st == status::OK { Ok(aux) } else { Err(st) }
}

/// Invokes an operation whose descriptor is already prepared.
///
/// # Safety
///
/// Safe: the descriptor is this program's own memory, borrowed for the call,
/// and the kernel validates the range rather than faulting on it.
fn call(
    handle: u64,
    operation: u32,
    desc: &mut Desc,
    len: u32,
    flags: u64,
    deadline: u64,
) -> Outcome {
    let pointer = if len == 0 {
        0
    } else {
        core::ptr::from_mut(desc).addr() as u64
    };
    // SAFETY: `pointer` names `len` bytes of this program's own descriptor,
    // readable and writable, and `len` is the length the schema assigns to
    // `operation`. An operation with no descriptor passes a null pointer and a
    // zero length, which is what the interface requires for that case.
    let (st, aux) = unsafe {
        thalyx_abi::invoke(
            entry::INVOKE,
            handle,
            u64::from(operation),
            pointer,
            u64::from(len),
            flags,
            deadline,
        )
    };
    finish(st, aux)
}

/// Invokes an operation that takes no descriptor.
fn call_bare(handle: u64, operation: u32) -> Outcome {
    // SAFETY: the operation takes no pointer, so there is no memory
    // precondition to uphold.
    let (st, aux) =
        unsafe { thalyx_abi::invoke(entry::INVOKE, handle, u64::from(operation), 0, 0, 0, 0) };
    finish(st, aux)
}

/// Invokes an operation whose descriptor the caller filled in through `build`.
fn with<T: Copy>(handle: u64, operation: u32, cookie: u64, body: T) -> Outcome {
    let mut desc = Desc::new();
    let len = desc.prepare(operation, cookie);
    desc.set_body(body);
    call(handle, operation, &mut desc, len, 0, 0)
}

/// Invokes an operation that writes a response, returning it with the aux value.
fn query<T: Copy>(handle: u64, operation: u32) -> Result<(T, u64), i64> {
    let mut desc = Desc::new();
    let len = desc.prepare(operation, 0);
    let aux = call(handle, operation, &mut desc, len, 0, 0)?;
    Ok((desc.body(), aux))
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/// Reads the effective interface limits.
pub fn limits() -> Result<Limits, i64> {
    let mut value = Limits {
        major: 0,
        minor: 0,
        max_descriptor_len: 0,
        max_inline_payload: 0,
        max_caps_per_message: 0,
        max_scope_depth: 0,
        max_derive_depth: 0,
        max_handles_per_domain: 0,
        max_endpoint_queue: 0,
        max_memory_pages_per_object: 0,
        control_log_capacity: 0,
        control_log_reserved: 0,
        receipt_batch: 0,
        cpu_window_ns: 0,
        cpu_quantum_ns: 0,
        page_size: 0,
        boot_epoch: 0,
    };
    // SAFETY: the pointer names this program's own `Limits`, which is exactly
    // the length passed, and the entry only writes it.
    let (st, _) = unsafe {
        thalyx_abi::invoke(
            entry::LIMITS_QUERY,
            0,
            0,
            core::ptr::from_mut(&mut value).addr() as u64,
            core::mem::size_of::<Limits>() as u64,
            0,
            0,
        )
    };
    if st == status::OK { Ok(value) } else { Err(st) }
}

/// Terminates the calling domain.
pub fn exit(code: u64) -> ! {
    loop {
        // SAFETY: the exit entry takes integers only and does not return on
        // success; the loop covers a kernel that refused it.
        unsafe {
            thalyx_abi::invoke(entry::EXIT, 0, code, 0, 0, 0, 0);
        }
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// Common capability operations
// ---------------------------------------------------------------------------

/// Narrows a capability into a new handle of the calling domain.
pub fn derive(handle: u64, rights_mask: u32, deadline_ns: u64, life_scope: u64) -> Outcome {
    with(
        handle,
        op::CAP_DERIVE,
        0,
        DeriveRequest {
            rights_mask,
            reserved0: 0,
            deadline_ns,
            life_scope,
        },
    )
}

/// Duplicates a capability into a new slot without changing its rights.
pub fn cap_copy(handle: u64) -> Outcome {
    call_bare(handle, op::CAP_COPY)
}

/// Drops a handle.
pub fn cap_close(handle: u64) -> Outcome {
    call_bare(handle, op::CAP_CLOSE)
}

/// Closes a grant's whole subtree to new admissions.
pub fn cap_fence(handle: u64) -> Outcome {
    call_bare(handle, op::CAP_FENCE)
}

/// Reports what a fenced grant is still waiting for.
pub fn cap_drain_status(handle: u64) -> Result<DrainReport, i64> {
    query::<DrainReport>(handle, op::CAP_DRAIN_STATUS).map(|(report, _)| report)
}

// ---------------------------------------------------------------------------
// Scopes
// ---------------------------------------------------------------------------

/// Creates a child scope with limits bounded by the parent's.
pub fn scope_create_child(scope: u64, limits: ScopeLimits, label: [u8; 16]) -> Outcome {
    with(
        scope,
        op::SCOPE_CREATE_CHILD,
        0,
        ScopeCreateRequest { limits, label },
    )
}

/// Reads a scope's limits, usage and state.
pub fn scope_query(scope: u64) -> Result<ScopeInfo, i64> {
    query::<ScopeInfo>(scope, op::SCOPE_QUERY).map(|(info, _)| info)
}

/// Closes a scope to new admissions without stopping what it already admitted.
pub fn scope_fence(scope: u64) -> Outcome {
    call_bare(scope, op::SCOPE_FENCE)
}

/// Reports what a fenced scope is still waiting for.
pub fn scope_drain_status(scope: u64) -> Result<DrainReport, i64> {
    query::<DrainReport>(scope, op::SCOPE_DRAIN_STATUS).map(|(report, _)| report)
}

/// Retires a drained scope and releases what it held.
pub fn scope_retire(scope: u64) -> Result<DrainReport, i64> {
    let mut desc = Desc::new();
    let len = desc.prepare(op::SCOPE_RETIRE, 0);
    match call(scope, op::SCOPE_RETIRE, &mut desc, len, 0, 0) {
        Ok(_) => Ok(desc.body()),
        Err(code) => Err(code),
    }
}

/// Creates a domain from a sealed executable image.
pub fn scope_create_domain(scope: u64, image_handle: u64, name: [u8; 16]) -> Outcome {
    with(
        scope,
        op::SCOPE_CREATE_DOMAIN,
        0,
        DomainCreateRequest { image_handle, name },
    )
}

/// Creates a memory object charged to the scope.
pub fn scope_create_memory(scope: u64, pages: u64, max_rights: u32, label: [u8; 16]) -> Outcome {
    with(
        scope,
        op::SCOPE_CREATE_MEMORY,
        0,
        MemoryCreateRequest {
            pages,
            max_rights,
            reserved0: 0,
            label,
        },
    )
}

/// Creates an endpoint charged to the scope.
pub fn scope_create_endpoint(scope: u64, queue_capacity: u32, label: [u8; 16]) -> Outcome {
    with(
        scope,
        op::SCOPE_CREATE_ENDPOINT,
        0,
        EndpointCreateRequest {
            queue_capacity,
            reserved0: 0,
            label,
        },
    )
}

/// Creates a signal charged to the scope.
pub fn scope_create_signal(scope: u64) -> Outcome {
    call_bare(scope, op::SCOPE_CREATE_SIGNAL)
}

// ---------------------------------------------------------------------------
// Domains
// ---------------------------------------------------------------------------

/// Maps pages of a memory object into a domain's address space.
pub fn domain_map(
    domain: u64,
    memory_handle: u64,
    vaddr: u64,
    offset_pages: u32,
    page_count: u32,
    rights: u32,
) -> Outcome {
    with(
        domain,
        op::DOMAIN_MAP,
        0,
        MapRequest {
            memory_handle,
            vaddr,
            offset_pages,
            page_count,
            rights,
            reserved0: 0,
        },
    )
}

/// Installs one of the caller's capabilities into a domain's table.
pub fn domain_install_cap(
    domain: u64,
    source_handle: u64,
    target_slot: u32,
    rights_mask: u32,
    deadline_ns: u64,
) -> Outcome {
    with(
        domain,
        op::DOMAIN_INSTALL_CAP,
        0,
        InstallCapRequest {
            source_handle,
            deadline_ns,
            target_slot,
            rights_mask,
        },
    )
}

/// Adds a thread to a domain that has not been activated yet.
pub fn domain_add_thread(domain: u64, entry_point: u64, stack_top: u64, argument: u64) -> Outcome {
    with(
        domain,
        op::DOMAIN_ADD_THREAD,
        0,
        ThreadCreateRequest {
            entry: entry_point,
            stack_top,
            argument,
            reserved0: 0,
        },
    )
}

/// Names the endpoint a domain's faults are reported on.
///
/// The queue cell is reserved now rather than when a fault happens, so a
/// supervisor whose queue is full of ordinary traffic still hears that one of
/// its domains died.
pub fn domain_set_fault_channel(domain: u64, endpoint_handle: u64) -> Outcome {
    with(
        domain,
        op::DOMAIN_SET_FAULT_CHANNEL,
        0,
        FaultChannelRequest {
            endpoint_handle,
            reserved0: 0,
        },
    )
}

/// Makes a built domain runnable.
pub fn domain_activate(domain: u64) -> Outcome {
    call_bare(domain, op::DOMAIN_ACTIVATE)
}

/// Stops a domain.
pub fn domain_terminate(domain: u64) -> Outcome {
    call_bare(domain, op::DOMAIN_TERMINATE)
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// Reads a memory object's size, rights ceiling, seal state and label.
pub fn memory_query(memory: u64) -> Result<MemoryInfo, i64> {
    query::<MemoryInfo>(memory, op::MEMORY_QUERY).map(|(info, _)| info)
}

/// Writes bytes into an unsealed memory object.
pub fn memory_write(memory: u64, offset: u64, source: &[u8]) -> Outcome {
    let mut request = MemoryBytes {
        offset,
        length: source.len() as u64,
        bytes: [0; 256],
    };
    if source.len() > request.bytes.len() {
        return Err(status::INVALID_ARGUMENT);
    }
    request.bytes[..source.len()].copy_from_slice(source);
    with(memory, op::MEMORY_WRITE, 0, request)
}

/// Reads bytes out of a memory object into `destination`.
pub fn memory_read(memory: u64, offset: u64, destination: &mut [u8]) -> Outcome {
    if destination.len() > 256 {
        return Err(status::INVALID_ARGUMENT);
    }
    let mut desc = Desc::new();
    let len = desc.prepare(op::MEMORY_READ, 0);
    desc.set_body(MemoryBytes {
        offset,
        length: destination.len() as u64,
        bytes: [0; 256],
    });
    let aux = call(memory, op::MEMORY_READ, &mut desc, len, 0, 0)?;
    let result: MemoryBytes = desc.body();
    let copied = (result.length as usize).min(destination.len());
    destination[..copied].copy_from_slice(&result.bytes[..copied]);
    Ok(aux)
}

/// Withdraws every writer and publishes the object as immutable.
pub fn memory_seal(memory: u64) -> Result<MemoryInfo, i64> {
    let mut desc = Desc::new();
    let len = desc.prepare(op::MEMORY_SEAL, 0);
    match call(memory, op::MEMORY_SEAL, &mut desc, len, 0, 0) {
        Ok(_) => Ok(desc.body()),
        Err(code) => Err(code),
    }
}

// ---------------------------------------------------------------------------
// Endpoints and invocations
// ---------------------------------------------------------------------------

/// Binds a facet of an endpoint, producing a capability a client may hold.
pub fn endpoint_bind_facet(
    endpoint: u64,
    facet: u64,
    rights_mask: u32,
    deadline_ns: u64,
) -> Outcome {
    with(
        endpoint,
        op::ENDPOINT_BIND_FACET,
        0,
        BindFacetRequest {
            facet,
            deadline_ns,
            rights_mask,
            reserved0: 0,
        },
    )
}

/// Sends a request and waits for its reply.
///
/// Returns the reply. A caller that does not want to wait passes `nonblocking`,
/// and gets `WOULD_BLOCK` rather than a queued message it cannot observe.
pub fn endpoint_call(
    endpoint: u64,
    cookie: u64,
    payload: &[u8],
    caps: &[(u64, u32)],
    deadline_ns: u64,
    nonblocking: bool,
) -> Result<CallResult, i64> {
    let mut request = SendRequest {
        payload_len: payload.len() as u32,
        cap_count: caps.len() as u32,
        caps: [0; 4],
        cap_ops: [0; 4],
        payload: [0; 256],
    };
    if payload.len() > request.payload.len() || caps.len() > request.caps.len() {
        return Err(status::INVALID_ARGUMENT);
    }
    request.payload[..payload.len()].copy_from_slice(payload);
    for (index, (handle, operation)) in caps.iter().enumerate() {
        request.caps[index] = *handle;
        request.cap_ops[index] = *operation;
    }

    let mut desc = Desc::new();
    let len = desc.prepare(op::ENDPOINT_CALL, cookie);
    desc.set_body(request);
    let flags = if nonblocking {
        thalyx_abi::generated::flag::NONBLOCKING
    } else {
        0
    };
    match call(
        endpoint,
        op::ENDPOINT_CALL,
        &mut desc,
        len,
        flags,
        deadline_ns,
    ) {
        Ok(_) => Ok(desc.body()),
        Err(code) => Err(code),
    }
}

/// Sends a request without waiting for a reply.
pub fn endpoint_send(
    endpoint: u64,
    cookie: u64,
    payload: &[u8],
    caps: &[(u64, u32)],
    deadline_ns: u64,
) -> Outcome {
    let mut request = SendRequest {
        payload_len: payload.len() as u32,
        cap_count: caps.len() as u32,
        caps: [0; 4],
        cap_ops: [0; 4],
        payload: [0; 256],
    };
    if payload.len() > request.payload.len() || caps.len() > request.caps.len() {
        return Err(status::INVALID_ARGUMENT);
    }
    request.payload[..payload.len()].copy_from_slice(payload);
    for (index, (handle, operation)) in caps.iter().enumerate() {
        request.caps[index] = *handle;
        request.cap_ops[index] = *operation;
    }
    let mut desc = Desc::new();
    let len = desc.prepare(op::ENDPOINT_SEND, cookie);
    desc.set_body(request);
    call(endpoint, op::ENDPOINT_SEND, &mut desc, len, 0, deadline_ns)
}

/// Waits for the next message on an endpoint.
///
/// The auxiliary value is the handle of the invocation the message admitted,
/// which is how a server names the work it is now holding.
pub fn endpoint_receive(
    endpoint: u64,
    deadline_ns: u64,
    nonblocking: bool,
) -> Result<(ReceiveResult, u64), i64> {
    let mut desc = Desc::new();
    let len = desc.prepare(op::ENDPOINT_RECEIVE, 0);
    let flags = if nonblocking {
        thalyx_abi::generated::flag::NONBLOCKING
    } else {
        0
    };
    let invocation = call(
        endpoint,
        op::ENDPOINT_RECEIVE,
        &mut desc,
        len,
        flags,
        deadline_ns,
    )?;
    Ok((desc.body(), invocation))
}

/// Replies to an invocation and resolves it.
pub fn invocation_reply(invocation: u64, result: u64, payload: &[u8]) -> Outcome {
    let mut request = ReplyRequest {
        payload_len: payload.len() as u32,
        cap_count: 0,
        caps: [0; 4],
        cap_ops: [0; 4],
        result,
        payload: [0; 256],
    };
    if payload.len() > request.payload.len() {
        return Err(status::INVALID_ARGUMENT);
    }
    request.payload[..payload.len()].copy_from_slice(payload);
    with(invocation, op::INVOCATION_REPLY, 0, request)
}

/// Announces an effect and reserves what closing it will cost.
pub fn invocation_begin_effect(
    invocation: u64,
    effect_kind: u32,
    closure_reserve_ns: u64,
) -> Outcome {
    with(
        invocation,
        op::INVOCATION_BEGIN_EFFECT,
        0,
        EffectRequest {
            closure_reserve_ns,
            effect_kind,
            reserved0: 0,
        },
    )
}

/// Records how an announced effect ended.
pub fn invocation_resolve(invocation: u64, outcome: u32, detail: u64) -> Outcome {
    with(
        invocation,
        op::INVOCATION_RESOLVE,
        0,
        ResolveRequest {
            outcome,
            reserved0: 0,
            detail,
        },
    )
}

/// Reads an invocation's origin, cancellation and effect state.
pub fn invocation_query(invocation: u64) -> Result<InvocationInfo, i64> {
    query::<InvocationInfo>(invocation, op::INVOCATION_QUERY).map(|(info, _)| info)
}

// ---------------------------------------------------------------------------
// Control log
// ---------------------------------------------------------------------------

/// Appends a service note whose origin the kernel stamps over the claim.
pub fn log_append(log: u64, kind: u32, a: u64, b: u64, claimed_origin_domain_id: u64) -> Outcome {
    with(
        log,
        op::LOG_APPEND,
        0,
        LogAppendRequest {
            kind,
            reserved0: 0,
            a,
            b,
            claimed_origin_domain_id,
        },
    )
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// Raises bits on a signal. Returns the signal's new sequence number.
pub fn signal_raise(signal: u64, bits: u64) -> Outcome {
    with(signal, op::SIGNAL_RAISE, 0, SignalBits { bits })
}

/// Waits until any bit of `mask` is set, consuming what it observed.
pub fn signal_wait(signal: u64, mask: u64, deadline_ns: u64) -> Result<SignalInfo, i64> {
    let mut desc = Desc::new();
    let len = desc.prepare(op::SIGNAL_WAIT, 0);
    desc.set_body(SignalBits { bits: mask });
    match call(signal, op::SIGNAL_WAIT, &mut desc, len, 0, deadline_ns) {
        Ok(_) => Ok(desc.body()),
        Err(code) => Err(code),
    }
}

/// Slots the supervisor installs a child's capabilities in, and the bits it
/// uses to be told what the server is doing.
///
/// A child domain starts with no way to discover anything: it cannot enumerate
/// its own table, and there is no naming service to ask. What it has is this
/// agreement, compiled into both sides. Writing it down once is what keeps the
/// convention from being three slightly different conventions.
pub mod slot {
    /// Server: the endpoint it receives work on.
    pub const SERVER_ENDPOINT: u32 = 1;
    /// Server: the control log it appends service notes to.
    pub const SERVER_LOG: u32 = 2;
    /// Server: the signal it tells the supervisor it is holding work on.
    pub const SERVER_SIGNAL: u32 = 3;

    /// Client: the endpoint facet it may call, and its only authority to act
    /// outside itself.
    pub const CLIENT_ENDPOINT: u32 = 1;
    /// Client: a scratch memory object it owns and may narrow for someone else.
    pub const CLIENT_BUFFER: u32 = 2;

    /// Signal bit: the server has admitted an effect and is holding the work.
    pub const HOLDING_WORK: u64 = 1 << 0;
    /// Signal bit: the server observed the fence and finished resolving.
    pub const WORK_RESOLVED: u64 = 1 << 1;
}

/// What a K2 program reports about its own observations.
///
/// These go out on the K1 diagnostic plane, which is deliberately **not** the
/// receipt plane: they are a program's claim about what it saw, not the
/// kernel's record of what it decided. The kernel's own records and the control
/// log are what a gate believes about authority; these say what the program
/// asked for and what status came back, which is the half only the program
/// knows. A gate that used only these would be reading the defendant's account
/// of the trial.
pub mod report {
    /// The interface version and limits the program read back.
    pub const LIMITS: u64 = 0x2001;
    /// A step of the supervisor's build succeeded. Value: the step number.
    pub const BUILT: u64 = 0x2002;
    /// A step of the supervisor's build failed. Value: the step number.
    pub const BUILD_FAILED: u64 = 0x2003;
    /// The supervisor observed the server holding admitted work.
    pub const HOLDING: u64 = 0x2004;
    /// A drain report the supervisor read. Value: packed counters.
    pub const DRAIN: u64 = 0x2005;
    /// The client's call returned. Value: the status, as an unsigned bit pattern.
    pub const CALL_RESULT: u64 = 0x2006;
    /// An operation the program expected to be refused. Value: the status.
    pub const REFUSED_AS_EXPECTED: u64 = 0x2007;
    /// An operation the program expected to be refused and which was not.
    pub const NOT_REFUSED: u64 = 0x2008;
    /// The server's view of an invocation's cancellation state.
    pub const CANCEL_STATE: u64 = 0x2009;
    /// The server read the message header the kernel built. Value: origin id.
    pub const ORIGIN: u64 = 0x200A;
    /// The program reached its own end of script. Value: steps completed.
    pub const DONE: u64 = 0x200B;
    /// A status the program did not expect at all. Value: the status.
    pub const UNEXPECTED: u64 = 0x200C;
}

/// Reports one observation on the diagnostic plane.
pub fn note(check: u64, value: u64) {
    crate::diag_note(thalyx_abi::note::SELF_CHECK, check, value);
}

/// Reports the status of an operation that was expected to be refused.
///
/// Returns true when the refusal happened and carried the expected code. A
/// negative control that silently passes is worse than no control at all, so
/// the two outcomes are reported as different things.
pub fn expect_refusal(outcome: Outcome, expected: i64) -> bool {
    match outcome {
        Err(code) if code == expected => {
            note(report::REFUSED_AS_EXPECTED, code as u64);
            true
        }
        Err(code) => {
            note(report::UNEXPECTED, code as u64);
            false
        }
        Ok(_) => {
            note(report::NOT_REFUSED, expected as u64);
            false
        }
    }
}

/// Pads a label or a name to the fixed width the interface uses.
#[must_use]
pub fn name16(text: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let bytes = text.as_bytes();
    let n = if bytes.len() < 16 { bytes.len() } else { 15 };
    let mut i = 0;
    while i < n {
        out[i] = bytes[i];
        i += 1;
    }
    out
}
