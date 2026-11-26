//! Direct ELF64 parsing and guest RAM loading (`PT_LOAD`, `.bss` zeroing, `e_entry`).

use std::fs;
use std::path::Path;

use object::LittleEndian;
use object::elf::{EM_X86_64, ET_EXEC, PT_LOAD};
use object::read::elf::{ElfFile64, FileHeader, ProgramHeader};

use crate::backend::{GuestAddr, HypervisorBackend};
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
struct LoadSegment {
    guest_addr: GuestAddr,
    file_offset: usize,
    file_size: usize,
    mem_size: usize,
}

/// Validated 64-bit x86-64 ELF guest image ready to load into guest RAM.
#[derive(Debug, Clone)]
pub struct ElfImage {
    bytes: Vec<u8>,
    entry: GuestAddr,
    segments: Vec<LoadSegment>,
}

impl ElfImage {
    /// Parses and validates an ELF64 x86-64 image from raw bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let elf = ElfFile64::<LittleEndian>::parse(bytes.as_slice()).map_err(|err| {
            Error::InvalidElf {
                reason: err.to_string(),
            }
        })?;
        let header = elf.elf_header();
        let endian = elf.endian();

        if header.e_machine(endian) != EM_X86_64 {
            return Err(Error::InvalidElf {
                reason: format!("expected EM_X86_64, got {:#x}", header.e_machine(endian)),
            });
        }
        if header.e_type(endian) != ET_EXEC {
            return Err(Error::InvalidElf {
                reason: format!(
                    "expected static ET_EXEC ELF (link with -no-pie), got e_type={:#x}",
                    header.e_type(endian)
                ),
            });
        }

        let entry = GuestAddr(header.e_entry(endian));
        let mut segments = Vec::new();

        for ph in elf.elf_program_headers() {
            if ph.p_type(endian) != PT_LOAD {
                continue;
            }
            let paddr = ph.p_paddr(endian);
            let vaddr = ph.p_vaddr(endian);
            let guest_addr = GuestAddr(if paddr != 0 { paddr } else { vaddr });

            let file_offset =
                usize::try_from(ph.p_offset(endian)).map_err(|_| Error::InvalidElf {
                    reason: "PT_LOAD file offset overflows usize".into(),
                })?;
            let file_size =
                usize::try_from(ph.p_filesz(endian)).map_err(|_| Error::InvalidElf {
                    reason: "PT_LOAD file size overflows usize".into(),
                })?;
            let mem_size = usize::try_from(ph.p_memsz(endian)).map_err(|_| Error::InvalidElf {
                reason: "PT_LOAD memory size overflows usize".into(),
            })?;

            if file_size > mem_size {
                return Err(Error::InvalidElf {
                    reason: format!("PT_LOAD p_filesz ({file_size}) exceeds p_memsz ({mem_size})"),
                });
            }
            let end = file_offset
                .checked_add(file_size)
                .ok_or_else(|| Error::InvalidElf {
                    reason: "PT_LOAD file range overflow".into(),
                })?;
            if end > bytes.len() {
                return Err(Error::InvalidElf {
                    reason: format!("PT_LOAD extends past EOF ({end} > {})", bytes.len()),
                });
            }

            segments.push(LoadSegment {
                guest_addr,
                file_offset,
                file_size,
                mem_size,
            });
        }

        if segments.is_empty() {
            return Err(Error::InvalidElf {
                reason: "ELF contains no PT_LOAD segments".into(),
            });
        }

        Ok(Self {
            bytes,
            entry,
            segments,
        })
    }

    /// Reads and validates an ELF64 x86-64 guest image from disk.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|err| Error::ReadFile {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
        Self::from_bytes(bytes)
    }

    /// Guest entry point address (`e_entry`).
    pub fn entry(&self) -> u64 {
        self.entry.0
    }

    /// Raw ELF bytes retained for host-side DWARF symbolization.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Verifies that all `PT_LOAD` segments and `e_entry` fit above `reserved_low_end` in RAM.
    pub(crate) fn validate_for_ram(&self, ram_bytes: usize, reserved_low_end: u64) -> Result<()> {
        let ram_end = ram_bytes as u64;
        let mut entry_in_segment = false;

        for seg in &self.segments {
            if seg.mem_size == 0 {
                continue;
            }
            let seg_start = seg.guest_addr.0;
            let seg_end = seg_start
                .checked_add(seg.mem_size as u64)
                .ok_or(Error::OutOfRange {
                    addr: seg_start,
                    len: seg.mem_size,
                })?;

            if seg_start < reserved_low_end {
                return Err(Error::InvalidElf {
                    reason: format!(
                        "PT_LOAD at {seg_start:#x} overlaps reserved low memory (< {reserved_low_end:#x})"
                    ),
                });
            }
            if seg_end > ram_end {
                return Err(Error::OutOfRange {
                    addr: seg_start,
                    len: seg.mem_size,
                });
            }
            if (seg_start..seg_end).contains(&self.entry.0) {
                entry_in_segment = true;
            }
        }

        if !entry_in_segment {
            return Err(Error::InvalidElf {
                reason: format!(
                    "e_entry {:#x} does not fall within any PT_LOAD segment",
                    self.entry.0
                ),
            });
        }
        Ok(())
    }

    /// Copies every `PT_LOAD` segment into guest RAM and zeroes trailing `.bss` bytes.
    pub(crate) fn load_into(&self, vm: &mut impl HypervisorBackend) -> Result<()> {
        for seg in &self.segments {
            if seg.file_size > 0 {
                let data = &self.bytes[seg.file_offset..seg.file_offset + seg.file_size];
                vm.write(seg.guest_addr, data)?;
            }
            let bss_len = seg.mem_size - seg.file_size;
            if bss_len > 0 {
                let bss_addr =
                    GuestAddr(seg.guest_addr.0.checked_add(seg.file_size as u64).ok_or(
                        Error::OutOfRange {
                            addr: seg.guest_addr.0,
                            len: seg.mem_size,
                        },
                    )?);
                vm.zero(bss_addr, bss_len)?;
            }
        }
        Ok(())
    }
}
