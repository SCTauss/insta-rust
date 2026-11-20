//! Crate-wide error type.

use std::fmt;

/// Failure raised by the runtime or its hypervisor backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The host hypervisor is missing or disabled (e.g. WHP feature off).
    Unavailable,
    /// A hypervisor call failed with the given platform status code.
    Hypervisor { call: &'static str, code: i32 },
    /// The host could not commit backing memory for guest RAM.
    OutOfMemory { size_bytes: usize },
    /// Guest RAM size is zero or not a multiple of the page size.
    BadSize { size_bytes: usize },
    /// A guest physical range falls outside mapped RAM.
    OutOfRange { addr: u64, len: usize },
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
                    "guest RAM size {size_bytes} is not a non-zero page multiple"
                )
            }
            Self::OutOfRange { addr, len } => {
                write!(f, "guest range {addr:#x}+{len:#x} is outside mapped RAM")
            }
        }
    }
}

impl std::error::Error for Error {}
