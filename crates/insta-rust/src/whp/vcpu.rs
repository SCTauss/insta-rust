//! WHP virtual processor: register state, exception decoding, and cancel-safe run loop.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use windows::Win32::System::Hypervisor::{
    WHV_PARTITION_HANDLE, WHV_REGISTER_NAME, WHV_REGISTER_VALUE, WHV_RUN_VP_EXIT_CONTEXT,
    WHV_X64_SEGMENT_REGISTER, WHV_X64_SEGMENT_REGISTER_0, WHvCancelRunVirtualProcessor,
    WHvCreateVirtualProcessor, WHvDeleteVirtualProcessor, WHvGetVirtualProcessorRegisters,
    WHvRunVirtualProcessor, WHvRunVpExitReasonCanceled, WHvRunVpExitReasonException,
    WHvRunVpExitReasonMemoryAccess, WHvRunVpExitReasonUnrecoverableException,
    WHvRunVpExitReasonX64Halt, WHvRunVpExitReasonX64IoPortAccess, WHvSetVirtualProcessorRegisters,
    WHvX64RegisterCr0, WHvX64RegisterCr2, WHvX64RegisterCr3, WHvX64RegisterCr4, WHvX64RegisterCs,
    WHvX64RegisterDs, WHvX64RegisterEfer, WHvX64RegisterEs, WHvX64RegisterFs, WHvX64RegisterGs,
    WHvX64RegisterR8, WHvX64RegisterR9, WHvX64RegisterR10, WHvX64RegisterR11, WHvX64RegisterR12,
    WHvX64RegisterR13, WHvX64RegisterR14, WHvX64RegisterR15, WHvX64RegisterRax, WHvX64RegisterRbp,
    WHvX64RegisterRbx, WHvX64RegisterRcx, WHvX64RegisterRdi, WHvX64RegisterRdx,
    WHvX64RegisterRflags, WHvX64RegisterRip, WHvX64RegisterRsi, WHvX64RegisterRsp,
    WHvX64RegisterSs,
};

use super::failed;
use crate::backend::{CancelHandle, Regs, Segment, SpecialRegs, VmExit};
use crate::error::Result;

const GPR_NAMES: [WHV_REGISTER_NAME; 18] = [
    WHvX64RegisterRax,
    WHvX64RegisterRbx,
    WHvX64RegisterRcx,
    WHvX64RegisterRdx,
    WHvX64RegisterRsi,
    WHvX64RegisterRdi,
    WHvX64RegisterRsp,
    WHvX64RegisterRbp,
    WHvX64RegisterR8,
    WHvX64RegisterR9,
    WHvX64RegisterR10,
    WHvX64RegisterR11,
    WHvX64RegisterR12,
    WHvX64RegisterR13,
    WHvX64RegisterR14,
    WHvX64RegisterR15,
    WHvX64RegisterRip,
    WHvX64RegisterRflags,
];

const SPECIAL_NAMES: [WHV_REGISTER_NAME; 10] = [
    WHvX64RegisterCs,
    WHvX64RegisterDs,
    WHvX64RegisterEs,
    WHvX64RegisterFs,
    WHvX64RegisterGs,
    WHvX64RegisterSs,
    WHvX64RegisterCr0,
    WHvX64RegisterCr3,
    WHvX64RegisterCr4,
    WHvX64RegisterEfer,
];

/// `WHV_X64_IO_PORT_ACCESS_INFO` bit layout.
const IO_IS_WRITE: u32 = 1;
const IO_SIZE_SHIFT: u32 = 1;
const IO_SIZE_MASK: u32 = 0b111;
const IO_STRING_OP: u32 = 1 << 4;

/// `WHV_VP_EXCEPTION_INFO` bit 0 indicates `ErrorCode` is valid.
const EXCEPTION_ERROR_CODE_VALID: u32 = 1;

/// x86-64 Page Fault (`#PF`) vector.
const VECTOR_PAGE_FAULT: u8 = 14;

/// Low nibble of `WHV_VP_EXIT_CONTEXT::_bitfield` is the exiting instruction length.
const INSTRUCTION_LEN_MASK: u8 = 0x0F;

/// Atomic vCPU run states guarding `WHvCancelRunVirtualProcessor` against enter/exit races.
const STATE_IDLE: u8 = 0;
const STATE_RUNNING: u8 = 1;
const STATE_CANCELING: u8 = 2;

/// Enforces `DECLSPEC_ALIGN(16)` from `WinHvPlatformDefs.h`, which `windows` 0.62 omits
/// from `WHV_UINT128` / `WHV_REGISTER_VALUE` / `WHV_RUN_VP_EXIT_CONTEXT`.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Aligned16<T>(T);

