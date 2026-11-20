//! WHP partition lifecycle and guest physical memory mapping.

use std::sync::{Mutex, PoisonError};

use windows::Win32::System::Hypervisor::{
    WHV_CAPABILITY, WHV_PARTITION_HANDLE, WHV_PARTITION_PROPERTY,
    WHvCapabilityCodeHypervisorPresent, WHvCreatePartition, WHvDeletePartition, WHvGetCapability,
    WHvMapGpaRange, WHvMapGpaRangeFlagExecute, WHvMapGpaRangeFlagRead, WHvMapGpaRangeFlagWrite,
    WHvPartitionPropertyCodeProcessorCount, WHvSetPartitionProperty, WHvSetupPartition,
};

use super::failed;
use super::memory::Memory;
use crate::backend::GuestAddr;
use crate::error::Result;

static SETUP_LOCK: Mutex<()> = Mutex::new(());

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

        // VID assigns its internal partition identity during setup; concurrent setups in one
        // process can collide, surfacing later as STATUS_VID_PARTITION_ALREADY_EXISTS
        // (0xC0370008) from WHvMapGpaRange. Serializing setup alone removes the race.
        let _setup = SETUP_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
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
