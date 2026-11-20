//! Windows Hypervisor Platform backend; the only module permitted to use `unsafe`.

mod memory;
mod partition;
#[cfg(test)]
mod tests;
mod vcpu;

use crate::backend::{GuestAddr, HypervisorBackend, Regs, SpecialRegs, VmExit};
use crate::error::{Error, Result};
use memory::Memory;
use partition::Partition;
use vcpu::Vcpu;

/// Single-vCPU WHP virtual machine with host-owned guest RAM at GPA 0.
pub struct Vm {
    // Field order is drop order: the vCPU goes before its partition, and the
    // partition (which holds the GPA mapping) goes before the RAM backing it.
    vcpu: Vcpu,
    _partition: Partition,
    memory: Memory,
}

impl HypervisorBackend for Vm {
    fn is_available() -> bool {
        partition::is_hypervisor_present()
    }

    fn create(ram_bytes: usize) -> Result<Self> {
        if !Self::is_available() {
            return Err(Error::Unavailable);
        }
        let memory = Memory::new(ram_bytes)?;
        let partition = Partition::new()?;
        // SAFETY: `memory` is moved into the returned `Vm` after `partition`, so it is
        // released only after the partition (and its mapping) is deleted. On early
        // return, locals drop in reverse order with the same guarantee.
        unsafe { partition.map(&memory, GuestAddr(0))? };
        // SAFETY: `partition` is set up and is stored after `vcpu` in `Vm`, so it
        // outlives the vCPU.
        let vcpu = unsafe { Vcpu::new(partition.handle(), 0)? };
        Ok(Self {
            vcpu,
            _partition: partition,
            memory,
        })
    }

    fn write(&mut self, addr: GuestAddr, bytes: &[u8]) -> Result<()> {
        self.memory.write(addr, bytes)
    }

    fn set_regs(&mut self, regs: &Regs) -> Result<()> {
        self.vcpu.set_regs(regs)
    }

    fn regs(&self) -> Result<Regs> {
        self.vcpu.regs()
    }

    fn set_special_regs(&mut self, sregs: &SpecialRegs) -> Result<()> {
        self.vcpu.set_special_regs(sregs)
    }

    fn run(&mut self) -> Result<VmExit> {
        self.vcpu.run()
    }
}

/// Maps a failed WHP call to a typed [`Error`] tagged with the API name.
fn failed(call: &'static str) -> impl FnOnce(windows::core::Error) -> Error {
    move |err| Error::Hypervisor {
        call,
        code: err.code().0,
    }
}
