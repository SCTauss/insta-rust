//! Library-first bare-metal real-time Rust runtime on Windows Hypervisor Platform.
#![deny(unsafe_code)]
// M1 ships the backend without a public facade; the M2 library API becomes its consumer.
#![cfg_attr(not(test), allow(dead_code))]

mod backend;
mod error;

#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // WHP FFI and guest RAM mapping are fenced into this module only.
mod whp;
