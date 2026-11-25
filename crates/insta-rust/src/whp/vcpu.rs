//! WHP virtual processor: register state and the blocking run loop.

use windows::Win32::System::Hypervisor::{
    WHV_PARTITION_HANDLE, WHV_REGISTER_NAME, WHV_REGISTER_VALUE, WHV_RUN_VP_EXIT_CONTEXT,
    WHV_X64_SEGMENT_REGISTER, WHV_X64_SEGMENT_REGISTER_0, WHvCreateVirtualProcessor,
    WHvDeleteVirtualProcessor, WHvGetVirtualProcessorRegisters, WHvRunVirtualProcessor,
    WHvRunVpExitReasonX64Halt, WHvRunVpExitReasonX64IoPortAccess, WHvSetVirtualProcessorRegisters,
    WHvX64RegisterCr0, WHvX64RegisterCr3, WHvX64RegisterCr4, WHvX64RegisterCs, WHvX64RegisterDs,
    WHvX64RegisterEfer, WHvX64RegisterEs, WHvX64RegisterFs, WHvX64RegisterGs, WHvX64RegisterR8,
    WHvX64RegisterR9, WHvX64RegisterR10, WHvX64RegisterR11, WHvX64RegisterR12, WHvX64RegisterR13,
    WHvX64RegisterR14, WHvX64RegisterR15, WHvX64RegisterRax, WHvX64RegisterRbp, WHvX64RegisterRbx,
    WHvX64RegisterRcx, WHvX64RegisterRdi, WHvX64RegisterRdx, WHvX64RegisterRflags,
    WHvX64RegisterRip, WHvX64RegisterRsi, WHvX64RegisterRsp, WHvX64RegisterSs,
};

use super::failed;
use crate::backend::{Regs, Segment, SpecialRegs, VmExit};
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

/// Low nibble of `WHV_VP_EXIT_CONTEXT::_bitfield` is the exiting instruction length.
const INSTRUCTION_LEN_MASK: u8 = 0x0F;

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

/// One virtual processor inside a WHP partition.
pub struct Vcpu {
    partition: WHV_PARTITION_HANDLE,
    index: u32,
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
        Ok(Self { partition, index })
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

    #[allow(dead_code)] // Used by M1 hardware tests and M3 fault register snapshots.
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

    /// Blocks while the guest runs on the physical CPU, then decodes the exit.
    pub fn run(&self) -> Result<VmExit> {
        let mut exit = Aligned16(WHV_RUN_VP_EXIT_CONTEXT::default());
        // SAFETY: `exit` is a writable, 16-byte-aligned `WHV_RUN_VP_EXIT_CONTEXT` of the size
        // passed, and the partition outlives `self` per `new`'s contract.
        unsafe {
            WHvRunVirtualProcessor(
                self.partition,
                self.index,
                (&raw mut exit.0).cast(),
                size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32,
            )
        }
        .map_err(failed("WHvRunVirtualProcessor"))?;

        match exit.0.ExitReason {
            reason if reason == WHvRunVpExitReasonX64Halt => Ok(VmExit::Halt),
            reason if reason == WHvRunVpExitReasonX64IoPortAccess => self.decode_io(&exit.0),
            reason => Ok(VmExit::Unhandled { code: reason.0 }),
        }
    }

    fn decode_io(&self, exit: &WHV_RUN_VP_EXIT_CONTEXT) -> Result<VmExit> {
        // SAFETY: the exit reason is `X64IoPortAccess`, so WHP filled `IoPortAccess`.
        let io = unsafe { exit.Anonymous.IoPortAccess };
        // SAFETY: `AsUINT32` is a plain integer view; every bit pattern is valid.
        let info = unsafe { io.AccessInfo.AsUINT32 };
        if info & IO_IS_WRITE == 0 || info & IO_STRING_OP != 0 {
            return Ok(VmExit::Unhandled {
                code: exit.ExitReason.0,
            });
        }
        let size_bytes = ((info >> IO_SIZE_SHIFT) & IO_SIZE_MASK) as u8;
        let mask = match size_bytes {
            1 => 0xFF,
            2 => 0xFFFF,
            _ => u32::MAX,
        };

        // WHP leaves RIP on the `out`; the host must retire it before re-entering.
        let len = u64::from(exit.VpContext._bitfield & INSTRUCTION_LEN_MASK);
        let next = reg64(exit.VpContext.Rip.wrapping_add(len));
        self.set(&[WHvX64RegisterRip], &[next])?;

        Ok(VmExit::IoOut {
            port: io.PortNumber,
            size_bytes,
            value: io.Rax as u32 & mask,
        })
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
        // SAFETY: the partition outlives `self` per `new`'s contract, and `index` was created
        // by this `Vcpu` and is never used again.
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
