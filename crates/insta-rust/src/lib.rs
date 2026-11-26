//! Library-first bare-metal real-time Rust runtime on Windows Hypervisor Platform.
#![deny(unsafe_code)]

mod backend;
mod error;
pub mod fault;
mod io;
mod loader;
mod watchdog;

#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // WHP FFI and guest RAM mapping are fenced into this module only.
mod whp;

use std::borrow::Cow;
use std::time::{Duration, Instant};

use backend::{HypervisorBackend, VmExit};

pub use backend::{PAGE_SIZE, Regs};
pub use error::{Error, Result};
pub use io::{INPUT_PORT, PANIC_PORT, STDOUT_PORT};
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
    /// Optional execution watchdog deadline; cancels the vCPU if exceeded.
    pub timeout: Option<Duration>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ram_bytes: DEFAULT_RAM_BYTES,
            timeout: None,
        }
    }
}

/// Terminal outcome of a guest execution run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    /// Guest halted cleanly via `hlt`.
    Halted,
    /// Guest panicked or triggered a trapped hardware exception.
    Faulted(fault::Report),
    /// Guest exceeded [`Config::timeout`] and was canceled by the watchdog.
    TimedOut {
        elapsed: Duration,
        regs: Regs,
        location: Option<fault::Location>,
    },
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
    elf_bytes: Vec<u8>,
    timeout: Option<Duration>,
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
        Ok(Self {
            vm,
            elf_bytes: elf.bytes().to_vec(),
            timeout: config.timeout,
        })
    }

    /// Executes the guest on the physical CPU with an empty input stream.
    pub fn run(&mut self) -> Result<RunOutput> {
        self.run_with_input(&[])
    }

    /// Executes the guest on the physical CPU, feeding `input` over [`INPUT_PORT`].
    pub fn run_with_input(&mut self, input: &[u8]) -> Result<RunOutput> {
        self.vm.clear_cancel();
        let start = Instant::now();
        let _timer = self
            .timeout
            .map(|d| watchdog::Timer::arm(d, self.vm.cancel_handle()));
        let mut channel = io::Channel::new(input);

        loop {
            match self.vm.run()? {
                VmExit::IoOut {
                    port,
                    size_bytes,
                    value,
                } => {
                    channel.write_port(port, size_bytes, value);
                }
                VmExit::IoIn { port, size_bytes } => {
                    let value = channel.read_port(port, size_bytes);
                    self.vm.complete_io_in(size_bytes, value)?;
                }
                VmExit::Halt => {
                    drop(_timer);
                    let exit = if let Some(message) = channel.panic_message() {
                        let regs = self.vm.regs()?;
                        let location = fault::symbolize(&self.elf_bytes, regs.rip);
                        ExitStatus::Faulted(fault::Report {
                            reason: fault::Reason::GuestPanic { message },
                            regs,
                            location,
                        })
                    } else {
                        ExitStatus::Halted
                    };
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit,
                    });
                }
                VmExit::Exception {
                    vector,
                    error_code,
                    faulting_addr,
                } => {
                    drop(_timer);
                    let regs = self.vm.regs()?;
                    let location = fault::symbolize(&self.elf_bytes, regs.rip);
                    let reason = fault::classify_exception(vector, error_code, faulting_addr);
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit: ExitStatus::Faulted(fault::Report {
                            reason,
                            regs,
                            location,
                        }),
                    });
                }
                VmExit::MemoryFault { gpa, gva } => {
                    drop(_timer);
                    let regs = self.vm.regs()?;
                    let location = fault::symbolize(&self.elf_bytes, regs.rip);
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit: ExitStatus::Faulted(fault::Report {
                            reason: fault::Reason::MemoryFault { gpa, gva },
                            regs,
                            location,
                        }),
                    });
                }
                VmExit::TripleFault => {
                    drop(_timer);
                    let regs = self.vm.regs()?;
                    let location = fault::symbolize(&self.elf_bytes, regs.rip);
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit: ExitStatus::Faulted(fault::Report {
                            reason: fault::Reason::TripleFault,
                            regs,
                            location,
                        }),
                    });
                }
                VmExit::Canceled => {
                    drop(_timer);
                    let regs = self.vm.regs()?;
                    let location = fault::symbolize(&self.elf_bytes, regs.rip);
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit: ExitStatus::TimedOut {
                            elapsed: start.elapsed(),
                            regs,
                            location,
                        },
                    });
                }
                VmExit::Unhandled { code } => {
                    drop(_timer);
                    return Ok(RunOutput {
                        stdout: channel.into_stdout(),
                        exit: ExitStatus::Unhandled { code },
                    });
                }
            }
        }
    }
}
