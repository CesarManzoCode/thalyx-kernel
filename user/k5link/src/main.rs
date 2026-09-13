//! The K5 link: Thalyx-Kernel's third backend for the real Thalyx.
//!
//! `vault/integration/thalyx.md` and the Thalyx-side
//! `vault/09-Notas-Tecnicas/Frontera-de-Plataforma.md` describe a platform
//! boundary a consumer reaches over a channel: `VersionedState`,
//! `ProgramLaunch`, `WorkControl`, `MessageTransport`, `MonotonicClock`,
//! `ObjectAuthority`, `EvidenceSink`. On Linux, `linux-managed` implements
//! that boundary against `thalyx-managed`'s store over a loopback. This domain
//! implements the same boundary against **this kernel's own mechanisms**: the
//! K4 durable state service on the K4 block driver over a real medium
//! ([`store`], [`managed`]), the kernel's scope tree for the life of a work
//! ([`works`]), `CLOCK_QUERY` for time, and a real kernel transport -- a
//! virtio-console function the kernel assigned, driven from user space
//! ([`virtio`]) -- for the managed messages a consumer sends.
//!
//! The consumer is the real Thalyx, unmodified, running on Linux and reaching
//! this machine over the socket QEMU exposes for the console. Its managed
//! client cannot tell this store from the loopback one: the conversation is
//! byte for byte `thalyx_platform::managed::protocol`, which is exactly the
//! point -- the same Thalyx, over the same boundary, on a different machine.
//!
//! What is deliberately **not** here: Thalyx's verbs, QuickJS, the parser, the
//! semantic provider, the tools, the answer. Those are Thalyx, and they run on
//! the host, which is where L1 runs them too. The launcher and validation the
//! boundary's `ProgramLaunch` names are exercised natively by K5's own
//! `nhacer`/`ncheck`/`nengine` and are not re-implemented here; a Thalyx
//! transaction that validates over the wire runs its tool where L1 runs it and
//! this domain records the verdict as its receipt, which is what keeps the
//! semantic coordinator identical between L1 and K1.

#![no_std]
#![no_main]

mod json;
mod managed;
mod store;
mod virtio;
mod works;

use thalyx_abi::{boot_handle, right};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k4fmt::pkg::bit as k4bit;
use thalyx_user_k5pkg::link::{
    CONTROL_PORT, FRAME_MAX, LINK_MAGIC, LinkConfig, MAX_PORTS, link_addr, link_slot, note,
};
use thalyx_user_rt::k2::{self, report};
use thalyx_user_rt::{self as rt, entry};

use managed::Managed;
use virtio::Console;
use works::Works;

/// Base of this program's FP pattern.
const FP_BASE: u64 = 0x1155_5566_6600_0001;

/// A worker port's frame reassembly. One four-byte length prefix and then that
/// many bytes, exactly the bridge's grammar (`thalyx_bridge::write_frame`).
struct Assembly {
    buffer: [u8; FRAME_MAX + 4],
    have: usize,
}

impl Assembly {
    const EMPTY: Assembly = Assembly {
        buffer: [0; FRAME_MAX + 4],
        have: 0,
    };

    fn push(&mut self, bytes: &[u8]) -> bool {
        if self.have + bytes.len() > self.buffer.len() {
            return false;
        }
        self.buffer[self.have..self.have + bytes.len()].copy_from_slice(bytes);
        self.have += bytes.len();
        true
    }

    /// The next complete frame's body, if one is buffered.
    fn frame(&self) -> Option<(usize, usize)> {
        if self.have < 4 {
            return None;
        }
        let length = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        if length > FRAME_MAX {
            return None;
        }
        if self.have < 4 + length {
            return None;
        }
        Some((4, length))
    }

    /// Drops a consumed frame, shifting the tail down.
    fn consume(&mut self, length: usize) {
        let end = 4 + length;
        self.buffer.copy_within(end..self.have, 0);
        self.have -= end;
    }
}

/// The reassembly buffers, the reply buffer and one out-frame, in `.bss`.
struct Buffers {
    ports: [Assembly; MAX_PORTS as usize + 1],
    reply: [u8; FRAME_MAX],
    outgoing: [u8; FRAME_MAX + 4],
}

static mut BUFFERS: Buffers = Buffers {
    ports: [Assembly::EMPTY; MAX_PORTS as usize + 1],
    reply: [0; FRAME_MAX],
    outgoing: [0; FRAME_MAX + 4],
};

#[allow(clippy::deref_addrof)]
fn buffers() -> &'static mut Buffers {
    // SAFETY: single-threaded domain; every borrow is dropped before the next.
    unsafe { &mut *(&raw mut BUFFERS) }
}

