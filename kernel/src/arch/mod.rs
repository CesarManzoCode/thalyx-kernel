//! Architecture support.
//!
//! K1 targets one architecture. The module exists so the boundary is a real one
//! from the first commit: common code names `arch::x86_64` explicitly rather
//! than being written as if there were nothing below it, and adding a second
//! architecture is a matter of introducing the selection here, not of
//! untangling the core.

pub mod x86_64;
