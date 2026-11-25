//! Library-first bare-metal real-time Rust runtime on Windows Hypervisor Platform.
#![deny(unsafe_code)]

mod backend;
mod error;
mod io;
mod loader;

#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // WHP FFI and guest RAM mapping are fenced into this module only.
mod whp;

use std::borrow::Cow;

use backend::{HypervisorBackend, Regs, VmExit};

pub use backend::PAGE_SIZE;
pub use error::{Error, Result};
pub use io::STDOUT_PORT;
pub use loader::ElfImage;

/// Default guest RAM allocation (16 MiB).
pub const DEFAULT_RAM_BYTES: usize = 16 * 1024 * 1024;

#[cfg(target_os = "windows")]
type ActiveVm = whp::Vm;

/// Configuration for booting a bare-metal guest [`Instance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Total guest physical RAM in bytes (must be a 2 MiB multiple between 4 MiB and 1 GiB).
    pub ram_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ram_bytes: DEFAULT_RAM_BYTES,
        }
    }
}

/// Terminal outcome of a guest execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// Guest halted cleanly via `hlt`.
    Halted,
    /// Guest triggered a VM exit not handled by the runtime yet.
    Unhandled { code: i32 },
}

/// Captured output and exit status from running an [`Instance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// Raw bytes emitted by the guest to [`STDOUT_PORT`].
    pub stdout: Vec<u8>,
    /// Why guest execution stopped.
    pub exit: ExitStatus,
}

impl RunOutput {
    /// Decodes captured stdout as UTF-8, replacing invalid sequences.
    pub fn stdout_str(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }
}

/// Returns whether hardware virtualization is available on this host.
pub fn is_available() -> bool {
    ActiveVm::is_available()
}

/// Loaded bare-metal guest instance ready to execute on hardware.
pub struct Instance {
    vm: ActiveVm,
}

impl Instance {
    /// Allocates guest RAM, installs 64-bit long-mode page tables, and loads `elf`.
    pub fn boot(elf: &ElfImage, config: Config) -> Result<Self> {
        whp::paging::validate_ram_bytes(config.ram_bytes)?;
        elf.validate_for_ram(config.ram_bytes, whp::paging::RESERVED_LOW_END)?;

        let mut vm = ActiveVm::create(config.ram_bytes)?;
        let sregs = whp::paging::install(&mut vm, config.ram_bytes)?;
        elf.load_into(&mut vm)?;
        vm.set_special_regs(&sregs)?;
        vm.set_regs(&Regs {
            rip: elf.entry(),
            rsp: whp::paging::INITIAL_STACK_TOP,
            ..Regs::default()
        })?;
        Ok(Self { vm })
    }

    /// Executes the guest on the physical CPU until it halts or hits an unhandled exit.
    pub fn run(&mut self) -> Result<RunOutput> {
        let mut channel = io::Channel::new();
        loop {
            match self.vm.run()? {
                VmExit::IoOut {
                    port,
                    size_bytes,
                    value,
                } => {
                    channel.write_port(port, size_bytes, value);
                }
                VmExit::Halt => {
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit: ExitStatus::Halted,
                    });
                }
                VmExit::Unhandled { code } => {
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit: ExitStatus::Unhandled { code },
                    });
                }
            }
        }
    }
}
