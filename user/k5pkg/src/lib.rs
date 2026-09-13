//! What the K5 package agrees on: the native target's boot record, the record
//! an image publishes about itself, and the protocols the port speaks.
//!
//! Two languages meet here. The native side is C -- the language runtime, the
//! validation tool and the inference engine are C programs on the
//! `x86_64-thalyx` target -- and the supervisor, the managed-state service and
//! the work driver are Rust on the kernel's own target. Everything they say to
//! each other crosses that seam, so it is written once, in `repr(C)`, and each
//! side's header or module refers back to this one.
//!
//! It is not authority. A slot number names nothing until a supervisor installs
//! a capability there, and an address is reachable only because a mapping was
//! made.

#![no_std]

pub mod generated;
pub mod link;
pub mod native;
pub mod plan;
pub mod proto;
pub mod thalyx;

pub use thalyx_user_k4fmt::Pod;
