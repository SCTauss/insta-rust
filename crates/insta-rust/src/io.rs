//! Host-side guest I/O channel: stdout, panic reports, and input byte stream.

/// x86 I/O port for guest stdout byte writes (COM1 convention).
pub const STDOUT_PORT: u16 = 0x3F8;

/// x86 I/O port for guest panic message writes.
pub const PANIC_PORT: u16 = 0x3F9;

/// x86 I/O port for host-to-guest input byte reads (`0` signals EOF).
pub const INPUT_PORT: u16 = 0x3FA;

/// Accumulates host-captured output/panic streams and serves input bytes to a guest.
#[derive(Debug, Default)]
pub struct Channel {
    stdout: Vec<u8>,
    panic: Vec<u8>,
    input: Vec<u8>,
    cursor: usize,
}

impl Channel {
    /// Creates an I/O channel seeded with `input` bytes for [`INPUT_PORT`] reads.
    pub fn new(input: &[u8]) -> Self {
        Self {
            stdout: Vec::new(),
            panic: Vec::new(),
            input: input.to_vec(),
            cursor: 0,
        }
    }

    /// Records a guest `out` exit targeting [`STDOUT_PORT`] or [`PANIC_PORT`].
    pub fn write_port(&mut self, port: u16, size_bytes: u8, value: u32) {
        let target = match port {
            STDOUT_PORT => &mut self.stdout,
            PANIC_PORT => &mut self.panic,
            _ => return,
        };
        let len = usize::from(size_bytes).min(size_of::<u32>());
        target.extend_from_slice(&value.to_le_bytes()[..len]);
    }

    /// Serves a guest `in` exit from [`INPUT_PORT`] (returns `0` past EOF or on other ports).
    pub fn read_port(&mut self, port: u16, size_bytes: u8) -> u32 {
        if port != INPUT_PORT {
            return 0;
        }
        let count = usize::from(size_bytes).clamp(1, size_of::<u32>());
        let mut bytes = [0u8; 4];
        for slot in &mut bytes[..count] {
            if let Some(&b) = self.input.get(self.cursor) {
                *slot = b;
                self.cursor += 1;
            }
        }
        u32::from_le_bytes(bytes)
    }

    /// Returns the captured guest panic message, if any bytes were written to [`PANIC_PORT`].
    pub fn panic_message(&self) -> Option<String> {
        if self.panic.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&self.panic).trim().to_string())
    }

    /// Returns all bytes captured from [`STDOUT_PORT`].
    pub fn into_stdout(self) -> Vec<u8> {
        self.stdout
    }
}
