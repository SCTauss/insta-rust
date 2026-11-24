use std::sync::{Mutex, MutexGuard, PoisonError};

use super::Vm;
use crate::backend::{GuestAddr, HypervisorBackend, PAGE_SIZE, Regs, SpecialRegs, VmExit};
use crate::error::Error;

const RAM_BYTES: usize = 16 * PAGE_SIZE;
const CODE_ADDR: u64 = 0x1000;
const STACK_TOP: u64 = 0x8000;
const RESULT_PORT: u16 = 0x10;

/// 16-bit real-mode program: AX = sum(1..=100), report AX on port 0x10, halt.
const SUM_PROGRAM: [u8; 12] = [
    0x31, 0xC0, // xor ax, ax
    0xB9, 0x64, 0x00, // mov cx, 100
    0x01, 0xC8, // add ax, cx
    0xE2, 0xFC, // loop -4 (back to add)
    0xE7, 0x10, // out 0x10, ax
    0xF4, // hlt
];
const SUM_RESULT: u32 = 5050;

/// Windows `vid.sys` permits only one mapped WHP partition per host process at a time,
/// so parallel tests in this binary must serialize live `Vm` lifetimes.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn lock_vm() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn boot(program: &[u8]) -> Vm {
    let mut vm = Vm::create(RAM_BYTES).expect("create WHP VM");
    vm.write(GuestAddr(CODE_ADDR), program)
        .expect("load program");
    vm.set_special_regs(&SpecialRegs::real_mode())
        .expect("set special registers");
    vm.set_regs(&Regs {
        rip: CODE_ADDR,
        rsp: STACK_TOP,
        ..Regs::default()
    })
    .expect("set registers");
    vm
}

#[test]
fn reports_whp_available() {
    assert!(
        Vm::is_available(),
        "Windows Hypervisor Platform is not enabled on this host"
    );
}

#[test]
fn runs_raw_code_on_vcpu() {
    let _guard = lock_vm();
    let mut vm = boot(&SUM_PROGRAM);

    let exit = vm.run().expect("run to out");
    assert_eq!(
        exit,
        VmExit::IoOut {
            port: RESULT_PORT,
            size_bytes: 2,
            value: SUM_RESULT,
        }
    );

    let exit = vm.run().expect("run to hlt");
    assert_eq!(exit, VmExit::Halt);

    let regs = vm.regs().expect("read registers");
    assert_eq!(regs.rax & 0xFFFF, u64::from(SUM_RESULT));
    assert_eq!(regs.rcx & 0xFFFF, 0, "loop counter must be exhausted");
    let end = CODE_ADDR + SUM_PROGRAM.len() as u64;
    assert_eq!(regs.rip, end, "WHP retires hlt before exiting");
}

#[test]
fn rejects_out_of_range_write() {
    let _guard = lock_vm();
    let mut vm = Vm::create(RAM_BYTES).expect("create WHP VM");
    let addr = (RAM_BYTES - 1) as u64;
    assert_eq!(
        vm.write(GuestAddr(addr), &[0; 2]),
        Err(Error::OutOfRange { addr, len: 2 })
    );
}

#[test]
fn rejects_unaligned_ram_size() {
    assert!(matches!(
        Vm::create(PAGE_SIZE + 1),
        Err(Error::BadSize { size_bytes }) if size_bytes == PAGE_SIZE + 1
    ));
}

#[test]
fn reuses_process_slot_across_sequential_vms() {
    let _guard = lock_vm();
    for _ in 0..16 {
        let mut vm = boot(&SUM_PROGRAM);
        assert_eq!(
            vm.run().expect("run to out"),
            VmExit::IoOut {
                port: RESULT_PORT,
                size_bytes: 2,
                value: SUM_RESULT,
            }
        );
        assert_eq!(vm.run().expect("run to hlt"), VmExit::Halt);
    }
}