fn config() -> Option<LinkConfig> {
    // SAFETY: the supervisor maps one read-only page here before activation.
    let bytes = unsafe {
        core::slice::from_raw_parts(link_addr::CONFIG as *const u8, size_of::<LinkConfig>())
    };
    let config = LinkConfig::read_from(bytes, 0)?;
    (config.magic == LINK_MAGIC).then_some(config)
}

/// The line a worker port serves, and the principal it serves as.
fn mapping(port_line: &[u32], port: u32) -> (usize, u32) {
    let line = port_line[port as usize];
    let line = if line == 0 { port } else { line };
    (line as usize, port)
}

/// Frames a reply body into the outgoing buffer and sends it on `port`.
fn send_reply(console: &mut Console, port: u32, body_len: usize) -> bool {
    let out = &mut buffers().outgoing;
    out[..4].copy_from_slice(&(body_len as u32).to_le_bytes());
    out[4..4 + body_len].copy_from_slice(&buffers().reply[..body_len]);
    let sent = console.send(port, &buffers().outgoing[..4 + body_len]);
    if sent {
        k2::note(note::FRAME_OUT, u64::from(port) | ((body_len as u64) << 8));
    }
    sent
}

/// Handles one control-port frame. Answers a small JSON reply and says whether
/// the run should end.
fn control_into(
    request: &[u8],
    store: &mut Managed,
    works: &mut Works,
    port_line: &mut [u32],
    out: &mut [u8],
) -> (usize, bool) {
    let mut writer = json::Writer::new(out);
    let op = json::string(request, "op").unwrap_or(b"");
    k2::note(note::CONTROL, u64::from(*op.first().unwrap_or(&0)));
    let mut shutdown = false;
    match op {
        b"bind" => {
            // Remap a worker port to a line, so the harness can put two ports
            // on one line and make two principals contend on one generation.
            // Refused once the port has served, so a live session's line never
            // moves under it.
            let port = json::number(request, "port").unwrap_or(0) as usize;
            let line = json::number(request, "line").unwrap_or(0) as u32;
            if port == 0 || port >= port_line.len() {
                writer.raw(b"{\"ok\":false,\"why\":\"no such port\"}");
            } else {
                port_line[port] = line;
                writer.raw(b"{\"ok\":true}");
            }
        }
        b"fence" => {
            let line = json::number(request, "line").unwrap_or(0) as u32;
            match works.work_of_line(line) {
                Some(work) if works.fence(work) => {
                    writer
                        .raw(b"{\"ok\":true,\"fenced\":")
                        .number(work)
                        .raw(b"}");
                }
                _ => {
                    writer.raw(b"{\"ok\":false,\"why\":\"no open work on that line\"}");
                }
            }
        }
        b"admit" => {
            // Whether a line's open work could still publish: what a fenced
            // work answers is the whole point of the gate.
            let line = json::number(request, "line").unwrap_or(0) as u32;
            let principal = json::number(request, "principal").unwrap_or(u64::from(line)) as u32;
            let name = json::string(request, "transaction").unwrap_or(b"");
            match store.work_admit(line as usize, principal, name) {
                Ok(()) => {
                    writer.raw(b"{\"admitted\":true}");
                }
                Err(why) => {
                    writer
                        .raw(b"{\"admitted\":false,\"why\":")
                        .str(why)
                        .raw(b"}");
                }
            }
        }
        b"stats" => {
            writer
                .raw(b"{\"ok\":true,\"store_calls\":")
                .number(store.store_calls)
                .raw(b",\"publications\":")
                .number(store.backing.publications)
                .raw(b",\"compactions\":")
                .number(store.backing.compactions)
                .raw(b",\"works_opened\":")
                .number(works.opened)
                .raw(b",\"works_fenced\":")
                .number(works.fenced)
                .raw(b",\"works_retired\":")
                .number(works.retired)
                .raw(b",\"k4_generation\":")
                .number(store.backing.k4_generation)
                .raw(b"}");
        }
        b"shutdown" => {
            shutdown = true;
            k2::note(note::SHUTDOWN, works.opened);
            writer.raw(b"{\"ok\":true}");
        }
        _ => {
            writer.raw(b"{\"ok\":false,\"why\":\"unknown control op\"}");
        }
    }
    (writer.finish().unwrap_or(0), shutdown)
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let device = boot_handle(link_slot::DEVICE);
    let irq = boot_handle(link_slot::IRQ);
    let ring_cap = boot_handle(link_slot::RING);
    let data_cap = boot_handle(link_slot::DATA);
    let stage_cap = boot_handle(link_slot::STAGE);
    let k4_facet = boot_handle(link_slot::STORE);
    let work_parent = boot_handle(link_slot::WORK_PARENT);
    let self_scope = boot_handle(link_slot::SELF_SCOPE);
    let ready = boot_handle(link_slot::READY);
    let _ = boot_handle(link_slot::LOG);

    let Some(config) = config() else {
        k2::note(note::LINK_UNEXPECTED, 0x9001);
        rt::exit(1)
    };

    let info = match k2::device_query(device) {
        Ok(info) => info,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(2)
        }
    };
    let session = info.session;

    // The rings and the data buffers, granted to the device. A console function
    // reads and writes both, so both are granted read-write.
    let ring_grant = match k2::device_dma_map(
        device,
        ring_cap,
        0,
        link_addr::RING_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
        0,
        session,
    ) {
        Ok(grant) => grant,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(3)
        }
    };
    let data_grant = match k2::device_dma_map(
        device,
        data_cap,
        0,
        thalyx_user_k5pkg::link::DATA_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
        0,
        session,
    ) {
        Ok(grant) => grant,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(4)
        }
    };

    let region = &link_addr::REGION;
    let Some(mut console) = Console::initialise(
        region[0],
        region[1],
        info.regions[1].notify_off_multiplier,
        region[3],
        irq,
        ring_grant.iova,
        data_grant.iova,
    ) else {
        k2::note(note::LINK_UNEXPECTED, 0x9002);
        rt::exit(5)
    };
    // Bus mastering is the assigner's, not the driver's; the supervisor turned
    // it on after building this domain. The device query above already showed
    // it, so nothing here touches configuration space.
    let _ = k2::device_set_master;

    let mut store = Managed::new(config.seed, k4_facet, stage_cap);
    if !store.recover() {
        k2::note(note::LINK_UNEXPECTED, 0x9003);
    }

    // Work scopes: one child scope per Thalyx transaction, under the parent the
    // supervisor gave this link `SCOPE_CREATE` on. A budget window and a page
    // ceiling per work, both drawn from the parent.
    let (work_budget, work_pages) = match k2::scope_query(work_parent) {
        Ok(parent) => (
            (parent.limits.cpu_budget_ns / 4).max(1_000_000),
            (parent.limits.memory_pages / 4).max(16),
        ),
        Err(_) => (10_000_000, 32),
    };
    let mut works = Works::new(work_parent, work_budget, work_pages);
    let _ = self_scope;

    if let Err(code) = k2::signal_raise(ready, k4bit::READY) {
        k2::note(report::UNEXPECTED, code as u64);
    }
    k2::note(note::LINK_READY, u64::from(console.max_ports));

    let mut port_line = config.port_line;
    let mut running = true;
    while running {
        let Some(received) = console.receive(k2::now_ns() + 1_000_000_000) else {
            continue;
        };
        let port = received.port;
        if port == 0 || port > MAX_PORTS {
            console.repost(port, received.buffer);
            continue;
        }
        k2::note(
            note::FRAME_IN,
            u64::from(port) | ((received.len as u64) << 8),
        );
        let overflowed = {
            // `rx_bytes` borrows the console; the assembly is a different
            // static, so appending to it here is sound, and the borrow ends
            // before the repost below.
            let bytes = console.rx_bytes(port, received.buffer, received.len);
            !buffers().ports[port as usize].push(bytes)
        };
        console.repost(port, received.buffer);
        if overflowed {
            k2::note(note::LINK_UNEXPECTED, 0x9004);
            buffers().ports[port as usize].have = 0;
            continue;
        }

        loop {
            let Some((offset, length)) = buffers().ports[port as usize].frame() else {
                break;
            };
            // The request and the reply are disjoint fields of one struct, so
            // one is read while the other is written.
            let (len, shutdown) = {
                let b = buffers();
                let (ports, reply) = (&b.ports, &mut b.reply);
                let request = &ports[port as usize].buffer[offset..offset + length];
                if port == CONTROL_PORT {
                    control_into(request, &mut store, &mut works, &mut port_line, reply)
                } else {
                    let (line, principal) = mapping(&port_line, port);
                    let len = store
                        .serve(line, principal, request, &mut works, reply)
                        .unwrap_or(0);
                    (len, false)
                }
            };
            buffers().ports[port as usize].consume(length);
            if shutdown {
                running = false;
            }
            if len == 0 {
                k2::note(note::LINK_UNEXPECTED, 0x9005);
            } else if !send_reply(&mut console, port, len) {
                running = false;
                break;
            }
            if !running {
                break;
            }
        }
    }

    k2::note(note::FRAMES, console.rx_frames);
    rt::exit(0)
}

entry!(run);