const _: () = {
    assert!(size_of::<Aligned16<WHV_REGISTER_VALUE>>() == 16);
    assert!(align_of::<Aligned16<WHV_REGISTER_VALUE>>() == 16);
    assert!(size_of::<WHV_RUN_VP_EXIT_CONTEXT>() == 224);
};

fn reg64(value: u64) -> Aligned16<WHV_REGISTER_VALUE> {
    let mut reg = WHV_REGISTER_VALUE::default();
    reg.Reg64 = value;
    Aligned16(reg)
}

struct CancelState {
    state: AtomicU8,
    cancel_requested: AtomicBool,
    alive: Mutex<bool>,
    partition: WHV_PARTITION_HANDLE,
    index: u32,
}

// SAFETY: WHP allows `WHvCancelRunVirtualProcessor` from another thread while a vCPU runs;
// `alive` ensures no cancel call touches `partition` during or after `Vcpu::drop`.
unsafe impl Send for CancelState {}
// SAFETY: all fields are atomic or mutex-guarded, and WHP partition handles are thread-safe
// for `WHvCancelRunVirtualProcessor` while `alive` is true.
unsafe impl Sync for CancelState {}

impl CancelHandle for CancelState {
    fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
        let alive = self.alive.lock().unwrap_or_else(PoisonError::into_inner);
        if *alive
            && self
                .state
                .compare_exchange(
                    STATE_RUNNING,
                    STATE_CANCELING,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
        {
            // SAFETY: `alive` is locked and true, so the partition and vCPU are still live.
            let _ = unsafe { WHvCancelRunVirtualProcessor(self.partition, self.index, 0) };
        }
    }
}

/// One virtual processor inside a WHP partition.
pub struct Vcpu {
    partition: WHV_PARTITION_HANDLE,
    index: u32,
    cancel: Arc<CancelState>,
    pending_io_in: Option<(u64, u64)>,
}

impl Vcpu {
    /// Creates virtual processor `index` in `partition`.
    ///
    /// # Safety
    /// `partition` must be a set-up WHP partition that outlives the returned `Vcpu`.
    pub unsafe fn new(partition: WHV_PARTITION_HANDLE, index: u32) -> Result<Self> {
        // SAFETY: caller guarantees `partition` is valid and set up.
        unsafe { WHvCreateVirtualProcessor(partition, index, 0) }
            .map_err(failed("WHvCreateVirtualProcessor"))?;
        let cancel = Arc::new(CancelState {
            state: AtomicU8::new(STATE_IDLE),
            cancel_requested: AtomicBool::new(false),
            alive: Mutex::new(true),
            partition,
            index,
        });
        Ok(Self {
            partition,
            index,
            cancel,
            pending_io_in: None,
        })
    }

    pub fn set_regs(&self, regs: &Regs) -> Result<()> {
        let values = [
            regs.rax,
            regs.rbx,
            regs.rcx,
            regs.rdx,
            regs.rsi,
            regs.rdi,
            regs.rsp,
            regs.rbp,
            regs.r8,
            regs.r9,
            regs.r10,
            regs.r11,
            regs.r12,
            regs.r13,
            regs.r14,
            regs.r15,
            regs.rip,
            regs.rflags,
        ]
        .map(reg64);
        self.set(&GPR_NAMES, &values)
    }

