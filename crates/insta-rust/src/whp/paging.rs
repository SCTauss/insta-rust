//! 64-bit long-mode identity page tables (2 MiB huge pages) and initial CPU state.

use crate::backend::{GuestAddr, HypervisorBackend, PAGE_SIZE, Segment, SpecialRegs};
use crate::error::{Error, Result};

/// Guest physical addresses of the 3-page identity map (PML4 -> PDPT -> PD).
pub const PML4_ADDR: GuestAddr = GuestAddr(0x1000);
const PDPT_ADDR: GuestAddr = GuestAddr(0x2000);
const PD_ADDR: GuestAddr = GuestAddr(0x3000);

/// Exclusive end of low reserved guest memory (null page + identity page tables).
pub const RESERVED_LOW_END: u64 = 0x4000;

/// Initial 16-byte-aligned stack top, right below the 2 MiB guest ELF load base.
pub const INITIAL_STACK_TOP: u64 = 0x001F_FFF0;

/// Minimum RAM required to hold the page tables, stack, and a guest image at 2 MiB.
pub const MIN_RAM_BYTES: usize = 4 * 1024 * 1024;

const HUGE_PAGE_BYTES: usize = 2 * 1024 * 1024;
const ENTRIES_PER_TABLE: usize = PAGE_SIZE / size_of::<u64>();

/// Single PD covers up to 512 * 2 MiB = 1 GiB of identity-mapped guest RAM.
pub const MAX_RAM_BYTES: usize = ENTRIES_PER_TABLE * HUGE_PAGE_BYTES;

const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_HUGE: u64 = 1 << 7;

const CR0_PE: u64 = 1 << 0;
const CR0_MP: u64 = 1 << 1;
const CR0_ET: u64 = 1 << 4;
const CR0_NE: u64 = 1 << 5;
const CR0_PG: u64 = 1 << 31;

const CR4_PAE: u64 = 1 << 5;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;

const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;

const CODE_SELECTOR: u16 = 0x08;
const DATA_SELECTOR: u16 = 0x10;

/// Validates that `ram_bytes` is a 2 MiB multiple within the supported identity-map window.
pub fn validate_ram_bytes(ram_bytes: usize) -> Result<()> {
    if !(MIN_RAM_BYTES..=MAX_RAM_BYTES).contains(&ram_bytes)
        || !ram_bytes.is_multiple_of(HUGE_PAGE_BYTES)
    {
        return Err(Error::BadSize {
            size_bytes: ram_bytes,
        });
    }
    Ok(())
}

/// Writes identity-mapped 4-level page tables into guest RAM and returns matching long-mode regs.
pub fn install(vm: &mut impl HypervisorBackend, ram_bytes: usize) -> Result<SpecialRegs> {
    validate_ram_bytes(ram_bytes)?;

    let pml4_entry = (PDPT_ADDR.0 | PTE_PRESENT | PTE_WRITABLE).to_le_bytes();
    vm.write(PML4_ADDR, &pml4_entry)?;

    let pdpt_entry = (PD_ADDR.0 | PTE_PRESENT | PTE_WRITABLE).to_le_bytes();
    vm.write(PDPT_ADDR, &pdpt_entry)?;

    let huge_pages = ram_bytes / HUGE_PAGE_BYTES;
    let mut pd = [0u8; PAGE_SIZE];
    for i in 0..huge_pages {
        let entry = ((i * HUGE_PAGE_BYTES) as u64) | PTE_PRESENT | PTE_WRITABLE | PTE_HUGE;
        let offset = i * size_of::<u64>();
        pd[offset..offset + size_of::<u64>()].copy_from_slice(&entry.to_le_bytes());
    }
    vm.write(PD_ADDR, &pd)?;

    let data = Segment::data64(DATA_SELECTOR);
    Ok(SpecialRegs {
        cs: Segment::code64(CODE_SELECTOR),
        ds: data,
        es: data,
        fs: data,
        gs: data,
        ss: data,
        cr0: CR0_PE | CR0_MP | CR0_ET | CR0_NE | CR0_PG,
        cr3: PML4_ADDR.0,
        cr4: CR4_PAE | CR4_OSFXSR | CR4_OSXMMEXCPT,
        efer: EFER_LME | EFER_LMA,
    })
}
