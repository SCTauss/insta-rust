use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use insta_rust::fault::Reason;
use insta_rust::{Config, ElfImage, ExitStatus, Instance};

/// Windows `vid.sys` permits only one mapped WHP partition per host process at a time.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn lock_vm() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/insta-rust has workspace root two levels up")
        .to_path_buf()
}

fn load_guest_basic() -> ElfImage {
    let root = workspace_root();
    let manifest = root.join("examples").join("guest-basic").join("Cargo.toml");
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--manifest-path",
            manifest.to_str().expect("UTF-8 manifest path"),
            "--target",
            "x86_64-unknown-none",
        ])
        .status()
        .expect("invoke cargo build for guest-basic");
    assert!(status.success(), "building examples/guest-basic failed");

    let elf_path = root
        .join("examples")
        .join("guest-basic")
        .join("target")
        .join("x86_64-unknown-none")
        .join("debug")
        .join("guest-basic");
    ElfImage::from_file(&elf_path).expect("load guest-basic ELF")
}

fn assert_guest_source(loc: &Option<insta_rust::fault::Location>) {
    let loc = loc.as_ref().expect("expected symbolized DWARF location");
    let file = loc.file.as_deref().expect("expected symbolized file path");
    assert!(
        file.ends_with("main.rs"),
        "expected guest-basic main.rs, got {file}"
    );
    assert!(loc.line.is_some(), "expected symbolized line number");
}

#[test]
fn catches_guest_panic() {
    let _guard = lock_vm();
    let elf = load_guest_basic();
    let mut instance = Instance::boot(&elf, Config::default()).expect("boot guest");
    let output = instance.run_with_input(b"panic").expect("run guest");

    match output.exit {
        ExitStatus::Faulted(report) => {
            match &report.reason {
                Reason::GuestPanic { message } => assert!(
                    message.contains("intentional guest panic"),
                    "unexpected panic message: {message}"
                ),
                other => panic!("expected GuestPanic, got {other:?}"),
            }
            assert_guest_source(&report.location);
        }
        other => panic!("expected Faulted(GuestPanic), got {other:?}"),
    }
}

#[test]
fn catches_ud2_exception() {
    let _guard = lock_vm();
    let elf = load_guest_basic();
    let mut instance = Instance::boot(&elf, Config::default()).expect("boot guest");
    let output = instance.run_with_input(b"ud2").expect("run guest");

    match output.exit {
        ExitStatus::Faulted(report) => {
            assert_eq!(report.reason, Reason::InvalidOpcode);
            assert_guest_source(&report.location);
        }
        other => panic!("expected Faulted(InvalidOpcode), got {other:?}"),
    }
}

#[test]
fn catches_divide_by_zero() {
    let _guard = lock_vm();
    let elf = load_guest_basic();
    let mut instance = Instance::boot(&elf, Config::default()).expect("boot guest");
    let output = instance.run_with_input(b"div0").expect("run guest");

    match output.exit {
        ExitStatus::Faulted(report) => {
            assert_eq!(report.reason, Reason::DivideByZero);
            assert_guest_source(&report.location);
        }
        other => panic!("expected Faulted(DivideByZero), got {other:?}"),
    }
}

#[test]
fn catches_out_of_bounds_page_fault() {
    let _guard = lock_vm();
    let elf = load_guest_basic();
    let mut instance = Instance::boot(&elf, Config::default()).expect("boot guest");
    let output = instance.run_with_input(b"oob").expect("run guest");

    match output.exit {
        ExitStatus::Faulted(report) => {
            assert!(
                matches!(
                    report.reason,
                    Reason::PageFault {
                        addr: 0x4000_0000,
                        ..
                    }
                ),
                "expected PageFault at 0x4000_0000, got {:?}",
                report.reason
            );
            assert_guest_source(&report.location);
        }
        other => panic!("expected Faulted(PageFault), got {other:?}"),
    }
}

#[test]
fn cancels_infinite_loop_with_watchdog() {
    let _guard = lock_vm();
    let elf = load_guest_basic();
    let config = Config {
        timeout: Some(Duration::from_millis(100)),
        ..Config::default()
    };
    let mut instance = Instance::boot(&elf, config).expect("boot guest");
    let started = Instant::now();
    let output = instance.run_with_input(b"hang").expect("run guest");
    let wall = started.elapsed();

    assert!(
        wall < Duration::from_secs(2),
        "watchdog took too long: {wall:?}"
    );
    match output.exit {
        ExitStatus::TimedOut { location, .. } => {
            assert_guest_source(&location);
        }
        other => panic!("expected TimedOut, got {other:?}"),
    }
}
