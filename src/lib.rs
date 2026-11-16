//! PulseForge: a hardware and system monitor for Windows that needs no kernel driver.
//!
//! The logic lives in this library so that it can be unit-tested; `main.rs`
//! only parses the command line and opens the window.
pub mod alerts;
pub mod collect;
pub mod csvlog;
pub mod demo;
pub mod history;
pub mod model;
pub mod report;
pub mod sampler;
pub mod selftest;
pub mod settings;
pub mod sys;
pub mod tray;
pub mod ui;
pub mod util;
