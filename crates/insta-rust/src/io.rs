//! Host-side guest I/O channel capturing port writes.

/// x86 I/O port for guest stdout byte writes (COM1 convention).
pub const STDOUT_PORT: u16 = 0x3F8;

/// Accumulates host-captured output emitted by a running guest.
#[derive(Debug, Default)]
pub struct Channel {
    stdout: Vec<u8>,
}

impl Channel {
    /// Creates an empty I/O channel.
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes a guest `out` exit if the port is recognized; returns `false` otherwise.
    pub fn write_port(&mut self, port: u16, size_bytes: u8, value: u32) -> bool {
        if port != STDOUT_PORT {
            return false;
        }
        let len = usize::from(size_bytes).min(size_of::<u32>());
        self.stdout.extend_from_slice(&value.to_le_bytes()[..len]);
        true
    }

    /// Returns all bytes captured from the guest stdout port.
    pub fn into_stdout(self) -> Vec<u8> {
        self.stdout
    }
}
