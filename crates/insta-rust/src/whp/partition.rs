//! WHP partition lifecycle and guest physical memory mapping.

use windows::Win32::System::Hypervisor::{
    WHV_CAPABILITY, WHV_EXTENDED_VM_EXITS, WHV_PARTITION_HANDLE, WHV_PARTITION_PROPERTY,
    WHvCapabilityCodeHypervisorPresent, WHvCreatePartition, WHvDeletePartition, WHvGetCapability,
    WHvMapGpaRange, WHvMapGpaRangeFlagExecute, WHvMapGpaRangeFlagRead, WHvMapGpaRangeFlagWrite,
    WHvPartitionPropertyCodeExceptionExitBitmap, WHvPartitionPropertyCodeExtendedVmExits,
    WHvPartitionPropertyCodeProcessorCount, WHvSetPartitionProperty, WHvSetupPartition,
};

use super::failed;
use super::memory::Memory;
use crate::backend::GuestAddr;
use crate::error::Result;

/// Bit 2 of `WHV_EXTENDED_VM_EXITS` enables `WHvRunVpExitReasonException`.
const EXTENDED_EXIT_EXCEPTION: u64 = 1 << 2;

/// First-chance exception vectors trapped before Triple Fault:
/// `#DE` (0), `#BP` (3), `#UD` (6), `#GP` (13), `#PF` (14).
const TRAPPED_EXCEPTION_BITMAP: u64 = (1 << 0) | (1 << 3) | (1 << 6) | (1 << 13) | (1 << 14);

/// Returns whether WHP reports a usable hypervisor on this host.
pub fn is_hypervisor_present() -> bool {
    let mut cap = WHV_CAPABILITY::default();
    // SAFETY: `cap` is a writable `WHV_CAPABILITY` and the size passed matches it exactly.
    let queried = unsafe {
        WHvGetCapability(
            WHvCapabilityCodeHypervisorPresent,
            (&raw mut cap).cast(),
            size_of::<WHV_CAPABILITY>() as u32,
            None,
        )
    };
    // SAFETY: on success WHP wrote the `HypervisorPresent` member queried above.
    queried.is_ok() && unsafe { cap.HypervisorPresent }.as_bool()
}

/// Owned, set-up WHP partition with a single virtual processor slot.
pub struct Partition {
    handle: WHV_PARTITION_HANDLE,
}

impl Partition {
    /// Creates a partition, sets its processor count to 1, and finalizes setup.
    pub fn new() -> Result<Self> {
        // SAFETY: no preconditions; on success the returned handle is owned by `Self`.
        let handle = unsafe { WHvCreatePartition() }.map_err(failed("WHvCreatePartition"))?;
        let partition = Self { handle };

        let prop = WHV_PARTITION_PROPERTY { ProcessorCount: 1 };
        // SAFETY: `handle` is a fresh, not-yet-set-up partition; `prop` outlives the call and
        // the size covers the `ProcessorCount` member WHP reads for this property code.
        unsafe {
            WHvSetPartitionProperty(
                handle,
                WHvPartitionPropertyCodeProcessorCount,
                (&raw const prop).cast(),
                size_of::<u32>() as u32,
            )
        }
        .map_err(failed("WHvSetPartitionProperty"))?;

        let extended = WHV_PARTITION_PROPERTY {
            ExtendedVmExits: WHV_EXTENDED_VM_EXITS {
                AsUINT64: EXTENDED_EXIT_EXCEPTION,
            },
        };
        // SAFETY: `handle` is not yet set up; `extended` covers the 8-byte `ExtendedVmExits` bitmask.
        unsafe {
            WHvSetPartitionProperty(
                handle,
                WHvPartitionPropertyCodeExtendedVmExits,
                (&raw const extended).cast(),
                size_of::<WHV_EXTENDED_VM_EXITS>() as u32,
            )
        }
        .map_err(failed("WHvSetPartitionProperty(ExtendedVmExits)"))?;

        let bitmap = WHV_PARTITION_PROPERTY {
            ExceptionExitBitmap: TRAPPED_EXCEPTION_BITMAP,
        };
        // SAFETY: `handle` is not yet set up; `bitmap` covers the 8-byte `ExceptionExitBitmap`.
        unsafe {
            WHvSetPartitionProperty(
                handle,
                WHvPartitionPropertyCodeExceptionExitBitmap,
                (&raw const bitmap).cast(),
                size_of::<u64>() as u32,
            )
        }
        .map_err(failed("WHvSetPartitionProperty(ExceptionExitBitmap)"))?;

        // SAFETY: `handle` is valid and all mandatory properties are configured.
        unsafe { WHvSetupPartition(handle) }.map_err(failed("WHvSetupPartition"))?;
        Ok(partition)
    }

    /// Raw handle for vCPU calls; valid for as long as `self` lives.
    pub fn handle(&self) -> WHV_PARTITION_HANDLE {
        self.handle
    }

    /// Maps all of `memory` read/write/execute at guest physical `base`.
    ///
    /// Windows `vid.sys` allows only one partition per host process to hold active
    /// `WHvMapGpaRange` mappings at a time; a second simultaneous mapping fails with
    /// `STATUS_VID_PARTITION_ALREADY_EXISTS` (`0xC0370008`).
    ///
    /// # Safety
    /// `memory` must stay allocated until this partition is dropped.
    pub unsafe fn map(&self, memory: &Memory, base: GuestAddr) -> Result<()> {
        let flags = WHvMapGpaRangeFlagRead | WHvMapGpaRangeFlagWrite | WHvMapGpaRangeFlagExecute;
        // SAFETY: `memory` is page-aligned committed RAM of `size_bytes` (a page multiple),
        // and the caller guarantees it outlives the partition holding this mapping.
        unsafe {
            WHvMapGpaRange(
                self.handle,
                memory.as_ptr(),
                base.0,
                memory.size_bytes() as u64,
                flags,
            )
        }
        .map_err(failed("WHvMapGpaRange"))
    }
}

impl Drop for Partition {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by `self` and is never used after this point.
        let _ = unsafe { WHvDeletePartition(self.handle) };
    }
}
