//! Interrupt and exception entry.
//!
//! Every vector, and the `syscall` instruction, converge on one frame layout
//! and one return path. That is deliberate: it means a thread can be switched
//! away from at any of those points and resumed later through the same code,
//! and it means the kernel has exactly one place where control returns to ring
//! 3.
//!
//! Both directions of that boundary exchange `GS`, and the exchange is
//! conditional on where the entry came from. An entry from ring 3 swaps, so the
//! per-processor block is reachable; an entry from ring 0 does not, because the
//! block is already loaded and swapping would install the user's base inside
//! the kernel. The condition is read from the code selector the processor
//! itself pushed, which is the only account of the origin that ring 3 cannot
//! write. The return path applies the same test to the selector it is about to
//! restore.
//!
//! The `syscall` instruction performs no stack switch, so the kernel does it,
//! and the two instructions that do it need somewhere to put the interrupted
//! user stack pointer. That slot is per-processor, reached through `GS` after
//! the swap, which is what makes the window safe with more than one processor
//! in the machine.

use crate::arch::x86_64::gdt;
use crate::percpu;

/// Vector the local APIC timer is programmed to raise.
pub const TIMER_VECTOR: u8 = 0x40;
/// Vector one processor sends another to make it invalidate translations.
pub const TLB_VECTOR: u8 = 0x41;
/// Vector one processor sends another to make it reconsider what to run.
pub const RESCHEDULE_VECTOR: u8 = 0x42;
/// First vector assigned to a device interrupt.
///
/// Device vectors are a separate range from the exceptions, the timer and the
/// interprocessor vectors above, so a vector is never both a device's and the
/// kernel's, and a stale device interrupt cannot be mistaken for either.
pub const DEVICE_VECTOR_BASE: u8 = 0x50;
/// Device vectors available.
pub const DEVICE_VECTOR_COUNT: u8 = 8;
/// Vector programmed into the local APIC spurious-interrupt register.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

const _: () = assert!(DEVICE_VECTOR_BASE > RESCHEDULE_VECTOR);
const _: () = assert!(DEVICE_VECTOR_BASE as u16 + DEVICE_VECTOR_COUNT as u16 <= 0xFF);
/// Pseudo-vector recorded for a `syscall` entry. Deliberately outside the
/// 0..=255 interrupt range so a frame's origin is never ambiguous.
pub const SYSCALL_VECTOR: u64 = 0x100;

/// Bytes reserved for each generated entry stub.
pub const STUB_STRIDE: usize = 16;

/// Saved machine state at a kernel entry.
///
/// Field order is the memory order the entry stubs build, lowest address
/// first. Changing either without the other silently corrupts every trap.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    /// Saved RAX.
    pub rax: u64,
    /// Saved RBX.
    pub rbx: u64,
    /// Saved RCX. Destroyed by `syscall`, which stores the return address here.
    pub rcx: u64,
    /// Saved RDX.
    pub rdx: u64,
    /// Saved RSI.
    pub rsi: u64,
    /// Saved RDI.
    pub rdi: u64,
    /// Saved RBP.
    pub rbp: u64,
    /// Saved R8.
    pub r8: u64,
    /// Saved R9.
    pub r9: u64,
    /// Saved R10.
    pub r10: u64,
    /// Saved R11. Destroyed by `syscall`, which stores RFLAGS here.
    pub r11: u64,
    /// Saved R12.
    pub r12: u64,
    /// Saved R13.
    pub r13: u64,
    /// Saved R14.
    pub r14: u64,
    /// Saved R15.
    pub r15: u64,
    /// Interrupt vector, or [`SYSCALL_VECTOR`].
    pub vector: u64,
    /// CPU-pushed error code, or zero for vectors that push none.
    pub error_code: u64,
    /// Interrupted instruction pointer.
    pub rip: u64,
    /// Interrupted code selector. Its low two bits are the privilege level the
    /// entry came from, which is how the kernel classifies a fault.
    pub cs: u64,
    /// Interrupted flags.
    pub rflags: u64,
    /// Interrupted stack pointer.
    pub rsp: u64,
    /// Interrupted stack selector.
    pub ss: u64,
}

