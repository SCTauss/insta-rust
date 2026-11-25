//! Reference bare-metal `no_std` guest running in 64-bit Long Mode on WHP.
#![no_std]
#![no_main]

use core::arch::asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};

const STDOUT_PORT: u16 = 0x3F8;

/// Lives in `.bss`; starting at 0 verifies host ELF `.bss` zeroing.
static BSS_COUNTER: AtomicU64 = AtomicU64::new(0);

struct Serial;

impl Serial {
    fn write_byte(byte: u8) {
        // SAFETY: `STDOUT_PORT` is the host-intercepted guest stdout I/O port.
        unsafe {
            asm!(
                "out dx, al",
                in("dx") STDOUT_PORT,
                in("al") byte,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
}

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            Self::write_byte(byte);
        }
        Ok(())
    }
}

fn halt_forever() -> ! {
    loop {
        // SAFETY: `hlt` cleanly yields control to the WHP host via `WHvRunVpExitReasonX64Halt`.
        unsafe {
            asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let prev = BSS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let sum: u64 = (1..=100).sum();
    let _ = writeln!(
        Serial,
        "hello from bare-metal rust! sum(1..=100)={sum} bss_prev={prev}"
    );
    halt_forever();
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    let _ = Serial.write_str("guest panic\n");
    halt_forever();
}
