//! x86_64 support.
//!
//! Everything that knows about page-table formats, descriptor tables, privilege
//! transitions, model-specific registers or the local APIC lives under this
//! module. Nothing above it does.

pub mod context;
pub mod cpu;
pub mod fpu;
pub mod gdt;
pub mod idt;
pub mod lapic;
pub mod paging;
pub mod pic;
pub mod pit;
pub mod serial;
pub mod syscall;
pub mod trap;
