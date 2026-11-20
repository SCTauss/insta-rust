//! Platform-neutral hypervisor contract implemented by each backend.

use crate::error::Result;

/// Guest page size; RAM sizes and mapping bases must be multiples of it.
pub const PAGE_SIZE: usize = 4096;

/// RFLAGS bit 1 is architecturally reserved and must always read as 1.
const RFLAGS_FIXED: u64 = 1 << 1;

/// CR0.ET is hardwired to 1 on every x86-64 CPU.
const CR0_ET: u64 = 1 << 4;

/// Guest physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GuestAddr(pub u64);

/// General-purpose registers plus instruction pointer and flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

impl Default for Regs {
    fn default() -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rsp: 0,
            rbp: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: 0,
            rflags: RFLAGS_FIXED,
        }
    }
}

/// Hidden segment descriptor cache as loaded into a vCPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    /// 4-bit descriptor type field.
    pub kind: u8,
    /// `S` bit: code/data rather than a system descriptor.
    pub code_data: bool,
    pub dpl: u8,
    pub present: bool,
    /// `L` bit: 64-bit code segment.
    pub long: bool,
    /// `D/B` bit: 32-bit default operand size.
    pub big: bool,
    pub granular: bool,
}

impl Segment {
    /// Execute/read code, accessed.
    pub const CODE: u8 = 0xB;
    /// Read/write data, accessed.
    pub const DATA: u8 = 0x3;

    /// Real-mode segment: base is `selector << 4` with a 64 KiB limit.
    pub fn real(selector: u16, kind: u8) -> Self {
        Self {
            base: u64::from(selector) << 4,
            limit: 0xFFFF,
            selector,
            kind,
            code_data: true,
            dpl: 0,
            present: true,
            long: false,
            big: false,
            granular: false,
        }
    }
}

/// Segment caches, control registers, and EFER.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecialRegs {
    pub cs: Segment,
    pub ds: Segment,
    pub es: Segment,
    pub fs: Segment,
    pub gs: Segment,
    pub ss: Segment,
    pub cr0: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub efer: u64,
}

impl SpecialRegs {
    /// 16-bit real mode with every segment at selector 0, paging and protection off.
    pub fn real_mode() -> Self {
        let data = Segment::real(0, Segment::DATA);
        Self {
            cs: Segment::real(0, Segment::CODE),
            ds: data,
            es: data,
            fs: data,
            gs: data,
            ss: data,
            cr0: CR0_ET,
            cr3: 0,
            cr4: 0,
            efer: 0,
        }
    }
}

/// Why the vCPU stopped executing guest code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmExit {
    /// Guest executed `hlt`.
    Halt,
    /// Guest wrote `value` (`size_bytes` wide) to I/O `port`; the `out` is already retired.
    IoOut {
        port: u16,
        size_bytes: u8,
        value: u32,
    },
    /// Exit the backend does not translate yet; carries the raw platform reason code.
    Unhandled { code: i32 },
}

/// Single-vCPU hardware virtual machine provided by a hypervisor backend.
pub trait HypervisorBackend: Sized {
    /// Returns whether this backend's hypervisor is usable on the host.
    fn is_available() -> bool;

    /// Creates a VM with `ram_bytes` of zeroed guest RAM mapped at GPA 0 and one vCPU.
    fn create(ram_bytes: usize) -> Result<Self>;

    /// Copies `bytes` into guest RAM starting at `addr`.
    fn write(&mut self, addr: GuestAddr, bytes: &[u8]) -> Result<()>;

    /// Loads the general-purpose register file.
    fn set_regs(&mut self, regs: &Regs) -> Result<()>;

    /// Reads the general-purpose register file.
    fn regs(&self) -> Result<Regs>;

    /// Loads segment caches, control registers, and EFER.
    fn set_special_regs(&mut self, sregs: &SpecialRegs) -> Result<()>;

    /// Runs the vCPU on the physical CPU until the next VM exit.
    fn run(&mut self) -> Result<VmExit>;
}
