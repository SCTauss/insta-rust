//! Fault classification, register snapshots, and host-side ELF DWARF symbolization.

use std::fmt;

use addr2line::Context;
use gimli::{Dwarf, EndianSlice, LittleEndian as GimliLe};
use object::read::elf::ElfFile64;
use object::{LittleEndian, Object, ObjectSection};

use crate::backend::Regs;

/// Classified reason a bare-metal guest faulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// `#DE` (vector 0): integer divide by zero or quotient overflow.
    DivideByZero,
    /// `#BP` (vector 3): software breakpoint (`int3`).
    Breakpoint,
    /// `#UD` (vector 6): invalid or undefined opcode (`ud2` or unconfigured SIMD).
    InvalidOpcode,
    /// `#GP` (vector 13): general protection fault.
    GeneralProtection { error_code: Option<u32> },
    /// `#PF` (vector 14): page fault at `addr` (`CR2`).
    PageFault { addr: u64, error_code: Option<u32> },
    /// Unmapped or disallowed guest physical memory access.
    MemoryFault { gpa: u64, gva: u64 },
    /// Unrecoverable exception (Triple Fault).
    TripleFault,
    /// Guest `#[panic_handler]` reported a panic and halted.
    GuestPanic { message: String },
    /// Other hardware exception vector.
    Exception { vector: u8, error_code: Option<u32> },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DivideByZero => f.write_str("DivideByZero (#DE)"),
            Self::Breakpoint => f.write_str("Breakpoint (#BP)"),
            Self::InvalidOpcode => f.write_str("InvalidOpcode (#UD)"),
            Self::GeneralProtection { error_code } => match error_code {
                Some(code) => write!(f, "GeneralProtection (#GP, error={code:#x})"),
                None => f.write_str("GeneralProtection (#GP)"),
            },
            Self::PageFault { addr, error_code } => match error_code {
                Some(code) => write!(f, "PageFault (#PF at {addr:#x}, error={code:#x})"),
                None => write!(f, "PageFault (#PF at {addr:#x})"),
            },
            Self::MemoryFault { gpa, gva } => {
                write!(f, "MemoryFault (gpa={gpa:#x}, gva={gva:#x})")
            }
            Self::TripleFault => f.write_str("TripleFault"),
            Self::GuestPanic { message } => write!(f, "GuestPanic: {message}"),
            Self::Exception { vector, error_code } => match error_code {
                Some(code) => write!(f, "Exception (vector={vector}, error={code:#x})"),
                None => write!(f, "Exception (vector={vector})"),
            },
        }
    }
}

/// DWARF-symbolized source location for a guest instruction pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub function: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(func) = &self.function {
            write!(f, "{func}")?;
        } else {
            f.write_str("<unknown>")?;
        }
        if let Some(file) = &self.file {
            write!(f, " at {file}")?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
                if let Some(col) = self.column {
                    write!(f, ":{col}")?;
                }
            }
        }
        Ok(())
    }
}

/// Structured fault diagnostic returned when a guest crashes or panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub reason: Reason,
    pub regs: Regs,
    pub location: Option<Location>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "guest fault: {}", self.reason)?;
        if let Some(loc) = &self.location {
            writeln!(f, "  at {loc}")?;
        }
        write!(
            f,
            "  rip={:#x} rsp={:#x} rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x} rflags={:#x}",
            self.regs.rip,
            self.regs.rsp,
            self.regs.rax,
            self.regs.rbx,
            self.regs.rcx,
            self.regs.rdx,
            self.regs.rflags
        )
    }
}

pub(crate) fn classify_exception(
    vector: u8,
    error_code: Option<u32>,
    faulting_addr: Option<u64>,
) -> Reason {
    match vector {
        0 => Reason::DivideByZero,
        3 => Reason::Breakpoint,
        6 => Reason::InvalidOpcode,
        13 => Reason::GeneralProtection { error_code },
        14 => Reason::PageFault {
            addr: faulting_addr.unwrap_or(0),
            error_code,
        },
        _ => Reason::Exception { vector, error_code },
    }
}

/// Resolves a guest virtual `rip` to `function` and `file:line` using ELF DWARF sections.
pub(crate) fn symbolize(elf_bytes: &[u8], rip: u64) -> Option<Location> {
    let elf = ElfFile64::<LittleEndian>::parse(elf_bytes).ok()?;
    let dwarf = Dwarf::load(
        |section| -> Result<EndianSlice<'_, GimliLe>, gimli::Error> {
            let data = elf
                .section_by_name(section.name())
                .and_then(|s| s.data().ok())
                .unwrap_or(&[]);
            Ok(EndianSlice::new(data, GimliLe))
        },
    )
    .ok()?;

    let ctx = Context::from_dwarf(dwarf).ok()?;
    let mut frames = ctx.find_frames(rip).skip_all_loads().ok()?;
    let mut fallback = None;
    while let Ok(Some(frame)) = frames.next() {
        let loc = frame_to_location(&frame);
        let is_std = loc
            .file
            .as_deref()
            .is_some_and(|f| f.starts_with("/rustc/") || f.starts_with(r"\rustc\"));
        if !is_std && loc.file.is_some() {
            return Some(loc);
        }
        if fallback.is_none() {
            fallback = Some(loc);
        }
    }
    fallback
}

fn frame_to_location(frame: &addr2line::Frame<'_, EndianSlice<'_, GimliLe>>) -> Location {
    let function = frame
        .function
        .as_ref()
        .and_then(|name| name.demangle().ok())
        .map(|s| s.into_owned());
    let (file, line, column) = match &frame.location {
        Some(loc) => (loc.file.map(str::to_string), loc.line, loc.column),
        None => (None, None, None),
    };
    Location {
        function,
        file,
        line,
        column,
    }
}
