//! Interrupt and exception entry.
//!
//! Every vector, and the `syscall` instruction, converge on one frame layout
//! and one return path. That is deliberate: it means a thread can be switched
//! away from at any of those points and resumed later through the same code,
//! and it means the kernel has exactly one place where control returns to ring
//! 3.

use crate::arch::x86_64::gdt;

/// Vector the local APIC timer is programmed to raise.
pub const TIMER_VECTOR: u8 = 0x40;
/// Vector programmed into the local APIC spurious-interrupt register.
pub const SPURIOUS_VECTOR: u8 = 0xFF;
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
    // masked here by IA32_FMASK, which is what makes the two-instruction window
    // around the scratch slot safe on a uniprocessor machine. The slot is
    // replaced by per-CPU state through GS when SMP arrives.
    ".balign 16",
    "thalyx_syscall_entry:",
    "mov qword ptr [rip + {user_rsp}], rsp",
    "mov rsp, qword ptr [rip + {kernel_rsp}]",
    "push {user_ss}",
    "push qword ptr [rip + {user_rsp}]",
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
    "jmp thalyx_trap_return",

    trap_dispatch = sym trap_dispatch,
    syscall_dispatch = sym crate::arch::x86_64::syscall::syscall_dispatch,
    user_rsp = sym SYSCALL_USER_RSP,
    kernel_rsp = sym SYSCALL_KERNEL_RSP,
    user_ss = const gdt::USER_DATA as u64,
    user_cs = const gdt::USER_CODE as u64,
    syscall_vector = const SYSCALL_VECTOR,
);

/// Scratch slot holding the interrupted user stack pointer between the two
/// instructions of the `syscall` stack switch. Uniprocessor only.
#[unsafe(no_mangle)]
static mut SYSCALL_USER_RSP: u64 = 0;

/// Kernel stack top the `syscall` entry switches to. Kept equal to the TSS
/// ring 0 stack pointer by the context switch.
#[unsafe(no_mangle)]
static mut SYSCALL_KERNEL_RSP: u64 = 0;

/// Points the `syscall` entry at a thread's kernel stack.
///
/// # Safety
///
/// `top` must be the top of the mapped kernel stack of the thread about to run,
/// and interrupts must be masked.
pub unsafe fn set_syscall_stack(top: u64) {
    // SAFETY: uniprocessor with interrupts masked; the only other reader is the
    // assembly entry, which cannot run concurrently.
    unsafe { SYSCALL_KERNEL_RSP = top };
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