const _: () = assert!(core::mem::size_of::<TrapFrame>() == 176);
/// Byte offset of [`TrapFrame::cs`] from the start of the frame, which is where
/// the return path stands when it decides whether to exchange `GS`.
const FRAME_CS_OFFSET: usize = 144;
const _: () = assert!(core::mem::offset_of!(TrapFrame, cs) == FRAME_CS_OFFSET);
const FRAME_RIP_OFFSET: usize = 136;
const _: () = assert!(core::mem::offset_of!(TrapFrame, rip) == FRAME_RIP_OFFSET);
const FRAME_RFLAGS_OFFSET: usize = 152;
const _: () = assert!(core::mem::offset_of!(TrapFrame, rflags) == FRAME_RFLAGS_OFFSET);
const FRAME_SS_OFFSET: usize = 168;
const _: () = assert!(core::mem::offset_of!(TrapFrame, ss) == FRAME_SS_OFFSET);
/// RFLAGS bits that make `sysretq` the wrong instruction: RF would resume a
/// debug fault the kernel never took, and VM is not a state a 64-bit return
/// may enter.
const SYSRET_FORBIDDEN_FLAGS: u64 = (1 << 16) | (1 << 17);
// The entry stub pushes 22 quadwords onto a 16-byte-aligned stack, so the
// System V requirement that RSP is 16-byte aligned at a `call` holds without a
// fixup.
const _: () = assert!(core::mem::size_of::<TrapFrame>() % 16 == 0);

impl TrapFrame {
    /// Privilege level the entry came from.
    #[must_use]
    pub const fn cpl(&self) -> u8 {
        (self.cs & 3) as u8
    }

    /// True when the entry came from ring 3.
    #[must_use]
    pub const fn from_user(&self) -> bool {
        self.cpl() == 3
    }
}

unsafe extern "C" {
    /// First byte of the generated entry stub table.
    static thalyx_trap_stubs: u8;
    /// Entry point installed in `IA32_LSTAR`.
    pub fn thalyx_syscall_entry();
}

/// Address of the entry stub for `vector`.
#[must_use]
pub fn stub_address(vector: usize) -> u64 {
    (&raw const thalyx_trap_stubs) as u64 + (vector * STUB_STRIDE) as u64
}

// Vectors that push an error code: #DF(8), #TS(10), #NP(11), #SS(12), #GP(13),
// #PF(14), #AC(17), #CP(21), #VC(29), #SX(30).
const ERROR_CODE_VECTORS: u64 = 0x6022_7D00;
const _: () = assert!(
    ERROR_CODE_VECTORS
        == (1 << 8)
            | (1 << 10)
            | (1 << 11)
            | (1 << 12)
            | (1 << 13)
            | (1 << 14)
            | (1 << 17)
            | (1 << 21)
            | (1 << 29)
            | (1 << 30)
);

