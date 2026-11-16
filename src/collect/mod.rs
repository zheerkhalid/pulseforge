//! The collectors: every module reads one kind of hardware or system
//! information using only documented user-mode interfaces. No kernel driver
//! is installed or needed; readings that Windows does not expose without one
//! are reported as unavailable, never invented.
//!
//! Each collector is defensive: a failing API call yields an empty or default
//! result (and a note where the model has room for one), not a panic.

pub mod battery;
pub mod cpu;
pub mod displays;
pub mod gpu;
pub mod memory;
pub mod network;
pub mod nvml;
pub mod pdh;
pub mod process;
pub mod sensors;
pub mod smart;
pub mod smbios;
pub mod storage;
pub mod system;
pub mod wmi;
