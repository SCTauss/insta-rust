//! Crate-wide error type.

use std::fmt;
use std::path::PathBuf;

/// Failure raised by the runtime or its hypervisor backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The host hypervisor is missing or disabled (e.g. WHP feature off).
    Unavailable,
    /// A hypervisor call failed with the given platform status code.
    Hypervisor { call: &'static str, code: i32 },
    /// The host could not commit backing memory for guest RAM.
    OutOfMemory { size_bytes: usize },
    /// Guest RAM size is zero, not a page multiple, or exceeds the supported window.
    BadSize { size_bytes: usize },
    /// A guest physical range falls outside mapped RAM.
    OutOfRange { addr: u64, len: usize },
    /// Could not read a guest binary from disk.
    ReadFile { path: PathBuf, reason: String },
    /// The guest binary is not a valid 64-bit x86-64 ELF image.
    InvalidElf { reason: String },
}

/// Result alias using [`Error`].
pub type Result<T> = core::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("hypervisor unavailable on this host"),
            Self::Hypervisor { call, code } => write!(f, "{call} failed: HRESULT {code:#010x}"),
            Self::OutOfMemory { size_bytes } => {
                write!(f, "failed to commit {size_bytes} bytes of guest RAM")
            }
            Self::BadSize { size_bytes } => {
                write!(
                    f,
                    "guest RAM size {size_bytes} is not a valid page-aligned size"
                )
            }
            Self::OutOfRange { addr, len } => {
                write!(f, "guest range {addr:#x}+{len:#x} is outside mapped RAM")
            }
            Self::ReadFile { path, reason } => {
                write!(f, "failed to read {}: {reason}", path.display())
            }
            Self::InvalidElf { reason } => write!(f, "invalid guest ELF: {reason}"),
        }
    }
}

impl std::error::Error for Error {}