core::arch::global_asm!(
    ".text",
    ".global thalyx_trap_stubs",
    ".global thalyx_syscall_entry",
    ".balign 16",
    "thalyx_trap_stubs:",
    ".set thalyx_vec, 0",
    ".rept 256",
    ".balign 16",
    // Push a zero where the CPU pushes no error code, so the frame layout is
    // the same for every vector. The immediates are emitted as bytes because
    // an assembler-level symbol operand to `push` is ambiguous in Intel syntax.
    ".if thalyx_vec < 32",
    ".if ((0x60227D00 >> thalyx_vec) & 1) == 0",
    ".byte 0x6a, 0x00",
    ".endif",
    ".else",
    ".byte 0x6a, 0x00",
    ".endif",
    ".if thalyx_vec < 128",
    ".byte 0x6a",
    ".byte thalyx_vec",
    ".else",
    ".byte 0x68",
    ".long thalyx_vec",
    ".endif",
    "jmp thalyx_trap_common",
    ".set thalyx_vec, thalyx_vec + 1",
    ".endr",

    "thalyx_trap_common:",
    // The vector and error code are already pushed, so the selector the
    // processor pushed sits three quadwords up. Its low two bits are the
    // privilege level the entry came from.
    "test byte ptr [rsp + 24], 3",
    "jz 2f",
    "swapgs",
    "2:",
    "push r15",
    "push r14",
    "push r13",
    "push r12",
    "push r11",
    "push r10",
    "push r9",
    "push r8",
    "push rbp",
    "push rdi",
    "push rsi",
    "push rdx",
    "push rcx",
    "push rbx",
    "push rax",
    // System V requires the direction flag clear on entry to compiled code and
    // an interrupt gate does not clear it.
    "cld",
    "mov rdi, rsp",
    "call {trap_dispatch}",
    // Falls through into the shared return path.

    ".global thalyx_trap_return",
    "thalyx_trap_return:",
    "test byte ptr [rsp + {frame_cs}], 3",
    "jz 3f",
    "swapgs",
    "3:",
    "pop rax",
    "pop rbx",
    "pop rcx",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop rbp",
    "pop r8",
    "pop r9",
    "pop r10",
    "pop r11",
    "pop r12",
    "pop r13",
    "pop r14",
    "pop r15",
    "add rsp, 16",
    "iretq",

    // `syscall` performs no stack switch, so the kernel does it. Interrupts are
    // masked here by IA32_FMASK, and the scratch slot the switch uses belongs
    // to this processor alone, which is what keeps the window safe when other
    // processors are running.
    ".balign 16",
    "thalyx_syscall_entry:",
    "swapgs",
    "mov gs:[{user_rsp}], rsp",
    "mov rsp, gs:[{kernel_rsp}]",
    "push {user_ss}",
    "push qword ptr gs:[{user_rsp}]",
    "push r11",
    "push {user_cs}",
    "push rcx",
    "push 0",
    "push {syscall_vector}",
    "push r15",
    "push r14",
    "push r13",
    "push r12",
    "push r11",
    "push r10",
    "push r9",
    "push r8",
    "push rbp",
    "push rdi",
    "push rsi",
    "push rdx",
    "push rcx",
    "push rbx",
    "push rax",
    "cld",
    "mov rdi, rsp",
    "call {syscall_dispatch}",

    // The fast return. `sysretq` is two or three times cheaper than `iretq`
    // on this class of processor, and it is safe exactly when the frame still
    // describes the return `syscall` set up: to the one user code selector
    // `IA32_STAR` makes it return to, with the matching stack selector, to a
    // canonical address, with no flag the instruction must not restore. Each
    // of those is checked here rather than assumed, and a frame that fails any
    // of them -- a handler that rewrote the return, a signal-like redirection,
    // anything that is not a plain return to the caller -- leaves through
    // `iretq`, which checks everything the processor can check.
    //
    // The canonical test is the reason the guard exists at all: `sysretq` with
    // a non-canonical RIP raises #GP *in ring 0* on Intel parts, on the
    // kernel stack the return was leaving, which is the classic way a kernel
    // turns a user-controlled address into a kernel fault. RAX is used as the
    // scratch because it is restored from the frame a few instructions later.
    "mov rax, [rsp + {frame_cs}]",
    "cmp rax, {user_cs}",
    "jne thalyx_trap_return",
    "mov rax, [rsp + {frame_ss}]",
    "cmp rax, {user_ss}",
    "jne thalyx_trap_return",
    "mov rax, [rsp + {frame_rflags}]",
    "test rax, {forbidden_flags}",
    "jnz thalyx_trap_return",
    "mov rax, [rsp + {frame_rip}]",
    "mov rcx, rax",
    "shl rcx, 16",
    "sar rcx, 16",
    "cmp rcx, rax",
    "jne thalyx_trap_return",

    // Committed. The processor came from ring 3, so the kernel's GS goes back
    // before the registers do; `sysretq` performs no swap of its own.
    "swapgs",
    "pop rax",
    "pop rbx",
    "pop rcx",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop rbp",
    "pop r8",
    "pop r9",
    "pop r10",
    "pop r11",
    "pop r12",
    "pop r13",
    "pop r14",
    "pop r15",
    "add rsp, 16",
    // RCX and R11 are the instruction's operands, not the caller's registers:
    // `syscall` destroyed both on entry, so restoring them from the frame's
    // general-purpose slots and then overwriting them here loses nothing the
    // caller still owned. They are read from the frame rather than kept from
    // the pops so that a handler which legitimately changed the return address
    // or the flags is honoured.
    "mov rcx, [rsp]",
    "mov r11, [rsp + 16]",
    "mov rsp, [rsp + 24]",
    "sysretq",

    trap_dispatch = sym trap_dispatch,
    syscall_dispatch = sym crate::arch::x86_64::syscall::syscall_dispatch,
    user_rsp = const percpu::OFFSET_USER_RSP,
    kernel_rsp = const percpu::OFFSET_KERNEL_RSP,
    frame_cs = const FRAME_CS_OFFSET,
    frame_rip = const FRAME_RIP_OFFSET,
    frame_rflags = const FRAME_RFLAGS_OFFSET,
    frame_ss = const FRAME_SS_OFFSET,
    forbidden_flags = const SYSRET_FORBIDDEN_FLAGS,
    user_ss = const gdt::USER_DATA as u64,
    user_cs = const gdt::USER_CODE as u64,
    syscall_vector = const SYSCALL_VECTOR,
);

/// Points this processor's `syscall` entry at a thread's kernel stack.
///
/// # Safety
///
/// `top` must be the top of the mapped kernel stack of the thread about to run
/// on this processor, and interrupts must be masked.
pub unsafe fn set_syscall_stack(top: u64) {
    // SAFETY: writes this processor's own per-processor block, whose only other
    // reader is this processor's own entry stub.
    unsafe { percpu::set_kernel_rsp(top) };
}

/// Dispatches one interrupt or exception. Called from the shared entry stub
/// with interrupts masked.
extern "sysv64" fn trap_dispatch(frame: *mut TrapFrame) {
    // SAFETY: the entry stub built the frame on the current kernel stack and
    // passes a pointer to it. It stays valid and exclusively owned for the
    // duration of this call.
    let frame = unsafe { &mut *frame };
    crate::trap::handle(frame);
}
