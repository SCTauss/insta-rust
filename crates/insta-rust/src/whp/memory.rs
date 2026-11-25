//! Page-aligned host allocation that backs guest physical RAM.

use core::ffi::c_void;
use core::ptr::NonNull;

use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

use crate::backend::{GuestAddr, PAGE_SIZE};
use crate::error::{Error, Result};

/// Committed, zero-initialized, page-aligned host memory owned exclusively by one VM.
pub struct Memory {
    ptr: NonNull<u8>,
    size_bytes: usize,
}

impl Memory {
    /// Commits `size_bytes` of zeroed RAM; the size must be a non-zero page multiple.
    pub fn new(size_bytes: usize) -> Result<Self> {
        if size_bytes == 0 || !size_bytes.is_multiple_of(PAGE_SIZE) {
            return Err(Error::BadSize { size_bytes });
        }
        // SAFETY: requests a fresh region at an OS-chosen address; no existing memory is touched.
        let raw =
            unsafe { VirtualAlloc(None, size_bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
        let ptr = NonNull::new(raw.cast::<u8>()).ok_or(Error::OutOfMemory { size_bytes })?;
        Ok(Self { ptr, size_bytes })
    }

    /// Host virtual address of the first byte (always page-aligned).
    pub fn as_ptr(&self) -> *const c_void {
        self.ptr.as_ptr().cast_const().cast()
    }

    /// Length of the region in bytes.
    pub fn size_bytes(&self) -> usize {
        self.size_bytes
    }

    /// Copies `bytes` into RAM at guest physical `addr` (RAM is mapped at GPA 0).
    pub fn write(&mut self, addr: GuestAddr, bytes: &[u8]) -> Result<()> {
        let range = self.checked_range(addr, bytes.len())?;
        self.as_mut_slice()[range].copy_from_slice(bytes);
        Ok(())
    }

    /// Zeroes `len` bytes of RAM starting at guest physical `addr`.
    pub fn zero(&mut self, addr: GuestAddr, len: usize) -> Result<()> {
        let range = self.checked_range(addr, len)?;
        self.as_mut_slice()[range].fill(0);
        Ok(())
    }

    fn checked_range(&self, addr: GuestAddr, len: usize) -> Result<core::ops::Range<usize>> {
        let err = Error::OutOfRange { addr: addr.0, len };
        let start = usize::try_from(addr.0).map_err(|_| err.clone())?;
        let end = start.checked_add(len).ok_or(err.clone())?;
        if end > self.size_bytes {
            return Err(err);
        }
        Ok(start..end)
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` covers `size_bytes` committed bytes owned by `self`. `&mut self`
        // excludes other host borrows, and the guest only runs inside `Vm::run`, which
        // also needs `&mut Vm`, so the guest never writes while this slice is live.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.size_bytes) }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `VirtualAlloc` with `MEM_RESERVE`; `MEM_RELEASE` requires
        // size 0. The owning `Vm` drops its partition first, so WHP no longer maps it.
        let _ = unsafe { VirtualFree(self.ptr.as_ptr().cast(), 0, MEM_RELEASE) };
    }
}
