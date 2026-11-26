//! Reference bare-metal `no_std` guest running in 64-bit Long Mode on WHP.
#![no_std]
#![no_main]

use core::arch::asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

const STDOUT_PORT: u16 = 0x3F8;
const PANIC_PORT: u16 = 0x3F9;
const INPUT_PORT: u16 = 0x3FA;

/// Lives in `.bss`; starting at 0 verifies host ELF `.bss` zeroing.
static BSS_COUNTER: AtomicU64 = AtomicU64::new(0);

struct PortWriter(u16);

impl PortWriter {
    fn write_byte(&self, byte: u8) {
        // SAFETY: `self.0` is a host-intercepted guest output I/O port (`STDOUT_PORT` or `PANIC_PORT`).
        unsafe {
            asm!(
                "out dx, al",
                in("dx") self.0,
                in("al") byte,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
}

impl Write for PortWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

fn read_input_byte() -> u8 {
    let val: u8;
    // SAFETY: `INPUT_PORT` is the host-intercepted guest input I/O port (`0` on EOF).
    unsafe {
        asm!(
            "in al, dx",
            in("dx") INPUT_PORT,
            out("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
    val
}

fn read_command(buf: &mut [u8; 64]) -> &str {
    let mut len = 0;
    while len < buf.len() {
        let b = read_input_byte();
        if b == 0 {
            break;
        }
        buf[len] = b;
        len += 1;
    }
    core::str::from_utf8(&buf[..len]).unwrap_or("").trim()
}

fn halt_forever() -> ! {
    loop {
        // SAFETY: `hlt` cleanly yields control to the WHP host via `WHvRunVpExitReasonX64Halt`.
        unsafe {
            asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[inline(never)]
fn trigger_panic() -> ! {
    panic!("intentional guest panic");
}

#[inline(never)]
fn trigger_ud2() -> ! {
    // SAFETY: intentional `#UD` fault trigger for M3 exception-trapping tests.
    unsafe {
        asm!("ud2", options(noreturn));
    }
}

#[inline(never)]
fn trigger_div0() -> ! {
    let divisor = 0u32;
    // SAFETY: intentional `#DE` divide-by-zero fault trigger via hardware `div`.
    unsafe {
        asm!(
            "xor edx, edx",
            "xor eax, eax",
            "div {d:e}",
            d = in(reg) divisor,
            options(nomem, nostack),
        );
    }
    halt_forever();
}

#[inline(never)]
fn trigger_oob() -> ! {
    // 1 GiB physical address is outside the default 16 MiB RAM + page tables -> `#PF`.
    let unmapped = 0x4000_0000usize as *const u64;
    // SAFETY: intentional out-of-bounds read to trigger `#PF` at `0x4000_0000`.
    let _ = unsafe { ptr::read_volatile(unmapped) };
    halt_forever();
}

#[inline(never)]
fn trigger_hang() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let mut buf = [0u8; 64];
    match read_command(&mut buf) {
        "panic" => trigger_panic(),
        "ud2" => trigger_ud2(),
        "div0" => trigger_div0(),
        "oob" => trigger_oob(),
        "hang" => trigger_hang(),
        _ => {
            let prev = BSS_COUNTER.fetch_add(1, Ordering::Relaxed);
            let sum: u64 = (1..=100).sum();
            let _ = writeln!(
                PortWriter(STDOUT_PORT),
                "hello from bare-metal rust! sum(1..=100)={sum} bss_prev={prev}"
            );
            halt_forever();
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let _ = writeln!(PortWriter(PANIC_PORT), "{info}");
    halt_forever();
}