    pub fn regs(&self) -> Result<Regs> {
        let mut values = [Aligned16(WHV_REGISTER_VALUE::default()); GPR_NAMES.len()];
        // SAFETY: `Aligned16<WHV_REGISTER_VALUE>` has the exact size (16) and C alignment (16)
        // of `WHV_REGISTER_VALUE`; both arrays have `GPR_NAMES.len()` elements and `values` is writable.
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                GPR_NAMES.as_ptr(),
                GPR_NAMES.len() as u32,
                values.as_mut_ptr().cast(),
            )
        }
        .map_err(failed("WHvGetVirtualProcessorRegisters"))?;
        // SAFETY: WHP returns every general-purpose register, RIP, and RFLAGS as `Reg64`.
        let [
            rax,
            rbx,
            rcx,
            rdx,
            rsi,
            rdi,
            rsp,
            rbp,
            r8,
            r9,
            r10,
            r11,
            r12,
            r13,
            r14,
            r15,
            rip,
            rflags,
        ] = values.map(|v| unsafe { v.0.Reg64 });
        Ok(Regs {
            rax,
            rbx,
            rcx,
            rdx,
            rsi,
            rdi,
            rsp,
            rbp,
            r8,
            r9,
            r10,
            r11,
            r12,
            r13,
            r14,
            r15,
            rip,
            rflags,
        })
    }

    pub fn set_special_regs(&self, sregs: &SpecialRegs) -> Result<()> {
        let seg = |s: &Segment| Aligned16(WHV_REGISTER_VALUE { Segment: to_whp(s) });
        let values = [
            seg(&sregs.cs),
            seg(&sregs.ds),
            seg(&sregs.es),
            seg(&sregs.fs),
            seg(&sregs.gs),
            seg(&sregs.ss),
            reg64(sregs.cr0),
            reg64(sregs.cr3),
            reg64(sregs.cr4),
            reg64(sregs.efer),
        ];
        self.set(&SPECIAL_NAMES, &values)
    }

    pub fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.cancel) as Arc<dyn CancelHandle>
    }

    pub fn clear_cancel(&self) {
        self.cancel.cancel_requested.store(false, Ordering::SeqCst);
    }

    pub fn complete_io_in(&mut self, size_bytes: u8, value: u32) -> Result<()> {
        let Some((next_rip, old_rax)) = self.pending_io_in.take() else {
            return Ok(());
        };
        let new_rax = match size_bytes {
            1 => (old_rax & !0xFF) | u64::from(value & 0xFF),
            2 => (old_rax & !0xFFFF) | u64::from(value & 0xFFFF),
            _ => u64::from(value),
        };
        self.set(
            &[WHvX64RegisterRax, WHvX64RegisterRip],
            &[reg64(new_rax), reg64(next_rip)],
        )
    }

    /// Blocks while the guest runs on the physical CPU, then decodes the exit.
    pub fn run(&mut self) -> Result<VmExit> {
        loop {
            if self.cancel.cancel_requested.load(Ordering::SeqCst) {
                return Ok(VmExit::Canceled);
            }
            self.cancel.state.store(STATE_RUNNING, Ordering::SeqCst);
            if self.cancel.cancel_requested.load(Ordering::SeqCst) {
                self.cancel.state.store(STATE_IDLE, Ordering::SeqCst);
                return Ok(VmExit::Canceled);
            }

            let mut exit = Aligned16(WHV_RUN_VP_EXIT_CONTEXT::default());
            // SAFETY: `exit` is a writable, 16-byte-aligned `WHV_RUN_VP_EXIT_CONTEXT` of the size
            // passed, and the partition outlives `self` per `new`'s contract.
            let run_res = unsafe {
                WHvRunVirtualProcessor(
                    self.partition,
                    self.index,
                    (&raw mut exit.0).cast(),
                    size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32,
                )
            };
            self.cancel.state.swap(STATE_IDLE, Ordering::SeqCst);
            run_res.map_err(failed("WHvRunVirtualProcessor"))?;

            match exit.0.ExitReason {
                reason if reason == WHvRunVpExitReasonX64Halt => return Ok(VmExit::Halt),
                reason if reason == WHvRunVpExitReasonX64IoPortAccess => {
                    return self.decode_io(&exit.0);
                }
                reason if reason == WHvRunVpExitReasonException => {
                    return self.decode_exception(&exit.0);
                }
                reason if reason == WHvRunVpExitReasonMemoryAccess => {
                    // SAFETY: the exit reason is `MemoryAccess`, so WHP filled `MemoryAccess`.
                    let mem = unsafe { exit.0.Anonymous.MemoryAccess };
                    return Ok(VmExit::MemoryFault {
                        gpa: mem.Gpa,
                        gva: mem.Gva,
                    });
                }
                reason if reason == WHvRunVpExitReasonUnrecoverableException => {
                    return Ok(VmExit::TripleFault);
                }
                reason if reason == WHvRunVpExitReasonCanceled => {
                    if self.cancel.cancel_requested.load(Ordering::SeqCst) {
                        return Ok(VmExit::Canceled);
                    }
                    // Stale cancel queued after a prior exit; drain and continue running.
                }
                reason => return Ok(VmExit::Unhandled { code: reason.0 }),
            }
        }
    }

    fn decode_io(&mut self, exit: &WHV_RUN_VP_EXIT_CONTEXT) -> Result<VmExit> {
        // SAFETY: the exit reason is `X64IoPortAccess`, so WHP filled `IoPortAccess`.
        let io = unsafe { exit.Anonymous.IoPortAccess };
        // SAFETY: `AsUINT32` is a plain integer view; every bit pattern is valid.
        let info = unsafe { io.AccessInfo.AsUINT32 };
        if info & IO_STRING_OP != 0 {
            return Ok(VmExit::Unhandled {
                code: exit.ExitReason.0,
            });
        }
        let size_bytes = ((info >> IO_SIZE_SHIFT) & IO_SIZE_MASK) as u8;
        let len = u64::from(exit.VpContext._bitfield & INSTRUCTION_LEN_MASK);
        let next_rip = exit.VpContext.Rip.wrapping_add(len);

        if info & IO_IS_WRITE == 0 {
            self.pending_io_in = Some((next_rip, io.Rax));
            return Ok(VmExit::IoIn {
                port: io.PortNumber,
                size_bytes,
            });
        }

        let mask = match size_bytes {
            1 => 0xFF,
            2 => 0xFFFF,
            _ => u32::MAX,
        };
        // WHP leaves RIP on the `out`; the host must retire it before re-entering.
        self.set(&[WHvX64RegisterRip], &[reg64(next_rip)])?;

        Ok(VmExit::IoOut {
            port: io.PortNumber,
            size_bytes,
            value: io.Rax as u32 & mask,
        })
    }

    fn decode_exception(&self, exit: &WHV_RUN_VP_EXIT_CONTEXT) -> Result<VmExit> {
        // SAFETY: the exit reason is `Exception`, so WHP filled `VpException`.
        let exc = unsafe { exit.Anonymous.VpException };
        // SAFETY: `AsUINT32` is a plain integer view; every bit pattern is valid.
        let info = unsafe { exc.ExceptionInfo.AsUINT32 };
        let error_code = (info & EXCEPTION_ERROR_CODE_VALID != 0).then_some(exc.ErrorCode);
        let faulting_addr = if exc.ExceptionType == VECTOR_PAGE_FAULT {
            // WHP populates `ExceptionParameter` with the faulting linear address on `#PF`
            // exits without necessarily committing it to architectural `CR2` first.
            let addr = if exc.ExceptionParameter != 0 {
                exc.ExceptionParameter
            } else {
                self.read_cr2().unwrap_or(0)
            };
            Some(addr)
        } else {
            None
        };
        Ok(VmExit::Exception {
            vector: exc.ExceptionType,
            error_code,
            faulting_addr,
        })
    }

    fn read_cr2(&self) -> Result<u64> {
        let mut value = [Aligned16(WHV_REGISTER_VALUE::default())];
        let names = [WHvX64RegisterCr2];
        // SAFETY: `Aligned16<WHV_REGISTER_VALUE>` has the exact size (16) and C alignment (16)
        // of `WHV_REGISTER_VALUE`; `names` and `value` have length 1.
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                names.as_ptr(),
                1,
                value.as_mut_ptr().cast(),
            )
        }
        .map_err(failed("WHvGetVirtualProcessorRegisters(Cr2)"))?;
        // SAFETY: WHP returns CR2 as `Reg64`.
        Ok(unsafe { value[0].0.Reg64 })
    }

    fn set(
        &self,
        names: &[WHV_REGISTER_NAME],
        values: &[Aligned16<WHV_REGISTER_VALUE>],
    ) -> Result<()> {
        debug_assert_eq!(names.len(), values.len());
        // SAFETY: `Aligned16<WHV_REGISTER_VALUE>` has the exact size (16) and C alignment (16)
        // of `WHV_REGISTER_VALUE`; `names` and `values` are equal-length live slices, and the
        // partition outlives `self` per `new`'s contract.
        unsafe {
            WHvSetVirtualProcessorRegisters(
                self.partition,
                self.index,
                names.as_ptr(),
                names.len() as u32,
                values.as_ptr().cast(),
            )
        }
        .map_err(failed("WHvSetVirtualProcessorRegisters"))
    }
}

impl Drop for Vcpu {
    fn drop(&mut self) {
        let mut alive = self
            .cancel
            .alive
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *alive = false;
        drop(alive);
        // SAFETY: `alive` is now false (no concurrent cancel call can run), the partition
        // outlives `self` per `new`'s contract, and `index` is never used again.
        let _ = unsafe { WHvDeleteVirtualProcessor(self.partition, self.index) };
    }
}

/// Packs a [`Segment`] into WHP's descriptor-cache layout.
fn to_whp(seg: &Segment) -> WHV_X64_SEGMENT_REGISTER {
    let attributes = u16::from(seg.kind & 0xF)
        | u16::from(seg.code_data) << 4
        | u16::from(seg.dpl & 0b11) << 5
        | u16::from(seg.present) << 7
        | u16::from(seg.long) << 13
        | u16::from(seg.big) << 14
        | u16::from(seg.granular) << 15;
    WHV_X64_SEGMENT_REGISTER {
        Base: seg.base,
        Limit: seg.limit,
        Selector: seg.selector,
        Anonymous: WHV_X64_SEGMENT_REGISTER_0 {
            Attributes: attributes,
        },
    }
}
